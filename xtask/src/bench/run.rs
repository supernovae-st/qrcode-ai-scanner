//! The measurement: corpus selection, the artifacts and their parity, one
//! driver per runtime and variant under the resource caps, ABBA segments
//! under the host gate, throughput and the WASM cold start.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::env::Facts;
use super::gate::{self, Gate, Reading};
use super::manifest::{self, LockDelta, Mismatch};
use super::proc::{self, CallError, Caps, Exit, Failure, Server};
use super::worker::HeapStats;
use crate::external::{self, CorpusRoot};
use crate::oracle::{self, DispositionKind};

pub(crate) const USAGE: &str = "usage: xtask bench run --out <new dir> \
[--dry-run | --wait-gate <seconds>] [--per-group <n>] [--sets zxing,gallery,vendored] \
[--modes full,full-unbounded,fast,frame] [--runtimes lib,cli,node,wasm] \
[--lib-base <bin>] [--lib-candidate <bin>] [--cli-base <bin>] [--cli-candidate <bin>] \
[--node-base <dir>] [--node-candidate <dir>] [--wasm-base <dir>] [--wasm-candidate <dir>] \
[--node-bin <node>] [--throughput-modes <modes>|none] [--rss-cap-mib <n, at most 1024>] \
[--reps <n> (dry run only)]";

/// One warm-up, then five timed repetitions per image and variant.
pub(crate) const WARMUP: u32 = 1;
pub(crate) const REPS: u32 = 5;
/// Cold-start processes per variant (five pairs cannot form a 95 %
/// signed-rank interval).
pub(crate) const COLD_PAIRS: u32 = 12;
/// Default and ceiling of the memory cap, MiB.
pub(crate) const RSS_CAP_MIB: u64 = 1024;
/// The wall cap: max(10 × the preset budget, 60 s); 60 s unbounded.
const WALL_FLOOR: Duration = Duration::from_secs(60);
const WALL_BUDGETS: u64 = 10;
/// A call above this much memory is flagged `over_envelope`.
pub(crate) const ENVELOPE: u64 = 512 << 20;
/// The shortest wait for an open gate a later segment gets once the
/// run's own `--wait-gate` deadline has passed.
const SEGMENT_WAIT: Duration = Duration::from_secs(180);
/// Attempts per segment: one, plus two retries after gate crossings.
const ATTEMPTS: u32 = 3;
/// How often a running throughput pass reads the gate.
const PASS_READ_EVERY: Duration = Duration::from_secs(10);
/// The wall a worker gets to answer `hello`.
const HELLO_WALL: Duration = Duration::from_secs(30);
/// The repository fixture every worker scans once, uncounted, before its
/// first image, and every WASM cold start scans: a clean version-2 QR
/// whose payload is public generated text.
pub(crate) const FIXTURE: &str = "fixtures/clean/gen_v2_m.png";
pub(crate) const FIXTURE_TEXT: &str = "qrc.ai/v2m";

macro_rules! named {
    ($ty:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $ty {
            #[allow(dead_code, reason = "not every enum iterates its variants")]
            pub(crate) const ALL: &[Self] = &[$(Self::$variant),+];
            pub(crate) fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }
        }
    };
}

/// Where the scanner runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Runtime {
    /// The library in-process, through a generated worker.
    Lib,
    /// One `qrscan` process per scan.
    Cli,
    /// The napi package in Node.
    Node,
    /// The wasm package in Node.
    Wasm,
}
named!(Runtime { Lib => "lib", Cli => "cli", Node => "node", Wasm => "wasm" });

impl Runtime {
    /// Native builds share a toolchain-and-profile parity rule; WASM
    /// packages a recipe rule.
    pub(crate) fn native(self) -> bool {
        self != Self::Wasm
    }
}

/// A = base (the published 0.9.0), B = candidate (this tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Variant {
    Base,
    Candidate,
}
named!(Variant { Base => "base", Candidate => "candidate" });

impl Variant {
    pub(crate) fn letter(self) -> &'static str {
        match self {
            Self::Base => "A",
            Self::Candidate => "B",
        }
    }
}

/// The four scan configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mode {
    Full,
    FullUnbounded,
    Fast,
    Frame,
}
named!(Mode { Full => "full", FullUnbounded => "full-unbounded", Fast => "fast", Frame => "frame" });

impl Mode {
    pub(crate) fn profile(self) -> &'static str {
        match self {
            Self::Full | Self::FullUnbounded => "full",
            Self::Fast => "fast",
            Self::Frame => "frame",
        }
    }

    pub(crate) fn budget(self) -> &'static str {
        if self == Self::FullUnbounded {
            "unbounded"
        } else {
            "default"
        }
    }

    /// The preset's wall-clock budget (the same in 0.9.0 and this tree).
    pub(crate) fn budget_ms(self) -> Option<u64> {
        use qrcode_ai_scanner::ScanProfile;
        let preset = match self {
            Self::Full => ScanProfile::Full,
            Self::FullUnbounded => return None,
            Self::Fast => ScanProfile::Fast,
            Self::Frame => ScanProfile::Frame,
        };
        preset.config().budget_ms
    }

    /// Whether the profile scores (a judgment the budget can cut).
    pub(crate) fn scores(self) -> bool {
        self != Self::Frame
    }

    /// A preset budget applies: every image gets an unbudgeted reference
    /// walk at segment start.
    pub(crate) fn budgeted(self) -> bool {
        self.budget_ms().is_some()
    }

    /// The wall cap of one call.
    pub(crate) fn wall_cap(self) -> Duration {
        self.budget_ms().map_or(WALL_FLOOR, |ms| {
            Duration::from_millis(ms * WALL_BUDGETS).max(WALL_FLOOR)
        })
    }
}

/// The oracle's sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Set {
    Zxing,
    Gallery,
    Vendored,
}
named!(Set { Zxing => "zxing", Gallery => "gallery", Vendored => "vendored" });

fn parse_list<T: Copy + PartialEq>(
    value: &str,
    all: &[T],
    name: fn(T) -> &'static str,
) -> Result<Vec<T>, String> {
    let mut picked = Vec::new();
    for word in value.split(',').filter(|w| !w.is_empty()) {
        let item = all
            .iter()
            .copied()
            .find(|t| name(*t) == word)
            .ok_or_else(|| format!("unknown name {word:?}"))?;
        if !picked.contains(&item) {
            picked.push(item);
        }
    }
    // canonical order, whatever the order on the command line
    Ok(all.iter().copied().filter(|t| picked.contains(t)).collect())
}

/// Parsed `bench run` flags.
#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) out: PathBuf,
    pub(crate) dry_run: bool,
    pub(crate) wait_gate: Option<u64>,
    pub(crate) per_group: Option<usize>,
    pub(crate) sets: Vec<Set>,
    pub(crate) modes: Vec<Mode>,
    pub(crate) runtimes: Vec<Runtime>,
    pub(crate) artifacts: BTreeMap<(Runtime, Variant), PathBuf>,
    pub(crate) node: String,
    pub(crate) throughput_modes: Vec<Mode>,
    pub(crate) reps: u32,
    pub(crate) rss_cap_mib: u64,
}

impl Options {
    /// Why this run is a subset of the frozen scope: only the complete
    /// scope — every set, image, mode and runtime, both variants of each,
    /// throughput in `full` — can end `completed`.
    pub(crate) fn subset(&self) -> Vec<String> {
        let names = |list: Vec<&str>| list.join(",");
        let mut why = Vec::new();
        if let Some(n) = self.per_group {
            why.push(format!("per-group sampling ({n} per group)"));
        }
        if self.sets != Set::ALL {
            why.push(format!(
                "sets {}",
                names(self.sets.iter().map(|s| s.as_str()).collect())
            ));
        }
        if self.modes != Mode::ALL {
            why.push(format!(
                "modes {}",
                names(self.modes.iter().map(|m| m.as_str()).collect())
            ));
        }
        if self.runtimes != Runtime::ALL {
            why.push(format!(
                "runtimes {}",
                names(self.runtimes.iter().map(|r| r.as_str()).collect())
            ));
        }
        for runtime in &self.runtimes {
            let both = Variant::ALL
                .iter()
                .all(|v| self.artifacts.contains_key(&(*runtime, *v)));
            if !both {
                why.push(format!("{} has one variant", runtime.as_str()));
            }
        }
        if self.runtimes.contains(&Runtime::Lib) && !self.throughput_modes.contains(&Mode::Full) {
            why.push(String::from("no throughput in full"));
        }
        why
    }

    /// The memory cap of one call, bytes.
    pub(crate) fn rss_cap(&self) -> u64 {
        self.rss_cap_mib << 20
    }

    pub(crate) fn caps(&self, wall: Duration) -> Caps {
        Caps {
            rss: self.rss_cap(),
            wall,
        }
    }
}

const VALUED: [&str; 18] = [
    "--out",
    "--wait-gate",
    "--per-group",
    "--sets",
    "--modes",
    "--runtimes",
    "--lib-base",
    "--lib-candidate",
    "--cli-base",
    "--cli-candidate",
    "--node-base",
    "--node-candidate",
    "--wasm-base",
    "--wasm-candidate",
    "--node-bin",
    "--throughput-modes",
    "--reps",
    "--rss-cap-mib",
];

fn artifact_flags(values: &mut BTreeMap<&str, String>) -> BTreeMap<(Runtime, Variant), PathBuf> {
    let mut artifacts = BTreeMap::new();
    for &runtime in Runtime::ALL {
        for &variant in Variant::ALL {
            let flag = format!("--{}-{}", runtime.as_str(), variant.as_str());
            if let Some(path) = values.remove(flag.as_str()) {
                artifacts.insert((runtime, variant), PathBuf::from(path));
            }
        }
    }
    artifacts
}

/// `--rss-cap-mib`: positive, never above [`RSS_CAP_MIB`].
fn rss_cap(value: Option<String>) -> Result<u64, String> {
    let Some(value) = value else {
        return Ok(RSS_CAP_MIB);
    };
    match value.parse::<u64>() {
        Ok(0) | Err(_) => Err(String::from("--rss-cap-mib takes a positive integer")),
        Ok(n) if n > RSS_CAP_MIB => Err(format!(
            "--rss-cap-mib is at most {RSS_CAP_MIB} (see Resource caps in bench/README.md)"
        )),
        Ok(n) => Ok(n),
    }
}

pub(crate) fn parse_options(args: Vec<String>) -> Result<Options, String> {
    let mut values: BTreeMap<&str, String> = BTreeMap::new();
    let mut dry_run = false;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        if flag == "--dry-run" {
            dry_run = true;
            continue;
        }
        let key = VALUED
            .iter()
            .copied()
            .find(|k| *k == flag)
            .ok_or_else(|| format!("unknown argument {flag:?}"))?;
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if values.insert(key, value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let number = |text: &str, flag: &str| {
        text.parse::<u64>()
            .map_err(|_| format!("{flag} takes a non-negative integer"))
    };
    let out = values
        .remove("--out")
        .map(PathBuf::from)
        .ok_or("--out is required")?;
    let wait_gate = values
        .remove("--wait-gate")
        .map(|v| number(&v, "--wait-gate"))
        .transpose()?;
    if dry_run && wait_gate.is_some() {
        return Err(String::from("--dry-run ignores the gate; drop --wait-gate"));
    }
    let per_group = match values.remove("--per-group") {
        Some(v) => Some(usize::try_from(number(&v, "--per-group")?).map_err(|e| e.to_string())?),
        None => None,
    };
    let reps = match values.remove("--reps") {
        None => REPS,
        Some(_) if !dry_run => {
            return Err(String::from(
                "the protocol fixes five repetitions; --reps is for dry runs",
            ));
        }
        Some(v) => u32::try_from(number(&v, "--reps")?)
            .map_err(|e| e.to_string())?
            .max(1),
    };
    let rss_cap_mib = rss_cap(values.remove("--rss-cap-mib"))?;
    let list = |values: &mut BTreeMap<&str, String>, flag: &str| values.remove(flag);
    let sets = list(&mut values, "--sets").map_or(Ok(Set::ALL.to_vec()), |v| {
        parse_list(&v, Set::ALL, Set::as_str)
    })?;
    let modes = list(&mut values, "--modes").map_or(Ok(Mode::ALL.to_vec()), |v| {
        parse_list(&v, Mode::ALL, Mode::as_str)
    })?;
    let artifacts = artifact_flags(&mut values);
    let present: Vec<Runtime> = Runtime::ALL
        .iter()
        .copied()
        .filter(|r| artifacts.keys().any(|(rt, _)| rt == r))
        .collect();
    let runtimes = match list(&mut values, "--runtimes") {
        Some(v) => parse_list(&v, Runtime::ALL, Runtime::as_str)?,
        None => present.clone(),
    };
    if let Some(missing) = runtimes.iter().find(|r| !present.contains(r)) {
        return Err(format!("runtime {} has no artifact flag", missing.as_str()));
    }
    if runtimes.is_empty() {
        return Err(String::from(
            "no artifact given: pass at least one --<runtime>-<variant>",
        ));
    }
    let throughput_modes = match list(&mut values, "--throughput-modes").as_deref() {
        Some("none") => Vec::new(),
        Some(v) => parse_list(v, Mode::ALL, Mode::as_str)?,
        None if runtimes.contains(&Runtime::Lib) => vec![Mode::Full],
        None => Vec::new(),
    };
    let node = list(&mut values, "--node-bin").unwrap_or_else(|| String::from("node"));
    Ok(Options {
        out,
        dry_run,
        wait_gate,
        per_group,
        sets,
        modes,
        runtimes,
        artifacts,
        node,
        throughput_modes,
        reps,
        rss_cap_mib,
    })
}

// ------------------------------------------------------------------ corpus

/// One measured image. `path` is relative to its corpus (the external
/// root, or the repo for vendored fixtures) — never absolute in a receipt.
#[derive(Debug, Clone)]
pub(crate) struct Image {
    pub(crate) set: Set,
    pub(crate) group: String,
    pub(crate) path: String,
    pub(crate) abs: PathBuf,
    pub(crate) sha256: String,
    pub(crate) bytes: usize,
}

/// The committed inputs behind a run: manifest and disposition hashes.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Inputs {
    pub(crate) manifest_sha256: String,
    pub(crate) vendored_sha256: String,
    pub(crate) dispositions_sha256: String,
    /// `QRSCAN_EXTERNAL_CORPUS` · `default` · `none` (vendored only).
    pub(crate) corpus_root: &'static str,
    /// Images per set in the frozen definition, and how many were measured.
    pub(crate) available: BTreeMap<&'static str, usize>,
    pub(crate) selected: BTreeMap<&'static str, usize>,
    pub(crate) per_group: Option<usize>,
    /// How the inputs stray from their pins ([`pin_findings`]); a gated
    /// run refuses any.
    pub(crate) pins: Vec<String>,
}

/// The oracle's dispositions-table hash (`oracle.rs`: path, image and
/// truth sha256, kind, pinned verdict, pin — one tab-separated line each),
/// recomputed from the same table so a receipt names the frozen set it ran
/// against. Pinned by a test against the accepted oracle receipts.
pub(crate) fn dispositions_sha256() -> String {
    let mut canon = String::new();
    for d in &oracle::DISPOSITIONS {
        let (kind, pin) = match d.kind {
            DispositionKind::KnownWrong { signature } => {
                ("known_wrong", format!("signature={signature}"))
            }
            DispositionKind::TruthIncomplete {
                excused: (symbology, text_sha256),
            } => (
                "truth_incomplete",
                format!("excused={}:{text_sha256}", oracle::wire(&symbology)),
            ),
        };
        let _ = writeln!(
            canon,
            "{}\t{}\t{}\t{kind}\t{}\t{pin}",
            d.path,
            d.image_sha256,
            d.truth_sha256,
            d.verdict.as_str()
        );
    }
    external::sha256_bytes(canon.as_bytes())
}

/// The pins of the frozen inputs: the external manifest, the vendored
/// manifest, and each set's images.
pub(crate) const MANIFEST_PIN: &str =
    "0f5dc2d45d868d3bcab48935629d435622429a46b95705bb24e25b4cb469f9dc";
pub(crate) const VENDORED_PIN: &str =
    "d5bc684ff31e69f8e75df1b31ecda6fdcb0751e2c4ee68123f3aebdc2f562db7";
pub(crate) const FROZEN_IMAGES: [(Set, usize); 3] =
    [(Set::Zxing, 179), (Set::Gallery, 161), (Set::Vendored, 41)];

/// How the inputs stray from their pins — empty when they hold: either
/// manifest's sha256, a set listing other than its frozen count, or a set
/// measured whole whose selection is not the frozen set.
pub(crate) fn pin_findings(inputs: &Inputs, sets: &[Set]) -> Vec<String> {
    let mut found = Vec::new();
    for (name, sha, pin) in [
        (external::MANIFEST, &inputs.manifest_sha256, MANIFEST_PIN),
        ("corpus.toml", &inputs.vendored_sha256, VENDORED_PIN),
    ] {
        if sha != pin {
            found.push(format!("{name} sha256 {sha}, not its pin {pin}"));
        }
    }
    for (set, frozen) in FROZEN_IMAGES {
        let listed = inputs.available.get(set.as_str()).copied().unwrap_or(0);
        if listed != frozen {
            found.push(format!(
                "{}: {listed} images listed, the frozen set has {frozen}",
                set.as_str()
            ));
        }
        let selected = inputs.selected.get(set.as_str()).copied().unwrap_or(0);
        if sets.contains(&set) && inputs.per_group.is_none() && selected != frozen {
            found.push(format!(
                "{}: {selected} images selected of the frozen {frozen}",
                set.as_str()
            ));
        }
    }
    found
}

fn external_images(
    sets: &[Set],
    rows: &[external::Row],
) -> Result<(Vec<Image>, &'static str), String> {
    let dir = match external::corpus_root(None) {
        CorpusRoot::Present(dir) => dir,
        CorpusRoot::DefaultAbsent(dir) => {
            return Err(format!(
                "no external corpus at {} — set QRSCAN_EXTERNAL_CORPUS, or measure --sets vendored",
                dir.display()
            ));
        }
        CorpusRoot::OverrideAbsent { dir, source } => {
            return Err(external::override_absent(&dir, source));
        }
    };
    let source = if std::env::var_os("QRSCAN_EXTERNAL_CORPUS").is_some() {
        "QRSCAN_EXTERNAL_CORPUS"
    } else {
        "default"
    };
    let images = rows
        .iter()
        .filter(|row| external::is_image(&row.path))
        .filter_map(|row| {
            let set = if row.path.starts_with("zxing-blackbox/") {
                Set::Zxing
            } else if row.path.starts_with("qrcode-ai/") {
                Set::Gallery
            } else {
                return None;
            };
            sets.contains(&set).then(|| Image {
                set,
                group: external::group_of(&row.path),
                path: row.path.clone(),
                abs: dir.join(&row.path),
                sha256: row.sha256.clone(),
                bytes: 0,
            })
        })
        .collect();
    Ok((images, source))
}

/// The images to measure, every one read and hash-checked: an external
/// image must still carry its pinned sha256 (a drifted file is not the
/// frozen set — configuration error).
pub(crate) fn select(options: &Options) -> Result<(Vec<Image>, Inputs), String> {
    let root = crate::repo_root();
    let read =
        |name: &str| std::fs::read_to_string(root.join(name)).map_err(|e| format!("{name}: {e}"));
    let manifest_text = read(external::MANIFEST)?;
    let rows = external::parse_manifest(&manifest_text)?;
    let vendored_text = read("corpus.toml")?;
    let corpus: crate::Corpus =
        toml::from_str(&vendored_text).map_err(|e| format!("corpus.toml: {e}"))?;
    let mut available: BTreeMap<&'static str, usize> = BTreeMap::new();
    for row in rows.iter().filter(|r| external::is_image(&r.path)) {
        let set = if row.path.starts_with("zxing-blackbox/") {
            Set::Zxing
        } else {
            Set::Gallery
        };
        *available.entry(set.as_str()).or_default() += 1;
    }
    available.insert(Set::Vendored.as_str(), corpus.entry.len());

    let wants_external = options.sets.iter().any(|s| *s != Set::Vendored);
    let (mut images, corpus_root) = if wants_external {
        external_images(&options.sets, &rows)?
    } else {
        (Vec::new(), "none")
    };
    if options.sets.contains(&Set::Vendored) {
        images.extend(corpus.entry.iter().map(|entry| Image {
            set: Set::Vendored,
            group: format!("vendored/{}", entry.category),
            path: entry.path.clone(),
            abs: root.join(&entry.path),
            sha256: String::new(),
            bytes: 0,
        }));
    }
    images.sort_by(|a, b| (a.set, &a.group, &a.path).cmp(&(b.set, &b.group, &b.path)));
    if let Some(limit) = options.per_group {
        let mut taken: BTreeMap<String, usize> = BTreeMap::new();
        images.retain(|image| {
            let count = taken.entry(image.group.clone()).or_default();
            *count += 1;
            *count <= limit
        });
    }
    for image in &mut images {
        verify(image)?;
    }
    let mut selected: BTreeMap<&'static str, usize> = BTreeMap::new();
    for image in &images {
        *selected.entry(image.set.as_str()).or_default() += 1;
    }
    let mut inputs = Inputs {
        manifest_sha256: external::sha256_bytes(manifest_text.as_bytes()),
        vendored_sha256: external::sha256_bytes(vendored_text.as_bytes()),
        dispositions_sha256: dispositions_sha256(),
        corpus_root,
        available,
        selected,
        per_group: options.per_group,
        pins: Vec::new(),
    };
    inputs.pins = pin_findings(&inputs, &options.sets);
    Ok((images, inputs))
}

fn verify(image: &mut Image) -> Result<(), String> {
    let path = image.abs.to_str().unwrap_or_default();
    if path.is_empty() || path.contains(['\t', '\n', '\r']) {
        return Err(format!(
            "{}: path unusable on the worker protocol",
            image.path
        ));
    }
    let bytes = std::fs::read(&image.abs).map_err(|e| format!("{}: {e}", image.path))?;
    let sha256 = external::sha256_bytes(&bytes);
    if image.sha256.is_empty() {
        image.sha256 = sha256;
    } else if image.sha256 != sha256 {
        return Err(format!(
            "{}: sha256 differs from the pinned manifest — not the frozen set",
            image.path
        ));
    }
    image.bytes = bytes.len();
    Ok(())
}

// --------------------------------------------------------------- artifacts

/// A file the harness may run as a measured child: a Mach-O or ELF
/// executable. A script could start the real work in a grandchild the
/// memory sampler never sees.
pub(crate) fn native_executable(path: &Path) -> Result<(), String> {
    use std::io::Read as _;
    const MAGIC: [[u8; 4]; 6] = [
        *b"\x7fELF",
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xfe, 0xed, 0xfa, 0xce],
        [0xca, 0xfe, 0xba, 0xbe],
    ];
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut head = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .map_err(|e| format!("{name}: {}", e.kind()))?;
    if MAGIC.contains(&head) {
        Ok(())
    } else {
        Err(format!("{name}: not a Mach-O or ELF executable"))
    }
}

/// The node executable `--node-bin` names, resolved once through `node -p
/// process.execPath` so that no shim or wrapper is ever the measured child,
/// and refused unless native.
pub(crate) fn resolve_node(node: &str, caps: Caps) -> Result<PathBuf, String> {
    let done = proc::run_once(Command::new(node).args(["-p", "process.execPath"]), caps)
        .map_err(|e| format!("{node}: {e}"))?;
    if done.failure.is_some() || done.usage.exit != Exit::Code(0) {
        return Err(format!("{node} -p process.execPath did not answer"));
    }
    let path = PathBuf::from(String::from_utf8_lossy(&done.stdout).trim());
    if !path.is_absolute() {
        return Err(format!("{node}: process.execPath is not absolute"));
    }
    native_executable(&path)?;
    Ok(path)
}

/// The measured binary of an artifact: the file itself, or inside a
/// package directory its one `.node` addon or `_bg.wasm` module.
pub(crate) fn binary_of(path: &Path) -> Result<PathBuf, String> {
    if !path.is_dir() {
        return Ok(path.to_path_buf());
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let addon = p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("node"));
            let module = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.to_ascii_lowercase().ends_with("_bg.wasm"));
            addon || module
        })
        .collect();
    found.sort();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("{}: no .node or _bg.wasm binary", path.display())),
        _ => Err(format!("{}: more than one binary", path.display())),
    }
}

/// What a receipt records about one artifact's binary: its hash, the
/// compiler commits it embeds, its WASM recipe, and its build manifest
/// (`<file>.build.json`, or `build.json` in a package directory).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Described {
    pub(crate) binary: String,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
    pub(crate) embedded_rustc_commits: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) wasm: Option<super::wasm::Recipe>,
    pub(crate) manifest: Option<manifest::Manifest>,
    pub(crate) manifest_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) manifest_error: Option<String>,
}

pub(crate) fn describe(path: &Path) -> Result<Described, String> {
    let binary = binary_of(path)?;
    let bytes = std::fs::read(&binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    let wasm = binary
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wasm"))
        .then(|| super::wasm::recipe(&bytes))
        .transpose()?;
    let manifest_path = manifest::manifest_path(path);
    let (manifest, manifest_sha256, manifest_error) = if manifest_path.is_file() {
        let text = std::fs::read(&manifest_path).unwrap_or_default();
        match manifest::read(&manifest_path) {
            Ok(m) => (Some(m), Some(external::sha256_bytes(&text)), None),
            Err(e) => (None, Some(external::sha256_bytes(&text)), Some(e)),
        }
    } else {
        (None, None, None)
    };
    Ok(Described {
        binary: binary
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        sha256: external::sha256_bytes(&bytes),
        bytes: bytes.len() as u64,
        embedded_rustc_commits: manifest::embedded_rustc_commits(&bytes),
        wasm,
        manifest,
        manifest_sha256,
        manifest_error,
    })
}

/// One measured file.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ArtifactFile {
    pub(crate) name: String,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}

/// A runtime's artifact, as copied into the run directory.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Artifact {
    pub(crate) runtime: &'static str,
    pub(crate) variant: &'static str,
    pub(crate) files: Vec<ArtifactFile>,
    pub(crate) described: Described,
    #[serde(skip)]
    pub(crate) key: (Runtime, Variant),
    /// The copy the run executes (a file, or a package directory).
    #[serde(skip)]
    pub(crate) path: PathBuf,
}

fn copy_tree(
    from: &Path,
    to: &Path,
    prefix: &str,
    files: &mut Vec<ArtifactFile>,
) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .map_err(|e| format!("{}: {e}", from.display()))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let source = entry.path();
        let target = to.join(&name);
        let label = format!("{prefix}{name}");
        if source.is_dir() {
            copy_tree(&source, &target, &format!("{label}/"), files)?;
        } else {
            files.push(copy_file(&source, &target, &label)?);
        }
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path, label: &str) -> Result<ArtifactFile, String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let bytes = std::fs::read(from).map_err(|e| format!("{}: {e}", from.display()))?;
    std::fs::write(to, &bytes).map_err(|e| format!("{}: {e}", to.display()))?;
    let permissions = std::fs::metadata(from)
        .map_err(|e| format!("{}: {e}", from.display()))?
        .permissions();
    std::fs::set_permissions(to, permissions).map_err(|e| format!("{}: {e}", to.display()))?;
    Ok(ArtifactFile {
        name: label.to_owned(),
        sha256: external::sha256_bytes(&bytes),
        bytes: bytes.len() as u64,
    })
}

/// Copy every artifact — with its build manifest — into
/// `<out>/artifacts/<runtime>-<variant>/`, hash and describe it: the run
/// executes the copies, so nothing rebuilt mid-run can change what the
/// receipt names.
pub(crate) fn snapshot(options: &Options) -> Result<Vec<Artifact>, String> {
    let mut artifacts = Vec::new();
    for (&(runtime, variant), source) in &options.artifacts {
        if !options.runtimes.contains(&runtime) {
            continue;
        }
        let dir = options.out.join("artifacts").join(format!(
            "{}-{}",
            runtime.as_str(),
            variant.as_str()
        ));
        let mut files = Vec::new();
        let path = if source.is_dir() {
            copy_tree(source, &dir, "", &mut files)?;
            dir
        } else {
            let name = source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| format!("{}: not a file", source.display()))?;
            let target = dir.join(&name);
            files.push(copy_file(source, &target, &name)?);
            let stamp = manifest::manifest_path(source);
            if stamp.is_file() {
                let label = format!("{name}.build.json");
                files.push(copy_file(
                    &stamp,
                    &manifest::manifest_path(&target),
                    &label,
                )?);
            }
            target
        };
        if files.is_empty() {
            return Err(format!("{}: no files", source.display()));
        }
        artifacts.push(Artifact {
            runtime: runtime.as_str(),
            variant: variant.as_str(),
            files,
            described: describe(&path)?,
            key: (runtime, variant),
            path,
        });
    }
    Ok(artifacts)
}

/// A worker's `hello`, checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Hello {
    pub(crate) line: String,
    pub(crate) scanner: String,
    pub(crate) runtime: String,
    /// sha256 of the compiled `worker.rs` (library workers).
    pub(crate) worker_source: Option<String>,
    /// sha256 of the generated `main.rs` (library workers).
    pub(crate) main_source: Option<String>,
    /// The binary the Node or WASM script loaded, by file name.
    pub(crate) loaded: Option<String>,
}

pub(crate) fn parse_hello(line: &str) -> Result<Hello, String> {
    let fields: Vec<&str> = line.split('\t').collect();
    let field = |i: usize| {
        fields
            .get(i)
            .copied()
            .filter(|f| *f != "-" && !f.is_empty())
            .map(str::to_owned)
    };
    match fields.first().copied() {
        Some("hello") if fields.get(1).copied() == Some(super::worker::PROTOCOL) => {}
        Some("hello") => {
            return Err(format!(
                "the worker speaks {:?}, not {} — rebuild it from one prepare of this tree",
                fields.get(1).copied().unwrap_or("an older protocol"),
                super::worker::PROTOCOL
            ));
        }
        Some("fail") => return Err(format!("the worker refused: {}", fields[1..].join(" "))),
        _ => return Err(String::from("no hello")),
    }
    let runtime = field(5).unwrap_or_default();
    let (worker_source, main_source, loaded) = if runtime == "rust-worker" {
        (field(6), field(7), None)
    } else {
        (None, None, field(7))
    };
    Ok(Hello {
        line: line.to_owned(),
        scanner: field(2).unwrap_or_default(),
        runtime,
        worker_source,
        main_source,
        loaded,
    })
}

/// The parity findings of one runtime's pair.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Parity {
    pub(crate) runtime: &'static str,
    pub(crate) findings: Vec<Mismatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lock_delta: Option<LockDelta>,
}

/// Library workers must run one worker source: the same on both sides and
/// the same as this harness's own `worker.rs`.
pub(crate) fn worker_parity(base: &Hello, candidate: &Hello, own: &str) -> Vec<Mismatch> {
    let mut found = Vec::new();
    let sources = [
        ("base", base.worker_source.as_deref()),
        ("candidate", candidate.worker_source.as_deref()),
    ];
    for (side, source) in sources {
        if source != Some(own) {
            found.push(Mismatch {
                what: "worker_source",
                detail: format!(
                    "{side} worker compiled from worker.rs {}, the harness has {own}",
                    source.unwrap_or("(unknown)")
                ),
            });
        }
    }
    if base.main_source != candidate.main_source {
        found.push(Mismatch {
            what: "worker_source",
            detail: String::from("the two generated mains differ — not one prepare"),
        });
    }
    found
}

/// Parity of every runtime that has both variants: identical files,
/// compilers, profiles and manifests for native pairs, worker sources for
/// library workers, recipes for WASM packages.
pub(crate) fn parity(
    artifacts: &[Artifact],
    hellos: &BTreeMap<(Runtime, Variant), Hello>,
    own_worker: &str,
) -> Vec<Parity> {
    let mut rows = Vec::new();
    for &runtime in Runtime::ALL {
        let side = |variant: Variant| artifacts.iter().find(|a| a.key == (runtime, variant));
        let (Some(a), Some(b)) = (side(Variant::Base), side(Variant::Candidate)) else {
            continue;
        };
        let (da, db) = (&a.described, &b.described);
        let mut findings = Vec::new();
        let sides = (
            (
                da.sha256.as_str(),
                da.embedded_rustc_commits.as_slice(),
                da.manifest.as_ref(),
            ),
            (
                db.sha256.as_str(),
                db.embedded_rustc_commits.as_slice(),
                db.manifest.as_ref(),
            ),
        );
        if runtime.native() {
            findings.extend(manifest::native_parity(sides.0, sides.1));
        } else {
            findings.extend(manifest::wasm_parity(sides.0, sides.1));
            if da.wasm != db.wasm {
                findings.push(Mismatch {
                    what: "wasm_recipe",
                    detail: format!(
                        "producers, wasm-bindgen or SIMD differ: base {}, candidate {}",
                        serde_json::to_string(&da.wasm).unwrap_or_default(),
                        serde_json::to_string(&db.wasm).unwrap_or_default()
                    ),
                });
            }
        }
        if runtime == Runtime::Lib {
            match (
                hellos.get(&(runtime, Variant::Base)),
                hellos.get(&(runtime, Variant::Candidate)),
            ) {
                (Some(ha), Some(hb)) => findings.extend(worker_parity(ha, hb, own_worker)),
                _ => findings.push(Mismatch {
                    what: "worker_source",
                    detail: String::from("a library worker did not say hello"),
                }),
            }
        }
        if matches!(runtime, Runtime::Node | Runtime::Wasm) {
            // the file the driver loaded, by its path in the package, must
            // be the one this receipt hashes
            for (variant, described) in [(Variant::Base, da), (Variant::Candidate, db)] {
                let loaded = hellos
                    .get(&(runtime, variant))
                    .and_then(|h| h.loaded.as_deref());
                if loaded != Some(described.binary.as_str()) {
                    findings.push(Mismatch {
                        what: "loaded",
                        detail: format!(
                            "{}: the driver loaded {}, the receipt hashes {}",
                            variant.as_str(),
                            loaded.unwrap_or("(nothing)"),
                            described.binary
                        ),
                    });
                }
            }
        }
        let lock_delta = match (
            da.manifest.as_ref().and_then(|m| m.lock.as_ref()),
            db.manifest.as_ref().and_then(|m| m.lock.as_ref()),
        ) {
            (Some(la), Some(lb)) => Some(manifest::lock_delta(la, lb)),
            _ => None,
        };
        rows.push(Parity {
            runtime: runtime.as_str(),
            findings,
            lock_delta,
        });
    }
    rows
}

// ----------------------------------------------------------------- samples

/// One decoded unit as it may be kept: symbology, text sha256, length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Unit {
    pub(crate) symbology: String,
    pub(crate) text_sha256: String,
    pub(crate) text_len: usize,
}

impl Unit {
    pub(crate) fn of(symbology: &str, text: &[u8]) -> Self {
        Self {
            symbology: symbology.to_owned(),
            text_sha256: external::sha256_bytes(text),
            text_len: text.len(),
        }
    }
}

/// sha256 over the ordered `symbology<TAB>text_sha256` lines — the
/// oracle's output signature (the one a `known_wrong` disposition pins).
pub(crate) fn signature(units: &[Unit]) -> String {
    let mut lines = String::new();
    for unit in units {
        let _ = writeln!(lines, "{}\t{}", unit.symbology, unit.text_sha256);
    }
    external::sha256_bytes(lines.as_bytes())
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(String::from("odd hex length"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| String::from("bad hex"))
        })
        .collect()
}

/// The walk a call made: stages run, Σ `transforms_tried`, and the
/// judgment's signature ([`super::worker::judgment`]; `None` when no score
/// came back).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Work {
    pub(crate) stages: u32,
    pub(crate) transforms: u64,
    pub(crate) judgment: Option<String>,
}

/// One completed call of the public API (an `ok` or an `error` reply).
#[derive(Debug, Clone, Default)]
pub(crate) struct Sample {
    /// 0 = warm-up.
    pub(crate) rep: u32,
    /// Position in the image's call order, warm-ups included.
    pub(crate) slot: u32,
    pub(crate) ns: u64,
    pub(crate) total_ms: Option<f64>,
    pub(crate) engine_panics: Option<u64>,
    /// `None` when the call failed.
    pub(crate) units: Option<Vec<Unit>>,
    pub(crate) error: Option<String>,
    /// In-process heap of a counted (warm-up) call.
    pub(crate) heap: Option<HeapStats>,
    /// Bytes a counted call left allocated once its report was dropped.
    pub(crate) retained: Option<i64>,
    /// Peak RSS of a one-shot CLI process.
    pub(crate) rss: Option<u64>,
    pub(crate) cpu_us: Option<u64>,
    pub(crate) work: Option<Work>,
}

impl Sample {
    /// The ladder's own clock reached the preset budget. Information only
    /// (an overrun of the ladder): a walk can complete past its deadline,
    /// so time never decides truncation.
    pub(crate) fn reached_budget(&self, mode: Mode) -> bool {
        mode.budget_ms()
            .zip(self.total_ms)
            .is_some_and(|(budget, total)| total >= ms(budget * 1_000_000))
    }

    /// The walk differs from the image's unbudgeted reference walk: other
    /// stages or Σ transforms, or other detections — what a deadline does
    /// when it stops the ladder before the reference walk's end.
    pub(crate) fn walk_cut(&self, reference: &Sample) -> bool {
        let walk = |s: &Sample| s.work.as_ref().map(|w| (w.stages, w.transforms));
        walk(self) != walk(reference) || self.units != reference.units
    }

    /// The judgment differs from the reference's: absent, or an axis
    /// stopped short (scoring profiles only).
    pub(crate) fn judgment_cut(&self, reference: &Sample, mode: Mode) -> bool {
        let judgment = |s: &Sample| s.work.as_ref().and_then(|w| w.judgment.clone());
        mode.scores() && judgment(self) != judgment(reference)
    }

    /// Truncated by the budget: its walk or its judgment differs from the
    /// reference's. A call that completes the reference walk is complete,
    /// whatever its time.
    pub(crate) fn truncated(&self, reference: &Sample, mode: Mode) -> bool {
        self.walk_cut(reference) || self.judgment_cut(reference, mode)
    }
}

fn opt_u64(field: &str) -> Result<Option<u64>, String> {
    if field == "-" {
        Ok(None)
    } else {
        field
            .parse()
            .map(Some)
            .map_err(|e| format!("{field:?}: {e}"))
    }
}

/// Parse a worker reply ([`super::worker::PROTOCOL`]); text is hashed on
/// arrival. `fail` — the request could not run — is an error.
pub(crate) fn parse_reply(reply: &str, rep: u32) -> Result<Sample, String> {
    let fields: Vec<&str> = reply.split('\t').collect();
    let at = |i: usize| {
        fields
            .get(i)
            .copied()
            .ok_or_else(|| format!("short reply ({} fields)", fields.len()))
    };
    let int = |i: usize| at(i)?.parse::<u64>().map_err(|e| format!("field {i}: {e}"));
    match fields.first().copied() {
        Some("ok") => {
            let heap = match (opt_u64(at(4)?)?, opt_u64(at(5)?)?, opt_u64(at(6)?)?) {
                (Some(peak), Some(calls), Some(bytes)) => Some(HeapStats { peak, calls, bytes }),
                _ => None,
            };
            let retained = match at(7)? {
                "-" => None,
                v => Some(v.parse::<i64>().map_err(|e| format!("retained: {e}"))?),
            };
            let work = Work {
                stages: u32::try_from(int(8)?).map_err(|e| e.to_string())?,
                transforms: int(9)?,
                judgment: match at(10)? {
                    "-" => None,
                    signature if signature.starts_with('v') => Some(signature.to_owned()),
                    other => return Err(format!("judgment {other:?}")),
                },
            };
            let count = usize::try_from(int(11)?).map_err(|e| e.to_string())?;
            if fields.len() != 12 + 2 * count {
                return Err(format!("{} fields for {count} detections", fields.len()));
            }
            let mut units = Vec::with_capacity(count);
            for k in 0..count {
                units.push(Unit::of(at(12 + 2 * k)?, &unhex(at(13 + 2 * k)?)?));
            }
            Ok(Sample {
                rep,
                ns: int(1)?,
                total_ms: Some(
                    at(2)?
                        .parse::<f64>()
                        .map_err(|e| format!("total_ms: {e}"))?,
                ),
                engine_panics: Some(int(3)?),
                units: Some(units),
                heap,
                retained,
                work: Some(work),
                ..Sample::default()
            })
        }
        Some("error") => Ok(Sample {
            rep,
            ns: int(1)?,
            error: Some(at(2)?.to_owned()),
            ..Sample::default()
        }),
        Some("fail") => Err(format!("worker: {}", fields[1..].join(" "))),
        _ => Err(String::from("unexpected worker reply")),
    }
}

/// The judgment signature of a JSON report's `score` — the string
/// [`super::worker::judgment`] builds in-process; `None` when there is no
/// score. `Err` when a score is there but of another shape.
pub(crate) fn json_judgment(score: &Value) -> Result<Option<String>, String> {
    if score.is_null() {
        return Ok(None);
    }
    let value = score["value"].as_u64().ok_or("score without a value")?;
    // a pre-0.9 report omits `weights_run`; serde reads it as 0
    let weights = score["weights_run"].as_u64().unwrap_or(0);
    let mut out = format!("v{value}w{weights}");
    for axis in score["axes"].as_array().ok_or("score without axes")? {
        let (Some(passed), Some(total)) = (axis["passed"].as_u64(), axis["total"].as_u64()) else {
            return Err(String::from("axis without passed/total"));
        };
        let failed = if axis["failed_at"].is_null() { "" } else { "x" };
        let _ = write!(out, ",{passed}/{total}{failed}");
    }
    Ok(Some(out))
}

/// The walk of a CLI report (`trace.stages`, `score`).
fn report_work(report: &Value) -> Result<Work, String> {
    let stages = report["trace"]["stages"]
        .as_array()
        .ok_or("report without trace stages")?;
    let mut transforms = 0;
    for stage in stages {
        transforms += stage["transforms_tried"]
            .as_u64()
            .ok_or("stage without transforms_tried")?;
    }
    Ok(Work {
        stages: u32::try_from(stages.len()).map_err(|e| e.to_string())?,
        transforms,
        judgment: json_judgment(&report["score"])?,
    })
}

/// How one call ended.
#[derive(Debug, Clone)]
pub(crate) enum Call {
    /// An `ok` or `error` reply: a completed call.
    Done(Sample),
    /// A resource failure: a cap, a signal, or a worker that died.
    Failed(Failure),
}

/// One `qrscan` process: outer wall time, its own peak RSS and CPU, and
/// the report's trace and detections (parsed, text hashed, then dropped).
/// `unbounded` runs the profile without its budget (`--budget-ms 0`): the
/// full-unbounded mode, and every reference walk.
fn cli_call(
    bin: &Path,
    (mode, unbounded): (Mode, bool),
    image: &Image,
    rep: u32,
    caps: Caps,
) -> Result<Call, String> {
    let mut command = Command::new(bin);
    command.arg(&image.abs).args(["--profile", mode.profile()]);
    if unbounded || mode == Mode::FullUnbounded {
        command.args(["--budget-ms", "0"]);
    }
    let done = proc::run_once(&mut command, caps)?;
    if let Some(failure) = done.failure {
        return Ok(Call::Failed(failure));
    }
    let mut sample = Sample {
        rep,
        ns: done.ns,
        rss: done.usage.max_rss,
        cpu_us: done
            .usage
            .user_us
            .zip(done.usage.sys_us)
            .map(|(u, s)| u + s),
        ..Sample::default()
    };
    match done.usage.exit {
        Exit::Code(0 | 1) => {
            let report: Value = serde_json::from_slice(&done.stdout)
                .map_err(|e| format!("{}: report JSON: {e}", image.path))?;
            sample.total_ms = report["trace"]["total_ms"].as_f64();
            sample.engine_panics = report["trace"]["engine_panics"].as_u64();
            sample.work = Some(report_work(&report).map_err(|e| format!("{}: {e}", image.path))?);
            let units = report["detections"]
                .as_array()
                .ok_or_else(|| format!("{}: no detections array", image.path))?
                .iter()
                .map(|d| {
                    let symbology = d["symbology"].as_str().unwrap_or("?");
                    let text = d["content"]["text"].as_str().unwrap_or_default();
                    Unit::of(symbology, text.as_bytes())
                })
                .collect();
            sample.units = Some(units);
        }
        Exit::Code(code) => {
            sample.error = Some(
                done.stderr
                    .code()
                    .map_or_else(|| format!("exit {code}"), str::to_owned),
            );
        }
        Exit::Signal(signal) => {
            return Ok(Call::Failed(Failure {
                kind: proc::Kind::Signal(signal),
                peak_rss: done.usage.max_rss,
                wall_ns: done.ns,
            }));
        }
    }
    Ok(Call::Done(sample))
}

fn script(name: &str) -> PathBuf {
    crate::repo_root().join("scripts").join(name)
}

/// The end of one worker process.
#[derive(Debug, Clone)]
pub(crate) struct ProcessEnd {
    pub(crate) hello: String,
    /// The larger of the kernel's peak RSS and the watchdog's peak sample.
    pub(crate) peak: Option<u64>,
    /// `bye`, plus the wasm linear-memory high-water mark for wasm.
    pub(crate) farewell: String,
    /// `quit` · `rss` · `wall` · `signal` · `exit` · `unsampled` ·
    /// `exception` · `abandoned`.
    pub(crate) ended: String,
    /// A resource failure ended this incarnation (its call is counted
    /// there): its peak never enters the A/B worker peak.
    pub(crate) capped: bool,
    /// A cap or a signal that ended the worker during `quit`, after its
    /// last reply: a resource failure of its own.
    pub(crate) quit_failure: Option<Failure>,
    pub(crate) stderr: proc::Stderr,
}

fn process_end(hello: &str, end: &proc::End, ended: &str, capped: bool) -> ProcessEnd {
    ProcessEnd {
        hello: hello.to_owned(),
        peak: end.peak(),
        farewell: end.farewell.clone(),
        ended: end
            .failure
            .map_or_else(|| ended.to_owned(), |f| f.kind.as_str().to_owned()),
        capped: capped || end.failure.is_some(),
        quit_failure: end.failure,
        stderr: end.stderr.clone(),
    }
}

// ---------------------------------------------------------------- segments

/// What happened to a segment (or a throughput run, or the WASM cost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Measured,
    /// Every attempt crossed the gate — no number kept.
    Discarded,
    /// The gate never opened before the deadline (or the dry run's host
    /// floor stopped the run first).
    NotRun,
    /// A protocol failure, or a resource failure in a throughput pass or a
    /// cold start.
    Failed,
}
named!(Outcome { Measured => "measured", Discarded => "discarded", NotRun => "not_run", Failed => "failed" });

/// A call a resource failure ended, with where it happened.
#[derive(Debug, Clone)]
pub(crate) struct CallFailure {
    pub(crate) image: usize,
    pub(crate) variant: Variant,
    pub(crate) rep: u32,
    pub(crate) slot: u32,
    pub(crate) attempt: u32,
    /// The image's unbudgeted reference walk, not one of its counted calls.
    pub(crate) reference: bool,
    pub(crate) failure: Failure,
}

/// One runtime × mode over every selected image.
pub(crate) struct Segment {
    pub(crate) runtime: Runtime,
    pub(crate) mode: Mode,
    pub(crate) outcome: Outcome,
    pub(crate) note: Option<String>,
    pub(crate) attempts: u32,
    pub(crate) readings: Vec<Reading>,
    /// Budgeted modes: image index → variant → the uncounted reference
    /// call without a budget.
    pub(crate) references: BTreeMap<usize, BTreeMap<Variant, Sample>>,
    /// image index → variant → completed calls, warm-up first.
    pub(crate) samples: BTreeMap<usize, BTreeMap<Variant, Vec<Sample>>>,
    /// Every resource failure of every attempt — never cleared.
    pub(crate) failures: Vec<CallFailure>,
    /// Every worker process of the kept attempt, respawns included.
    pub(crate) processes: BTreeMap<Variant, Vec<ProcessEnd>>,
}

impl Segment {
    /// Drop what an attempt measured (its resource failures stay).
    fn discard(&mut self) {
        self.references.clear();
        self.samples.clear();
        self.processes.clear();
    }
}

/// The A B B A order of `reps` runs per variant (one variant: its runs
/// back to back).
pub(crate) fn abba(variants: &[Variant], reps: u32) -> Vec<Variant> {
    match variants {
        [only] => vec![*only; reps as usize],
        [a, b] => (0..2 * reps)
            .map(|k| if matches!(k % 4, 0 | 3) { *a } else { *b })
            .collect(),
        _ => Vec::new(),
    }
}

/// The calls of image `index`: one warm-up per variant, then `reps` timed
/// calls each in A B B A blocks — A first on even-indexed images, B first
/// on odd ones, warm-ups in the same order — so the slot offset of a
/// monotone drift cancels within a class. Each entry is (variant, rep); its
/// position is the call's slot.
pub(crate) fn image_order(variants: &[Variant], reps: u32, index: usize) -> Vec<(Variant, u32)> {
    let mut first: Vec<Variant> = variants.to_vec();
    if index % 2 == 1 {
        first.reverse();
    }
    let mut counts: BTreeMap<Variant, u32> = BTreeMap::new();
    let mut order: Vec<(Variant, u32)> = first.iter().map(|v| (*v, 0)).collect();
    for variant in abba(&first, reps) {
        let rep = counts.entry(variant).or_default();
        *rep += 1;
        order.push((variant, *rep));
    }
    order
}

/// Shared state of one run.
pub(crate) struct Context<'a> {
    pub(crate) options: &'a Options,
    pub(crate) gate: &'a Gate,
    pub(crate) images: &'a [Image],
    pub(crate) artifacts: BTreeMap<(Runtime, Variant), PathBuf>,
    pub(crate) deadline: Instant,
    pub(crate) fixture: PathBuf,
    /// Set once a dry run's host floor stopped the run: no later phase starts.
    pub(crate) stopped: RefCell<Option<String>>,
}

/// One variant's driver within an attempt: a persistent worker (respawned
/// after a resource failure) or the CLI binary.
struct Side {
    runtime: Runtime,
    variant: Variant,
    label: String,
    server: Option<Server>,
    hello: String,
    /// The worker's memory cap (a throughput worker's scales with threads).
    rss_cap: u64,
    ends: Vec<ProcessEnd>,
}

impl Context<'_> {
    fn variants(&self, runtime: Runtime) -> Vec<Variant> {
        Variant::ALL
            .iter()
            .copied()
            .filter(|v| self.artifacts.contains_key(&(runtime, *v)))
            .collect()
    }

    fn path(&self, runtime: Runtime, variant: Variant) -> Result<&PathBuf, String> {
        self.artifacts
            .get(&(runtime, variant))
            .ok_or_else(|| format!("no {} {} artifact", runtime.as_str(), variant.as_str()))
    }

    fn command(&self, runtime: Runtime, path: &Path) -> Command {
        match runtime {
            Runtime::Lib | Runtime::Cli => Command::new(path),
            Runtime::Node | Runtime::Wasm => {
                let name = if runtime == Runtime::Node {
                    "bench-node.mjs"
                } else {
                    "bench-wasm.mjs"
                };
                let mut node = Command::new(&self.options.node);
                node.arg(script(name)).arg("serve").arg(path);
                node
            }
        }
    }

    /// Spawn a worker under the memory cap `rss_cap` and check its `hello`.
    fn hello(
        &self,
        runtime: Runtime,
        variant: Variant,
        rss_cap: u64,
    ) -> Result<(Server, Hello), String> {
        let label = format!("{}-{}", runtime.as_str(), variant.as_str());
        let path = self.path(runtime, variant)?;
        let mut server = Server::spawn(self.command(runtime, path), &label, rss_cap)?;
        let line = server
            .request("hello", HELLO_WALL)
            .map_err(|e| format!("{label}: hello: {e}"))?;
        let hello = parse_hello(&line).map_err(|e| format!("{label}: {e}"))?;
        Ok((server, hello))
    }

    /// A ready driver: the worker spawned under `rss_cap`, its `hello`
    /// checked, then one uncounted priming scan of the repository fixture
    /// in `mode`.
    fn start(
        &self,
        runtime: Runtime,
        variant: Variant,
        (mode, rss_cap): (Mode, u64),
    ) -> Result<Side, String> {
        let label = format!("{}-{}", runtime.as_str(), variant.as_str());
        if runtime == Runtime::Cli {
            return Ok(Side {
                runtime,
                variant,
                label,
                server: None,
                hello: String::new(),
                rss_cap,
                ends: Vec::new(),
            });
        }
        let (mut server, hello) = self.hello(runtime, variant, rss_cap)?;
        let line = scan_line(mode, mode.budget(), &self.fixture, false);
        let primed = server
            .request(&line, mode.wall_cap())
            .map_err(|e| format!("{label}: priming scan: {e}"))?;
        let sample = parse_reply(&primed, 0).map_err(|e| format!("{label}: priming scan: {e}"))?;
        if sample.units.is_none() {
            return Err(format!("{label}: priming scan failed"));
        }
        Ok(Side {
            runtime,
            variant,
            label,
            server: Some(server),
            hello: hello.line,
            rss_cap,
            ends: Vec::new(),
        })
    }

    /// Before a segment or phase: a dry run reads the host floor (and stops
    /// the run when it is crossed); a gated run waits for an open gate, at
    /// least [`SEGMENT_WAIT`], or gives up.
    fn before(&self, phase: &str) -> Result<Vec<Reading>, Vec<Reading>> {
        if self.stopped.borrow().is_some() {
            return Err(Vec::new());
        }
        if self.options.dry_run {
            let reading = self.gate.floor(phase);
            println!("{} (dry run: gate not enforced)", reading.line());
            if !reading.open {
                *self.stopped.borrow_mut() =
                    Some(format!("host floor crossed — {}", reading.line()));
                return Err(vec![reading]);
            }
            return Ok(vec![reading]);
        }
        let deadline = self.deadline.max(Instant::now() + SEGMENT_WAIT);
        let (last, kept) = self.gate.wait(phase, deadline);
        if last.open { Ok(kept) } else { Err(kept) }
    }

    /// A reading inside a phase: the gate with the elapsed-bounded own
    /// allowance (gated runs), or the host floor (dry runs).
    fn during(&self, phase: &str, threads: usize, elapsed: Duration) -> Reading {
        if self.options.dry_run {
            self.gate.floor(phase)
        } else {
            self.gate.read(phase, gate::own_allowance(threads, elapsed))
        }
    }

    fn stop(&self, reading: &Reading) {
        if self.options.dry_run && !reading.open {
            *self.stopped.borrow_mut() = Some(format!("host floor crossed — {}", reading.line()));
        }
    }
}

/// A `scan` request; `budget` is `default` or `unbounded`.
fn scan_line(mode: Mode, budget: &str, path: &Path, memory: bool) -> String {
    format!(
        "scan\t{}\t{budget}\t{}\t{}",
        mode.profile(),
        u8::from(memory),
        path.display()
    )
}

impl Side {
    /// One call; a protocol failure is an error (the segment fails).
    fn call(
        &mut self,
        ctx: &Context<'_>,
        mode: Mode,
        image: &Image,
        rep: u32,
    ) -> Result<Call, String> {
        self.request(ctx, (mode, false), image, rep)
    }

    /// The image's reference walk: the same profile without its budget,
    /// uncounted, under the unbounded mode's caps.
    fn reference(&mut self, ctx: &Context<'_>, mode: Mode, image: &Image) -> Result<Call, String> {
        self.request(ctx, (mode, true), image, 0)
    }

    fn request(
        &mut self,
        ctx: &Context<'_>,
        (mode, unbounded): (Mode, bool),
        image: &Image,
        rep: u32,
    ) -> Result<Call, String> {
        let (budget, wall) = if unbounded {
            ("unbounded", Mode::FullUnbounded.wall_cap())
        } else {
            (mode.budget(), mode.wall_cap())
        };
        let caps = ctx.options.caps(wall);
        let Some(server) = self.server.as_mut() else {
            let bin = ctx.path(self.runtime, self.variant)?;
            return cli_call(bin, (mode, unbounded), image, rep, caps)
                .map_err(|e| format!("{}: {}: {e}", self.label, image.path));
        };
        let memory = rep == 0 && !unbounded && self.runtime == Runtime::Lib;
        match server.request(&scan_line(mode, budget, &image.abs, memory), caps.wall) {
            Ok(reply) => parse_reply(&reply, rep)
                .map(Call::Done)
                .map_err(|e| format!("{}: {}: {e}", self.label, image.path)),
            Err(CallError::Died(failure)) => Ok(Call::Failed(failure)),
            Err(e) => Err(format!("{}: {}: {e}", self.label, image.path)),
        }
    }

    /// Replace a dead (or poisoned) worker with a fresh, primed one;
    /// `capped` when a resource failure ended the old incarnation.
    fn respawn(
        &mut self,
        ctx: &Context<'_>,
        mode: Mode,
        (why, capped): (&str, bool),
    ) -> Result<(), String> {
        if let Some(server) = self.server.take() {
            let end = server.kill();
            self.ends.push(process_end(&self.hello, &end, why, capped));
            let fresh = ctx.start(self.runtime, self.variant, (mode, self.rss_cap))?;
            self.server = fresh.server;
            self.hello = fresh.hello;
        }
        Ok(())
    }

    /// Kill the worker and start no successor (an abandoned pass).
    fn retire(&mut self, why: &str) {
        if let Some(server) = self.server.take() {
            let end = server.kill();
            self.ends.push(process_end(&self.hello, &end, why, false));
        }
    }

    fn finish(mut self) -> Vec<ProcessEnd> {
        if let Some(server) = self.server.take() {
            let end = server.finish();
            self.ends
                .push(process_end(&self.hello, &end, "quit", false));
        }
        self.ends
    }
}

fn median_ms(samples: &[Sample]) -> Option<f64> {
    let timed: Vec<f64> = samples
        .iter()
        .filter(|s| s.rep > 0 && s.error.is_none())
        .map(|s| ms(s.ns))
        .collect();
    super::stats::median(&timed)
}

pub(crate) fn ms(ns: u64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "durations stay far below 2^52 ns"
    )]
    let ns = ns as f64;
    ns / 1e6
}

/// A resource failure: logged, kept with where it happened, the variant
/// abandoned for the image and its worker respawned.
fn failed_call(
    ctx: &Context<'_>,
    segment: &mut Segment,
    side: &mut Side,
    (index, rep, slot, reference): (usize, u32, u32, bool),
    failure: Failure,
) -> Result<(), String> {
    println!(
        "{}/{} {} {}{}: resource failure ({}) — {} respawned, image abandoned for it",
        segment.runtime.as_str(),
        segment.mode.as_str(),
        ctx.images[index].path,
        side.variant.letter(),
        if reference { " reference" } else { "" },
        failure.kind.as_str(),
        side.label
    );
    segment.failures.push(CallFailure {
        image: index,
        variant: side.variant,
        rep,
        slot,
        attempt: segment.attempts,
        reference,
        failure,
    });
    side.respawn(ctx, segment.mode, (failure.kind.as_str(), true))
}

/// The reference walks of a budgeted segment: per image, each variant once
/// without a budget, uncounted, the gate read before every image. Returns
/// the variants each image lost to a resource failure, or the reading that
/// crossed the gate.
fn reference_pass(
    ctx: &Context<'_>,
    segment: &mut Segment,
    sides: &mut BTreeMap<Variant, Side>,
    variants: &[Variant],
    started: Instant,
) -> Result<Result<BTreeMap<usize, BTreeSet<Variant>>, Reading>, String> {
    let phase = format!("{}/{}", segment.runtime.as_str(), segment.mode.as_str());
    let mut abandoned: BTreeMap<usize, BTreeSet<Variant>> = BTreeMap::new();
    for (index, image) in ctx.images.iter().enumerate() {
        let reading = ctx.during(&format!("{phase} reference"), 1, started.elapsed());
        if !reading.open {
            ctx.stop(&reading);
            return Ok(Err(reading));
        }
        // the warm-up order: A first on even images, B first on odd ones
        for (variant, _) in image_order(variants, 0, index) {
            let side = sides.get_mut(&variant).ok_or("driver")?;
            match side.reference(ctx, segment.mode, image)? {
                Call::Done(sample) => {
                    segment
                        .references
                        .entry(index)
                        .or_default()
                        .insert(variant, sample);
                }
                Call::Failed(failure) => {
                    failed_call(ctx, segment, side, (index, 0, 0, true), failure)?;
                    abandoned.entry(index).or_default().insert(variant);
                }
            }
        }
    }
    Ok(Ok(abandoned))
}

/// One attempt at a segment; `Ok(Some(reading))` when the gate (or a dry
/// run's host floor) was crossed.
fn attempt(ctx: &Context<'_>, segment: &mut Segment) -> Result<Option<Reading>, String> {
    let (runtime, mode) = (segment.runtime, segment.mode);
    let variants = ctx.variants(runtime);
    let mut sides: BTreeMap<Variant, Side> = BTreeMap::new();
    for &variant in &variants {
        sides.insert(
            variant,
            ctx.start(runtime, variant, (mode, ctx.options.rss_cap()))?,
        );
    }
    let phase = format!("{}/{}", runtime.as_str(), mode.as_str());
    let started = Instant::now();
    let mut lost = BTreeMap::new();
    if mode.budgeted() {
        match reference_pass(ctx, segment, &mut sides, &variants, started)? {
            Ok(abandoned) => lost = abandoned,
            Err(crossed) => return Ok(Some(crossed)),
        }
    }
    for (index, image) in ctx.images.iter().enumerate() {
        let reading = ctx.during(&format!("{phase} image"), 1, started.elapsed());
        if !reading.open {
            ctx.stop(&reading);
            return Ok(Some(reading));
        }
        let mut runs: BTreeMap<Variant, Vec<Sample>> = BTreeMap::new();
        let mut abandoned: BTreeSet<Variant> = lost.remove(&index).unwrap_or_default();
        for (slot, (variant, rep)) in image_order(&variants, ctx.options.reps, index)
            .into_iter()
            .enumerate()
        {
            if abandoned.contains(&variant) {
                continue;
            }
            let slot = u32::try_from(slot).unwrap_or(u32::MAX);
            let side = sides.get_mut(&variant).ok_or("driver")?;
            match side.call(ctx, mode, image, rep)? {
                Call::Done(mut sample) => {
                    sample.slot = slot;
                    // a trapped wasm instance cannot be trusted with the next call
                    let trapped =
                        runtime == Runtime::Wasm && sample.error.as_deref() == Some("exception");
                    runs.entry(variant).or_default().push(sample);
                    if trapped {
                        side.respawn(ctx, mode, ("exception", false))?;
                    }
                }
                Call::Failed(failure) => {
                    failed_call(ctx, segment, side, (index, rep, slot, false), failure)?;
                    abandoned.insert(variant);
                }
            }
        }
        let mut line = format!(
            "{phase} [{}/{}] {}",
            index + 1,
            ctx.images.len(),
            image.path
        );
        for (variant, samples) in &runs {
            if let Some(median) = median_ms(samples) {
                let _ = write!(line, " · {} {median:.1} ms", variant.letter());
            }
        }
        println!("{line}");
        segment.samples.insert(index, runs);
    }
    for (variant, side) in sides {
        segment.processes.insert(variant, side.finish());
    }
    Ok(None)
}

/// A segment with its gate log: retried after a crossing (the crossed
/// attempt is discarded whole), at most [`ATTEMPTS`] attempts.
pub(crate) fn run_segment(ctx: &Context<'_>, runtime: Runtime, mode: Mode) -> Segment {
    let mut segment = Segment {
        runtime,
        mode,
        outcome: Outcome::NotRun,
        note: None,
        attempts: 0,
        readings: Vec::new(),
        references: BTreeMap::new(),
        samples: BTreeMap::new(),
        failures: Vec::new(),
        processes: BTreeMap::new(),
    };
    let phase = format!("{}/{}", runtime.as_str(), mode.as_str());
    while segment.attempts < ATTEMPTS {
        segment.attempts += 1;
        let started = Instant::now();
        match ctx.before(&format!("{phase} start")) {
            Ok(readings) => segment.readings.extend(readings),
            Err(readings) => {
                segment.readings.extend(readings);
                segment.note =
                    Some(ctx.stopped.borrow().clone().unwrap_or_else(|| {
                        String::from("the gate stayed closed until the deadline")
                    }));
                return segment;
            }
        }
        match attempt(ctx, &mut segment) {
            Ok(None) => {
                let after = ctx.during(&format!("{phase} after"), 1, started.elapsed());
                let held = ctx.options.dry_run || after.open;
                ctx.stop(&after);
                segment.readings.push(after);
                if held {
                    segment.outcome = Outcome::Measured;
                    return segment;
                }
                segment.outcome = Outcome::Discarded;
            }
            Ok(Some(crossed)) => {
                println!(
                    "{} — attempt {} discarded",
                    crossed.line(),
                    segment.attempts
                );
                segment.readings.push(crossed);
                segment.outcome = Outcome::Discarded;
                if ctx.stopped.borrow().is_some() {
                    segment.note.clone_from(&ctx.stopped.borrow());
                    segment.discard();
                    return segment;
                }
            }
            Err(e) => {
                eprintln!("bench: {phase}: {e}");
                segment.outcome = Outcome::Failed;
                segment.note = Some(e);
                segment.discard();
                return segment;
            }
        }
        segment.discard();
    }
    segment.note = Some(String::from("every attempt crossed the gate"));
    segment
}

// -------------------------------------------------------------- throughput

/// One pass of every selected image through a bounded queue.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Pass {
    pub(crate) variant: &'static str,
    pub(crate) jobs: u64,
    pub(crate) ns: u64,
    /// Jobs whose scan returned an error or panicked: never counted as
    /// images per second.
    pub(crate) failed: u64,
}

impl Pass {
    /// Completed scans per second.
    pub(crate) fn rate(&self) -> Option<f64> {
        let seconds = ms(self.ns) / 1e3;
        #[expect(clippy::cast_precision_loss, reason = "job counts are tiny")]
        let done = self.jobs.saturating_sub(self.failed) as f64;
        (seconds > 0.0).then(|| done / seconds)
    }
}

/// A throughput pass a resource failure ended.
#[derive(Debug, Clone)]
pub(crate) struct PassFailure {
    pub(crate) variant: Variant,
    pub(crate) pass: usize,
    pub(crate) failure: Failure,
}

pub(crate) struct Throughput {
    pub(crate) mode: Mode,
    pub(crate) threads: usize,
    /// Capacity of the job queue (indices; in-flight scans equal threads).
    pub(crate) bound: usize,
    pub(crate) wall_cap: Duration,
    /// The workers' memory cap, bytes ([`throughput_cap`]).
    pub(crate) rss_cap: u64,
    pub(crate) passes: Vec<Pass>,
    pub(crate) failures: Vec<PassFailure>,
    pub(crate) outcome: Outcome,
    pub(crate) note: Option<String>,
    pub(crate) readings: Vec<Reading>,
    /// The workers of this thread count: peak RSS per variant.
    pub(crate) processes: BTreeMap<Variant, Vec<ProcessEnd>>,
}

/// A throughput worker's memory cap: a quarter of the per-call cap per
/// thread — 256 MiB at the default 1024 MiB — and never above three
/// per-call caps (3072 MiB). The images a pass preloads count against it
/// too.
pub(crate) fn throughput_cap(threads: usize, rss_cap_mib: u64) -> u64 {
    let threads = u64::try_from(threads).unwrap_or(u64::MAX);
    let per_call = rss_cap_mib << 20;
    (per_call / 4).saturating_mul(threads).min(3 * per_call)
}

/// 1, 4 and all logical cores (deduplicated, ascending).
pub(crate) fn thread_counts(cores: usize) -> Vec<usize> {
    let mut counts = vec![1, 4.min(cores.max(1)), cores.max(1)];
    counts.dedup();
    counts
}

/// The image list of the throughput passes: absolute paths, so it lives
/// outside the run directory and is deleted when the passes end.
struct ListFile(PathBuf);

impl ListFile {
    fn write(images: &[Image], mode: Mode) -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "qrscan-bench-{}-{}.txt",
            std::process::id(),
            mode.as_str()
        ));
        let mut text = String::new();
        for image in images {
            let _ = writeln!(text, "{}", image.abs.display());
        }
        std::fs::write(&path, text).map_err(|e| format!("throughput list: {e}"))?;
        Ok(Self(path))
    }
}

impl Drop for ListFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn parse_pass(reply: &str, variant: Variant) -> Result<Pass, String> {
    let fields: Vec<&str> = reply.split('\t').collect();
    let int = |i: usize| -> Result<u64, String> {
        fields
            .get(i)
            .and_then(|f| f.parse().ok())
            .ok_or_else(|| format!("throughput reply: {}", fields.first().unwrap_or(&"?")))
    };
    if fields.first() != Some(&"ok") || fields.len() != 4 {
        return Err(format!("throughput reply: {}", fields.join(" ")));
    }
    Ok(Pass {
        variant: variant.as_str(),
        jobs: int(1)?,
        ns: int(2)?,
        failed: int(3)?,
    })
}

/// Throughput of the in-process library at 1, 4 and all cores: fresh
/// workers per thread count (so each count has its own peak RSS), A B B A
/// passes, each pass under the memory cap and a wall cap of the per-image
/// cap times ⌈images / threads⌉, the gate read every [`PASS_READ_EVERY`]
/// with an allowance of T·(1 − e^(−t/60)).
#[allow(
    clippy::too_many_lines,
    reason = "one thread count's passes, failures and gate reads read best in one loop"
)]
pub(crate) fn run_throughput(ctx: &Context<'_>, mode: Mode) -> Result<Vec<Throughput>, String> {
    if ctx.stopped.borrow().is_some() {
        // a stopped run starts no new phase: every count is not run
        return Ok(thread_counts(ctx.gate.cores)
            .into_iter()
            .map(|threads| Throughput {
                mode,
                threads,
                bound: 2 * threads,
                wall_cap: Duration::ZERO,
                rss_cap: throughput_cap(threads, ctx.options.rss_cap_mib),
                passes: Vec::new(),
                failures: Vec::new(),
                outcome: Outcome::NotRun,
                note: ctx.stopped.borrow().clone(),
                readings: Vec::new(),
                processes: BTreeMap::new(),
            })
            .collect());
    }
    let list = ListFile::write(ctx.images, mode)?;
    let variants = ctx.variants(Runtime::Lib);
    let mut results = Vec::new();
    for threads in thread_counts(ctx.gate.cores) {
        let bound = 2 * threads;
        let phase = format!("throughput/{}/{threads}", mode.as_str());
        let rounds = u32::try_from(ctx.images.len().div_ceil(threads).max(1)).unwrap_or(u32::MAX);
        let mut result = Throughput {
            mode,
            threads,
            bound,
            wall_cap: mode.wall_cap().saturating_mul(rounds),
            rss_cap: throughput_cap(threads, ctx.options.rss_cap_mib),
            passes: Vec::new(),
            failures: Vec::new(),
            outcome: Outcome::Measured,
            note: None,
            readings: Vec::new(),
            processes: BTreeMap::new(),
        };
        match ctx.before(&format!("{phase} start")) {
            Ok(readings) => result.readings.extend(readings),
            Err(readings) => {
                result.readings.extend(readings);
                result.outcome = Outcome::NotRun;
                result.note.clone_from(&ctx.stopped.borrow());
                results.push(result);
                continue;
            }
        }
        let mut sides: BTreeMap<Variant, Side> = BTreeMap::new();
        for &variant in &variants {
            sides.insert(
                variant,
                ctx.start(Runtime::Lib, variant, (mode, result.rss_cap))?,
            );
        }
        let line = format!(
            "throughput\t{}\t{}\t{threads}\t{bound}\t{}",
            mode.profile(),
            mode.budget(),
            list.0.display()
        );
        // the passes run back to back: the series' own load builds from the
        // first pass on, so the allowance counts from there
        let series = Instant::now();
        for (pass, &variant) in abba(&variants, 2).iter().enumerate() {
            let side = sides.get_mut(&variant).ok_or("throughput side")?;
            let server = side.server.as_mut().ok_or("throughput server")?;
            let mut crossing = None;
            let reply = server.request_watch(&line, result.wall_cap, PASS_READ_EVERY, &mut |_| {
                let reading = ctx.during(&format!("{phase} during"), threads, series.elapsed());
                let open = reading.open;
                if !open {
                    crossing = Some(reading);
                }
                open
            });
            match reply {
                Ok(reply) => {
                    let done = parse_pass(&reply, variant)?;
                    println!(
                        "{phase} {} {} images in {:.1} s · {:.2} img/s{}",
                        variant.letter(),
                        done.jobs,
                        ms(done.ns) / 1e3,
                        done.rate().unwrap_or(0.0),
                        if done.failed > 0 {
                            format!(" ({} failed, not counted)", done.failed)
                        } else {
                            String::new()
                        }
                    );
                    result.passes.push(done);
                }
                Err(CallError::Died(failure)) => {
                    println!(
                        "{phase} {}: resource failure ({})",
                        variant.letter(),
                        failure.kind.as_str()
                    );
                    result.failures.push(PassFailure {
                        variant,
                        pass,
                        failure,
                    });
                    result.outcome = Outcome::Failed;
                    side.respawn(ctx, mode, (failure.kind.as_str(), true))?;
                }
                Err(CallError::Aborted) => {
                    if let Some(reading) = crossing {
                        println!("{} — pass discarded", reading.line());
                        ctx.stop(&reading);
                        result.readings.push(reading);
                    }
                    // no successor: the remaining passes of this count are off
                    side.retire("abandoned");
                    result.outcome = Outcome::Discarded;
                    result.passes.clear();
                    break;
                }
                Err(CallError::Protocol(e)) => return Err(e),
            }
        }
        if result.outcome != Outcome::Discarded {
            let after = ctx.during(&format!("{phase} after"), threads, series.elapsed());
            if !ctx.options.dry_run && !after.open {
                result.outcome = Outcome::Discarded;
                result.passes.clear();
            }
            ctx.stop(&after);
            result.readings.push(after);
        }
        for (variant, side) in sides {
            result.processes.insert(variant, side.finish());
        }
        results.push(result);
    }
    drop(list);
    Ok(results)
}

// --------------------------------------------------------------- wasm cost

/// Packed-artifact sizes (Node zlib: gzip level 9, brotli quality 11).
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Sizes {
    pub(crate) raw: u64,
    pub(crate) gzip9: u64,
    pub(crate) brotli11: u64,
    /// The JS glue next to it.
    pub(crate) glue: u64,
}

/// One cold Node process: compile, instantiate and the first scan of the
/// repository fixture.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Cold {
    pub(crate) rep: u32,
    pub(crate) compile_ns: u64,
    pub(crate) instantiate_ns: u64,
    pub(crate) first_scan_ns: u64,
    pub(crate) memory_bytes: u64,
    pub(crate) peak_rss: Option<u64>,
}

impl Cold {
    /// The verdict metric: compile + instantiate + first scan.
    pub(crate) fn cold_ns(&self) -> u64 {
        self.compile_ns + self.instantiate_ns + self.first_scan_ns
    }
}

pub(crate) struct WasmCost {
    pub(crate) sizes: BTreeMap<Variant, Sizes>,
    pub(crate) runs: BTreeMap<Variant, Vec<Cold>>,
    pub(crate) failures: Vec<(Variant, Failure)>,
    pub(crate) outcome: Outcome,
    pub(crate) note: Option<String>,
    pub(crate) readings: Vec<Reading>,
}

fn wasm_tool(
    ctx: &Context<'_>,
    args: &[&Path],
    command: &str,
) -> Result<(Vec<String>, proc::Finished), String> {
    let mut node = Command::new(&ctx.options.node);
    node.arg(script("bench-wasm.mjs")).arg(command).args(args);
    let done = proc::run_once(&mut node, ctx.options.caps(WALL_FLOOR))?;
    let text = String::from_utf8_lossy(&done.stdout).into_owned();
    let line = text.lines().last().unwrap_or_default().to_owned();
    let fields: Vec<String> = line.split('\t').map(str::to_owned).collect();
    if done.failure.is_none()
        && (fields.first().map(String::as_str) != Some("ok") || done.usage.exit != Exit::Code(0))
    {
        return Err(format!(
            "bench-wasm.mjs {command}: {} (stderr {} lines)",
            fields.first().map_or("no reply", String::as_str),
            done.stderr.lines
        ));
    }
    Ok((fields, done))
}

fn numbers(fields: &[String], command: &str) -> Result<Vec<u64>, String> {
    fields
        .iter()
        .map(|f| {
            f.parse::<u64>()
                .map_err(|e| format!("bench-wasm.mjs {command}: {e}"))
        })
        .collect()
}

/// One cold-start line (`ok · compile · instantiate · first scan · memory ·
/// n · (symbology · hex)×n`), as a warm-up (rep 0). The first scan must
/// decode the fixture to its payload, or the measurement is void.
pub(crate) fn parse_cold(fields: &[String], peak_rss: Option<u64>) -> Result<Cold, String> {
    let head = numbers(fields.get(1..6).unwrap_or_default(), "cold")?;
    let [
        compile_ns,
        instantiate_ns,
        first_scan_ns,
        memory_bytes,
        count,
    ] = head[..]
    else {
        return Err(String::from("bench-wasm.mjs cold: five numbers expected"));
    };
    let mut units = Vec::new();
    for pair in fields.get(6..).unwrap_or_default().chunks(2) {
        if let [symbology, text] = pair {
            units.push(Unit::of(symbology, &unhex(text)?));
        }
    }
    let expected = vec![Unit::of("qr_code", FIXTURE_TEXT.as_bytes())];
    if usize::try_from(count).ok() != Some(units.len()) || units != expected {
        return Err(String::from(
            "bench-wasm.mjs cold: the fixture did not decode to its payload",
        ));
    }
    Ok(Cold {
        rep: 0,
        compile_ns,
        instantiate_ns,
        first_scan_ns,
        memory_bytes,
        peak_rss,
    })
}

/// Sizes, then the cold starts: one warm-up per variant, then
/// [`COLD_PAIRS`] processes each in A B B A order, every one under the caps
/// with the gate read before it and enforced after the series. The sizes
/// step reads the gate (a dry run: the host floor) first; a stopped run
/// starts neither.
pub(crate) fn run_wasm_cost(ctx: &Context<'_>) -> Result<WasmCost, String> {
    let variants = ctx.variants(Runtime::Wasm);
    let mut cost = WasmCost {
        sizes: BTreeMap::new(),
        runs: BTreeMap::new(),
        failures: Vec::new(),
        outcome: Outcome::Measured,
        note: None,
        readings: Vec::new(),
    };
    match ctx.before("wasm/sizes") {
        Ok(readings) => cost.readings.extend(readings),
        Err(readings) => {
            cost.readings.extend(readings);
            cost.outcome = Outcome::NotRun;
            cost.note = Some(
                ctx.stopped
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| String::from("the gate stayed closed until the deadline")),
            );
            return Ok(cost);
        }
    }
    for &variant in &variants {
        let dir = ctx.path(Runtime::Wasm, variant)?;
        let (fields, done) = wasm_tool(ctx, &[dir.as_path()], "sizes")?;
        if let Some(failure) = done.failure {
            cost.failures.push((variant, failure));
            cost.outcome = Outcome::Failed;
            return Ok(cost);
        }
        if let [raw, gzip9, brotli11, glue] =
            numbers(fields.get(1..).unwrap_or_default(), "sizes")?[..]
        {
            cost.sizes.insert(
                variant,
                Sizes {
                    raw,
                    gzip9,
                    brotli11,
                    glue,
                },
            );
        }
    }
    match ctx.before("wasm/cold start") {
        Ok(readings) => cost.readings.extend(readings),
        Err(readings) => {
            cost.readings.extend(readings);
            cost.outcome = Outcome::NotRun;
            cost.note.clone_from(&ctx.stopped.borrow());
            return Ok(cost);
        }
    }
    let runs: Vec<Variant> = variants
        .iter()
        .chain(abba(&variants, COLD_PAIRS).iter())
        .copied()
        .collect();
    let started = Instant::now();
    for (k, variant) in runs.into_iter().enumerate() {
        let reading = ctx.during("wasm/cold during", 1, started.elapsed());
        if !reading.open {
            ctx.stop(&reading);
            cost.readings.push(reading);
            cost.outcome = if ctx.options.dry_run {
                Outcome::NotRun
            } else {
                Outcome::Discarded
            };
            cost.runs.clear();
            return Ok(cost);
        }
        let dir = ctx.path(Runtime::Wasm, variant)?;
        let (fields, done) = wasm_tool(ctx, &[dir.as_path(), ctx.fixture.as_path()], "cold")?;
        if let Some(failure) = done.failure {
            cost.failures.push((variant, failure));
            cost.outcome = Outcome::Failed;
            continue;
        }
        let mut cold = parse_cold(&fields, done.usage.max_rss)?;
        let list = cost.runs.entry(variant).or_default();
        if k >= variants.len() {
            cold.rep = u32::try_from(list.len()).unwrap_or(u32::MAX);
        }
        list.push(cold);
    }
    let after = ctx.during("wasm/cold after", 1, started.elapsed());
    if !ctx.options.dry_run && !after.open {
        cost.outcome = Outcome::Discarded;
        cost.runs.clear();
    }
    ctx.stop(&after);
    cost.readings.push(after);
    Ok(cost)
}

/// Why a run measured nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The gate stayed closed until the deadline (exit 3).
    Gate(String),
    /// A dry run's host floor, or the host lock (exit 3).
    Floor(String),
    /// A pair unfit for comparison, or a worker of another protocol (exit 2).
    Parity(String),
    /// Inputs off their pins (exit 2).
    Inputs(String),
}

impl Refusal {
    pub(crate) fn exit(&self) -> i32 {
        match self {
            Self::Gate(_) | Self::Floor(_) => 3,
            Self::Parity(_) | Self::Inputs(_) => 2,
        }
    }

    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Gate(t) | Self::Floor(t) | Self::Parity(t) | Self::Inputs(t) => t,
        }
    }
}

/// Everything a receipt needs.
pub(crate) struct Measurement {
    pub(crate) options: Options,
    pub(crate) images: Vec<Image>,
    pub(crate) inputs: Inputs,
    pub(crate) facts: Facts,
    pub(crate) artifacts: Vec<Artifact>,
    pub(crate) hellos: BTreeMap<(Runtime, Variant), Hello>,
    pub(crate) parity: Vec<Parity>,
    pub(crate) threshold: f64,
    /// Start and end readings of the whole run.
    pub(crate) readings: Vec<Reading>,
    pub(crate) segments: Vec<Segment>,
    pub(crate) throughput: Vec<Throughput>,
    pub(crate) wasm: Option<WasmCost>,
    /// Why nothing was measured, if so.
    pub(crate) refused: Option<Refusal>,
    /// A failure outside the segments (throughput, wasm cost).
    pub(crate) failures: Vec<String>,
    /// Set when a dry run's host floor stopped the run.
    pub(crate) stopped: Option<String>,
}

/// Every worker says hello before anything is measured: the protocol, the
/// library workers' sources, the binary Node and WASM loaded.
pub(crate) fn preflight(ctx: &Context<'_>) -> Result<BTreeMap<(Runtime, Variant), Hello>, String> {
    let mut hellos = BTreeMap::new();
    for &runtime in &ctx.options.runtimes {
        if runtime == Runtime::Cli {
            continue;
        }
        for variant in ctx.variants(runtime) {
            let (server, hello) = ctx.hello(runtime, variant, ctx.options.rss_cap())?;
            drop(server.finish());
            hellos.insert((runtime, variant), hello);
        }
    }
    Ok(hellos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(args: &[&str]) -> Result<Options, String> {
        parse_options(args.iter().map(|a| (*a).to_owned()).collect())
    }

    #[test]
    fn abba_orders_balance_both_variants() {
        use Variant::{Base as A, Candidate as B};
        assert_eq!(abba(&[A, B], 5), vec![A, B, B, A, A, B, B, A, A, B]);
        assert_eq!(abba(&[A, B], 2), vec![A, B, B, A]);
        assert_eq!(abba(&[B], 3), vec![B, B, B]);
        assert!(abba(&[], 5).is_empty());
        let twelve = abba(&[A, B], COLD_PAIRS);
        assert_eq!(twelve.iter().filter(|v| **v == A).count(), 12);
        assert_eq!(twelve.len(), 24);
    }

    /// ABBAABBAAB on even images, BAABBAABBA on odd ones (warm-ups in the
    /// same order); over two images the median slots of A and B are equal.
    #[test]
    fn image_orders_alternate_and_balance_slots() {
        use Variant::{Base as A, Candidate as B};
        let even = image_order(&[A, B], 5, 0);
        let odd = image_order(&[A, B], 5, 1);
        let letters = |order: &[(Variant, u32)]| -> String {
            order.iter().map(|(v, _)| v.letter()).collect()
        };
        assert_eq!(letters(&even), "ABABBAABBAAB");
        assert_eq!(letters(&odd), "BABAABBAABBA");
        assert_eq!(
            even,
            vec![
                (A, 0),
                (B, 0),
                (A, 1),
                (B, 1),
                (B, 2),
                (A, 2),
                (A, 3),
                (B, 3),
                (B, 4),
                (A, 4),
                (A, 5),
                (B, 5)
            ]
        );
        let slots = |variant: Variant| -> Vec<usize> {
            let mut slots: Vec<usize> = [&even, &odd]
                .iter()
                .flat_map(|order| {
                    order
                        .iter()
                        .enumerate()
                        .filter(move |(_, (v, rep))| *v == variant && *rep > 0)
                        .map(|(slot, _)| slot)
                })
                .collect();
            slots.sort_unstable();
            slots
        };
        assert_eq!(slots(A), slots(B), "the same slots over an image pair");
        assert_eq!(slots(A), vec![2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(
            image_order(&[B], 2, 1),
            vec![(B, 0), (B, 1), (B, 2)],
            "one variant: its calls back to back"
        );
    }

    #[test]
    fn thread_counts_are_one_four_and_all() {
        assert_eq!(thread_counts(12), vec![1, 4, 12]);
        assert_eq!(thread_counts(4), vec![1, 4]);
        assert_eq!(thread_counts(2), vec![1, 2]);
        assert_eq!(thread_counts(1), vec![1]);
    }

    #[test]
    fn options_follow_the_protocol() {
        let parsed =
            opts(&["--out", "/x", "--lib-base", "/a", "--lib-candidate", "/b"]).expect("parses");
        assert_eq!(parsed.runtimes, vec![Runtime::Lib]);
        assert_eq!(parsed.modes, Mode::ALL.to_vec());
        assert_eq!(parsed.sets, Set::ALL.to_vec());
        assert_eq!(parsed.reps, REPS);
        assert_eq!(parsed.throughput_modes, vec![Mode::Full]);
        assert_eq!(parsed.rss_cap_mib, 1024);
        assert!(!parsed.dry_run);
        assert_eq!(parsed.subset(), vec![String::from("runtimes lib")]);

        let dry = opts(&[
            "--out",
            "/x",
            "--dry-run",
            "--reps",
            "1",
            "--modes",
            "frame,full",
            "--cli-base",
            "/q",
            "--rss-cap-mib",
            "512",
        ])
        .expect("parses");
        assert_eq!(
            (dry.reps, dry.modes.clone(), dry.rss_cap_mib),
            (1, vec![Mode::Full, Mode::Frame], 512)
        );
        assert!(dry.throughput_modes.is_empty(), "no lib, no throughput");
        assert_eq!(
            dry.subset(),
            vec![
                String::from("modes full,frame"),
                String::from("runtimes cli"),
                String::from("cli has one variant"),
            ]
        );

        for bad in [
            vec!["--out", "/x", "--lib-base", "/a", "--reps", "3"],
            vec![
                "--out",
                "/x",
                "--dry-run",
                "--wait-gate",
                "60",
                "--lib-base",
                "/a",
            ],
            vec!["--lib-base", "/a"],
            vec!["--out", "/x"],
            vec!["--out", "/x", "--lib-base", "/a", "--runtimes", "cli"],
            vec!["--out", "/x", "--lib-base", "/a", "--modes", "turbo"],
            vec!["--out", "/x", "--lib-base", "/a", "--bogus"],
            vec!["--out", "/x", "--out", "/y", "--lib-base", "/a"],
            vec!["--out", "/x", "--lib-base", "/a", "--rss-cap-mib", "2048"],
            vec!["--out", "/x", "--lib-base", "/a", "--rss-cap-mib", "0"],
        ] {
            assert!(opts(&bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// The complete frozen scope is the only one that is not a subset.
    #[test]
    fn only_the_complete_scope_is_not_a_subset() {
        let mut args = vec!["--out", "/x"];
        for flag in [
            "--lib-base",
            "--lib-candidate",
            "--cli-base",
            "--cli-candidate",
            "--node-base",
            "--node-candidate",
            "--wasm-base",
            "--wasm-candidate",
        ] {
            args.extend([flag, "/p"]);
        }
        assert!(opts(&args).expect("parses").subset().is_empty());
        let mut sampled = args.clone();
        sampled.extend(["--per-group", "1"]);
        assert_eq!(
            opts(&sampled).expect("parses").subset(),
            vec![String::from("per-group sampling (1 per group)")]
        );
        let mut no_throughput = args.clone();
        no_throughput.extend(["--throughput-modes", "none"]);
        assert_eq!(
            opts(&no_throughput).expect("parses").subset(),
            vec![String::from("no throughput in full")]
        );
        // a base-only runtime (--wasm-candidate dropped)
        let mut base_only = Vec::new();
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            if *arg == "--wasm-candidate" {
                rest.next();
            } else {
                base_only.push(*arg);
            }
        }
        assert_eq!(
            opts(&base_only).expect("parses").subset(),
            vec![String::from("wasm has one variant")]
        );
    }

    /// A worker reply becomes a sample with hashes; the text is gone.
    #[test]
    fn replies_parse_into_hashed_samples() {
        let hex = super::super::worker::hex(b"bench");
        let ok = parse_reply(
            &format!("ok\t1500000\t1.25\t0\t-\t-\t-\t-\t2\t7\tv80w100,5/5,3/5x\t1\tqr_code\t{hex}"),
            2,
        )
        .expect("ok");
        assert_eq!((ok.rep, ok.ns, ok.engine_panics), (2, 1_500_000, Some(0)));
        assert_eq!((ok.heap, ok.retained), (None, None));
        assert_eq!(
            ok.work,
            Some(Work {
                stages: 2,
                transforms: 7,
                judgment: Some(String::from("v80w100,5/5,3/5x")),
            })
        );
        let units = ok.units.expect("units");
        assert_eq!(units, vec![Unit::of("qr_code", b"bench")]);
        assert_eq!(units[0].text_len, 5);
        assert_eq!(units[0].text_sha256, external::sha256_bytes(b"bench"));

        let counted = parse_reply("ok\t9\t0.5\t1\t4096\t12\t8000\t-64\t1\t1\t-\t0", 0).expect("ok");
        assert_eq!(counted.work.and_then(|w| w.judgment), None, "no score");
        assert_eq!(
            counted.heap,
            Some(HeapStats {
                peak: 4096,
                calls: 12,
                bytes: 8000
            })
        );
        assert_eq!(counted.retained, Some(-64));
        assert_eq!(counted.units, Some(Vec::new()));

        let error = parse_reply("error\t77\tQRS-001", 1).expect("error reply");
        assert_eq!(
            (error.ns, error.error.as_deref(), error.units.is_none()),
            (77, Some("QRS-001"), true)
        );
        assert!(parse_reply("fail\tread: entity not found", 1).is_err());
        assert!(parse_reply("ok\t1\t2", 1).is_err());
        assert!(
            parse_reply("ok\t1\t1.0\t0\t-\t-\t-\t-\t1\t1\t-\t1\tqr_code\tzz", 1).is_err(),
            "bad hex"
        );
        assert!(
            parse_reply("ok\t1\t1.0\t0\t-\t-\t-\t-\t1\t1\t-\t2\tqr_code\t41", 1).is_err(),
            "fewer detections than announced"
        );
        assert!(
            parse_reply("ok\t1\t1.0\t0\t-\t-\t-\t-\t1\t1\t1\t0\t0", 1).is_err(),
            "a protocol 2 reply (scored, axes cut) is not a judgment"
        );
    }

    #[test]
    fn hellos_carry_protocol_and_sources() {
        let line = format!(
            "hello\t{}\t0.9.0\t2\t4\trust-worker\t{}\t{}",
            super::super::worker::PROTOCOL,
            "a".repeat(64),
            "b".repeat(64)
        );
        let hello = parse_hello(&line).expect("hello");
        assert_eq!(hello.worker_source, Some("a".repeat(64)));
        assert_eq!(hello.main_source, Some("b".repeat(64)));
        assert_eq!(hello.scanner, "0.9.0");
        let node = parse_hello(&format!(
            "hello\t{}\t0.9.0\t-\t-\tnode-napi\t-\tqrcode-ai-scanner.darwin-arm64.node",
            super::super::worker::PROTOCOL
        ))
        .expect("hello");
        assert_eq!(
            node.loaded.as_deref(),
            Some("qrcode-ai-scanner.darwin-arm64.node")
        );
        assert!(
            parse_hello("hello\t0.9.0\t2\t4\trust-worker").is_err(),
            "a worker of the first protocol is refused"
        );
        assert!(parse_hello("fail\tloaded an installed addon").is_err());
    }

    /// One worker source on both sides, the harness's own.
    #[test]
    fn worker_sources_must_match_the_harness() {
        let hello = |source: &str, main: &str| Hello {
            line: String::new(),
            scanner: String::new(),
            runtime: String::from("rust-worker"),
            worker_source: Some(source.to_owned()),
            main_source: Some(main.to_owned()),
            loaded: None,
        };
        assert!(worker_parity(&hello("w", "m"), &hello("w", "m"), "w").is_empty());
        let stale = worker_parity(&hello("old", "m"), &hello("w", "m"), "w");
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].what, "worker_source");
        assert_eq!(
            worker_parity(&hello("w", "m1"), &hello("w", "m2"), "w")
                .iter()
                .map(|m| m.detail.as_str())
                .collect::<Vec<_>>(),
            vec!["the two generated mains differ — not one prepare"]
        );
        assert_eq!(
            worker_parity(&hello("x", "m"), &hello("x", "m"), "w").len(),
            2
        );
    }

    /// Same lines, same hash as the oracle's output signature.
    #[test]
    fn signatures_match_the_oracle_format() {
        let units = vec![Unit::of("qr_code", b"E"), Unit::of("ean13", b"W")];
        let expected = external::sha256_bytes(
            format!(
                "qr_code\t{}\nean13\t{}\n",
                external::sha256_bytes(b"E"),
                external::sha256_bytes(b"W")
            )
            .as_bytes(),
        );
        assert_eq!(signature(&units), expected);
        assert_eq!(signature(&[]), external::sha256_bytes(b""));
    }

    /// The recomputed table hash equals `inputs.dispositions_sha256` of the
    /// accepted oracle receipts — update both together.
    #[test]
    fn dispositions_hash_matches_the_oracle_receipts() {
        assert_eq!(
            dispositions_sha256(),
            "8b9553ccbef71094638f30e1083cdb62c7ecc86ae313d667dc7afd7f6f7c7888"
        );
    }

    #[test]
    fn modes_map_to_binding_budgets_and_caps() {
        assert_eq!(Mode::FullUnbounded.budget_ms(), None);
        assert_eq!(Mode::Full.budget_ms(), Some(4_000));
        assert_eq!(Mode::Fast.budget_ms(), Some(800));
        assert_eq!(Mode::Frame.budget_ms(), Some(80));
        assert_eq!(
            Mode::ALL
                .iter()
                .map(|m| (m.profile(), m.budget(), m.wall_cap().as_secs()))
                .collect::<Vec<_>>(),
            vec![
                ("full", "default", 60),
                ("full", "unbounded", 60),
                ("fast", "default", 60),
                ("frame", "default", 60)
            ],
            "max(10 × budget, 60 s): every preset budget is under 6 s"
        );
    }

    /// Truncation is read against the image's reference walk, never from
    /// the clock. The reference ran 2 stages, 5 transforms, decoded `x` and
    /// judged `v80w100,5/5,5/5`.
    #[test]
    fn truncation_is_read_against_the_reference_walk() {
        let call =
            |total_ms: f64, (stages, transforms): (u32, u64), text: &str, judgment: &str| Sample {
                rep: 1,
                total_ms: Some(total_ms),
                units: Some(if text.is_empty() {
                    Vec::new()
                } else {
                    vec![Unit::of("qr_code", text.as_bytes())]
                }),
                work: Some(Work {
                    stages,
                    transforms,
                    judgment: (judgment != "-").then(|| judgment.to_owned()),
                }),
                ..Sample::default()
            };
        let full = "v80w100,5/5,5/5";
        let reference = call(950.0, (2, 5), "x", full);
        let same = call(4210.0, (2, 5), "x", full);
        assert!(
            same.reached_budget(Mode::Full) && !same.truncated(&reference, Mode::Full),
            "the reference walk completed past the deadline: complete, whatever its time"
        );
        assert!(!call(79.9, (2, 5), "x", "-").reached_budget(Mode::Frame));
        assert!(!call(1e6, (2, 5), "x", full).reached_budget(Mode::FullUnbounded));
        for (cut, why) in [
            (call(4001.0, (2, 4), "x", full), "a stage stopped short"),
            (call(4001.0, (1, 5), "x", full), "a stage skipped"),
            (call(4001.0, (2, 5), "", "-"), "the decode lost"),
        ] {
            assert!(cut.walk_cut(&reference), "{why}");
            assert!(cut.truncated(&reference, Mode::Full), "{why}");
        }
        let dropped = call(3990.0, (2, 5), "x", "-");
        assert!(
            !dropped.walk_cut(&reference)
                && dropped.judgment_cut(&reference, Mode::Full)
                && dropped.truncated(&reference, Mode::Full),
            "this tree drops a cut judgment"
        );
        let short = call(3990.0, (2, 5), "x", "v62w100,5/5,2/5");
        assert!(
            short.judgment_cut(&reference, Mode::Fast),
            "0.9.0 ships a cut axis"
        );
        let short_after_failure = call(3990.0, (2, 5), "x", "v62w100,5/5,1/5x");
        assert!(
            short_after_failure.judgment_cut(&reference, Mode::Fast),
            "a lighting set cut after a failed cell: its passed count differs"
        );
        assert!(
            !dropped.judgment_cut(&reference, Mode::Frame),
            "frame never scores"
        );
    }

    #[test]
    fn cli_reports_yield_their_work() {
        let report = serde_json::json!({
            "trace": {"total_ms": 80.5, "engine_panics": 0, "stages": [
                {"stage": "pyramid", "transforms_tried": 1}, {"stage": "direct", "transforms_tried": 1},
                {"stage": "enhance", "transforms_tried": 4}]},
            "score": {"value": 72, "weights_run": 100, "axes": [
                {"axis": "blur", "passed": 3, "total": 5, "failed_at": "blur 2.0"},
                {"axis": "lighting", "passed": 2, "total": 5, "failed_at": null},
                {"axis": "contrast", "passed": 5, "total": 5, "failed_at": null}]},
        });
        assert_eq!(
            report_work(&report),
            Ok(Work {
                stages: 3,
                transforms: 6,
                judgment: Some(String::from("v72w100,3/5x,2/5,5/5")),
            })
        );
        let unscored = serde_json::json!({"trace": {"stages": []}, "score": null});
        assert_eq!(
            report_work(&unscored),
            Ok(Work {
                stages: 0,
                transforms: 0,
                judgment: None,
            })
        );
        let pre_09 = serde_json::json!({"value": 50, "axes": []});
        assert_eq!(json_judgment(&pre_09), Ok(Some(String::from("v50w0"))));
        assert!(report_work(&serde_json::json!({"trace": {}})).is_err());
        assert!(
            report_work(&serde_json::json!({"trace": {"stages": [{"stage": "direct"}]}})).is_err(),
            "a stage without transforms_tried"
        );
        assert!(json_judgment(&serde_json::json!({"axes": []})).is_err());
    }

    /// The Node and WASM drivers on fake packages: work facts on the wire,
    /// a report of the wrong shape is a protocol `fail` (never a timed
    /// error), a `QRS-` error stays a timed error, a `scan_image` of
    /// another signature and a package that loaded no addon of its own are
    /// refused at `hello`. Skipped where `node` is absent.
    #[test]
    #[cfg(all(unix, target_pointer_width = "64"))]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture package, every driver path"
    )]
    fn node_drivers_reply_outside_the_clock_and_refuse_strangers() {
        if !Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            eprintln!("node not found: driver test skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("qrscan-bench-drivers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let wasm = dir.join("wasm");
        std::fs::create_dir_all(&wasm).expect("dir");
        let put = |path: &Path, text: &[u8]| std::fs::write(path, text).expect("write");
        put(
            &wasm.join("package.json"),
            br#"{"main": "qrcode-ai-scanner.js", "type": "module"}"#,
        );
        put(&wasm.join("qrcode-ai-scanner_bg.wasm"), b"\0asm\x01\0\0\0");
        let glue = |signature: &str| {
            format!(
                "export function initSync() {{ return {{ memory: {{ buffer: {{ byteLength: 65536 }} }} }}; }}\n\
                 export function version() {{ return '0.0.0-test'; }}\n\
                 export function scan_image({signature}) {{\n\
                   if (profile === 'frame') return {{}};\n\
                   if (profile === 'fast') throw new Error('too large [QRS-001]');\n\
                   return {{ trace: {{ total_ms: budget_ms === 0 ? 2.5 : 1.5, engine_panics: 0,\n\
                     stages: [{{ stage: 'direct', transforms_tried: 2 }}, {{ stage: 'enhance', transforms_tried: 3 }}] }},\n\
                     score: {{ value: 70, weights_run: 100, axes: [{{ passed: 2, total: 5, failed_at: null }}, {{ passed: 1, total: 5, failed_at: 'blur 1.0' }}] }},\n\
                     detections: [{{ symbology: 'qr_code', content: {{ text: 'hi' }} }}] }};\n\
                 }}\n"
            )
        };
        put(
            &wasm.join("qrcode-ai-scanner.js"),
            glue("bytes, profile, max_dimension, max_pixels, budget_ms, score_skip_axes")
                .as_bytes(),
        );
        let image = dir.join("image.bin");
        put(&image, b"x");
        let serve = |script: &str, package: &Path| {
            let mut node = Command::new("node");
            node.arg(super::script(script)).arg("serve").arg(package);
            Server::spawn(node, "driver", 1 << 30).expect("spawn")
        };
        let wall = Duration::from_secs(30);
        let mut server = serve("bench-wasm.mjs", &wasm);
        let hello = parse_hello(&server.request("hello", wall).expect("hello")).expect("hello");
        assert_eq!(
            (hello.runtime.as_str(), hello.loaded.as_deref()),
            ("wasm-node", Some("qrcode-ai-scanner_bg.wasm"))
        );
        let line = |mode: Mode, path: &Path| scan_line(mode, mode.budget(), path, false);
        let ok = parse_reply(
            &server
                .request(&line(Mode::FullUnbounded, &image), wall)
                .expect("reply"),
            1,
        )
        .expect("ok");
        assert_eq!(
            (ok.total_ms, ok.work, ok.units),
            (
                Some(2.5),
                Some(Work {
                    stages: 2,
                    transforms: 5,
                    judgment: Some(String::from("v70w100,2/5,1/5x")),
                }),
                Some(vec![Unit::of("qr_code", b"hi")])
            ),
            "budget 0 reached the binding; the judgment as the worker writes it"
        );
        let shape = server
            .request(&line(Mode::Frame, &image), wall)
            .expect("reply");
        assert_eq!(shape, "fail\tunexpected report shape");
        let error = parse_reply(
            &server
                .request(&line(Mode::Fast, &image), wall)
                .expect("reply"),
            1,
        )
        .expect("error reply");
        assert_eq!(error.error.as_deref(), Some("QRS-001"));
        let missing = server
            .request(&line(Mode::Full, &dir.join("absent.png")), wall)
            .expect("reply");
        assert_eq!(missing, "fail\tENOENT", "no path in a failure reason");
        assert_eq!(server.finish().farewell, "bye\t65536");

        let cold = proc::run_once(
            Command::new("node")
                .arg(super::script("bench-wasm.mjs"))
                .arg("cold")
                .arg(&wasm)
                .arg(&image),
            Caps { rss: 1 << 30, wall },
        )
        .expect("cold");
        let line = String::from_utf8_lossy(&cold.stdout).trim().to_owned();
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields[0], "ok", "{line}");
        assert_eq!(fields[4..].to_vec(), vec!["65536", "1", "qr_code", "6869"]);

        put(
            &wasm.join("qrcode-ai-scanner.js"),
            glue("bytes, profile, budget_ms").as_bytes(),
        );
        let mut moved = serve("bench-wasm.mjs", &wasm);
        let refused = parse_hello(&moved.request("hello", wall).expect("reply"));
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("scan_image takes (bytes, profile, budget_ms)")),
            "{refused:?}"
        );
        drop(moved);

        let napi = dir.join("node");
        std::fs::create_dir_all(&napi).expect("dir");
        put(&napi.join("package.json"), br#"{"name": "fake"}"#);
        put(
            &napi.join("index.js"),
            b"module.exports = { version: () => '0', scan: async () => ({}) };\n",
        );
        let mut stranger = serve("bench-node.mjs", &napi);
        let refused = parse_hello(&stranger.request("hello", wall).expect("reply"));
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("0 native addons loaded")),
            "{refused:?}"
        );
        drop(stranger);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fake library worker speaking the protocol: every scan answers one
    /// `qr_code` "x" in 1 ms; a scan of a path containing `bad` makes the
    /// process exit mid-call.
    #[cfg(all(unix, target_pointer_width = "64"))]
    fn fake_worker(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let script = format!(
            "#!/bin/sh\n\
             while IFS= read -r line; do\n\
               case \"$line\" in\n\
                 hello) printf 'hello\\t{}\\t0.9.0\\t2\\t4\\trust-worker\\t-\\t-\\n' ;;\n\
                 quit) echo bye; exit 0 ;;\n\
                 scan*bad*) exit 3 ;;\n\
                 scan*) printf 'ok\\t1000000\\t1.0\\t0\\t-\\t-\\t-\\t-\\t1\\t1\\t-\\t1\\tqr_code\\t78\\n' ;;\n\
                 *) printf 'fail\\tunknown request\\n' ;;\n\
               esac\n\
             done\n",
            super::super::worker::PROTOCOL
        );
        let path = dir.join("fake-worker.sh");
        std::fs::write(&path, script).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[cfg(all(unix, target_pointer_width = "64"))]
    fn fake_images(names: &[&str]) -> Vec<Image> {
        names
            .iter()
            .map(|name| Image {
                set: Set::Vendored,
                group: String::from("vendored/clean"),
                path: format!("fixtures/{name}.png"),
                abs: PathBuf::from(format!("/nonexistent/{name}.png")),
                sha256: String::new(),
                bytes: 0,
            })
            .collect()
    }

    /// The gated path: a gate that closes before image 1 discards the
    /// attempt; the retry keeps only its own samples. Three crossings leave
    /// the segment discarded, with nothing kept.
    #[test]
    #[cfg(all(unix, target_pointer_width = "64"))]
    fn gate_crossings_discard_whole_attempts() {
        let dir = std::env::temp_dir().join(format!("qrscan-bench-segment-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let worker = fake_worker(&dir);
        let worker = worker.to_str().expect("utf-8");
        let options = opts(&[
            "--out",
            "/nonexistent/out",
            "--modes",
            "frame",
            "--sets",
            "vendored",
            "--lib-base",
            worker,
            "--lib-candidate",
            worker,
        ])
        .expect("options");
        let images = fake_images(&["a", "b"]);
        let run = |gate: &Gate| {
            let ctx = Context {
                options: &options,
                gate,
                images: &images,
                artifacts: options.artifacts.clone(),
                deadline: Instant::now(),
                fixture: PathBuf::from("/nonexistent/fixture.png"),
                stopped: RefCell::new(None),
            };
            run_segment(&ctx, Runtime::Lib, Mode::Frame)
        };
        // frame is budgeted: start, the reference walks of a and b, then
        // image a, image b (closed) — then start, references a, b, images
        // a, b, after
        let retried = run(&Gate::scripted(&[
            true, true, true, true, false, true, true, true, true, true, true,
        ]));
        assert_eq!((retried.outcome, retried.attempts), (Outcome::Measured, 2));
        assert_eq!(retried.samples.len(), 2);
        assert!(
            retried
                .samples
                .values()
                .all(|v| v.values().all(|s| s.len() == 6)),
            "attempt 2 alone: one warm-up and five timed calls per variant"
        );
        assert!(
            retried.references.values().all(|v| v.len() == 2) && retried.references.len() == 2,
            "one reference walk per image and variant"
        );
        assert_eq!(
            retried.readings.iter().filter(|r| !r.open).count(),
            1,
            "the crossing is logged"
        );
        // a crossing during the reference walks discards the attempt too
        let crossed = run(&Gate::scripted(&[true, false, true, false, true, false]));
        assert_eq!((crossed.outcome, crossed.attempts), (Outcome::Discarded, 3));
        assert!(
            crossed.samples.is_empty()
                && crossed.references.is_empty()
                && crossed.processes.is_empty()
        );
        assert_eq!(
            crossed.note.as_deref(),
            Some("every attempt crossed the gate")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A respawn: a worker that dies on image 2 — in its reference walk,
    /// the first call a budgeted segment makes of it — leaves a resource
    /// failure per variant, is respawned (hello and priming again), and
    /// image 3 is measured; the dead processes are kept and image 2 gets no
    /// counted call.
    #[test]
    #[cfg(all(unix, target_pointer_width = "64"))]
    fn a_dying_worker_is_attributed_respawned_and_the_run_goes_on() {
        let dir = std::env::temp_dir().join(format!("qrscan-bench-respawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let worker = fake_worker(&dir);
        let worker = worker.to_str().expect("utf-8");
        let options = opts(&[
            "--out",
            "/nonexistent/out",
            "--modes",
            "frame",
            "--sets",
            "vendored",
            "--lib-base",
            worker,
            "--lib-candidate",
            worker,
        ])
        .expect("options");
        let images = fake_images(&["ok0", "ok1", "bad2", "ok3"]);
        let gate = Gate::scripted(&[true; 10]);
        let ctx = Context {
            options: &options,
            gate: &gate,
            images: &images,
            artifacts: options.artifacts.clone(),
            deadline: Instant::now(),
            fixture: PathBuf::from("/nonexistent/fixture.png"),
            stopped: RefCell::new(None),
        };
        let segment = run_segment(&ctx, Runtime::Lib, Mode::Frame);
        assert_eq!(segment.outcome, Outcome::Measured);
        let failures: Vec<(usize, Variant, bool, proc::Kind)> = segment
            .failures
            .iter()
            .map(|f| (f.image, f.variant, f.reference, f.failure.kind))
            .collect();
        assert_eq!(
            failures,
            vec![
                (2, Variant::Base, true, proc::Kind::Exit(3)),
                (2, Variant::Candidate, true, proc::Kind::Exit(3)),
            ]
        );
        assert_eq!(
            segment.references.keys().copied().collect::<Vec<_>>(),
            vec![0, 1, 3],
            "image 2 has no reference walk"
        );
        for index in [0, 1, 3] {
            let calls = &segment.samples[&index];
            assert!(
                calls.values().all(|s| s.len() == 6),
                "image {index} measured"
            );
        }
        assert!(
            segment.samples.get(&2).is_none_or(BTreeMap::is_empty),
            "a failed call is never a sample"
        );
        for variant in [Variant::Base, Variant::Candidate] {
            let ends: Vec<(&str, bool)> = segment.processes[&variant]
                .iter()
                .map(|e| (e.ended.as_str(), e.capped))
                .collect();
            assert_eq!(
                ends,
                vec![("exit", true), ("quit", false)],
                "{variant:?}: the dead worker (capped: out of the A/B peak), then its respawn"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The inputs are checked against their pins — both manifests, each
    /// set's frozen count, and a set measured whole.
    #[test]
    fn inputs_are_checked_against_their_pins() {
        let frozen: BTreeMap<&'static str, usize> = FROZEN_IMAGES
            .iter()
            .map(|(s, n)| (s.as_str(), *n))
            .collect();
        let pinned = Inputs {
            manifest_sha256: MANIFEST_PIN.to_owned(),
            vendored_sha256: VENDORED_PIN.to_owned(),
            dispositions_sha256: String::new(),
            corpus_root: "default",
            available: frozen.clone(),
            selected: frozen.clone(),
            per_group: None,
            pins: Vec::new(),
        };
        assert!(pin_findings(&pinned, Set::ALL).is_empty());
        let drifted = Inputs {
            manifest_sha256: String::from("ff"),
            ..pinned.clone()
        };
        assert_eq!(
            pin_findings(&drifted, Set::ALL),
            vec![format!(
                "corpus-external.tsv sha256 ff, not its pin {MANIFEST_PIN}"
            )]
        );
        let mut dropped = pinned.clone();
        dropped.available.insert("zxing", 178);
        dropped.selected.insert("zxing", 178);
        assert_eq!(
            pin_findings(&dropped, Set::ALL),
            vec![
                String::from("zxing: 178 images listed, the frozen set has 179"),
                String::from("zxing: 178 images selected of the frozen 179"),
            ]
        );
        let mut sampled = pinned.clone();
        sampled.per_group = Some(1);
        sampled.selected = BTreeMap::from([("vendored", 9)]);
        assert!(
            pin_findings(&sampled, &[Set::Vendored]).is_empty(),
            "a declared subset is partial, not off the pins"
        );
    }

    /// A Node or WASM driver must have loaded, by its path in the package,
    /// the file the receipt hashes.
    #[test]
    fn drivers_must_load_the_file_the_receipt_hashes() {
        let artifact = |variant: Variant, sha: &str| Artifact {
            runtime: "node",
            variant: variant.as_str(),
            files: Vec::new(),
            described: Described {
                binary: String::from("qrcode-ai-scanner.darwin-arm64.node"),
                sha256: sha.to_owned(),
                bytes: 1,
                embedded_rustc_commits: Vec::new(),
                wasm: None,
                manifest: None,
                manifest_sha256: None,
                manifest_error: None,
            },
            key: (Runtime::Node, variant),
            path: PathBuf::new(),
        };
        let artifacts = [
            artifact(Variant::Base, "aa"),
            artifact(Variant::Candidate, "bb"),
        ];
        let hello = |loaded: &str| Hello {
            line: String::new(),
            scanner: String::new(),
            runtime: String::from("node-napi"),
            worker_source: None,
            main_source: None,
            loaded: Some(loaded.to_owned()),
        };
        let loaded = |candidate: &str| -> Vec<String> {
            let hellos = BTreeMap::from([
                (
                    (Runtime::Node, Variant::Base),
                    hello("qrcode-ai-scanner.darwin-arm64.node"),
                ),
                ((Runtime::Node, Variant::Candidate), hello(candidate)),
            ]);
            parity(&artifacts, &hellos, "w")[0]
                .findings
                .iter()
                .filter(|f| f.what == "loaded")
                .map(|f| f.detail.clone())
                .collect()
        };
        assert!(loaded("qrcode-ai-scanner.darwin-arm64.node").is_empty());
        assert_eq!(
            loaded("nested/qrcode-ai-scanner.darwin-arm64.node"),
            vec![String::from(
                "candidate: the driver loaded nested/qrcode-ai-scanner.darwin-arm64.node, \
                 the receipt hashes qrcode-ai-scanner.darwin-arm64.node"
            )]
        );
    }

    /// A throughput worker's memory cap is a quarter of the per-call cap
    /// per thread, at most three per-call caps.
    #[test]
    fn throughput_caps_scale_with_threads() {
        const MIB: u64 = 1 << 20;
        let caps: Vec<u64> = [1, 4, 12, 16]
            .into_iter()
            .map(|threads| throughput_cap(threads, RSS_CAP_MIB) / MIB)
            .collect();
        assert_eq!(caps, vec![256, 1024, 3072, 3072]);
        assert_eq!(
            throughput_cap(12, 512) / MIB,
            1536,
            "a lower --rss-cap-mib scales it down"
        );
    }

    /// Only Mach-O or ELF executables run as measured children, and
    /// `--node-bin` resolves to node's own path.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn only_native_executables_and_the_resolved_node_run() {
        let dir = std::env::temp_dir().join(format!("qrscan-bench-native-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let script = fake_worker(&dir);
        assert_eq!(
            native_executable(&script),
            Err(String::from(
                "fake-worker.sh: not a Mach-O or ELF executable"
            ))
        );
        assert_eq!(native_executable(Path::new("/bin/sh")), Ok(()));
        assert!(native_executable(&dir.join("absent")).is_err());
        let caps = Caps {
            rss: 1 << 30,
            wall: Duration::from_secs(30),
        };
        if Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            let node = resolve_node("node", caps).expect("node resolves");
            assert!(
                node.is_absolute() && native_executable(&node).is_ok(),
                "{node:?}"
            );
        }
        assert!(resolve_node(script.to_str().expect("utf-8"), caps).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With a fake package: a Node driver whose parent goes away mid-scan
    /// — a synchronous 20 s scan that blocks its main thread — leaves
    /// within about 50 ms.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn node_drivers_leave_when_their_parent_does() {
        if !Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            eprintln!("node not found: driver test skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("qrscan-bench-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let put = |name: &str, text: &[u8]| std::fs::write(dir.join(name), text).expect("write");
        put(
            "package.json",
            br#"{"main": "qrcode-ai-scanner.js", "type": "module"}"#,
        );
        put("qrcode-ai-scanner_bg.wasm", b"\0asm\x01\0\0\0");
        put(
            "qrcode-ai-scanner.js",
            b"export function initSync() { return { memory: { buffer: { byteLength: 65536 } } }; }\n\
              export function version() { return '0.0.0-test'; }\n\
              export function scan_image(bytes, profile, max_dimension, max_pixels, budget_ms) {\n\
                const end = Date.now() + 20000; while (Date.now() < end) {}\n\
                return { trace: { total_ms: 1, engine_panics: 0, stages: [] }, score: null, detections: [] };\n\
              }\n",
        );
        put("image.bin", b"x");
        let mut shell = Command::new("sh")
            .args([
                "-c",
                "printf 'scan\\tfull\\tdefault\\t0\\t%s\\n' \"$2\" | node \"$0\" serve \"$1\" & echo \"driver $!\"; sleep 1",
                script("bench-wasm.mjs").to_str().expect("utf-8"),
                dir.to_str().expect("utf-8"),
                dir.join("image.bin").to_str().expect("utf-8"),
            ])
            .env_remove(super::super::worker::PARENT_ENV)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("shell");
        let driver = proc::tests::read_pid(shell.stdout.take().expect("stdout"), "driver ");
        assert!(shell.wait().expect("the shell exits").success());
        assert!(
            proc::tests::gone_within(driver, Duration::from_secs(1)),
            "the orphaned driver left mid-scan"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cold start counts only when its first scan decoded the fixture.
    #[test]
    fn cold_starts_must_decode_the_fixture() {
        let line = |text: &str| -> Vec<String> {
            format!(
                "ok\t2000000\t500000\t90000000\t1114112\t1\tqr_code\t{}",
                super::super::worker::hex(text.as_bytes())
            )
            .split('\t')
            .map(str::to_owned)
            .collect()
        };
        let cold = parse_cold(&line(FIXTURE_TEXT), Some(1 << 27)).expect("cold");
        assert_eq!(
            (cold.rep, cold.cold_ns(), cold.memory_bytes, cold.peak_rss),
            (0, 92_500_000, 1_114_112, Some(1 << 27))
        );
        assert!(
            parse_cold(&line("other"), None).is_err(),
            "a wrong decode voids it"
        );
        let empty: Vec<String> = "ok\t1\t1\t1\t1\t0".split('\t').map(str::to_owned).collect();
        assert!(parse_cold(&empty, None).is_err(), "no decode voids it");
    }

    #[test]
    fn passes_count_completed_scans_only() {
        let pass = parse_pass("ok\t20\t2000000000\t4", Variant::Base).expect("pass");
        assert_eq!(
            pass.rate().map(|r| format!("{r:.2}")),
            Some(String::from("8.00"))
        );
        assert!(parse_pass("fail\tread the list: not found", Variant::Base).is_err());
        assert!(parse_pass("ok\t1\t2", Variant::Base).is_err());
    }
}
