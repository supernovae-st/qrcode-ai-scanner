//! `xtask bench` — outer-call latency, memory, allocations, throughput and
//! WASM cost of the scanner, base (the published 0.9.0) versus candidate
//! (this tree), by the pinned benchmark protocol `qrscan-oracle/v1.4`: the
//! oracle's sets, four runtimes and four modes, the environment, the method
//! and host gate, and the receipts, as `bench/README.md` states them.
//! Correctness belongs to `xtask oracle`; the bench only records whether
//! outputs agree.
//!
//! - `bench prepare --work DIR [--node-addon LIB]` writes the two worker
//!   crates outside the workspace — each its own `[workspace]` root with a
//!   lockfile seeded from the workspace lock and the workspace release
//!   profile — one depending on this tree by path, one on
//!   `qrcode-ai-scanner = "=0.9.0"` from crates.io; given a built napi
//!   library it also assembles the candidate Node package.
//! - `bench prepare-base --work DIR …` extracts the v0.9.0 sources the base
//!   artifacts are rebuilt from (the release commit's `git archive`, the
//!   published CLI crate), prints how to build each, and assembles the base
//!   Node package.
//! - `bench stamp --source DIR --artifact FILE -- <cargo build …>` runs the
//!   build and writes the artifact's manifest from what the build reported;
//!   `--wasm --package DIR -- <build script>` does the same for a WASM
//!   package. `bench inspect PATH` prints what a run would record of it.
//! - `bench gate [--wait SECONDS]` reads the host gate: exit 0 open, 3
//!   closed; `--wait` holds the host lock while it polls.
//! - `bench run --out DIR …` measures (flags in [`run::USAGE`]).
//!
//! Method: per image, in a budgeted mode an uncounted reference walk
//! without the budget, then one warm-up per variant and A B B A blocks of
//! five timed calls each, A first on even-indexed images and B first on
//! odd ones; per-image median; per class (oracle group, or set) p50/p95/max
//! by nearest rank over the valid pairs. Latency compares the pairs whose
//! median calls completed their reference walk: the Hodges-Lehmann
//! estimate of the per-image log-ratios with the exact signed-rank 95 %
//! interval — a regression only when the lower bound exceeds ln(1 + δ);
//! truncation and lost judgments get exact one-sided `McNemar` tests, work
//! the truncated pairs. Every process runs in its own group under an RSS
//! cap and a wall cap that outlive the harness; a killed call is a resource
//! failure, never a sample. `--dry-run` records the gate without enforcing
//! it (the caps and the host floor still apply) and labels every number
//! exploratory: no verdict.
//!
//! Exit codes: 0 completed (the whole declared frozen scope, nothing
//! regressed) · 1 a regression, or a failure (resource or protocol) · 2
//! usage, configuration, an unfit pair, inputs off their pins or an
//! existing run directory · 3 refused by the host lock, the gate or the
//! host floor, partial (a subset, something not measured, or a set row the
//! exclusion rule caught), or an exploratory dry run (never a pass).
//!
//! Privacy: decoded text crosses the worker pipes as hex and is hashed on
//! arrival; receipts and samples carry (symbology, text sha256, length)
//! and corpus-relative paths only — never decoded text. Worker stderr is
//! reduced to counts, `QRS-` codes and panic locations.

mod env;
mod gate;
mod manifest;
mod proc;
mod report;
mod run;
mod stats;
mod wasm;
#[allow(
    dead_code,
    reason = "its entry points run in the generated worker binaries; compiled here for lints and tests"
)]
mod worker;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::external;

const WORKER_TOML: &str = include_str!("../../bench/worker/Cargo.toml.in");
const WORKER_MAIN: &str = include_str!("../../bench/worker/main.rs.in");
const USAGE: &str = "usage: xtask bench <prepare --work <dir> [--node-addon <lib>] \
                     | prepare-base --work <dir> [--node-addon <lib>] [--cli-crate <file>] \
                     | stamp --source <dir> (--artifact <file> | --wasm --package <dir>) -- <build command…> \
                     | inspect <path> | gate [--wait <seconds>] | run --out <dir> …>";
/// The v0.9.0 release commit (tag `v0.9.0`): the source every rebuilt base
/// artifact comes from.
const BASE_COMMIT: &str = "170e3fb30008f0dc20793712213f7d4c6f2d233e";
/// The directory `prepare-base` extracts that commit into.
const BASE_DIR: &str = "qrcode-ai-scanner-0.9.0";

pub(crate) fn run(args: Vec<String>) {
    let mut args = args.into_iter();
    let code = match args.next().as_deref() {
        Some("prepare") => prepare(args.collect()),
        Some("prepare-base") => prepare_base(args.collect()),
        Some("stamp") => stamp(args.collect()),
        Some("inspect") => inspect(args.collect()),
        Some("gate") => gate_command(args.collect()),
        Some("run") => measure(args.collect()),
        _ => {
            eprintln!("{USAGE}\n{}", run::USAGE);
            2
        }
    };
    std::process::exit(code);
}

/// `--flag value` pairs, each at most once, from a closed list.
fn flags(
    args: Vec<String>,
    known: &[&'static str],
) -> Result<BTreeMap<&'static str, String>, String> {
    let mut values = BTreeMap::new();
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let key = known
            .iter()
            .copied()
            .find(|k| *k == flag)
            .ok_or_else(|| format!("unknown argument {flag:?}"))?;
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if values.insert(key, value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    Ok(values)
}

fn repo() -> PathBuf {
    let root = crate::repo_root();
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// The napi binary name native.js loads next to itself.
fn napi_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-x64"),
        ("linux", "x86_64") => Some("linux-x64-gnu"),
        ("linux", "aarch64") => Some("linux-arm64-gnu"),
        ("windows", "x86_64") => Some("win32-x64-msvc"),
        _ => None,
    }
}

fn write_file(path: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

/// The worker crate for one side: manifest, seeded lock, main.
fn write_worker(
    work: &Path,
    variant: &str,
    dependency: &str,
    root: &Path,
) -> Result<PathBuf, String> {
    let dir = work.join(format!("worker-{variant}"));
    let worker_rs = root.join("xtask/src/bench/worker.rs");
    let manifest = WORKER_TOML
        .replace("@VARIANT@", variant)
        .replace("@DEPENDENCY@", dependency);
    let main = WORKER_MAIN.replace("@WORKER_RS@", &format!("{:?}", worker_rs.to_string_lossy()));
    let lock = std::fs::read(root.join("Cargo.lock")).map_err(|e| format!("Cargo.lock: {e}"))?;
    write_file(&dir.join("Cargo.toml"), manifest.as_bytes())?;
    write_file(&dir.join("Cargo.lock"), &lock)?;
    write_file(&dir.join("src/main.rs"), main.as_bytes())?;
    Ok(dir)
}

fn prepare(args: Vec<String>) -> i32 {
    let mut values = match flags(args, &["--work", "--node-addon"]) {
        Ok(values) => values,
        Err(e) => {
            eprintln!("bench prepare: {e}\n{USAGE}");
            return 2;
        }
    };
    let Some(work) = values.remove("--work").map(PathBuf::from) else {
        eprintln!("bench prepare: --work is required\n{USAGE}");
        return 2;
    };
    match prepare_in(
        &work,
        values.remove("--node-addon").map(PathBuf::from).as_deref(),
    ) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("bench prepare: {e}");
            2
        }
    }
}

fn prepare_in(work: &Path, node_addon: Option<&Path>) -> Result<(), String> {
    let root = repo();
    let core = root.join("crates/qrcode-ai-scanner");
    let core = core
        .to_str()
        .filter(|p| !p.contains('\''))
        .ok_or("unusable scanner path")?;
    let tree = env::source(&root);
    let generated = [String::from("Cargo.toml"), String::from("src/main.rs")];
    let base = write_worker(work, "base", "\"=0.9.0\"", &root)?;
    let candidate = write_worker(work, "candidate", &format!("{{ path = '{core}' }}"), &root)?;
    for dir in [&base, &candidate] {
        manifest::mark_source(
            dir,
            "generated",
            "xtask bench prepare",
            (&tree.git_sha, &tree.tree),
            &generated,
        )?;
    }
    println!("worker crates (outside the workspace, lock seeded from Cargo.lock):");
    println!(
        "  {}  — qrcode-ai-scanner =0.9.0 from crates.io",
        base.display()
    );
    println!(
        "  {}  — qrcode-ai-scanner by path ({core})",
        candidate.display()
    );
    if let Some(addon) = node_addon {
        let dir = work.join("node-candidate");
        if !is_empty_dir(&dir) {
            return Err(format!(
                "{}: exists — assemble into a fresh directory",
                dir.display()
            ));
        }
        let bytes = std::fs::read(addon).map_err(|e| format!("{}: {e}", addon.display()))?;
        let stamped = match manifest::read(&manifest::manifest_path(addon)) {
            Ok(m) if m.artifact.sha256 == external::sha256_bytes(&bytes) => Some(m),
            Ok(_) => return Err(format!("{}: changed since it was stamped", addon.display())),
            Err(_) => None,
        };
        let has_manifest = stamped.is_some();
        node_package(
            &root.join("crates/qrcode-ai-scanner-node"),
            &bytes,
            stamped,
            &dir,
        )?;
        println!(
            "  {}  — the tree's Node package with {}{}",
            dir.display(),
            addon.display(),
            if has_manifest {
                " and its build manifest"
            } else {
                " (not stamped)"
            }
        );
    }
    println!(
        "\nbuild and stamp each worker on its own (one cargo process at a time) — the stamp runs the build:\n  \
         xtask bench stamp --source {b} --artifact <target>/release/qrscan-bench-worker-base \
         -- cargo build --release --manifest-path {b}/Cargo.toml\n  \
         xtask bench stamp --source {c} --artifact <target>/release/qrscan-bench-worker-candidate \
         -- cargo build --release --manifest-path {c}/Cargo.toml\n\
         published npm packages unpack with: node scripts/bench-node.mjs unpack <tgz> <dir> \
         --strip package/ [--only <file>]",
        b = base.display(),
        c = candidate.display()
    );
    Ok(())
}

/// `path` made absolute and canonical through its longest existing
/// ancestor: a directory that does not exist yet still resolves — and is
/// checked — before anything is created.
fn resolved(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => break,
        }
    }
    let mut out = std::fs::canonicalize(existing).unwrap_or_else(|_| existing.to_path_buf());
    for name in rest.iter().rev() {
        out.push(name);
    }
    out
}

/// Regular files of a tar archive (ustar, pax `path=` records, GNU long
/// names) into `dest`; returns their paths relative to `dest`. Refuses an
/// absolute path or one with a `..` component; skips links.
fn untar(tar: &[u8], dest: &Path) -> Result<Vec<String>, String> {
    let text = |header: &[u8], start: usize, len: usize| {
        let raw = header.get(start..start + len).unwrap_or_default();
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).into_owned()
    };
    let mut files = Vec::new();
    let mut long: Option<String> = None;
    let mut at = 0;
    while let Some(header) = tar.get(at..at + 512) {
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let size = usize::from_str_radix(text(header, 124, 12).trim(), 8)
            .map_err(|e| format!("tar entry size: {e}"))?;
        let kind = header[156];
        let prefix = if header.get(257..262) == Some(b"ustar".as_slice()) {
            text(header, 345, 155)
        } else {
            String::new()
        };
        let plain = text(header, 0, 100);
        let name = long.take().unwrap_or_else(|| {
            if prefix.is_empty() {
                plain
            } else {
                format!("{prefix}/{plain}")
            }
        });
        let body = tar
            .get(at + 512..at + 512 + size)
            .ok_or("truncated tar entry")?;
        at += 512 + size.div_ceil(512) * 512;
        match kind {
            b'x' => {
                long = String::from_utf8_lossy(body)
                    .lines()
                    .find_map(|l| l.split_once(" path=").map(|(_, p)| p.to_owned()));
            }
            b'L' => {
                long = Some(
                    String::from_utf8_lossy(body)
                        .trim_end_matches('\0')
                        .to_owned(),
                );
            }
            b'0' | 0 => {
                let rel = name.trim_start_matches("./");
                let path = Path::new(rel);
                if path.is_absolute()
                    || path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(format!("tar entry outside the destination: {rel:?}"));
                }
                write_file(&dest.join(path), body)?;
                #[cfg(unix)]
                if u32::from_str_radix(text(header, 100, 8).trim(), 8).is_ok_and(|m| m & 0o111 != 0)
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::set_permissions(
                        dest.join(path),
                        std::fs::Permissions::from_mode(0o755),
                    )
                    .map_err(|e| format!("{rel}: {e}"))?;
                }
                files.push(rel.to_owned());
            }
            _ => {}
        }
    }
    files.sort();
    Ok(files)
}

fn is_empty_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |mut entries| entries.next().is_none())
}

/// The v0.9.0 source tree, extracted once from `git archive` of
/// [`BASE_COMMIT`] and marked; a later call checks the mark instead.
fn base_source(work: &Path, root: &Path) -> Result<PathBuf, String> {
    let src = work.join(BASE_DIR);
    if src.join(manifest::SOURCE_MARK).is_file() {
        let id = manifest::source_of(&src);
        if id.kind != "archive" || id.sha != BASE_COMMIT || id.tree != "clean" {
            return Err(format!(
                "{}: not the clean v0.9.0 extraction ({} {} {})",
                src.display(),
                id.kind,
                id.sha,
                id.tree
            ));
        }
        return Ok(src);
    }
    if !is_empty_dir(&src) {
        return Err(format!(
            "{}: exists and is not an extraction",
            src.display()
        ));
    }
    let tag = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "v0.9.0^{commit}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    if tag.as_deref().is_some_and(|t| t != BASE_COMMIT) {
        return Err(format!("tag v0.9.0 names {tag:?}, not {BASE_COMMIT}"));
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["archive", "--format=tar", BASE_COMMIT])
        .output()
        .map_err(|e| format!("git archive: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git archive {BASE_COMMIT}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let files = untar(&out.stdout, &src)?;
    manifest::mark_source(
        &src,
        "archive",
        "git archive of tag v0.9.0",
        (BASE_COMMIT, "clean"),
        &files,
    )?;
    println!(
        "extracted {} files of {BASE_COMMIT} into {}",
        files.len(),
        src.display()
    );
    Ok(src)
}

/// `prepare-base`: the v0.9.0 sources of the rebuilt base artifacts,
/// outside the workspace — the git archive of the release commit (the Node
/// addon, and anything else built from the tree) and, given the published
/// `.crate`, the CLI crate with its packaged lock. With a built and stamped
/// addon it assembles the base Node package.
fn prepare_base(args: Vec<String>) -> i32 {
    let mut values = match flags(args, &["--work", "--node-addon", "--cli-crate"]) {
        Ok(values) => values,
        Err(e) => {
            eprintln!("bench prepare-base: {e}\n{USAGE}");
            return 2;
        }
    };
    let Some(work) = values.remove("--work").map(PathBuf::from) else {
        eprintln!("bench prepare-base: --work is required\n{USAGE}");
        return 2;
    };
    let addon = values.remove("--node-addon").map(PathBuf::from);
    let crate_file = values.remove("--cli-crate").map(PathBuf::from);
    match prepare_base_in(&work, addon.as_deref(), crate_file.as_deref()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("bench prepare-base: {e}");
            2
        }
    }
}

fn prepare_base_in(
    work: &Path,
    addon: Option<&Path>,
    crate_file: Option<&Path>,
) -> Result<(), String> {
    let root = repo();
    // resolved before anything is created: a relative or not-yet-existing
    // --work inside the workspace is refused too
    if resolved(work).starts_with(&root) {
        return Err(format!(
            "{}: extract outside the workspace (see Build parity in bench/README.md)",
            work.display()
        ));
    }
    let src = base_source(work, &root)?;
    if let Some(file) = crate_file {
        let dir = extract_crate(work, file)?;
        println!(
            "published crate in {d} — build and stamp it alone, with its packaged lock:\n  \
             xtask bench stamp --source {d} --artifact <target>/base-cli/release/qrscan \
             -- cargo build --release --locked --manifest-path {d}/Cargo.toml --target-dir <target>/base-cli",
            d = dir.display()
        );
    }
    println!(
        "v0.9.0 WASM package — build and stamp it from {s} on the candidate toolchain with the \
         release recipe (wasm-bindgen of its lock, binaryen version_130 on PATH):\n  \
         xtask bench stamp --wasm --source {s} --package {s}/crates/qrcode-ai-scanner-wasm/pkg \
         -- bash {s}/scripts/build-wasm.sh",
        s = src.display()
    );
    let Some(addon) = addon else {
        println!(
            "v0.9.0 source in {s} — build and stamp the base Node addon on the candidate toolchain, \
             its own lock and workspace release profile (one cargo process at a time):\n  \
             xtask bench stamp --source {s} --artifact <target>/base-node/release/<cdylib> \
             -- cargo build -p qrcode-ai-scanner-node --release --locked --manifest-path {s}/Cargo.toml \
             --target-dir <target>/base-node\n  \
             xtask bench prepare-base --work {w} --node-addon <target>/base-node/release/<cdylib>",
            s = src.display(),
            w = work.display()
        );
        return Ok(());
    };
    let stamped = manifest::read(&manifest::manifest_path(addon))?;
    if stamped.source.kind != "archive" || stamped.source.sha != BASE_COMMIT {
        return Err(format!(
            "{}: the addon's manifest names source {} {}, not the v0.9.0 archive",
            addon.display(),
            stamped.source.kind,
            stamped.source.sha
        ));
    }
    let bytes = std::fs::read(addon).map_err(|e| format!("{}: {e}", addon.display()))?;
    if external::sha256_bytes(&bytes) != stamped.artifact.sha256 {
        return Err(format!("{}: changed since it was stamped", addon.display()));
    }
    let dir = work.join("node-base");
    if !is_empty_dir(&dir) {
        return Err(format!(
            "{}: exists — assemble into a fresh directory",
            dir.display()
        ));
    }
    node_package(
        &src.join("crates/qrcode-ai-scanner-node"),
        &bytes,
        Some(stamped),
        &dir,
    )?;
    println!(
        "  {}  — the v0.9.0 Node package with the rebuilt addon",
        dir.display()
    );
    Ok(())
}

/// A published `.crate` (the base CLI, with its packaged lock) extracted
/// into `work` once and marked; a later call checks the mark instead.
fn extract_crate(work: &Path, file: &Path) -> Result<PathBuf, String> {
    let bytes = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let sha = external::sha256_bytes(&bytes);
    let listing = std::process::Command::new("tar")
        .arg("-tzf")
        .arg(file)
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    let top = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .next()
        .and_then(|l| l.split('/').next())
        .map(str::to_owned)
        .filter(|t| !t.is_empty() && !t.contains(".."))
        .ok_or("unreadable .crate listing")?;
    let dir = work.join(&top);
    if dir.join(manifest::SOURCE_MARK).is_file() {
        let id = manifest::source_of(&dir);
        if id.sha != sha || id.tree != "clean" {
            return Err(format!(
                "{}: not the clean extraction of this crate",
                dir.display()
            ));
        }
        return Ok(dir);
    }
    if !is_empty_dir(&dir) {
        return Err(format!(
            "{}: exists and is not an extraction",
            dir.display()
        ));
    }
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(file)
        .arg("-C")
        .arg(work)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err(format!("tar -xzf {}: {status}", file.display()));
    }
    let files = external::walk_sorted(&dir)?;
    manifest::mark_source(&dir, "crate", "published crate", (&sha, "clean"), &files)?;
    Ok(dir)
}

/// A Node package directory: the package files of `package`, the addon
/// under the name native.js loads, and the addon's build manifest.
fn node_package(
    package: &Path,
    addon: &[u8],
    stamped: Option<manifest::Manifest>,
    dir: &Path,
) -> Result<(), String> {
    let platform = napi_platform().ok_or("no napi binary name for this platform")?;
    for name in ["index.js", "native.js", "package.json"] {
        let bytes = std::fs::read(package.join(name)).map_err(|e| format!("{name}: {e}"))?;
        write_file(&dir.join(name), &bytes)?;
    }
    let binary = format!("qrcode-ai-scanner.{platform}.node");
    write_file(&dir.join(&binary), addon)?;
    if let Some(mut stamped) = stamped {
        stamped.artifact.name = binary;
        manifest::write(&dir.join(manifest::PACKAGE_MANIFEST), &stamped)?;
    }
    Ok(())
}

/// `stamp`: run an artifact's build and write the manifest the build
/// yields, next to the artifact. Native:
/// `--source DIR --artifact FILE -- <cargo build …>`; WASM:
/// `--wasm --source DIR --package DIR -- <the build script>`.
fn stamp(args: Vec<String>) -> i32 {
    let (flags, command) = match args.iter().position(|a| a == "--") {
        Some(at) => (args[..at].to_vec(), args[at + 1..].to_vec()),
        None => (args, Vec::new()),
    };
    let mut wasm = false;
    let mut values: BTreeMap<&str, PathBuf> = BTreeMap::new();
    let mut flags = flags.into_iter();
    while let Some(flag) = flags.next() {
        if flag == "--wasm" {
            wasm = true;
            continue;
        }
        let key = match flag.as_str() {
            "--artifact" => "artifact",
            "--source" => "source",
            "--package" => "package",
            _ => {
                eprintln!("bench stamp: unexpected {flag:?}\n{USAGE}");
                return 2;
            }
        };
        let Some(value) = flags.next() else {
            eprintln!("bench stamp: {flag} needs a value\n{USAGE}");
            return 2;
        };
        if values.insert(key, PathBuf::from(value)).is_some() {
            eprintln!("bench stamp: {flag} given twice\n{USAGE}");
            return 2;
        }
    }
    if command.is_empty() {
        eprintln!("bench stamp: the stamp runs the build — add -- <the build command>\n{USAGE}");
        return 2;
    }
    let target = if wasm { "package" } else { "artifact" };
    let (Some(source), Some(subject)) = (values.get("source"), values.get(target)) else {
        eprintln!("bench stamp: --source and --{target} are required\n{USAGE}");
        return 2;
    };
    let stamped = if wasm {
        manifest::stamp_wasm(subject, source, &command, "wasm-opt")
    } else {
        manifest::stamp_build(subject, source, &command)
    };
    let path = manifest::manifest_path(subject);
    match stamped.and_then(|m| manifest::write(&path, &m).map(|()| m)) {
        Ok(m) => {
            println!(
                "{} · {} · rustc {} · embedded [{}] · source {} {} ({})",
                path.display(),
                m.artifact.sha256,
                m.rustc.as_ref().map_or("unknown", |r| r.release.as_str()),
                m.embedded_rustc_commits.join(", "),
                m.source.kind,
                m.source.sha,
                m.source.tree
            );
            0
        }
        Err(e) => {
            eprintln!("bench stamp: {e}");
            2
        }
    }
}

/// `inspect`: what `bench run` would record about an artifact — its hash,
/// embedded compiler commits, WASM recipe and build manifest.
fn inspect(args: Vec<String>) -> i32 {
    let mut args = args.into_iter();
    let (Some(path), None) = (args.next(), args.next()) else {
        eprintln!("bench inspect: one path\n{USAGE}");
        return 2;
    };
    match run::describe(Path::new(&path)) {
        Ok(facts) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&facts).unwrap_or_default()
            );
            0
        }
        Err(e) => {
            eprintln!("bench inspect: {e}");
            2
        }
    }
}

fn gate_command(args: Vec<String>) -> i32 {
    let wait = flags(args, &["--wait"]).and_then(|mut values| {
        values
            .remove("--wait")
            .map(|text| {
                text.parse::<u64>()
                    .map_err(|_| String::from("--wait takes seconds"))
            })
            .transpose()
    });
    let wait = match wait {
        Ok(wait) => wait,
        Err(e) => {
            eprintln!("bench gate: {e}");
            return 2;
        }
    };
    let gate = gate::Gate::new(&repo());
    let open = match wait {
        None => {
            let reading = gate.read("now", 0.0);
            println!("{}", reading.line());
            reading.open
        }
        Some(seconds) => {
            // reads the gate as a run would, taking the host lock while it
            // waits; the lock goes when this command exits
            gate.wait("wait", Instant::now() + Duration::from_secs(seconds))
                .0
                .open
        }
    };
    if open { 0 } else { 3 }
}

/// The run's context over a measurement's options, images and artifacts.
fn context<'a>(
    measurement: &'a run::Measurement,
    gate: &'a gate::Gate,
    deadline: Instant,
) -> run::Context<'a> {
    run::Context {
        options: &measurement.options,
        gate,
        images: &measurement.images,
        artifacts: measurement
            .artifacts
            .iter()
            .map(|a| (a.key, a.path.clone()))
            .collect(),
        deadline,
        fixture: repo().join(run::FIXTURE),
        stopped: std::cell::RefCell::new(None),
    }
}

/// The executables a run starts: native ones only — a script could run
/// the real work in a grandchild the memory sampler never sees — and the
/// node of `--node-bin` resolved once to its own path.
fn checked_children(options: &mut run::Options) -> Result<(), String> {
    for (&(runtime, _), path) in &options.artifacts {
        if options.runtimes.contains(&runtime)
            && matches!(runtime, run::Runtime::Lib | run::Runtime::Cli)
        {
            run::native_executable(path)?;
        }
    }
    if options
        .runtimes
        .iter()
        .any(|r| matches!(r, run::Runtime::Node | run::Runtime::Wasm))
    {
        let node = run::resolve_node(&options.node, options.caps(Duration::from_secs(30)))?;
        node.to_str()
            .ok_or("the node path is not UTF-8")?
            .clone_into(&mut options.node);
    }
    Ok(())
}

/// What must hold before a run starts anything: the caps can be enforced
/// here, the stop handler is installed before the first child, process
/// memory can be sampled, and the run directory is fresh (receipts are
/// immutable).
fn preconditions(options: &run::Options) -> Result<(), String> {
    if !proc::CAPS_ENFORCED {
        return Err(String::from(
            "the resource caps need 64-bit macOS or Linux (see Resource caps in bench/README.md)",
        ));
    }
    proc::guard_signals().map_err(|e| format!("cannot install the stop handler: {e}"))?;
    if !proc::memory_sampled() {
        return Err(String::from(
            "process memory cannot be sampled here, so no memory cap can hold (see Resource caps in bench/README.md)",
        ));
    }
    if !is_empty_dir(&options.out) {
        return Err(format!(
            "{} exists and is not empty — every run gets a fresh --out",
            options.out.display()
        ));
    }
    Ok(())
}

/// The run's first line: what it measures, and under which caps.
fn announce(options: &run::Options, images: usize, inputs: &run::Inputs) {
    let selected: Vec<String> = inputs
        .selected
        .iter()
        .map(|(set, n)| format!("{set} {n}"))
        .collect();
    let names = |list: Vec<&str>| list.join(",");
    println!(
        "bench {} · {images} images ({}) · runtimes {} · modes {} · {} timed calls per image and variant · caps {} MiB, max(10 × budget, 60 s)",
        if options.dry_run {
            "DRY RUN (exploratory, gate not enforced, caps and host floor enforced)"
        } else {
            "gated run"
        },
        selected.join(", "),
        names(options.runtimes.iter().map(|r| r.as_str()).collect()),
        names(options.modes.iter().map(|m| m.as_str()).collect()),
        options.reps,
        options.rss_cap_mib
    );
}

fn measure(args: Vec<String>) -> i32 {
    let mut options = match run::parse_options(args) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("bench: {e}\n{}", run::USAGE);
            return 2;
        }
    };
    if let Err(e) = preconditions(&options) {
        eprintln!("bench: {e}");
        return 2;
    }
    let repo = repo();
    let gate = gate::Gate::new(&repo);
    let deadline = Instant::now() + Duration::from_secs(options.wait_gate.unwrap_or(0));
    // the host lock before anything else — node, images, environment
    // probes, preflight: nothing of this run executes beside another run;
    // a gated run waits for it until its deadline
    match gate.acquire_by(deadline) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "bench: another bench run holds the host lock — refused, nothing started (exit 3)"
            );
            return 3;
        }
        Err(e) => {
            eprintln!("bench: {e} — refused, nothing started (exit 3)");
            return 3;
        }
    }
    if let Err(e) = checked_children(&mut options) {
        eprintln!("bench: {e}");
        return 2;
    }
    let out = match std::fs::create_dir_all(&options.out)
        .and_then(|()| std::fs::canonicalize(&options.out))
    {
        Ok(out) => out,
        Err(e) => {
            eprintln!("bench: {}: {e}", options.out.display());
            return 2;
        }
    };
    let prepared = run::select(&options).and_then(|(images, inputs)| {
        let artifacts = run::snapshot(&options)?;
        Ok((images, inputs, artifacts))
    });
    let (images, inputs, artifacts) = match prepared {
        Ok(prepared) => prepared,
        Err(e) => {
            eprintln!("bench: {e}");
            return 2;
        }
    };
    announce(&options, images.len(), &inputs);
    let facts = env::collect(&repo, &options.node);
    let mut measurement = run::Measurement {
        threshold: gate.threshold,
        readings: Vec::new(),
        segments: Vec::new(),
        throughput: Vec::new(),
        wasm: None,
        refused: None,
        failures: Vec::new(),
        stopped: None,
        hellos: BTreeMap::new(),
        parity: Vec::new(),
        options,
        images,
        inputs,
        facts,
        artifacts,
    };
    measurement.refused = admit(&mut measurement, &gate, deadline);
    if measurement.refused.is_none() {
        measure_all(&mut measurement, &gate, deadline);
    }
    let end = if measurement.options.dry_run {
        gate.floor("run end")
    } else {
        gate.read("run end", 0.0)
    };
    measurement.readings.push(end);
    match report::write(&measurement, &out) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("bench: cannot write the receipt: {e}");
            2
        }
    }
}

/// Everything a run checks before it measures, the host lock held: a dry
/// run reads the host floor; every worker says hello (protocol, worker
/// source, loaded binary); a gated run refuses an unfit pair, then waits
/// for the gate.
fn admit(
    measurement: &mut run::Measurement,
    gate: &gate::Gate,
    deadline: Instant,
) -> Option<run::Refusal> {
    let dry = measurement.options.dry_run;
    for pin in &measurement.inputs.pins {
        println!("inputs: {pin}");
    }
    if !dry && !measurement.inputs.pins.is_empty() {
        return Some(run::Refusal::Inputs(format!(
            "the inputs are not the frozen set: {} (see Build parity in bench/README.md)",
            measurement.inputs.pins.join("; ")
        )));
    }
    if dry {
        let reading = gate.floor("run start");
        println!("{} (dry run: gate not enforced)", reading.line());
        let open = reading.open;
        let line = reading.line();
        measurement.readings.push(reading);
        if !open {
            return Some(run::Refusal::Floor(format!(
                "the host floor refused the dry run — {line}"
            )));
        }
    }
    let hellos = {
        let ctx = context(measurement, gate, deadline);
        run::preflight(&ctx)
    };
    match hellos {
        Ok(hellos) => measurement.hellos = hellos,
        Err(e) => return Some(run::Refusal::Parity(e)),
    }
    measurement.parity = run::parity(
        &measurement.artifacts,
        &measurement.hellos,
        &worker::source_sha256(),
    );
    for row in &measurement.parity {
        for finding in &row.findings {
            println!(
                "parity {}: {}: {}",
                row.runtime, finding.what, finding.detail
            );
        }
    }
    let unfit: Vec<String> = measurement
        .parity
        .iter()
        .filter(|row| !row.findings.is_empty())
        .map(|row| row.runtime.to_owned())
        .collect();
    if !dry && !unfit.is_empty() {
        return Some(run::Refusal::Parity(format!(
            "unfit pairs ({}): see the parity rows and Build parity in bench/README.md",
            unfit.join(", ")
        )));
    }
    if !dry {
        let (last, kept) = gate.wait("run start", deadline);
        measurement.readings.extend(kept);
        if !last.open {
            return Some(run::Refusal::Gate(format!(
                "the host gate stayed closed until the deadline, nothing was measured — last {}",
                last.line()
            )));
        }
    }
    None
}

/// Segments (runtime × mode), then throughput, then the WASM cost.
fn measure_all(measurement: &mut run::Measurement, gate: &gate::Gate, deadline: Instant) {
    let ctx = context(measurement, gate, deadline);
    let mut segments = Vec::new();
    for &runtime in &ctx.options.runtimes {
        for &mode in &ctx.options.modes {
            segments.push(run::run_segment(&ctx, runtime, mode));
        }
    }
    let mut throughput = Vec::new();
    let mut failures = Vec::new();
    for &mode in &ctx.options.throughput_modes {
        match run::run_throughput(&ctx, mode) {
            Ok(mut runs) => throughput.append(&mut runs),
            Err(e) => failures.push(format!("throughput {}: {e}", mode.as_str())),
        }
    }
    let wasm = if ctx.options.runtimes.contains(&run::Runtime::Wasm) {
        run::run_wasm_cost(&ctx)
            .map_err(|e| failures.push(format!("wasm cost: {e}")))
            .ok()
    } else {
        None
    };
    let stopped = ctx.stopped.borrow().clone();
    drop(ctx);
    measurement.segments = segments;
    measurement.throughput = throughput;
    measurement.wasm = wasm;
    measurement.failures = failures;
    measurement.stopped = stopped;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worker template mirrors the workspace release profile by hand;
    /// this keeps both workers on the workspace release profile when either
    /// side changes.
    #[test]
    fn worker_profiles_match_the_workspace_manifest() {
        let workspace = std::fs::read_to_string(crate::repo_root().join("Cargo.toml"))
            .expect("workspace manifest");
        let template = WORKER_TOML
            .replace("@VARIANT@", "base")
            .replace("@DEPENDENCY@", "\"=0.9.0\"");
        let profile = |text: &str| {
            manifest::effective_profile(Some(text), &[])
                .expect("profile")
                .0
        };
        assert_eq!(profile(&template), profile(&workspace));
        assert_eq!(
            profile(&template).get("lto").map(String::as_str),
            Some("\"thin\""),
            "the template carries a profile, not cargo's defaults"
        );
    }

    fn tar_entry(name: &str, body: &[u8], kind: u8) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644");
        header[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
        header[156] = kind;
        header[257..262].copy_from_slice(b"ustar");
        let mut entry = header;
        entry.extend_from_slice(body);
        entry.resize(entry.len().div_ceil(512) * 512, 0);
        entry
    }

    #[test]
    fn archives_extract_inside_their_destination_only() {
        let dest = std::env::temp_dir().join(format!("qrscan-bench-untar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let long = "deep/".repeat(30) + "file.txt";
        let pax = format!("{} path={long}\n", long.len() + 7 + 3);
        let mut tar = tar_entry("pax_global_header", b"52 comment=170e3fb\n", b'g');
        tar.extend(tar_entry("a/b.txt", b"hello", b'0'));
        tar.extend(tar_entry("PaxHeaders/x", pax.as_bytes(), b'x'));
        tar.extend(tar_entry("ignored-short-name", b"long", b'0'));
        tar.extend(tar_entry("dir/", b"", b'5'));
        tar.extend(vec![0u8; 1024]);
        let files = untar(&tar, &dest).expect("extracts");
        assert_eq!(files, vec![String::from("a/b.txt"), long.clone()]);
        assert_eq!(std::fs::read(dest.join("a/b.txt")).expect("file"), b"hello");
        assert_eq!(std::fs::read(dest.join(&long)).expect("long file"), b"long");
        let evil = tar_entry("../evil.txt", b"x", b'0');
        assert!(
            untar(&evil, &dest).is_err(),
            "no entry outside the destination"
        );
        assert!(!dest.join("../evil.txt").exists());
        let _ = std::fs::remove_dir_all(&dest);
    }

    /// A `prepare-base --work` that does not exist yet, relative or not,
    /// still resolves inside the workspace — and is refused before anything
    /// is created.
    #[test]
    fn prepare_base_refuses_a_work_dir_inside_the_workspace_before_creating_it() {
        let root = repo();
        let inside = root.join("bench/not-created/deeper");
        assert!(resolved(&inside).starts_with(&root));
        let relative = Path::new("not-created-relative");
        assert_eq!(
            resolved(relative),
            std::fs::canonicalize(".").expect("cwd").join(relative)
        );
        let refused = prepare_base_in(&inside, None, None);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("extract outside the workspace")),
            "{refused:?}"
        );
        assert!(!root.join("bench/not-created").exists(), "nothing created");
    }

    /// A run never writes into a directory that already holds something —
    /// exit 2, every file left as it was.
    #[test]
    fn an_existing_run_directory_is_refused() {
        let out = std::env::temp_dir().join(format!("qrscan-bench-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).expect("dir");
        std::fs::write(out.join("receipt.json"), "{\"status\": \"refused\"}\n").expect("receipt");
        let args = [
            "--out",
            out.to_str().expect("utf-8"),
            "--dry-run",
            "--lib-base",
            "/nonexistent/worker",
        ];
        assert_eq!(measure(args.iter().map(|a| (*a).to_owned()).collect()), 2);
        assert_eq!(
            std::fs::read_to_string(out.join("receipt.json")).expect("still there"),
            "{\"status\": \"refused\"}\n"
        );
        assert_eq!(
            std::fs::read_dir(&out).expect("dir").count(),
            1,
            "nothing added"
        );
        let _ = std::fs::remove_dir_all(&out);
    }
}
