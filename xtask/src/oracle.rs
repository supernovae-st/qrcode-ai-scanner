//! Strict content oracle — the complete detection list of every image
//! judged against its complete expected set.
//!
//! The external pins (`corpus-report --external`) are a tripwire: `match`
//! passes when SOME detection's text equals the truth, so `[expected,
//! wrong]` pins `match`, a wrong→wrong row stays green and gallery pins
//! prove detection only. This instrument gives each image one strict
//! [`Outcome`] instead.
//!
//! A truth is a set of units, each met by at most ONE detection. A unit
//! whose bytes are known independently of the scanner under test is
//! structural (`corpus.toml` `raw_hex`, with `eci` and `fnc1` when the
//! symbol declares them) and is judged by content identity
//! (`spec/01-report.md` § Comparing contents): a detection with the same
//! raw, FNC1 position, located ECI list and text meets it `exact`; one with
//! the same raw and FNC1 position whose declarations were signalled but
//! not located, or whose charset is `unknown` without contradicting the
//! declarations, meets it `unread`; nothing else meets it — equal text over
//! other bytes is another content. A unit that knows only its text (a
//! zxing `.txt`: the exact file bytes, UTF-8 with an ISO-8859-1 fallback,
//! never trimmed; a `corpus.toml` entry without `raw_hex`) is met `exact`
//! by equal text and `unread` by a detection whose charset is `unknown`. A
//! detection that carries no bytes (an observed decoder that reports text
//! only) is held to the text rule. `unread` has its own rate and never
//! blocks. A zxing unit names `qr_code`; `corpus.toml` has no symbology
//! field, so its units accept any. An entry without `expected` is a
//! negative sample unless it is a frontier pin (`expect = "fail"`).
//! Gallery and frontier rows have no truth: they are `unlabelled_*` and
//! never enter a rate denominator. Every report must also meet the
//! grouping contract of `spec/01-report.md`: at most 16 detections, QR
//! family first, no two detections with one identity.
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
//! `missed`, `partial` and `unread` lower the exact rate without blocking.
//!
//! Scans are budget-free with scoring off ([`external::scanner`]), so no
//! verdict depends on host load. The `--json` receipt names the scanned
//! tree (git SHA and clean/dirty, read at run time) and the outcome rule
//! of each truth kind, holds no timing and no absolute path, and payloads
//! only as text and raw sha256 + byte length: two runs over the same inputs
//! are byte-identical. The tree state leaves out the files the run itself
//! writes (`--json`, `--emit-observed`), and an unwalkable corpus is named
//! as `<corpus root>`, so both claims hold wherever those files go.
//!
//! `--observed <jsonl> --decoder <name>` judges another decoder's recorded
//! outputs on the same hash-verified images, with the same judge,
//! dispositions and tallies. The file holds one metadata line
//! (`{"decoder", "version", "settings"}`) and one record per image
//! (`{"path", "sha256", "outputs": [{"symbology", "text"}], "error"}`); an
//! output that also carries `raw_hex` (with an optional wire `charset`,
//! `eci` and `fnc1`) is judged by identity, one without it by the text
//! rule. Identical outputs are grouped before judging, the 16-detection cap
//! and QR-first order (our wire shape) are recorded as information, and
//! dispositions apply their outcome rule unchanged — an output that is not
//! the pinned qrcode-ai-scanner reading marks the entry `differs`
//! (information) instead of stale. `--emit-observed <jsonl>` writes our own
//! scans in that shape (decoded text and bytes included: keep it local).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use qrcode_ai_scanner::{
    Charset, Detection, EngineKind, Fnc1Position, ImageInput, Scanner, Symbology, Versions,
};
use rayon::prelude::*;
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Serialize, Serializer};

use crate::external::{self, CorpusRoot, Row, Status};
use crate::truth_amendments::{self, Amendment};

/// Judge-rules identifier carried by every receipt — bump it with any rule
/// change so receipts made under different rules never compare silently.
/// v1.1: dispositions pin the output they excuse, a text-only unit consumes
/// one observed group, and one exit-code family (0 · 1 · 2 · 3) with the
/// scanned tree in every receipt. v2: a truth that knows the bytes is
/// judged by content identity, a text truth by text unless the detection's
/// charset is `unknown`; `unread` is an outcome with its own rate; the
/// grouping contract forbids two detections of one identity; the receipt
/// states the rule of each truth kind ([`RULES`]). v2.1: a validated text
/// amendment supersedes historical disposition truth without excusing output.
const RULES_ID: &str = "qrscan-oracle/v2.1";
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
    /// Every expected unit met exactly, nothing else.
    Exact,
    /// Every expected unit met, plus output nobody expected.
    Extra,
    /// Some expected units met, no unexpected output.
    Partial,
    /// Some expected units met, plus unexpected output.
    Mixed,
    /// Output, none of it meeting a unit.
    Wrong,
    /// Every expected unit met, at least one by a detection without a
    /// verified text reading (`unknown`, or declarations signalled and not
    /// located), nothing else — its own rate, never blocking.
    Unread,
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
    const ALL: [Self; 13] = [
        Self::Exact,
        Self::Extra,
        Self::Partial,
        Self::Mixed,
        Self::Wrong,
        Self::Unread,
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
            Self::Unread => "unread",
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

/// One located ECI designator, as the report lists it (`spec/01-report.md`
/// § `eci`): the assignment number and the byte offset into `raw` where it
/// takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Eci {
    pub(crate) designator: u32,
    pub(crate) start: u32,
}

/// What a structural truth knows about the symbol, independently of the
/// scanner under test: its bytes and its declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) raw: Vec<u8>,
    /// The designators the symbol declares, located; `None` = none.
    pub(crate) eci: Option<Vec<Eci>>,
    pub(crate) fnc1: Option<Fnc1Position>,
}

/// One expected content unit as every truth names it: symbology and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    /// `None` accepts any symbology (`corpus.toml` has no symbology field).
    pub(crate) symbology: Option<Symbology>,
    /// Exact expected text — never trimmed or normalised.
    pub(crate) text: String,
}

/// One expected unit as the judge holds it: the unit, and its identity
/// when the symbol's bytes are known — a structural unit, judged by content
/// identity; without one, a text unit, judged by its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Expected {
    pub(crate) unit: Unit,
    pub(crate) identity: Option<Identity>,
}

impl From<Unit> for Expected {
    fn from(unit: Unit) -> Self {
        Self {
            unit,
            identity: None,
        }
    }
}

/// What a detection states beyond its text (`spec/01-report.md`
/// § Content). Our scanner always states it; an observed decoder that
/// reports text only does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Content {
    pub(crate) raw: Vec<u8>,
    /// `None`: an observed decoder that names no label, read as a complete
    /// reading.
    pub(crate) charset: Option<Charset>,
    /// `None`: no designator observed · `Some([])`: signalled, not located.
    pub(crate) eci: Option<Vec<Eci>>,
    pub(crate) fnc1: Option<Fnc1Position>,
}

impl Content {
    /// `charset` `unknown`: `text` is not a complete decoding.
    fn is_lossy(&self) -> bool {
        self.charset.is_some_and(Charset::is_lossy)
    }
}

/// One detection as observed. Its text and bytes stay in memory: receipts
/// carry their sha256 and length only, stdout previews them for public
/// sets only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) symbology: Symbology,
    pub(crate) text: String,
    pub(crate) content: Option<Content>,
    pub(crate) engines: Vec<EngineKind>,
}

/// Our scanner's detections as the judge sees them, in report order.
pub(crate) fn observations(detections: &[Detection]) -> Vec<Seen> {
    detections
        .iter()
        .map(|d| Seen {
            symbology: d.symbology,
            text: d.content.text.clone(),
            content: Some(Content {
                raw: d.content.raw.clone(),
                charset: Some(d.content.charset),
                eci: d.content.eci.as_ref().map(|list| {
                    list.iter()
                        .map(|s| Eci {
                            designator: s.designator,
                            start: s.start,
                        })
                        .collect()
                }),
                fnc1: d.content.fnc1,
            }),
            engines: d.engines.clone(),
        })
        .collect()
}

/// A detection whose bytes are its text's UTF-8, read `utf8` with nothing
/// declared — the shape of every plain payload.
#[cfg(test)]
pub(crate) fn plain(symbology: Symbology, text: &str) -> Seen {
    Seen {
        symbology,
        text: text.to_owned(),
        content: Some(Content {
            raw: text.as_bytes().to_vec(),
            charset: Some(Charset::Utf8),
            eci: None,
            fnc1: None,
        }),
        engines: vec![EngineKind::Rqrr],
    }
}

/// How one detection meets one expected unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Meet {
    /// Not this unit: another content, or a reading the truth contradicts.
    No,
    /// This unit's content, read as the truth reads it.
    Exact,
    /// This unit's content, without a text reading the scanner verified.
    Unread,
}

impl Meet {
    fn as_str(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::Exact => "exact",
            Self::Unread => "unread",
        }
    }
}

/// The rule of one (unit, detection) pair. A structural unit and a
/// detection with bytes compare by identity (`spec/01-report.md`
/// § Comparing contents): the raw and FNC1 position must match; equal
/// declarations then judge the reading; an unlocated signal (`[]`), or an
/// `unknown` reading where no designator was observed, is unread; any
/// other declaration contradicts the truth. Everything else — a text unit,
/// a detection without bytes — follows the text rule.
fn meet(expected: &Expected, seen: &Seen) -> Meet {
    let unit = &expected.unit;
    if unit.symbology.is_some_and(|s| s != seen.symbology) {
        return Meet::No;
    }
    let lossy = seen.content.as_ref().is_some_and(Content::is_lossy);
    let reading = || match (lossy, seen.text == unit.text) {
        (true, _) => Meet::Unread,
        (false, true) => Meet::Exact,
        (false, false) => Meet::No,
    };
    let (Some(truth), Some(content)) = (&expected.identity, &seen.content) else {
        return reading();
    };
    if content.raw != truth.raw || content.fnc1 != truth.fnc1 {
        return Meet::No;
    }
    match (&truth.eci, &content.eci) {
        (declared, observed) if declared == observed => reading(),
        (Some(_), Some(observed)) if observed.is_empty() => Meet::Unread,
        (Some(_), None) if lossy => Meet::Unread,
        _ => Meet::No,
    }
}

/// One unit's place in the best matching: the detection that meets it and
/// how, or `None`.
type Met = Option<(usize, Meet)>;

/// The one-to-one matching of units to detections with the most exact
/// meets, then the most meets. `relation[u][d]` is how detection `d` meets
/// unit `u`. Kuhn–Munkres (shortest augmenting paths with potentials) on
/// the costs exact 0 · unread `w − 1` · none `w`, with `w` = units + 1 so
/// that one more exact meet outweighs any number of unread ones; every unit
/// also owns an unmet column at cost `w`, so each assignment is complete.
fn best_matching(relation: &[Vec<Meet>], detections: usize) -> Vec<Met> {
    let units = relation.len();
    let none = i64::try_from(units).map_or(i64::MAX / 4, |n| n + 1);
    let columns = detections + units;
    let cost = |unit: usize, column: usize| match relation[unit].get(column) {
        Some(Meet::Exact) => 0,
        Some(Meet::Unread) => none - 1,
        _ => none,
    };
    // 1-based: unit and column 0 are the sentinels; `owner[c]` is the unit
    // holding column c (0 = free), `via[c]` the column it was reached from.
    let mut unit_potential = vec![0i64; units + 1];
    let mut column_potential = vec![0i64; columns + 1];
    let mut owner = vec![0usize; columns + 1];
    let mut via = vec![0usize; columns + 1];
    for unit in 1..=units {
        owner[0] = unit;
        let mut column = 0;
        let mut slack = vec![i64::MAX; columns + 1];
        let mut visited = vec![false; columns + 1];
        while owner[column] != 0 {
            visited[column] = true;
            let holder = owner[column];
            let (mut delta, mut next) = (i64::MAX, 0);
            for c in 1..=columns {
                if visited[c] {
                    continue;
                }
                let reduced =
                    cost(holder - 1, c - 1) - unit_potential[holder] - column_potential[c];
                if reduced < slack[c] {
                    slack[c] = reduced;
                    via[c] = column;
                }
                if slack[c] < delta {
                    delta = slack[c];
                    next = c;
                }
            }
            for c in 0..=columns {
                if visited[c] {
                    unit_potential[owner[c]] += delta;
                    column_potential[c] -= delta;
                } else {
                    slack[c] -= delta;
                }
            }
            column = next;
        }
        while column != 0 {
            let previous = via[column];
            owner[column] = owner[previous];
            column = previous;
        }
    }
    let mut met = vec![None; units];
    for (column, &holder) in owner.iter().enumerate().take(detections + 1).skip(1) {
        if holder > 0 && relation[holder - 1][column - 1] != Meet::No {
            met[holder - 1] = Some((column - 1, relation[holder - 1][column - 1]));
        }
    }
    met
}

/// The strict verdict for one image. `observed` is the report's complete
/// detection list: an image is `exact` only when every expected unit is
/// met exactly AND nothing else came back. Matching is one-to-one: a unit
/// consumes at most one detection, so a text unit met by the same text
/// under two symbologies leaves one detection over — `extra`.
pub(crate) fn judge_detections(expected: &[Expected], observed: &[Seen]) -> Outcome {
    judgment(expected, observed).0
}

/// [`judge_detections`] for a caller that holds (symbology, text) pairs
/// only: every unit is a text unit and no detection carries bytes, so the
/// text rule decides.
pub(crate) fn judge(expected: &[Unit], observed: &[(Symbology, &str)]) -> Outcome {
    let expected: Vec<Expected> = expected.iter().cloned().map(Expected::from).collect();
    let observed: Vec<Seen> = observed
        .iter()
        .map(|(symbology, text)| Seen {
            symbology: *symbology,
            text: (*text).to_owned(),
            content: None,
            engines: Vec::new(),
        })
        .collect();
    judge_detections(&expected, &observed)
}

/// [`judge_detections`] with, per expected unit, the detection that met
/// it.
fn judgment(expected: &[Expected], observed: &[Seen]) -> (Outcome, Vec<Met>) {
    if expected.is_empty() {
        let outcome = if observed.is_empty() {
            Outcome::NegativeHeld
        } else {
            Outcome::FalsePositive
        };
        return (outcome, Vec::new());
    }
    if observed.is_empty() {
        return (Outcome::Missed, vec![None; expected.len()]);
    }
    let relation: Vec<Vec<Meet>> = expected
        .iter()
        .map(|unit| observed.iter().map(|seen| meet(unit, seen)).collect())
        .collect();
    let met = best_matching(&relation, observed.len());
    let found = met.iter().flatten().count();
    let unread = met
        .iter()
        .flatten()
        .filter(|(_, how)| *how == Meet::Unread)
        .count();
    let extra = found < observed.len();
    let outcome = match (found, found == expected.len(), extra) {
        (0, _, _) => Outcome::Wrong,
        (_, true, false) if unread == 0 => Outcome::Exact,
        (_, true, false) => Outcome::Unread,
        (_, true, true) => Outcome::Extra,
        (_, false, false) => Outcome::Partial,
        (_, false, true) => Outcome::Mixed,
    };
    (outcome, met)
}

/// Grouping-contract violations of one report (`spec/01-report.md`).
/// Messages name detection indices only — payload never leaves here.
pub(crate) fn contract_violations(observed: &[Seen]) -> Vec<String> {
    let mut found = wire_shape_violations(observed);
    found.extend(shared_identities(observed));
    found
}

/// The two rules that shape OUR wire report: at most 16 detections, QR
/// family first. A comparator never promised them, so in observed mode
/// they are recorded as information instead of judged.
fn wire_shape_violations(observed: &[Seen]) -> Vec<String> {
    let mut found = Vec::new();
    if observed.len() > MAX_GROUPS {
        found.push(format!(
            "{} detections exceed the cap of {MAX_GROUPS}",
            observed.len()
        ));
    }
    if let Some(first_other) = observed.iter().position(|d| !d.symbology.is_qr_family())
        && let Some(late) = observed[first_other..]
            .iter()
            .position(|d| d.symbology.is_qr_family())
    {
        found.push(format!(
            "QR-family detection #{} follows non-QR detection #{first_other}",
            first_other + late
        ));
    }
    found
}

/// The grouping rule every decoder is held to: no two detections of one
/// report share an identity — equal text over different bytes is two
/// contents.
fn shared_identities(observed: &[Seen]) -> Vec<String> {
    let mut found = Vec::new();
    for (index, seen) in observed.iter().enumerate() {
        if let Some(first) = observed[..index]
            .iter()
            .position(|other| same_content(other, seen))
        {
            found.push(format!(
                "detection #{index} shares the identity of detection #{first}"
            ));
        }
    }
    found
}

/// Whether two detections denote the same content: equal symbology, raw
/// and FNC1 position and compatible ECI (both absent, or one unlocated, or
/// equal lists). Without bytes on both sides, (symbology, text) is the
/// only identity there is.
fn same_content(one: &Seen, other: &Seen) -> bool {
    if one.symbology != other.symbology {
        return false;
    }
    match (&one.content, &other.content) {
        (Some(left), Some(right)) => {
            let compatible = match (&left.eci, &right.eci) {
                (None, None) => true,
                (Some(left_eci), Some(right_eci)) => {
                    left_eci.is_empty() || right_eci.is_empty() || left_eci == right_eci
                }
                _ => false,
            };
            left.raw == right.raw && left.fnc1 == right.fnc1 && compatible
        }
        _ => one.text == other.text,
    }
}

// ----------------------------------------------------------- dispositions

/// What a disposition says about its row — and exactly what it excuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispositionKind {
    /// A confident decode the truth contradicts: stays `wrong` and blocks
    /// until fixed, honestly refused, or independently superseded by a
    /// validated truth amendment. `signature` pins the historical read
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
        // Historical read judged wrong against the original upstream truth.
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
    /// Original pins still hold, but independently validated effective text
    /// supersedes this historical judgment. The complete-set verdict stands.
    Superseded,
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
            Self::Superseded => "superseded",
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
/// the output a `known_wrong` disposition pins. The pins name zxing rows,
/// whose truths are text, so the signature is the text.
fn observed_signature(observed: &[Seen]) -> String {
    let mut lines = String::new();
    for seen in observed {
        writeln!(
            lines,
            "{}\t{}",
            wire(&seen.symbology),
            external::sha256_bytes(seen.text.as_bytes())
        )
        .expect("write to string");
    }
    external::sha256_bytes(lines.as_bytes())
}

/// A truth-incomplete row judged against its truth plus the excused unit,
/// which may stand in for one detection. Truth and excused units only →
/// `ambiguous` (whether the truth is met exactly or unread); nothing at
/// all → `missed`; any other output keeps its strict verdict (`extra`,
/// `mixed`, `wrong`).
fn excused_outcome(
    expected: &[Expected],
    observed: &[Seen],
    (symbology, text_sha256): (Symbology, &str),
) -> Outcome {
    if observed.is_empty() {
        return Outcome::Missed;
    }
    let mut rest = observed.to_vec();
    if let Some(at) = rest.iter().position(|seen| {
        seen.symbology == symbology && external::sha256_bytes(seen.text.as_bytes()) == text_sha256
    }) {
        rest.remove(at);
    }
    match judge_detections(expected, &rest) {
        Outcome::Exact | Outcome::Unread | Outcome::Missed => Outcome::Ambiguous,
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
    if let Some(amendment) = row.truth_amendment {
        let effective_matches = row.expected.as_deref().is_some_and(|units| {
            matches!(units, [unit] if unit.identity.is_none()
                && unit.unit.symbology == Some(Symbology::QrCode)
                && external::sha256_bytes(unit.unit.text.as_bytes()) == amendment.effective_text_sha256)
        });
        if matches!(disposition.kind, DispositionKind::KnownWrong { .. })
            && amendment == &truth_amendments::Q13
            && amendment.image_path == row.path
            && amendment.image_sha256 == row.image_sha256
            && amendment.original_truth_sha256 == truth_sha256
            && effective_matches
        {
            return (verdict, DispositionState::Superseded);
        }
        return (verdict, DispositionState::Void);
    }
    let observed = detections_of(&row.scan);
    let pinned_verdict = verdict == disposition.verdict;
    let (outcome, holds) = match disposition.kind {
        DispositionKind::KnownWrong { signature } => (
            verdict,
            pinned_verdict && observed_signature(observed) == signature,
        ),
        DispositionKind::TruthIncomplete { excused } => {
            let expected = row.expected.as_deref().unwrap_or_default();
            let outcome = excused_outcome(expected, observed, excused);
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
            detections: observations(&report.detections),
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
    expected: Option<Vec<Expected>>,
    /// sha256 of the truth's source bytes (`.txt` file or manifest string).
    truth_sha256: Option<String>,
    /// Present only after original inputs passed the shared amendment resolver.
    truth_amendment: Option<&'static Amendment>,
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

/// The detections of a scan, in report order — none for a failed scan.
fn detections_of(scan: &Scan) -> &[Seen] {
    match scan {
        Scan::Report { detections, .. } => detections,
        Scan::Failed { .. } => &[],
    }
}

/// Judge one scan. `wire_binds` is our own scanner, which owes the whole
/// grouping contract; an observed decoder owes the grouping rule but not
/// our wire shape (cap, order), which then lands in `wire_info`.
fn verdict_of(expected: Option<&[Expected]>, scan: &Scan, wire_binds: bool) -> Verdict {
    if matches!(scan, Scan::Failed { .. }) {
        return Verdict {
            raw: None,
            outcome: Outcome::Error,
            contract: Vec::new(),
            wire_info: Vec::new(),
        };
    }
    let observed = detections_of(scan);
    let (contract, wire_info) = if wire_binds {
        (contract_violations(observed), Vec::new())
    } else {
        (shared_identities(observed), wire_shape_violations(observed))
    };
    let (raw, outcome) = match expected {
        Some(units) => {
            let verdict = judge_detections(units, observed);
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
/// `raw_b64` and runner flags ride along for the record.
#[derive(Debug, Deserialize)]
struct ObservedLine {
    path: String,
    sha256: String,
    #[serde(default)]
    outputs: Vec<ObservedOutput>,
    error: Option<String>,
}

/// One decoder output: its text, and, when the decoder reports them, the
/// payload bytes (`raw_hex`, lowercase) with the wire `charset`, `eci` and
/// `fnc1` — an output with bytes is judged by identity.
#[derive(Debug, Deserialize)]
struct ObservedOutput {
    symbology: Symbology,
    text: String,
    #[serde(default)]
    raw_hex: Option<String>,
    #[serde(default)]
    charset: Option<Charset>,
    #[serde(default)]
    eci: Option<Vec<Eci>>,
    #[serde(default)]
    fnc1: Option<Fnc1Position>,
}

impl ObservedOutput {
    /// The output as the judge sees it; declarations without bytes are
    /// refused rather than dropped.
    fn seen(self) -> Result<Seen, String> {
        let content = match self.raw_hex {
            Some(hex) => Some(Content {
                raw: parse_hex(&hex)?,
                charset: self.charset,
                eci: self.eci,
                fnc1: self.fnc1,
            }),
            None if self.charset.is_some() || self.eci.is_some() || self.fnc1.is_some() => {
                return Err("charset, eci or fnc1 without raw_hex".to_owned());
            }
            None => None,
        };
        Ok(Seen {
            symbology: self.symbology,
            text: self.text,
            content,
            engines: Vec::new(),
        })
    }
}

/// One observed image, grouped.
#[derive(Debug)]
struct ObservedImage {
    sha256: String,
    scan: Scan,
    /// Outputs folded into an earlier identical output.
    merged: usize,
}

/// A decoder's parsed observations.
#[derive(Debug)]
struct Observed {
    meta: DecoderMeta,
    records: BTreeMap<String, ObservedImage>,
}

/// Fold identical outputs, first occurrence first: a payload reported twice
/// is one content. Outputs that differ in any field stay apart (equal text
/// over other bytes is another content).
fn group_outputs(outputs: Vec<ObservedOutput>) -> Result<(Vec<Seen>, usize), String> {
    let mut groups: Vec<Seen> = Vec::with_capacity(outputs.len());
    let mut merged = 0;
    for output in outputs {
        let seen = output.seen()?;
        if groups.contains(&seen) {
            merged += 1;
        } else {
            groups.push(seen);
        }
    }
    Ok((groups, merged))
}

/// Lowercase hex, two digits per byte.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{b:02x}").expect("write to string");
    }
    out
}

/// The bytes of a lowercase hex string, two digits per byte.
fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let pairs = text.as_bytes().chunks(2);
    pairs
        .map(|pair| match pair {
            [high, low] => digit(*high)
                .zip(digit(*low))
                .map(|(high, low)| (high << 4) | low),
            _ => None,
        })
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(|| "raw_hex must be lowercase hex, two digits per byte".to_owned())
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
                let (detections, merged) =
                    group_outputs(record.outputs).map_err(|e| format!("line {n}: {e}"))?;
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
    /// Hash-verified non-image; original `.txt` bytes retained for provenance.
    Aux(Option<Vec<u8>>),
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
    Probe::Aux(is_text.then_some(bytes))
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
            // an unwalkable corpus cannot be verified: a gate failure, which
            // names the root as such, never by its absolute path
            let root = dir.to_string_lossy();
            let e = if root.len() > 1 {
                e.replace(root.as_ref(), "<corpus root>")
            } else {
                e
            };
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

    // path → (pinned sha256, original bytes) of every verified truth file
    let mut truths: BTreeMap<&str, (&str, Vec<u8>)> = BTreeMap::new();
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
    truths: &BTreeMap<&str, (&str, Vec<u8>)>,
    problems: &mut Vec<String>,
) -> Option<Judged> {
    let (set, expected, truth_sha256, truth_amendment) = if row.path.starts_with("zxing-blackbox/")
    {
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
        let truth = match truth_amendments::resolve(&row.path, &row.sha256, sha256, text) {
            Ok(truth) => truth,
            Err(why) => {
                problems.push(why);
                return None;
            }
        };
        let unit = Unit {
            symbology: Some(symbology),
            text: truth.text,
        };
        (
            Set::Zxing,
            Some(vec![Expected::from(unit)]),
            Some((*sha256).to_owned()),
            truth.amendment,
        )
    } else if row.path.starts_with("qrcode-ai/") {
        (Set::Gallery, None, None, None)
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
        truth_amendment,
        verdict: verdict.raw,
        outcome: verdict.outcome,
        disposition: None,
        scan,
        contract: verdict.contract,
        wire_info: verdict.wire_info,
        merged,
    })
}

/// `corpus.toml` → expected set: `expected` is the truth's text, and
/// `raw_hex` (with `eci` and `fnc1`) its bytes and declarations when they
/// are known — a structural unit; without `raw_hex` a text unit. No
/// `expected` is a negative sample, unless the entry is a frontier pin
/// (`expect = "fail"`), which has no truth at all. A byte truth that cannot
/// stand is refused, never read as a text truth: bytes without a text,
/// declarations without bytes, hex other than lowercase pairs, an `eci`
/// list that is empty (a scanner state, never a truth), out of order,
/// past the bytes or beyond 999999, an `fnc1` other than first or second.
pub(crate) fn vendored_expectation(entry: &crate::Entry) -> Result<Option<Vec<Expected>>, String> {
    if !matches!(entry.expect.as_deref(), None | Some("pass" | "fail")) {
        return Err("expect must be \"pass\" or \"fail\"".to_owned());
    }
    let identity = match (&entry.raw_hex, entry.expected.is_some()) {
        (None, _) if entry.eci.is_some() || entry.fnc1.is_some() => {
            return Err("eci or fnc1 without raw_hex".to_owned());
        }
        (None, _) => None,
        (Some(_), false) => return Err("raw_hex without an expected text".to_owned()),
        (Some(hex), true) => Some(identity_of(hex, entry.eci.as_deref(), entry.fnc1)?),
    };
    Ok(match (entry.expected.as_deref(), entry.expect.as_deref()) {
        (Some(text), _) => Some(vec![Expected {
            unit: Unit {
                symbology: None,
                text: text.to_owned(),
            },
            identity,
        }]),
        (None, Some("fail")) => None,
        (None, _) => Some(Vec::new()),
    })
}

/// A byte truth's identity, checked as `spec/01-report.md` § `eci`
/// states the located list.
fn identity_of(
    raw_hex: &str,
    eci: Option<&[Eci]>,
    fnc1: Option<Fnc1Position>,
) -> Result<Identity, String> {
    let raw = parse_hex(raw_hex)?;
    if let Some(list) = eci {
        if list.is_empty() {
            return Err(
                "eci = [] is a scanner state (signalled, not located), never a truth".into(),
            );
        }
        let in_order = list.windows(2).all(|pair| pair[0].start <= pair[1].start);
        let within = list
            .iter()
            .all(|e| usize::try_from(e.start).is_ok_and(|start| start <= raw.len()));
        if !in_order || !within || list.iter().any(|e| e.designator > 999_999) {
            return Err(
                "eci entries must be designators 0-999999 with non-decreasing starts within raw"
                    .into(),
            );
        }
    }
    if fnc1.is_some_and(|f| !matches!(f, Fnc1Position::First | Fnc1Position::Second)) {
        return Err("fnc1 must be \"first\" or \"second\"".into());
    }
    Ok(Identity {
        raw,
        eci: eci.map(<[Eci]>::to_vec),
        fnc1,
    })
}

/// Every entry's expected set, in manifest order, or the first broken
/// truth (named by its path).
pub(crate) fn vendored_truths(
    corpus: &crate::Corpus,
) -> Result<Vec<Option<Vec<Expected>>>, String> {
    corpus
        .entry
        .iter()
        .map(|entry| vendored_expectation(entry).map_err(|e| format!("{}: {e}", entry.path)))
        .collect()
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

/// Judge every `corpus.toml` entry (vendored, always present) from
/// `source`, against its expected set from [`vendored_truths`].
fn evaluate_vendored(
    root: &Path,
    corpus: &crate::Corpus,
    truths: &[Option<Vec<Expected>>],
    source: Source<'_>,
    integrity: &mut Integrity,
) -> Vec<Judged> {
    let probes: Vec<(&crate::Entry, &Option<Vec<Expected>>, VendoredProbe)> = corpus
        .entry
        .par_iter()
        .zip(truths.par_iter())
        .map(|(entry, expected)| {
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
            (entry, expected, probe)
        })
        .collect();
    let mut rows = Vec::with_capacity(probes.len());
    for (entry, expected, probe) in probes {
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
        let expected = expected.clone();
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
            truth_amendment: None,
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
            (
                DispositionState::Holds | DispositionState::Differs | DispositionState::Superseded,
                _,
            ) => continue,
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
struct Counts([u32; 13]);

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
    /// Rows with expected units — the exact- and unread-rate denominator.
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

    fn unread(&self) -> u32 {
        self.outcomes.get(Outcome::Unread)
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
    truth_amendments_sha256: String,
}

/// The outcome rule of each truth kind, as every receipt states it.
#[derive(Serialize)]
struct Rules {
    /// A unit that knows the symbol's bytes.
    structural: &'static str,
    /// A unit that knows only its text.
    text: &'static str,
    /// One image, from the best one-to-one matching of units to detections.
    image: &'static str,
    /// One report.
    contract: &'static str,
}

const RULES: Rules = Rules {
    structural: "judged by content identity: met exact by a detection with the truth's raw, \
                 FNC1 position, located ECI list and text, its charset not unknown; met unread by \
                 one with the truth's raw and FNC1 position whose ECI use was signalled but not \
                 located (eci []), or whose charset is unknown with the truth's declarations or \
                 none observed; any other detection does not meet it (equal text over other bytes \
                 is another content). A detection that carries no bytes is held to the text rule",
    text: "judged by text: met exact by a detection with the truth's text, its charset not \
           unknown; met unread by a detection whose charset is unknown; any other detection does \
           not meet it",
    image: "one detection per unit, most exact meets first: exact = every unit met exact and \
            nothing else; unread = every unit met, one or more unread, nothing else (its own \
            rate, never blocking); extra = every unit met plus other output; partial = some units \
            met, nothing else; mixed = some met plus other output; wrong = output, no unit met; \
            missed = no output",
    contract: "at most 16 detections, QR family first, and no two detections denote the same \
               content: same symbology, raw and FNC1 position and compatible ECI (both absent, \
               or one [] or equal lists); detections without bytes compare by (symbology, text)",
};

#[derive(Serialize)]
struct SetReceipt {
    set: &'static str,
    #[serde(flatten)]
    tally: Tally,
    /// Wilson 95% interval of exact / truth.
    exact_wilson95: Option<[String; 2]>,
    /// Wilson 95% interval of unread / truth.
    unread_wilson95: Option<[String; 2]>,
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
    /// `structural` (bytes known: judged by identity) · `text`.
    kind: &'static str,
    text_sha256: String,
    text_len: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_len: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    eci: Option<Vec<Eci>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fnc1: Option<String>,
    /// The detection that met this unit under the raw verdict, and how;
    /// `null` = not met.
    met: Option<MetReceipt>,
}

#[derive(Serialize)]
struct MetReceipt {
    detection: usize,
    how: &'static str,
}

#[derive(Serialize)]
struct DetectionReceipt {
    symbology: Symbology,
    text_sha256: String,
    text_len: usize,
    /// `null` for a decoder that reports text only.
    raw_sha256: Option<String>,
    raw_len: Option<usize>,
    charset: Option<String>,
    /// As the report states it: absent = no designator observed, `[]` =
    /// signalled, not located.
    #[serde(skip_serializing_if = "Option::is_none")]
    eci: Option<Vec<Eci>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fnc1: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    truth_amendment: Option<&'static Amendment>,
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

/// The scanned tree: `HEAD`, and `clean` or `dirty` by `git status` with
/// the files this run writes (`outputs`: `--json`, `--emit-observed`) left
/// out, so a report kept inside the checkout never makes the next run's
/// tree dirty.
fn source_identity(repo: &Path, outputs: &[&Path]) -> SourceEcho {
    let git_sha = git_stdout(repo, &["rev-parse", "HEAD"])
        .map(|sha| sha.trim().to_owned())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| String::from("unknown"));
    let excluded: Vec<String> = outputs
        .iter()
        .filter_map(|path| inside(repo, path))
        .map(|rel| format!(":(exclude,literal){rel}"))
        .collect();
    let mut status = vec!["status", "--porcelain", "--untracked-files=all", "--", "."];
    status.extend(excluded.iter().map(String::as_str));
    let tree = match git_stdout(repo, &status) {
        Some(status) if status.trim().is_empty() => "clean",
        Some(_) => "dirty",
        None => "unknown",
    };
    SourceEcho {
        git_sha,
        tree: tree.to_owned(),
    }
}

/// `path` relative to `repo`, when it lies inside it — both resolved, the
/// file through its directory (it may not exist yet; a directory that does
/// not exist yet holds nothing to leave out).
fn inside(repo: &Path, path: &Path) -> Option<String> {
    let repo = std::fs::canonicalize(repo).ok()?;
    let absolute = std::env::current_dir().ok()?.join(path);
    let dir = std::fs::canonicalize(absolute.parent()?).ok()?;
    let file = dir.join(absolute.file_name()?);
    let rel = file.strip_prefix(&repo).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

#[derive(Serialize)]
struct Receipt {
    oracle: &'static str,
    /// The outcome rule per truth kind under [`RULES_ID`].
    rules: Rules,
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

fn detection_receipt(seen: &Seen) -> DetectionReceipt {
    let (text_sha256, text_len) = text_hash(&seen.text);
    let content = seen.content.as_ref();
    DetectionReceipt {
        symbology: seen.symbology,
        text_sha256,
        text_len,
        raw_sha256: content.map(|c| external::sha256_bytes(&c.raw)),
        raw_len: content.map(|c| c.raw.len()),
        charset: content.and_then(|c| c.charset).map(|c| wire(&c)),
        eci: content.and_then(|c| c.eci.clone()),
        fnc1: content.and_then(|c| c.fnc1).map(|f| wire(&f)),
        engines: seen.engines.clone(),
    }
}

fn unit_receipt(expected: &Expected, met: Met) -> UnitReceipt {
    let (text_sha256, text_len) = text_hash(&expected.unit.text);
    let identity = expected.identity.as_ref();
    UnitReceipt {
        symbology: expected.unit.symbology,
        kind: if identity.is_some() {
            "structural"
        } else {
            "text"
        },
        text_sha256,
        text_len,
        raw_sha256: identity.map(|i| external::sha256_bytes(&i.raw)),
        raw_len: identity.map(|i| i.raw.len()),
        eci: identity.and_then(|i| i.eci.clone()),
        fnc1: identity.and_then(|i| i.fnc1).map(|f| wire(&f)),
        met: met.map(|(detection, how)| MetReceipt {
            detection,
            how: how.as_str(),
        }),
    }
}

fn row_receipt(row: &Judged) -> RowReceipt {
    let (detections, engine_panics, error) = match &row.scan {
        Scan::Report {
            detections,
            engine_panics,
        } => (
            detections.iter().map(detection_receipt).collect(),
            *engine_panics,
            None,
        ),
        Scan::Failed { code, .. } => (Vec::new(), 0, Some(*code)),
    };
    let expected = row.expected.as_ref().map(|units| {
        // one entry per unit (none for a negative sample)
        let (_, met) = judgment(units, detections_of(&row.scan));
        units
            .iter()
            .zip(met)
            .map(|(unit, met)| unit_receipt(unit, met))
            .collect()
    });
    RowReceipt {
        set: row.set.as_str(),
        group: row.group.clone(),
        path: row.path.clone(),
        image_sha256: row.image_sha256.clone(),
        pin: row.pin.map(Status::as_str),
        truth_sha256: row.truth_sha256.clone(),
        truth_amendment: row.truth_amendment,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    charset: Option<Charset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    eci: Option<&'a [Eci]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fnc1: Option<Fnc1Position>,
}

impl<'a> EmittedOutput<'a> {
    fn of(seen: &'a Seen) -> Self {
        let content = seen.content.as_ref();
        Self {
            symbology: seen.symbology,
            text: &seen.text,
            raw_hex: content.map(|c| hex(&c.raw)),
            charset: content.and_then(|c| c.charset),
            eci: content.and_then(|c| c.eci.as_deref()),
            fnc1: content.and_then(|c| c.fnc1),
        }
    }
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
/// re-judged through the comparator path. The file holds decoded TEXT and
/// bytes — private gallery payloads included — and must stay local.
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
                (detections.iter().map(EmittedOutput::of).collect(), None)
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
            for (label, k) in [("exact", tally.exact()), ("unread", tally.unread())] {
                write!(line, " · {label} {k}/{}", tally.truth).expect("write to string");
                if let Some([low, high]) = wilson95(k, tally.truth) {
                    write!(line, " (Wilson 95% {low}–{high})").expect("write to string");
                }
            }
        }
        if tally.negatives > 0 {
            let held = tally.outcomes.get(Outcome::NegativeHeld);
            write!(line, " · negative_held {held}/{}", tally.negatives).expect("write to string");
        }
        for outcome in Outcome::ALL {
            let n = tally.outcomes.get(outcome);
            let rated = matches!(
                outcome,
                Outcome::Exact | Outcome::Unread | Outcome::NegativeHeld
            );
            if n > 0 && !rated {
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

/// ` raw a1 (1 B) · eci · eci [4@0] · fnc1 first` — the bytes (hex, the
/// first 24), charset and declarations of a PUBLIC unit or detection.
fn content_note(
    raw: &[u8],
    charset: Option<Charset>,
    eci: Option<&[Eci]>,
    fnc1: Option<Fnc1Position>,
) -> String {
    const MAX_BYTES: usize = 24;
    let cut = if raw.len() > MAX_BYTES { "…" } else { "" };
    let mut note = format!(
        " raw {}{cut} ({} B)",
        hex(&raw[..raw.len().min(MAX_BYTES)]),
        raw.len()
    );
    if let Some(charset) = charset {
        write!(note, " · {}", wire(&charset)).expect("write to string");
    }
    if let Some(eci) = eci {
        let list: Vec<String> = eci
            .iter()
            .map(|e| format!("{}@{}", e.designator, e.start))
            .collect();
        write!(note, " · eci [{}]", list.join(", ")).expect("write to string");
    }
    if let Some(fnc1) = fnc1 {
        write!(note, " · fnc1 {}", wire(&fnc1)).expect("write to string");
    }
    note
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
                for expected in row.expected.iter().flatten() {
                    let symbology = expected
                        .unit
                        .symbology
                        .map_or_else(|| "any".to_owned(), |s| wire(&s));
                    let bytes = expected.identity.as_ref().map_or_else(String::new, |i| {
                        content_note(&i.raw, None, i.eci.as_deref(), i.fnc1)
                    });
                    println!(
                        "{:17}expected {symbology} {}{bytes}",
                        "",
                        preview(&expected.unit.text)
                    );
                }
                for d in detections {
                    let engines: Vec<String> = d.engines.iter().map(wire).collect();
                    let bytes = d.content.as_ref().map_or_else(String::new, |c| {
                        content_note(&c.raw, c.charset, c.eci.as_deref(), c.fnc1)
                    });
                    println!(
                        "{:17}observed {} {}{bytes} ({})",
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

impl Args {
    /// The files this run writes, which its tree state leaves out.
    fn outputs(&self) -> Vec<&Path> {
        [self.json.as_deref(), self.emit_observed.as_deref()]
            .into_iter()
            .flatten()
            .collect()
    }
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
        rules: RULES,
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
            truth_amendments_sha256: truth_amendments::fingerprint(),
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
            unread_wilson95: wilson95(tally.unread(), tally.truth),
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
    let truths = vendored_truths(&corpus).unwrap_or_else(|e| {
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
    let identity = source_identity(&root, &args.outputs());
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
    rows.extend(evaluate_vendored(
        &root,
        &corpus,
        &truths,
        source,
        &mut integrity,
    ));
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
        println!(
            "observed (decoded text and bytes — keep local): {}",
            path.display()
        );
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

    /// A report of plain QR detections (UTF-8 bytes, nothing declared).
    fn report(observed: &[&str]) -> Scan {
        Scan::Report {
            detections: observed.iter().map(|text| plain(QrCode, text)).collect(),
            engine_panics: 0,
        }
    }

    fn eci(list: &[(u32, u32)]) -> Vec<Eci> {
        list.iter()
            .map(|&(designator, start)| Eci { designator, start })
            .collect()
    }

    /// A QR detection with explicit bytes, label and declarations.
    fn read(text: &str, raw: &[u8], charset: Charset, declared: Option<&[(u32, u32)]>) -> Seen {
        Seen {
            symbology: QrCode,
            text: text.to_owned(),
            content: Some(Content {
                raw: raw.to_vec(),
                charset: Some(charset),
                eci: declared.map(eci),
                fnc1: None,
            }),
            engines: vec![EngineKind::Rqrr],
        }
    }

    /// A structural QR unit: text, bytes and located declarations.
    fn structural(text: &str, raw: &[u8], declared: Option<&[(u32, u32)]>) -> Expected {
        Expected {
            unit: qr(text),
            identity: Some(Identity {
                raw: raw.to_vec(),
                eci: declared.map(eci),
                fnc1: None,
            }),
        }
    }

    /// A text QR unit.
    fn text_unit(text: &str) -> Expected {
        Expected::from(qr(text))
    }

    /// Public payloads of the two disposition rows (zxing blackbox suite
    /// `qrcode-2`): 13's truth and the misread it is pinned to, 16's outer
    /// truth and the unnamed inner symbol it excuses.
    const TRUTH_13: &str =
        "The 2005 USGS aerial photograph of the Washington Monument is censored.";
    const READ_13: &str =
        "The 2005 USGS aerial photography of the Washington Monument is censored.";
    const OUTER_16: &str = "[\u{5916}\u{5074}QR\u{30b3}\u{30fc}\u{30c9}]\r\n \r\n\
                            *\u{ff80}\u{ff9e}\u{ff8c}\u{ff9e}\u{ff99}QR*\r\nhttp://d-qr.net/ex/";
    const INNER_16: &str = "[\u{5185}\u{5074}QR\u{30b3}\u{30fc}\u{30c9}]\r\n\r\n\
                            *\u{30c0}\u{30d6}\u{30eb}QR*\r\nhttp://d-qr.net/ex/";

    /// A zxing-shaped row for `disposition`'s path and pinned hashes, judged
    /// as our own scan.
    fn zxing_row(disposition: &Disposition, truth: &str, observed: &[&str]) -> Judged {
        let expected = vec![text_unit(truth)];
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
            truth_amendment: None,
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

    /// `unread` joins the vocabulary beside `wrong` and `missed`, outside
    /// the blocking class: its own rate.
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
                "unread",
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

    /// Every receipt names its judge rules and amendment provenance so
    /// historical truth and effective truth are never compared silently.
    #[test]
    fn receipts_carry_the_rules_id() {
        let source = SourceEcho {
            git_sha: String::from("unknown"),
            tree: String::from("unknown"),
        };
        let receipt = new_receipt(source, "manifest", "vendored", Integrity::default());
        let json = serde_json::to_value(&receipt).expect("receipt serialises");
        assert_eq!(json["oracle"], "qrscan-oracle/v2.1");
        assert_eq!(
            json["inputs"]["truth_amendments_sha256"],
            "0798a4c08952380c3195494be65e658045c5a1d89e43065e9def3b4485a022c8"
        );
        let rules = json["rules"].as_object().expect("a rules object");
        let mut kinds: Vec<&str> = rules.keys().map(String::as_str).collect();
        kinds.sort_unstable();
        assert_eq!(kinds, ["contract", "image", "structural", "text"]);
        assert!(
            rules
                .values()
                .all(|rule| rule.as_str().is_some_and(|r| r.len() > 40)),
            "{rules:?}"
        );
    }

    /// Plain QR detections (UTF-8 bytes, nothing declared).
    fn plains(observed: &[(Symbology, &str)]) -> Vec<Seen> {
        observed
            .iter()
            .map(|(symbology, text)| plain(*symbology, text))
            .collect()
    }

    /// Kept from v1.1 with the identity rule in place of (symbology, text):
    /// two identical detections still share an identity.
    #[test]
    fn contract_checks_flag_cap_order_and_duplicates() {
        let texts: Vec<String> = (0..17).map(|i| format!("payload-{i}")).collect();
        let seventeen: Vec<Seen> = texts.iter().map(|text| plain(QrCode, text)).collect();
        let found = contract_violations(&seventeen);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("17 detections"), "{found:?}");
        assert!(
            contract_violations(&seventeen[..16]).is_empty(),
            "16 is the cap, not a violation"
        );

        let found = contract_violations(&plains(&[(Ean13, "1"), (QrCode, "2")]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("QR-family detection #1"), "{found:?}");
        assert!(
            contract_violations(&plains(&[(QrCode, "2"), (MicroQrCode, "3"), (Ean13, "1")]))
                .is_empty()
        );

        let found = contract_violations(&plains(&[(QrCode, "A"), (QrCode, "A")]));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("shares the identity of detection #0"),
            "{found:?}"
        );
        // an EAN-13 and a QR carrying the same digits are two contents
        assert!(contract_violations(&plains(&[(QrCode, "1"), (Ean13, "1")])).is_empty());
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

    fn amended_q13(observed: &[&str]) -> Judged {
        let pin = &truth_amendments::Q13;
        let row = Row {
            status: Status::Wrong,
            sha256: pin.image_sha256.to_owned(),
            path: pin.image_path.to_owned(),
        };
        let truths = BTreeMap::from([(
            "zxing-blackbox/qrcode-2/13.txt",
            (pin.original_truth_sha256, TRUTH_13.as_bytes().to_vec()),
        )]);
        let mut problems = Vec::new();
        let judged = external_row(&row, (report(observed), 0, true), &truths, &mut problems)
            .expect("validated q13 source");
        assert_eq!(problems, Vec::<String>::new());
        judged
    }

    #[test]
    fn amended_text_is_shared_and_supersedes_history_without_excusing_output() {
        for (observed, expected_outcome) in [
            (vec![READ_13], Outcome::Exact),
            (vec![TRUTH_13], Outcome::Wrong),
            (vec!["unrelated"], Outcome::Wrong),
            (vec![READ_13, "unrelated"], Outcome::Extra),
            (vec![], Outcome::Missed),
        ] {
            let mut rows = vec![
                amended_q13(&observed),
                zxing_row(&DISPOSITIONS[1], OUTER_16, &[OUTER_16, INNER_16]),
            ];
            let checks = apply_dispositions(&mut rows, true);
            assert_eq!(rows[0].verdict, Some(expected_outcome));
            assert_eq!(rows[0].outcome, expected_outcome);
            assert_eq!(checks[0].state, DispositionState::Superseded);
            assert_eq!(rows[1].outcome, Outcome::Ambiguous);
            assert_eq!(checks[1].state, DispositionState::Holds);
            let expected = rows[0].expected.as_ref().expect("text truth");
            assert_eq!(expected.len(), 1);
            assert_eq!(expected[0].identity, None);
            assert_eq!(expected[0].unit.symbology, Some(QrCode));
            assert_eq!(expected[0].unit.text, READ_13);
            let shared = truth_amendments::resolve(
                &rows[0].path,
                &rows[0].image_sha256,
                rows[0].truth_sha256.as_deref().expect("original hash"),
                TRUTH_13.as_bytes(),
            )
            .expect("same shared loader");
            assert_eq!(expected[0].unit.text, shared.text);
            let problems = blockers(&Integrity::default(), &rows, &checks);
            let wanted = if expected_outcome.is_wrong_class() {
                vec![format!(
                    "{} — {}",
                    expected_outcome.as_str(),
                    truth_amendments::Q13.image_path
                )]
            } else {
                vec![]
            };
            assert_eq!(problems, wanted);
            let receipt = serde_json::to_value(row_receipt(&rows[0])).expect("receipt");
            assert_eq!(
                receipt["truth_sha256"],
                external::sha256_bytes(TRUTH_13.as_bytes())
            );
            assert_eq!(
                receipt["truth_amendment"]["effective_text_sha256"],
                external::sha256_bytes(READ_13.as_bytes())
            );
            assert_eq!(receipt["disposition"], "known_wrong");
            assert_eq!(receipt["disposition_state"], "superseded");
            let (_, sets) = tallies(&rows);
            let tally = &sets[&Set::Zxing];
            assert_eq!(
                (tally.images, tally.truth, tally.negatives, tally.unlabelled),
                (2, 2, 0, 0)
            );
            for outcome in Outcome::ALL {
                assert_eq!(
                    tally.outcomes.get(outcome),
                    u32::from(outcome == expected_outcome)
                        + u32::from(outcome == Outcome::Ambiguous)
                );
            }
        }
    }

    #[test]
    fn amended_external_row_refuses_drift_instead_of_emitting_a_false_exact() {
        let pin = &truth_amendments::Q13;
        for (image_sha, truth_sha, truth, error) in [
            (
                "changed",
                pin.original_truth_sha256,
                TRUTH_13,
                "q13 truth amendment: image sha256 mismatch",
            ),
            (
                pin.image_sha256,
                "changed",
                TRUTH_13,
                "q13 truth amendment: original truth sha256 mismatch",
            ),
            (
                pin.image_sha256,
                pin.original_truth_sha256,
                READ_13,
                "q13 truth amendment: original truth sha256 mismatch",
            ),
        ] {
            let row = Row {
                status: Status::Wrong,
                sha256: image_sha.to_owned(),
                path: pin.image_path.to_owned(),
            };
            let truths = BTreeMap::from([(
                "zxing-blackbox/qrcode-2/13.txt",
                (truth_sha, truth.as_bytes().to_vec()),
            )]);
            let mut problems = Vec::new();
            assert!(
                external_row(&row, (report(&[READ_13]), 0, true), &truths, &mut problems).is_none()
            );
            assert_eq!(problems, [error]);
        }
        let mut row = amended_q13(&[READ_13]);
        row.expected = Some(vec![text_unit(TRUTH_13)]);
        assert_eq!(
            settle(&row, Outcome::Wrong, &DISPOSITIONS[0], true),
            (Outcome::Wrong, DispositionState::Void)
        );
    }

    #[test]
    fn amended_external_row_requires_the_original_truth_file() {
        let pin = &truth_amendments::Q13;
        let row = Row {
            status: Status::Wrong,
            sha256: pin.image_sha256.to_owned(),
            path: pin.image_path.to_owned(),
        };
        let mut problems = Vec::new();
        assert!(
            external_row(
                &row,
                (report(&[]), 0, true),
                &BTreeMap::new(),
                &mut problems
            )
            .is_none()
        );
        assert_eq!(
            problems,
            [
                "labelled image without a verified zxing-blackbox/qrcode-2/13.txt or a pinned symbology: zxing-blackbox/qrcode-2/13.png"
            ]
        );
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
        assert_eq!(observed_signature(&[plain(QrCode, READ_13)]), signature);
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
            settled(&known_wrong, &zxing_row(&known_wrong, TRUTH_13, &[READ_13])),
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
        let mut redrawn = zxing_row(&known_wrong, TRUTH_13, &[READ_13]);
        redrawn.image_sha256 = "0".repeat(64);
        assert_eq!(
            settled(&known_wrong, &redrawn),
            (Outcome::Wrong, DispositionState::Void)
        );
        let mut amended = zxing_row(&known_wrong, TRUTH_13, &[READ_13]);
        amended.truth_sha256 = Some("0".repeat(64));
        assert_eq!(settled(&known_wrong, &amended).1, DispositionState::Void);
        // the truth plus the excused inner symbol: ambiguous, as pinned
        let both = zxing_row(&double_qr, OUTER_16, &[INNER_16, OUTER_16]);
        assert_eq!(
            settled(&double_qr, &both),
            (Outcome::Ambiguous, DispositionState::Holds)
        );
    }

    /// A known-wrong pin excuses one misread: a DIFFERENT wrong text on 13 is
    /// not the misread the entry pins — stale, even though the verdict class
    /// is the same.
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
            decoder(&known_wrong, &zxing_row(&known_wrong, TRUTH_13, &[READ_13])),
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

    /// An incomplete truth excuses one named symbol: a wrong extra output next
    /// to 16's outer truth must not hide behind `truth_incomplete` — never
    /// `ambiguous`.
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
            zxing_row(&known_wrong, TRUTH_13, &[READ_13]),
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
        let here = source_identity(&crate::repo_root(), &[]);
        let hex = |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit());
        assert!(here.git_sha == "unknown" || hex(&here.git_sha), "{here:?}");
        assert!(
            ["clean", "dirty", "unknown"].contains(&here.tree.as_str()),
            "{here:?}"
        );
        let nowhere = std::env::temp_dir().join(format!("qrscan-no-repo-{}", std::process::id()));
        assert_eq!(
            source_identity(&nowhere, &[]),
            SourceEcho {
                git_sha: String::from("unknown"),
                tree: String::from("unknown"),
            },
            "git failing is recorded, never fatal"
        );
        let selected = crate::resolve_repo_root(Some(nowhere.as_os_str()), &crate::repo_root())
            .expect("absolute override");
        assert_eq!(selected, nowhere);
        assert_eq!(
            source_identity(&selected, &[]),
            SourceEcho {
                git_sha: String::from("unknown"),
                tree: String::from("unknown"),
            },
            "runtime source identity must not fall back to the instrument's build checkout"
        );
    }

    /// The tree state leaves out the files the run itself writes: the JSON
    /// report and the emitted observations, inside the checkout, keep it
    /// clean; the same files named by nobody, or any other change, make it
    /// dirty. A repository without a commit still has a status.
    #[test]
    fn the_tree_state_leaves_out_the_files_the_run_writes() {
        let repo = std::env::temp_dir().join(format!("qrscan-oracle-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("out")).expect("repo dir");
        let init = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .output()
            .expect("git");
        assert!(init.status.success(), "git init");
        std::fs::write(repo.join("oracle.json"), "{}").expect("write");
        std::fs::write(repo.join("out/observed.jsonl"), "").expect("write");
        let written = [repo.join("oracle.json"), repo.join("out/observed.jsonl")];
        let written: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
        let tree = |outputs: &[&Path]| source_identity(&repo, outputs).tree;
        assert_eq!(tree(&written), "clean");
        assert_eq!(tree(&[]), "dirty", "the same files, written by nobody here");
        std::fs::write(repo.join("out/other.txt"), "x").expect("write");
        assert_eq!(tree(&written), "dirty", "a change the run did not write");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// An unwalkable corpus is an integrity problem that names the corpus
    /// root as such, never by its absolute path.
    #[test]
    fn a_walk_failure_names_the_corpus_root_not_its_path() {
        let dir =
            std::env::temp_dir().join(format!("qrscan-oracle-unwalkable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scanner = external::scanner();
        let mut integrity = Integrity::default();
        let judged = evaluate_external(&dir, &[], Source::Scan(&scanner), &mut integrity);
        assert!(judged.is_empty());
        assert_eq!(integrity.problems.len(), 1, "{:?}", integrity.problems);
        let problem = &integrity.problems[0];
        assert!(
            problem.starts_with("corpus walk failed: <corpus root>: "),
            "{problem}"
        );
        assert!(!problem.contains(&*dir.to_string_lossy()), "{problem}");
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
        // the bytes travel as sha256 + length too, never as hex
        assert!(
            !json.contains(&hex(PAYLOAD.as_bytes())),
            "payload bytes leaked"
        );
        let (sha256, len) = text_hash(PAYLOAD);
        assert!(json.contains(&sha256) && json.contains(&format!("\"text_len\":{len}")));
        assert!(json.contains(&format!("\"raw_len\":{len}")), "{json}");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        let mut keys = Vec::new();
        collect_keys(&value, &mut keys);
        for leak in ["text", "raw", "raw_hex"] {
            assert!(!keys.iter().any(|key| key == leak), "{keys:?}");
        }
        for hashed in ["text_sha256", "raw_sha256"] {
            assert!(keys.iter().any(|key| key == hashed), "{keys:?}");
        }
    }

    #[test]
    fn wilson_interval_matches_reference_values() {
        let interval = |low: &str, high: &str| Some([low.to_owned(), high.to_owned()]);
        assert_eq!(wilson95(169, 179), interval("0.900", "0.969"));
        assert_eq!(wilson95(34, 34), interval("0.898", "1.000"));
        assert_eq!(wilson95(0, 8), interval("0.000", "0.324"));
        assert_eq!(wilson95(0, 0), None);
    }

    fn entry(expected: Option<&str>, expect: Option<&str>) -> crate::Entry {
        crate::Entry {
            path: String::from("fixtures/x.png"),
            category: String::from("clean"),
            expected: expected.map(str::to_owned),
            expect: expect.map(str::to_owned),
            raw_hex: None,
            eci: None,
            fnc1: None,
        }
    }

    /// Kept from v1.1 (text, negative, frontier) on the entry itself.
    #[test]
    fn vendored_entries_map_to_expected_sets() {
        let text = |t: &str| {
            Ok(Some(vec![Expected::from(Unit {
                symbology: None,
                text: t.to_owned(),
            })]))
        };
        assert_eq!(vendored_expectation(&entry(Some("T"), None)), text("T"));
        assert_eq!(
            vendored_expectation(&entry(None, None)),
            Ok(Some(Vec::new())),
            "negative"
        );
        assert_eq!(
            vendored_expectation(&entry(None, Some("pass"))),
            Ok(Some(Vec::new()))
        );
        assert_eq!(
            vendored_expectation(&entry(None, Some("fail"))),
            Ok(None),
            "frontier"
        );
        // a frontier pin WITH truth is judged against it
        assert_eq!(
            vendored_expectation(&entry(Some("T"), Some("fail"))),
            text("T")
        );
        for expected in [None, Some("T")] {
            assert_eq!(
                vendored_expectation(&entry(expected, Some("FAIL"))),
                Err(String::from("expect must be \"pass\" or \"fail\"")),
                "a mistyped frontier flag must not silently become a negative or a truth"
            );
        }
    }

    /// `raw_hex` (with `eci`, `fnc1`) makes the unit structural; a broken
    /// byte truth is refused, never read as a text truth.
    #[test]
    fn vendored_byte_truths_are_structural_or_refused() {
        let mut declared = entry(Some("\u{104}"), None);
        declared.raw_hex = Some(String::from("a1"));
        declared.eci = Some(eci(&[(4, 0)]));
        declared.fnc1 = Some(Fnc1Position::First);
        assert_eq!(
            vendored_expectation(&declared),
            Ok(Some(vec![Expected {
                unit: Unit {
                    symbology: None,
                    text: String::from("\u{104}"),
                },
                identity: Some(Identity {
                    raw: vec![0xA1],
                    eci: Some(eci(&[(4, 0)])),
                    fnc1: Some(Fnc1Position::First),
                }),
            }]))
        );
        let broken = [
            ("A1", None, None, Some("A")),                // uppercase hex
            ("a", None, None, Some("A")),                 // odd length
            ("4g", None, None, Some("A")),                // not hex
            ("41", Some(&[][..]), None, Some("A")),       // eci [] is a scanner state
            ("41", Some(&[(4, 2)][..]), None, Some("A")), // start past the bytes
            ("4142", Some(&[(4, 1), (26, 0)][..]), None, Some("AB")), // starts decrease
            ("41", Some(&[(1_000_000, 0)][..]), None, Some("A")), // beyond 999999
            ("41", None, Some(Fnc1Position::Unrecognized), Some("A")), // fnc1 typo
            ("41", None, None, None),                     // bytes without a text
        ];
        for (raw_hex, declared, fnc1, expected) in broken {
            let mut bad = entry(expected, None);
            bad.raw_hex = Some(raw_hex.to_owned());
            bad.eci = declared.map(eci);
            bad.fnc1 = fnc1;
            assert!(
                vendored_expectation(&bad).is_err(),
                "{raw_hex} {declared:?} {fnc1:?}"
            );
        }
        // declarations without bytes
        let mut orphan = entry(Some("A"), None);
        orphan.fnc1 = Some(Fnc1Position::Second);
        assert!(vendored_expectation(&orphan).is_err());
        let corpus = crate::Corpus {
            entry: vec![entry(Some("A"), None), orphan],
        };
        let refused = vendored_truths(&corpus).expect_err("a broken truth");
        assert!(refused.starts_with("fixtures/x.png: "), "{refused}");
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
        assert!(detections.iter().all(|d| d.content.is_none()), "text only");
        assert!(matches!(
            &observed.records["fixtures/clean/c.png"].scan,
            Scan::Failed { code: "decoder_error", detail } if detail == "boom"
        ));
    }

    /// An output with `raw_hex` carries its bytes and declarations; equal
    /// text over other bytes is another content, an identical output folds.
    #[test]
    fn observed_outputs_with_bytes_group_by_identity() {
        let output = |raw_hex: &str| {
            serde_json::json!({"symbology": "qr_code", "text": "\u{FFFD}", "raw_hex": raw_hex,
                               "charset": "unknown", "eci": [], "fnc1": "second"})
        };
        let line = serde_json::json!({"path": "a.png", "sha256": "aa",
                                      "outputs": [output("a1"), output("c3a9"), output("a1")]});
        let text = observed_file("probe", &[line.to_string()]);
        let observed = parse_observed(&text, "probe").expect("parses");
        let image = &observed.records["a.png"];
        assert_eq!(image.merged, 1);
        let detections = detections_of(&image.scan);
        assert_eq!(detections.len(), 2, "{detections:?}");
        assert_eq!(
            detections[0].content,
            Some(Content {
                raw: vec![0xA1],
                charset: Some(Charset::Unknown),
                eci: Some(Vec::new()),
                fnc1: Some(Fnc1Position::Second),
            })
        );
        assert_eq!(
            detections[1].content.as_ref().map(|c| c.raw.clone()),
            Some(vec![0xC3, 0xA9])
        );
    }

    #[test]
    fn malformed_observed_files_are_refused() {
        let ok = record("a.png", "aa", &[]);
        let bad_symbology = record("a.png", "aa", &[("qr_kode", "A")]);
        let output = |extra: serde_json::Value| {
            let mut output = serde_json::json!({"symbology": "qr_code", "text": "A"});
            if let (Some(map), serde_json::Value::Object(extra)) = (output.as_object_mut(), extra) {
                map.extend(extra);
            }
            serde_json::json!({"path": "a.png", "sha256": "aa", "outputs": [output]}).to_string()
        };
        let bad_hex = output(serde_json::json!({"raw_hex": "4"}));
        let orphan = output(serde_json::json!({"eci": [{"designator": 26, "start": 0}]}));
        for (text, why) in [
            (
                observed_file("zxing-cpp", &[bad_hex]),
                "raw_hex not hex pairs",
            ),
            (
                observed_file("zxing-cpp", &[orphan]),
                "declarations without raw_hex",
            ),
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
        let text_only = |symbology: Symbology, text: &str| Seen {
            symbology,
            text: text.to_owned(),
            content: None,
            engines: Vec::new(),
        };
        let mut detections: Vec<Seen> = texts.iter().map(|text| text_only(QrCode, text)).collect();
        detections.insert(0, text_only(Ean13, "1"));
        let scan = Scan::Report {
            detections: detections.clone(),
            engine_panics: 0,
        };
        let observed = verdict_of(None, &scan, false);
        assert!(observed.contract.is_empty(), "{:?}", observed.contract);
        assert_eq!(observed.wire_info.len(), 2, "{:?}", observed.wire_info);
        let ours = verdict_of(None, &scan, true);
        assert!(ours.wire_info.is_empty());
        assert_eq!(
            ours.contract,
            contract_violations(&detections),
            "our scan keeps the full contract, in the usual order"
        );
        let duplicated = report(&["x", "x"]);
        assert_eq!(verdict_of(None, &duplicated, false).contract.len(), 1);
    }

    /// Our own judged scans, emitted and parsed back, give the same
    /// detections (text, bytes, declarations) and verdicts — the
    /// self-consistency the comparator path rests on. The emitted receipt
    /// fields stay hash-only.
    #[test]
    fn emitted_observations_rejudge_identically() {
        let [known_wrong, double_qr] = DISPOSITIONS;
        let mut errored = zxing_row(&known_wrong, "x", &[]);
        errored.path = String::from("fixtures/clean/broken.png");
        errored.scan = Scan::Failed {
            code: "QRS-001",
            detail: String::from("corrupt"),
        };
        let mut declared = zxing_row(&known_wrong, "\u{104}", &[]);
        declared.path = String::from("fixtures/clean/declared.png");
        declared.expected = Some(vec![structural("\u{104}", &[0xA1], Some(&[(4, 0)]))]);
        let mut signalled = read("\u{FFFD}", &[0xA1], Charset::Unknown, Some(&[]));
        if let Some(content) = signalled.content.as_mut() {
            content.fnc1 = Some(Fnc1Position::First);
        }
        declared.scan = Scan::Report {
            detections: vec![
                read("\u{104}", &[0xA1], Charset::Eci, Some(&[(4, 0)])),
                signalled,
            ],
            engine_panics: 0,
        };
        let verdict = verdict_of(declared.expected.as_deref(), &declared.scan, true);
        declared.verdict = verdict.raw;
        let rows = vec![
            zxing_row(&known_wrong, "photograph", &["photography"]),
            zxing_row(&double_qr, "outer", &["inner", "outer"]),
            declared,
            errored,
        ];
        let text = observed_jsonl(&rows).expect("emits");
        assert_eq!(text.lines().count(), 1 + rows.len());
        let observed = parse_observed(&text, OUR_DECODER).expect("parses back");
        for row in &rows[..3] {
            let image = &observed.records[&row.path];
            assert_eq!(image.sha256, row.image_sha256);
            let again = verdict_of(row.expected.as_deref(), &image.scan, false);
            assert_eq!(again.raw, row.verdict, "{}", row.path);
            let units = |d: &[Seen]| -> Vec<(Symbology, String, Option<Content>)> {
                d.iter()
                    .map(|s| (s.symbology, s.text.clone(), s.content.clone()))
                    .collect()
            };
            assert_eq!(
                units(detections_of(&image.scan)),
                units(detections_of(&row.scan))
            );
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

    // ------------------------------------------------- identity and unread

    /// The ECI 4 'Ą' truth (raw A1, eci [4@0]): read located [4@0] it is
    /// exact; signalled (eci []), unread; with no designator observed and
    /// read '¡' as latin1, wrong; 'Ą' over its UTF-8 bytes C4 84 meets
    /// nothing — equal text is not an identity; located as ECI 3 ('¡',
    /// latin1), wrong.
    #[test]
    fn an_eci4_truth_is_judged_by_identity() {
        let truth = [structural("\u{104}", &[0xA1], Some(&[(4, 0)]))];
        let verdict = |seen: Seen| judge_detections(&truth, &[seen]);
        let located = read("\u{104}", &[0xA1], Charset::Eci, Some(&[(4, 0)]));
        assert_eq!(verdict(located), Outcome::Exact);
        let signalled = read("\u{FFFD}", &[0xA1], Charset::Unknown, Some(&[]));
        assert_eq!(verdict(signalled), Outcome::Unread);
        let absent = read("\u{A1}", &[0xA1], Charset::Latin1, None);
        assert_eq!(verdict(absent), Outcome::Wrong);
        let utf8 = read("\u{104}", &[0xC4, 0x84], Charset::Utf8, None);
        assert_eq!(meet(&truth[0], &utf8), Meet::No, "equal text, other bytes");
        assert_eq!(verdict(utf8), Outcome::Wrong);
        let eci3 = read("\u{A1}", &[0xA1], Charset::Latin1, Some(&[(3, 0)]));
        assert_eq!(verdict(eci3), Outcome::Wrong);
    }

    /// The Kanji truth 山田 (8E 52 93 63, read `shift_jis`): the right bytes
    /// read 'ŽR“c' as latin1 are a wrong reading of the right content.
    #[test]
    fn a_kanji_truth_judges_the_reading_too() {
        let raw = [0x8E, 0x52, 0x93, 0x63];
        let truth = [structural("\u{5C71}\u{7530}", &raw, None)];
        let shift_jis = read("\u{5C71}\u{7530}", &raw, Charset::ShiftJis, None);
        assert_eq!(judge_detections(&truth, &[shift_jis]), Outcome::Exact);
        let latin1 = read("\u{17D}R\u{201C}c", &raw, Charset::Latin1, None);
        assert_eq!(judge_detections(&truth, &[latin1]), Outcome::Wrong);
    }

    /// A text truth is judged by text unless the detection's charset is
    /// `unknown`: 'Ą' read signalled over A1, and 茅 read signalled over
    /// C3 A9 (its GB 2312 bytes), are unread; the same C3 A9 read 'é' as
    /// utf8 is a wrong text.
    #[test]
    fn a_text_truth_is_unread_on_an_unknown_reading() {
        let signalled = read("\u{FFFD}", &[0xA1], Charset::Unknown, Some(&[]));
        assert_eq!(
            judge_detections(&[text_unit("\u{104}")], &[signalled]),
            Outcome::Unread
        );
        let signalled = read(
            "\u{FFFD}\u{FFFD}",
            &[0xC3, 0xA9],
            Charset::Unknown,
            Some(&[]),
        );
        assert_eq!(
            judge_detections(&[text_unit("\u{8305}")], &[signalled]),
            Outcome::Unread
        );
        let utf8 = read("\u{E9}", &[0xC3, 0xA9], Charset::Utf8, None);
        assert_eq!(
            judge_detections(&[text_unit("\u{8305}")], &[utf8]),
            Outcome::Wrong
        );
        // an unknown reading is never exact, even over the truth's text
        let lossy_equal = read("\u{104}", &[0xA1], Charset::Unknown, None);
        assert_eq!(meet(&text_unit("\u{104}"), &lossy_equal), Meet::Unread);
    }

    /// Declarations the truth contradicts never meet it, even unread: a
    /// signal or a located list where the symbol declares none, another
    /// FNC1 position, another located list. An `unknown` reading with no
    /// designator observed over the truth's bytes is unread.
    #[test]
    fn contradicting_declarations_do_not_meet() {
        let undeclared = [structural("A", b"A", None)];
        let signalled = read("\u{FFFD}", b"A", Charset::Unknown, Some(&[]));
        assert_eq!(judge_detections(&undeclared, &[signalled]), Outcome::Wrong);
        let located = read("A", b"A", Charset::Utf8, Some(&[(26, 0)]));
        assert_eq!(judge_detections(&undeclared, &[located]), Outcome::Wrong);
        let mut gs1 = plain(QrCode, "A");
        if let Some(content) = gs1.content.as_mut() {
            content.fnc1 = Some(Fnc1Position::First);
        }
        assert_eq!(judge_detections(&undeclared, &[gs1]), Outcome::Wrong);

        let declared = [structural("\u{104}", &[0xA1], Some(&[(4, 0)]))];
        let unobserved = read("\u{FFFD}", &[0xA1], Charset::Unknown, None);
        assert_eq!(judge_detections(&declared, &[unobserved]), Outcome::Unread);
        let other_list = read("\u{FFFD}", &[0xA1], Charset::Unknown, Some(&[(170, 0)]));
        assert_eq!(judge_detections(&declared, &[other_list]), Outcome::Wrong);
    }

    /// The grouping contract is content identity: equal text over other
    /// bytes is two contents; two identical detections, or a signalled
    /// declaration beside its located list, are one; an unobserved
    /// declaration or another FNC1 position is another content.
    #[test]
    fn the_contract_check_is_content_identity() {
        let declared = read("\u{104}", &[0xA1], Charset::Eci, Some(&[(4, 0)]));
        let utf8 = read("\u{104}", &[0xC4, 0x84], Charset::Utf8, None);
        assert!(
            contract_violations(&[declared.clone(), utf8]).is_empty(),
            "equal text, different raw"
        );
        assert_eq!(
            contract_violations(&[declared.clone(), declared.clone()]),
            ["detection #1 shares the identity of detection #0"]
        );
        let signalled = read("\u{FFFD}", &[0xA1], Charset::Unknown, Some(&[]));
        assert_eq!(
            contract_violations(&[declared.clone(), signalled.clone()]).len(),
            1,
            "[] is the content of the located list"
        );
        let unobserved = read("\u{A1}", &[0xA1], Charset::Latin1, None);
        assert!(contract_violations(&[declared.clone(), unobserved.clone()]).is_empty());
        assert!(contract_violations(&[signalled, unobserved]).is_empty());
        let mut second = declared.clone();
        if let Some(content) = second.content.as_mut() {
            content.fnc1 = Some(Fnc1Position::Second);
        }
        assert!(contract_violations(&[declared, second]).is_empty());
    }

    /// The matching keeps the most exact meets, then the most meets: here a
    /// unit-order greedy pass gives the shared exact read to the text unit
    /// and leaves the structural unit unmet (`mixed`); the best matching
    /// meets both (`unread`).
    #[test]
    fn the_matching_keeps_the_most_exact_then_the_most_meets() {
        let units = [text_unit("A"), structural("A", b"A", None)];
        let exact_for_both = plain(QrCode, "A");
        let unread_for_text = read("\u{FFFD}", &[0xFF], Charset::Unknown, None);
        let observed = [exact_for_both, unread_for_text];
        let (outcome, met) = judgment(&units, &observed);
        assert_eq!(outcome, Outcome::Unread);
        assert_eq!(met, [Some((1, Meet::Unread)), Some((0, Meet::Exact))]);
        // an unknown read beside the exact one is extra output, not unread
        let lone = [text_unit("A")];
        assert_eq!(judge_detections(&lone, &observed), Outcome::Extra);
        assert_eq!(
            best_matching(&[vec![Meet::Unread, Meet::Exact]], 2),
            [Some((1, Meet::Exact))]
        );
        assert_eq!(best_matching(&[vec![Meet::No]], 1), [None]);
    }

    /// `unread` is counted beside `exact` (its own rate), never blocks, and
    /// reaches the receipt with its Wilson interval and the unit's meet.
    #[test]
    fn unread_has_its_own_rate_and_never_blocks() {
        let [known_wrong, _] = DISPOSITIONS;
        let mut row = zxing_row(&known_wrong, "\u{104}", &[]);
        row.scan = Scan::Report {
            detections: vec![read("\u{FFFD}", &[0xA1], Charset::Unknown, Some(&[]))],
            engine_panics: 0,
        };
        let verdict = verdict_of(row.expected.as_deref(), &row.scan, true);
        (row.verdict, row.outcome) = (verdict.raw, verdict.outcome);
        assert_eq!(row.outcome, Outcome::Unread);
        let rows = [row];
        assert!(blockers(&Integrity::default(), &rows, &[]).is_empty());
        let (groups, sets) = tallies(&rows);
        let tally = sets[&Set::Zxing];
        assert_eq!((tally.truth, tally.exact(), tally.unread()), (1, 0, 1));
        let source = SourceEcho {
            git_sha: String::from("unknown"),
            tree: String::from("unknown"),
        };
        let mut receipt = new_receipt(source, "manifest", "vendored", Integrity::default());
        fill_receipt(&mut receipt, &rows, (groups, sets), &[], Vec::new());
        let json = serde_json::to_value(&receipt).expect("receipt serialises");
        assert_eq!(json["status"], "pass");
        assert_eq!(json["sets"][0]["outcomes"]["unread"], 1);
        assert_eq!(
            json["sets"][0]["unread_wilson95"],
            serde_json::json!(["0.207", "1.000"])
        );
        let unit = &json["rows"][0]["expected"][0];
        assert_eq!(unit["kind"], "text");
        assert_eq!(
            unit["met"],
            serde_json::json!({"detection": 0, "how": "unread"})
        );
        let detection = &json["rows"][0]["detections"][0];
        assert_eq!(
            (
                &detection["charset"],
                &detection["eci"],
                detection["raw_len"].as_u64()
            ),
            (
                &serde_json::json!("unknown"),
                &serde_json::json!([]),
                Some(1)
            )
        );
    }

    /// An observed decoder that reports text only meets a structural unit
    /// by the text rule — identity needs bytes on both sides.
    #[test]
    fn a_detection_without_bytes_is_held_to_the_text_rule() {
        let truth = [structural("\u{104}", &[0xA1], Some(&[(4, 0)]))];
        let text_only = Seen {
            symbology: QrCode,
            text: String::from("\u{104}"),
            content: None,
            engines: Vec::new(),
        };
        assert_eq!(
            judge_detections(&truth, std::slice::from_ref(&text_only)),
            Outcome::Exact
        );
        let other = Seen {
            text: String::from("\u{A1}"),
            ..text_only
        };
        assert_eq!(judge_detections(&truth, &[other]), Outcome::Wrong);
    }

    /// Hex in both directions, lowercase only.
    #[test]
    fn hex_round_trips() {
        assert_eq!(hex(&[0x00, 0xA1, 0xFF]), "00a1ff");
        assert_eq!(parse_hex("00a1ff"), Ok(vec![0x00, 0xA1, 0xFF]));
        assert_eq!(parse_hex(""), Ok(Vec::new()));
        for bad in ["0", "A1", "0g", "a 1"] {
            assert!(parse_hex(bad).is_err(), "{bad}");
        }
    }

    // ------------------------------------------------------- corpus.toml

    /// sha256 over one line per entry, in file order: path, category,
    /// expected, expect, source, NUL-separated, each `=value` or `!` when
    /// absent.
    fn entry_list_sha256(text: &str) -> String {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Keys {
            entry: Vec<EntryKeys>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct EntryKeys {
            path: String,
            category: String,
            expected: Option<String>,
            expect: Option<String>,
            source: Option<String>,
            #[serde(rename = "raw_hex")]
            _raw_hex: Option<String>,
            #[serde(rename = "eci")]
            _eci: Option<Vec<Eci>>,
            #[serde(rename = "fnc1")]
            _fnc1: Option<String>,
        }
        let field = |value: Option<&str>| value.map_or_else(|| "!".to_owned(), |v| format!("={v}"));
        let keys: Keys = toml::from_str(text).expect("corpus.toml parses with known keys only");
        let mut canon = String::new();
        for e in &keys.entry {
            let fields = [
                e.path.clone(),
                e.category.clone(),
                field(e.expected.as_deref()),
                field(e.expect.as_deref()),
                field(e.source.as_deref()),
            ];
            canon.push_str(&fields.join("\0"));
            canon.push('\n');
        }
        external::sha256_bytes(canon.as_bytes())
    }

    fn committed_corpus() -> String {
        let path = crate::repo_root().join(VENDORED);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// Pin the entry list independently of byte-truth fields. The only
    /// classification amendment is `FAIL_1491`: negative → unlabelled frontier.
    #[test]
    fn byte_truths_leave_the_entry_list_unchanged() {
        const BASE_ENTRY_LIST_SHA256: &str =
            "1d332b3e14d237d8239c15c8244758bcb3e71779d72fe1ef735e6c78fe358387";
        let text = committed_corpus();
        assert_eq!(entry_list_sha256(&text), BASE_ENTRY_LIST_SHA256);
        let corpus: crate::Corpus = toml::from_str(&text).expect("corpus.toml parses");
        assert_eq!(corpus.entry.len(), 41);
    }

    #[test]
    fn fail_1491_is_hash_pinned_unlabelled_and_never_a_correct_negative() {
        let text = committed_corpus();
        let corpus: crate::Corpus = toml::from_str(&text).expect("parses");
        let fixture = "fixtures/degraded/FAIL_1491ms_0_584c998c.png";
        let entry = corpus
            .entry
            .iter()
            .find(|e| e.path == fixture)
            .expect("entry");
        let bytes = std::fs::read(crate::repo_root().join(fixture)).expect("fixture");
        assert_eq!(
            external::sha256_bytes(&bytes),
            "dc8e895cffc2664db1e8aa879c2698f57ed7f11deef7c3194460b48033c265eb"
        );
        assert_eq!(entry.category, "frontier");
        assert_eq!(entry.expect.as_deref(), Some("fail"));
        assert_eq!(entry.expected, None);
        assert_eq!(entry.raw_hex, None);
        assert_eq!(vendored_expectation(entry), Ok(None));
        let manifest: toml::Value = toml::from_str(&text).expect("manifest");
        assert_eq!(
            manifest["entry"]
                .as_array()
                .expect("entries")
                .iter()
                .find(|e| e["path"].as_str() == Some(fixture))
                .expect("source")["source"]
                .as_str(),
            Some("v0.2 reference image — unlabelled frontier; WeChat observation, 2026-10-09")
        );

        // Synthetic observations exercise classification only; none supplies
        // truth for the fixture or claims to measure the current scanner.
        for (observed, state, outcome) in [
            (vec![], crate::EntryState::Held, Outcome::UnlabelledBlind),
            (
                vec![plain(Symbology::QrCode, "probe")],
                crate::EntryState::Gained,
                Outcome::UnlabelledDecoded,
            ),
        ] {
            assert_eq!(crate::entry_state(entry, &observed), (state, None));
            let scan = Scan::Report {
                detections: observed.clone(),
                engine_panics: 0,
            };
            let verdict = verdict_of(None, &scan, true);
            assert_eq!(verdict.raw, None);
            assert_eq!(verdict.outcome, outcome);
            assert_eq!(detections_of(&scan), observed);
        }
    }

    #[test]
    fn committed_frontiers_stay_in_receipts_outside_correctness_denominators() {
        let corpus: crate::Corpus = toml::from_str(&committed_corpus()).expect("parses");
        let truths = vendored_truths(&corpus).expect("truths");
        let root = crate::repo_root();
        let fixture = "fixtures/degraded/FAIL_1491ms_0_584c998c.png";
        for outputs in [vec![], vec![("qr_code", "probe")]] {
            let records: Vec<String> = corpus
                .entry
                .iter()
                .map(|entry| {
                    let sha256 = external::sha256_bytes(
                        &std::fs::read(root.join(&entry.path)).expect("image"),
                    );
                    record(
                        &entry.path,
                        &sha256,
                        if entry.path == fixture { &outputs } else { &[] },
                    )
                })
                .collect();
            let observed = parse_observed(&observed_file("synthetic", &records), "synthetic")
                .expect("records");
            let mut integrity = Integrity::default();
            let rows = evaluate_vendored(
                &root,
                &corpus,
                &truths,
                Source::Observed(&observed),
                &mut integrity,
            );
            assert_eq!(integrity.problems, Vec::<String>::new());
            assert_eq!(integrity.vendored_read, 41);
            let (groups, sets) = tallies(&rows);
            let tally = &sets[&Set::Vendored];
            assert_eq!(
                (tally.images, tally.truth, tally.negatives, tally.unlabelled),
                (41, 34, 0, 7)
            );
            assert_eq!(tally.outcomes.get(Outcome::Missed), 34);
            for outcome in [
                Outcome::Exact,
                Outcome::Wrong,
                Outcome::FalsePositive,
                Outcome::NegativeHeld,
            ] {
                assert_eq!(tally.outcomes.get(outcome), 0);
            }
            let decoded = u32::from(!outputs.is_empty());
            assert_eq!(tally.outcomes.get(Outcome::UnlabelledDecoded), decoded);
            assert_eq!(tally.outcomes.get(Outcome::UnlabelledBlind), 7 - decoded);
            let frontier = &groups[&(Set::Vendored, String::from("vendored/frontier"))];
            assert_eq!(
                (
                    frontier.images,
                    frontier.truth,
                    frontier.negatives,
                    frontier.unlabelled
                ),
                (7, 0, 0, 7)
            );
            let row = rows
                .iter()
                .find(|row| row.path == fixture)
                .expect("row retained");
            let receipt = serde_json::to_value(row_receipt(row)).expect("receipt");
            assert_eq!(receipt["expected"], serde_json::Value::Null);
            assert_eq!(receipt["truth_sha256"], serde_json::Value::Null);
            assert_eq!(receipt["verdict"], serde_json::Value::Null);
            assert_eq!(
                receipt["outcome"],
                if outputs.is_empty() {
                    "unlabelled_blind"
                } else {
                    "unlabelled_decoded"
                }
            );
            assert_eq!(
                receipt["detections"].as_array().expect("detections").len(),
                outputs.len()
            );
            if !outputs.is_empty() {
                assert_eq!(
                    receipt["detections"][0]["text_sha256"],
                    external::sha256_bytes(b"probe")
                );
                assert_eq!(receipt["detections"][0]["text_len"], 5);
            }
        }
    }

    /// The committed byte truths: every one is the UTF-8 encoding of its
    /// text (ASCII payloads, one GS-led element string) and declares no
    /// ECI and no FNC1 — none of these symbols does. The entries with a
    /// text and no bytes are exactly the three no independent reader
    /// decodes.
    #[test]
    fn byte_truths_are_the_bytes_of_their_texts() {
        let corpus: crate::Corpus = toml::from_str(&committed_corpus()).expect("parses");
        let truths = vendored_truths(&corpus).expect("every truth is well formed");
        let mut structural = 0;
        let mut text_only = Vec::new();
        for (entry, expected) in corpus.entry.iter().zip(&truths) {
            for unit in expected.iter().flatten() {
                match &unit.identity {
                    Some(identity) => {
                        structural += 1;
                        assert_eq!(identity.raw, unit.unit.text.as_bytes(), "{}", entry.path);
                        assert_eq!(
                            (&identity.eci, identity.fnc1),
                            (&None, None),
                            "{}",
                            entry.path
                        );
                    }
                    None => text_only.push(entry.path.as_str()),
                }
            }
        }
        assert_eq!(structural, 31);
        assert_eq!(
            text_only,
            [
                "fixtures/artistic/OK_1069ms_85_8b6a54b3.png",
                "fixtures/artistic/blob-style-monkey-logo.webp",
                "fixtures/degraded/logo-occluded-rescue.png",
            ]
        );
    }
}
