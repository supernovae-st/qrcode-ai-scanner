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
//! `corpus.toml` truth is text only, so its unit accepts any symbology —
//! but, like every unit, it is satisfied by ONE observed group: the same
//! text under a second symbology is extra output. An entry without
//! `expected` is a negative sample unless it is a frontier pin
//! (`expect = "fail"`). Gallery and frontier rows have no truth: they are
//! `unlabelled_*` and never enter a rate denominator. Every report must
//! also meet the grouping contract of `spec/01-report.md`: at most 16
//! groups, QR family first, no duplicate (symbology, text).
//!
//! [`DISPOSITIONS`] holds the judgments a truth file cannot express,
//! pinned by image AND truth sha256 and by exactly what they excuse: a
//! `known_wrong` entry pins the misread's output signature, a
//! `truth_incomplete` entry pins the unnamed symbol it allows next to the
//! truth (any other output is still judged). An output the pin does not
//! describe makes the entry stale and fails the run, so the table is edited
//! deliberately. Expected values are never relabelled to match current
//! output.
//!
//! Exit codes (the family `external.rs` shares): 0 pass · 1 gate failure —
//! any `wrong`/`extra`/`mixed`/`false_positive` (a `known_wrong` disposition
//! still counts), any `error`, contract or integrity failure, or a stale
//! disposition · 2 usage or configuration error — bad flags, an unreadable
//! committed input, a relative or absent configured corpus root
//! (`--corpus-root`, `QRSCAN_EXTERNAL_CORPUS`), an unwritable receipt · 3
//! skipped — no corpus at the default root: `SKIPPED (not a pass)`.
//! `missed` and `partial` lower the exact rate without blocking.
//!
//! Scans are budget-free with scoring off ([`external::scanner`]), so no
//! verdict depends on host load. The `--json` receipt names the scanned
//! tree (git SHA and clean/dirty, read at run time), holds no timing and no
//! absolute path, and payloads only as text sha256 + byte length +
//! engines: two runs over the same inputs are byte-identical.
//!
//! `--observed <jsonl> --decoder <name>` judges another decoder's recorded
//! outputs on the same hash-verified images, with the same judge,
//! dispositions and tallies. The file holds one metadata line
//! (`{"decoder", "version", "settings"}`) and one record per image
//! (`{"path", "sha256", "outputs": [{"symbology", "text"}], "error"}`).
//! Outputs are grouped by (symbology, text) before judging, the 16-group cap
//! and QR-first order (our wire shape) are recorded as information, and
//! dispositions apply their outcome rule unchanged — an output that is not
//! the pinned qrcode-ai-scanner reading marks the entry `differs`
//! (information) instead of stale. `--emit-observed <jsonl>` writes our own
//! scans in that shape (decoded text included: keep it local).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use qrcode_ai_scanner::{EngineKind, ImageInput, Scanner, Symbology, Versions};
use rayon::prelude::*;
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Serialize, Serializer};

use crate::external::{self, CorpusRoot, Row, Status};

/// Judge-rules identifier carried by every receipt — bump it with any rule
/// change so receipts made under different rules never compare silently.
/// v1.1: dispositions pin the output they excuse, a text-only unit consumes
/// one observed group, and one exit-code family (0 · 1 · 2 · 3) with the
/// scanned tree in every receipt.
const RULES_ID: &str = "qrscan-oracle/v1.1";
/// The vendored manifest, at the repo root.
const VENDORED: &str = "corpus.toml";
/// Detection groups per report (`spec/01-report.md`, anti-amplification).
const MAX_GROUPS: usize = 16;
/// z of a two-sided 95% interval.
const WILSON_Z95: f64 = 1.959_963_984_540_054;
const USAGE: &str = "usage: xtask oracle [--json <path>] [--corpus-root <absolute path>] \
                     [--observed <jsonl> --decoder <name>] [--emit-observed <jsonl>]";

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
/// unit is found AND nothing else came back. Matching is one-to-one: a
/// unit consumes at most one observed group, so a text-only unit met by
/// the same text under two symbologies leaves one group over — `extra`.
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
    // Symbology-pinned units choose first: a text-only unit can take any
    // group a pinned one leaves, so this greedy order finds a maximum
    // matching (units only compete within one text).
    let pinned_first = expected
        .iter()
        .filter(|e| e.symbology.is_some())
        .chain(expected.iter().filter(|e| e.symbology.is_none()));
    let mut consumed = vec![false; observed.len()];
    let mut found = 0;
    for unit in pinned_first {
        let free = (0..observed.len()).find(|&i| !consumed[i] && hit(unit, &observed[i]));
        if let Some(i) = free {
            consumed[i] = true;
            found += 1;
        }
    }
    let extra = consumed.contains(&false);
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
    let mut found = wire_shape_violations(observed);
    found.extend(duplicate_groups(observed));
    found
}

/// The two rules that shape OUR wire report: at most 16 groups, QR family
/// first. A comparator never promised them, so in observed mode they are
/// recorded as information instead of judged.
fn wire_shape_violations(observed: &[(Symbology, &str)]) -> Vec<String> {
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
    found
}

/// The grouping rule every decoder is held to: one group per
/// (symbology, text) content unit.
fn duplicate_groups(observed: &[(Symbology, &str)]) -> Vec<String> {
    let mut found = Vec::new();
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

/// What a disposition says about its row — and exactly what it excuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispositionKind {
    /// A confident decode the truth contradicts: stays `wrong` and blocks
    /// until fixed or honestly refused. `signature` pins the misread
    /// ([`observed_signature`] at pin time): any other output — even
    /// another wrong text — makes the entry stale.
    KnownWrong { signature: &'static str },
    /// The truth names fewer symbols than the image holds. `excused` pins
    /// the unnamed symbol as (symbology, text sha256): it may appear once
    /// beside the truth, anything else is judged as usual (so a new wrong
    /// output is still `extra`). Reading only truth and excused units is
    /// `ambiguous` — never `exact`, never `wrong` — until the truth is
    /// amended with independent confirmation.
    TruthIncomplete { excused: (Symbology, &'static str) },
}

impl DispositionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::KnownWrong { .. } => "known_wrong",
            Self::TruthIncomplete { .. } => "truth_incomplete",
        }
    }

    /// The pin in canonical text, for the table hash.
    fn pin(self) -> String {
        match self {
            Self::KnownWrong { signature } => format!("signature={signature}"),
            Self::TruthIncomplete {
                excused: (symbology, text_sha256),
            } => format!("excused={}:{text_sha256}", wire(&symbology)),
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
    /// The qrcode-ai-scanner judge verdict this entry was written against.
    pub(crate) verdict: Outcome,
    /// Why — public corpus facts only.
    pub(crate) note: &'static str,
}

/// Pinned dispositions; hashes copied from the committed manifest, pins
/// from the accepted receipt of the base scan.
pub(crate) const DISPOSITIONS: [Disposition; 2] = [
    Disposition {
        path: "zxing-blackbox/qrcode-2/13.png",
        image_sha256: "a8d498d6d2d6a3e27e24fd75ed023cd5b8b6b42cb4996674e88e555d68855ec1",
        truth_sha256: "c19d2dcb13469aae263a41cd73add68c45e086223a04c41368128c47143c0e5a",
        // [(qr_code, sha256 of the 72-byte "…photography…" misread)]
        kind: DispositionKind::KnownWrong {
            signature: "1aae7f6958dc301435f86f4de43c5e9dc4a62f6885d6f83dbdab39b35d96cb55",
        },
        verdict: Outcome::Wrong,
        note: "one letter off: the decode reads \"photography\" where the truth reads \"photograph\"",
    },
    Disposition {
        path: "zxing-blackbox/qrcode-2/16.png",
        image_sha256: "24fef8babbb2862f2f37fe05b74b45daaa87f1b0c5d7610c0f40eb63becfe9e1",
        truth_sha256: "c93aaefb0b894232954b026da265394fedffd76a1ac251704ff83095b3b183be",
        // the inner symbol: its 57-byte "[内側QRコード]…" payload
        kind: DispositionKind::TruthIncomplete {
            excused: (
                Symbology::QrCode,
                "dd992cf99bdb39989fb2b58d9d6ed0ae26c5b6a891982402985a4fcad1c8ee21",
            ),
        },
        verdict: Outcome::Extra,
        note: "double QR (one symbol nested in another): the truth names the outer one only",
    },
];

/// How a disposition met its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispositionState {
    /// Hashes and pin as written — the disposition's outcome applies.
    Holds,
    /// Our own scan: hashes as pinned, but the output is not what the pin
    /// excuses — edit the table deliberately (blocks).
    Stale,
    /// Observed decoder: its output is not the pinned qrcode-ai-scanner
    /// reading. Information only — the outcome rule still applies.
    Differs,
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
            Self::Differs => "differs",
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

/// sha256 over the ordered `symbology<TAB>text_sha256` lines of a report —
/// the output a `known_wrong` disposition pins.
fn observed_signature(observed: &[(Symbology, &str)]) -> String {
    let mut lines = String::new();
    for (symbology, text) in observed {
        writeln!(
            lines,
            "{}\t{}",
            wire(symbology),
            external::sha256_bytes(text.as_bytes())
        )
        .expect("write to string");
    }
    external::sha256_bytes(lines.as_bytes())
}

/// A truth-incomplete row judged against its truth plus the excused unit,
/// which may stand in for one observed group. Truth and excused units
/// only → `ambiguous`; nothing at all → `missed`; any other output keeps
/// its strict verdict (`extra`, `mixed`, `wrong`).
fn excused_outcome(
    expected: &[Unit],
    observed: &[(Symbology, &str)],
    (symbology, text_sha256): (Symbology, &str),
) -> Outcome {
    if observed.is_empty() {
        return Outcome::Missed;
    }
    let mut rest = observed.to_vec();
    if let Some(at) = rest
        .iter()
        .position(|(s, t)| *s == symbology && external::sha256_bytes(t.as_bytes()) == text_sha256)
    {
        rest.remove(at);
    }
    match judge(expected, &rest) {
        Outcome::Exact | Outcome::Missed => Outcome::Ambiguous,
        other => other,
    }
}

/// Final outcome and state of a judged row (raw `verdict`) under its
/// disposition — the same outcome rule for every decoder. `gate` is our
/// own scan, for which an output the pin does not excuse makes the table
/// stale; any other decoder's output merely `differs` from the pinned
/// reading.
fn settle(
    row: &Judged,
    verdict: Outcome,
    disposition: &Disposition,
    gate: bool,
) -> (Outcome, DispositionState) {
    let truth_sha256 = row.truth_sha256.as_deref().unwrap_or_default();
    if disposition.image_sha256 != row.image_sha256 || disposition.truth_sha256 != truth_sha256 {
        return (verdict, DispositionState::Void);
    }
    let observed = units_of(&row.scan);
    let pinned_verdict = verdict == disposition.verdict;
    let (outcome, holds) = match disposition.kind {
        DispositionKind::KnownWrong { signature } => (
            verdict,
            pinned_verdict && observed_signature(&observed) == signature,
        ),
        DispositionKind::TruthIncomplete { excused } => {
            let expected = row.expected.as_deref().unwrap_or_default();
            let outcome = excused_outcome(expected, &observed, excused);
            (outcome, pinned_verdict && outcome == Outcome::Ambiguous)
        }
    };
    let state = if holds {
        DispositionState::Holds
    } else if gate {
        DispositionState::Stale
    } else {
        DispositionState::Differs
    };
    (outcome, state)
}

/// sha256 of the table's canonical text, pins included — a receipt names
/// the exact table its verdicts were settled under.
fn dispositions_sha256() -> String {
    let mut canon = String::new();
    for d in &DISPOSITIONS {
        writeln!(
            canon,
            "{}\t{}\t{}\t{}\t{}\t{}",
            d.path,
            d.image_sha256,
            d.truth_sha256,
            d.kind.as_str(),
            d.verdict.as_str(),
            d.kind.pin()
        )
        .expect("write to string");
    }
    external::sha256_bytes(canon.as_bytes())
}

/// Settle every disposition against its judged row (in place); `gate` as
/// in [`settle`].
fn apply_dispositions(rows: &mut [Judged], gate: bool) -> Vec<DispositionCheck> {
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
            let (outcome, state) = settle(row, verdict, &disposition, gate);
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
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    symbology: Symbology,
    text: String,
    engines: Vec<EngineKind>,
}

/// What one scan produced.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scan {
    Report {
        detections: Vec<Seen>,
        engine_panics: u8,
    },
    /// `code` is the `QRS-xxx` wire code, `panic`, or `decoder_error` for
    /// an observed decoder's reported failure.
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
    /// Blocking grouping-contract violations.
    contract: Vec<String>,
    /// Wire-shape notes (cap, order) for an observed decoder — information.
    wire_info: Vec<String>,
    /// Observed outputs folded into an earlier identical group.
    merged: usize,
}

/// The judgment of one scan before dispositions.
#[derive(Debug, PartialEq, Eq)]
struct Verdict {
    /// Raw judge verdict (labelled rows).
    raw: Option<Outcome>,
    outcome: Outcome,
    /// Blocking grouping-contract violations.
    contract: Vec<String>,
    /// Wire-shape violations recorded as information only.
    wire_info: Vec<String>,
}

/// The (symbology, text) groups of a scan, in report order — none for a
/// failed scan.
fn units_of(scan: &Scan) -> Vec<(Symbology, &str)> {
    match scan {
        Scan::Report { detections, .. } => detections
            .iter()
            .map(|d| (d.symbology, d.text.as_str()))
            .collect(),
        Scan::Failed { .. } => Vec::new(),
    }
}

/// Judge one scan. `wire_binds` is our own scanner, which owes the whole
/// grouping contract; an observed decoder owes the grouping rule but not
/// our wire shape (cap, order), which then lands in `wire_info`.
fn verdict_of(expected: Option<&[Unit]>, scan: &Scan, wire_binds: bool) -> Verdict {
    if matches!(scan, Scan::Failed { .. }) {
        return Verdict {
            raw: None,
            outcome: Outcome::Error,
            contract: Vec::new(),
            wire_info: Vec::new(),
        };
    }
    let observed = units_of(scan);
    let (contract, wire_info) = if wire_binds {
        (contract_violations(&observed), Vec::new())
    } else {
        (
            duplicate_groups(&observed),
            wire_shape_violations(&observed),
        )
    };
    let (raw, outcome) = match expected {
        Some(units) => {
            let verdict = judge(units, &observed);
            (Some(verdict), verdict)
        }
        None if observed.is_empty() => (None, Outcome::UnlabelledBlind),
        None => (None, Outcome::UnlabelledDecoded),
    };
    Verdict {
        raw,
        outcome,
        contract,
        wire_info,
    }
}

// ------------------------------------------------------------- observed

/// Name the emitted and observed files use for our own scanner.
const OUR_DECODER: &str = "qrcode-ai-scanner";

/// A decoder's metadata line in an `--observed` file.
#[derive(Debug, Deserialize)]
struct DecoderMeta {
    decoder: String,
    version: String,
    #[serde(default)]
    settings: serde_json::Value,
}

/// One image record of an `--observed` file. Unknown fields are ignored —
/// `raw_b64` and runner flags ride along for the record; the judge's unit
/// is (symbology, text).
#[derive(Debug, Deserialize)]
struct ObservedLine {
    path: String,
    sha256: String,
    #[serde(default)]
    outputs: Vec<ObservedOutput>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ObservedOutput {
    symbology: Symbology,
    text: String,
}

/// One observed image, grouped.
#[derive(Debug)]
struct ObservedImage {
    sha256: String,
    scan: Scan,
    /// Outputs folded into an earlier identical (symbology, text) group.
    merged: usize,
}

/// A decoder's parsed observations.
#[derive(Debug)]
struct Observed {
    meta: DecoderMeta,
    records: BTreeMap<String, ObservedImage>,
}

/// Group outputs by (symbology, text), first occurrence first: comparator
/// outputs are content units, so a payload reported twice is one unit.
fn group_outputs(outputs: Vec<ObservedOutput>) -> (Vec<Seen>, usize) {
    let mut groups: Vec<Seen> = Vec::with_capacity(outputs.len());
    let mut merged = 0;
    for output in outputs {
        if groups
            .iter()
            .any(|g| g.symbology == output.symbology && g.text == output.text)
        {
            merged += 1;
        } else {
            groups.push(Seen {
                symbology: output.symbology,
                text: output.text,
                engines: Vec::new(),
            });
        }
    }
    (groups, merged)
}

/// Parse an `--observed` JSONL file written by `decoder`: exactly one
/// metadata line plus one record per image path. Error messages name line
/// numbers and paths, never payload text.
fn parse_observed(text: &str, decoder: &str) -> Result<Observed, String> {
    let mut meta: Option<DecoderMeta> = None;
    let mut records = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let n = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("line {n}: {e}"))?;
        if value.get("path").is_some() {
            let record: ObservedLine =
                serde_json::from_value(value).map_err(|e| format!("line {n}: {e}"))?;
            let (scan, merged) = if let Some(detail) = record.error {
                let failed = Scan::Failed {
                    code: "decoder_error",
                    detail,
                };
                (failed, 0)
            } else {
                let (detections, merged) = group_outputs(record.outputs);
                let scan = Scan::Report {
                    detections,
                    engine_panics: 0,
                };
                (scan, merged)
            };
            let image = ObservedImage {
                sha256: record.sha256,
                scan,
                merged,
            };
            if records.insert(record.path.clone(), image).is_some() {
                return Err(format!("line {n}: {} is observed twice", record.path));
            }
        } else {
            let line_meta: DecoderMeta = serde_json::from_value(value).map_err(|e| {
                format!("line {n}: neither an image record nor decoder metadata: {e}")
            })?;
            if meta.replace(line_meta).is_some() {
                return Err(format!("line {n}: a second decoder metadata line"));
            }
        }
    }
    let meta = meta.ok_or("no decoder metadata line (decoder, version)")?;
    if meta.decoder != decoder {
        return Err(format!(
            "--decoder {decoder:?} but the file was written by {:?}",
            meta.decoder
        ));
    }
    Ok(Observed { meta, records })
}

/// Where image observations come from.
#[derive(Clone, Copy)]
enum Source<'a> {
    /// Our scanner, run on the verified bytes.
    Scan(&'a Scanner),
    /// A decoder's recorded outputs.
    Observed(&'a Observed),
}

impl Source<'_> {
    /// Our own scanner owes the whole wire contract and gates the table.
    fn is_gate(self) -> bool {
        matches!(self, Self::Scan(_))
    }

    /// The observation of one verified image (and its merged-output count),
    /// or why there is none.
    fn observe(self, path: &str, sha256: &str, bytes: &[u8]) -> Result<(Scan, usize), String> {
        match self {
            Self::Scan(scanner) => Ok((scan_bytes(scanner, bytes), 0)),
            Self::Observed(observed) => {
                let record = observed
                    .records
                    .get(path)
                    .ok_or_else(|| format!("not observed by the decoder: {path}"))?;
                if record.sha256 != sha256 {
                    return Err(format!(
                        "the observed record hashed other bytes ({}): {path}",
                        record.sha256
                    ));
                }
                Ok((record.scan.clone(), record.merged))
            }
        }
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
    /// Hash-verified image, its observation and merged-output count.
    Image(Scan, usize),
    /// Hash-verified image the source has no usable observation for.
    Unobserved(String),
}

fn probe(dir: &Path, row: &Row, source: Source<'_>) -> Probe {
    let bytes = match std::fs::read(dir.join(&row.path)) {
        Ok(bytes) => bytes,
        Err(e) => return Probe::Unreadable(e.to_string()),
    };
    if external::sha256_bytes(&bytes) != row.sha256 {
        return Probe::Drift;
    }
    if external::is_image(&row.path) {
        return match source.observe(&row.path, &row.sha256, &bytes) {
            Ok((scan, merged)) => Probe::Image(scan, merged),
            Err(why) => Probe::Unobserved(why),
        };
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

/// Re-walk and re-hash the external corpus (`verify`'s integrity rules),
/// then judge every hash-verified image row from `source`.
fn evaluate_external(
    dir: &Path,
    pinned: &[Row],
    source: Source<'_>,
    integrity: &mut Integrity,
) -> Vec<Judged> {
    let on_disk: BTreeSet<String> = match external::walk_sorted(dir) {
        Ok(paths) => paths.into_iter().collect(),
        Err(e) => {
            // an unwalkable corpus cannot be verified: a gate failure
            integrity.problems.push(format!("corpus walk failed: {e}"));
            return Vec::new();
        }
    };
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
        .map(|row| (row, probe(dir, row, source)))
        .collect();

    // path → (pinned sha256, text) of every hash-verified truth file
    let mut truths: BTreeMap<&str, (&str, String)> = BTreeMap::new();
    let mut scans: Vec<(&Row, Scan, usize)> = Vec::new();
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
            Probe::Image(scan, merged) => {
                integrity.verified += 1;
                scans.push((row, scan, merged));
            }
            Probe::Unobserved(why) => {
                integrity.verified += 1;
                integrity.problems.push(why);
            }
        }
    }
    let wire_binds = source.is_gate();
    scans
        .into_iter()
        .filter_map(|(row, scan, merged)| {
            let observation = (scan, merged, wire_binds);
            external_row(row, observation, &truths, &mut integrity.problems)
        })
        .collect()
}

/// Expected set + judgment of one hash-verified external image;
/// `observation` is (scan, merged outputs, wire contract binds).
fn external_row(
    row: &Row,
    (scan, merged, wire_binds): (Scan, usize, bool),
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
    let verdict = verdict_of(expected.as_deref(), &scan, wire_binds);
    Some(Judged {
        set,
        group: external::group_of(&row.path),
        path: row.path.clone(),
        image_sha256: row.sha256.clone(),
        pin: Some(row.status),
        expected,
        truth_sha256,
        verdict: verdict.raw,
        outcome: verdict.outcome,
        disposition: None,
        scan,
        contract: verdict.contract,
        wire_info: verdict.wire_info,
        merged,
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

/// A vendored fixture read once.
enum VendoredProbe {
    Unreadable(String),
    /// Read, but the source has no usable observation for it.
    Unobserved(String),
    Image {
        sha256: String,
        scan: Scan,
        merged: usize,
    },
}

/// Judge every `corpus.toml` entry (vendored, always present) from `source`.
fn evaluate_vendored(
    root: &Path,
    corpus: &crate::Corpus,
    source: Source<'_>,
    integrity: &mut Integrity,
) -> Vec<Judged> {
    let probes: Vec<(&crate::Entry, VendoredProbe)> = corpus
        .entry
        .par_iter()
        .map(|entry| {
            let probe = match std::fs::read(root.join(&entry.path)) {
                Err(e) => VendoredProbe::Unreadable(e.to_string()),
                Ok(bytes) => {
                    let sha256 = external::sha256_bytes(&bytes);
                    match source.observe(&entry.path, &sha256, &bytes) {
                        Ok((scan, merged)) => VendoredProbe::Image {
                            sha256,
                            scan,
                            merged,
                        },
                        Err(why) => VendoredProbe::Unobserved(why),
                    }
                }
            };
            (entry, probe)
        })
        .collect();
    let mut rows = Vec::with_capacity(probes.len());
    for (entry, probe) in probes {
        let (image_sha256, scan, merged) = match probe {
            VendoredProbe::Image {
                sha256,
                scan,
                merged,
            } => (sha256, scan, merged),
            VendoredProbe::Unreadable(e) => {
                integrity
                    .problems
                    .push(format!("vendored fixture unreadable: {}: {e}", entry.path));
                continue;
            }
            VendoredProbe::Unobserved(why) => {
                integrity.vendored_read += 1;
                integrity.problems.push(why);
                continue;
            }
        };
        integrity.vendored_read += 1;
        let expected = vendored_expectation(entry.expected.as_deref(), entry.expect.as_deref());
        let verdict = verdict_of(expected.as_deref(), &scan, source.is_gate());
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
            verdict: verdict.raw,
            outcome: verdict.outcome,
            disposition: None,
            scan,
            contract: verdict.contract,
            wire_info: verdict.wire_info,
            merged,
        });
    }
    rows
}

/// Observed records whose path is in neither manifest — integrity problems:
/// a runner that walked a different corpus must not pass unnoticed.
fn unknown_observed_paths(
    observed: &Observed,
    pinned: &[Row],
    corpus: &crate::Corpus,
) -> Vec<String> {
    let known: BTreeSet<&str> = pinned
        .iter()
        .map(|row| row.path.as_str())
        .chain(corpus.entry.iter().map(|entry| entry.path.as_str()))
        .collect();
    observed
        .records
        .keys()
        .filter(|path| !known.contains(path.as_str()))
        .map(|path| format!("observed record outside both manifests: {path}"))
        .collect()
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
                Some((DispositionKind::KnownWrong { .. }, DispositionState::Holds))
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
            (DispositionState::Holds | DispositionState::Differs, _) => continue,
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
    /// Observed decoder: wire-shape notes (cap, order) — information only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    wire_info: Vec<String>,
    /// Observed decoder: outputs folded into an earlier identical group.
    #[serde(skip_serializing_if = "Option::is_none")]
    merged_duplicates: Option<usize>,
}

/// The observed decoder a receipt judged, as its runner declared it.
#[derive(Serialize)]
struct DecoderEcho {
    name: String,
    version: String,
    settings: serde_json::Value,
    /// Image records in the observed file.
    records: usize,
}

/// The scanned tree, read from git at run time — strings, `unknown` when
/// git cannot tell (never fatal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SourceEcho {
    git_sha: String,
    /// `clean` · `dirty` (uncommitted or untracked changes) · `unknown`.
    tree: String,
}

fn git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn source_identity(repo: &Path) -> SourceEcho {
    let git_sha = git_stdout(repo, &["rev-parse", "HEAD"])
        .map(|sha| sha.trim().to_owned())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| String::from("unknown"));
    let tree = match git_stdout(repo, &["status", "--porcelain"]) {
        Some(status) if status.trim().is_empty() => "clean",
        Some(_) => "dirty",
        None => "unknown",
    };
    SourceEcho {
        git_sha,
        tree: tree.to_owned(),
    }
}

#[derive(Serialize)]
struct Receipt {
    oracle: &'static str,
    status: &'static str,
    exit_code: i32,
    reason: Option<&'static str>,
    /// The judging build (our scanner's versions, in both modes).
    versions: Versions,
    /// The scanned tree (this checkout), in both modes.
    source: SourceEcho,
    /// Our scan configuration; absent when the observations were recorded
    /// by another decoder.
    #[serde(skip_serializing_if = "Option::is_none")]
    scan: Option<ScanEcho>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decoder: Option<DecoderEcho>,
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
        wire_info: row.wire_info.clone(),
        merged_duplicates: (row.merged > 0).then_some(row.merged),
    }
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_receipt(path: &Path, receipt: &Receipt) -> Result<(), String> {
    let mut json = serde_json::to_string_pretty(receipt).map_err(|e| e.to_string())?;
    json.push('\n');
    write_text(path, &json)
}

#[derive(Serialize)]
struct EmittedMeta {
    decoder: &'static str,
    version: String,
    settings: ScanEcho,
}

#[derive(Serialize)]
struct EmittedOutput<'a> {
    symbology: Symbology,
    text: &'a str,
}

#[derive(Serialize)]
struct EmittedRecord<'a> {
    path: &'a str,
    sha256: &'a str,
    outputs: Vec<EmittedOutput<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Our own judged scans in the `--observed` JSONL shape, so they can be
/// re-judged through the comparator path. The file holds decoded TEXT —
/// private gallery payloads included — and must stay local.
fn observed_jsonl(rows: &[Judged]) -> Result<String, String> {
    let versions = Versions::current();
    let meta = EmittedMeta {
        decoder: OUR_DECODER,
        version: format!("{} (pipeline {})", versions.scanner, versions.pipeline),
        settings: SCAN_ECHO,
    };
    let mut out = serde_json::to_string(&meta).map_err(|e| e.to_string())?;
    out.push('\n');
    for row in rows {
        let (outputs, error) = match &row.scan {
            Scan::Report { detections, .. } => {
                let outputs = detections
                    .iter()
                    .map(|d| EmittedOutput {
                        symbology: d.symbology,
                        text: &d.text,
                    })
                    .collect();
                (outputs, None)
            }
            Scan::Failed { code, detail } => (Vec::new(), Some(format!("{code}: {detail}"))),
        };
        let record = EmittedRecord {
            path: &row.path,
            sha256: &row.image_sha256,
            outputs,
            error,
        };
        out.push_str(&serde_json::to_string(&record).map_err(|e| e.to_string())?);
        out.push('\n');
    }
    Ok(out)
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
    /// Judge a decoder's recorded outputs instead of scanning.
    observed: Option<PathBuf>,
    /// The decoder the observed file must declare.
    decoder: Option<String>,
    /// Also write our own scans in the observed shape.
    emit_observed: Option<PathBuf>,
}

const FLAGS: [&str; 5] = [
    "--json",
    "--corpus-root",
    "--observed",
    "--decoder",
    "--emit-observed",
];

fn parse_args(args: Vec<String>) -> Result<Args, String> {
    let mut values: BTreeMap<&'static str, String> = BTreeMap::new();
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let Some(key) = FLAGS.iter().copied().find(|known| *known == flag) else {
            return Err(format!("unknown argument {flag:?}"));
        };
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        if values.insert(key, value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let mut take = |key: &str| values.remove(key);
    let parsed = Args {
        json: take("--json").map(PathBuf::from),
        corpus_root: take("--corpus-root").map(PathBuf::from),
        observed: take("--observed").map(PathBuf::from),
        decoder: take("--decoder"),
        emit_observed: take("--emit-observed").map(PathBuf::from),
    };
    if parsed.observed.is_some() != parsed.decoder.is_some() {
        return Err("--observed and --decoder go together".to_owned());
    }
    if parsed.observed.is_some() && parsed.emit_observed.is_some() {
        return Err("--emit-observed records our own scan; drop --observed".to_owned());
    }
    Ok(parsed)
}

/// The observed file named by `--observed`, or exit 2.
fn load_observed(path: &Path, decoder: &str) -> Observed {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("oracle: {}: {e}", path.display());
        std::process::exit(2);
    });
    parse_observed(&text, decoder).unwrap_or_else(|e| {
        eprintln!("oracle: {}: {e}", path.display());
        std::process::exit(2);
    })
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
fn new_receipt(
    source: SourceEcho,
    manifest_text: &str,
    vendored_text: &str,
    integrity: Integrity,
) -> Receipt {
    Receipt {
        oracle: RULES_ID,
        status: "skipped",
        exit_code: 3,
        reason: Some("no external corpus at the default root"),
        versions: Versions::current(),
        source,
        scan: Some(SCAN_ECHO),
        decoder: None,
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
/// (exit 3, never a pass); an absent configured root is a configuration
/// error (exit 2) — a mistyped override must not read as a skip either.
fn measurable(root: CorpusRoot, receipt: &mut Receipt) -> Result<PathBuf, String> {
    match root {
        CorpusRoot::Present(dir) => Ok(dir),
        CorpusRoot::DefaultAbsent(dir) => Err(format!(
            "oracle: SKIPPED (not a pass) — no external corpus at {}",
            dir.display()
        )),
        CorpusRoot::OverrideAbsent { dir, source } => {
            receipt.status = "error";
            receipt.exit_code = 2;
            receipt.reason = Some("the configured external corpus root does not exist");
            receipt.blockers = vec![format!(
                "configuration — the external corpus root set by {} does not exist",
                source.as_str()
            )];
            Err(format!(
                "oracle: ERROR (exit 2) — {}",
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

/// Our scan, or the observed decoder, in one header line — with the tree
/// that judged it.
fn print_header(observed: Option<&Observed>, source: &SourceEcho, rows: &[Judged], entries: usize) {
    let external_images = rows.iter().filter(|r| r.set != Set::Vendored).count();
    let tree = format!("source {} ({})", source.git_sha, source.tree);
    match observed {
        None => {
            let versions = Versions::current();
            println!(
                "oracle {RULES_ID} · scanner {} (pipeline {}) · {tree} · Full, budget-free, \
                 scoring off · {external_images} external images + {entries} vendored entries",
                versions.scanner, versions.pipeline
            );
        }
        Some(observed) => println!(
            "oracle {RULES_ID} · {tree} · observed decoder {} {} · {} records · same judge, \
             dispositions and tallies (wire cap/order as information) · {external_images} \
             external images + {entries} vendored entries",
            observed.meta.decoder,
            observed.meta.version,
            observed.records.len()
        ),
    }
}

/// `xtask oracle [--json PATH] [--corpus-root ABS] [--observed JSONL
/// --decoder NAME] [--emit-observed JSONL]`.
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
    let observed = args
        .observed
        .as_deref()
        .map(|path| load_observed(path, args.decoder.as_deref().unwrap_or_default()));
    let integrity = Integrity {
        manifest_rows: pinned.len(),
        vendored_entries: corpus.entry.len(),
        ..Integrity::default()
    };
    let identity = source_identity(&root);
    let mut receipt = new_receipt(identity, &manifest_text, &vendored_text, integrity);
    if let Some(observed) = &observed {
        receipt.scan = None;
        receipt.decoder = Some(DecoderEcho {
            name: observed.meta.decoder.clone(),
            version: observed.meta.version.clone(),
            settings: observed.meta.settings.clone(),
            records: observed.records.len(),
        });
    }

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
    let source = observed
        .as_ref()
        .map_or(Source::Scan(&scanner), Source::Observed);
    let mut rows = evaluate_external(&dir, &pinned, source, &mut integrity);
    let checks = apply_dispositions(&mut rows, source.is_gate());
    rows.extend(evaluate_vendored(&root, &corpus, source, &mut integrity));
    if let Some(observed) = &observed {
        let unknown = unknown_observed_paths(observed, &pinned, &corpus);
        integrity.problems.extend(unknown);
    }
    let blockers = blockers(&integrity, &rows, &checks);

    print_header(
        observed.as_ref(),
        &receipt.source,
        &rows,
        corpus.entry.len(),
    );
    if let Some(path) = &args.emit_observed {
        let written = observed_jsonl(&rows).and_then(|text| write_text(path, &text));
        if let Err(e) = written {
            eprintln!("oracle: cannot write the observed file: {e}");
            std::process::exit(2);
        }
        println!("observed (decoded text — keep local): {}", path.display());
    }
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

    /// Public payloads of the two disposition rows (zxing blackbox suite
    /// `qrcode-2`): 13's truth and the misread it is pinned to, 16's outer
    /// truth and the unnamed inner symbol it excuses.
    const TRUTH_13: &str =
        "The 2005 USGS aerial photograph of the Washington Monument is censored.";
    const MISREAD_13: &str =
        "The 2005 USGS aerial photography of the Washington Monument is censored.";
    const OUTER_16: &str = "[\u{5916}\u{5074}QR\u{30b3}\u{30fc}\u{30c9}]\r\n \r\n\
                            *\u{ff80}\u{ff9e}\u{ff8c}\u{ff9e}\u{ff99}QR*\r\nhttp://d-qr.net/ex/";
    const INNER_16: &str = "[\u{5185}\u{5074}QR\u{30b3}\u{30fc}\u{30c9}]\r\n\r\n\
                            *\u{30c0}\u{30d6}\u{30eb}QR*\r\nhttp://d-qr.net/ex/";

    /// A zxing-shaped row for `disposition`'s path and pinned hashes, judged
    /// as our own scan.
    fn zxing_row(disposition: &Disposition, truth: &str, observed: &[&str]) -> Judged {
        let expected = vec![qr(truth)];
        let scan = report(observed);
        let verdict = verdict_of(Some(expected.as_slice()), &scan, true);
        Judged {
            set: Set::Zxing,
            group: external::group_of(disposition.path),
            path: disposition.path.to_owned(),
            image_sha256: disposition.image_sha256.to_owned(),
            pin: None,
            expected: Some(expected),
            truth_sha256: Some(disposition.truth_sha256.to_owned()),
            verdict: verdict.raw,
            outcome: verdict.outcome,
            disposition: None,
            scan,
            contract: verdict.contract,
            wire_info: verdict.wire_info,
            merged: 0,
        }
    }

    /// One synthetic `--observed` file: metadata plus the given records.
    fn observed_file(decoder: &str, records: &[String]) -> String {
        let mut text = format!(
            "{{\"decoder\": \"{decoder}\", \"version\": \"9.9.9\", \"settings\": {{\"mode\": \"test\"}}}}\n"
        );
        for record in records {
            text.push_str(record);
            text.push('\n');
        }
        text
    }

    fn record(path: &str, sha256: &str, outputs: &[(&str, &str)]) -> String {
        let outputs: Vec<serde_json::Value> = outputs
            .iter()
            .map(|(symbology, text)| serde_json::json!({"symbology": symbology, "text": text}))
            .collect();
        serde_json::json!({"path": path, "sha256": sha256, "outputs": outputs}).to_string()
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

    /// Every receipt names the rules it was judged under; v1 receipts
    /// (before pinned excuses and one-to-one text-only units) must never
    /// read as comparable to v1.1 ones.
    #[test]
    fn receipts_carry_the_rules_id() {
        let source = SourceEcho {
            git_sha: String::from("unknown"),
            tree: String::from("unknown"),
        };
        let receipt = new_receipt(source, "manifest", "vendored", Integrity::default());
        let json = serde_json::to_value(&receipt).expect("receipt serialises");
        assert_eq!(json["oracle"], "qrscan-oracle/v1.1");
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

    /// The pins are the public payloads' own hashes, nothing typed by hand.
    #[test]
    fn disposition_pins_match_the_public_payloads() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        assert_eq!(
            external::sha256_bytes(TRUTH_13.as_bytes()),
            known_wrong.truth_sha256
        );
        assert_eq!(
            external::sha256_bytes(OUTER_16.as_bytes()),
            double_qr.truth_sha256
        );
        let DispositionKind::KnownWrong { signature } = known_wrong.kind else {
            panic!("13 is known_wrong");
        };
        assert_eq!(observed_signature(&[(QrCode, MISREAD_13)]), signature);
        let DispositionKind::TruthIncomplete { excused } = double_qr.kind else {
            panic!("16 is truth_incomplete");
        };
        let inner = external::sha256_bytes(INNER_16.as_bytes());
        assert_eq!(excused, (QrCode, inner.as_str()));
        // the table hash covers the pins
        assert_ne!(dispositions_sha256(), external::sha256_bytes(b""));
    }

    #[test]
    fn dispositions_hold_or_go_stale() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        let settled =
            |d: &Disposition, row: &Judged| settle(row, row.verdict.expect("labelled"), d, true);
        assert_eq!(
            settled(
                &known_wrong,
                &zxing_row(&known_wrong, TRUTH_13, &[MISREAD_13])
            ),
            (Outcome::Wrong, DispositionState::Holds)
        );
        // fixed (exact) or honestly refused (missed): stale, never silent
        for observed in [&[TRUTH_13][..], &[]] {
            let row = zxing_row(&known_wrong, TRUTH_13, observed);
            assert_eq!(
                settled(&known_wrong, &row),
                (row.verdict.expect("labelled"), DispositionState::Stale)
            );
        }
        // a re-shot image or an amended truth voids the pin
        let mut redrawn = zxing_row(&known_wrong, TRUTH_13, &[MISREAD_13]);
        redrawn.image_sha256 = "0".repeat(64);
        assert_eq!(
            settled(&known_wrong, &redrawn),
            (Outcome::Wrong, DispositionState::Void)
        );
        let mut amended = zxing_row(&known_wrong, TRUTH_13, &[MISREAD_13]);
        amended.truth_sha256 = Some("0".repeat(64));
        assert_eq!(settled(&known_wrong, &amended).1, DispositionState::Void);
        // the truth plus the excused inner symbol: ambiguous, as pinned
        let both = zxing_row(&double_qr, OUTER_16, &[INNER_16, OUTER_16]);
        assert_eq!(
            settled(&double_qr, &both),
            (Outcome::Ambiguous, DispositionState::Holds)
        );
    }

    /// The review finding on 13: a DIFFERENT wrong text is not the misread
    /// the entry excuses — stale, even though the verdict class is the same.
    #[test]
    fn known_wrong_holds_only_for_the_pinned_misread() {
        let [known_wrong, _] = DISPOSITIONS;
        let other = "The 2005 USGS aerial photographs of the Washington Monument is censored.";
        let mut rows = vec![zxing_row(&known_wrong, TRUTH_13, &[other])];
        assert_eq!(rows[0].verdict, Some(Outcome::Wrong));
        let checks = apply_dispositions(&mut rows, true);
        assert_eq!(checks[0].state, DispositionState::Stale);
        assert_eq!(rows[0].outcome, Outcome::Wrong);
        let found = blockers(&Integrity::default(), &rows, &checks);
        assert!(
            found
                .iter()
                .any(|b| b.starts_with("stale disposition [known_wrong]")),
            "{found:?}"
        );
        assert!(
            !found.iter().any(|b| b.contains("[known_wrong:")),
            "a stale entry no longer labels the row: {found:?}"
        );
    }

    /// The same outcome rule for every decoder; only our own scan's
    /// unexcused output is a stale (blocking) table — another decoder's
    /// merely `differs` from the pinned qrcode-ai-scanner reading.
    #[test]
    fn dispositions_apply_identically_to_every_decoder() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        let decoder =
            |d: &Disposition, row: &Judged| settle(row, row.verdict.expect("labelled"), d, false);
        assert_eq!(
            decoder(
                &known_wrong,
                &zxing_row(&known_wrong, TRUTH_13, &[MISREAD_13])
            ),
            (Outcome::Wrong, DispositionState::Holds),
            "a decoder sharing the misread is counted wrong"
        );
        assert_eq!(
            decoder(
                &known_wrong,
                &zxing_row(&known_wrong, TRUTH_13, &[TRUTH_13])
            ),
            (Outcome::Exact, DispositionState::Differs),
            "a decoder reading the truth exactly is exact"
        );
        // the excused symbol is allowed for every decoder, in any order;
        // anything else is judged
        for (observed, outcome) in [
            (&[OUTER_16][..], Outcome::Ambiguous),
            (&[INNER_16], Outcome::Ambiguous),
            (&[OUTER_16, INNER_16], Outcome::Ambiguous),
            (&["garbage", OUTER_16], Outcome::Extra),
        ] {
            let row = zxing_row(&double_qr, OUTER_16, observed);
            assert_eq!(decoder(&double_qr, &row).0, outcome, "{observed:?}");
        }

        let mut rows = vec![
            zxing_row(&known_wrong, TRUTH_13, &[TRUTH_13]),
            zxing_row(&double_qr, OUTER_16, &[INNER_16]),
        ];
        let checks = apply_dispositions(&mut rows, false);
        let states: Vec<DispositionState> = checks.iter().map(|c| c.state).collect();
        assert_eq!(
            states,
            [DispositionState::Differs, DispositionState::Differs]
        );
        assert_eq!(
            (rows[0].outcome, rows[1].outcome),
            (Outcome::Exact, Outcome::Ambiguous)
        );
        let found = blockers(&Integrity::default(), &rows, &checks);
        assert!(found.is_empty(), "differs is information: {found:?}");
    }

    /// The review finding on 16: a wrong extra output next to the outer
    /// truth must not hide behind `truth_incomplete` — never `ambiguous`.
    #[test]
    fn truth_incomplete_excuses_only_its_pinned_symbol() {
        let [_, double_qr] = DISPOSITIONS;
        for observed in [
            &["garbage", OUTER_16][..],
            &[INNER_16, OUTER_16, "garbage"],
            &["garbage"],
        ] {
            let mut rows = vec![zxing_row(&double_qr, OUTER_16, observed)];
            let checks = apply_dispositions(&mut rows, true);
            let outcome = rows[0].outcome;
            assert!(outcome.is_wrong_class(), "{observed:?}: {outcome:?}");
            assert_eq!(checks[1].state, DispositionState::Stale, "{observed:?}");
            let found = blockers(&Integrity::default(), &rows, &checks);
            assert!(
                found
                    .iter()
                    .any(|b| b.starts_with(outcome.as_str()) && b.contains(double_qr.path)),
                "{found:?}"
            );
        }
        // the truth alone, or the excused symbol alone: still ambiguous (the
        // truth is incomplete) but not the pinned reading — stale
        for observed in [&[OUTER_16][..], &[INNER_16]] {
            let mut rows = vec![zxing_row(&double_qr, OUTER_16, observed)];
            let checks = apply_dispositions(&mut rows, true);
            assert_eq!(
                (rows[0].outcome, checks[1].state),
                (Outcome::Ambiguous, DispositionState::Stale),
                "{observed:?}"
            );
        }
    }

    /// End to end on real-shaped rows: `known_wrong` stays wrong AND
    /// blocks; `truth_incomplete` turns the pinned reading into a
    /// non-blocking `ambiguous`.
    #[test]
    fn holding_dispositions_settle_rows_and_known_wrong_blocks() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        let mut rows = vec![
            zxing_row(&known_wrong, TRUTH_13, &[MISREAD_13]),
            zxing_row(&double_qr, OUTER_16, &[INNER_16, OUTER_16]),
        ];
        let checks = apply_dispositions(&mut rows, true);
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
        let mut rows = vec![zxing_row(&known_wrong, TRUTH_13, &[TRUTH_13])];
        let checks = apply_dispositions(&mut rows, true);
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
        let verdict = verdict_of(errored.expected.as_deref(), &errored.scan, true);
        assert_eq!(
            (verdict.raw, verdict.outcome, verdict.contract.len()),
            (None, Outcome::Error, 0)
        );
        (errored.verdict, errored.outcome) = (verdict.raw, verdict.outcome);
        // one unit consumes one group: the duplicate is unconsumed output
        // AND a grouping-contract violation
        let duplicated = zxing_row(&known_wrong, "x", &["x", "x"]);
        assert_eq!(duplicated.outcome, Outcome::Extra);
        let found = blockers(&Integrity::default(), &[errored, duplicated], &[]);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("error (QRS-001)"), "{found:?}");
        assert!(found[1].starts_with("extra"), "{found:?}");
        assert!(found[2].starts_with("contract"), "{found:?}");
    }

    /// A text-only unit is ONE content unit: the same text under a second
    /// symbology is extra output, never exact.
    #[test]
    fn a_text_only_unit_consumes_one_group() {
        let text_only = [Unit {
            symbology: None,
            text: "E".to_owned(),
        }];
        assert_eq!(
            judge(&text_only, &[(QrCode, "E"), (MicroQrCode, "E")]),
            Outcome::Extra
        );
        assert_eq!(judge(&text_only, &[(MicroQrCode, "E")]), Outcome::Exact);
        // pinned units choose first: a mixed set still matches exactly in
        // either order
        let mixed = [text_only[0].clone(), qr("E")];
        assert_eq!(
            judge(&mixed, &[(QrCode, "E"), (MicroQrCode, "E")]),
            Outcome::Exact
        );
        assert_eq!(
            judge(&mixed, &[(MicroQrCode, "E"), (QrCode, "E")]),
            Outcome::Exact
        );
        assert_eq!(judge(&mixed, &[(MicroQrCode, "E")]), Outcome::Partial);
    }

    #[test]
    fn source_identity_is_read_from_git_or_unknown() {
        let here = source_identity(&crate::repo_root());
        let hex = |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit());
        assert!(here.git_sha == "unknown" || hex(&here.git_sha), "{here:?}");
        assert!(
            ["clean", "dirty", "unknown"].contains(&here.tree.as_str()),
            "{here:?}"
        );
        let nowhere = std::env::temp_dir().join(format!("qrscan-no-repo-{}", std::process::id()));
        assert_eq!(
            source_identity(&nowhere),
            SourceEcho {
                git_sha: String::from("unknown"),
                tree: String::from("unknown"),
            },
            "git failing is recorded, never fatal"
        );
    }

    /// A throwaway corpus root under the system temp dir, removed on drop —
    /// the real corpus stays out of unit tests.
    struct TempCorpus(PathBuf);

    impl TempCorpus {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("qrscan-oracle-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp corpus");
            Self(dir)
        }

        /// Write `bytes` at `rel`; the manifest row pinning them.
        fn put(&self, rel: &str, bytes: &[u8], status: Status) -> Row {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(&path, bytes).expect("write");
            Row {
                status,
                sha256: external::sha256_bytes(bytes),
                path: rel.to_owned(),
            }
        }
    }

    impl Drop for TempCorpus {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn png(image: image::GrayImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageLuma8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png");
        bytes
    }

    fn qr_png(text: &str) -> Vec<u8> {
        let code = qrcode::QrCode::new(text.as_bytes()).expect("encodes");
        png(code
            .render::<image::Luma<u8>>()
            .module_dimensions(4, 4)
            .build())
    }

    fn blank_png() -> Vec<u8> {
        png(image::GrayImage::from_pixel(64, 64, image::Luma([255])))
    }

    /// A generated corpus through probe → `external_row` → judge: hashes
    /// verified, truth read from its manifested `.txt`, every set judged.
    #[test]
    fn a_synthetic_corpus_is_verified_and_judged_end_to_end() {
        let corpus = TempCorpus::new("e2e");
        let truth = "https://example.invalid/synthetic-truth";
        let rows = vec![
            corpus.put("qrcode-ai/wild/a.png", &qr_png("gallery"), Status::Decode),
            corpus.put(
                "zxing-blackbox/qrcode-9/1.png",
                &qr_png(truth),
                Status::Match,
            ),
            corpus.put(
                "zxing-blackbox/qrcode-9/1.txt",
                truth.as_bytes(),
                Status::Aux,
            ),
            corpus.put("zxing-blackbox/qrcode-9/2.png", &blank_png(), Status::Blind),
            corpus.put(
                "zxing-blackbox/qrcode-9/2.txt",
                b"never decoded",
                Status::Aux,
            ),
        ];
        let scanner = external::scanner();
        let mut integrity = Integrity::default();
        let judged = evaluate_external(&corpus.0, &rows, Source::Scan(&scanner), &mut integrity);
        assert!(integrity.problems.is_empty(), "{:?}", integrity.problems);
        assert_eq!(integrity.verified, rows.len());
        let outcomes: Vec<(&str, Outcome)> = judged
            .iter()
            .map(|row| (row.path.as_str(), row.outcome))
            .collect();
        assert_eq!(
            outcomes,
            [
                ("qrcode-ai/wild/a.png", Outcome::UnlabelledDecoded),
                ("zxing-blackbox/qrcode-9/1.png", Outcome::Exact),
                ("zxing-blackbox/qrcode-9/2.png", Outcome::Missed),
            ]
        );
        let truth_sha256 = external::sha256_bytes(truth.as_bytes());
        assert_eq!(
            judged[1].truth_sha256.as_deref(),
            Some(truth_sha256.as_str())
        );
        assert_eq!(judged[1].pin, Some(Status::Match));
    }

    /// A generated corpus judged from a decoder's recorded outputs: the
    /// same judge applies, and a record that hashed other bytes or is
    /// missing is an integrity problem, never judged.
    #[test]
    fn observed_outputs_are_judged_on_a_synthetic_corpus() {
        let corpus = TempCorpus::new("observed");
        let truth = "https://example.invalid/synthetic-truth";
        let rows = vec![
            corpus.put(
                "zxing-blackbox/qrcode-9/1.png",
                &qr_png(truth),
                Status::Match,
            ),
            corpus.put(
                "zxing-blackbox/qrcode-9/1.txt",
                truth.as_bytes(),
                Status::Aux,
            ),
            corpus.put("zxing-blackbox/qrcode-9/2.png", &blank_png(), Status::Blind),
            corpus.put("zxing-blackbox/qrcode-9/2.txt", b"two", Status::Aux),
            corpus.put("zxing-blackbox/qrcode-9/3.png", &blank_png(), Status::Blind),
            corpus.put("zxing-blackbox/qrcode-9/3.txt", b"three", Status::Aux),
        ];
        let text = observed_file(
            "probe",
            &[
                record(
                    &rows[0].path,
                    &rows[0].sha256,
                    &[("qr_code", truth), ("micro_qr_code", truth)],
                ),
                record(&rows[2].path, &"0".repeat(64), &[]),
            ],
        );
        let observed = parse_observed(&text, "probe").expect("parses");
        let mut integrity = Integrity::default();
        let source = Source::Observed(&observed);
        let judged = evaluate_external(&corpus.0, &rows, source, &mut integrity);
        assert_eq!(integrity.verified, rows.len(), "the files are intact");
        let outcomes: Vec<(&str, Outcome)> = judged
            .iter()
            .map(|row| (row.path.as_str(), row.outcome))
            .collect();
        assert_eq!(
            outcomes,
            [("zxing-blackbox/qrcode-9/1.png", Outcome::Extra)],
            "the truth under a second symbology is extra output"
        );
        let problems = integrity.problems.join("\n");
        for needle in [
            "the observed record hashed other bytes",
            "not observed by the decoder: zxing-blackbox/qrcode-9/3.png",
        ] {
            assert!(problems.contains(needle), "{problems}");
        }
    }

    #[test]
    fn records_outside_both_manifests_are_problems() {
        let text = observed_file(
            "probe",
            &[
                record("zxing-blackbox/qrcode-9/1.png", "aa", &[]),
                record("elsewhere/x.png", "bb", &[]),
            ],
        );
        let observed = parse_observed(&text, "probe").expect("parses");
        let pinned = vec![Row {
            status: Status::Blind,
            sha256: "a".repeat(64),
            path: String::from("zxing-blackbox/qrcode-9/1.png"),
        }];
        let corpus = crate::Corpus { entry: Vec::new() };
        assert_eq!(
            unknown_observed_paths(&observed, &pinned, &corpus),
            ["observed record outside both manifests: elsewhere/x.png"]
        );
    }

    #[test]
    fn integrity_flags_drift_missing_and_unmanifested_files() {
        let corpus = TempCorpus::new("integrity");
        let good = corpus.put("qrcode-ai/wild/a.png", &blank_png(), Status::Blind);
        let mut drifted = corpus.put("qrcode-ai/wild/b.png", &blank_png(), Status::Blind);
        drifted.sha256 = "0".repeat(64);
        corpus.put("qrcode-ai/wild/stray.png", &blank_png(), Status::Blind);
        let missing = Row {
            status: Status::Blind,
            sha256: "1".repeat(64),
            path: String::from("qrcode-ai/wild/gone.png"),
        };
        let rows = vec![good, drifted, missing];
        let scanner = external::scanner();
        let mut integrity = Integrity::default();
        let judged = evaluate_external(&corpus.0, &rows, Source::Scan(&scanner), &mut integrity);
        assert_eq!(judged.len(), 1, "only the verified image is judged");
        assert_eq!(integrity.verified, 1);
        let problems = integrity.problems.join("\n");
        for needle in [
            "unmanifested file on disk (regenerate the manifest): qrcode-ai/wild/stray.png",
            "manifested file missing on disk: qrcode-ai/wild/gone.png",
            "sha256 drift (corpus file changed — regenerate or restore): qrcode-ai/wild/b.png",
        ] {
            assert!(problems.contains(needle), "{problems}");
        }
        let found = blockers(&integrity, &judged, &[]);
        assert_eq!(found.len(), 3, "integrity problems block: {found:?}");
    }

    #[test]
    fn a_labelled_image_without_verified_truth_is_not_judged() {
        let corpus = TempCorpus::new("no-truth");
        let rows = vec![corpus.put("zxing-blackbox/qrcode-9/1.png", &blank_png(), Status::Blind)];
        let scanner = external::scanner();
        let mut integrity = Integrity::default();
        let judged = evaluate_external(&corpus.0, &rows, Source::Scan(&scanner), &mut integrity);
        assert!(judged.is_empty());
        assert_eq!(
            integrity.problems,
            [
                "labelled image without a verified zxing-blackbox/qrcode-9/1.txt or a pinned \
                 symbology: zxing-blackbox/qrcode-9/1.png"
            ]
        );
    }

    #[test]
    fn an_unwalkable_corpus_is_an_integrity_failure() {
        let absent =
            std::env::temp_dir().join(format!("qrscan-oracle-gone-{}", std::process::id()));
        let scanner = external::scanner();
        let mut integrity = Integrity::default();
        let judged = evaluate_external(&absent, &[], Source::Scan(&scanner), &mut integrity);
        assert!(judged.is_empty());
        assert_eq!(integrity.problems.len(), 1);
        assert!(
            integrity.problems[0].starts_with("corpus walk failed:"),
            "{:?}",
            integrity.problems
        );
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
    /// configured root that does not exist is a configuration error
    /// (exit 2) whose receipt names the source but carries no path. No
    /// filesystem access.
    #[test]
    fn absent_roots_skip_only_at_the_default() {
        let dir = crate::repo_root().join("absent-corpus");
        let source = SourceEcho {
            git_sha: String::from("unknown"),
            tree: String::from("unknown"),
        };
        let fresh = || new_receipt(source.clone(), "manifest", "vendored", Integrity::default());

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
                verdict.starts_with("oracle: ERROR (exit 2)") && verdict.contains(source.as_str()),
                "{verdict}"
            );
            assert_eq!((receipt.status, receipt.exit_code), ("error", 2));
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
                ..Args::default()
            })
        );
        assert_eq!(
            parse(&["--observed", "o.jsonl", "--decoder", "zxing-cpp"]),
            Ok(Args {
                observed: Some(PathBuf::from("o.jsonl")),
                decoder: Some(String::from("zxing-cpp")),
                ..Args::default()
            })
        );
        assert!(parse(&["--json"]).is_err(), "missing value");
        assert!(
            parse(&["--json", "a", "--json", "b"]).is_err(),
            "repeated flag"
        );
        assert!(parse(&["--external"]).is_err(), "unknown flag");
        assert!(parse(&["--observed", "o.jsonl"]).is_err(), "no --decoder");
        assert!(parse(&["--decoder", "zbar"]).is_err(), "no --observed");
        assert!(
            parse(&["--observed", "o", "--decoder", "d", "--emit-observed", "e"]).is_err(),
            "emit is for our own scan"
        );
    }

    #[test]
    fn observed_files_parse_and_group_by_content_unit() {
        let text = observed_file(
            "zxing-cpp",
            &[
                record("zxing-blackbox/qrcode-1/1.png", "aa", &[("qr_code", "A")]),
                String::new(),
                // one payload reported twice is one content unit
                record(
                    "qrcode-ai/wild/x.webp",
                    "bb",
                    &[("qr_code", "B"), ("micro_qr_code", "B"), ("qr_code", "B")],
                ),
                r#"{"path": "fixtures/clean/c.png", "sha256": "cc", "outputs": [], "error": "boom", "flag": 1}"#
                    .to_owned(),
            ],
        );
        let observed = parse_observed(&text, "zxing-cpp").expect("parses");
        assert_eq!(
            (
                observed.meta.decoder.as_str(),
                observed.meta.version.as_str()
            ),
            ("zxing-cpp", "9.9.9")
        );
        assert_eq!(observed.records.len(), 3);
        let grouped = &observed.records["qrcode-ai/wild/x.webp"];
        assert_eq!(grouped.merged, 1);
        let Scan::Report { detections, .. } = &grouped.scan else {
            panic!("a report");
        };
        let units: Vec<(Symbology, &str)> = detections
            .iter()
            .map(|d| (d.symbology, d.text.as_str()))
            .collect();
        assert_eq!(units, [(QrCode, "B"), (MicroQrCode, "B")]);
        assert!(matches!(
            &observed.records["fixtures/clean/c.png"].scan,
            Scan::Failed { code: "decoder_error", detail } if detail == "boom"
        ));
    }

    #[test]
    fn malformed_observed_files_are_refused() {
        let ok = record("a.png", "aa", &[]);
        let bad_symbology = record("a.png", "aa", &[("qr_kode", "A")]);
        for (text, why) in [
            (format!("{ok}\n"), "no metadata line"),
            (
                observed_file("zbar", std::slice::from_ref(&ok)),
                "decoder name mismatch",
            ),
            (
                observed_file("zxing-cpp", &[ok.clone(), ok.clone()]),
                "a path observed twice",
            ),
            (
                observed_file("zxing-cpp", &[bad_symbology]),
                "unknown symbology",
            ),
            (
                observed_file("zxing-cpp", &[String::from("{oops")]),
                "not JSON",
            ),
            (
                format!(
                    "{}{}",
                    observed_file("zxing-cpp", &[]),
                    observed_file("zxing-cpp", &[])
                ),
                "two metadata lines",
            ),
        ] {
            assert!(parse_observed(&text, "zxing-cpp").is_err(), "{why}");
        }
    }

    /// An observation is used only for the bytes the manifest pins.
    #[test]
    fn observed_records_must_match_the_verified_bytes() {
        let text = observed_file(
            "zbar",
            &[record(
                "zxing-blackbox/qrcode-1/1.png",
                "aa",
                &[("qr_code", "A")],
            )],
        );
        let observed = parse_observed(&text, "zbar").expect("parses");
        let source = Source::Observed(&observed);
        assert!(!source.is_gate());
        let (scan, merged) = source
            .observe("zxing-blackbox/qrcode-1/1.png", "aa", b"")
            .expect("observed");
        assert_eq!(merged, 0);
        assert!(matches!(scan, Scan::Report { ref detections, .. } if detections.len() == 1));
        assert!(
            source
                .observe("zxing-blackbox/qrcode-1/1.png", "bb", b"")
                .is_err(),
            "hashed other bytes"
        );
        assert!(
            source
                .observe("zxing-blackbox/qrcode-1/2.png", "aa", b"")
                .is_err(),
            "not observed"
        );
    }

    /// Cap and order are OUR wire contract: information for an observed
    /// decoder, blocking for our own scan. Duplicates bind everyone.
    #[test]
    fn wire_shape_is_information_for_observed_decoders() {
        let texts: Vec<String> = (0..17).map(|i| format!("p{i}")).collect();
        let mut detections: Vec<Seen> = texts
            .iter()
            .map(|text| Seen {
                symbology: QrCode,
                text: text.clone(),
                engines: Vec::new(),
            })
            .collect();
        detections.insert(
            0,
            Seen {
                symbology: Ean13,
                text: String::from("1"),
                engines: Vec::new(),
            },
        );
        let scan = Scan::Report {
            detections,
            engine_panics: 0,
        };
        let observed = verdict_of(None, &scan, false);
        assert!(observed.contract.is_empty(), "{:?}", observed.contract);
        assert_eq!(observed.wire_info.len(), 2, "{:?}", observed.wire_info);
        let ours = verdict_of(None, &scan, true);
        assert!(ours.wire_info.is_empty());
        let units: Vec<(Symbology, &str)> = std::iter::once((Ean13, "1"))
            .chain(texts.iter().map(|t| (QrCode, t.as_str())))
            .collect();
        assert_eq!(
            ours.contract,
            contract_violations(&units),
            "our scan keeps the full contract, in the usual order"
        );
        let duplicated = report(&["x", "x"]);
        assert_eq!(verdict_of(None, &duplicated, false).contract.len(), 1);
    }

    /// Our own judged scans, emitted and parsed back, give the same
    /// content units and verdicts — the self-consistency the comparator
    /// path rests on. The emitted receipt fields stay hash-only.
    #[test]
    fn emitted_observations_rejudge_identically() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        let mut errored = zxing_row(&known_wrong, "x", &[]);
        errored.path = String::from("fixtures/clean/broken.png");
        errored.scan = Scan::Failed {
            code: "QRS-001",
            detail: String::from("corrupt"),
        };
        let rows = vec![
            zxing_row(&known_wrong, "photograph", &["photography"]),
            zxing_row(&double_qr, "outer", &["inner", "outer"]),
            errored,
        ];
        let text = observed_jsonl(&rows).expect("emits");
        assert_eq!(text.lines().count(), 1 + rows.len());
        let observed = parse_observed(&text, OUR_DECODER).expect("parses back");
        for row in &rows[..2] {
            let image = &observed.records[&row.path];
            assert_eq!(image.sha256, row.image_sha256);
            let again = verdict_of(row.expected.as_deref(), &image.scan, false);
            assert_eq!(again.raw, row.verdict, "{}", row.path);
            let (Scan::Report { detections: a, .. }, Scan::Report { detections: b, .. }) =
                (&image.scan, &row.scan)
            else {
                panic!("reports");
            };
            let units = |d: &[Seen]| -> Vec<(Symbology, String)> {
                d.iter().map(|s| (s.symbology, s.text.clone())).collect()
            };
            assert_eq!(units(a), units(b));
        }
        assert!(matches!(
            &observed.records["fixtures/clean/broken.png"].scan,
            Scan::Failed { code: "decoder_error", detail } if detail == "QRS-001: corrupt"
        ));
        // merged counts surface in the receipt only when non-zero
        let mut merged_row = zxing_row(&known_wrong, "a", &["a"]);
        merged_row.merged = 2;
        let json = serde_json::to_string(&[row_receipt(&merged_row), row_receipt(&rows[0])])
            .expect("serialises");
        assert_eq!(json.matches("merged_duplicates").count(), 1, "{json}");
        assert!(!json.contains("photography"), "{json}");
    }
}
