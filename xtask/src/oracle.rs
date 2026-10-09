//! Strict content oracle — the complete detection list of every image
//! judged against its complete expected set.
//!
//! The external pins (`corpus-report --external`) are a tripwire: `match`
//! passes when SOME detection's text equals the truth, so `[expected,
//! wrong]` pins `match`, a wrong→wrong row stays green and gallery pins
//! prove detection only. This instrument gives each image one strict
//! [`Outcome`] instead.
//!
//! The unit is (symbology, text). A zxing row expects `{(qr_code, .txt)}`:
//! the exact file bytes (UTF-8, ISO-8859-1 fallback), never trimmed.
//! `corpus.toml` truth is text only, so its unit accepts any symbology; an
//! entry without `expected` is a negative sample unless it is a frontier
//! pin (`expect = "fail"`). Gallery and frontier rows have no truth: they
//! are `unlabelled_*` and never enter a rate denominator. Every report must
//! also meet the grouping contract of `spec/01-report.md`: at most 16
//! groups, QR family first, no duplicate (symbology, text).
//!
//! [`DISPOSITIONS`] holds the judgments a truth file cannot express,
//! pinned by image AND truth sha256 plus the verdict they were written
//! against: a changed verdict makes an entry stale and fails the run, so
//! the table is edited deliberately. Expected values are never relabelled
//! to match current output.
//!
//! Exit 0 = pass. Exit 1 = any `wrong`/`extra`/`mixed`/`false_positive`
//! (a `known_wrong` disposition still counts), any `error`, any contract
//! or integrity failure, a stale disposition, or a configured corpus root
//! (`--corpus-root`, `QRSCAN_EXTERNAL_CORPUS`) that does not exist. Exit 2
//! = usage error, unreadable committed input or unwritable receipt. Exit 3
//! = no corpus at the default root: `SKIPPED (not a pass)`. `missed` and
//! `partial` lower the exact rate without blocking.
//!
//! Scans are budget-free with scoring off ([`external::scanner`]), so no
//! verdict depends on host load. The `--json` receipt holds no timing and
//! no absolute path, and payloads only as text sha256 + byte length +
//! engines: two runs over the same inputs are byte-identical.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use qrcode_ai_scanner::{EngineKind, ImageInput, Scanner, Symbology, Versions};
use rayon::prelude::*;
use serde::ser::SerializeMap as _;
use serde::{Serialize, Serializer};

use crate::external::{self, CorpusRoot, Row, Status};

/// Judge-rules identifier carried by every receipt — bump it with any rule
/// change so receipts made under different rules never compare silently.
const RULES_ID: &str = "qrscan-oracle/v1";
/// The vendored manifest, at the repo root.
const VENDORED: &str = "corpus.toml";
/// Detection groups per report (`spec/01-report.md`, anti-amplification).
const MAX_GROUPS: usize = 16;
/// z of a two-sided 95% interval.
const WILSON_Z95: f64 = 1.959_963_984_540_054;
const USAGE: &str = "usage: xtask oracle [--json <path>] [--corpus-root <absolute path>]";

// ------------------------------------------------------------------ judge

/// Strict per-image outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Every expected unit found, nothing else.
    Exact,
    /// Every expected unit found, plus output nobody expected.
    Extra,
    /// Some expected units found, no unexpected output.
    Partial,
    /// Some expected units found, plus unexpected output.
    Mixed,
    /// Output, none of it expected.
    Wrong,
    /// Expected units, no output.
    Missed,
    /// A must-not-decode sample stayed blind.
    NegativeHeld,
    /// A must-not-decode sample decoded.
    FalsePositive,
    /// A disposition says the truth itself is incomplete.
    Ambiguous,
    /// No truth; something decoded.
    UnlabelledDecoded,
    /// No truth; nothing decoded.
    UnlabelledBlind,
    /// The scan returned `Err` or panicked — the image went unmeasured.
    Error,
}

impl Outcome {
    /// Declaration order — the column order of every table and receipt.
    const ALL: [Self; 12] = [
        Self::Exact,
        Self::Extra,
        Self::Partial,
        Self::Mixed,
        Self::Wrong,
        Self::Missed,
        Self::NegativeHeld,
        Self::FalsePositive,
        Self::Ambiguous,
        Self::UnlabelledDecoded,
        Self::UnlabelledBlind,
        Self::Error,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Extra => "extra",
            Self::Partial => "partial",
            Self::Mixed => "mixed",
            Self::Wrong => "wrong",
            Self::Missed => "missed",
            Self::NegativeHeld => "negative_held",
            Self::FalsePositive => "false_positive",
            Self::Ambiguous => "ambiguous",
            Self::UnlabelledDecoded => "unlabelled_decoded",
            Self::UnlabelledBlind => "unlabelled_blind",
            Self::Error => "error",
        }
    }

    /// Output that contradicts the truth — blocks in every case.
    fn is_wrong_class(self) -> bool {
        matches!(
            self,
            Self::Wrong | Self::Extra | Self::Mixed | Self::FalsePositive
        )
    }
}

impl Serialize for Outcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One expected content unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    /// `None` accepts any symbology (`corpus.toml` truth is text only).
    pub(crate) symbology: Option<Symbology>,
    /// Exact expected text — never trimmed or normalised.
    pub(crate) text: String,
}

/// The strict verdict for one image. `observed` is the report's complete
/// (symbology, text) list: an image is `exact` only when every expected
/// unit is found AND nothing else came back.
pub(crate) fn judge(expected: &[Unit], observed: &[(Symbology, &str)]) -> Outcome {
    let hit =
        |e: &Unit, o: &(Symbology, &str)| e.text == o.1 && e.symbology.is_none_or(|s| s == o.0);
    if expected.is_empty() {
        return if observed.is_empty() {
            Outcome::NegativeHeld
        } else {
            Outcome::FalsePositive
        };
    }
    if observed.is_empty() {
        return Outcome::Missed;
    }
    let found = expected
        .iter()
        .filter(|e| observed.iter().any(|o| hit(e, o)))
        .count();
    let extra = observed.iter().any(|o| !expected.iter().any(|e| hit(e, o)));
    match (found, found == expected.len(), extra) {
        (0, _, _) => Outcome::Wrong,
        (_, true, false) => Outcome::Exact,
        (_, true, true) => Outcome::Extra,
        (_, false, false) => Outcome::Partial,
        (_, false, true) => Outcome::Mixed,
    }
}

/// Grouping-contract violations of one report (`spec/01-report.md`).
/// Messages name detection indices only — payload text never leaves here.
pub(crate) fn contract_violations(observed: &[(Symbology, &str)]) -> Vec<String> {
    let mut found = Vec::new();
    if observed.len() > MAX_GROUPS {
        found.push(format!(
            "{} detection groups exceed the cap of {MAX_GROUPS}",
            observed.len()
        ));
    }
    if let Some(first_other) = observed.iter().position(|(s, _)| !s.is_qr_family())
        && let Some(late) = observed[first_other..]
            .iter()
            .position(|(s, _)| s.is_qr_family())
    {
        found.push(format!(
            "QR-family detection #{} follows non-QR detection #{first_other}",
            first_other + late
        ));
    }
    for (index, group) in observed.iter().enumerate() {
        if let Some(first) = observed[..index].iter().position(|g| g == group) {
            found.push(format!(
                "detection #{index} repeats group #{first} (same symbology and text)"
            ));
        }
    }
    found
}

// ----------------------------------------------------------- dispositions

/// What a disposition says about its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispositionKind {
    /// A confident decode the truth contradicts: stays `wrong` and blocks
    /// until fixed or honestly refused.
    KnownWrong,
    /// The truth names fewer symbols than the image holds: `ambiguous` —
    /// never `exact`, never `wrong` — until the truth is amended with
    /// independent confirmation.
    TruthIncomplete,
}

impl DispositionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::KnownWrong => "known_wrong",
            Self::TruthIncomplete => "truth_incomplete",
        }
    }

    /// The outcome a holding disposition reports.
    fn outcome(self) -> Outcome {
        match self {
            Self::KnownWrong => Outcome::Wrong,
            Self::TruthIncomplete => Outcome::Ambiguous,
        }
    }
}

/// One pinned judgment on one external row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Disposition {
    pub(crate) path: &'static str,
    pub(crate) image_sha256: &'static str,
    pub(crate) truth_sha256: &'static str,
    pub(crate) kind: DispositionKind,
    /// The judge verdict this entry was written against.
    pub(crate) verdict: Outcome,
    /// Why — public corpus facts only.
    pub(crate) note: &'static str,
}

/// Pinned dispositions; hashes copied from the committed manifest.
pub(crate) const DISPOSITIONS: [Disposition; 2] = [
    Disposition {
        path: "zxing-blackbox/qrcode-2/13.png",
        image_sha256: "a8d498d6d2d6a3e27e24fd75ed023cd5b8b6b42cb4996674e88e555d68855ec1",
        truth_sha256: "c19d2dcb13469aae263a41cd73add68c45e086223a04c41368128c47143c0e5a",
        kind: DispositionKind::KnownWrong,
        verdict: Outcome::Wrong,
        note: "one letter off: the decode reads \"photography\" where the truth reads \"photograph\"",
    },
    Disposition {
        path: "zxing-blackbox/qrcode-2/16.png",
        image_sha256: "24fef8babbb2862f2f37fe05b74b45daaa87f1b0c5d7610c0f40eb63becfe9e1",
        truth_sha256: "c93aaefb0b894232954b026da265394fedffd76a1ac251704ff83095b3b183be",
        kind: DispositionKind::TruthIncomplete,
        verdict: Outcome::Extra,
        note: "double QR (one symbol nested in another): the truth names the outer one only",
    },
];

/// How a disposition met its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispositionState {
    /// Hashes and verdict as pinned — the disposition's outcome applies.
    Holds,
    /// Hashes as pinned, verdict changed — edit the table deliberately.
    Stale,
    /// The row's image or truth sha256 differs from the pinned one.
    Void,
    /// The row was not judged (absent, unreadable, drifted or errored).
    Unevaluated,
}

impl DispositionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Holds => "holds",
            Self::Stale => "stale",
            Self::Void => "void",
            Self::Unevaluated => "unevaluated",
        }
    }
}

/// A disposition settled against the current run.
struct DispositionCheck {
    disposition: Disposition,
    state: DispositionState,
    /// The raw judge verdict of its row, when the row was judged.
    observed: Option<Outcome>,
}

/// Final outcome of a judged row under its disposition.
fn settle(
    verdict: Outcome,
    disposition: &Disposition,
    image_sha256: &str,
    truth_sha256: &str,
) -> (Outcome, DispositionState) {
    if disposition.image_sha256 != image_sha256 || disposition.truth_sha256 != truth_sha256 {
        (verdict, DispositionState::Void)
    } else if verdict == disposition.verdict {
        (disposition.kind.outcome(), DispositionState::Holds)
    } else {
        (verdict, DispositionState::Stale)
    }
}

/// sha256 of the table's canonical text — a receipt names the exact table
/// its verdicts were settled under.
fn dispositions_sha256() -> String {
    let mut canon = String::new();
    for d in &DISPOSITIONS {
        writeln!(
            canon,
            "{}\t{}\t{}\t{}\t{}",
            d.path,
            d.image_sha256,
            d.truth_sha256,
            d.kind.as_str(),
            d.verdict.as_str()
        )
        .expect("write to string");
    }
    external::sha256_bytes(canon.as_bytes())
}

/// Settle every disposition against its judged row (in place).
fn apply_dispositions(rows: &mut [Judged]) -> Vec<DispositionCheck> {
    DISPOSITIONS
        .iter()
        .map(|&disposition| {
            let unevaluated = DispositionCheck {
                disposition,
                state: DispositionState::Unevaluated,
                observed: None,
            };
            let Some(row) = rows.iter_mut().find(|row| row.path == disposition.path) else {
                return unevaluated;
            };
            let Some(verdict) = row.verdict else {
                return unevaluated;
            };
            let truth_sha256 = row.truth_sha256.as_deref().unwrap_or_default();
            let (outcome, state) = settle(verdict, &disposition, &row.image_sha256, truth_sha256);
            row.outcome = outcome;
            row.disposition = Some((disposition.kind, state));
            DispositionCheck {
                disposition,
                state,
                observed: Some(verdict),
            }
        })
        .collect()
}

// --------------------------------------------------------------- measure

/// Corpus sets, in table order: labelled frozen, unlabelled frozen, dev.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Set {
    Zxing,
    Gallery,
    Vendored,
}

impl Set {
    fn as_str(self) -> &'static str {
        match self {
            Self::Zxing => "zxing",
            Self::Gallery => "gallery",
            Self::Vendored => "vendored",
        }
    }
}

/// One detection as observed. The text stays in memory: receipts carry
/// its sha256 and length only, stdout previews it for public sets only.
struct Seen {
    symbology: Symbology,
    text: String,
    engines: Vec<EngineKind>,
}

/// What one scan produced.
enum Scan {
    Report {
        detections: Vec<Seen>,
        engine_panics: u8,
    },
    /// `code` is the `QRS-xxx` wire code, or `panic`.
    Failed { code: &'static str, detail: String },
}

fn scan_bytes(scanner: &Scanner, bytes: &[u8]) -> Scan {
    // A panic escaping the lib must cost one named row, not the whole run.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scanner.scan(ImageInput::encoded(bytes))
    }));
    match caught {
        Ok(Ok(report)) => Scan::Report {
            engine_panics: report.trace.engine_panics,
            detections: report
                .detections
                .into_iter()
                .map(|d| Seen {
                    symbology: d.symbology,
                    text: d.content.text,
                    engines: d.engines,
                })
                .collect(),
        },
        Ok(Err(e)) => Scan::Failed {
            code: e.code(),
            detail: e.to_string(),
        },
        Err(payload) => Scan::Failed {
            code: "panic",
            detail: payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default(),
        },
    }
}

/// One judged image.
struct Judged {
    set: Set,
    group: String,
    path: String,
    image_sha256: String,
    /// External manifest pin (the any-based status), for contrast.
    pin: Option<Status>,
    /// `None` unlabelled · empty = negative sample · else the truth.
    expected: Option<Vec<Unit>>,
    /// sha256 of the truth's source bytes (`.txt` file or manifest string).
    truth_sha256: Option<String>,
    /// Raw judge verdict (labelled rows), before any disposition.
    verdict: Option<Outcome>,
    outcome: Outcome,
    disposition: Option<(DispositionKind, DispositionState)>,
    scan: Scan,
    contract: Vec<String>,
}

/// Raw verdict (labelled rows), outcome before dispositions, and the
/// grouping-contract violations of one scan.
fn verdict_of(expected: Option<&[Unit]>, scan: &Scan) -> (Option<Outcome>, Outcome, Vec<String>) {
    let Scan::Report { detections, .. } = scan else {
        return (None, Outcome::Error, Vec::new());
    };
    let observed: Vec<(Symbology, &str)> = detections
        .iter()
        .map(|d| (d.symbology, d.text.as_str()))
        .collect();
    let contract = contract_violations(&observed);
    match expected {
        Some(units) => {
            let verdict = judge(units, &observed);
            (Some(verdict), verdict, contract)
        }
        None if observed.is_empty() => (None, Outcome::UnlabelledBlind, contract),
        None => (None, Outcome::UnlabelledDecoded, contract),
    }
}

/// Integrity accounting for both corpora.
#[derive(Default, Serialize)]
struct Integrity {
    manifest_rows: usize,
    /// Manifest rows present on disk with the pinned sha256.
    verified: usize,
    vendored_entries: usize,
    vendored_read: usize,
    problems: Vec<String>,
}

/// One manifest row re-read from disk. The bytes are hashed once and the
/// SAME bytes are scanned — nothing can change between check and judgment.
enum Probe {
    Unreadable(String),
    Drift,
    /// Hash-verified non-image; a `.txt` file's content decoded as truth.
    Aux(Option<String>),
    /// Hash-verified image and its scan.
    Image(Scan),
}

fn probe(dir: &Path, row: &Row, scanner: &Scanner) -> Probe {
    let bytes = match std::fs::read(dir.join(&row.path)) {
        Ok(bytes) => bytes,
        Err(e) => return Probe::Unreadable(e.to_string()),
    };
    if external::sha256_bytes(&bytes) != row.sha256 {
        return Probe::Drift;
    }
    if external::is_image(&row.path) {
        return Probe::Image(scan_bytes(scanner, &bytes));
    }
    let is_text = Path::new(&row.path)
        .extension()
        .is_some_and(|ext| ext == "txt");
    Probe::Aux(is_text.then(|| external::decode_truth(bytes)))
}

/// Sibling truth file of a manifest image (`…/13.png` → `…/13.txt`) — the
/// rule `external::ground_truth` reads by.
fn truth_path(image: &str) -> String {
    Path::new(image)
        .with_extension("txt")
        .to_string_lossy()
        .replace('\\', "/")
}

/// The symbology a labelled suite's truth is about — the zxing suite name
/// carries it. Any other suite has no pinned symbology and is not judged.
fn suite_symbology(path: &str) -> Option<Symbology> {
    path.strip_prefix("zxing-blackbox/qrcode-")
        .map(|_| Symbology::QrCode)
}

/// Re-walk, re-hash and scan the external corpus (`verify`'s integrity
/// rules), then judge every hash-verified image row.
fn evaluate_external(
    dir: &Path,
    pinned: &[Row],
    scanner: &Scanner,
    integrity: &mut Integrity,
) -> Vec<Judged> {
    let on_disk: BTreeSet<String> = external::walk_sorted(dir).into_iter().collect();
    let manifested: BTreeSet<&str> = pinned.iter().map(|row| row.path.as_str()).collect();
    for path in on_disk.iter().filter(|p| !manifested.contains(p.as_str())) {
        integrity.problems.push(format!(
            "unmanifested file on disk (regenerate the manifest): {path}"
        ));
    }
    for row in pinned {
        if !on_disk.contains(&row.path) {
            integrity
                .problems
                .push(format!("manifested file missing on disk: {}", row.path));
        }
        if external::is_image(&row.path) == (row.status == Status::Aux) {
            integrity.problems.push(format!(
                "manifest status {} contradicts the file kind: {}",
                row.status.as_str(),
                row.path
            ));
        }
    }
    let probes: Vec<(&Row, Probe)> = pinned
        .par_iter()
        .filter(|row| on_disk.contains(&row.path))
        .map(|row| (row, probe(dir, row, scanner)))
        .collect();

    // path → (pinned sha256, text) of every hash-verified truth file
    let mut truths: BTreeMap<&str, (&str, String)> = BTreeMap::new();
    let mut scans: Vec<(&Row, Scan)> = Vec::new();
    for (row, probe) in probes {
        match probe {
            Probe::Unreadable(e) => integrity
                .problems
                .push(format!("unreadable: {}: {e}", row.path)),
            Probe::Drift => integrity.problems.push(format!(
                "sha256 drift (corpus file changed — regenerate or restore): {}",
                row.path
            )),
            Probe::Aux(text) => {
                integrity.verified += 1;
                if let Some(text) = text {
                    truths.insert(row.path.as_str(), (row.sha256.as_str(), text));
                }
            }
            Probe::Image(scan) => {
                integrity.verified += 1;
                scans.push((row, scan));
            }
        }
    }
    scans
        .into_iter()
        .filter_map(|(row, scan)| external_row(row, scan, &truths, &mut integrity.problems))
        .collect()
}

/// Expected set + judgment of one hash-verified external image.
fn external_row(
    row: &Row,
    scan: Scan,
    truths: &BTreeMap<&str, (&str, String)>,
    problems: &mut Vec<String>,
) -> Option<Judged> {
    let (set, expected, truth_sha256) = if row.path.starts_with("zxing-blackbox/") {
        let truth_file = truth_path(&row.path);
        let (Some(symbology), Some((sha256, text))) =
            (suite_symbology(&row.path), truths.get(truth_file.as_str()))
        else {
            problems.push(format!(
                "labelled image without a verified {truth_file} or a pinned symbology: {}",
                row.path
            ));
            return None;
        };
        let unit = Unit {
            symbology: Some(symbology),
            text: text.clone(),
        };
        (Set::Zxing, Some(vec![unit]), Some((*sha256).to_owned()))
    } else if row.path.starts_with("qrcode-ai/") {
        (Set::Gallery, None, None)
    } else {
        problems.push(format!(
            "image outside the known corpus roots: {}",
            row.path
        ));
        return None;
    };
    let (verdict, outcome, contract) = verdict_of(expected.as_deref(), &scan);
    Some(Judged {
        set,
        group: external::group_of(&row.path),
        path: row.path.clone(),
        image_sha256: row.sha256.clone(),
        pin: Some(row.status),
        expected,
        truth_sha256,
        verdict,
        outcome,
        disposition: None,
        scan,
        contract,
    })
}

/// `corpus.toml` → expected set: `expected` is text-only truth; no
/// `expected` is a negative sample, unless the entry is a frontier pin
/// (`expect = "fail"`), which has no truth at all.
pub(crate) fn vendored_expectation(
    expected: Option<&str>,
    expect: Option<&str>,
) -> Option<Vec<Unit>> {
    match (expected, expect) {
        (Some(text), _) => Some(vec![Unit {
            symbology: None,
            text: text.to_owned(),
        }]),
        (None, Some("fail")) => None,
        (None, _) => Some(Vec::new()),
    }
}

/// A vendored fixture read once: its sha256 and scan, or the read error.
type VendoredProbe<'a> = (&'a crate::Entry, Result<(String, Scan), String>);

/// Scan and judge every `corpus.toml` entry (vendored, always present).
fn evaluate_vendored(
    root: &Path,
    corpus: &crate::Corpus,
    scanner: &Scanner,
    integrity: &mut Integrity,
) -> Vec<Judged> {
    let probes: Vec<VendoredProbe<'_>> = corpus
        .entry
        .par_iter()
        .map(|entry| {
            let probe = std::fs::read(root.join(&entry.path))
                .map(|bytes| (external::sha256_bytes(&bytes), scan_bytes(scanner, &bytes)))
                .map_err(|e| e.to_string());
            (entry, probe)
        })
        .collect();
    let mut rows = Vec::with_capacity(probes.len());
    for (entry, probe) in probes {
        let (image_sha256, scan) = match probe {
            Ok(measured) => measured,
            Err(e) => {
                integrity
                    .problems
                    .push(format!("vendored fixture unreadable: {}: {e}", entry.path));
                continue;
            }
        };
        integrity.vendored_read += 1;
        let expected = vendored_expectation(entry.expected.as_deref(), entry.expect.as_deref());
        let (verdict, outcome, contract) = verdict_of(expected.as_deref(), &scan);
        rows.push(Judged {
            set: Set::Vendored,
            group: format!("vendored/{}", entry.category),
            path: entry.path.clone(),
            image_sha256,
            pin: None,
            expected,
            truth_sha256: entry
                .expected
                .as_deref()
                .map(|text| external::sha256_bytes(text.as_bytes())),
            verdict,
            outcome,
            disposition: None,
            scan,
            contract,
        });
    }
    rows
}

/// Every reason this run is not a pass, in a stable order.
fn blockers(integrity: &Integrity, rows: &[Judged], checks: &[DispositionCheck]) -> Vec<String> {
    let mut out: Vec<String> = integrity
        .problems
        .iter()
        .map(|p| format!("integrity — {p}"))
        .collect();
    for row in rows {
        if row.outcome.is_wrong_class() {
            let known_wrong = matches!(
                row.disposition,
                Some((DispositionKind::KnownWrong, DispositionState::Holds))
            );
            let label = if known_wrong {
                " [known_wrong: counted wrong until fixed or honestly refused]"
            } else {
                ""
            };
            out.push(format!("{}{label} — {}", row.outcome.as_str(), row.path));
        }
        if let Scan::Failed { code, .. } = &row.scan {
            out.push(format!("error ({code}) — {}", row.path));
        }
        for violation in &row.contract {
            out.push(format!("contract — {}: {violation}", row.path));
        }
    }
    for check in checks {
        let d = &check.disposition;
        let why = match (check.state, check.observed) {
            (DispositionState::Holds, _) => continue,
            (DispositionState::Stale, Some(now)) => format!(
                "pinned verdict {}, now {} — update DISPOSITIONS deliberately",
                d.verdict.as_str(),
                now.as_str()
            ),
            (DispositionState::Void, _) => {
                "image or truth sha256 differs from the pinned one".to_owned()
            }
            _ => "row not judged (absent, unreadable, drifted or errored)".to_owned(),
        };
        out.push(format!(
            "{} disposition [{}] — {}: {why}",
            check.state.as_str(),
            d.kind.as_str(),
            d.path
        ));
    }
    out
}

// ---------------------------------------------------------------- tallies

/// Outcome counts in [`Outcome::ALL`] order.
#[derive(Debug, Clone, Copy, Default)]
struct Counts([u32; 12]);

impl Counts {
    fn add(&mut self, outcome: Outcome) {
        self.0[outcome as usize] += 1;
    }

    fn get(&self, outcome: Outcome) -> u32 {
        self.0[outcome as usize]
    }
}

impl Serialize for Counts {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(Outcome::ALL.len()))?;
        for outcome in Outcome::ALL {
            map.serialize_entry(outcome.as_str(), &self.get(outcome))?;
        }
        map.end()
    }
}

/// One table line: images by label kind, plus outcomes.
#[derive(Debug, Clone, Copy, Default, Serialize)]
struct Tally {
    images: u32,
    /// Rows with expected units — the exact-rate denominator.
    truth: u32,
    negatives: u32,
    unlabelled: u32,
    outcomes: Counts,
}

impl Tally {
    fn add(&mut self, row: &Judged) {
        self.images += 1;
        match row.expected.as_deref() {
            None => self.unlabelled += 1,
            Some([]) => self.negatives += 1,
            Some(_) => self.truth += 1,
        }
        self.outcomes.add(row.outcome);
    }

    fn exact(&self) -> u32 {
        self.outcomes.get(Outcome::Exact)
    }
}

type Groups = BTreeMap<(Set, String), Tally>;

fn tallies(rows: &[Judged]) -> (Groups, BTreeMap<Set, Tally>) {
    let mut groups = Groups::new();
    let mut sets: BTreeMap<Set, Tally> = BTreeMap::new();
    for row in rows {
        groups
            .entry((row.set, row.group.clone()))
            .or_default()
            .add(row);
        sets.entry(row.set).or_default().add(row);
    }
    (groups, sets)
}

/// Wilson score 95% interval of `k` successes in `n` (3 decimals) — stays
/// honest at the 0/n and n/n edges where the normal interval collapses.
fn wilson95(k: u32, n: u32) -> Option<[String; 2]> {
    if n == 0 {
        return None;
    }
    let (k, n) = (f64::from(k), f64::from(n));
    let p = k / n;
    let z2 = WILSON_Z95 * WILSON_Z95;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let half = WILSON_Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    Some([
        format!("{:.3}", (centre - half).max(0.0)),
        format!("{:.3}", (centre + half).min(1.0)),
    ])
}

// ---------------------------------------------------------------- receipt

/// The scan configuration behind every verdict — mirrors
/// `external::scanner()`.
#[derive(Serialize)]
struct ScanEcho {
    profile: &'static str,
    budget_ms: Option<u64>,
    score_depth: &'static str,
}

const SCAN_ECHO: ScanEcho = ScanEcho {
    profile: "full",
    budget_ms: None,
    score_depth: "off",
};

#[derive(Serialize)]
struct FileEcho {
    path: &'static str,
    sha256: String,
}

#[derive(Serialize)]
struct Inputs {
    manifest: FileEcho,
    vendored: FileEcho,
    dispositions_sha256: String,
}

#[derive(Serialize)]
struct SetReceipt {
    set: &'static str,
    #[serde(flatten)]
    tally: Tally,
    /// Wilson 95% interval of exact / truth.
    exact_wilson95: Option<[String; 2]>,
}

#[derive(Serialize)]
struct GroupReceipt {
    set: &'static str,
    group: String,
    #[serde(flatten)]
    tally: Tally,
}

#[derive(Serialize)]
struct DispositionReceipt {
    path: &'static str,
    kind: &'static str,
    pinned_verdict: Outcome,
    observed_verdict: Option<Outcome>,
    state: &'static str,
}

#[derive(Serialize)]
struct UnitReceipt {
    symbology: Option<Symbology>,
    text_sha256: String,
    text_len: usize,
}

#[derive(Serialize)]
struct DetectionReceipt {
    symbology: Symbology,
    text_sha256: String,
    text_len: usize,
    engines: Vec<EngineKind>,
}

/// One image. Payloads appear as sha256 + byte length only.
#[derive(Serialize)]
struct RowReceipt {
    set: &'static str,
    group: String,
    path: String,
    image_sha256: String,
    pin: Option<&'static str>,
    truth_sha256: Option<String>,
    /// `null` = unlabelled · `[]` = negative sample.
    expected: Option<Vec<UnitReceipt>>,
    verdict: Option<Outcome>,
    outcome: Outcome,
    disposition: Option<&'static str>,
    disposition_state: Option<&'static str>,
    detections: Vec<DetectionReceipt>,
    engine_panics: u8,
    error: Option<&'static str>,
    contract: Vec<String>,
}

#[derive(Serialize)]
struct Receipt {
    oracle: &'static str,
    status: &'static str,
    exit_code: i32,
    reason: Option<&'static str>,
    versions: Versions,
    scan: ScanEcho,
    inputs: Inputs,
    integrity: Integrity,
    sets: Vec<SetReceipt>,
    groups: Vec<GroupReceipt>,
    dispositions: Vec<DispositionReceipt>,
    blockers: Vec<String>,
    rows: Vec<RowReceipt>,
}

fn text_hash(text: &str) -> (String, usize) {
    (external::sha256_bytes(text.as_bytes()), text.len())
}

fn row_receipt(row: &Judged) -> RowReceipt {
    let (detections, engine_panics, error) = match &row.scan {
        Scan::Report {
            detections,
            engine_panics,
        } => {
            let detections = detections
                .iter()
                .map(|d| {
                    let (text_sha256, text_len) = text_hash(&d.text);
                    DetectionReceipt {
                        symbology: d.symbology,
                        text_sha256,
                        text_len,
                        engines: d.engines.clone(),
                    }
                })
                .collect();
            (detections, *engine_panics, None)
        }
        Scan::Failed { code, .. } => (Vec::new(), 0, Some(*code)),
    };
    let expected = row.expected.as_ref().map(|units| {
        units
            .iter()
            .map(|unit| {
                let (text_sha256, text_len) = text_hash(&unit.text);
                UnitReceipt {
                    symbology: unit.symbology,
                    text_sha256,
                    text_len,
                }
            })
            .collect()
    });
    RowReceipt {
        set: row.set.as_str(),
        group: row.group.clone(),
        path: row.path.clone(),
        image_sha256: row.image_sha256.clone(),
        pin: row.pin.map(Status::as_str),
        truth_sha256: row.truth_sha256.clone(),
        expected,
        verdict: row.verdict,
        outcome: row.outcome,
        disposition: row.disposition.map(|(kind, _)| kind.as_str()),
        disposition_state: row.disposition.map(|(_, state)| state.as_str()),
        detections,
        engine_panics,
        error,
        contract: row.contract.clone(),
    }
}

fn write_receipt(path: &Path, receipt: &Receipt) -> Result<(), String> {
    let mut json = serde_json::to_string_pretty(receipt).map_err(|e| e.to_string())?;
    json.push('\n');
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))
}

// ----------------------------------------------------------------- stdout

/// Wire spelling of a serde enum (`qr_code`, `rqrr`).
pub(crate) fn wire<T: Serialize>(value: &T) -> String {
    if let Ok(serde_json::Value::String(name)) = serde_json::to_value(value) {
        name
    } else {
        String::from("?")
    }
}

/// Escaped, char-bounded preview of PUBLIC text (zxing, vendored). `{:?}`
/// neutralises terminal control bytes in decoded, attacker-controlled text.
fn preview(text: &str) -> String {
    const MAX_CHARS: usize = 40;
    let mut cut: String = text.chars().take(MAX_CHARS).collect();
    if cut.len() < text.len() {
        cut.push('…');
    }
    format!("{cut:?}")
}

fn print_groups(groups: &Groups) {
    let mut table = String::from("| group | images |");
    for outcome in Outcome::ALL {
        write!(table, " {} |", outcome.as_str()).expect("write to string");
    }
    table.push_str(" exact n/N |\n|---|---|");
    table.push_str(&"---|".repeat(Outcome::ALL.len() + 1));
    table.push('\n');
    for ((_, group), tally) in groups {
        write!(table, "| {group} | {} |", tally.images).expect("write to string");
        for outcome in Outcome::ALL {
            write!(table, " {} |", tally.outcomes.get(outcome)).expect("write to string");
        }
        if tally.truth == 0 {
            table.push_str(" — |\n");
        } else {
            writeln!(table, " {}/{} |", tally.exact(), tally.truth).expect("write to string");
        }
    }
    println!("\n{table}");
}

fn print_sets(sets: &BTreeMap<Set, Tally>, integrity: &Integrity) {
    for (set, tally) in sets {
        let name = set.as_str();
        let mut line = format!("{name:<9} {} images", tally.images);
        if tally.truth > 0 {
            write!(line, " · exact {}/{}", tally.exact(), tally.truth).expect("write to string");
            if let Some([low, high]) = wilson95(tally.exact(), tally.truth) {
                write!(line, " (Wilson 95% {low}–{high})").expect("write to string");
            }
        }
        if tally.negatives > 0 {
            let held = tally.outcomes.get(Outcome::NegativeHeld);
            write!(line, " · negative_held {held}/{}", tally.negatives).expect("write to string");
        }
        for outcome in Outcome::ALL {
            let n = tally.outcomes.get(outcome);
            if n > 0 && !matches!(outcome, Outcome::Exact | Outcome::NegativeHeld) {
                write!(line, " · {} {n}", outcome.as_str()).expect("write to string");
            }
        }
        if tally.truth + tally.negatives == 0 {
            line.push_str(" — no truth, never a rate");
        }
        println!("{line}");
    }
    println!(
        "integrity {}/{} manifest rows verified (present, pinned sha256) · {}/{} vendored entries read · {} problems",
        integrity.verified,
        integrity.manifest_rows,
        integrity.vendored_read,
        integrity.vendored_entries,
        integrity.problems.len()
    );
}

fn print_dispositions(checks: &[DispositionCheck]) {
    println!(
        "\ndispositions (pinned by image + truth sha256 · table {}):",
        dispositions_sha256()
    );
    for check in checks {
        let d = &check.disposition;
        let observed = check.observed.map_or("—", Outcome::as_str);
        println!(
            "  {:<16} {} — {} (verdict {observed}, pinned {}) · {}",
            d.kind.as_str(),
            d.path,
            check.state.as_str(),
            d.verdict.as_str(),
            d.note
        );
    }
}

/// Labelled rows that are not `exact`/`negative_held`, with escaped
/// previews — public sets only, never the gallery.
fn print_labelled_misses(rows: &[Judged]) {
    let misses: Vec<&Judged> = rows
        .iter()
        .filter(|row| matches!(row.set, Set::Zxing | Set::Vendored))
        .filter(|row| row.expected.is_some())
        .filter(|row| !matches!(row.outcome, Outcome::Exact | Outcome::NegativeHeld))
        .collect();
    println!("\nlabelled rows not exact: {}", misses.len());
    for row in misses {
        let label = row.disposition.map_or_else(String::new, |(kind, state)| {
            let verdict = row.verdict.map_or("—", Outcome::as_str);
            format!(
                " [{} {} · verdict {verdict}]",
                kind.as_str(),
                state.as_str()
            )
        });
        println!("  {:<14} {}{label}", row.outcome.as_str(), row.path);
        match &row.scan {
            Scan::Failed { code, detail } => println!("{:17}{code}: {detail}", ""),
            Scan::Report { detections, .. } if !detections.is_empty() => {
                for unit in row.expected.iter().flatten() {
                    let symbology = unit
                        .symbology
                        .map_or_else(|| "any".to_owned(), |s| wire(&s));
                    println!("{:17}expected {symbology} {}", "", preview(&unit.text));
                }
                for d in detections {
                    let engines: Vec<String> = d.engines.iter().map(wire).collect();
                    println!(
                        "{:17}observed {} {} ({})",
                        "",
                        wire(&d.symbology),
                        preview(&d.text),
                        engines.join(", ")
                    );
                }
            }
            Scan::Report { .. } => {}
        }
    }
}

// -------------------------------------------------------------------- run

/// Parsed `oracle` flags.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    json: Option<PathBuf>,
    corpus_root: Option<PathBuf>,
}

fn parse_args(args: Vec<String>) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--json" => &mut parsed.json,
            "--corpus-root" => &mut parsed.corpus_root,
            _ => return Err(format!("unknown argument {flag:?}")),
        };
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    Ok(parsed)
}

/// A committed input the oracle cannot run without — exit 2 when absent.
fn read_committed(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap_or_else(|e| {
        eprintln!("oracle: {name} unreadable at the repo root ({e})");
        std::process::exit(2);
    })
}

fn emit_receipt(path: Option<&Path>, receipt: &Receipt) {
    let Some(path) = path else { return };
    if let Err(e) = write_receipt(path, receipt) {
        eprintln!("oracle: cannot write the receipt: {e}");
        std::process::exit(2);
    }
    println!("receipt: {}", path.display());
}

/// The receipt as known before any scan — identity, inputs, integrity
/// denominators — and `skipped` until [`fill_receipt`] records a run.
fn new_receipt(manifest_text: &str, vendored_text: &str, integrity: Integrity) -> Receipt {
    Receipt {
        oracle: RULES_ID,
        status: "skipped",
        exit_code: 3,
        reason: Some("no external corpus at the default root"),
        versions: Versions::current(),
        scan: SCAN_ECHO,
        inputs: Inputs {
            manifest: FileEcho {
                path: external::MANIFEST,
                sha256: external::sha256_bytes(manifest_text.as_bytes()),
            },
            vendored: FileEcho {
                path: VENDORED,
                sha256: external::sha256_bytes(vendored_text.as_bytes()),
            },
            dispositions_sha256: dispositions_sha256(),
        },
        integrity,
        sets: Vec::new(),
        groups: Vec::new(),
        dispositions: Vec::new(),
        blockers: Vec::new(),
        rows: Vec::new(),
    }
}

/// The corpus directory to measure, or the verdict line of a run that has
/// none, with the receipt settled to match: the absent default is a skip
/// (exit 3, never a pass); an absent configured root is a failure (exit 1)
/// — a mistyped override must not read as a skip either.
fn measurable(root: CorpusRoot, receipt: &mut Receipt) -> Result<PathBuf, String> {
    match root {
        CorpusRoot::Present(dir) => Ok(dir),
        CorpusRoot::DefaultAbsent(dir) => Err(format!(
            "oracle: SKIPPED (not a pass) — no external corpus at {}",
            dir.display()
        )),
        CorpusRoot::OverrideAbsent { dir, source } => {
            receipt.status = "fail";
            receipt.exit_code = 1;
            receipt.reason = Some("the configured external corpus root does not exist");
            receipt.blockers = vec![format!(
                "integrity — the external corpus root set by {} does not exist",
                source.as_str()
            )];
            Err(format!(
                "oracle: FAIL (exit 1) — {}",
                external::override_absent(&dir, source)
            ))
        }
    }
}

fn fill_receipt(
    receipt: &mut Receipt,
    rows: &[Judged],
    (groups, sets): (Groups, BTreeMap<Set, Tally>),
    checks: &[DispositionCheck],
    blockers: Vec<String>,
) {
    receipt.exit_code = i32::from(!blockers.is_empty());
    receipt.status = if blockers.is_empty() { "pass" } else { "fail" };
    receipt.reason = None;
    receipt.blockers = blockers;
    receipt.sets = sets
        .into_iter()
        .map(|(set, tally)| SetReceipt {
            set: set.as_str(),
            tally,
            exact_wilson95: wilson95(tally.exact(), tally.truth),
        })
        .collect();
    receipt.groups = groups
        .into_iter()
        .map(|((set, group), tally)| GroupReceipt {
            set: set.as_str(),
            group,
            tally,
        })
        .collect();
    receipt.dispositions = checks
        .iter()
        .map(|check| DispositionReceipt {
            path: check.disposition.path,
            kind: check.disposition.kind.as_str(),
            pinned_verdict: check.disposition.verdict,
            observed_verdict: check.observed,
            state: check.state.as_str(),
        })
        .collect();
    receipt.rows = rows.iter().map(row_receipt).collect();
}

fn print_verdict(blockers: &[String]) {
    if blockers.is_empty() {
        println!(
            "\noracle: PASS (exit 0) — no wrong-class outcome, error, contract or integrity \
             failure; dispositions current"
        );
        return;
    }
    println!("\noracle: FAIL (exit 1) — {} blocker(s):", blockers.len());
    for blocker in blockers {
        println!("  {blocker}");
    }
}

/// `xtask oracle [--json PATH] [--corpus-root ABS]`.
pub(crate) fn run(args: Vec<String>) {
    let args = parse_args(args).unwrap_or_else(|e| {
        eprintln!("oracle: {e}\n{USAGE}");
        std::process::exit(2);
    });
    let root = crate::repo_root();
    let manifest_text = read_committed(&root, external::MANIFEST);
    let pinned = external::parse_manifest(&manifest_text).unwrap_or_else(|e| {
        eprintln!("oracle: {}: {e}", external::MANIFEST);
        std::process::exit(2);
    });
    let vendored_text = read_committed(&root, VENDORED);
    let corpus: crate::Corpus = toml::from_str(&vendored_text).unwrap_or_else(|e| {
        eprintln!("oracle: {VENDORED}: {e}");
        std::process::exit(2);
    });
    let integrity = Integrity {
        manifest_rows: pinned.len(),
        vendored_entries: corpus.entry.len(),
        ..Integrity::default()
    };
    let mut receipt = new_receipt(&manifest_text, &vendored_text, integrity);

    let corpus_root = external::corpus_root(args.corpus_root.as_deref());
    let dir = match measurable(corpus_root, &mut receipt) {
        Ok(dir) => dir,
        Err(verdict) => {
            println!(
                "{verdict}; {} manifest rows and {} vendored entries NOT evaluated",
                pinned.len(),
                corpus.entry.len()
            );
            emit_receipt(args.json.as_deref(), &receipt);
            std::process::exit(receipt.exit_code);
        }
    };
    let mut integrity = std::mem::take(&mut receipt.integrity);

    let scanner = external::scanner();
    let mut rows = evaluate_external(&dir, &pinned, &scanner, &mut integrity);
    let checks = apply_dispositions(&mut rows);
    rows.extend(evaluate_vendored(&root, &corpus, &scanner, &mut integrity));
    let blockers = blockers(&integrity, &rows, &checks);

    let versions = Versions::current();
    println!(
        "oracle {RULES_ID} · scanner {} (pipeline {}) · Full, budget-free, scoring off · \
         {} external images + {} vendored entries",
        versions.scanner,
        versions.pipeline,
        rows.iter().filter(|r| r.set != Set::Vendored).count(),
        corpus.entry.len()
    );
    let (groups, sets) = tallies(&rows);
    print_groups(&groups);
    print_sets(&sets, &integrity);
    print_dispositions(&checks);
    print_labelled_misses(&rows);

    receipt.integrity = integrity;
    fill_receipt(&mut receipt, &rows, (groups, sets), &checks, blockers);
    emit_receipt(args.json.as_deref(), &receipt);
    print_verdict(&receipt.blockers);
    std::process::exit(receipt.exit_code);
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::RootSource;
    use qrcode_ai_scanner::Symbology::{Ean13, MicroQrCode, QrCode};

    fn qr(text: &str) -> Unit {
        Unit {
            symbology: Some(QrCode),
            text: text.to_owned(),
        }
    }

    fn report(observed: &[&str]) -> Scan {
        Scan::Report {
            detections: observed
                .iter()
                .map(|text| Seen {
                    symbology: QrCode,
                    text: (*text).to_owned(),
                    engines: vec![EngineKind::Rqrr],
                })
                .collect(),
            engine_panics: 0,
        }
    }

    /// A zxing-shaped row for `disposition`'s path and pinned hashes.
    fn zxing_row(disposition: &Disposition, truth: &str, observed: &[&str]) -> Judged {
        let expected = vec![qr(truth)];
        let scan = report(observed);
        let (verdict, outcome, contract) = verdict_of(Some(expected.as_slice()), &scan);
        Judged {
            set: Set::Zxing,
            group: external::group_of(disposition.path),
            path: disposition.path.to_owned(),
            image_sha256: disposition.image_sha256.to_owned(),
            pin: None,
            expected: Some(expected),
            truth_sha256: Some(disposition.truth_sha256.to_owned()),
            verdict,
            outcome,
            disposition: None,
            scan,
            contract,
        }
    }

    fn collect_keys(value: &serde_json::Value, keys: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, inner) in map {
                    keys.push(key.clone());
                    collect_keys(inner, keys);
                }
            }
            serde_json::Value::Array(items) => {
                for inner in items {
                    collect_keys(inner, keys);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn one_expected_unit_exact_wrong_missed() {
        let expected = [qr("E")];
        assert_eq!(judge(&expected, &[(QrCode, "E")]), Outcome::Exact);
        assert_eq!(judge(&expected, &[(QrCode, "W")]), Outcome::Wrong);
        assert_eq!(judge(&expected, &[]), Outcome::Missed);
    }

    /// The `any()` witness: the pin rule calls `[E, W]` a `match`; the
    /// oracle never calls it exact, in either order.
    #[test]
    fn an_extra_detection_is_never_exact() {
        let expected = [qr("E")];
        for observed in [
            [(QrCode, "E"), (QrCode, "W")],
            [(QrCode, "W"), (QrCode, "E")],
        ] {
            assert!(
                observed.iter().any(|(_, text)| *text == "E"),
                "any() says match"
            );
            assert_eq!(judge(&expected, &observed), Outcome::Extra);
        }
    }

    #[test]
    fn symbology_is_part_of_the_unit() {
        assert_eq!(judge(&[qr("E")], &[(MicroQrCode, "E")]), Outcome::Wrong);
        // corpus.toml truth is text-only: any symbology carries it
        let text_only = [Unit {
            symbology: None,
            text: "E".to_owned(),
        }];
        assert_eq!(judge(&text_only, &[(MicroQrCode, "E")]), Outcome::Exact);
    }

    #[test]
    fn two_expected_units() {
        let expected = [qr("A"), qr("B")];
        assert_eq!(judge(&expected, &[(QrCode, "A")]), Outcome::Partial);
        assert_eq!(
            judge(&expected, &[(QrCode, "A"), (QrCode, "W")]),
            Outcome::Mixed
        );
        assert_eq!(
            judge(&expected, &[(QrCode, "B"), (QrCode, "A")]),
            Outcome::Exact
        );
        assert_eq!(
            judge(&expected, &[(QrCode, "A"), (QrCode, "B"), (QrCode, "W")]),
            Outcome::Extra
        );
        assert_eq!(judge(&expected, &[(QrCode, "W")]), Outcome::Wrong);
    }

    #[test]
    fn negative_samples() {
        assert_eq!(judge(&[], &[]), Outcome::NegativeHeld);
        assert_eq!(judge(&[], &[(QrCode, "W")]), Outcome::FalsePositive);
    }

    #[test]
    fn outcome_vocabulary_and_blocking_class_are_pinned() {
        let names: Vec<&str> = Outcome::ALL.into_iter().map(Outcome::as_str).collect();
        assert_eq!(
            names,
            [
                "exact",
                "extra",
                "partial",
                "mixed",
                "wrong",
                "missed",
                "negative_held",
                "false_positive",
                "ambiguous",
                "unlabelled_decoded",
                "unlabelled_blind",
                "error"
            ]
        );
        for (index, outcome) in Outcome::ALL.into_iter().enumerate() {
            assert_eq!(
                outcome as usize, index,
                "Counts indexes by declaration order"
            );
        }
        let blocking: Vec<&str> = Outcome::ALL
            .into_iter()
            .filter(|o| o.is_wrong_class())
            .map(Outcome::as_str)
            .collect();
        assert_eq!(blocking, ["extra", "mixed", "wrong", "false_positive"]);
    }

    #[test]
    fn contract_checks_flag_cap_order_and_duplicates() {
        let texts: Vec<String> = (0..17).map(|i| format!("payload-{i}")).collect();
        let seventeen: Vec<(Symbology, &str)> =
            texts.iter().map(|text| (QrCode, text.as_str())).collect();
        let found = contract_violations(&seventeen);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("17 detection groups"), "{found:?}");
        assert!(
            contract_violations(&seventeen[..16]).is_empty(),
            "16 is the cap, not a violation"
        );

        let found = contract_violations(&[(Ean13, "1"), (QrCode, "2")]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("QR-family detection #1"), "{found:?}");
        assert!(contract_violations(&[(QrCode, "2"), (MicroQrCode, "3"), (Ean13, "1")]).is_empty());

        let found = contract_violations(&[(QrCode, "A"), (QrCode, "A")]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("repeats group #0"), "{found:?}");
        // an EAN-13 and a QR carrying the same digits are two groups
        assert!(contract_violations(&[(QrCode, "1"), (Ean13, "1")]).is_empty());
        // messages carry indices, never payload text
        assert!(found.iter().all(|m| !m.contains("\"A\"")));
    }

    #[test]
    fn dispositions_match_the_committed_manifest() {
        let path = crate::repo_root().join(external::MANIFEST);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let rows = external::parse_manifest(&text).expect("manifest parses");
        let pin = |p: &str| -> Row {
            rows.iter()
                .find(|row| row.path == p)
                .cloned()
                .unwrap_or_else(|| panic!("{p} not in the manifest"))
        };
        for d in &DISPOSITIONS {
            assert_eq!(
                pin(d.path).sha256,
                d.image_sha256,
                "{}: image sha256",
                d.path
            );
            let truth = pin(&truth_path(d.path));
            assert_eq!(truth.status, Status::Aux, "{}: truth row", d.path);
            assert_eq!(truth.sha256, d.truth_sha256, "{}: truth sha256", d.path);
        }
        // the any()-based pins the two dispositions correct
        assert_eq!(pin("zxing-blackbox/qrcode-2/13.png").status, Status::Wrong);
        assert_eq!(pin("zxing-blackbox/qrcode-2/16.png").status, Status::Match);
    }

    #[test]
    fn dispositions_hold_or_go_stale() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        let (image, truth) = (known_wrong.image_sha256, known_wrong.truth_sha256);
        assert_eq!(
            settle(Outcome::Wrong, &known_wrong, image, truth),
            (Outcome::Wrong, DispositionState::Holds)
        );
        // fixed (exact) or honestly refused (missed): stale, never silent
        for now in [Outcome::Exact, Outcome::Missed] {
            assert_eq!(
                settle(now, &known_wrong, image, truth),
                (now, DispositionState::Stale)
            );
        }
        // a re-shot image or an amended truth voids the pin
        let other = "0".repeat(64);
        assert_eq!(
            settle(Outcome::Wrong, &known_wrong, image, &other),
            (Outcome::Wrong, DispositionState::Void)
        );
        assert_eq!(
            settle(Outcome::Wrong, &known_wrong, &other, truth),
            (Outcome::Wrong, DispositionState::Void)
        );
        let (image, truth) = (double_qr.image_sha256, double_qr.truth_sha256);
        assert_eq!(
            settle(Outcome::Extra, &double_qr, image, truth),
            (Outcome::Ambiguous, DispositionState::Holds)
        );
        assert_eq!(
            settle(Outcome::Exact, &double_qr, image, truth),
            (Outcome::Exact, DispositionState::Stale)
        );
    }

    /// End to end on real-shaped rows: `known_wrong` stays wrong AND
    /// blocks; `truth_incomplete` turns a raw `extra` into a non-blocking
    /// `ambiguous`.
    #[test]
    fn holding_dispositions_settle_rows_and_known_wrong_blocks() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        let mut rows = vec![
            zxing_row(&known_wrong, "photograph", &["photography"]),
            zxing_row(&double_qr, "outer", &["inner", "outer"]),
        ];
        let checks = apply_dispositions(&mut rows);
        assert!(checks.iter().all(|c| c.state == DispositionState::Holds));
        assert_eq!(
            (rows[0].verdict, rows[0].outcome),
            (Some(Outcome::Wrong), Outcome::Wrong)
        );
        assert_eq!(
            (rows[1].verdict, rows[1].outcome),
            (Some(Outcome::Extra), Outcome::Ambiguous)
        );
        let found = blockers(&Integrity::default(), &rows, &checks);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("known_wrong") && found[0].contains(known_wrong.path),
            "{found:?}"
        );
    }

    #[test]
    fn a_changed_verdict_or_a_missing_row_blocks_as_stale() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        let mut rows = vec![zxing_row(&known_wrong, "photograph", &["photograph"])];
        let checks = apply_dispositions(&mut rows);
        assert_eq!(
            rows[0].outcome,
            Outcome::Exact,
            "the honest verdict is kept"
        );
        assert_eq!(checks[0].state, DispositionState::Stale);
        assert_eq!(checks[1].state, DispositionState::Unevaluated);
        let found = blockers(&Integrity::default(), &rows, &checks);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found[0].starts_with("stale disposition [known_wrong]"),
            "{found:?}"
        );
        assert!(found[1].contains(double_qr.path), "{found:?}");
    }

    #[test]
    fn errors_and_contract_violations_block() {
        let [known_wrong, _] = DISPOSITIONS;
        let mut errored = zxing_row(&known_wrong, "x", &[]);
        errored.scan = Scan::Failed {
            code: "QRS-001",
            detail: String::from("corrupt"),
        };
        let (verdict, outcome, contract) = verdict_of(errored.expected.as_deref(), &errored.scan);
        assert_eq!(
            (verdict, outcome, contract.len()),
            (None, Outcome::Error, 0)
        );
        (errored.verdict, errored.outcome) = (verdict, outcome);
        let duplicated = zxing_row(&known_wrong, "x", &["x", "x"]);
        assert_eq!(
            duplicated.outcome,
            Outcome::Exact,
            "duplicates are a contract matter"
        );
        let found = blockers(&Integrity::default(), &[errored, duplicated], &[]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].starts_with("error (QRS-001)"), "{found:?}");
        assert!(found[1].starts_with("contract"), "{found:?}");
    }

    #[test]
    fn receipt_rows_carry_hashes_never_text() {
        const PAYLOAD: &str = "https://example.invalid/private-payload-7f3a";
        let [known_wrong, _] = DISPOSITIONS;
        let mut gallery = zxing_row(&known_wrong, PAYLOAD, &[PAYLOAD, "second"]);
        gallery.set = Set::Gallery;
        gallery.expected = None;
        let labelled = zxing_row(&known_wrong, PAYLOAD, &[PAYLOAD]);
        let json = serde_json::to_string(&[row_receipt(&gallery), row_receipt(&labelled)])
            .expect("receipt rows serialise");
        assert!(!json.contains(PAYLOAD), "payload text leaked: {json}");
        assert!(!json.contains("second"), "payload text leaked: {json}");
        let (sha256, len) = text_hash(PAYLOAD);
        assert!(json.contains(&sha256) && json.contains(&format!("\"text_len\":{len}")));
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        let mut keys = Vec::new();
        collect_keys(&value, &mut keys);
        assert!(!keys.iter().any(|key| key == "text"), "{keys:?}");
        assert!(keys.iter().any(|key| key == "text_sha256"), "{keys:?}");
    }

    #[test]
    fn wilson_interval_matches_reference_values() {
        let interval = |low: &str, high: &str| Some([low.to_owned(), high.to_owned()]);
        assert_eq!(wilson95(169, 179), interval("0.900", "0.969"));
        assert_eq!(wilson95(34, 34), interval("0.898", "1.000"));
        assert_eq!(wilson95(0, 8), interval("0.000", "0.324"));
        assert_eq!(wilson95(0, 0), None);
    }

    #[test]
    fn vendored_entries_map_to_expected_sets() {
        let text = |t: &str| {
            Some(vec![Unit {
                symbology: None,
                text: t.to_owned(),
            }])
        };
        assert_eq!(vendored_expectation(Some("T"), None), text("T"));
        assert_eq!(
            vendored_expectation(None, None),
            Some(Vec::new()),
            "negative"
        );
        assert_eq!(vendored_expectation(None, Some("pass")), Some(Vec::new()));
        assert_eq!(vendored_expectation(None, Some("fail")), None, "frontier");
        // a frontier pin WITH truth is judged against it
        assert_eq!(vendored_expectation(Some("T"), Some("fail")), text("T"));
    }

    #[test]
    fn truth_files_and_suite_symbology() {
        assert_eq!(
            truth_path("zxing-blackbox/qrcode-2/13.png"),
            "zxing-blackbox/qrcode-2/13.txt"
        );
        assert_eq!(
            suite_symbology("zxing-blackbox/qrcode-6/1.png"),
            Some(QrCode)
        );
        assert_eq!(suite_symbology("zxing-blackbox/aztec-1/1.png"), None);
    }

    /// No corpus at the default root is a skip (exit 3, never a pass); a
    /// configured root that does not exist is a failure (exit 1) whose
    /// receipt names the source but carries no path. No filesystem access.
    #[test]
    fn absent_roots_skip_only_at_the_default() {
        let dir = crate::repo_root().join("absent-corpus");
        let fresh = || new_receipt("manifest", "vendored", Integrity::default());

        let mut receipt = fresh();
        let verdict = measurable(CorpusRoot::DefaultAbsent(dir.clone()), &mut receipt)
            .expect_err("nothing to measure");
        assert!(
            verdict.starts_with("oracle: SKIPPED (not a pass)"),
            "{verdict}"
        );
        assert_eq!((receipt.status, receipt.exit_code), ("skipped", 3));

        for source in [RootSource::Env, RootSource::Explicit] {
            let mut receipt = fresh();
            let root = CorpusRoot::OverrideAbsent {
                dir: dir.clone(),
                source,
            };
            let verdict = measurable(root, &mut receipt).expect_err("nothing to measure");
            assert!(
                verdict.starts_with("oracle: FAIL (exit 1)") && verdict.contains(source.as_str()),
                "{verdict}"
            );
            assert_eq!((receipt.status, receipt.exit_code), ("fail", 1));
            assert_eq!(receipt.blockers.len(), 1, "{:?}", receipt.blockers);
            let json = serde_json::to_string(&receipt).expect("receipt serialises");
            assert!(!json.contains("absent-corpus"), "a path leaked: {json}");
        }

        let mut receipt = fresh();
        assert_eq!(
            measurable(CorpusRoot::Present(dir.clone()), &mut receipt),
            Ok(dir)
        );
        assert_eq!(
            receipt.status, "skipped",
            "the run, not the root, settles it"
        );
    }

    #[test]
    fn oracle_flags() {
        let parse = |args: &[&str]| parse_args(args.iter().map(|a| (*a).to_owned()).collect());
        assert_eq!(parse(&[]), Ok(Args::default()));
        assert_eq!(
            parse(&["--json", "r.json", "--corpus-root", "/corpus"]),
            Ok(Args {
                json: Some(PathBuf::from("r.json")),
                corpus_root: Some(PathBuf::from("/corpus")),
            })
        );
        assert!(parse(&["--json"]).is_err(), "missing value");
        assert!(
            parse(&["--json", "a", "--json", "b"]).is_err(),
            "repeated flag"
        );
        assert!(parse(&["--external"]).is_err(), "unknown flag");
    }
}
