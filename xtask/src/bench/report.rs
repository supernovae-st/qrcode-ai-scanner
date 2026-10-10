//! Receipts: `receipt.json` (summaries, verdicts, parity, gate log,
//! environment), `receipt.md` (the short tables), `samples.jsonl`
//! (every completed call) and `images.jsonl` (per-image summaries), written
//! once into a fresh run directory — every file created new, its sha256
//! printed. Payloads appear as symbology + text sha256 + length only;
//! paths are corpus-relative.
//!
//! Statistics follow `bench/README.md`: a resource failure fails
//! its class; failed calls never enter latency, memory, overrun or
//! throughput figures; a pair enters a paired test only when both sides
//! completed every call without error, with stable and agreeing outputs.
//! In a budgeted mode a call is truncated when its walk or judgment differs
//! from the image's unbudgeted reference walk; latency is judged on the
//! valid pairs whose median calls are complete on both sides, truncation
//! and lost judgments by exact one-sided `McNemar` tests over the valid
//! pairs, work on the pairs where a side was truncated. Memory is judged in
//! full-unbounded only.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::proc::Failure;
use super::run::{
    COLD_PAIRS, CallFailure, ENVELOPE, Image, Measurement, Mode, Outcome, ProcessEnd, Runtime,
    Sample, Segment, Set, Throughput, Variant, WasmCost, ms, signature, thread_counts,
};
use super::stats::{self, McNemar, Shift, Verdict};

/// Benchmark-harness identifier, bumped with any method change.
pub(crate) const BENCH_ID: &str = "qrscan-bench/v3";
/// The protocol version the method implements.
pub(crate) const PROTOCOL: &str = "qrscan-oracle/v1.4";
/// δ table: fractions, and the size δ in basis points (sizes are exact
/// integers, so their verdict is too).
pub(crate) const DELTA_P50: f64 = 0.05;
pub(crate) const DELTA_P95: f64 = 0.10;
pub(crate) const DELTA_MEMORY: f64 = 0.10;
pub(crate) const DELTA_SIZE_BP: u64 = 200;
pub(crate) const DELTA_COLD_START: f64 = 0.10;
pub(crate) const DELTA_WORK: f64 = 0.05;

fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte and call counts stay far below 2^52"
)]
fn float(x: u64) -> f64 {
    x as f64
}

fn timed(samples: &[Sample]) -> impl Iterator<Item = &Sample> {
    samples.iter().filter(|s| s.rep > 0)
}

/// What a completed call produced, as one comparable string.
fn output_of(sample: &Sample) -> String {
    match (&sample.units, &sample.error) {
        (Some(units), _) => signature(units),
        (None, Some(code)) => format!("error:{code}"),
        (None, None) => String::from("error"),
    }
}

/// Per-image facts of one variant.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent per-image facts, each read on its own"
)]
struct ImageSide {
    /// Timed calls completed (an `ok` or an `error` reply).
    timed: usize,
    /// Timed calls that returned a report.
    ok: usize,
    /// Error replies of every call, the warm-up included.
    errors: usize,
    /// Resource failures of this image and variant: its reference walk,
    /// its warm-up and its timed calls.
    failures: usize,
    median_ms: Option<f64>,
    median_total_ms: Option<f64>,
    output: Option<String>,
    /// One output over every completed call, the warm-up included.
    stable: bool,
    heap_peak: Option<u64>,
    alloc_calls: Option<u64>,
    retained: Option<i64>,
    median_rss: Option<f64>,
    /// Calls above the memory envelope (heap or process RSS).
    over_envelope: usize,
    /// Median Σ transforms of the timed calls.
    transforms: Option<f64>,
    /// Budgeted modes: whether the image's reference walk came back.
    reference: bool,
    /// Timed calls truncated — their walk or judgment differs from the
    /// reference's — and, of every timed call, those whose walk differs,
    /// whose judgment differs, and whose ladder reached the budget (an
    /// overrun: information).
    truncated: usize,
    walk_cut: usize,
    judgment_cut: usize,
    reached: usize,
    /// Timed calls slower than the preset budget.
    over_budget_calls: usize,
    /// Whether the call(s) at the median latency were truncated, or lost
    /// their judgment.
    median_truncated: bool,
    median_judgment_cut: bool,
}

/// The call(s) the per-image median latency comes from: the middle one
/// of the completed timed calls by time (ties by slot), the middle two
/// for an even count.
fn median_calls<'a>(ok: &[&'a Sample]) -> Vec<&'a Sample> {
    let mut sorted = ok.to_vec();
    sorted.sort_by_key(|s| (s.ns, s.slot));
    let n = sorted.len();
    match n {
        0 => Vec::new(),
        _ if n % 2 == 1 => vec![sorted[n / 2]],
        _ => vec![sorted[n / 2 - 1], sorted[n / 2]],
    }
}

fn image_side(
    samples: &[Sample],
    reference: Option<&Sample>,
    failures: usize,
    mode: Mode,
) -> ImageSide {
    let ok: Vec<&Sample> = timed(samples).filter(|s| s.error.is_none()).collect();
    let ms_values: Vec<f64> = ok.iter().map(|s| ms(s.ns)).collect();
    let totals: Vec<f64> = ok.iter().filter_map(|s| s.total_ms).collect();
    let outputs: Vec<String> = samples
        .iter()
        .filter(|s| s.error.is_none())
        .map(output_of)
        .collect();
    let rss: Vec<f64> = ok.iter().filter_map(|s| s.rss).map(float).collect();
    let transforms: Vec<f64> = ok
        .iter()
        .filter_map(|s| s.work.as_ref().map(|w| float(w.transforms)))
        .collect();
    let warm = samples.iter().find(|s| s.rep == 0 && s.error.is_none());
    let over_envelope = samples
        .iter()
        .filter(|s| {
            s.rss.is_some_and(|r| r > ENVELOPE) || s.heap.is_some_and(|h| h.peak > ENVELOPE)
        })
        .count();
    let budget = mode.budget_ms().map(|b| ms(b * 1_000_000));
    let reference = reference.filter(|_| mode.budgeted());
    let truncated = |s: &Sample| reference.is_some_and(|r| s.truncated(r, mode));
    let judged = |s: &Sample| reference.is_some_and(|r| s.judgment_cut(r, mode));
    let middle = median_calls(&ok);
    ImageSide {
        timed: timed(samples).count(),
        ok: ok.len(),
        errors: samples.iter().filter(|s| s.error.is_some()).count(),
        failures,
        median_ms: stats::median(&ms_values),
        median_total_ms: stats::median(&totals),
        output: timed(samples)
            .find(|s| s.error.is_none())
            .or(warm)
            .map(output_of),
        stable: outputs.windows(2).all(|w| w[0] == w[1]),
        heap_peak: warm.and_then(|s| s.heap).map(|h| h.peak),
        alloc_calls: warm.and_then(|s| s.heap).map(|h| h.calls),
        retained: warm.and_then(|s| s.retained),
        median_rss: stats::median(&rss),
        over_envelope,
        transforms: stats::median(&transforms),
        reference: reference.is_some(),
        truncated: ok.iter().filter(|s| truncated(s)).count(),
        walk_cut: ok
            .iter()
            .filter(|s| reference.is_some_and(|r| s.walk_cut(r)))
            .count(),
        judgment_cut: ok.iter().filter(|s| judged(s)).count(),
        reached: ok.iter().filter(|s| s.reached_budget(mode)).count(),
        over_budget_calls: budget.map_or(0, |b| ms_values.iter().filter(|&&v| v > b).count()),
        median_truncated: middle.iter().any(|s| truncated(s)),
        median_judgment_cut: middle.iter().any(|s| judged(s)),
    }
}

impl ImageSide {
    /// Every call completed without error, nothing failed.
    fn complete(&self, reps: u32) -> bool {
        self.timed == reps as usize && self.errors == 0 && self.failures == 0
    }
}

/// Why a pair is kept out of the paired tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Excluded {
    Failure,
    Error,
    Incomplete,
    /// A budgeted mode without the reference walk to read truncation from.
    NoReference,
    Unstable,
    Disagreement,
}

impl Excluded {
    fn as_str(self) -> &'static str {
        match self {
            Self::Failure => "resource_failure",
            Self::Error => "error",
            Self::Incomplete => "incomplete",
            Self::NoReference => "no_reference",
            Self::Unstable => "unstable",
            Self::Disagreement => "disagreement",
        }
    }
}

fn pair_status(a: &ImageSide, b: &ImageSide, reps: u32, mode: Mode) -> Result<(), Excluded> {
    if a.failures + b.failures > 0 {
        Err(Excluded::Failure)
    } else if a.errors + b.errors > 0 {
        Err(Excluded::Error)
    } else if !a.complete(reps) || !b.complete(reps) {
        Err(Excluded::Incomplete)
    } else if mode.budgeted() && !(a.reference && b.reference) {
        Err(Excluded::NoReference)
    } else if !a.stable || !b.stable {
        Err(Excluded::Unstable)
    } else if a.output != b.output {
        Err(Excluded::Disagreement)
    } else {
        Ok(())
    }
}

type Rows = Vec<(usize, ImageSide)>;

/// One valid pair: image index, base side, candidate side.
type Pair<'a> = (usize, &'a ImageSide, &'a ImageSide);

/// The valid pairs of a class and the excluded images by reason.
fn pairs_of<'a>(
    (base, candidate): (&'a Rows, &'a Rows),
    reps: u32,
    mode: Mode,
) -> (Vec<Pair<'a>>, BTreeMap<&'static str, usize>) {
    let by_image: BTreeMap<usize, &ImageSide> = candidate.iter().map(|(i, s)| (*i, s)).collect();
    let in_base: BTreeSet<usize> = base.iter().map(|(i, _)| *i).collect();
    let mut excluded: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut valid = Vec::new();
    for (index, a) in base {
        let Some(b) = by_image.get(index) else {
            *excluded.entry(Excluded::Incomplete.as_str()).or_default() += 1;
            continue;
        };
        match pair_status(a, b, reps, mode) {
            Ok(()) => valid.push((*index, a, *b)),
            Err(why) => *excluded.entry(why.as_str()).or_default() += 1,
        }
    }
    let candidate_only = by_image.keys().filter(|i| !in_base.contains(i)).count();
    if candidate_only > 0 {
        *excluded.entry(Excluded::Incomplete.as_str()).or_default() += candidate_only;
    }
    (valid, excluded)
}

/// A class: an oracle group or a whole set.
struct Class {
    name: String,
    level: &'static str,
    members: Vec<usize>,
}

fn classes(images: &[Image]) -> Vec<Class> {
    let mut groups: BTreeMap<(Set, String), Vec<usize>> = BTreeMap::new();
    let mut sets: BTreeMap<Set, Vec<usize>> = BTreeMap::new();
    for (index, image) in images.iter().enumerate() {
        groups
            .entry((image.set, image.group.clone()))
            .or_default()
            .push(index);
        sets.entry(image.set).or_default().push(index);
    }
    let groups = groups.into_iter().map(|((_, name), members)| Class {
        name,
        level: "group",
        members,
    });
    let sets = sets.into_iter().map(|(set, members)| Class {
        name: set.as_str().to_owned(),
        level: "set",
        members,
    });
    groups.chain(sets).collect()
}

fn wilson_json(k: u64, n: u64) -> Value {
    stats::wilson95(k, n).map_or(Value::Null, |[lower, upper]| json!([r3(lower), r3(upper)]))
}

fn spread_json(values: &[f64]) -> Value {
    stats::spread(values).map_or(
        Value::Null,
        |s| json!({"n": s.n, "p50": r3(s.p50), "p95": r3(s.p95), "max": r3(s.max)}),
    )
}

fn shift_json(shift: Option<&Shift>) -> Value {
    shift.map_or(Value::Null, |s| {
        json!({
            "pairs": s.n,
            "hl_pct": r3(stats::pct_change(s.estimate)),
            "ci95_pct": s.interval.map(|[l, u]| [r3(stats::pct_change(l)), r3(stats::pct_change(u))]),
            "confidence": s.confidence.map(r3),
        })
    })
}

/// The label of a verdict: a failed class and a dry run override the
/// statistic (no verdict is drawn from a run the gate did not hold).
fn label(verdict: Verdict, failed: bool, exploratory: bool) -> &'static str {
    if failed {
        Verdict::Failed.as_str()
    } else if exploratory {
        "exploratory"
    } else {
        verdict.as_str()
    }
}

/// One declared result line for the verdict table.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Declared {
    pub(crate) target: String,
    pub(crate) class: String,
    pub(crate) metric: &'static str,
    pub(crate) verdict: &'static str,
    pub(crate) estimate_pct: Option<f64>,
    pub(crate) ci_pct: Option<[f64; 2]>,
    /// [`McNemar`] rows: the one-sided p and the discordant images, [only
    /// the candidate, only the base].
    pub(crate) p_value: Option<f64>,
    pub(crate) discordant: Option<[usize; 2]>,
    /// Why a row is not testable when nothing was measured for it.
    pub(crate) note: Option<String>,
}

fn declare(
    target: &str,
    class: &str,
    metric: &'static str,
    shift: Option<&Shift>,
    verdict: &'static str,
) -> Declared {
    Declared {
        target: target.to_owned(),
        class: class.to_owned(),
        metric,
        verdict,
        estimate_pct: shift.map(|s| r3(stats::pct_change(s.estimate))),
        ci_pct: shift
            .and_then(|s| s.interval)
            .map(|[l, u]| [r3(stats::pct_change(l)), r3(stats::pct_change(u))]),
        p_value: None,
        discordant: None,
        note: None,
    }
}

fn declare_test(
    target: &str,
    class: &str,
    metric: &'static str,
    test: &McNemar,
    verdict: &'static str,
) -> Declared {
    Declared {
        p_value: Some(test.p),
        discordant: Some([test.against, test.favour]),
        ..declare(target, class, metric, None, verdict)
    }
}

/// The δ metrics a (runtime, mode) target declares per class.
pub(crate) fn metrics(runtime: Runtime, mode: Mode) -> Vec<&'static str> {
    let mut list = vec!["latency_p50", "latency_p95_tail"];
    if mode.budgeted() {
        list.push("truncation");
        if mode.scores() {
            list.push("lost_judgment");
        }
        list.push("work");
    } else {
        list.extend(match runtime {
            Runtime::Lib => ["peak_heap_p50", "peak_heap_tail"].as_slice(),
            Runtime::Cli => ["peak_rss_p50", "peak_rss_tail"].as_slice(),
            Runtime::Node | Runtime::Wasm => ["peak_memory"].as_slice(),
        });
    }
    list
}

/// A signed percentage for a table cell, one decimal, never `-0.0`.
fn pct_cell(pct: f64) -> String {
    let rounded = (pct * 10.0).round() / 10.0 + 0.0;
    format!("{rounded:+.1}")
}

fn md_shift(shift: Option<&Shift>) -> String {
    shift.map_or_else(
        || String::from("—"),
        |s| match s.interval {
            Some([l, u]) => format!(
                "{} % [{}, {}]",
                pct_cell(stats::pct_change(s.estimate)),
                pct_cell(stats::pct_change(l)),
                pct_cell(stats::pct_change(u))
            ),
            None => format!(
                "{} % (n={}, no CI)",
                pct_cell(stats::pct_change(s.estimate)),
                s.n
            ),
        },
    )
}

fn md_ms(values: &[f64]) -> String {
    stats::spread(values).map_or_else(
        || String::from("— | — | —"),
        |s| format!("{:.1} | {:.1} | {:.1}", s.p50, s.p95, s.max),
    )
}

/// Everything one class of one segment yields.
struct ClassResult {
    json: Value,
    latency_md: String,
    budget_md: Option<String>,
    memory_md: Option<String>,
}

/// Figures of one variant over a set of its images: latency, trace,
/// overruns, the budget facts, transforms and memory spreads.
fn figures(sides: &[&ImageSide], mode: Mode) -> (Value, Vec<f64>) {
    let pick = |f: &dyn Fn(&ImageSide) -> Option<f64>| -> Vec<f64> {
        sides.iter().filter_map(|s| f(s)).collect()
    };
    let latency = pick(&|s| s.median_ms);
    let totals = pick(&|s| s.median_total_ms);
    let heap = pick(&|s| s.heap_peak.map(float));
    let allocs = pick(&|s| s.alloc_calls.map(float));
    #[expect(
        clippy::cast_precision_loss,
        reason = "retained bytes stay far below 2^52"
    )]
    let retained = pick(&|s| s.retained.map(|r| r as f64));
    let rss = pick(&|s| s.median_rss);
    let transforms = pick(&|s| s.transforms);
    let sum = |f: fn(&ImageSide) -> usize| sides.iter().map(|s| f(s)).sum::<usize>();
    let count = |f: fn(&ImageSide) -> bool| sides.iter().filter(|s| f(s)).count();
    let calls = sum(|s| s.ok);
    let images = sides.len() as u64;
    let overruns = mode.budget_ms().map(|budget| {
        let limit = ms(budget * 1_000_000);
        let over = sides
            .iter()
            .filter(|s| s.median_ms.is_some_and(|m| m > limit))
            .count() as u64;
        json!({"budget_ms": budget, "images_over": over, "images": images,
               "wilson95": wilson_json(over, images),
               "calls_over": sum(|s| s.over_budget_calls), "calls": calls,
               "calls_note": "call-level count, clustered by image: no interval"})
    });
    let budget = mode.budgeted().then(|| {
        json!({"calls": calls, "truncated": sum(|s| s.truncated), "walk_cut": sum(|s| s.walk_cut),
               "judgment_cut": sum(|s| s.judgment_cut), "reached_budget": sum(|s| s.reached),
               "images_median_truncated": count(|s| s.median_truncated),
               "images_median_judgment_cut": count(|s| s.median_judgment_cut)})
    });
    let json = json!({
        "images": images,
        "latency_ms": spread_json(&latency),
        "trace_total_ms": spread_json(&totals),
        "overruns": overruns,
        "budget": budget,
        "transforms": spread_json(&transforms),
        "peak_heap_bytes": spread_json(&heap),
        "alloc_calls": spread_json(&allocs),
        "retained_bytes": spread_json(&retained),
        "peak_rss_bytes": spread_json(&rss),
    });
    (json, latency)
}

/// The per-variant summary of a class. With a comparison its figures cover
/// the valid pairs — the verdict set — and `all_complete` keeps the figures
/// of every image the variant completed; a variant without a partner reads
/// its complete images.
fn side_json(
    rows: &Rows,
    mode: Mode,
    reps: u32,
    valid: Option<&BTreeSet<usize>>,
) -> (Value, Vec<f64>) {
    let complete: Vec<&ImageSide> = rows
        .iter()
        .map(|(_, side)| side)
        .filter(|s| s.complete(reps))
        .collect();
    let (main, over): (Vec<&ImageSide>, &str) = match valid {
        Some(valid) => (
            rows.iter()
                .filter(|(i, _)| valid.contains(i))
                .map(|(_, side)| side)
                .collect(),
            "valid pairs",
        ),
        None => (complete.clone(), "complete images"),
    };
    let (mut json, latency) = figures(&main, mode);
    let sum = |f: fn(&ImageSide) -> usize| rows.iter().map(|(_, s)| f(s)).sum::<usize>();
    json["over"] = json!(over);
    json["images_measured"] = json!(rows.len());
    json["complete"] = json!(complete.len());
    json["errors"] = json!(sum(|s| s.errors));
    json["resource_failures"] = json!(sum(|s| s.failures));
    json["stable_outputs"] = json!(rows.iter().filter(|(_, s)| s.stable).count());
    json["over_envelope_calls"] = json!(sum(|s| s.over_envelope));
    if valid.is_some() {
        json["all_complete"] = figures(&complete, mode).0;
    }
    (json, latency)
}

/// What a comparison adds to the Markdown tables: its cells of the
/// latency, budget and memory rows.
struct Compared {
    latency: String,
    budget: Option<String>,
    memory: Option<String>,
}

fn shift_of(pairs: &[(f64, f64)]) -> Option<Shift> {
    stats::hodges_lehmann(&stats::log_ratios(pairs))
}

fn mcnemar_json(test: &McNemar, verdict: &str) -> Value {
    json!({"only_candidate": test.against, "only_base": test.favour, "p_one_sided": test.p,
           "alpha": stats::ALPHA, "verdict": verdict})
}

fn md_mcnemar(test: &McNemar, verdict: &str) -> String {
    format!(
        "p {:.4} ({} B only / {} A only) {verdict}",
        test.p, test.against, test.favour
    )
}

/// Base versus candidate on the valid pairs of one class: latency on the
/// pairs whose median calls are complete on both sides; in budgeted modes
/// truncation and lost judgments by exact one-sided [`McNemar`] tests over
/// every valid pair, and work on the pairs where a side was truncated;
/// memory judged in full-unbounded only.
#[allow(
    clippy::too_many_lines,
    reason = "one class's verdicts read best in one place"
)]
fn compare(
    segment: &Segment,
    class: &Class,
    (valid, excluded): (&[Pair<'_>], &BTreeMap<&'static str, usize>),
    (exploratory, failed): (bool, bool),
    declared: &mut Vec<Declared>,
    json: &mut Value,
) -> Compared {
    let target = format!("{}/{}", segment.runtime.as_str(), segment.mode.as_str());
    let name = &class.name;
    let mode = segment.mode;
    let values = |set: &[&Pair<'_>], f: &dyn Fn(&ImageSide) -> Option<f64>| -> Vec<(f64, f64)> {
        set.iter()
            .filter_map(|(_, a, b)| Some((f(a)?, f(b)?)))
            .collect()
    };
    let all: Vec<&Pair<'_>> = valid.iter().collect();
    let (complete, truncated): (Vec<&Pair<'_>>, Vec<&Pair<'_>>) = valid
        .iter()
        .partition(|(_, a, b)| !a.median_truncated && !b.median_truncated);

    let latency = values(&complete, &|s| s.median_ms);
    let shift = shift_of(&latency);
    let tail = stats::tail_pairs(&latency);
    let tail_shift = shift_of(&tail);
    let p50 = label(
        stats::verdict(shift.as_ref(), DELTA_P50),
        failed,
        exploratory,
    );
    let p95 = label(
        stats::verdict(tail_shift.as_ref(), DELTA_P95),
        failed,
        exploratory,
    );
    declared.push(declare(&target, name, "latency_p50", shift.as_ref(), p50));
    declared.push(declare(
        &target,
        name,
        "latency_p95_tail",
        tail_shift.as_ref(),
        p95,
    ));
    let excluded_total: usize = excluded.values().sum();
    json["pairs"] = json!({"valid": valid.len(), "excluded": excluded, "excluded_total": excluded_total,
                           "complete": complete.len(), "truncated": truncated.len()});
    json["failed"] = json!(failed);
    json["latency"] = json!({
        "pairs": complete.len(),
        "over": "the valid pairs whose median calls are complete on both sides",
        "p50": shift_json(shift.as_ref()), "p50_verdict": p50,
        "p95_tail": shift_json(tail_shift.as_ref()), "p95_tail_pairs": tail.len(), "p95_verdict": p95,
        "nonpositive_dropped": stats::nonpositive(&latency),
    });

    let budget_md = mode.budgeted().then(|| {
        let mcnemar = |status: fn(&ImageSide) -> bool| {
            stats::mcnemar(
                valid.iter().filter(|(_, a, b)| !status(a) && status(b)).count(),
                valid.iter().filter(|(_, a, b)| status(a) && !status(b)).count(),
            )
        };
        let tested = |test: &McNemar| {
            let verdict = if valid.len() < stats::MIN_PAIRS {
                Verdict::NotTestable
            } else if test.significant {
                Verdict::Regression
            } else {
                Verdict::NoRegression
            };
            label(verdict, failed, exploratory)
        };
        let cut = mcnemar(|s| s.median_truncated);
        let cut_verdict = tested(&cut);
        declared.push(declare_test(&target, name, "truncation", &cut, cut_verdict));
        json["budget_bound"] = json!({
            "pairs": truncated.len(), "of_valid": valid.len(),
            "note": "valid pairs whose median call is truncated on a side: their latency is budget-bound, so they are judged on truncation, judgments and work, never on latency",
        });
        json["truncation"] = mcnemar_json(&cut, cut_verdict);
        let judgments = mode.scores().then(|| {
            let test = mcnemar(|s| s.median_judgment_cut);
            let verdict = tested(&test);
            declared.push(declare_test(&target, name, "lost_judgment", &test, verdict));
            json["lost_judgment"] = mcnemar_json(&test, verdict);
            md_mcnemar(&test, verdict)
        });
        let work = values(&truncated, &|s| s.transforms);
        let work_shift = shift_of(&work);
        let work_verdict = label(
            stats::work_verdict(work_shift.as_ref(), DELTA_WORK),
            failed,
            exploratory,
        );
        declared.push(declare(&target, name, "work", work_shift.as_ref(), work_verdict));
        json["work"] = json!({
            "pairs": truncated.len(),
            "over": "the valid pairs where a side was truncated",
            "shift": shift_json(work_shift.as_ref()), "verdict": work_verdict,
            "nonpositive_dropped": stats::nonpositive(&work),
        });
        let images = |pick: fn(&Pair<'_>) -> bool| valid.iter().filter(|p| pick(p)).count();
        format!(
            "{}/{} | {}/{} | {} | {} | {} | {} | {work_verdict}",
            images(|(_, a, _)| a.median_truncated),
            valid.len(),
            images(|(_, _, b)| b.median_truncated),
            valid.len(),
            truncated.len(),
            md_mcnemar(&cut, cut_verdict),
            judgments.unwrap_or_else(|| String::from("—")),
            md_shift(work_shift.as_ref()),
        )
    });

    let judged_memory = mode == Mode::FullUnbounded;
    let memory_metric = match segment.runtime {
        Runtime::Lib => Some((
            "peak_heap_p50",
            "peak_heap_tail",
            values(&all, &|s| s.heap_peak.map(float)),
        )),
        Runtime::Cli => Some((
            "peak_rss_p50",
            "peak_rss_tail",
            values(&all, &|s| s.median_rss),
        )),
        Runtime::Node | Runtime::Wasm => None,
    };
    let memory_md = if let Some((p50_metric, tail_metric, memory)) = memory_metric {
        let mem_shift = shift_of(&memory);
        let mem_tail = stats::tail_pairs(&memory);
        let tail_mem_shift = shift_of(&mem_tail);
        if judged_memory {
            let v50 = label(
                stats::verdict(mem_shift.as_ref(), DELTA_MEMORY),
                failed,
                exploratory,
            );
            let vtail = label(
                stats::verdict(tail_mem_shift.as_ref(), DELTA_MEMORY),
                failed,
                exploratory,
            );
            declared.push(declare(&target, name, p50_metric, mem_shift.as_ref(), v50));
            declared.push(declare(
                &target,
                name,
                tail_metric,
                tail_mem_shift.as_ref(),
                vtail,
            ));
            json["memory"] = json!({
                "p50": {"metric": p50_metric, "shift": shift_json(mem_shift.as_ref()), "verdict": v50},
                "tail": {"metric": tail_metric, "pairs": mem_tail.len(), "shift": shift_json(tail_mem_shift.as_ref()), "verdict": vtail},
                "nonpositive_dropped": stats::nonpositive(&memory),
            });
            Some(format!(
                "{} | {v50} | {} | {vtail}",
                md_shift(mem_shift.as_ref()),
                md_shift(tail_mem_shift.as_ref())
            ))
        } else {
            json["memory"] = json!({
                "p50": {"metric": p50_metric, "shift": shift_json(mem_shift.as_ref())},
                "tail": {"metric": tail_metric, "pairs": mem_tail.len(), "shift": shift_json(tail_mem_shift.as_ref())},
                "nonpositive_dropped": stats::nonpositive(&memory),
                "note": "information: in a budgeted mode the walk decides the memory, so memory is judged in full-unbounded only",
            });
            None
        }
    } else {
        if judged_memory {
            let verdict = label(Verdict::NotTestable, failed, exploratory);
            declared.push(declare(&target, name, "peak_memory", None, verdict));
            json["memory"] = json!({"metric": "peak_memory", "verdict": verdict,
                                    "note": "per-image memory is not testable for node and wasm; see the worker processes"});
        }
        None
    };
    let allocs = values(&all, &|s| s.alloc_calls.map(float));
    if !allocs.is_empty() {
        json["alloc_calls"] = shift_json(shift_of(&allocs).as_ref());
    }
    Compared {
        latency: format!(
            "{excluded_total} | {}/{} | {} | {p50} | {p95}",
            complete.len(),
            valid.len(),
            md_shift(shift.as_ref())
        ),
        budget: budget_md,
        memory: memory_md,
    }
}

/// The per-image sides of a class in a segment, per variant that measured
/// any of its images.
fn class_sides(segment: &Segment, class: &Class) -> BTreeMap<Variant, Rows> {
    let mode = segment.mode;
    Variant::ALL
        .iter()
        .map(|&v| {
            let rows: Rows = class
                .members
                .iter()
                .filter_map(|i| {
                    let failures = segment
                        .failures
                        .iter()
                        .filter(|f| f.image == *i && f.variant == v)
                        .count();
                    let reference = segment.references.get(i).and_then(|m| m.get(&v));
                    let samples = segment.samples.get(i).and_then(|m| m.get(&v));
                    match samples {
                        Some(samples) => Some((*i, image_side(samples, reference, failures, mode))),
                        None if failures > 0 => {
                            Some((*i, image_side(&[], reference, failures, mode)))
                        }
                        None => None,
                    }
                })
                .collect();
            (v, rows)
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// Summaries of one class: each variant's side, then the comparison.
fn class_row(
    segment: &Segment,
    class: &Class,
    (exploratory, reps): (bool, u32),
    declared: &mut Vec<Declared>,
) -> ClassResult {
    let mode = segment.mode;
    let per_variant = class_sides(segment, class);
    let mut json =
        json!({"class": class.name, "level": class.level, "images": class.members.len()});
    let failed = class
        .members
        .iter()
        .any(|i| segment.failures.iter().any(|f| f.image == *i));
    let both = per_variant
        .get(&Variant::Base)
        .zip(per_variant.get(&Variant::Candidate));
    let pairing = both.map(|sides| pairs_of(sides, reps, mode));
    let valid_set: Option<BTreeSet<usize>> = pairing
        .as_ref()
        .map(|(valid, _)| valid.iter().map(|(i, _, _)| *i).collect());
    let mut md_sides = String::new();
    for variant in Variant::ALL {
        let Some(rows) = per_variant.get(variant) else {
            md_sides.push_str(" | — | — | —");
            continue;
        };
        let (side, latency) = side_json(rows, mode, reps, valid_set.as_ref());
        json[variant.as_str()] = side;
        let _ = write!(md_sides, " | {}", md_ms(&latency));
    }
    let compared = if let (Some((valid, excluded)), Some((base, candidate))) = (&pairing, both) {
        // A-versus-B output agreement over every image both measured
        // (information; the excluded counts carry the disagreements)
        let outputs: BTreeMap<usize, &Option<String>> =
            candidate.iter().map(|(i, s)| (*i, &s.output)).collect();
        let compared = base.iter().filter(|(i, _)| outputs.contains_key(i)).count() as u64;
        let agree = base
            .iter()
            .filter(|(i, a)| outputs.get(i).is_some_and(|b| **b == a.output))
            .count() as u64;
        json["agreement"] =
            json!({"k": agree, "n": compared, "wilson95": wilson_json(agree, compared)});
        compare(
            segment,
            class,
            (valid, excluded),
            (exploratory, failed),
            declared,
            &mut json,
        )
    } else {
        // one variant: every δ metric is declared, not testable
        let target = format!("{}/{}", segment.runtime.as_str(), mode.as_str());
        let verdict = label(Verdict::NotTestable, failed, exploratory);
        for metric in metrics(segment.runtime, mode) {
            declared.push(declare(&target, &class.name, metric, None, verdict));
        }
        json["failed"] = json!(failed);
        json["comparison"] = json!("one variant: not testable");
        Compared {
            latency: format!("— | — | — | {verdict} | {verdict}"),
            budget: None,
            memory: None,
        }
    };
    let latency_md = format!(
        "| {} | {}{md_sides} | {} |",
        class.name,
        class.members.len(),
        compared.latency
    );
    ClassResult {
        json,
        latency_md,
        budget_md: compared.budget.map(|b| format!("| {} | {b} |", class.name)),
        memory_md: compared.memory.map(|m| format!("| {} | {m} |", class.name)),
    }
}

/// The Markdown tables, filled segment by segment (set rows only — the
/// group rows live in the JSON).
#[derive(Default)]
struct Tables {
    latency: String,
    budget: String,
    memory: String,
    processes: String,
    failures: String,
}

const MIB: f64 = 1_048_576.0;

fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| String::from("—"), |b| format!("{:.1}", float(b) / MIB))
}

fn failure_json(failure: &Failure) -> Value {
    json!({
        "kind": failure.kind.as_str(),
        "code": failure.kind.code(),
        "peak_rss_bytes": failure.peak_rss,
        "wall_ms": r3(ms(failure.wall_ns)),
        "over_envelope": failure.peak_rss.is_some_and(|p| p > ENVELOPE),
    })
}

fn wasm_memory(end: &ProcessEnd) -> Option<u64> {
    end.farewell
        .split('\t')
        .nth(1)
        .and_then(|bytes| bytes.parse::<u64>().ok())
}

/// A worker process: its hello, how it ended, its peak and envelope flag,
/// its stderr reduced to counts, codes and panic locations.
fn process_json(end: &ProcessEnd) -> Value {
    let hello: Vec<&str> = end.hello.split('\t').collect();
    let field = |i: usize| hello.get(i).copied().filter(|f| *f != "-");
    json!({
        "protocol": field(1), "scanner": field(2), "pipeline": field(3), "score_contract": field(4),
        "runtime": field(5),
        "worker_source_sha256": if field(5) == Some("rust-worker") { field(6) } else { None },
        "loaded": if field(5) == Some("rust-worker") { None } else { field(7) },
        "ended": end.ended,
        "capped": end.capped,
        "quit_failure": end.quit_failure.as_ref().map(failure_json),
        "peak_rss_bytes": end.peak,
        "over_envelope": end.peak.is_some_and(|p| p > ENVELOPE),
        "wasm_memory_bytes": wasm_memory(end),
        "stderr": {"lines": end.stderr.lines, "qrs_codes": end.stderr.codes, "panic_locations": end.stderr.panics},
    })
}

/// The worker peak of one variant: the largest of its incarnations a cap or
/// a crash did not end — a capped incarnation's peak is reported under the
/// resource failures, never as the A/B peak.
fn worker_peak(ends: &[ProcessEnd]) -> Option<u64> {
    ends.iter()
        .filter(|e| !e.capped)
        .filter_map(|e| e.peak)
        .max()
}

fn processes_json(processes: &BTreeMap<Variant, Vec<ProcessEnd>>) -> Value {
    let map: serde_json::Map<String, Value> = processes
        .iter()
        .map(|(variant, ends)| {
            let peak = worker_peak(ends);
            (
                variant.as_str().to_owned(),
                json!({
                    "processes": ends.len(),
                    "capped": ends.iter().filter(|e| e.capped).count(),
                    "peak_rss_bytes": peak,
                    "over_envelope": peak.is_some_and(|p| p > ENVELOPE),
                    "wasm_memory_bytes": ends.iter().filter(|e| !e.capped).filter_map(wasm_memory).max(),
                    "each": ends.iter().map(process_json).collect::<Vec<_>>(),
                }),
            )
        })
        .collect();
    Value::Object(map)
}

/// Workers a cap or a signal ended during `quit`: resource failures that
/// fail the run though no call was in flight.
fn quit_failures(processes: &BTreeMap<Variant, Vec<ProcessEnd>>) -> usize {
    processes
        .values()
        .flatten()
        .filter(|e| e.quit_failure.is_some())
        .count()
}

fn call_failure_json(m: &Measurement, f: &CallFailure) -> Value {
    let image = &m.images[f.image];
    json!({
        "image": image.path, "set": image.set.as_str(), "group": image.group,
        "variant": f.variant.as_str(),
        "call": if f.reference { "reference" } else if f.rep == 0 { "warmup" } else { "timed" },
        "rep": (!f.reference).then_some(f.rep), "slot": (!f.reference).then_some(f.slot),
        "attempt": f.attempt,
        "failure": failure_json(&f.failure),
    })
}

/// The Markdown rows of a segment's worker processes (peaks without the
/// capped incarnations) and resource failures (calls, and workers a cap
/// or a signal ended at `quit`).
fn process_and_failure_rows(m: &Measurement, segment: &Segment, target: &str, tables: &mut Tables) {
    if !segment.processes.is_empty() {
        let peak = |variant: Variant| {
            segment
                .processes
                .get(&variant)
                .and_then(|ends| worker_peak(ends))
        };
        let wasm = |variant: Variant| {
            segment.processes.get(&variant).and_then(|ends| {
                ends.iter()
                    .filter(|e| !e.capped)
                    .filter_map(wasm_memory)
                    .max()
            })
        };
        let incarnations = |variant: Variant| segment.processes.get(&variant).map_or(0, Vec::len);
        let over = [Variant::Base, Variant::Candidate]
            .into_iter()
            .filter(|v| peak(*v).is_some_and(|p| p > ENVELOPE))
            .map(Variant::letter)
            .collect::<Vec<_>>();
        let _ = writeln!(
            tables.processes,
            "| {target} | {} | {} | {} | {} | {} / {} | {} |",
            mib(peak(Variant::Base)),
            mib(peak(Variant::Candidate)),
            mib(wasm(Variant::Base)),
            mib(wasm(Variant::Candidate)),
            incarnations(Variant::Base),
            incarnations(Variant::Candidate),
            if over.is_empty() {
                String::from("—")
            } else {
                over.join(", ")
            }
        );
    }
    for f in &segment.failures {
        let image = &m.images[f.image];
        let _ = writeln!(
            tables.failures,
            "| {target} | {} | {}{} | {} | {} | {} | {:.0} |",
            image.path,
            f.variant.letter(),
            if f.reference { " (reference walk)" } else { "" },
            f.attempt,
            f.failure.kind.as_str(),
            mib(f.failure.peak_rss),
            ms(f.failure.wall_ns)
        );
    }
    for (variant, ends) in &segment.processes {
        for failure in ends.iter().filter_map(|e| e.quit_failure.as_ref()) {
            let _ = writeln!(
                tables.failures,
                "| {target} | (worker at quit) | {} | {} | {} | {} | {:.0} |",
                variant.letter(),
                segment.attempts,
                failure.kind.as_str(),
                mib(failure.peak_rss),
                ms(failure.wall_ns)
            );
        }
    }
}

fn segment_json(
    m: &Measurement,
    segment: &Segment,
    exploratory: bool,
    declared: &mut Vec<Declared>,
    tables: &mut Tables,
) -> Value {
    let target = format!("{}/{}", segment.runtime.as_str(), segment.mode.as_str());
    let processes = processes_json(&segment.processes);
    process_and_failure_rows(m, segment, &target, tables);
    let mut rows = Vec::new();
    if segment.outcome == Outcome::Measured {
        for class in classes(&m.images) {
            let row = class_row(segment, &class, (exploratory, m.options.reps), declared);
            if class.level == "set" {
                let _ = writeln!(tables.latency, "| {target} {}", row.latency_md);
                if let Some(line) = &row.budget_md {
                    let _ = writeln!(tables.budget, "| {target} {line}");
                }
                if let Some(line) = &row.memory_md {
                    let _ = writeln!(tables.memory, "| {target} {line}");
                }
            }
            rows.push(row.json);
        }
    } else {
        let _ = writeln!(
            tables.latency,
            "| {target} | {} — {} | | | | | | | | | | | | |",
            segment.outcome.as_str(),
            segment.note.as_deref().unwrap_or("")
        );
    }
    json!({
        "runtime": segment.runtime.as_str(),
        "mode": segment.mode.as_str(),
        "budget_ms": segment.mode.budget_ms(),
        "wall_cap_s": segment.mode.wall_cap().as_secs(),
        "outcome": segment.outcome.as_str(),
        "note": segment.note,
        "attempts": segment.attempts,
        "resource_failures": segment.failures.iter().map(|f| call_failure_json(m, f)).collect::<Vec<_>>(),
        "processes": processes,
        "classes": rows,
        "gate": segment.readings,
    })
}

/// Unbounded scans are deterministic: every runtime of a variant should
/// return the same outputs per image (information; the oracle judges).
fn cross_runtime(m: &Measurement) -> Value {
    let mut out = serde_json::Map::new();
    for &variant in Variant::ALL {
        let segments: Vec<&Segment> = m
            .segments
            .iter()
            .filter(|s| s.mode == Mode::FullUnbounded && s.outcome == Outcome::Measured)
            .collect();
        let mut agree = 0u64;
        let mut total = 0u64;
        let mut runtimes = Vec::new();
        for index in 0..m.images.len() {
            let outputs: Vec<(Runtime, Option<String>)> = segments
                .iter()
                .filter_map(|segment| {
                    let samples = segment.samples.get(&index)?.get(&variant)?;
                    Some((
                        segment.runtime,
                        image_side(samples, None, 0, segment.mode).output,
                    ))
                })
                .collect();
            if outputs.len() < 2 {
                continue;
            }
            if runtimes.is_empty() {
                runtimes = outputs
                    .iter()
                    .map(|(runtime, _)| runtime.as_str())
                    .collect();
            }
            total += 1;
            agree += u64::from(outputs.windows(2).all(|pair| pair[0].1 == pair[1].1));
        }
        if total > 0 {
            out.insert(
                variant.as_str().to_owned(),
                json!({"runtimes": runtimes, "k": agree, "n": total, "wilson95": wilson_json(agree, total)}),
            );
        }
    }
    Value::Object(out)
}

fn throughput_json(runs: &[Throughput], md: &mut String) -> Value {
    if !runs.is_empty() {
        let _ = writeln!(
            md,
            "\n## Throughput (library in-process; completed scans per second)\n\n\
             | mode | threads | queue bound | A img/s | B img/s | B/A (unpaired) | A / B peak RSS MiB | failed jobs | outcome |\n\
             |---|---|---|---|---|---|---|---|---|"
        );
    }
    let rows: Vec<Value> = runs
        .iter()
        .map(|run| {
            let rate = |variant: Variant| {
                let passes: Vec<f64> = run
                    .passes
                    .iter()
                    .filter(|pass| pass.variant == variant.as_str())
                    .filter_map(super::run::Pass::rate)
                    .collect();
                stats::median(&passes)
            };
            let (base, candidate) = (rate(Variant::Base), rate(Variant::Candidate));
            let ratio = base
                .zip(candidate)
                .map(|(before, after)| r3(after / before));
            let cell = |value: Option<f64>| {
                value.map_or_else(|| String::from("—"), |value| format!("{value:.2}"))
            };
            let peak = |variant: Variant| run.processes.get(&variant).and_then(|ends| worker_peak(ends));
            let failed_jobs: u64 = run.passes.iter().map(|p| p.failed).sum();
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} | {} | {} | {} / {} | {} | {} |",
                run.mode.as_str(),
                run.threads,
                run.bound,
                cell(base),
                cell(candidate),
                cell(ratio),
                mib(peak(Variant::Base)),
                mib(peak(Variant::Candidate)),
                failed_jobs,
                run.outcome.as_str()
            );
            json!({
                "mode": run.mode.as_str(), "threads": run.threads, "queue_bound": run.bound,
                "queue_note": "the queue bounds pending image indices; the images are preloaded, so in-flight scans (and memory) scale with the thread count",
                "wall_cap_s": run.wall_cap.as_secs(),
                "rss_cap_bytes": run.rss_cap,
                "passes": run.passes, "base_images_per_s": base.map(r3),
                "candidate_images_per_s": candidate.map(r3), "ratio": ratio,
                "ratio_kind": "unpaired",
                "ratio_note": "median of each variant's pass rates — completed scans over its own pass time, failed jobs' time included; the variants' completed sets may differ, and no δ is declared for throughput",
                "resource_failures": run.failures.iter().map(|f| json!({"variant": f.variant.as_str(), "pass": f.pass, "failure": failure_json(&f.failure)})).collect::<Vec<_>>(),
                "processes": processes_json(&run.processes),
                "outcome": run.outcome.as_str(), "note": run.note, "gate": run.readings,
            })
        })
        .collect();
    Value::Array(rows)
}

#[allow(
    clippy::too_many_lines,
    reason = "sizes, recipes, cold starts and their verdicts share one table"
)]
fn wasm_json(
    m: &Measurement,
    cost: &WasmCost,
    exploratory: bool,
    declared: &mut Vec<Declared>,
    md: &mut String,
) -> Value {
    let _ = writeln!(
        md,
        "\n## WASM (published package layout, Node zlib; cold start = compile + instantiate + first fixture scan)\n\n\
         | variant | raw | gzip-9 | brotli-11 | glue JS | compile p50 ms | instantiate p50 ms | first scan p50 ms | cold start p50 ms | SIMD | wasm-bindgen |\n\
         |---|---|---|---|---|---|---|---|---|---|---|"
    );
    let failed = !cost.failures.is_empty();
    let mut variants = serde_json::Map::new();
    let series = |variant: Variant, f: fn(&super::run::Cold) -> u64| -> Vec<f64> {
        cost.runs
            .get(&variant)
            .map(|runs| {
                runs.iter()
                    .filter(|c| c.rep > 0)
                    .map(|c| ms(f(c)))
                    .collect()
            })
            .unwrap_or_default()
    };
    for &variant in Variant::ALL {
        let sizes = cost.sizes.get(&variant);
        let recipe = m
            .artifacts
            .iter()
            .find(|a| a.key == (Runtime::Wasm, variant))
            .and_then(|a| a.described.wasm.clone());
        let compile = series(variant, |c| c.compile_ns);
        let instantiate = series(variant, |c| c.instantiate_ns);
        let first = series(variant, |c| c.first_scan_ns);
        let cold = series(variant, super::run::Cold::cold_ns);
        if sizes.is_none() && cold.is_empty() && recipe.is_none() {
            continue;
        }
        // nearest rank, like every p50 of the JSON receipt
        let p50 = |v: &[f64]| {
            stats::percentile(v, 50).map_or_else(|| String::from("—"), |x| format!("{x:.2}"))
        };
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            variant.as_str(),
            sizes.map_or(0, |s| s.raw),
            sizes.map_or(0, |s| s.gzip9),
            sizes.map_or(0, |s| s.brotli11),
            sizes.map_or(0, |s| s.glue),
            p50(&compile),
            p50(&instantiate),
            p50(&first),
            p50(&cold),
            recipe
                .as_ref()
                .and_then(|r| r.simd)
                .map_or("unknown", |s| if s { "yes" } else { "no" }),
            recipe
                .as_ref()
                .and_then(|r| r.wasm_bindgen.clone())
                .unwrap_or_else(|| String::from("—"))
        );
        variants.insert(
            variant.as_str().to_owned(),
            json!({"sizes": sizes, "recipe": recipe, "compile_ms": spread_json(&compile),
                   "instantiate_ms": spread_json(&instantiate), "first_scan_ms": spread_json(&first),
                   "cold_start_ms": spread_json(&cold), "runs": cost.runs.get(&variant)}),
        );
    }
    let mut out = json!({"variants": variants, "outcome": cost.outcome.as_str(), "note": cost.note,
                         "resource_failures": cost.failures.iter().map(|(v, f)| json!({"variant": v.as_str(), "failure": failure_json(f)})).collect::<Vec<_>>(),
                         "gate": cost.readings});
    match (
        cost.sizes.get(&Variant::Base),
        cost.sizes.get(&Variant::Candidate),
    ) {
        (Some(base), Some(candidate)) => {
            let size = |before: u64, after: u64| {
                label(
                    stats::size_verdict(before, after, DELTA_SIZE_BP),
                    failed,
                    exploratory,
                )
            };
            out["size_verdicts"] = json!({
                "gzip9": size(base.gzip9, candidate.gzip9),
                "brotli11": size(base.brotli11, candidate.brotli11),
            });
            for (metric, before, after) in [
                ("wasm_gzip9", base.gzip9, candidate.gzip9),
                ("wasm_brotli11", base.brotli11, candidate.brotli11),
            ] {
                declared.push(Declared {
                    estimate_pct: Some(r3((float(after) / float(before) - 1.0) * 100.0)),
                    ..declare("wasm", "package", metric, None, size(before, after))
                });
            }
        }
        _ => {
            for metric in ["wasm_gzip9", "wasm_brotli11"] {
                declared.push(declare(
                    "wasm",
                    "package",
                    metric,
                    None,
                    label(Verdict::NotTestable, failed, exploratory),
                ));
            }
        }
    }
    let (base, candidate) = (
        series(Variant::Base, super::run::Cold::cold_ns),
        series(Variant::Candidate, super::run::Cold::cold_ns),
    );
    let pairs: Vec<(f64, f64)> = base
        .iter()
        .copied()
        .zip(candidate.iter().copied())
        .collect();
    let shift = stats::hodges_lehmann(&stats::log_ratios(&pairs));
    let verdict = label(
        stats::verdict(shift.as_ref(), DELTA_COLD_START),
        failed,
        exploratory,
    );
    declared.push(declare(
        "wasm",
        "package",
        "cold_start_p50",
        shift.as_ref(),
        verdict,
    ));
    out["cold_start"] = json!({"shift": shift_json(shift.as_ref()), "verdict": verdict});
    out
}

/// Pairs excluded from the paired tests over the whole run.
fn excluded_totals(segments: &[Value]) -> BTreeMap<String, u64> {
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    for segment in segments {
        for class in segment["classes"].as_array().into_iter().flatten() {
            if class["level"] != "set" {
                continue;
            }
            for (why, n) in class["pairs"]["excluded"].as_object().into_iter().flatten() {
                *totals.entry(why.clone()).or_default() += n.as_u64().unwrap_or(0);
            }
        }
    }
    totals
}

/// What the declared frozen scope still lacks: every runtime × mode
/// segment measured, every thread count of the `full` throughput pass with
/// its four passes, and the WASM cost of both variants with every cold
/// start — derived from the scope, never from the phases a run happens to
/// hold.
pub(crate) fn missing(m: &Measurement) -> Vec<String> {
    let mut missing = Vec::new();
    for &runtime in Runtime::ALL {
        for &mode in Mode::ALL {
            let target = format!("{}/{}", runtime.as_str(), mode.as_str());
            match m
                .segments
                .iter()
                .find(|s| s.runtime == runtime && s.mode == mode)
            {
                Some(s) if s.outcome == Outcome::Measured => {}
                Some(s) => missing.push(format!("{target} {}", s.outcome.as_str())),
                None => missing.push(format!("{target} not selected")),
            }
        }
    }
    for threads in thread_counts(m.facts.cores_logical) {
        match m
            .throughput
            .iter()
            .find(|t| t.mode == Mode::Full && t.threads == threads)
        {
            Some(t) if t.outcome == Outcome::Measured && t.passes.len() == 4 => {}
            Some(t) => missing.push(format!("throughput full/{threads} {}", t.outcome.as_str())),
            None => missing.push(format!("throughput full/{threads} not selected")),
        }
    }
    let wasm = m.wasm.as_ref().is_some_and(|w| {
        w.outcome == Outcome::Measured
            && Variant::ALL.iter().all(|v| {
                w.sizes.contains_key(v)
                    && w.runs.get(v).is_some_and(|runs| {
                        runs.iter().filter(|c| c.rep > 0).count() == COLD_PAIRS as usize
                    })
            })
    });
    if !wasm {
        missing.push(String::from("wasm cost of both variants"));
    }
    missing
}

/// The exclusion rule over every set row of a measured comparison: fewer
/// than 6 valid pairs, or more than 10 % of the row's images excluded,
/// ends the run partial; A/B disagreements in a budgeted mode — detections
/// a budget cost — are named in the status.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SetRows {
    pub(crate) breaches: Vec<String>,
    pub(crate) budgeted_disagreements: Vec<String>,
}

pub(crate) fn set_rows(m: &Measurement) -> SetRows {
    let mut rows = SetRows::default();
    let sets = classes(&m.images);
    for segment in m.segments.iter().filter(|s| s.outcome == Outcome::Measured) {
        let target = format!("{}/{}", segment.runtime.as_str(), segment.mode.as_str());
        for class in sets.iter().filter(|c| c.level == "set") {
            let sides = class_sides(segment, class);
            let (Some(base), Some(candidate)) =
                (sides.get(&Variant::Base), sides.get(&Variant::Candidate))
            else {
                continue;
            };
            let (valid, excluded) = pairs_of((base, candidate), m.options.reps, segment.mode);
            let images = class.members.len();
            let total: usize = excluded.values().sum();
            if valid.len() < stats::MIN_PAIRS {
                rows.breaches.push(format!(
                    "{target} {}: {} valid pairs",
                    class.name,
                    valid.len()
                ));
            }
            if 10 * total > images {
                rows.breaches.push(format!(
                    "{target} {}: {total} of {images} images excluded",
                    class.name
                ));
            }
            if segment.mode.budgeted()
                && let Some(n) = excluded.get(Excluded::Disagreement.as_str())
            {
                rows.budgeted_disagreements
                    .push(format!("{target} {} {n}", class.name));
            }
        }
    }
    rows
}

/// The run's status and exit code: refused 3 (gate, host floor) or 2
/// (parity, inputs) · failed 1 (any resource failure or protocol failure,
/// dry runs included) · exploratory 3 (a dry run never passes) ·
/// regression 1 · partial 3 (a subset, anything of the declared scope not
/// measured, or a set row the exclusion rule caught) · completed 0.
pub(crate) fn status(m: &Measurement, declared: &[Declared]) -> (&'static str, i32) {
    if let Some(refusal) = &m.refused {
        return ("refused", refusal.exit());
    }
    let failed = !m.failures.is_empty()
        || m.segments.iter().any(|s| {
            s.outcome == Outcome::Failed
                || !s.failures.is_empty()
                || quit_failures(&s.processes) > 0
        })
        || m.throughput.iter().any(|t| {
            t.outcome == Outcome::Failed
                || !t.failures.is_empty()
                || quit_failures(&t.processes) > 0
        })
        || m.wasm
            .as_ref()
            .is_some_and(|w| w.outcome == Outcome::Failed || !w.failures.is_empty());
    if failed {
        return ("failed", 1);
    }
    if m.options.dry_run {
        return ("exploratory", 3);
    }
    if declared
        .iter()
        .any(|d| d.verdict == Verdict::Regression.as_str())
    {
        return ("regression", 1);
    }
    let complete = m.stopped.is_none()
        && m.options.subset().is_empty()
        && m.inputs.pins.is_empty()
        && missing(m).is_empty()
        && set_rows(m).breaches.is_empty();
    if complete {
        ("completed", 0)
    } else {
        ("partial", 3)
    }
}

/// What the status line adds to the state: the excluded pairs, the A/B
/// disagreements of budgeted modes, and why a run is partial.
fn status_detail(m: &Measurement, excluded: &BTreeMap<String, u64>) -> (Value, String) {
    let rows = set_rows(m);
    let missing = missing(m);
    let subset = m.options.subset();
    let mut parts = Vec::new();
    if !excluded.is_empty() {
        parts.push(format!(
            "excluded pairs: {}",
            excluded
                .iter()
                .map(|(why, n)| format!("{why} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !rows.budgeted_disagreements.is_empty() {
        parts.push(format!(
            "budgeted-mode A/B disagreement: {}",
            rows.budgeted_disagreements.join(", ")
        ));
    }
    if !rows.breaches.is_empty() {
        parts.push(format!("exclusion rule: {}", rows.breaches.join(", ")));
    }
    if !missing.is_empty() {
        parts.push(format!(
            "not measured: {} of the declared scope",
            missing.len()
        ));
    }
    if !m.inputs.pins.is_empty() {
        parts.push(format!("inputs off their pins: {}", m.inputs.pins.len()));
    }
    let json = json!({
        "subset": subset,
        "inputs_off_pins": m.inputs.pins,
        "missing": missing,
        "exclusion_breaches": rows.breaches,
        "budgeted_disagreements": rows.budgeted_disagreements,
    });
    (json, parts.join(" · "))
}

/// Explicit `not_testable` rows for every metric of the frozen scope that
/// was not measured: each set of each runtime × mode a run did not measure,
/// and the WASM metrics without a cost phase.
fn unmeasured_rows(m: &Measurement, declared: &mut Vec<Declared>) {
    let measured_sets: BTreeSet<Set> = m.images.iter().map(|i| i.set).collect();
    let not_testable = Verdict::NotTestable.as_str();
    for &runtime in Runtime::ALL {
        for &mode in Mode::ALL {
            let target = format!("{}/{}", runtime.as_str(), mode.as_str());
            let segment = m
                .segments
                .iter()
                .find(|s| s.runtime == runtime && s.mode == mode);
            let measured = segment.is_some_and(|s| s.outcome == Outcome::Measured);
            let reason = match segment {
                None => String::from("not selected"),
                Some(s) if !measured => format!(
                    "{}{}",
                    s.outcome.as_str(),
                    s.note
                        .as_deref()
                        .map(|n| format!(": {n}"))
                        .unwrap_or_default()
                ),
                Some(_) => String::from("set not selected"),
            };
            for &set in Set::ALL {
                if measured && measured_sets.contains(&set) {
                    continue;
                }
                for metric in metrics(runtime, mode) {
                    declared.push(Declared {
                        note: Some(reason.clone()),
                        ..declare(&target, set.as_str(), metric, None, not_testable)
                    });
                }
            }
        }
    }
    if m.wasm.is_none() {
        for metric in ["wasm_gzip9", "wasm_brotli11", "cold_start_p50"] {
            declared.push(Declared {
                note: Some(String::from("wasm not selected")),
                ..declare("wasm", "package", metric, None, not_testable)
            });
        }
    }
}

fn method_json(m: &Measurement) -> Value {
    json!({
        "warmup": super::run::WARMUP,
        "reps": m.options.reps,
        "order": "per image: one warm-up per variant, then A B B A blocks — A first on even-indexed images, B first on odd ones (warm-ups in the same order); every sample records its slot",
        "priming": format!("every worker scans {} once, uncounted, after hello (and after every respawn)", super::run::FIXTURE),
        "statistic": "per-image median of the completed timed calls; class p50/p95/max by nearest rank over the valid pairs (all_complete: over every image the variant completed)",
        "pairs": "an image enters a paired test only when both variants completed every call — reference walk, warm-up and timed calls — without error, with one output over every call, the two variants agreeing; excluded pairs are counted by reason",
        "reference": "budgeted modes: every image and variant gets one uncounted call of the same profile without a budget at segment start, under the unbounded caps",
        "truncation": "a timed call is truncated when its walk (stages, the transforms_tried sum, detections) or its judgment signature (composite, weights run, per-axis cells passed of planned) differs from the reference walk's; a call that completes the reference walk is complete whatever its time — the ladder reaching the budget is an overrun, information only",
        "comparison": "per-class paired log-ratio ln(B/A) over the valid pairs whose median calls are complete on both sides; Hodges-Lehmann estimate, exact Wilcoxon signed-rank 95 % interval; regression only when the lower bound exceeds ln(1 + δ)",
        "p95": "the same on the tail of those pairs: the k = floor(n/10) with the largest per-image geometric mean of A and B; testable when k >= 6, i.e. n >= 60",
        "budget_bound": "labels the valid pairs whose median call is truncated on a side; it never suppresses the latency verdict of the complete pairs",
        "truncation_test": "per class over the valid pairs (at least 6): exact one-sided McNemar on the median call's truncation status, candidate-only against base-only; p < 0.05 is a truncation regression; the same on judgment-cut status is a lost-judgment regression",
        "work": "HL of ln(B/A) of the per-image median transforms_tried sum over the valid pairs where a side was truncated; regression when the upper bound is below ln(1 - 0.05)",
        "overruns": "per image: the per-image median slower than the preset budget (Wilson 95 % over images); call-level counts are clustered and carry no interval",
        "delta": {"p50": DELTA_P50, "p95": DELTA_P95, "peak_memory": DELTA_MEMORY, "wasm_gzip_brotli_bp": DELTA_SIZE_BP, "cold_start_p50": DELTA_COLD_START, "work": DELTA_WORK},
        "cold_start_pairs": super::run::COLD_PAIRS,
        "cold_start": "per cold Node process: compile + instantiate + the first scan of the fixture (full, unbounded); compile and instantiate alone are information",
        "memory": "lib: counting global allocator on the warm-up call (net peak heap, allocation calls, bytes retained after the report is dropped); cli: wait4 peak RSS per process; median shift and tail (k largest per-image peaks), δ 10 % each, judged in full-unbounded only — information in budgeted modes; node and wasm per-image memory not testable — worker peak RSS and wasm linear memory reported per segment",
        "envelope_bytes": ENVELOPE,
        "caps": {
            "rss_bytes": m.options.rss_cap(),
            "wall": "max(10 × the preset budget, 60 s); 60 s unbounded and for reference walks; a throughput pass: the per-image cap × ceil(images / threads)",
            "throughput_rss": "a quarter of the per-call memory cap per thread, at most three per-call caps: 256 MiB per thread, at most 3072 MiB, at the default",
            "poll_ms": super::proc::POLL.as_millis(),
            "memory_sampled": "resident size (macOS: or the physical footprint, whichever is larger) and the kernel's high-water mark, so a spike between two samples is still seen",
            "returned": "a call that returns with a kernel peak above the memory cap, or after its wall cap, is a resource failure",
            "survive_the_harness": "every child in its own process group; SIGTERM, SIGHUP and SIGINT kill every live group before the harness exits; one-shot children carry a kernel CPU-time limit at their wall cap; workers leave within about 50 ms once their parent changes; a child whose memory cannot be sampled is not run; only Mach-O or ELF executables and the resolved node run",
        },
        "gate": {
            "load_per_core": super::gate::LOAD_PER_CORE, "threshold": m.threshold,
            "own_allowance": "T × (1 − e^(−elapsed/60)): 1 thread in a latency segment or cold start, T in a throughput pass",
            "also_closed_by": "a sibling build, battery power or Low Power Mode, swap above 8 GB (8 × 10^9 bytes), another bench run holding the host lock, a lock file that no longer names the held lock, and any part of a reading that cannot be read — load, process table, swap, power — or whose probe passed its cap",
            "read": "before every segment, every reference pass image, every image, the WASM sizes, every cold start, every 10 s of a throughput pass, and after each",
            "lock": "a host-wide lock file outside the system temporary directory, taken before anything else runs and held for the whole run; its inode is re-checked at every reading",
        },
        "host_floor": {"load1": super::gate::FLOOR_LOAD, "swap_bytes": super::gate::FLOOR_SWAP, "applies": "dry runs: refused at start, stopped when crossed; no new phase starts once stopped"},
        "modes": m.options.modes.iter().map(|mode| json!({"mode": mode.as_str(), "profile": mode.profile(), "budget_ms": mode.budget_ms(), "wall_cap_s": mode.wall_cap().as_secs()})).collect::<Vec<_>>(),
    })
}

/// One completed call as a `samples.jsonl` line: `call` is `reference`,
/// `warmup` or `timed`; with the image's reference walk, its truncation
/// facts (null without one).
fn sample_line(
    segment: &Segment,
    (image, variant): (&Image, Variant),
    (s, call): (&Sample, &str),
    reference: Option<&Sample>,
) -> Value {
    let cut = |f: &dyn Fn(&Sample) -> bool| reference.map(f);
    json!({
        "runtime": segment.runtime.as_str(), "mode": segment.mode.as_str(),
        "variant": variant.as_str(), "image": image.path, "image_sha256": image.sha256,
        "call": call, "rep": (call != "reference").then_some(s.rep), "slot": (call != "reference").then_some(s.slot),
        "ns": s.ns, "total_ms": s.total_ms, "engine_panics": s.engine_panics,
        "detections": s.units, "signature": s.units.as_deref().map(signature), "error": s.error,
        "peak_heap": s.heap.map(|h| h.peak), "alloc_calls": s.heap.map(|h| h.calls),
        "alloc_bytes": s.heap.map(|h| h.bytes), "retained": s.retained, "rss": s.rss, "cpu_us": s.cpu_us,
        "work": s.work, "reached_budget": s.reached_budget(segment.mode),
        "truncated": cut(&|r| s.truncated(r, segment.mode)),
        "walk_cut": cut(&|r| s.walk_cut(r)),
        "judgment_cut": cut(&|r| s.judgment_cut(r, segment.mode)),
    })
}

fn samples_lines(m: &Measurement) -> String {
    let mut out = String::new();
    for segment in m.segments.iter().filter(|s| s.outcome == Outcome::Measured) {
        for (index, references) in &segment.references {
            for (variant, s) in references {
                let line = sample_line(
                    segment,
                    (&m.images[*index], *variant),
                    (s, "reference"),
                    None,
                );
                let _ = writeln!(out, "{line}");
            }
        }
        for (index, variants) in &segment.samples {
            let image = &m.images[*index];
            for (variant, samples) in variants {
                let reference = segment.references.get(index).and_then(|r| r.get(variant));
                for s in samples {
                    let call = if s.rep == 0 { "warmup" } else { "timed" };
                    let line = sample_line(segment, (image, *variant), (s, call), reference);
                    let _ = writeln!(out, "{line}");
                }
            }
        }
    }
    out
}

fn images_lines(m: &Measurement) -> String {
    let mut out = String::new();
    for segment in m.segments.iter().filter(|s| s.outcome == Outcome::Measured) {
        for (index, variants) in &segment.samples {
            let image = &m.images[*index];
            let mut line = json!({
                "runtime": segment.runtime.as_str(), "mode": segment.mode.as_str(), "set": image.set.as_str(),
                "group": image.group, "image": image.path, "image_sha256": image.sha256, "bytes": image.bytes,
            });
            let mut sides = BTreeMap::new();
            for (variant, samples) in variants {
                let failures = segment
                    .failures
                    .iter()
                    .filter(|f| f.image == *index && f.variant == *variant)
                    .count();
                let reference = segment.references.get(index).and_then(|r| r.get(variant));
                let side = image_side(samples, reference, failures, segment.mode);
                line[variant.as_str()] = json!({
                    "median_ms": side.median_ms.map(r3), "median_total_ms": side.median_total_ms.map(r3),
                    "output": side.output, "stable": side.stable, "errors": side.errors,
                    "resource_failures": side.failures, "complete": side.complete(m.options.reps),
                    "peak_heap": side.heap_peak, "alloc_calls": side.alloc_calls, "retained": side.retained,
                    "median_rss": side.median_rss, "transforms": side.transforms,
                    "reference": segment.mode.budgeted().then_some(side.reference),
                    "truncated_calls": side.truncated, "judgment_cut_calls": side.judgment_cut,
                    "median_truncated": side.median_truncated, "median_judgment_cut": side.median_judgment_cut,
                    "over_envelope_calls": side.over_envelope,
                });
                sides.insert(*variant, side);
            }
            if let (Some(a), Some(b)) = (sides.get(&Variant::Base), sides.get(&Variant::Candidate))
            {
                line["pair"] = json!(match pair_status(a, b, m.options.reps, segment.mode) {
                    Ok(()) => "valid",
                    Err(why) => why.as_str(),
                });
            }
            let _ = writeln!(out, "{line}");
        }
    }
    out
}

/// The load of the last reading of `phase` (the gate wait keeps its first
/// and last reading; the last one is when the run started).
fn load_at(m: &Measurement, phase: &str) -> String {
    m.readings
        .iter()
        .rev()
        .find(|r| r.phase == phase)
        .and_then(|r| r.load)
        .map_or_else(
            || String::from("—"),
            |l| format!("{:.2} {:.2} {:.2}", l.one, l.five, l.fifteen),
        )
}

fn markdown_head(
    m: &Measurement,
    (state, exit, detail): (&str, i32, &str),
    excluded: &BTreeMap<String, u64>,
) -> String {
    let f = &m.facts;
    let mut md = format!("# Benchmark receipt — {BENCH_ID} ({PROTOCOL})\n\n");
    if m.options.dry_run {
        md.push_str("**EXPLORATORY — dry run: the host gate was recorded, not enforced (only the host floor and the resource caps were). These numbers are not a baseline and carry no verdict; the run exits 3.**\n\n");
    }
    let selected: Vec<String> = m
        .inputs
        .selected
        .iter()
        .map(|(set, n)| {
            format!(
                "{set} {n}/{}",
                m.inputs.available.get(set).copied().unwrap_or(0)
            )
        })
        .collect();
    let subset = m.options.subset();
    let excluded_cell = if excluded.is_empty() {
        String::from("none")
    } else {
        excluded
            .iter()
            .map(|(why, n)| format!("{why} {n}"))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    let _ = write!(
        md,
        "| fact | value |\n|---|---|\n\
         | status | {state} (exit {exit}){} |\n\
         | scope | {} |\n\
         | excluded pairs (set rows) | {excluded_cell} |\n\
         | date | {} |\n\
         | source | {} ({}) |\n\
         | host | {} · {} · {} logical cores ({} P + {} E) · {} bytes RAM |\n\
         | os | {} · kernel {} |\n\
         | power at start | {} · battery {} · low power {} |\n\
         | load at start / end | {} / {} (gate ceiling {:.2}) |\n\
         | caps | RSS {} MiB · wall max(10 × budget, 60 s) · envelope 512 MiB |\n\
         | harness | rustc {} · cargo toolchain {} · harness {} (binary sha256 {}) · RAYON_NUM_THREADS {} |\n\
         | node | {} |\n\
         | images | {} · per group {} |\n\
         | inputs | manifest {} · vendored {} · dispositions {} |\n",
        m.refused
            .as_ref()
            .map(|r| format!(" — {}", r.text()))
            .or_else(|| m.stopped.as_ref().map(|s| format!(" — stopped: {s}")))
            .unwrap_or_default()
            + &if detail.is_empty() {
                String::new()
            } else {
                format!(" — {detail}")
            },
        if subset.is_empty() {
            String::from("the complete frozen scope")
        } else {
            format!("subset: {}", subset.join(" · "))
        },
        f.date_utc,
        f.source.git_sha,
        f.source.tree,
        f.host_model,
        f.cpu,
        f.cores_logical,
        f.cores_performance,
        f.cores_efficiency,
        f.ram_bytes.unwrap_or(0),
        f.os,
        f.kernel,
        f.power.source,
        f.power
            .battery_percent
            .map_or_else(|| String::from("—"), |p| format!("{p} %")),
        f.power
            .low_power_mode
            .map_or("—", |on| if on { "on" } else { "off" }),
        load_at(m, "run start"),
        load_at(m, "run end"),
        m.threshold,
        m.options.rss_cap_mib,
        f.rustc,
        f.cargo_toolchain,
        f.harness_profile,
        f.harness_sha256,
        f.rayon_num_threads.as_deref().unwrap_or("unset"),
        f.node,
        selected.join(" · "),
        m.inputs
            .per_group
            .map_or_else(|| String::from("all"), |n| n.to_string()),
        m.inputs.manifest_sha256,
        m.inputs.vendored_sha256,
        m.inputs.dispositions_sha256,
    );
    md.push_str(&markdown_artifacts(m));
    md
}

/// The artifacts table (binary, compilers, manifest, profile) and the pair
/// parity table.
fn markdown_artifacts(m: &Measurement) -> String {
    let mut md = String::new();
    let _ = writeln!(
        md,
        "\n## Artifacts\n\n| runtime | variant | binary | sha256 | rustc commits | manifest | profile · source |\n|---|---|---|---|---|---|---|"
    );
    for artifact in &m.artifacts {
        let d = &artifact.described;
        let short = |s: &str| s.get(..12).unwrap_or(s).to_owned();
        let manifest = d.manifest.as_ref().map_or_else(
            || {
                d.manifest_error.as_ref().map_or_else(
                    || String::from("none"),
                    |e| format!("unreadable: {}", e.replace('|', "/")),
                )
            },
            |mf| {
                format!(
                    "{} · {} {} ({})",
                    mf.rustc.as_ref().map_or("?", |r| r.release.as_str()),
                    mf.source.kind,
                    short(&mf.source.sha),
                    mf.source.tree
                )
            },
        );
        let profile = d.manifest.as_ref().map_or_else(
            || String::from("—"),
            |mf| {
                [
                    "lto",
                    "codegen-units",
                    "strip",
                    "package.rxing.overflow-checks",
                ]
                .iter()
                .map(|k| format!("{k}={}", mf.profile.get(*k).map_or("-", String::as_str)))
                .collect::<Vec<_>>()
                .join(" ")
            },
        );
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {manifest} | {profile} |",
            artifact.runtime,
            artifact.variant,
            d.binary,
            d.sha256,
            d.embedded_rustc_commits
                .iter()
                .map(|c| short(c))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !m.parity.is_empty() {
        let _ = writeln!(
            md,
            "\n## Pair parity\n\n| runtime | findings | lock delta |\n|---|---|---|"
        );
        for row in &m.parity {
            let findings = if row.findings.is_empty() {
                String::from("fit")
            } else {
                row.findings
                    .iter()
                    .map(|f| format!("{}: {}", f.what, f.detail))
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            let delta = row.lock_delta.as_ref().map_or_else(
                || String::from("—"),
                |d| {
                    format!(
                        "{} same · {} differ{} · only base {} · only candidate {}",
                        d.same,
                        d.version_differs.len(),
                        if d.version_differs.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", d.version_differs.join(", "))
                        },
                        d.only_base,
                        d.only_candidate
                    )
                },
            );
            let _ = writeln!(md, "| {} | {findings} | {delta} |", row.runtime);
        }
    }
    md
}

/// Create `name` in `out` — never over an existing file — and return its
/// sha256.
fn put(out: &Path, name: &str, content: &str) -> Result<String, String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join(name))
        .map_err(|e| format!("{name}: {e}"))?;
    file.write_all(content.as_bytes())
        .map_err(|e| format!("{name}: {e}"))?;
    Ok(crate::external::sha256_bytes(content.as_bytes()))
}

/// Write the run directory; returns the exit code.
#[allow(
    clippy::too_many_lines,
    reason = "the receipt layout reads best in one place"
)]
pub(crate) fn write(m: &Measurement, out: &Path) -> Result<i32, String> {
    let exploratory = m.options.dry_run;
    let mut declared = Vec::new();
    let mut tables = Tables::default();
    let segments: Vec<Value> = m
        .segments
        .iter()
        .map(|s| segment_json(m, s, exploratory, &mut declared, &mut tables))
        .collect();
    let excluded = excluded_totals(&segments);
    let mut body = format!(
        "\n## Latency (outer call, ms; A = base 0.9.0, B = candidate; over the valid pairs; set rows — groups in receipt.json)\n\n\
         | target | class | n | A p50 | A p95 | A max | B p50 | B p95 | B max | excluded pairs | complete / valid pairs | Δ HL [95 % CI] | p50 | p95 tail |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n{}",
        tables.latency
    );
    if !tables.budget.is_empty() {
        let _ = write!(
            body,
            "\n## Budget (a timed call is truncated when its walk or judgment differs from the image's unbudgeted reference walk; images by their median call, over the valid pairs)\n\n\
             | target | class | A truncated | B truncated | budget-bound pairs | truncation (one-sided McNemar) | lost judgments | work Δ HL [95 % CI] (truncated pairs) | work verdict |\n\
             |---|---|---|---|---|---|---|---|---|\n{}",
            tables.budget
        );
    }
    if !tables.memory.is_empty() {
        let _ = write!(
            body,
            "\n## Memory per scan (full-unbounded; lib: net peak heap of the warm-up call; cli: process peak RSS)\n\n\
             | target | class | Δ HL p50 [95 % CI] | p50 | Δ HL tail [95 % CI] | tail |\n\
             |---|---|---|---|---|---|\n{}",
            tables.memory
        );
    }
    if !tables.processes.is_empty() {
        let _ = write!(
            body,
            "\n## Worker processes (peak RSS MiB; wasm linear memory MiB; envelope 512 MiB)\n\n\
             | target | A RSS | B RSS | A wasm memory | B wasm memory | processes A / B | over envelope |\n\
             |---|---|---|---|---|---|---|\n{}",
            tables.processes
        );
    }
    if !tables.failures.is_empty() {
        let _ = write!(
            body,
            "\n## Resource failures (never a sample; the class is failed)\n\n\
             | target | image | variant | attempt | kind | peak MiB | wall ms |\n\
             |---|---|---|---|---|---|---|\n{}",
            tables.failures
        );
    }
    let throughput = throughput_json(&m.throughput, &mut body);
    let wasm = m
        .wasm
        .as_ref()
        .map(|w| wasm_json(m, w, exploratory, &mut declared, &mut body));
    unmeasured_rows(m, &mut declared);
    let (state, exit) = status(m, &declared);
    let (detail, detail_line) = status_detail(m, &excluded);
    let regressions: Vec<&Declared> = declared
        .iter()
        .filter(|d| d.verdict == Verdict::Regression.as_str())
        .collect();
    let _ = writeln!(body, "\n## Verdicts\n");
    if exploratory {
        let _ = writeln!(body, "None: a dry run never declares a verdict.");
    } else {
        let count_of = |v: &str| declared.iter().filter(|d| d.verdict == v).count();
        let _ = writeln!(
            body,
            "{} regression(s) · {} no regression · {} not testable · {} failed — over {} declared comparisons.",
            regressions.len(),
            count_of(Verdict::NoRegression.as_str()),
            count_of(Verdict::NotTestable.as_str()),
            count_of(Verdict::Failed.as_str()),
            declared.len()
        );
        for d in &regressions {
            let _ = match (d.p_value, d.discordant) {
                (Some(p), Some([only_b, only_a])) => writeln!(
                    body,
                    "- {} · {} · {}: one-sided McNemar p {p:.6} ({only_b} images B only, {only_a} A only)",
                    d.target, d.class, d.metric
                ),
                _ => writeln!(
                    body,
                    "- {} · {} · {}: {:?} % {:?}",
                    d.target, d.class, d.metric, d.estimate_pct, d.ci_pct
                ),
            };
        }
    }
    let receipt = json!({
        "bench": BENCH_ID,
        "protocol": PROTOCOL,
        "mode": if exploratory { "dry-run" } else { "gated" },
        "exploratory": exploratory,
        "status": state,
        "exit_code": exit,
        "status_detail": detail,
        "scope": {"complete": m.options.subset().is_empty(), "subset": m.options.subset()},
        "refused": m.refused.as_ref().map(|r| json!({"exit": r.exit(), "why": r.text()})),
        "stopped": m.stopped,
        "failures": m.failures,
        "excluded_pairs": excluded,
        "method": method_json(m),
        "environment": m.facts,
        "inputs": m.inputs,
        "artifacts": m.artifacts,
        "hellos": m.hellos.iter().map(|((r, v), h)| json!({"runtime": r.as_str(), "variant": v.as_str(), "hello": h})).collect::<Vec<_>>(),
        "parity": m.parity,
        "gate": m.readings,
        "segments": segments,
        "cross_runtime_agreement_full_unbounded": cross_runtime(m),
        "throughput": throughput,
        "wasm": wasm,
        "verdicts": declared.iter().map(|d| json!({"target": d.target, "class": d.class, "metric": d.metric, "verdict": d.verdict, "estimate_pct": d.estimate_pct, "ci95_pct": d.ci_pct, "p_one_sided": d.p_value, "discordant": d.discordant, "note": d.note})).collect::<Vec<_>>(),
    });
    let mut text = serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?;
    text.push('\n');
    let files = [
        ("receipt.json", text),
        (
            "receipt.md",
            markdown_head(m, (state, exit, &detail_line), &excluded) + &body,
        ),
        ("samples.jsonl", samples_lines(m)),
        ("images.jsonl", images_lines(m)),
    ];
    if detail_line.is_empty() {
        println!("\nbench {state} (exit {exit})");
    } else {
        println!("\nbench {state} (exit {exit}) — {detail_line}");
    }
    for (name, content) in &files {
        let sha = put(out, name, content)?;
        println!("  {name}  sha256 {sha}");
    }
    println!("receipt: {}", out.join("receipt.json").display());
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::env;
    use crate::bench::proc::Kind;
    use crate::bench::run::{Inputs, Options, Refusal, Unit, Work, parse_options};
    use crate::bench::worker::HeapStats;

    fn sample(rep: u32, ns: u64, text: &str) -> Sample {
        Sample {
            rep,
            slot: rep,
            ns,
            total_ms: Some(1.0),
            engine_panics: Some(0),
            units: Some(vec![Unit::of("qr_code", text.as_bytes())]),
            heap: (rep == 0).then_some(HeapStats {
                peak: 1000,
                calls: 10,
                bytes: 2000,
            }),
            retained: (rep == 0).then_some(16),
            work: Some(Work {
                stages: 1,
                transforms: 3,
                judgment: Some(String::from("v80w100,5/5")),
            }),
            ..Sample::default()
        }
    }

    /// The per-image summary drops the warm-up from the latency, takes the
    /// median of the completed timed calls, flags unstable outputs — the
    /// warm-up's included — and keeps the warm-up's heap.
    #[test]
    fn image_sides_use_timed_reps_and_warm_up_memory() {
        let samples = vec![
            sample(0, 99_000_000, "x"),
            sample(1, 3_000_000, "x"),
            sample(2, 1_000_000, "x"),
            sample(3, 2_000_000, "x"),
            sample(4, 5_000_000, "x"),
            sample(5, 4_000_000, "y"),
        ];
        let side = image_side(&samples, None, 0, Mode::Full);
        assert_eq!(side.median_ms, Some(3.0));
        assert!(!side.stable, "one rep decoded other text");
        assert_eq!(
            (side.heap_peak, side.alloc_calls, side.retained, side.errors),
            (Some(1000), Some(10), Some(16), 0)
        );
        assert_eq!(side.transforms, Some(3.0));
        let steady = image_side(&samples[..5], None, 0, Mode::Full);
        assert!(steady.stable);
        let mut odd_warm_up = samples[..5].to_vec();
        odd_warm_up[0] = sample(0, 99_000_000, "w");
        assert!(
            !image_side(&odd_warm_up, None, 0, Mode::Full).stable,
            "the warm-up's output counts toward stability (every call)"
        );
    }

    /// One of five timed calls killed (`signal 9`, 30 s) and one erroring.
    /// Neither enters the median, the RSS, the overruns; the pair leaves
    /// the paired tests with its reason. A warm-up error excludes the pair
    /// too, and a budgeted pair needs its reference walks.
    #[test]
    fn failed_and_erroring_calls_never_become_samples() {
        let reference = sample(0, 2_000_000, "x");
        let side_of = |samples: &[Sample], failures: usize| {
            image_side(samples, Some(&reference), failures, Mode::Frame)
        };
        let mut errored = sample(3, 30_000_000_000, "x");
        errored.units = None;
        errored.error = Some(String::from("QRS-001"));
        let samples = vec![
            sample(0, 9_000_000, "x"),
            sample(1, 3_000_000, "x"),
            sample(2, 1_000_000, "x"),
            errored.clone(),
            sample(4, 5_000_000, "x"),
        ];
        let side = side_of(&samples, 1);
        assert_eq!(side.median_ms, Some(3.0), "the 30 s error is not a latency");
        assert_eq!(
            (side.timed, side.ok, side.errors, side.failures),
            (4, 3, 1, 1)
        );
        assert_eq!(side.over_budget_calls, 0, "an error is not an overrun");
        assert!(!side.complete(5));
        let runs = |reps: u32, text: &str| {
            (0..=reps)
                .map(|r| sample(r, 2_000_000, text))
                .collect::<Vec<_>>()
        };
        let clean = side_of(&runs(5, "x"), 0);
        assert!(clean.complete(5));
        let status = |b: &ImageSide| pair_status(&clean, b, 5, Mode::Frame);
        assert_eq!(status(&side), Err(Excluded::Failure));
        assert_eq!(status(&side_of(&samples, 0)), Err(Excluded::Error));
        let mut warm_error = runs(5, "x");
        warm_error[0] = Sample { rep: 0, ..errored };
        assert_eq!(
            status(&side_of(&warm_error, 0)),
            Err(Excluded::Error),
            "a warm-up error is an error exclusion"
        );
        assert_eq!(
            status(&side_of(&runs(3, "x"), 0)),
            Err(Excluded::Incomplete)
        );
        assert_eq!(
            status(&side_of(&runs(5, "z"), 0)),
            Err(Excluded::Disagreement)
        );
        let mut flip = runs(5, "x");
        flip[5] = sample(5, 2_000_000, "z");
        assert_eq!(status(&side_of(&flip, 0)), Err(Excluded::Unstable));
        assert_eq!(status(&clean), Ok(()));
        let unreferenced = image_side(&runs(5, "x"), None, 0, Mode::Frame);
        assert_eq!(status(&unreferenced), Err(Excluded::NoReference));
        assert_eq!(
            pair_status(&unreferenced, &unreferenced, 5, Mode::FullUnbounded),
            Ok(()),
            "an unbudgeted mode has no reference walk to need"
        );
    }

    #[test]
    fn classes_are_groups_then_sets() {
        let image = |set, group: &str, path: &str| Image {
            set,
            group: group.to_owned(),
            path: path.to_owned(),
            abs: std::path::PathBuf::from(path),
            sha256: String::new(),
            bytes: 0,
        };
        let images = vec![
            image(Set::Zxing, "zxing-blackbox/qrcode-1", "a"),
            image(Set::Zxing, "zxing-blackbox/qrcode-2", "b"),
            image(Set::Vendored, "vendored/clean", "c"),
        ];
        let names: Vec<(String, &str, Vec<usize>)> = classes(&images)
            .into_iter()
            .map(|c| (c.name, c.level, c.members))
            .collect();
        assert_eq!(
            names,
            vec![
                (String::from("zxing-blackbox/qrcode-1"), "group", vec![0]),
                (String::from("zxing-blackbox/qrcode-2"), "group", vec![1]),
                (String::from("vendored/clean"), "group", vec![2]),
                (String::from("zxing"), "set", vec![0, 1]),
                (String::from("vendored"), "set", vec![2]),
            ]
        );
    }

    #[test]
    fn percent_cells_never_read_minus_zero() {
        assert_eq!(pct_cell(-0.04), "+0.0");
        assert_eq!(pct_cell(-0.0), "+0.0");
        assert_eq!(pct_cell(5.0), "+5.0");
        assert_eq!(pct_cell(-12.34), "-12.3");
    }

    #[test]
    fn labels_put_failures_first_and_dry_runs_next() {
        assert_eq!(label(Verdict::Regression, false, true), "exploratory");
        assert_eq!(label(Verdict::Regression, false, false), "regression");
        assert_eq!(label(Verdict::NoRegression, true, true), "failed");
        assert_eq!(label(Verdict::NotTestable, false, false), "not_testable");
    }

    // ---- a synthetic run: images, segments, and the status table ----

    fn image(i: usize, set: Set) -> Image {
        Image {
            set,
            group: format!("{}/g", set.as_str()),
            path: format!("{}/{i}.png", set.as_str()),
            abs: std::path::PathBuf::from(format!("/nonexistent/{i}.png")),
            sha256: format!("{i:064}"),
            bytes: 1,
        }
    }

    fn facts() -> env::Facts {
        env::Facts {
            date_utc: String::from("2026-10-10T00:00:00Z"),
            host_model: String::new(),
            cpu: String::new(),
            cores_logical: 12,
            cores_physical: String::new(),
            cores_performance: String::new(),
            cores_efficiency: String::new(),
            ram_bytes: None,
            os: String::new(),
            kernel: String::new(),
            power: env::Power {
                source: String::from("AC Power"),
                battery_percent: None,
                low_power_mode: Some(false),
            },
            rustc: String::new(),
            cargo_toolchain: String::new(),
            harness_profile: "debug",
            rayon_num_threads: None,
            node: String::new(),
            source: env::Source {
                git_sha: String::from("x"),
                tree: String::from("clean"),
            },
            harness_sha256: String::from("h"),
        }
    }

    fn options(args: &[&str]) -> Options {
        parse_options(args.iter().map(|a| (*a).to_owned()).collect()).expect("options")
    }

    fn complete_scope() -> Vec<&'static str> {
        let mut args = vec!["--out", "/nonexistent/out"];
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
        args
    }

    /// `n` images; per image a base median `base_ms` and a candidate median
    /// `ratio` times it (frame mode, all calls completed and agreeing, every
    /// walk the reference walk).
    fn segment(n: usize, base_ms: f64, ratio: f64) -> Segment {
        let mut samples = BTreeMap::new();
        let mut references = BTreeMap::new();
        for i in 0..n {
            let mut variants = BTreeMap::new();
            for (variant, scale) in [(Variant::Base, 1.0), (Variant::Candidate, ratio)] {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "test durations"
                )]
                let ns = |k: f64| (base_ms * scale * (1.0 + k / 1000.0) * 1e6) as u64;
                let calls = (0..=5)
                    .map(|rep| {
                        let mut s = sample(rep, ns(f64::from(rep)), "x");
                        s.total_ms = Some(1.0);
                        s
                    })
                    .collect();
                variants.insert(variant, calls);
            }
            samples.insert(i, variants);
            references.insert(
                i,
                Variant::ALL
                    .iter()
                    .map(|v| (*v, sample(0, 1, "x")))
                    .collect(),
            );
        }
        Segment {
            runtime: Runtime::Lib,
            mode: Mode::Frame,
            outcome: Outcome::Measured,
            note: None,
            attempts: 1,
            readings: Vec::new(),
            references,
            samples,
            failures: Vec::new(),
            processes: BTreeMap::new(),
        }
    }

    /// One call of a model: outer `ms`, the walk (stages, Σ transforms),
    /// the decoded text (none when empty) and the judgment signature.
    fn call(
        rep: u32,
        ms: f64,
        (stages, transforms): (u32, u64),
        text: &str,
        judged: Option<&str>,
    ) -> Sample {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test durations"
        )]
        let ns = (ms * 1e6) as u64;
        Sample {
            rep,
            slot: rep,
            ns,
            total_ms: Some(ms),
            engine_panics: Some(0),
            units: Some(if text.is_empty() {
                Vec::new()
            } else {
                vec![Unit::of("qr_code", text.as_bytes())]
            }),
            work: Some(Work {
                stages,
                transforms,
                judgment: judged.map(str::to_owned),
            }),
            ..Sample::default()
        }
    }

    /// An image's calls of one variant: a warm-up (the first call's twin)
    /// and one timed call per entry, all of one walk.
    fn calls(ms: &[f64], walk: (u32, u64), text: &str, judged: Option<&str>) -> Vec<Sample> {
        std::iter::once(call(0, ms[0], walk, text, judged))
            .chain(
                (1u32..)
                    .zip(ms)
                    .map(|(rep, m)| call(rep, *m, walk, text, judged)),
            )
            .collect()
    }

    /// A segment of `mode` from per-image [base, candidate] (calls,
    /// reference walk).
    fn model(mode: Mode, images: Vec<[(Vec<Sample>, Sample); 2]>) -> Segment {
        let mut segment = segment(0, 1.0, 1.0);
        segment.mode = mode;
        for (i, [base, candidate]) in images.into_iter().enumerate() {
            let mut calls = BTreeMap::new();
            let mut references = BTreeMap::new();
            for (variant, (samples, reference)) in
                [(Variant::Base, base), (Variant::Candidate, candidate)]
            {
                calls.insert(variant, samples);
                references.insert(variant, reference);
            }
            segment.samples.insert(i, calls);
            segment.references.insert(i, references);
        }
        segment
    }

    fn row<'a>(declared: &'a [Declared], class: &str, metric: &str) -> &'a Declared {
        declared
            .iter()
            .find(|d| d.class == class && d.metric == metric)
            .unwrap_or_else(|| panic!("no {metric} row for {class}"))
    }

    fn measurement(args: &[&str], images: usize, segments: Vec<Segment>) -> Measurement {
        let options = options(args);
        Measurement {
            images: (0..images).map(|i| image(i, Set::Vendored)).collect(),
            inputs: Inputs {
                manifest_sha256: String::new(),
                vendored_sha256: String::new(),
                dispositions_sha256: String::new(),
                corpus_root: "none",
                available: BTreeMap::new(),
                selected: BTreeMap::new(),
                per_group: options.per_group,
                pins: Vec::new(),
            },
            options,
            facts: facts(),
            artifacts: Vec::new(),
            hellos: BTreeMap::new(),
            parity: Vec::new(),
            threshold: 6.0,
            readings: Vec::new(),
            segments,
            throughput: Vec::new(),
            wasm: None,
            refused: None,
            failures: Vec::new(),
            stopped: None,
        }
    }

    fn declared_of(m: &Measurement) -> Vec<Declared> {
        let mut declared = Vec::new();
        let mut tables = Tables::default();
        for s in &m.segments {
            segment_json(m, s, m.options.dry_run, &mut declared, &mut tables);
        }
        declared
    }

    fn verdict_of(declared: &[Declared], class: &str, metric: &str) -> &'static str {
        declared
            .iter()
            .find(|d| d.class == class && d.metric == metric)
            .map_or("missing", |d| d.verdict)
    }

    fn throughput_row(threads: usize) -> Throughput {
        let pass = |variant: &'static str| crate::bench::run::Pass {
            variant,
            jobs: 10,
            ns: 1_000_000_000,
            failed: 0,
        };
        Throughput {
            mode: Mode::Full,
            threads,
            bound: 2 * threads,
            wall_cap: std::time::Duration::from_secs(60),
            rss_cap: crate::bench::run::throughput_cap(threads, 1024),
            passes: vec![
                pass("base"),
                pass("candidate"),
                pass("candidate"),
                pass("base"),
            ],
            failures: Vec::new(),
            outcome: Outcome::Measured,
            note: None,
            readings: Vec::new(),
            processes: BTreeMap::new(),
        }
    }

    /// A WASM cost: the base's sizes (gzip 10 000, brotli 8 000), the
    /// candidate's `gzip` and `brotli`, and 12 + 12 cold starts after a
    /// warm-up each, every part of the candidate's `ratio` times the base's.
    fn wasm_cost((gzip, brotli): (u64, u64), ratio: f64) -> WasmCost {
        let sizes = |gzip9, brotli11| crate::bench::run::Sizes {
            raw: 20_000,
            gzip9,
            brotli11,
            glue: 10,
        };
        let colds = |scale: f64| -> Vec<crate::bench::run::Cold> {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "test durations"
            )]
            let scaled = |ns: u32| (f64::from(ns) * scale) as u64;
            (0..=COLD_PAIRS)
                .map(|rep| crate::bench::run::Cold {
                    rep,
                    compile_ns: scaled(2_000_000),
                    instantiate_ns: scaled(500_000),
                    first_scan_ns: scaled(80_000_000 + rep * 1_000_000),
                    memory_bytes: 1 << 20,
                    peak_rss: Some(100 << 20),
                })
                .collect()
        };
        WasmCost {
            sizes: BTreeMap::from([
                (Variant::Base, sizes(10_000, 8_000)),
                (Variant::Candidate, sizes(gzip, brotli)),
            ]),
            runs: BTreeMap::from([
                (Variant::Base, colds(1.0)),
                (Variant::Candidate, colds(ratio)),
            ]),
            failures: Vec::new(),
            outcome: Outcome::Measured,
            note: None,
            readings: Vec::new(),
        }
    }

    /// The whole declared scope: every runtime × mode segment, `adjust`ed,
    /// the three thread counts of the `full` throughput pass on a 12-core
    /// host, and the WASM cost of both variants.
    fn whole(adjust: impl Fn(&mut Segment)) -> Measurement {
        let mut segments = Vec::new();
        for &runtime in Runtime::ALL {
            for &mode in Mode::ALL {
                let mut s = segment(10, 10.0, 1.0);
                s.runtime = runtime;
                s.mode = mode;
                adjust(&mut s);
                segments.push(s);
            }
        }
        let mut m = measurement(&complete_scope(), 10, segments);
        m.throughput = thread_counts(12).into_iter().map(throughput_row).collect();
        m.wasm = Some(wasm_cost((10_000, 8_000), 1.0));
        m
    }

    /// Every row of the status table with its exit code. Completeness is
    /// derived from the declared scope: one segment alone, a missing thread
    /// count or a missing WASM cost leave the run partial.
    #[test]
    fn every_status_row_has_its_exit_code() {
        let full = complete_scope();
        let row = |m: &Measurement| status(m, &declared_of(m));

        let completed = whole(|_| {});
        assert_eq!(row(&completed), ("completed", 0));
        assert!(missing(&completed).is_empty());

        let one_segment = measurement(&full, 10, vec![segment(10, 10.0, 1.0)]);
        assert_eq!(row(&one_segment), ("partial", 3), "1 of 16 segments");
        assert_eq!(missing(&one_segment).len(), 15 + 3 + 1);
        let mut no_count = whole(|_| {});
        no_count.throughput.pop();
        assert_eq!(
            row(&no_count),
            ("partial", 3),
            "the 12-thread pass is missing"
        );
        assert_eq!(
            missing(&no_count),
            vec![String::from("throughput full/12 not selected")]
        );
        let mut no_wasm = whole(|_| {});
        no_wasm.wasm = None;
        assert_eq!(row(&no_wasm), ("partial", 3));

        let regression = whole(|s| {
            if (s.runtime, s.mode) == (Runtime::Cli, Mode::Fast) {
                *s = Segment {
                    runtime: Runtime::Cli,
                    mode: Mode::Fast,
                    ..segment(10, 10.0, 1.2)
                };
            }
        });
        assert_eq!(row(&regression), ("regression", 1));

        let mut subset_args = full.clone();
        subset_args.extend(["--per-group", "1"]);
        let mut subset = whole(|_| {});
        subset.options = options(&subset_args);
        assert_eq!(row(&subset), ("partial", 3));

        let discarded = whole(|s| {
            if (s.runtime, s.mode) == (Runtime::Node, Mode::Frame) {
                s.outcome = Outcome::Discarded;
            }
        });
        assert_eq!(row(&discarded), ("partial", 3));
        assert_eq!(
            missing(&discarded),
            vec![String::from("node/frame discarded")]
        );

        let mut dry_args = full.clone();
        dry_args.push("--dry-run");
        assert_eq!(
            row(&measurement(&dry_args, 10, vec![segment(10, 10.0, 1.2)])),
            ("exploratory", 3)
        );

        let rss = Failure {
            kind: Kind::Rss,
            peak_rss: Some(1 << 30),
            wall_ns: 1_600_000_000,
        };
        let killed = whole(|s| {
            if (s.runtime, s.mode) == (Runtime::Lib, Mode::Full) {
                s.failures.push(CallFailure {
                    image: 3,
                    variant: Variant::Candidate,
                    rep: 2,
                    slot: 4,
                    attempt: 1,
                    reference: false,
                    failure: rss,
                });
            }
        });
        assert_eq!(row(&killed), ("failed", 1));
        let mut killed_dry = segment(10, 10.0, 1.0);
        killed_dry.failures = vec![CallFailure {
            image: 0,
            variant: Variant::Base,
            rep: 0,
            slot: 0,
            attempt: 1,
            reference: true,
            failure: Failure {
                kind: Kind::Wall,
                peak_rss: None,
                wall_ns: 60_000_000_000,
            },
        }];
        assert_eq!(
            row(&measurement(&dry_args, 10, vec![killed_dry])),
            ("failed", 1),
            "caps apply to dry runs and reference walks, and so does their verdict"
        );

        let protocol = whole(|s| {
            if (s.runtime, s.mode) == (Runtime::Wasm, Mode::Fast) {
                s.outcome = Outcome::Failed;
            }
        });
        assert_eq!(row(&protocol), ("failed", 1));
    }

    /// The refusal rows: the gate and the host floor 3, an unfit pair and
    /// inputs off their pins 2; a run whose inputs stray from the pins can
    /// never end completed.
    #[test]
    fn refusals_have_their_exit_codes() {
        let row = |m: &Measurement| status(m, &declared_of(m));
        let mut refused = measurement(&complete_scope(), 10, Vec::new());
        for (refusal, exit) in [
            (Refusal::Gate(String::from("closed")), 3),
            (Refusal::Floor(String::from("load1 above 30")), 3),
            (Refusal::Parity(String::from("compilers differ")), 2),
            (Refusal::Inputs(String::from("corpus.toml off its pin")), 2),
        ] {
            refused.refused = Some(refusal);
            assert_eq!(row(&refused), ("refused", exit));
        }
        let mut off_pins = whole(|_| {});
        off_pins.inputs.pins = vec![String::from("vendored: 40 images listed, not 41")];
        assert_eq!(row(&off_pins), ("partial", 3));
    }

    /// A set row with more than 10 % of its images excluded, or fewer than
    /// 6 valid pairs, ends the whole run partial, and an A/B disagreement
    /// in a budgeted mode is named in the status line with the excluded
    /// counts.
    #[test]
    fn exclusions_reach_the_status() {
        let disagree = |s: &mut Segment, images: &[usize]| {
            for i in images {
                for sample in s
                    .samples
                    .get_mut(i)
                    .and_then(|v| v.get_mut(&Variant::Candidate))
                    .expect("image")
                {
                    sample.units = Some(vec![Unit::of("qr_code", b"other")]);
                }
            }
        };
        // one excluded image of ten: within the 10 %, still complete
        let one = whole(|s| {
            if (s.runtime, s.mode) == (Runtime::Lib, Mode::Frame) {
                disagree(s, &[0]);
            }
        });
        assert_eq!(status(&one, &declared_of(&one)), ("completed", 0));
        let rows = set_rows(&one);
        assert!(rows.breaches.is_empty());
        assert_eq!(
            rows.budgeted_disagreements,
            vec![String::from("lib/frame vendored 1")]
        );
        let (_, line) = status_detail(&one, &BTreeMap::from([(String::from("disagreement"), 1)]));
        assert_eq!(
            line,
            "excluded pairs: disagreement 1 · budgeted-mode A/B disagreement: lib/frame vendored 1"
        );

        // two of ten: above 10 %
        let two = whole(|s| {
            if (s.runtime, s.mode) == (Runtime::Cli, Mode::FullUnbounded) {
                disagree(s, &[0, 1]);
            }
        });
        assert_eq!(status(&two, &declared_of(&two)), ("partial", 3));
        assert_eq!(
            set_rows(&two).breaches,
            vec![String::from(
                "cli/full-unbounded vendored: 2 of 10 images excluded"
            )]
        );

        // five valid pairs left of ten: fewer than six
        let five = whole(|s| {
            if (s.runtime, s.mode) == (Runtime::Node, Mode::Full) {
                disagree(s, &[0, 1, 2, 3, 4]);
            }
        });
        assert_eq!(
            set_rows(&five).breaches,
            vec![
                String::from("node/full vendored: 5 valid pairs"),
                String::from("node/full vendored: 5 of 10 images excluded"),
            ]
        );
    }

    /// Every metric of the frozen scope a run did not measure is declared,
    /// not testable, with why — 209 rows for one lib/frame segment on the
    /// vendored set.
    #[test]
    fn unmeasured_scope_is_declared_not_testable() {
        let m = measurement(&complete_scope(), 10, vec![segment(10, 10.0, 1.0)]);
        let mut declared = Vec::new();
        unmeasured_rows(&m, &mut declared);
        assert_eq!(declared.len(), 209);
        assert!(declared.iter().all(|d| d.verdict == "not_testable"));
        let note = |target: &str, class: &str, metric: &str| {
            declared
                .iter()
                .find(|d| d.target == target && d.class == class && d.metric == metric)
                .and_then(|d| d.note.clone())
        };
        assert_eq!(
            note("cli/full", "gallery", "lost_judgment").as_deref(),
            Some("not selected")
        );
        assert_eq!(
            note("lib/frame", "zxing", "truncation").as_deref(),
            Some("set not selected")
        );
        assert_eq!(
            note("lib/frame", "vendored", "truncation"),
            None,
            "measured"
        );
        assert_eq!(
            note("wasm", "package", "cold_start_p50").as_deref(),
            Some("wasm not selected")
        );
        assert_eq!(
            note("node/full-unbounded", "zxing", "peak_memory").as_deref(),
            Some("not selected")
        );
    }

    /// A class with a resource failure is failed — every verdict of it;
    /// the other classes keep theirs.
    #[test]
    fn a_resource_failure_fails_its_class() {
        let mut killed = segment(10, 10.0, 1.0);
        killed.failures.push(CallFailure {
            image: 3,
            variant: Variant::Candidate,
            rep: 2,
            slot: 4,
            attempt: 1,
            reference: false,
            failure: Failure {
                kind: Kind::Signal(9),
                peak_rss: None,
                wall_ns: 1,
            },
        });
        killed.samples.get_mut(&3).expect("image 3").insert(
            Variant::Candidate,
            (0..=1).map(|r| sample(r, 1_000_000, "x")).collect(),
        );
        let m = measurement(&complete_scope(), 10, vec![killed]);
        let declared = declared_of(&m);
        assert_eq!(verdict_of(&declared, "vendored", "latency_p50"), "failed");
        for metric in ["latency_p95_tail", "truncation", "work"] {
            assert_eq!(
                verdict_of(&declared, "vendored/g", metric),
                "failed",
                "{metric}"
            );
        }
    }

    /// Nine valid pairs at +20 % and one disagreeing pair: the regression
    /// is declared over the nine; the excluded pair is counted.
    #[test]
    fn disagreeing_pairs_leave_the_tests_and_are_counted() {
        let mut seg = segment(10, 10.0, 1.2);
        for sample in seg
            .samples
            .get_mut(&0)
            .and_then(|v| v.get_mut(&Variant::Candidate))
            .expect("image 0")
        {
            sample.units = Some(vec![Unit::of("qr_code", b"other")]);
        }
        let m = measurement(&complete_scope(), 10, vec![seg]);
        let mut declared = Vec::new();
        let mut tables = Tables::default();
        let json = segment_json(&m, &m.segments[0], false, &mut declared, &mut tables);
        let set = json["classes"]
            .as_array()
            .and_then(|c| c.iter().find(|c| c["class"] == "vendored"))
            .expect("set row");
        assert_eq!(set["pairs"]["valid"], 9);
        assert_eq!(set["pairs"]["excluded"]["disagreement"], 1);
        assert_eq!(set["latency"]["p50"]["pairs"], 9);
        assert_eq!(
            verdict_of(&declared, "vendored", "latency_p50"),
            "regression"
        );
        assert_eq!(excluded_totals(&[json]).get("disagreement"), Some(&1));
    }

    /// The jitter of the synthetic cases below: ±2 %, no RNG.
    fn jitter(i: usize, rep: u32) -> f64 {
        #[expect(clippy::cast_precision_loss, reason = "image indices are tiny")]
        let i = i as f64;
        1.0 + 0.02 * (7.0 * i + 3.0 * f64::from(rep)).sin()
    }

    fn reps(f: impl Fn(u32) -> f64) -> Vec<f64> {
        (1..=5).map(f).collect()
    }

    /// A slowdown that hides behind the budget: 18 frame images whose
    /// complete walk (2 stages, 2 transforms, nothing decoded) ends past
    /// the 80 ms budget on both sides, the candidate 30 % slower; 12 fast
    /// images alike. Counting every call that reaches the budget as cut
    /// would make the class budget-bound with work +0.0 %, hiding the
    /// slowdown. Every walk is the reference walk, so all 30 pairs are
    /// complete and latency judges them.
    #[test]
    fn a_slowdown_on_complete_walks_is_a_latency_regression() {
        let images = (0..30)
            .map(|i| {
                if i < 18 {
                    let t = |rep| {
                        (84.0 + 3.0 * f64::from(u32::try_from(i % 6).unwrap_or(0))) * jitter(i, rep)
                    };
                    let reference = call(0, 1.0, (2, 2), "", None);
                    [
                        (
                            calls(&reps(|r| t(r) + 5.0), (2, 2), "", None),
                            reference.clone(),
                        ),
                        (
                            calls(&reps(|r| 1.3 * (t(r) + 5.0)), (2, 2), "", None),
                            reference,
                        ),
                    ]
                } else {
                    let t =
                        |rep| (4.0 + f64::from(u32::try_from(i % 5).unwrap_or(0))) * jitter(i, rep);
                    let text = format!("qr{i}");
                    let side = || {
                        (
                            calls(&reps(|r| t(r) + 4.0), (1, 1), &text, None),
                            call(0, 1.0, (1, 1), &text, None),
                        )
                    };
                    [side(), side()]
                }
            })
            .collect();
        let m = measurement(&complete_scope(), 30, vec![model(Mode::Frame, images)]);
        let declared = declared_of(&m);
        let p50 = row(&declared, "vendored", "latency_p50");
        assert_eq!(
            (p50.verdict, p50.estimate_pct, p50.ci_pct),
            ("regression", Some(14.018), Some([14.018, 30.0]))
        );
        let cut = row(&declared, "vendored", "truncation");
        assert_eq!(
            (cut.verdict, cut.discordant, cut.p_value),
            ("no_regression", Some([0, 0]), Some(1.0))
        );
        assert_eq!(
            row(&declared, "vendored", "work").verdict,
            "not_testable",
            "no truncated pair"
        );
        assert_eq!(status(&m, &declared), ("regression", 1));
    }

    /// A faster candidate that walks less: 18 images the base decodes in
    /// `direct` (2 transforms) in the attempt that crosses 80 ms, the
    /// candidate in `pyramid` (1 transform) at 40 ms. Counting the base's
    /// calls that reach the budget as cut would invent a work regression
    /// (−29.3 %); both walks are their own reference walks, so nothing is
    /// truncated and the faster candidate is no regression.
    #[test]
    fn a_faster_candidate_is_no_regression() {
        let images = (0..30)
            .map(|i| {
                let text = format!("qr{i}");
                let fast =
                    |rep| (4.0 + f64::from(u32::try_from(i % 5).unwrap_or(0))) * jitter(i, rep);
                let base = if i < 18 {
                    let t = |rep| {
                        (82.0 + 2.0 * f64::from(u32::try_from(i % 6).unwrap_or(0))) * jitter(i, rep)
                    };
                    (
                        calls(&reps(|r| t(r) + 5.0), (2, 2), &text, None),
                        call(0, 1.0, (2, 2), &text, None),
                    )
                } else {
                    (
                        calls(&reps(|r| fast(r) + 4.0), (1, 1), &text, None),
                        call(0, 1.0, (1, 1), &text, None),
                    )
                };
                let candidate = if i < 18 {
                    calls(&reps(|r| 40.0 * jitter(i, r) + 5.0), (1, 1), &text, None)
                } else {
                    calls(&reps(|r| fast(r) + 4.0), (1, 1), &text, None)
                };
                [base, (candidate, call(0, 1.0, (1, 1), &text, None))]
            })
            .collect();
        let m = measurement(&complete_scope(), 30, vec![model(Mode::Frame, images)]);
        let declared = declared_of(&m);
        let p50 = row(&declared, "vendored", "latency_p50");
        assert_eq!(
            (p50.verdict, p50.estimate_pct, p50.ci_pct),
            ("no_regression", Some(-31.159), Some([-49.957, -28.893]))
        );
        assert_eq!(
            row(&declared, "vendored", "truncation").verdict,
            "no_regression"
        );
        assert_eq!(row(&declared, "vendored", "work").verdict, "not_testable");
        assert!(
            declared.iter().all(|d| d.verdict != "regression"),
            "nothing invented"
        );
    }

    /// A self-hiding slowdown: ten frame images whose complete walk takes
    /// 60–78 ms in the base and SLOW × that in the candidate, outer =
    /// ladder + 5 ms. Truncation read from the clock would hide ×1.3
    /// (+27.96 %) and ×3.0 behind `budget_bound`; on complete pairs each is
    /// a latency regression.
    #[test]
    fn self_hiding_slowdowns_are_latency_regressions() {
        for (slow, estimate, ci) in [
            (1.1, 9.321, [9.275, 9.365]),
            (1.3, 27.964, [27.826, 28.096]),
            (3.0, 186.423, [185.507, 187.309]),
        ] {
            let images = (0..10)
                .map(|i| {
                    let t = 60.0 + 2.0 * f64::from(i);
                    let reference = call(0, 1.0, (2, 2), "", None);
                    [
                        (calls(&[t + 5.0; 5], (2, 2), "", None), reference.clone()),
                        (calls(&[t * slow + 5.0; 5], (2, 2), "", None), reference),
                    ]
                })
                .collect();
            let m = measurement(&complete_scope(), 10, vec![model(Mode::Frame, images)]);
            let declared = declared_of(&m);
            let p50 = row(&declared, "vendored", "latency_p50");
            assert_eq!(
                (p50.verdict, p50.estimate_pct, p50.ci_pct),
                ("regression", Some(estimate), Some(ci)),
                "×{slow}"
            );
        }
    }

    /// A judgment cut at a 4 s budget (full): ladder 1 s, the base's
    /// judgment 2.0–2.9 s, the candidate's SLOW × slower; past the deadline
    /// this tree drops the judgment and the call ends at 4005 ms. Judged on
    /// work alone, the class would read budget-bound with no regression;
    /// the cut judgments are truncations: ×1.2 cuts 5 of 10 images, the
    /// exact one-sided p is 1/32 for both tests; ×1.5 cuts all 10
    /// (p 1/1024).
    #[test]
    fn lost_judgments_are_truncation_regressions() {
        let full = "v80w100,5/5";
        let build = |slow: f64| -> Vec<[(Vec<Sample>, Sample); 2]> {
            (0..10)
                .map(|i| {
                    let text = format!("qr{i}");
                    let reference = call(0, 1.0, (3, 6), &text, Some(full));
                    let base = 3000.0 + 100.0 * f64::from(i);
                    let total = 1000.0 + (2000.0 + 100.0 * f64::from(i)) * slow;
                    let (ms, judged) = if total >= 4000.0 {
                        (4005.0, None)
                    } else {
                        (total, Some(full))
                    };
                    [
                        (
                            calls(&[base; 5], (3, 6), &text, Some(full)),
                            reference.clone(),
                        ),
                        (calls(&[ms; 5], (3, 6), &text, judged), reference),
                    ]
                })
                .collect()
        };
        let m = measurement(&complete_scope(), 10, vec![model(Mode::Full, build(1.2))]);
        let declared = declared_of(&m);
        let p50 = row(&declared, "vendored", "latency_p50");
        assert_eq!(
            (p50.verdict, p50.estimate_pct, p50.ci_pct),
            ("not_testable", Some(13.744), None),
            "five complete pairs"
        );
        for metric in ["truncation", "lost_judgment"] {
            let test = row(&declared, "vendored", metric);
            assert_eq!(
                (test.verdict, test.discordant, test.p_value),
                ("regression", Some([5, 0]), Some(0.031_25)),
                "{metric}"
            );
        }
        let work = row(&declared, "vendored", "work");
        assert_eq!(
            (work.verdict, work.estimate_pct),
            ("not_testable", Some(0.0))
        );
        assert_eq!(status(&m, &declared), ("regression", 1));

        let m = measurement(&complete_scope(), 10, vec![model(Mode::Full, build(1.5))]);
        let declared = declared_of(&m);
        assert_eq!(
            row(&declared, "vendored", "latency_p50").verdict,
            "not_testable"
        );
        let lost = row(&declared, "vendored", "lost_judgment");
        assert_eq!(
            (lost.verdict, lost.discordant, lost.p_value),
            ("regression", Some([10, 0]), Some(0.000_976_562_5))
        );
        let work = row(&declared, "vendored", "work");
        assert_eq!(
            (work.verdict, work.estimate_pct, work.ci_pct),
            ("no_regression", Some(0.0), Some([0.0, 0.0])),
            "the ladder did the same work"
        );
    }

    /// Excluded images: 6 valid pairs, complete, the candidate 30 % slower,
    /// next to 6 images both variants truncate and disagree on. Labels read
    /// from every image would let the excluded calls make the class
    /// budget-bound and hide the regression; they come from the valid
    /// pairs.
    #[test]
    fn excluded_images_never_decide_a_verdict() {
        let mut images: Vec<[(Vec<Sample>, Sample); 2]> = (0..6)
            .map(|i| {
                let t = 40.0 + f64::from(i);
                let reference = call(0, 1.0, (2, 2), "", None);
                [
                    (calls(&[t; 5], (2, 2), "", None), reference.clone()),
                    (calls(&[t * 1.3; 5], (2, 2), "", None), reference),
                ]
            })
            .collect();
        for _ in 0..6 {
            images.push([
                (
                    calls(&[85.0; 5], (1, 1), "a", None),
                    call(0, 1.0, (2, 2), "a", None),
                ),
                (
                    calls(&[85.0; 5], (1, 1), "b", None),
                    call(0, 1.0, (2, 2), "b", None),
                ),
            ]);
        }
        let m = measurement(&complete_scope(), 12, vec![model(Mode::Frame, images)]);
        let mut declared = Vec::new();
        let json = segment_json(
            &m,
            &m.segments[0],
            false,
            &mut declared,
            &mut Tables::default(),
        );
        let p50 = row(&declared, "vendored", "latency_p50");
        assert_eq!(
            (p50.verdict, p50.estimate_pct, p50.ci_pct),
            ("regression", Some(30.0), Some([30.0, 30.0]))
        );
        let set = json["classes"]
            .as_array()
            .and_then(|c| c.iter().find(|c| c["class"] == "vendored"))
            .expect("set row");
        assert_eq!(
            (
                &set["pairs"]["valid"],
                &set["pairs"]["excluded"]["disagreement"],
                &set["budget_bound"]["pairs"]
            ),
            (&json!(6), &json!(6), &json!(0))
        );
        assert_eq!(
            set["base"]["budget"]["truncated"], 0,
            "counted over the valid pairs only"
        );
        assert_eq!(set["base"]["all_complete"]["budget"]["truncated"], 30);
    }

    /// An image's latency is complete when its median call is — two
    /// truncated calls among five (the slowest) leave it complete, three
    /// make it truncated; with four calls the middle two must both be
    /// complete.
    #[test]
    fn latency_reads_complete_pairs_by_their_median_call() {
        let reference = call(0, 1.0, (2, 4), "x", None);
        let side = |ms: &[(f64, bool)]| {
            let mut samples = vec![call(0, ms[0].0, (2, 4), "x", None)];
            for (rep, (m, cut)) in (1u32..).zip(ms) {
                let walk = if *cut { (2, 3) } else { (2, 4) };
                samples.push(call(rep, *m, walk, "x", None));
            }
            image_side(&samples, Some(&reference), 0, Mode::Frame)
        };
        let two = side(&[
            (10.0, false),
            (11.0, false),
            (12.0, false),
            (30.0, true),
            (31.0, true),
        ]);
        assert_eq!(
            (two.truncated, two.median_truncated, two.median_ms),
            (2, false, Some(12.0))
        );
        let three = side(&[
            (10.0, false),
            (11.0, false),
            (30.0, true),
            (31.0, true),
            (32.0, true),
        ]);
        assert_eq!((three.truncated, three.median_truncated), (3, true));
        let even = side(&[(10.0, false), (11.0, false), (30.0, true), (31.0, true)]);
        assert!(
            even.median_truncated,
            "the middle pair holds a truncated call"
        );

        // one-sided: six images only the base truncates are no regression;
        // six only the candidate truncates are (p = 1/64)
        let build = |base_cut: bool| -> Vec<[(Vec<Sample>, Sample); 2]> {
            (0..6)
                .map(|_| {
                    let reference = call(0, 1.0, (2, 4), "x", None);
                    let cut = calls(&[81.0; 5], (2, 3), "x", None);
                    let whole = calls(&[70.0; 5], (2, 4), "x", None);
                    let (a, b) = if base_cut { (cut, whole) } else { (whole, cut) };
                    [(a, reference.clone()), (b, reference)]
                })
                .collect()
        };
        for (base_cut, verdict, discordant, p) in [
            (true, "no_regression", [0, 6], 1.0),
            (false, "regression", [6, 0], 0.015_625),
        ] {
            let m = measurement(
                &complete_scope(),
                6,
                vec![model(Mode::Frame, build(base_cut))],
            );
            let declared = declared_of(&m);
            let test = row(&declared, "vendored", "truncation");
            assert_eq!(
                (test.verdict, test.discordant, test.p_value),
                (verdict, Some(discordant), Some(p))
            );
            assert_eq!(
                row(&declared, "vendored", "latency_p50").verdict,
                "not_testable",
                "no complete pair left"
            );
        }
    }

    /// Work is judged on the pairs where a side was truncated, and only
    /// there. Six complete pairs whose candidate reference walk is half the
    /// base's (a faster algorithm) stay out; six where the candidate
    /// stopped at 7 of the reference's 10 transforms give exactly −30 %.
    #[test]
    fn work_reads_truncated_pairs_only() {
        let mut images: Vec<[(Vec<Sample>, Sample); 2]> = (0..6)
            .map(|i| {
                let i = f64::from(i);
                [
                    (
                        calls(&[50.0 + i; 5], (2, 4), "x", None),
                        call(0, 1.0, (2, 4), "x", None),
                    ),
                    (
                        calls(&[30.0 + i; 5], (1, 2), "x", None),
                        call(0, 1.0, (1, 2), "x", None),
                    ),
                ]
            })
            .collect();
        for i in 0..6 {
            let i = f64::from(i);
            let reference = call(0, 1.0, (3, 10), "x", None);
            images.push([
                (calls(&[70.0 + i; 5], (3, 10), "x", None), reference.clone()),
                (calls(&[81.0 + i; 5], (3, 7), "x", None), reference),
            ]);
        }
        let m = measurement(&complete_scope(), 12, vec![model(Mode::Frame, images)]);
        let declared = declared_of(&m);
        let work = row(&declared, "vendored", "work");
        assert_eq!(
            (work.verdict, work.estimate_pct, work.ci_pct),
            ("regression", Some(-30.0), Some([-30.0, -30.0]))
        );
        let p50 = row(&declared, "vendored", "latency_p50");
        assert_eq!(
            (p50.verdict, p50.estimate_pct, p50.ci_pct),
            ("no_regression", Some(-38.136), Some([-40.0, -36.364])),
            "latency over the six complete pairs"
        );
        let cut = row(&declared, "vendored", "truncation");
        assert_eq!((cut.verdict, cut.discordant), ("regression", Some([6, 0])));
    }

    /// Memory median shift and tail, δ 10 % each, and the 512 MiB envelope
    /// flag — judged in full-unbounded only, information in a budgeted
    /// mode; node and wasm memory not testable.
    #[test]
    fn memory_is_judged_by_median_and_tail_with_an_envelope() {
        let grown = |mode: Mode| {
            let mut seg = segment(60, 10.0, 1.0);
            seg.mode = mode;
            for (i, variants) in &mut seg.samples {
                let candidate = variants.get_mut(&Variant::Candidate).expect("candidate");
                // the six largest images (by heap) grow by 50 %: a tail blow-up
                let base_peak = 1000 + 100 * (*i as u64);
                let peak = if *i >= 54 {
                    base_peak * 3 / 2
                } else {
                    base_peak
                };
                candidate[0].heap = Some(HeapStats {
                    peak,
                    calls: 10,
                    bytes: 2000,
                });
                let base = variants.get_mut(&Variant::Base).expect("base");
                base[0].heap = Some(HeapStats {
                    peak: base_peak,
                    calls: 10,
                    bytes: 2000,
                });
            }
            let big = seg
                .samples
                .get_mut(&0)
                .and_then(|v| v.get_mut(&Variant::Base))
                .expect("image 0");
            big[1].rss = Some(ENVELOPE + 1);
            seg
        };
        let set_of = |json: &Value| {
            json["classes"]
                .as_array()
                .and_then(|c| c.iter().find(|c| c["class"] == "vendored"))
                .cloned()
                .expect("set row")
        };
        let m = measurement(&complete_scope(), 60, vec![grown(Mode::FullUnbounded)]);
        let mut declared = Vec::new();
        let json = segment_json(
            &m,
            &m.segments[0],
            false,
            &mut declared,
            &mut Tables::default(),
        );
        assert_eq!(
            verdict_of(&declared, "vendored", "peak_heap_p50"),
            "no_regression"
        );
        assert_eq!(
            verdict_of(&declared, "vendored", "peak_heap_tail"),
            "regression"
        );
        let set = set_of(&json);
        assert_eq!(set["memory"]["tail"]["pairs"], 6);
        assert_eq!(set["memory"]["nonpositive_dropped"], 0);
        assert_eq!(set["base"]["over_envelope_calls"], 1);

        let m = measurement(&complete_scope(), 60, vec![grown(Mode::Frame)]);
        let mut declared = Vec::new();
        let json = segment_json(
            &m,
            &m.segments[0],
            false,
            &mut declared,
            &mut Tables::default(),
        );
        assert_eq!(
            verdict_of(&declared, "vendored", "peak_heap_tail"),
            "missing",
            "a budgeted mode declares no memory verdict"
        );
        let set = set_of(&json);
        assert!(set["memory"]["tail"]["verdict"].is_null());
        assert_eq!(
            set["memory"]["tail"]["shift"]["pairs"], 6,
            "kept as information"
        );

        for (mode, expected) in [
            (Mode::FullUnbounded, "not_testable"),
            (Mode::Frame, "missing"),
        ] {
            let mut node = segment(10, 10.0, 1.0);
            node.runtime = Runtime::Node;
            node.mode = mode;
            let m = measurement(&complete_scope(), 10, vec![node]);
            assert_eq!(
                verdict_of(&declared_of(&m), "vendored", "peak_memory"),
                expected
            );
        }
    }

    /// A capped incarnation's peak stays out of the A/B worker peak and its
    /// envelope flag (it is reported with its failure), and a worker a
    /// signal ended during `quit` fails the run though no call was in
    /// flight.
    #[test]
    fn capped_incarnations_and_quit_failures() {
        let end = |peak: u64, capped: bool, quit_failure: Option<Failure>| ProcessEnd {
            hello: String::new(),
            peak: Some(peak),
            farewell: String::from("bye"),
            ended: String::from(if capped { "rss" } else { "quit" }),
            capped,
            quit_failure,
            stderr: crate::bench::proc::Stderr::default(),
        };
        let mut seg = segment(10, 10.0, 1.0);
        seg.processes.insert(
            Variant::Base,
            vec![end(2 << 30, true, None), end(100 << 20, false, None)],
        );
        seg.processes
            .insert(Variant::Candidate, vec![end(120 << 20, false, None)]);
        let m = measurement(&complete_scope(), 10, vec![seg]);
        let mut declared = Vec::new();
        let json = segment_json(
            &m,
            &m.segments[0],
            false,
            &mut declared,
            &mut Tables::default(),
        );
        let base = &json["processes"]["base"];
        assert_eq!(
            (
                &base["peak_rss_bytes"],
                &base["capped"],
                &base["over_envelope"]
            ),
            (&json!(100 << 20), &json!(1), &json!(false))
        );
        assert_eq!(base["each"][0]["capped"], true);
        assert_eq!(
            status(&m, &declared),
            ("partial", 3),
            "nothing failed: one segment of the declared scope"
        );

        let mut seg = segment(10, 10.0, 1.0);
        let killed = Failure {
            kind: Kind::Signal(9),
            peak_rss: Some(100 << 20),
            wall_ns: 1,
        };
        seg.processes
            .insert(Variant::Base, vec![end(100 << 20, false, Some(killed))]);
        let m = measurement(&complete_scope(), 10, vec![seg]);
        assert_eq!(status(&m, &declared_of(&m)), ("failed", 1));
    }

    /// A runtime measured with one variant declares every δ metric of its
    /// target not testable, and the run cannot complete.
    #[test]
    fn one_variant_declares_not_testable() {
        let mut seg = segment(10, 10.0, 1.0);
        for variants in seg.samples.values_mut() {
            variants.remove(&Variant::Candidate);
        }
        let mut args = complete_scope();
        let at = args
            .iter()
            .position(|a| *a == "--lib-candidate")
            .expect("flag");
        args.drain(at..at + 2);
        let m = measurement(&args, 10, vec![seg]);
        let declared = declared_of(&m);
        for metric in ["latency_p50", "latency_p95_tail", "truncation", "work"] {
            assert_eq!(
                verdict_of(&declared, "vendored", metric),
                "not_testable",
                "{metric}"
            );
        }
        assert_eq!(
            verdict_of(&declared, "vendored", "lost_judgment"),
            "missing",
            "frame never scores"
        );
        assert_eq!(status(&m, &declared), ("partial", 3));
    }

    /// The WASM cost verdicts, on synthetic sizes and 12 + 12 cold starts
    /// after a warm-up each: a candidate 20 % slower in every cold start is
    /// a cold-start regression (status 1), an equal one is not (the whole
    /// scope completes), one failed cold start fails the run, a missing
    /// candidate leaves the size and cold-start rows not testable and the
    /// run partial, and the size δ reads +2.00 % as no regression and
    /// +2.01 % as one.
    #[test]
    fn wasm_costs_are_judged() {
        let judged = |cost: WasmCost| {
            let mut m = whole(|_| {});
            m.wasm = Some(cost);
            let mut rows = Vec::new();
            let json = wasm_json(
                &m,
                m.wasm.as_ref().expect("cost"),
                false,
                &mut rows,
                &mut String::new(),
            );
            let mut all = declared_of(&m);
            all.extend(rows.iter().cloned());
            (status(&m, &all), rows, json)
        };
        let row = |rows: &[Declared], metric: &str| {
            rows.iter()
                .find(|d| d.metric == metric)
                .map(|d| (d.verdict, d.estimate_pct, d.ci_pct))
        };

        let (state, rows, json) = judged(wasm_cost((10_000, 8_000), 1.2));
        assert_eq!(state, ("regression", 1));
        assert_eq!(
            row(&rows, "cold_start_p50"),
            Some(("regression", Some(20.0), Some([20.0, 20.0])))
        );
        assert_eq!(json["cold_start"]["shift"]["pairs"], 12);

        let (state, rows, _) = judged(wasm_cost((10_000, 8_000), 1.0));
        assert_eq!(state, ("completed", 0));
        assert_eq!(
            row(&rows, "cold_start_p50"),
            Some(("no_regression", Some(0.0), Some([0.0, 0.0])))
        );
        assert_eq!(row(&rows, "wasm_gzip9").map(|r| r.0), Some("no_regression"));

        let mut failed = wasm_cost((10_000, 8_000), 1.0);
        failed.failures.push((
            Variant::Candidate,
            Failure {
                kind: Kind::Wall,
                peak_rss: None,
                wall_ns: 60_000_000_000,
            },
        ));
        failed.outcome = Outcome::Failed;
        let (state, rows, _) = judged(failed);
        assert_eq!(state, ("failed", 1));
        assert_eq!(row(&rows, "cold_start_p50").map(|r| r.0), Some("failed"));

        let mut base_only = wasm_cost((10_000, 8_000), 1.0);
        base_only.sizes.remove(&Variant::Candidate);
        base_only.runs.remove(&Variant::Candidate);
        let (state, rows, _) = judged(base_only);
        assert_eq!(state, ("partial", 3));
        for metric in ["wasm_gzip9", "wasm_brotli11", "cold_start_p50"] {
            assert_eq!(
                row(&rows, metric).map(|r| r.0),
                Some("not_testable"),
                "{metric}"
            );
        }

        let (_, rows, _) = judged(wasm_cost((10_200, 8_160), 1.0));
        assert_eq!(
            row(&rows, "wasm_gzip9"),
            Some(("no_regression", Some(2.0), None)),
            "exactly +2.00 %"
        );
        let (state, rows, _) = judged(wasm_cost((10_201, 8_000), 1.0));
        assert_eq!(
            row(&rows, "wasm_gzip9"),
            Some(("regression", Some(2.01), None))
        );
        assert_eq!(state, ("regression", 1));
    }

    /// The throughput B/A ratio divides two unpaired rates — each variant's
    /// completed scans over its own pass time, failed jobs' time included —
    /// and says so. 10 of 10 jobs in 1 s against 8 of 10 read 10, 8 and
    /// 0.8.
    #[test]
    fn throughput_ratios_are_labelled_unpaired() {
        let mut run = throughput_row(4);
        for pass in run.passes.iter_mut().filter(|p| p.variant == "candidate") {
            pass.failed = 2;
        }
        let mut md = String::new();
        let rows = throughput_json(&[run], &mut md);
        assert_eq!(
            (
                &rows[0]["base_images_per_s"],
                &rows[0]["candidate_images_per_s"],
                &rows[0]["ratio"],
                &rows[0]["ratio_kind"]
            ),
            (&json!(10.0), &json!(8.0), &json!(0.8), &json!("unpaired"))
        );
        assert!(md.contains("| B/A (unpaired) |"), "{md}");
        assert!(md.contains("| 10.00 | 8.00 | 0.80 |"), "{md}");
    }

    /// The before / after row reads phases: the last `run start` reading
    /// (the one that opened the gate) and the `run end` reading.
    #[test]
    fn markdown_loads_come_from_phases() {
        let mut m = measurement(&complete_scope(), 1, Vec::new());
        let gate = crate::bench::gate::Gate::scripted(&[false, true, true]);
        for phase in ["run start", "run start", "run end"] {
            m.readings.push(gate.read(phase, 0.0));
        }
        m.readings[0].load = Some(crate::bench::gate::Load {
            one: 40.0,
            five: 40.0,
            fifteen: 40.0,
        });
        m.readings[1].load = Some(crate::bench::gate::Load {
            one: 2.0,
            five: 3.0,
            fifteen: 4.0,
        });
        m.readings[2].load = Some(crate::bench::gate::Load {
            one: 5.0,
            five: 6.0,
            fifteen: 7.0,
        });
        let head = markdown_head(&m, ("completed", 0, ""), &BTreeMap::new());
        assert!(
            head.contains(
                "| load at start / end | 2.00 3.00 4.00 / 5.00 6.00 7.00 (gate ceiling 6.00) |"
            ),
            "{head}"
        );
    }

    /// Receipts are created, never overwritten.
    #[test]
    fn receipt_files_are_created_once() {
        let dir = std::env::temp_dir().join(format!("qrscan-bench-put-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let sha = put(&dir, "receipt.json", "{}\n").expect("first write");
        assert_eq!(sha, crate::external::sha256_bytes(b"{}\n"));
        assert!(
            put(&dir, "receipt.json", "other").is_err(),
            "an existing receipt stays"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("receipt.json")).expect("read"),
            "{}\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
