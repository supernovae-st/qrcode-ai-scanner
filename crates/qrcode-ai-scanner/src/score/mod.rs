//! The score (spec/04-score.md) — survival ramps · caps · UEC · hints.
//!
//! Output language: VALIDATION, never ISO verification (no calibrated
//! optics). Geometry-class signals (perspective/rotation survival, finder
//! integrity, quiet zone) transfer to uncalibrated digital images;
//! reflectance-class signals (contrast/lighting survival) are relative.
//!
//! Determinism: every stress cell is a pure transform of the normalized
//! luma; ramps stop at the first failure (the knee). The lighting set is
//! unordered — no knee-exit, though depth still picks the cell subset.
//! Same input + same depth ⇒ same score, as far as the decode engines are
//! deterministic. Known exception until fixed upstream: rxing's PDF417
//! decoder breaks codeword-confidence ties in `HashMap` iteration order
//! (randomized per map), so a PDF417 cell near a decode threshold can pass
//! on one run and fail on the next (`pdf417_*_repeatability` diagnostics).
//!
//! Budget: a judgment runs its calibration and EVERY planned cell, or it is
//! absent. An unrun cell is not a failed one — read as a failure, a
//! deadline cut would ship a pristine symbol as a fragile verdict. Only the
//! deadline (or the test-only work quota) makes a judgment absent; a
//! cancelled token stays an error (`QRS-005`, the scan-wide contract). The
//! bound remembers every step it refused — an absent judgment, or a knee
//! bisection a complete one could not run — and the report says the budget
//! cut planned work (`trace.budget_exhausted`).

pub(crate) mod iso15415;
pub(crate) mod structural;
pub(crate) mod uec;
pub(crate) mod warp;

use web_time::Instant;

use crate::engine::{self, content::ContentKey};
use crate::error::Result;
use crate::input::LumaImage;
use crate::ladder::{CancelToken, MergedDetection, ScoreDepth};
use crate::report::{AxisScore, EcLevel, Grade, Hint, Score, StressAxis};
use crate::transform;

/// A stress-cell builder: base image + ramp index → transformed cell.
type RampBuilder<'a> = &'a dyn Fn(&LumaImage, usize) -> LumaImage;
/// Builds the ONE bisection cell between the knee and its lower neighbour
/// (the unstressed base for a knee at index 0) + its wire label.
type RampRefiner<'a> = &'a dyn Fn(&LumaImage, usize) -> (LumaImage, String);

/// Stress cells run on a ≤512px base — bounded cost, consistent geometry.
const STRESS_BASE_SIDE: u32 = 512;

#[cfg(test)]
thread_local! {
    /// Stress bases built on this thread — the witness that a judgment the
    /// deadline refuses at its first step costs nothing.
    static STRESS_BASES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Test-only: while set, a judgment panics in its own computation once
    /// its first step is admitted — a contained fault with no real panic
    /// source.
    pub(crate) static JUDGMENT_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Composite weights per axis (sum = 100). The contract constants of v3.
const WEIGHTS: [(StressAxis, u32); 6] = [
    (StressAxis::Resolution, 22),
    (StressAxis::Blur, 18),
    (StressAxis::Contrast, 15),
    (StressAxis::Perspective, 20),
    (StressAxis::Rotation, 10),
    (StressAxis::Lighting, 15),
];

/// Structural caps: a dead finder caps the composite at 40, a violated
/// quiet zone at 60 (the two documented AI-art killers).
const FINDER_INTEGRITY_FLOOR: f32 = 0.5;
const FINDER_DAMAGE_CAP: u8 = 40;
const QUIET_ZONE_CAP: u8 = 60;
/// UEC margin cap (the occlusion-cliff fix · door-admin probe 2026-08-05):
/// a consumed RS budget is invisible to the stress axes — the composite sat
/// flat while a growing center logo ate the margin, then decode died with
/// zero warning (at EC=H the raise-EC hint can never fire). Below the
/// half-budget line the margin caps the value CONTINUOUSLY: 0.5 → 100
/// (no-op seam) · 0.25 (ISO D) → 70 · 0.0 (ISO F, at the RS limit) → 40,
/// the finder-damage floor; at ISO's limit v1, v2-L, v3-L cap at 47-91 (spec/04).
const UEC_HEALTHY_MARGIN: f32 = 0.5;
const UEC_ZERO_MARGIN_CAP: u8 = 40;

/// The composite cap a measured RS margin allows — 100 (no-op) at or above
/// the healthy half-budget, then a continuous slope down to
/// [`UEC_ZERO_MARGIN_CAP`] at margin zero.
fn uec_margin_cap(margin: f32) -> u8 {
    if margin >= UEC_HEALTHY_MARGIN {
        return 100;
    }
    let span = f32::from(100 - UEC_ZERO_MARGIN_CAP);
    let cap = f32::from(UEC_ZERO_MARGIN_CAP) + (margin.max(0.0) / UEC_HEALTHY_MARGIN) * span;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "cap in 40..=100"
    )]
    let cap = cap.round() as u8;
    cap
}

/// Whether a cell's reads carry the scored content: one read that would join
/// the primary detection — identical bytes, an agreeing FNC1 position, a
/// compatible ECI account ([`ContentKey::admits`], 07-pipeline property 7).
/// Neither text nor corners are compared; [`cell_decode`] filters the
/// symbology.
fn detections_match(found: &[engine::RawDetection], expected: &ContentKey) -> bool {
    found.iter().any(|d| expected.admits(d))
}

/// Stress cells decode ONLY the scored symbology — survival of the same
/// content in another symbology would be a false signal, and the filtered
/// pass keeps cell cost at one decoder instead of all of them.
fn cell_decode(img: &LumaImage, symbology: crate::report::Symbology) -> engine::EngineOutcome {
    engine::decode_filtered(img, engine::FormatFilter::Only(symbology))
}

/// The limits one judgment runs under — the shared scan deadline, the
/// cancel token and, in tests, a deterministic work quota — plus the work
/// units it spent and the decoder panics its decodes caught. One unit = one
/// probe step: a calibration decode class, one stress cell, one knee
/// bisection or one envelope probe. Each step is admitted BEFORE it runs (an
/// in-flight decode is not interruptible), so a quota cuts exactly where a
/// deadline can, but on the same step every run.
pub(crate) struct Bound<'a> {
    cancel: &'a CancelToken,
    deadline: Option<Instant>,
    quota: Option<u32>,
    spent: u32,
    /// A planned step was refused.
    cut: bool,
    /// Decoder panics the engine boundary caught in this bound's decodes —
    /// the report counts them in `trace.engine_panics`.
    panics: u8,
}

impl<'a> Bound<'a> {
    /// The scan's own limits: its cancel token and shared deadline.
    pub(crate) fn new(cancel: &'a CancelToken, deadline: Option<Instant>) -> Self {
        Self {
            cancel,
            deadline,
            quota: None,
            spent: 0,
            cut: false,
            panics: 0,
        }
    }

    /// The same limits capped at `units` probe steps.
    #[cfg(test)]
    fn with_quota(self, units: u32) -> Self {
        Self {
            quota: Some(units),
            ..self
        }
    }

    /// Probe steps admitted so far.
    #[cfg(test)]
    fn spent(&self) -> u32 {
        self.spent
    }

    /// Admit one probe step — `Ok(false)` once the deadline passed or the
    /// quota is spent: the step must not run, and the bound remembers the
    /// cut. A cancelled token is `Err(Cancelled)` instead — cancellation is
    /// the caller's `QRS-005`, never an absent judgment.
    pub(crate) fn admit(&mut self) -> Result<bool> {
        if self.cancel.is_cancelled() {
            return Err(crate::error::ScanError::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d)
            || self.quota.is_some_and(|units| self.spent >= units)
        {
            self.cut = true;
            return Ok(false);
        }
        self.spent += 1;
        Ok(true)
    }

    /// Whether a planned step was refused — the judgment is then absent, or
    /// a knee bisection of a complete one is missing.
    pub(crate) fn cut(&self) -> bool {
        self.cut
    }

    /// The decoder panics this bound's decodes caught.
    pub(crate) fn panics(&self) -> u8 {
        self.panics
    }

    /// One decode of a probe step ([`cell_decode`]): its reads, the decoder
    /// panics it caught added to the tally. How the step is judged does not
    /// change — a panicking decoder read nothing.
    fn decode(
        &mut self,
        img: &LumaImage,
        symbology: crate::report::Symbology,
    ) -> Vec<engine::RawDetection> {
        let outcome = cell_decode(img, symbology);
        self.panics = self.panics.saturating_add(outcome.panics);
        outcome.detections
    }
}

/// Whether a judgment at `depth` that skips `skip` plans any stress cell:
/// `false` for depth `Off` or with every axis skipped. The one rule behind
/// [`crate::ScanConfig::computes_score`] and [`Judgment::NoAxes`].
pub(crate) fn plans_cells(depth: ScoreDepth, skip: &[StressAxis]) -> bool {
    !depth_indices(depth).is_empty() && !WEIGHTS.iter().all(|(axis, _)| skip.contains(axis))
}

/// What one bounded judgment produced.
#[derive(Debug)]
pub(crate) enum Judgment {
    /// Calibration and every planned cell ran.
    Complete(Score, Vec<Hint>),
    /// No cell is planned (every axis skipped, or depth `Off`) — an
    /// axis-less composite would be fiction.
    NoAxes,
    /// The deadline or the work quota cut a planned step: no verdict, never
    /// a partial composite whose unrun cells read as failures.
    Interrupted,
}

/// How a bounded calibration walk ended.
pub(crate) enum Calibrated {
    /// The cheapest decode class that reads the base.
    Class(CellProbe),
    /// No class reads the base at stress scale (the score then
    /// legitimately reads zero margin).
    Undecodable,
    /// The deadline or the quota cut the walk first.
    Interrupted,
}

/// The decode class a symbol's UNSTRESSED baseline needs — stress cells
/// probe the same class. An artistic symbol that only decodes through a
/// boost rung would otherwise read margin-zero on every cell (direct+otsu
/// fail even unstressed), conflating "fragile" with "undecodable".
#[derive(Clone, Copy)]
pub(crate) struct CellProbe {
    /// The deep rung the baseline needed, if any.
    pub(crate) rung: Option<crate::ladder::Rung>,
    /// The symbology being scored — every cell decode filters on it.
    pub(crate) symbology: crate::report::Symbology,
}

impl CellProbe {
    /// Calibrate on the unstressed base: direct → otsu → first decoding
    /// deep rung that reads `expected`, the primary's content. `None` = the
    /// base does not decode at stress scale at all (score legitimately reads
    /// zero margin).
    #[cfg(test)]
    pub(crate) fn calibrate(
        base: &LumaImage,
        expected: &ContentKey,
        symbology: crate::report::Symbology,
    ) -> Option<Self> {
        // a fresh token, no deadline, no quota: this walk is never cut
        let never = CancelToken::new();
        let mut unbounded = Bound::new(&never, None);
        match Self::calibrate_within(base, expected, symbology, &mut unbounded) {
            Ok(Calibrated::Class(probe)) => Some(probe),
            Ok(Calibrated::Undecodable | Calibrated::Interrupted) | Err(_) => None,
        }
    }

    /// The calibration walk under a judgment's bound. The shallow class
    /// (direct + otsu — what every cell tries first) is one step, each deep
    /// rung one more: up to 16 steps, never one past the deadline.
    #[cfg(test)]
    fn calibrate_within(
        base: &LumaImage,
        expected: &ContentKey,
        symbology: crate::report::Symbology,
        bound: &mut Bound<'_>,
    ) -> Result<Calibrated> {
        if !bound.admit()? {
            return Ok(Calibrated::Interrupted);
        }
        Self::calibrate_admitted(base, expected, symbology, bound)
    }

    /// The calibration walk once `bound` admitted its first step, the
    /// shallow class: its caller admits that step before building `base`,
    /// so a walk the deadline already prevents costs no pixel work.
    pub(crate) fn calibrate_admitted(
        base: &LumaImage,
        expected: &ContentKey,
        symbology: crate::report::Symbology,
        bound: &mut Bound<'_>,
    ) -> Result<Calibrated> {
        let shallow = Self {
            rung: None,
            symbology,
        };
        if cell_passes(base, expected, shallow, bound) {
            return Ok(Calibrated::Class(shallow));
        }
        for rung in crate::ladder::DEEP_RUNGS {
            if !bound.admit()? {
                return Ok(Calibrated::Interrupted);
            }
            let boosted = rung.apply(base);
            if detections_match(&bound.decode(&boosted, symbology), expected) {
                return Ok(Calibrated::Class(Self {
                    rung: Some(rung),
                    symbology,
                }));
            }
        }
        Ok(Calibrated::Undecodable)
    }
}

/// One stress cell: did the (transformed) image still decode to the same
/// SYMBOL? Identity is the content, never its text (07-pipeline property
/// 7): a cell's read counts when it would join the primary detection — the
/// same bytes, an agreeing FNC1 position, a compatible ECI account. A lone
/// cell read cannot see what decided the primary's text — the segments or
/// declarations a parse of the symbol observed — so a text match failed
/// symbols that survived: unmarked Kanji or Hanzi, declared bytes.
///
/// Probe = direct + otsu + the baseline's deep rung when it needed one —
/// the margin is measured RELATIVE to the symbol's own decode class, never
/// with the full ladder's recovery power (that asymmetry is the point).
/// The decodes run through `bound`, which tallies the decoder panics they
/// caught.
pub(crate) fn cell_passes(
    img: &LumaImage,
    expected: &ContentKey,
    probe: CellProbe,
    bound: &mut Bound<'_>,
) -> bool {
    if detections_match(&bound.decode(img, probe.symbology), expected) {
        return true;
    }
    let binarized = transform::otsu_threshold(img);
    if detections_match(&bound.decode(&binarized, probe.symbology), expected) {
        return true;
    }
    match probe.rung {
        Some(rung) => {
            let boosted = rung.apply(img);
            detections_match(&bound.decode(&boosted, probe.symbology), expected)
        }
        None => false,
    }
}

/// Ramp cell indices at a given depth (Reduced takes the 2nd and 4th
/// intensities of each five-step ramp).
fn depth_indices(depth: ScoreDepth) -> &'static [usize] {
    match depth {
        ScoreDepth::Off => &[],
        ScoreDepth::Reduced => &[1, 3],
        ScoreDepth::Full => &[0, 1, 2, 3, 4],
    }
}

/// Lighting cell picks at a given depth — the set's own subset (Reduced
/// keeps hard shadow + glare).
fn lighting_indices(depth: ScoreDepth) -> &'static [usize] {
    match depth {
        ScoreDepth::Off => &[],
        ScoreDepth::Reduced => &[1, 2],
        ScoreDepth::Full => &[0, 1, 2, 3, 4],
    }
}

/// Run one ordered ramp with early-stop at the first failure (the knee):
/// `(passed, knee)`. Stopping at a knee COMPLETES the ramp — intensities
/// are ordered, later cells only get harder. `Ok(None)` = the deadline or
/// the quota cut a planned cell before the ramp could say where it breaks.
fn run_ramp(
    base: &LumaImage,
    expected: &ContentKey,
    bound: &mut Bound<'_>,
    probe: CellProbe,
    indices: &[usize],
    build: impl Fn(&LumaImage, usize) -> LumaImage,
) -> Result<Option<(u8, Option<usize>)>> {
    let mut passed = 0u8;
    for &i in indices {
        if !bound.admit()? {
            return Ok(None);
        }
        let cell = build(base, i);
        if !cell_passes(&cell, expected, probe, bound) {
            return Ok(Some((passed, Some(i))));
        }
        passed += 1;
    }
    Ok(Some((passed, None)))
}

/// Wire label of one stress cell — what `AxisScore.failed_at` carries. Human-
/// readable, stable (the panel prints it verbatim): the intensity that killed
/// the ramp, or the lighting defect that failed. Indexes the FULL ramp.
fn cell_label(axis: StressAxis, i: usize) -> &'static str {
    match axis {
        StressAxis::Resolution => ["358px", "256px", "179px", "128px", "90px"][i.min(4)],
        StressAxis::Blur => ["blur 0.5", "blur 1.0", "blur 1.5", "blur 2.0", "blur 2.5"][i.min(4)],
        StressAxis::Contrast => [
            "contrast 70%",
            "contrast 55%",
            "contrast 40%",
            "contrast 30%",
            "contrast 20%",
        ][i.min(4)],
        StressAxis::Perspective => ["10°", "18°", "26°", "34°", "42°"][i.min(4)],
        StressAxis::Rotation => ["10°", "20°", "30°", "40°", "50°"][i.min(4)],
        StressAxis::Lighting => [
            "soft shadow",
            "hard shadow",
            "glare",
            "overexposure",
            "underexposure",
        ][i.min(4)],
    }
}

/// One bisection probe tightens a knee (Full depth only): the report gains
/// the tightest TESTED failing intensity, the composite reads nothing from
/// it. Deadline or quota cut / no knee / Reduced depth → honestly absent.
fn refine_knee(
    base: &LumaImage,
    expected: &ContentKey,
    probe: CellProbe,
    axis: StressAxis,
    knee: usize,
    bound: &mut Bound<'_>,
    refine: RampRefiner<'_>,
) -> Result<Option<String>> {
    if !bound.admit()? {
        return Ok(None);
    }
    let (cell, label) = refine(base, knee);
    Ok(Some(if cell_passes(&cell, expected, probe, bound) {
        cell_label(axis, knee).to_owned()
    } else {
        label
    }))
}

/// The lighting defect SET (unordered — no knee, no refinement): shadows,
/// centred glare, exposure extremes. Split from `run_axes` for line-budget
/// and because its pass-set semantics differ from the ordered ramps: the
/// set is complete only once EVERY pick ran — `Ok(None)` when the deadline
/// or the quota cut one.
fn run_lighting(
    base: &LumaImage,
    expected: &ContentKey,
    depth: ScoreDepth,
    bound: &mut Bound<'_>,
    probe: CellProbe,
) -> Result<Option<AxisScore>> {
    #[allow(
        clippy::cast_precision_loss,
        reason = "image dimensions bounded by Limits::max_dimension (lint presence varies by build shape)"
    )]
    // Glare lands on the DATA region (the symbol centre), never a finder:
    // at the old (0.3w, 0.3h) placement the saturated spot erased the
    // top-left finder — a structural kill NO design survives (a pristine
    // black-on-white symbol capped at 4/5 lighting forever, the 95-ceiling
    // class). Centred, the error-correction budget decides: weak ECC dies,
    // healthy ECC recovers — measurable, actionable (raise ECC), honest.
    let (cx, cy, radius) = (
        base.width() as f32 * 0.5,
        base.height() as f32 * 0.5,
        base.width().min(base.height()) as f32 * 0.18,
    );
    let lighting_cells: [&dyn Fn(&LumaImage) -> LumaImage; 5] = [
        &|b| warp::shadow_gradient(b, 0.4),
        &|b| warp::shadow_gradient(b, 0.7),
        &|b| warp::glare_blob(b, cx, cy, radius),
        &|b| warp::exposure(b, 60),
        &|b| warp::exposure(b, -60),
    ];
    let lighting_picks = lighting_indices(depth);
    let mut lighting_passed = 0u8;
    let mut first_failed = None;
    for &i in lighting_picks {
        if !bound.admit()? {
            return Ok(None);
        }
        if cell_passes(&lighting_cells[i](base), expected, probe, bound) {
            lighting_passed += 1;
        } else if first_failed.is_none() {
            first_failed = Some(i);
        }
    }
    Ok(Some(AxisScore {
        axis: StressAxis::Lighting,
        passed: lighting_passed,
        total: u8::try_from(lighting_picks.len()).unwrap_or(u8::MAX),
        failed_at: first_failed.map(|i| cell_label(StressAxis::Lighting, i).to_owned()),
        // an unordered defect SET has no knee to bisect
        refined_failed_at: None,
    }))
}

/// Run the five ordered ramps + the lighting set at the given depth, then
/// bisect their knees. Axes in `skip` never run — their cells are never
/// built (the integration perf win) and they are absent from the returned
/// list (the report self-describes what was measured). `Ok(None)` = the
/// deadline or the quota cut a planned cell: no axis list, never a partial
/// one.
fn run_axes(
    base: &LumaImage,
    expected: &ContentKey,
    depth: ScoreDepth,
    skip: &[StressAxis],
    bound: &mut Bound<'_>,
    probe: CellProbe,
) -> Result<Option<Vec<AxisScore>>> {
    // ---- ordered ramps (intensity grows with the index) ----
    let resolution_sides: [u32; 5] = [358, 256, 179, 128, 90];
    let blur_sigmas: [f32; 5] = [0.5, 1.0, 1.5, 2.0, 2.5];
    let contrast_factors: [f32; 5] = [0.7, 0.55, 0.4, 0.3, 0.2];
    let tilt_degrees: [f32; 5] = [10.0, 18.0, 26.0, 34.0, 42.0];
    let rotation_degrees: [f32; 5] = [10.0, 20.0, 30.0, 40.0, 50.0];

    let indices = depth_indices(depth);
    let mut axes = Vec::with_capacity(6);
    // midpoint of the knee cell and its lower neighbour (index 0 bisects
    // against the UNSTRESSED value: base side 512 · sigma 0 · factor 1 · 0°)
    let mid = |arr: &[f32; 5], i: usize, unstressed: f32| -> f32 {
        f32::midpoint(if i == 0 { unstressed } else { arr[i - 1] }, arr[i])
    };
    let ramps: [(StressAxis, RampBuilder<'_>, RampRefiner<'_>); 5] = [
        (
            StressAxis::Resolution,
            &|b, i| transform::downscale_to(b, resolution_sides[i]),
            &|b, i| {
                let side = u32::midpoint(
                    if i == 0 {
                        STRESS_BASE_SIDE
                    } else {
                        resolution_sides[i - 1]
                    },
                    resolution_sides[i],
                );
                (transform::downscale_to(b, side), format!("{side}px"))
            },
        ),
        (
            StressAxis::Blur,
            &|b, i| transform::gaussian_blur(b, blur_sigmas[i]),
            &|b, i| {
                let sigma = mid(&blur_sigmas, i, 0.0);
                (transform::gaussian_blur(b, sigma), format!("blur {sigma}"))
            },
        ),
        (
            StressAxis::Contrast,
            &|b, i| transform::contrast_boost(b, contrast_factors[i], 1.0),
            &|b, i| {
                let factor = mid(&contrast_factors, i, 1.0);
                (
                    transform::contrast_boost(b, factor, 1.0),
                    format!("contrast {:.0}%", factor * 100.0),
                )
            },
        ),
        (
            StressAxis::Perspective,
            &|b, i| warp::perspective_tilt(b, tilt_degrees[i]),
            &|b, i| {
                let deg = mid(&tilt_degrees, i, 0.0);
                (warp::perspective_tilt(b, deg), format!("{deg}\u{b0}"))
            },
        ),
        (
            StressAxis::Rotation,
            &|b, i| warp::rotate(b, rotation_degrees[i]),
            &|b, i| {
                let deg = mid(&rotation_degrees, i, 0.0);
                (warp::rotate(b, deg), format!("{deg}\u{b0}"))
            },
        ),
    ];
    let mut knees = Vec::new();
    for (axis, build, refine) in ramps {
        if skip.contains(&axis) {
            continue; // never built, never run — absent from the report
        }
        let Some((passed, knee)) = run_ramp(base, expected, bound, probe, indices, build)? else {
            return Ok(None);
        };
        if let Some(i) = knee {
            knees.push((axes.len(), i, refine));
        }
        axes.push(AxisScore {
            axis,
            passed,
            total: u8::try_from(indices.len()).unwrap_or(u8::MAX),
            failed_at: knee.map(|i| cell_label(axis, i).to_owned()),
            refined_failed_at: None,
        });
    }

    if !skip.contains(&StressAxis::Lighting) {
        let Some(lighting) = run_lighting(base, expected, depth, bound, probe)? else {
            return Ok(None);
        };
        axes.push(lighting);
    }

    // Bisection runs LAST: informational, it may only spend what the
    // composite's cells left — a cut drops a label, never the judgment.
    if matches!(depth, ScoreDepth::Full) {
        for (slot, knee, refine) in knees {
            if let Some(entry) = axes.get_mut(slot) {
                entry.refined_failed_at =
                    refine_knee(base, expected, probe, entry.axis, knee, bound, refine)?;
            }
        }
    }
    Ok(Some(axes))
}

/// Weighted composite + structural caps + the margin cap + the hint list.
///
/// `uec` is the MEASURED margin: it caps the value whether or not its
/// section ships, so skipping the check can never move `value`.
/// `publish_uec` gates only what the wire shows — `score.uec` and the two
/// hints it drives.
fn compose(
    axes: Vec<AxisScore>,
    structural: Option<crate::report::StructuralReport>,
    uec: Option<crate::report::UecReport>,
    publish_uec: bool,
    iso15415: Option<crate::report::Iso15415Report>,
    detection: &MergedDetection,
) -> (Score, Vec<Hint>) {
    // Renormalize over the axes that RAN: with the full six this divides by
    // 100 (Σ contract weights — byte-identical to the fixed divisor it
    // replaces); with skipped axes the remaining weights re-span 0-100.
    // run_axes never returns an empty list (judge() short-circuits the
    // all-skipped config before composing), so the divisor is never 0.
    let mut weighted = 0u32;
    let mut weight_run = 0u32;
    for score in &axes {
        let weight = WEIGHTS
            .iter()
            .find(|(axis, _)| *axis == score.axis)
            .map_or(0, |(_, w)| *w);
        weight_run += weight;
        if score.total > 0 {
            weighted += weight * u32::from(score.passed) * 100 / u32::from(score.total);
        }
    }
    let mut value = u8::try_from((weighted / weight_run.max(1)).min(100)).unwrap_or(100);

    let mut hints = Vec::new();
    if let Some(s) = &structural {
        let weakest = s
            .finder_integrity
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, v)| (i, *v));
        if let Some((corner, integrity)) = weakest
            && integrity < FINDER_INTEGRITY_FLOOR
        {
            value = value.min(FINDER_DAMAGE_CAP);
            #[expect(clippy::cast_possible_truncation, reason = "corner in 0..3")]
            hints.push(Hint::FixFinderPattern {
                corner: corner as u8,
            });
        }
        if !s.quiet_zone_ok {
            value = value.min(QUIET_ZONE_CAP);
            hints.push(Hint::RestoreQuietZone);
        }
    }
    // The raise-EC `value < 70` gate reads the composite BEFORE the margin
    // cap: a cap-driven drop is the margin's own verdict, and margin-driven
    // hints speak only through the published section (published, the
    // thin-margin path covers every cap below 70 — such a margin is grade F).
    let pre_margin_cap = value;
    // A melting RS margin caps the composite on a continuous slope (see the
    // UEC_HEALTHY_MARGIN doc) — the report's `uec` field carries the why.
    if let Some(u) = &uec {
        value = value.min(uec_margin_cap(u.margin));
    }
    let uec = uec.filter(|_| publish_uec);

    // ---- axis-derived hints (stable order) ----
    let fraction = |axis: StressAxis| -> Option<(u8, u8)> {
        axes.iter()
            .find(|a| a.axis == axis)
            .map(|a| (a.passed, a.total))
    };
    if let Some((p, t)) = fraction(StressAxis::Contrast)
        && t > 0
        && u32::from(p) * 100 / u32::from(t) <= 40
    {
        hints.push(Hint::IncreaseContrast);
    }
    if let Some((p, t)) = fraction(StressAxis::Resolution)
        && t > 0
        && u32::from(p) * 100 / u32::from(t) <= 40
    {
        hints.push(Hint::EnlargeModules);
    }
    if let Some((p, t)) = fraction(StressAxis::Blur)
        && t > 0
        && p == 0
    {
        // dies at the mildest blur — art texture is eating the margin
        hints.push(Hint::ReduceArtTexture);
    }
    // UEC drives the EC hint with priority: a thin real margin is the
    // strongest possible "raise EC" signal even when stress survival is OK.
    let thin_margin = uec.is_some_and(|u| {
        matches!(
            u.grade,
            crate::report::UecGrade::D | crate::report::UecGrade::F
        )
    });
    if let Some(current) = detection.ec
        && current < EcLevel::H
        && (pre_margin_cap < 70 || thin_margin)
    {
        hints.push(Hint::RaiseErrorCorrection { current });
    }
    // Margin ZERO is qualitatively different from thin: the worst block
    // consumed its entire correction budget, which is exactly the signature
    // of a Reed-Solomon miscorrection (caught live on the zxing blackbox
    // corpus: rqrr returned "photography" for a "photograph" ground truth
    // at 12/24 errors — this hint is the machine-readable distrust signal).
    // It reads only the PUBLISHED section by design: its errors/capacity ARE
    // the section's numbers, so a host that skips `uec` gets neither — the
    // margin still capped the value (at most 40 at margin zero).
    if let Some(u) = &uec
        && u.margin <= 0.0
    {
        hints.push(Hint::LowCorrectionMargin {
            errors: u.worst_block_errors,
            capacity: u.worst_block_capacity,
        });
    }

    let score = Score {
        value,
        grade: Grade::from_value(value),
        // the honesty integer: how much contract stands behind `value`
        weights_run: u8::try_from(weight_run.min(100)).unwrap_or(100),
        axes,
        structural,
        uec,
        iso15415,
    };
    (score, hints)
}

/// Evaluate the score for the primary detection under `bound` (the scan's
/// shared deadline), its cells matching `key`, the detection's content. No
/// judgment when no cell is planned (every axis skipped — an axis-less
/// composite would be fiction; callers treat it exactly like
/// `ScoreDepth::Off`) and when the deadline cuts it: absent, never partial.
/// `bound` then says whether the deadline prevented a planned step — the
/// judgment's (then absent) or a knee bisection's (the score stands, its
/// label is missing) — and how many decoder panics the cells caught. A
/// cancelled token is `Err(Cancelled)` (`QRS-005`).
pub(crate) fn evaluate(
    luma: &LumaImage,
    detection: &MergedDetection,
    key: &ContentKey,
    depth: ScoreDepth,
    skip: &[StressAxis],
    skip_checks: &[crate::ladder::ScoreCheck],
    bound: &mut Bound<'_>,
) -> Result<Option<(Score, Vec<Hint>)>> {
    Ok(
        match judge(luma, detection, key, depth, skip, skip_checks, bound)? {
            Judgment::Complete(score, hints) => Some((score, hints)),
            Judgment::NoAxes | Judgment::Interrupted => None,
        },
    )
}

/// Judge the primary detection under `bound`: calibration, every planned
/// cell, then the knee bisections — every cell matching `key`, the
/// detection's content. Structural, UEC and ISO run only once every planned
/// cell has run — a judgment the deadline or the quota cuts is
/// `Interrupted`, while a cancelled token is `Err(Cancelled)` (`QRS-005`).
pub(crate) fn judge(
    luma: &LumaImage,
    detection: &MergedDetection,
    key: &ContentKey,
    depth: ScoreDepth,
    skip: &[StressAxis],
    skip_checks: &[crate::ladder::ScoreCheck],
    bound: &mut Bound<'_>,
) -> Result<Judgment> {
    if !plans_cells(depth, skip) {
        return Ok(Judgment::NoAxes);
    }
    // the calibration's first step is admitted before the stress base is
    // built: a judgment the deadline already prevents costs nothing
    if !bound.admit()? {
        return Ok(Judgment::Interrupted);
    }
    #[cfg(test)]
    STRESS_BASES.with(|built| built.set(built.get() + 1));
    #[cfg(test)]
    assert!(
        !JUDGMENT_FAULT.with(std::cell::Cell::get),
        "injected judgment fault"
    );
    let base = transform::downscale_to(luma, STRESS_BASE_SIDE);
    // calibrate the cell probe on the unstressed base (its decode class)
    let probe = match CellProbe::calibrate_admitted(&base, key, detection.symbology, bound)? {
        Calibrated::Class(probe) => probe,
        // no class reads the unstressed base: the shallow probe measures
        // it anyway — a legitimately (near-)zero margin, fully judged
        Calibrated::Undecodable => CellProbe {
            rung: None,
            symbology: detection.symbology,
        },
        Calibrated::Interrupted => return Ok(Judgment::Interrupted),
    };
    let Some(axes) = run_axes(&base, key, depth, skip, bound, probe)? else {
        return Ok(Judgment::Interrupted);
    };
    // Photometric checks sample the ORIGINAL luma — when the geometry came
    // from an INVERTING attempt (light-on-dark symbol), hand them an
    // inverted view or every module's polarity reads flipped (a clean
    // symbol would score finder integrity ≈ 0.33 + earn bogus caps/hints).
    let photometric_view = detection
        .photometric_inverted
        .then(|| transform::invert(luma));
    let sample_luma = photometric_view.as_ref().unwrap_or(luma);
    let structural = match (detection.corners, detection.version) {
        (Some(corners), Some(version)) => structural::check(sample_luma, corners, version),
        _ => None,
    };
    // The UEC replay runs whenever the bitstream exists: its margin caps the
    // composite, and skipping a check never moves a published verdict. A
    // skipped `uec` withholds only what it shows — `score.uec`, the two hints
    // it drives (compose) and the ISO block's unused_error_correction
    // parameter, whose grade still counts in the ISO `overall` (the minimum
    // over every MEASURED parameter). A skipped ISO block never runs. The
    // stream is the verified orientation's, and it measures a margin only
    // when it error-corrects errors-only under the detection's format — a
    // stream the S5 rescue recovered with erasures keeps its own count.
    let publish_uec = !skip_checks.contains(&crate::ladder::ScoreCheck::Uec);
    let skip_iso = skip_checks.contains(&crate::ladder::ScoreCheck::Iso15415);
    let source = if detection
        .engines
        .contains(&crate::report::EngineKind::Rescue)
    {
        uec::Stream::Rescued
    } else {
        uec::Stream::Decoded
    };
    let uec_report = match (
        &detection.masked_stream,
        detection.version,
        detection.ec,
        detection.mask,
    ) {
        (Some(stream), Some(version), Some(ec), Some(mask)) => {
            uec::compute(&stream.bits, stream.bit_len, version, ec, mask, source)
        }
        _ => None,
    };
    let iso = if skip_iso {
        None
    } else {
        match (detection.corners, detection.version, &structural) {
            (Some(corners), Some(version), Some(s)) => {
                iso15415::compute(sample_luma, corners, version, s, uec_report.as_ref()).map(
                    |mut card| {
                        if !publish_uec {
                            card.unused_error_correction = None;
                        }
                        card
                    },
                )
            }
            _ => None,
        }
    };
    let (score, hints) = compose(axes, structural, uec_report, publish_uec, iso, detection);
    Ok(Judgment::Complete(score, hints))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::ladder::{self, ScanConfig};
    use crate::transform::normalize;

    fn detection_with(ec: Option<EcLevel>) -> MergedDetection {
        MergedDetection {
            symbology: crate::report::Symbology::QrCode,
            raw: b"x".to_vec(),
            text: "x".into(),
            charset: crate::report::Charset::Utf8,
            masked_stream: None,
            corners: None,
            version: Some(2),
            ec,
            mask: Some(0),
            fnc1: false,
            photometric_inverted: false,
            engines: vec![crate::report::EngineKind::Rqrr],
        }
    }

    /// `compose` under the default checks — the UEC section published.
    fn compose_published(
        axes: Vec<AxisScore>,
        structural: Option<crate::report::StructuralReport>,
        uec: Option<crate::report::UecReport>,
        iso15415: Option<crate::report::Iso15415Report>,
        detection: &MergedDetection,
    ) -> (Score, Vec<Hint>) {
        compose(axes, structural, uec, true, iso15415, detection)
    }

    fn axes_with(overrides: &[(StressAxis, u8, u8)]) -> Vec<AxisScore> {
        let mut axes: Vec<AxisScore> = WEIGHTS
            .iter()
            .map(|&(axis, _)| AxisScore {
                axis,
                passed: 5,
                total: 5,
                failed_at: None,
                refined_failed_at: None,
            })
            .collect();
        for &(axis, passed, total) in overrides {
            let slot = axes.iter_mut().find(|a| a.axis == axis).unwrap();
            slot.passed = passed;
            slot.total = total;
        }
        axes
    }

    /// The composite is a PUBLISHED contract — pin its arithmetic exactly
    /// (mutation testing showed the weighted math was never value-pinned).
    #[test]
    fn composite_weights_are_exact() {
        let d = detection_with(Some(EcLevel::H));
        let (score, _) = compose_published(axes_with(&[]), None, None, None, &d);
        assert_eq!(score.value, 100);

        // resolution 4/5: (22·80 + 78·100)/100 = 95
        let (score, _) = compose_published(
            axes_with(&[(StressAxis::Resolution, 4, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 95);

        // blur 0/5: (18·0 + 82·100)/100 = 82
        let (score, hints) =
            compose_published(axes_with(&[(StressAxis::Blur, 0, 5)]), None, None, None, &d);
        assert_eq!(score.value, 82);
        assert!(hints.contains(&Hint::ReduceArtTexture), "{hints:?}");

        // rotation 2/5: (10·40 + 90·100)/100 = 94
        let (score, _) = compose_published(
            axes_with(&[(StressAxis::Rotation, 2, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 94);
    }

    #[test]
    fn structural_caps_are_exact_with_boundaries() {
        use crate::report::StructuralReport;
        let d = detection_with(Some(EcLevel::H));

        // worst finder 0.49 < 0.5 floor → capped at 40 + the corner named
        let (score, hints) = compose_published(
            axes_with(&[]),
            Some(StructuralReport {
                finder_integrity: [1.0, 1.0, 0.49],
                quiet_zone_ok: true,
            }),
            None,
            None,
            &d,
        );
        assert_eq!(score.value, FINDER_DAMAGE_CAP);
        assert!(
            hints.contains(&Hint::FixFinderPattern { corner: 2 }),
            "{hints:?}"
        );

        // exactly at the floor → NO cap
        let (score, hints) = compose_published(
            axes_with(&[]),
            Some(StructuralReport {
                finder_integrity: [1.0, 1.0, 0.5],
                quiet_zone_ok: true,
            }),
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 100);
        assert!(hints.is_empty(), "{hints:?}");

        // quiet zone violated → capped at 60
        let (score, hints) = compose_published(
            axes_with(&[]),
            Some(StructuralReport {
                finder_integrity: [1.0, 1.0, 1.0],
                quiet_zone_ok: false,
            }),
            None,
            None,
            &d,
        );
        assert_eq!(score.value, QUIET_ZONE_CAP);
        assert!(hints.contains(&Hint::RestoreQuietZone));
    }

    #[test]
    fn hint_thresholds_fire_exactly_at_the_published_boundaries() {
        let d = detection_with(Some(EcLevel::H));

        // contrast survival 40% fires, 60% does not
        let (_, hints) = compose_published(
            axes_with(&[(StressAxis::Contrast, 2, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(hints.contains(&Hint::IncreaseContrast), "{hints:?}");
        let (_, hints) = compose_published(
            axes_with(&[(StressAxis::Contrast, 3, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(!hints.contains(&Hint::IncreaseContrast), "{hints:?}");

        // resolution likewise → EnlargeModules
        let (_, hints) = compose_published(
            axes_with(&[(StressAxis::Resolution, 2, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(hints.contains(&Hint::EnlargeModules), "{hints:?}");
        let (_, hints) = compose_published(
            axes_with(&[(StressAxis::Resolution, 3, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(!hints.contains(&Hint::EnlargeModules), "{hints:?}");

        // blur partial survival (1/5) is NOT the texture hint (only 0/5 is)
        let (_, hints) =
            compose_published(axes_with(&[(StressAxis::Blur, 1, 5)]), None, None, None, &d);
        assert!(!hints.contains(&Hint::ReduceArtTexture), "{hints:?}");
    }

    #[test]
    fn raise_ec_hint_value_and_uec_paths() {
        use crate::report::{UecGrade, UecReport};
        let d = detection_with(Some(EcLevel::Q));

        // value 94 (rotation 2/5) + EC<H + healthy margin → no hint
        let (_, hints) = compose_published(
            axes_with(&[(StressAxis::Rotation, 2, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(
            !hints
                .iter()
                .any(|h| matches!(h, Hint::RaiseErrorCorrection { .. })),
            "{hints:?}"
        );

        // value < 70 → fires (blur 0/5 + perspective 0/5: 82-20=62)
        let (score, hints) = compose_published(
            axes_with(&[(StressAxis::Blur, 0, 5), (StressAxis::Perspective, 0, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(score.value < 70, "{}", score.value);
        assert!(
            hints.iter().any(|h| matches!(
                h,
                Hint::RaiseErrorCorrection {
                    current: EcLevel::Q
                }
            )),
            "{hints:?}"
        );

        // thin UEC margin (grade D) fires even when every axis passes — and
        // since the margin cap (2026-08-06) the value SAYS the melt too
        // (0.30/0.5 × 60 + 40 = 76, no longer a flat 100 over a thin budget)
        let thin = UecReport {
            margin: 0.30,
            grade: UecGrade::D,
            worst_block_errors: 6,
            worst_block_capacity: 18,
        };
        let (score, hints) = compose_published(axes_with(&[]), None, Some(thin), None, &d);
        assert_eq!(score.value, 76);
        assert!(
            hints
                .iter()
                .any(|h| matches!(h, Hint::RaiseErrorCorrection { .. })),
            "{hints:?}"
        );

        // margin exactly ZERO → miscorrection-risk hint, with the block stats
        let limit = UecReport {
            margin: 0.0,
            grade: UecGrade::F,
            worst_block_errors: 12,
            worst_block_capacity: 24,
        };
        let (_, hints) = compose_published(axes_with(&[]), None, Some(limit), None, &d);
        assert!(
            hints.contains(&Hint::LowCorrectionMargin {
                errors: 12,
                capacity: 24
            }),
            "{hints:?}"
        );
        // thin-but-nonzero margin does NOT fire the miscorrection hint
        let (_, hints) = compose_published(axes_with(&[]), None, Some(thin), None, &d);
        assert!(
            !hints
                .iter()
                .any(|h| matches!(h, Hint::LowCorrectionMargin { .. })),
            "{hints:?}"
        );

        // EC already H → never fires
        let dh = detection_with(Some(EcLevel::H));
        let (_, hints) = compose_published(
            axes_with(&[(StressAxis::Blur, 0, 5)]),
            None,
            Some(thin),
            None,
            &dh,
        );
        assert!(
            !hints
                .iter()
                .any(|h| matches!(h, Hint::RaiseErrorCorrection { .. })),
            "{hints:?}"
        );
    }

    /// The occlusion cliff (door-admin probe 2026-08-05): a center logo eats
    /// the RS budget while every stress axis keeps passing — the composite
    /// sat at 88 from `logo_scale` 12→18 then fell to no-decode at 24 with no
    /// warning (at EC=H the raise-EC hint can never fire). The margin IS the
    /// distance to that cliff, so it must SHAPE the value: flat while the
    /// budget is healthy, a continuous slope below the half-budget line.
    #[test]
    fn uec_margin_caps_the_composite_continuously() {
        use crate::report::{UecGrade, UecReport};
        let d = detection_with(Some(EcLevel::H));
        let at = |margin: f32| UecReport {
            margin,
            grade: UecGrade::from_margin(margin),
            worst_block_errors: 6,
            worst_block_capacity: 24,
        };
        let value = |uec: Option<UecReport>| {
            compose_published(axes_with(&[]), None, uec, None, &d)
                .0
                .value
        };

        // healthy budget (≥ half) — the cap is a no-op, including the seam
        assert_eq!(value(None), 100, "no measurement → no penalty");
        assert_eq!(value(Some(at(1.0))), 100);
        assert_eq!(
            value(Some(at(0.5))),
            100,
            "continuous seam at the threshold"
        );

        // below half-budget the value slopes down — the cliff gets a warning
        let v37 = value(Some(at(0.37))); // ISO C
        let v25 = value(Some(at(0.25))); // ISO D
        let v10 = value(Some(at(0.10)));
        let v0 = value(Some(at(0.0))); // ISO F — at the RS limit
        assert!(
            v37 < 100 && v25 < v37 && v10 < v25 && v0 < v10,
            "monotone slope: {v37} {v25} {v10} {v0}"
        );
        assert_eq!(v25, 70, "ISO D lands at 70 — visibly below the far band");
        assert_eq!(v0, 40, "RS limit = the finder-damage floor, never a pass");

        // the cap CAPS — it never raises a value the axes already sank
        let sunk = compose_published(
            axes_with(&[(StressAxis::Blur, 0, 5), (StressAxis::Perspective, 0, 5)]),
            None,
            Some(at(0.45)),
            None,
            &d,
        )
        .0
        .value;
        assert!(sunk < 94, "axes verdict survives under a mild cap: {sunk}");
    }

    #[test]
    fn depth_index_sets_pinned() {
        assert_eq!(depth_indices(ScoreDepth::Off), &[] as &[usize]);
        assert_eq!(depth_indices(ScoreDepth::Reduced), &[1, 3]);
        assert_eq!(depth_indices(ScoreDepth::Full), &[0, 1, 2, 3, 4]);
        assert_eq!(lighting_indices(ScoreDepth::Off), &[] as &[usize]);
        assert_eq!(lighting_indices(ScoreDepth::Reduced), &[1, 2]);
        assert_eq!(lighting_indices(ScoreDepth::Full), &[0, 1, 2, 3, 4]);
    }

    /// An axis can legitimately carry `total == 0` (`ScoreDepth::Off`, an empty
    /// ramp). Every `score.total > 0` guard protects a division BY that total,
    /// so a `>`→`>=` swap divides by zero. Under each mutant the matching
    /// compose below panics; under the real guard it yields the exact value.
    #[test]
    fn zero_total_axes_are_skipped_not_divided() {
        let d = detection_with(Some(EcLevel::H));

        // weighted-sum guard (:286) — resolution skipped ⇒ (100−22) = 78
        let (score, _) = compose_published(
            axes_with(&[(StressAxis::Resolution, 0, 0)]),
            None,
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 78);

        // contrast-hint guard (:322) — contrast skipped ⇒ (100−15) = 85, no hint
        let (score, hints) = compose_published(
            axes_with(&[(StressAxis::Contrast, 0, 0)]),
            None,
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 85);
        assert!(!hints.contains(&Hint::IncreaseContrast), "{hints:?}");

        // resolution-hint guard (:328) — a 0/0 axis is not "≤40% survival"
        let (_, hints) = compose_published(
            axes_with(&[(StressAxis::Resolution, 0, 0)]),
            None,
            None,
            None,
            &d,
        );
        assert!(!hints.contains(&Hint::EnlargeModules), "{hints:?}");

        // blur-hint guard (:334) — total==0 must NOT read as "dies at the
        // mildest blur" (that guard is `t > 0`, then `p == 0`)
        let (score, hints) =
            compose_published(axes_with(&[(StressAxis::Blur, 0, 0)]), None, None, None, &d);
        assert_eq!(score.value, 82);
        assert!(!hints.contains(&Hint::ReduceArtTexture), "{hints:?}");
    }

    /// `value < 70` (strict) is the raise-EC gate — the companion test pins the
    /// FIRES side at value 62; this pins the boundary: value EXACTLY 70 with
    /// EC<H and a healthy margin must NOT fire (:350 `<`→`<=`). Perspective 0/5
    /// + Rotation 0/5 drop 20+10 weight ⇒ weighted 7000 ⇒ value 70.
    #[test]
    fn raise_ec_hint_uses_strict_less_than_at_the_value_boundary() {
        let d = detection_with(Some(EcLevel::Q));
        let (score, hints) = compose_published(
            axes_with(&[
                (StressAxis::Perspective, 0, 5),
                (StressAxis::Rotation, 0, 5),
            ]),
            None,
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 70);
        assert!(
            !hints
                .iter()
                .any(|h| matches!(h, Hint::RaiseErrorCorrection { .. })),
            "value==70 is NOT below the strict-< 70 threshold: {hints:?}"
        );
    }

    /// The content of a plain byte-mode symbol of the `qrcode` encoder: its
    /// bytes, no ECI declaration, no FNC1 — what both engines read of it.
    fn plain_key(payload: &[u8]) -> ContentKey {
        ContentKey {
            raw: payload.to_vec(),
            eci: crate::engine::content::EciEvidence::NoEci,
            fnc1: crate::engine::content::Fnc1::None,
        }
    }

    /// The primary detection of an unbounded Full walk and its content.
    fn primary_of(planes: &crate::transform::SourcePlanes) -> (MergedDetection, ContentKey) {
        let outcome = ladder::run(planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
        (outcome.merged[0].clone(), outcome.contents[0].0.clone())
    }

    /// Decode a pristine QR and hand back the calibrated stress base + probe,
    /// so the private `run_axes` can be driven directly (its internals never
    /// had a value-level test — the lighting accumulator and deadline break
    /// only surface here).
    fn clean_base_and_probe() -> (LumaImage, ContentKey, CellProbe) {
        let code =
            qrcode::QrCode::with_error_correction_level(b"stress-axis pin", qrcode::EcLevel::Q)
                .unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(8, 8)
            .build();
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let planes = normalize(&ImageInput::encoded(&png), &Limits::default()).unwrap();
        let (d, key) = primary_of(&planes);
        let base = transform::downscale_to(&planes.luma, STRESS_BASE_SIDE);
        let probe = CellProbe::calibrate(&base, &key, d.symbology).unwrap();
        (base, key, probe)
    }

    /// Every COMPLETE judgment's axes are honest: `passed == total` exactly
    /// when nothing failed, `failed_at` names one of the axis's PLANNED
    /// cells, and a ramp's knee sits right after its survivors.
    fn assert_honest_axes(axes: &[AxisScore], depth: ScoreDepth) {
        for a in axes {
            let picks = if a.axis == StressAxis::Lighting {
                lighting_indices(depth)
            } else {
                depth_indices(depth)
            };
            assert_eq!(usize::from(a.total), picks.len(), "planned cells: {a:?}");
            assert_eq!(
                a.passed == a.total,
                a.failed_at.is_none(),
                "passed == total iff nothing failed: {a:?}"
            );
            if let Some(label) = a.failed_at.as_deref() {
                let at = picks.iter().position(|&i| cell_label(a.axis, i) == label);
                assert!(at.is_some(), "failed_at names a planned cell: {a:?}");
                if a.axis != StressAxis::Lighting {
                    assert_eq!(
                        at,
                        Some(usize::from(a.passed)),
                        "a ramp's knee follows its survivors: {a:?}"
                    );
                }
            }
        }
    }

    /// `run_axes` with no deadline and no quota — never cut.
    fn unbounded_axes(
        base: &LumaImage,
        key: &ContentKey,
        skip: &[StressAxis],
        probe: CellProbe,
    ) -> Vec<AxisScore> {
        let cancel = CancelToken::new();
        let axes = run_axes(
            base,
            key,
            ScoreDepth::Full,
            skip,
            &mut Bound::new(&cancel, None),
            probe,
        )
        .unwrap()
        .expect("an unbounded run is never cut");
        assert_honest_axes(&axes, ScoreDepth::Full);
        axes
    }

    /// `evaluate` with no deadline — a complete judgment.
    fn judged(
        luma: &LumaImage,
        detection: &MergedDetection,
        key: &ContentKey,
        depth: ScoreDepth,
        skip: &[StressAxis],
        checks: &[crate::ladder::ScoreCheck],
    ) -> (Score, Vec<Hint>) {
        let cancel = CancelToken::new();
        let mut bound = Bound::new(&cancel, None);
        let judged = evaluate(luma, detection, key, depth, skip, checks, &mut bound).unwrap();
        assert!(!bound.cut(), "an unbounded judgment is never cut");
        let (score, hints) = judged.expect("an unbounded judgment with axes completes");
        assert_honest_axes(&score.axes, depth);
        (score, hints)
    }

    /// THE 100-IS-REACHABLE INVARIANT (operator lock 2026-07-10). A pristine
    /// full-frame symbol survives ALL five lighting cells — the glare blob
    /// lands on the DATA region (symbol centre), where the error-correction
    /// budget absorbs it. The old (0.3w, 0.3h) placement saturated the
    /// top-left finder: a structural kill NO design survives, capping every
    /// perfect render at 4/5 lighting (the 95-ceiling class — the axis
    /// measured the instrument, not the symbol; same class as the W8
    /// rotation-cropping fix). This pin also catches the flood-mutant
    /// (`radius/0.18` whites the whole frame → the cell fails → 4/5 here).
    #[test]
    fn pristine_symbol_survives_all_lighting_cells() {
        let (base, key, probe) = clean_base_and_probe();
        let axes = unbounded_axes(&base, &key, &[], probe);
        let lighting = axes
            .iter()
            .find(|a| a.axis == StressAxis::Lighting)
            .unwrap();
        assert_eq!(
            lighting.passed, 5,
            "a pristine full-frame symbol must survive every lighting cell \
             (glare sits on the data region, not a finder): {lighting:?}"
        );
        assert!(
            lighting.failed_at.is_none(),
            "all-passed carries no failed_at: {lighting:?}"
        );
    }

    /// THE PRODUCT PINS — the builder's real rendered pixels, committed as
    /// fixtures. (a) The default render (black rounded modules on white,
    /// ECC M) reaches the full 100: every lost point below this is caused by
    /// the USER's design, never by the instrument. (b) A real styled
    /// template (pale-pink hearts + centre logo) loses lighting cells and
    /// the wire NAMES the first failed cell — the explainability contract
    /// the panel renders ("Light 3/5 · hard shadow"). Also the guard for
    /// glare placement/radius mutants: an off-canvas or flooding blob flips
    /// (a) or (b).
    #[test]
    fn builder_default_render_reaches_100_and_styled_explains_its_losses() {
        let score_of = |rel: &str| {
            let path = format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"));
            let bytes = std::fs::read(&path).expect("builder fixture present");
            let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
            let (detection, key) = primary_of(&planes);
            judged(&planes.luma, &detection, &key, ScoreDepth::Full, &[], &[])
        };

        // (a) the default render: a perfect score is REACHABLE.
        let (default_score, _) = score_of("fixtures/clean/builder-default-rounded-1024.png");
        assert_eq!(
            default_score.value, 100,
            "the pristine default render must reach 100 — a ceiling below that \
             measures the instrument, not the design: {:?}",
            default_score.axes
        );

        // (b) the styled template: losses exist AND name their cell.
        let (styled_score, _) = score_of("fixtures/artistic/builder-template-hearts-web-1024.png");
        assert!(
            styled_score.value < 100,
            "the pale styled template keeps real, explained losses: {}",
            styled_score.value
        );
        let lighting = styled_score
            .axes
            .iter()
            .find(|a| a.axis == StressAxis::Lighting)
            .unwrap();
        assert!(
            lighting.passed < lighting.total,
            "the styled template loses lighting cells: {lighting:?}"
        );
        assert_eq!(
            lighting.failed_at.as_deref(),
            Some("hard shadow"),
            "the first failed cell is named on the wire: {lighting:?}"
        );
    }

    /// A flawless high-version symbol with the renderer's standard 4-module
    /// quiet zone. Engines are rotation-invariant on clean symbols (finder
    /// geometry is angle-free), so every death on this ramp is a PROBE
    /// artifact. The same-canvas rotate amputated the corners — a v10's
    /// corner radius (≈0.62·w) leaves the frame at the very first 10° step —
    /// and the axis read "fragile at 10°" for a symbol any phone rotates
    /// through happily: the probe measured the frame, not the engine.
    #[test]
    fn rotation_ramp_measures_engine_tolerance_not_frame_cropping() {
        let code = qrcode::QrCode::with_version(
            b"rotation probe: the frame must never eat the finders",
            qrcode::Version::Normal(10),
            qrcode::EcLevel::Q,
        )
        .unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(4, 4)
            .build();
        let (w, h) = (img.width(), img.height());
        let base = LumaImage::new(img.into_raw(), w, h);
        let key = plain_key(b"rotation probe: the frame must never eat the finders");
        let probe = CellProbe::calibrate(&base, &key, crate::report::Symbology::QrCode)
            .expect("flawless base calibrates");
        let axes = unbounded_axes(&base, &key, &[], probe);
        let rotation = axes
            .iter()
            .find(|a| a.axis == StressAxis::Rotation)
            .unwrap();
        assert_eq!(
            rotation.passed, 5,
            "a flawless v10 died on the rotation ramp — the probe is measuring \
             frame cropping, not engine tolerance: {rotation:?}"
        );
    }

    /// Integration skip: skipped axes never run (their cells are never
    /// built), the report self-describes (axes[] omits them), the composite
    /// renormalizes over the run weights, and axis-derived hints from
    /// skipped axes structurally cannot fire. The builder case: a generated
    /// preview has no capture angle — perspective + rotation are noise there.
    #[test]
    fn skipped_axes_never_run_and_the_composite_renormalizes() {
        let (base, key, probe) = clean_base_and_probe();
        let full = unbounded_axes(&base, &key, &[], probe);
        let skipped = unbounded_axes(
            &base,
            &key,
            &[StressAxis::Perspective, StressAxis::Rotation],
            probe,
        );
        assert_eq!(skipped.len(), 4, "six axes minus the two skipped");
        assert!(
            skipped
                .iter()
                .all(|a| a.axis != StressAxis::Perspective && a.axis != StressAxis::Rotation),
            "skipped axes are absent from the report: {skipped:?}"
        );
        // The four surviving axes measure identically to the full run —
        // skipping is subtraction, never a change to what still runs.
        for a in &skipped {
            let twin = full.iter().find(|f| f.axis == a.axis).unwrap();
            assert_eq!((a.passed, a.total), (twin.passed, twin.total));
        }
        // Renormalized arithmetic on this pristine fixture (every cell
        // passes since the glare cell moved onto the data region — the
        // 100-is-reachable invariant, pinned by the sibling tests): both
        // sums are all-100s, so full AND skip read exactly 100. The divisor
        // truth (Σ run weights, not the constant 100) is pinned just below
        // by the lone half-passed lighting axis reading 50, never 7.
        let d = detection_with(Some(EcLevel::H));
        let (score_full, _) = compose_published(full, None, None, None, &d);
        let (score_skip, _) = compose_published(skipped, None, None, None, &d);
        assert_eq!(score_full.value, 100, "full six-axis composite");
        assert_eq!(score_skip.value, 100, "renormalized four-axis composite");
        // And the renormalization actually renormalizes: a half-passed
        // lighting axis alone must read 50, not 7 (its weight over 100).
        let lone = vec![AxisScore {
            axis: StressAxis::Lighting,
            passed: 1,
            total: 2,
            failed_at: None,
            refined_failed_at: None,
        }];
        let (score_lone, _) = compose_published(lone, None, None, None, &d);
        assert_eq!(score_lone.value, 50, "weights re-span the run set");
    }

    /// The glare cell ACTS — the anti-no-op / placement / radius guard. A
    /// symbol parked in the bottom-right quadrant of a 2× canvas puts the
    /// canvas centre exactly on its top-left finder: the centred blob kills
    /// it (4/5 + `failed_at` "glare"). Mutants all flip this: a no-op glare
    /// reads 5/5; an off-canvas centre (`0.5`→`/0.5`) reads 5/5; a flooding
    /// radius (`/0.18`) takes more cells down (≤3). The pristine-full-frame
    /// sibling pins the other side: centred glare on the DATA region is
    /// survivable — together they hold the cell to "lethal exactly where a
    /// finder sits, absorbable where the ECC budget rules".
    #[test]
    fn glare_cell_stays_lethal_on_a_finder_and_names_itself() {
        let code =
            qrcode::QrCode::with_error_correction_level(b"canvas glare pin", qrcode::EcLevel::H)
                .unwrap();
        let qr = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let (qw, qh) = (qr.width(), qr.height());
        let (cw, ch) = (qw * 2, qh * 2);
        let mut canvas = vec![255u8; (cw * ch) as usize];
        for yy in 0..qh {
            for xx in 0..qw {
                canvas[((yy + qh) * cw + (xx + qw)) as usize] = qr.get_pixel(xx, yy).0[0];
            }
        }
        let base = LumaImage::new(canvas, cw, ch);
        let key = plain_key(b"canvas glare pin");
        let probe = CellProbe::calibrate(&base, &key, crate::report::Symbology::QrCode)
            .expect("corner symbol calibrates");
        let axes = unbounded_axes(&base, &key, &[], probe);
        let lighting = axes
            .iter()
            .find(|a| a.axis == StressAxis::Lighting)
            .unwrap();
        assert_eq!(
            lighting.passed, 4,
            "the canvas-centre blob sits on the corner symbol's TL finder and \
             must kill exactly the glare cell: {lighting:?}"
        );
        assert_eq!(
            lighting.failed_at.as_deref(),
            Some("glare"),
            "the kill names itself on the wire: {lighting:?}"
        );
    }

    /// The lighting set counts survivors with `lighting_passed += 1`; a
    /// `+=`→`*=` swap (:261) would multiply the zero seed forever. A pristine
    /// symbol survives ≥1 of the five lighting cells, so the count must move.
    #[test]
    fn lighting_pass_count_accumulates() {
        let (base, key, probe) = clean_base_and_probe();
        let axes = unbounded_axes(&base, &key, &[], probe);
        let lighting = axes
            .iter()
            .find(|a| a.axis == StressAxis::Lighting)
            .unwrap();
        assert!(
            lighting.passed >= 1,
            "clean symbol must survive ≥1 lighting cell: {lighting:?}"
        );
    }

    /// A deadline already in the past stops the lighting set before any cell
    /// runs (`Instant::now() >= d`) — and the set is then INTERRUPTED, not a
    /// fabricated 0/5. The mutant `<` never fires on a past deadline, so it
    /// would run the set and report survivors.
    #[test]
    fn a_past_deadline_stops_the_lighting_set() {
        let (base, key, probe) = clean_base_and_probe();
        let cancel = CancelToken::new();
        let mut bound = Bound::new(&cancel, Some(Instant::now()));
        let lighting = run_lighting(&base, &key, ScoreDepth::Full, &mut bound, probe).unwrap();
        assert!(
            lighting.is_none(),
            "a past deadline interrupts the set: {lighting:?}"
        );
        assert_eq!(bound.spent(), 0, "no lighting cell ran");
    }

    /// Kanji-mode regression for the SCORING path (the ladder-side fix has
    /// its own test): stress cells match by content — both engines read the
    /// original Shift JIS bytes, so an rxing-only survival counts. Keyed on
    /// rxing's re-encoded bytes, cells silently failed every such survival
    /// on Kanji symbols.
    #[test]
    fn kanji_symbol_scores_with_content_keyed_cells() {
        let sjis: &[u8] = &[0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];
        let code = qrcode::QrCode::with_error_correction_level(sjis, qrcode::EcLevel::Q).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(8, 8)
            .build();
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let planes = normalize(&ImageInput::encoded(&png), &Limits::default()).unwrap();
        let (detection, key) = primary_of(&planes);
        assert_eq!(detection.text, "こんにちは");

        let (score, _) = judged(&planes.luma, &detection, &key, ScoreDepth::Full, &[], &[]);
        // a pristine 8px/module symbol survives the early cells of every
        // axis — keyed on rxing's re-encoded bytes, its rxing-only cells
        // scored ZERO
        let total_passed: u32 = score.axes.iter().map(|a| u32::from(a.passed)).sum();
        assert!(
            total_passed >= 12,
            "kanji symbol must survive stress cells: {:?}",
            score.axes
        );
        assert!(score.value >= 50, "score {}", score.value);
    }

    /// The section-skip seam: `score_skip_checks` nulls exactly the skipped
    /// blocks, silences the UEC-driven hints with them, and leaves the
    /// composite untouched — while the empty default stays byte-identical
    /// (uec + iso present on a clean rqrr decode).
    #[test]
    fn skip_checks_null_their_sections_and_leave_value_alone() {
        use crate::ladder::ScoreCheck;
        let (luma, detection, key) = pristine_q_example();
        let run =
            |checks: &[ScoreCheck]| judged(&luma, &detection, &key, ScoreDepth::Full, &[], checks);

        // Default: every section present on a clean bitstream decode.
        let (full, _) = run(&[]);
        assert!(full.uec.is_some(), "clean rqrr decode must measure UEC");
        assert!(full.iso15415.is_some(), "corners + version must yield ISO");

        // Both skipped: sections null, hints silent, composite unchanged.
        let (skipped, hints) = run(&[ScoreCheck::Uec, ScoreCheck::Iso15415]);
        assert!(skipped.uec.is_none());
        assert!(skipped.iso15415.is_none());
        assert!(
            !hints
                .iter()
                .any(|h| matches!(h, Hint::LowCorrectionMargin { .. })),
            "no uec → its miscorrection hint cannot fire"
        );
        assert_eq!(
            skipped.value, full.value,
            "checks are surface truth — the axis composite never moves"
        );

        // UEC alone: ISO still present, its UEC parameter honestly null.
        let (uec_only, _) = run(&[ScoreCheck::Uec]);
        assert!(uec_only.uec.is_none());
        let iso = uec_only.iso15415.expect("iso block still runs");
        assert!(
            iso.unused_error_correction.is_none(),
            "no uec input → the ISO parameter reads null, never faked"
        );
    }

    /// A v2-Q symbol — ONE RS block, 22 EC codewords — with one module
    /// toggled in each of its first `k` codewords (the MSB of codeword `i`
    /// sits at zigzag bit `8·i`): exactly `k` corrected errors in the worst
    /// block, so the margin reads `1 − 2k/22`, while every module boundary
    /// stays crisp and the stress cells keep decoding. 10 px modules on the
    /// standard 4-module quiet zone, decoded by the unbounded Full ladder.
    fn thin_margin_v2q(k: u8) -> (LumaImage, MergedDetection, ContentKey) {
        const MODULE: usize = 10;
        const QUIET: usize = 4;
        let code = qrcode::QrCode::with_version(
            b"thin margin pin",
            qrcode::Version::Normal(2),
            qrcode::EcLevel::Q,
        )
        .unwrap();
        let width = code.width();
        let mut dark: Vec<bool> = code
            .into_colors()
            .into_iter()
            .map(|c| c == qrcode::Color::Dark)
            .collect();
        for &(y, x) in crate::matrix::zigzag::zigzag_positions(2)
            .iter()
            .step_by(8)
            .take(usize::from(k))
        {
            dark[y * width + x] ^= true;
        }
        let side = (width + 2 * QUIET) * MODULE;
        let mut pixels = vec![255u8; side * side];
        for (i, _) in dark.iter().enumerate().filter(|(_, is_dark)| **is_dark) {
            let (my, mx) = (i / width + QUIET, i % width + QUIET);
            for row in pixels.chunks_mut(side).skip(my * MODULE).take(MODULE) {
                row[mx * MODULE..(mx + 1) * MODULE].fill(0);
            }
        }
        let side = u32::try_from(side).unwrap();
        let planes =
            normalize(&ImageInput::luma8(&pixels, side, side), &Limits::default()).unwrap();
        let (detection, key) = primary_of(&planes);
        (planes.luma, detection, key)
    }

    /// Skip invariance on the case the margin-1.0 sibling cannot see: a
    /// thin margin, where the UEC cap is ACTIVE. Skipping the `uec` section
    /// withholds the why (section + the two hints it drives) — never the
    /// verdict: value, grade, `weights_run` and axes are identical.
    #[test]
    fn skipping_uec_never_moves_a_thin_margin_composite() {
        use crate::ladder::ScoreCheck;
        use crate::report::UecGrade;
        const K: u8 = 8;
        let (luma, detection, key) = thin_margin_v2q(K);
        assert!(
            detection.masked_stream.is_some(),
            "precondition: the rqrr stream is present — the margin is measurable"
        );
        // contrast alone: one axis that survives the damage end to end, so
        // the uncapped composite is exactly 100 and every lost point is the cap's
        let only_contrast = [
            StressAxis::Resolution,
            StressAxis::Blur,
            StressAxis::Perspective,
            StressAxis::Rotation,
            StressAxis::Lighting,
        ];
        let run = |checks: &[ScoreCheck]| {
            judged(
                &luma,
                &detection,
                &key,
                ScoreDepth::Full,
                &only_contrast,
                checks,
            )
        };

        let (shown, shown_hints) = run(&[]);
        let uec = shown.uec.expect("the default checks publish the margin");
        assert_eq!(
            (uec.worst_block_errors, uec.worst_block_capacity),
            (K, 22),
            "precondition: exactly K errors in the one v2-Q block: {uec:?}"
        );
        let expected_margin = 1.0 - 2.0 * f32::from(K) / 22.0;
        assert!(
            (uec.margin - expected_margin).abs() < 1e-6,
            "precondition: margin {} vs 1 − 2·{K}/22",
            uec.margin
        );
        assert_eq!(uec.grade, UecGrade::D);
        assert_eq!(
            shown
                .axes
                .iter()
                .map(|a| (a.axis, a.passed, a.total))
                .collect::<Vec<_>>(),
            vec![(StressAxis::Contrast, 5, 5)],
            "precondition: the uncapped composite is 100"
        );
        assert_eq!(shown.value, uec_margin_cap(uec.margin));
        assert_eq!(shown.value, 73, "0.2727/0.5 × 60 + 40 = 72.7 → 73");
        assert!(
            shown_hints.contains(&Hint::RaiseErrorCorrection {
                current: EcLevel::Q
            }),
            "a published grade-D margin drives the raise-EC hint: {shown_hints:?}"
        );

        let (hidden, hidden_hints) = run(&[ScoreCheck::Uec]);
        assert_eq!(
            (hidden.value, hidden.grade, hidden.weights_run),
            (shown.value, shown.grade, shown.weights_run),
            "skipping a check never moves the composite"
        );
        assert_eq!(hidden.axes, shown.axes);
        assert!(hidden.uec.is_none(), "the skipped section stays withheld");
        let shown_iso = shown.iso15415.expect("the ISO block runs");
        let hidden_iso = hidden.iso15415.expect("the ISO block still runs");
        assert!(
            hidden_iso.unused_error_correction.is_none(),
            "the ISO UEC parameter follows the published section"
        );
        // the ISO verdict is the minimum over every MEASURED parameter: the
        // grade-D margin keeps setting it while its parameter is withheld
        let margin_grade = shown_iso
            .unused_error_correction
            .expect("published by default")
            .grade;
        assert_eq!(shown_iso.overall, margin_grade, "{shown_iso:?}");
        assert_eq!(
            hidden_iso.overall, shown_iso.overall,
            "skipping a check never moves the ISO overall"
        );
        assert!(
            !hidden_hints.iter().any(|h| matches!(
                h,
                Hint::RaiseErrorCorrection { .. } | Hint::LowCorrectionMargin { .. }
            )),
            "UEC-driven hints follow the published section: {hidden_hints:?}"
        );
    }

    /// The pristine EC-Q symbol of the section-skip seam — a clean rqrr
    /// decode whose every stress cell survives.
    fn pristine_q_example() -> (LumaImage, MergedDetection, ContentKey) {
        let code =
            qrcode::QrCode::with_error_correction_level(b"https://example.com", qrcode::EcLevel::Q)
                .unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(8, 8)
            .build();
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let planes = normalize(&ImageInput::encoded(&png), &Limits::default()).unwrap();
        let (detection, key) = primary_of(&planes);
        (planes.luma, detection, key)
    }

    /// A deadline that expired before scoring judged NOTHING — so the report
    /// says nothing. The pre-fix pipeline read every unrun cell as a failure:
    /// a pristine symbol came back 0/poor with four fabricated hints.
    #[test]
    fn an_expired_deadline_yields_no_judgment() {
        let (luma, detection, key) = pristine_q_example();
        let cancel = CancelToken::new();
        for depth in [ScoreDepth::Full, ScoreDepth::Reduced] {
            let mut expired = Bound::new(&cancel, Some(Instant::now()));
            let judged = evaluate(&luma, &detection, &key, depth, &[], &[], &mut expired).unwrap();
            assert!(
                judged.is_none(),
                "{depth:?}: an expired deadline must yield no judgment, got {judged:?}"
            );
            assert!(expired.cut(), "{depth:?}: and says the deadline cut it");
            // not even calibration ran: the deadline is checked before every step
            let mut bound = Bound::new(&cancel, Some(Instant::now()));
            let outcome = judge(&luma, &detection, &key, depth, &[], &[], &mut bound).unwrap();
            assert!(
                matches!(outcome, Judgment::Interrupted),
                "{depth:?}: {outcome:?}"
            );
            assert_eq!(bound.spent(), 0, "{depth:?}: no calibration step, no cell");
        }
    }

    /// A judgment the bound refuses at its first step builds nothing — not
    /// even the stress base: the deadline is checked before any pixel work.
    #[test]
    fn a_judgment_refused_at_its_first_step_builds_nothing() {
        let (luma, detection, key) = pristine_q_example();
        let cancel = CancelToken::new();
        let built = || STRESS_BASES.with(std::cell::Cell::get);
        let before = built();
        let mut bound = Bound::new(&cancel, None).with_quota(0);
        let outcome = judge(
            &luma,
            &detection,
            &key,
            ScoreDepth::Full,
            &[],
            &[],
            &mut bound,
        )
        .unwrap();
        assert!(matches!(outcome, Judgment::Interrupted), "{outcome:?}");
        assert_eq!(bound.spent(), 0);
        assert_eq!(built() - before, 0, "no stress base for a refused judgment");
        // one admitted step builds the base once
        let mut bound = Bound::new(&cancel, None).with_quota(1);
        let outcome = judge(
            &luma,
            &detection,
            &key,
            ScoreDepth::Full,
            &[],
            &[],
            &mut bound,
        )
        .unwrap();
        assert!(matches!(outcome, Judgment::Interrupted), "{outcome:?}");
        assert_eq!(built() - before, 1);
    }

    /// A knee bisection the bound refuses drops only its label: the score
    /// stands, and the evaluation reports the cut (the report's flag).
    #[test]
    fn a_cut_bisection_keeps_the_score_and_reports_the_cut() {
        let (luma, detection, key) = kneed_example();
        let cancel = CancelToken::new();
        let mut free = Bound::new(&cancel, None);
        let Judgment::Complete(reference, _) = judge(
            &luma,
            &detection,
            &key,
            ScoreDepth::Full,
            &[],
            &[],
            &mut free,
        )
        .unwrap() else {
            panic!("an unbounded judgment completes");
        };
        assert!(!free.cut(), "an unbounded judgment is never cut");
        let complete = complete_units(&luma, &detection, &key, &reference);
        assert!(
            free.spent() > complete,
            "precondition: knees to bisect after the cells"
        );
        let mut bound = Bound::new(&cancel, None).with_quota(complete);
        let Judgment::Complete(score, _) = judge(
            &luma,
            &detection,
            &key,
            ScoreDepth::Full,
            &[],
            &[],
            &mut bound,
        )
        .unwrap() else {
            panic!("every planned cell fits the quota");
        };
        assert_eq!(
            (score.value, score.grade),
            (reference.value, reference.grade)
        );
        assert!(score.axes.iter().all(|a| a.refined_failed_at.is_none()));
        assert!(bound.cut(), "the refused bisections are a cut");
    }

    /// `ScanConfig::computes_score` and `Judgment::NoAxes` are one rule:
    /// whatever the depth and the skipped axes, a configuration judges
    /// nothing exactly when it computes no score.
    #[test]
    fn judge_no_axes_iff_config_computes_no_score() {
        let (luma, detection, key) = pristine_q_example();
        let cancel = CancelToken::new();
        let every: Vec<StressAxis> = WEIGHTS.iter().map(|&(axis, _)| axis).collect();
        let skips: [&[StressAxis]; 4] = [
            &[],
            &[StressAxis::Lighting],
            &every[..every.len() - 1],
            &every,
        ];
        for depth in [ScoreDepth::Off, ScoreDepth::Reduced, ScoreDepth::Full] {
            for skip in skips {
                let mut config = ScanConfig::full();
                config.score_depth = depth;
                config.score_skip_axes = skip.to_vec();
                // a quota of 0: a planned judgment is interrupted at its first
                // step, so the rule is read without running a cell
                let mut bound = Bound::new(&cancel, None).with_quota(0);
                let outcome = judge(&luma, &detection, &key, depth, skip, &[], &mut bound).unwrap();
                assert_eq!(
                    matches!(outcome, Judgment::NoAxes),
                    !config.computes_score(),
                    "{depth:?} skipping {skip:?}: {outcome:?}"
                );
            }
        }
        assert!(ScanConfig::full().computes_score() && ScanConfig::fast().computes_score());
        assert!(!ScanConfig::frame().computes_score());
    }

    /// No planned cell, no judgment — every axis skipped (an axis-less
    /// composite would be fiction) and depth `Off` (the frame posture) both
    /// judge nothing and spend nothing.
    #[test]
    fn no_planned_cell_judges_nothing() {
        let (luma, detection, key) = pristine_q_example();
        let cancel = CancelToken::new();
        let every_axis: Vec<StressAxis> = WEIGHTS.iter().map(|&(axis, _)| axis).collect();
        for (depth, skip) in [
            (ScoreDepth::Full, every_axis.as_slice()),
            (ScoreDepth::Reduced, every_axis.as_slice()),
            (ScoreDepth::Off, &[][..]),
        ] {
            let mut bound = Bound::new(&cancel, None);
            let outcome = judge(&luma, &detection, &key, depth, skip, &[], &mut bound).unwrap();
            assert!(
                matches!(outcome, Judgment::NoAxes),
                "{depth:?}: {outcome:?}"
            );
            assert_eq!(bound.spent(), 0);
            let mut free = Bound::new(&cancel, None);
            let judged = evaluate(&luma, &detection, &key, depth, skip, &[], &mut free).unwrap();
            assert!(
                judged.is_none() && !free.cut(),
                "{depth:?}: nothing planned, nothing cut: {judged:?}"
            );
        }
    }

    /// Cancellation is QRS-005 at the very first step — calibration
    /// included (its walk used to run up to 17 decodes deaf to the token).
    #[test]
    fn cancellation_reaches_calibration() {
        let (luma, detection, key) = pristine_q_example();
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut bound = Bound::new(&cancel, None);
        let outcome = judge(
            &luma,
            &detection,
            &key,
            ScoreDepth::Full,
            &[],
            &[],
            &mut bound,
        );
        assert!(
            matches!(outcome, Err(crate::error::ScanError::Cancelled)),
            "{outcome:?}"
        );
        assert_eq!(bound.spent(), 0);
    }

    /// A committed fixture decoded by the unbounded Full ladder.
    fn fixture_example(rel: &str) -> (LumaImage, MergedDetection, ContentKey) {
        let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).expect("fixture present");
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let (detection, key) = primary_of(&planes);
        (planes.luma, detection, key)
    }

    /// The blob-style production template: it decodes only through a morph
    /// rung and its ramps knee.
    fn kneed_example() -> (LumaImage, MergedDetection, ContentKey) {
        fixture_example("artistic/blob-style-monkey-logo.webp")
    }

    /// The logo-occluded v5-H symbol only the S5 rescue decodes (errors and
    /// erasures): no stress-cell decode class reads its base — the
    /// Undecodable boundary, still a complete judgment.
    fn rescue_example() -> (LumaImage, MergedDetection, ContentKey) {
        let (luma, detection, key) = fixture_example("degraded/logo-occluded-rescue.png");
        assert_eq!(
            detection.engines,
            vec![crate::report::EngineKind::Rescue],
            "precondition: a rescue-only decode"
        );
        (luma, detection, key)
    }

    /// The unbounded calibration walk: how it ended and the steps it took.
    fn calibration_walk(
        luma: &LumaImage,
        detection: &MergedDetection,
        key: &ContentKey,
    ) -> (Calibrated, u32) {
        let base = transform::downscale_to(luma, STRESS_BASE_SIDE);
        let cancel = CancelToken::new();
        let mut walk = Bound::new(&cancel, None);
        let calibrated =
            CellProbe::calibrate_within(&base, key, detection.symbology, &mut walk).unwrap();
        (calibrated, walk.spent())
    }

    /// The probe steps a complete judgment needs, derived independently of
    /// `judge`: the calibration walk (to a class, or through every class when
    /// none reads the base), then every PLANNED cell — a ramp runs through
    /// its knee, the lighting set runs whole.
    fn complete_units(
        luma: &LumaImage,
        detection: &MergedDetection,
        key: &ContentKey,
        score: &Score,
    ) -> u32 {
        let (calibrated, walk) = calibration_walk(luma, detection, key);
        assert!(
            !matches!(calibrated, Calibrated::Interrupted),
            "an unbounded walk ends"
        );
        let cells: u32 = score
            .axes
            .iter()
            .map(|a| {
                if a.axis == StressAxis::Lighting {
                    u32::from(a.total)
                } else {
                    u32::from(a.passed) + u32::from(a.failed_at.is_some())
                }
            })
            .sum();
        walk + cells
    }

    /// Fixture-specific preconditions of the quota sweep: the kneed symbol
    /// has knees to bisect; the rescue-only one sits on the Undecodable
    /// boundary — its walk tries EVERY decode class and finds none, yet the
    /// judgment completes (every cell judged, value 0).
    fn assert_sweep_preconditions(
        name: &str,
        luma: &LumaImage,
        detection: &MergedDetection,
        key: &ContentKey,
        reference: &Score,
        bisections: usize,
    ) {
        match name {
            "kneed" => assert!(bisections > 0, "precondition: knees to bisect"),
            "undecodable" => {
                let (calibrated, walk) = calibration_walk(luma, detection, key);
                assert!(matches!(calibrated, Calibrated::Undecodable));
                let every_class = u32::try_from(crate::ladder::DEEP_RUNGS.len()).unwrap() + 1;
                assert_eq!(walk, every_class, "shallow class + every deep rung tried");
                assert_eq!(reference.value, 0, "{:?}", reference.axes);
            }
            _ => {}
        }
    }

    /// The deterministic stand-in for a wall-clock cut, landed on EVERY step
    /// in turn: below the complete unit count there is no score at all; at
    /// or above it the judgment is the unbounded one — value, grade,
    /// weights, axes and hints — and only a knee bisection the quota could
    /// not reach may be absent (bisections run last, so at exactly the
    /// complete count none ran). Three shapes: a pristine symbol, a kneed
    /// one, and a rescue-only decode no stress cell can read.
    #[test]
    fn work_quota_cuts_are_absent_never_partial() {
        let cancel = CancelToken::new();
        for (name, (luma, detection, key)) in [
            ("pristine", pristine_q_example()),
            ("kneed", kneed_example()),
            ("undecodable", rescue_example()),
        ] {
            let mut free = Bound::new(&cancel, None);
            let Judgment::Complete(reference, reference_hints) = judge(
                &luma,
                &detection,
                &key,
                ScoreDepth::Full,
                &[],
                &[],
                &mut free,
            )
            .unwrap() else {
                panic!("{name}: an unbounded judgment completes");
            };
            assert_honest_axes(&reference.axes, ScoreDepth::Full);
            let complete = complete_units(&luma, &detection, &key, &reference);
            let bisections = reference
                .axes
                .iter()
                .filter(|a| a.refined_failed_at.is_some())
                .count();
            let total = complete + u32::try_from(bisections).unwrap();
            assert_eq!(free.spent(), total, "{name}: no hidden step");
            assert_sweep_preconditions(name, &luma, &detection, &key, &reference, bisections);
            for quota in 0..=total + 1 {
                let mut bound = Bound::new(&cancel, None).with_quota(quota);
                let outcome = judge(
                    &luma,
                    &detection,
                    &key,
                    ScoreDepth::Full,
                    &[],
                    &[],
                    &mut bound,
                );
                assert!(bound.spent() <= quota, "{name}: the quota bounds the spend");
                assert_eq!(
                    bound.cut(),
                    quota < total,
                    "{name}: quota {quota} of {total}: the bound reports a refused step"
                );
                let (score, hints) = match outcome.unwrap() {
                    Judgment::Complete(score, hints) => (score, hints),
                    Judgment::Interrupted => {
                        assert!(
                            quota < complete,
                            "{name}: quota {quota} ≥ {complete} completes"
                        );
                        continue;
                    }
                    Judgment::NoAxes => panic!("{name}: every axis is planned"),
                };
                assert!(
                    quota >= complete,
                    "{name}: quota {quota} < {complete} judged"
                );
                assert_honest_axes(&score.axes, ScoreDepth::Full);
                assert_eq!(
                    (score.value, score.grade, score.weights_run),
                    (reference.value, reference.grade, reference.weights_run),
                    "{name}: quota {quota}"
                );
                assert_eq!(hints, reference_hints, "{name}: quota {quota}");
                assert_eq!(
                    (score.structural, score.uec, score.iso15415),
                    (reference.structural, reference.uec, reference.iso15415)
                );
                assert_eq!(score.axes.len(), reference.axes.len());
                for (a, r) in score.axes.iter().zip(&reference.axes) {
                    assert_eq!(
                        (a.axis, a.passed, a.total, &a.failed_at),
                        (r.axis, r.passed, r.total, &r.failed_at),
                        "{name}: quota {quota}"
                    );
                    let refined_ok = if quota >= total {
                        a.refined_failed_at == r.refined_failed_at
                    } else if quota == complete {
                        a.refined_failed_at.is_none()
                    } else {
                        a.refined_failed_at.is_none() || a.refined_failed_at == r.refined_failed_at
                    };
                    assert!(refined_ok, "{name}: quota {quota}: {a:?} vs {r:?}");
                }
            }
        }
    }

    /// The grid rqrr detects in `luma`, read as sampled — unmirrored — with
    /// the format word it decodes that way: the still-masked stream, version,
    /// EC level and mask.
    fn as_sampled(luma: &LumaImage) -> (crate::engine::MaskedStream, u8, EcLevel, u8) {
        let width = luma.width() as usize;
        let data = luma.data();
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
                data[y * width + x]
            });
        let grid = prepared.detect_grids().remove(0);
        let (meta, raw) = grid.get_raw_data().expect("the sampled format decodes");
        // rqrr's ecc_level is the format word's two-bit field
        let ec = [EcLevel::M, EcLevel::L, EcLevel::H, EcLevel::Q][usize::from(meta.ecc_level)];
        let stream = crate::engine::MaskedStream {
            bits: raw.data[..raw.len.div_ceil(8)].to_vec(),
            bit_len: raw.len,
        };
        (
            stream,
            u8::try_from(meta.version.0).unwrap(),
            ec,
            u8::try_from(meta.mask).unwrap(),
        )
    }

    /// A stream that does not error-correct errors-only under the format it
    /// was read with measures no margin: no `uec` section, no cap, no margin
    /// hint. The case: a mirrored symbol read unmirrored, whose format word
    /// decodes to another EC level and mask while its stream holds the wrong
    /// orientation's bits — the zero margin such a stream read once capped a
    /// clean mirrored code at 40. The S5 rescue's stream, which needed
    /// erasures, keeps its margin.
    #[test]
    fn a_stream_that_does_not_verify_has_no_uec_and_no_cap() {
        use crate::rescue::{Acceptance, Rederived, Unverified};
        let luma = crate::rescue::test_symbols::from_segments_mirrored(
            qrcode::Version::Normal(3),
            qrcode::EcLevel::M,
            |b| b.push_byte_data(b"https://qrc-ai.com/mirrored"),
        );
        let planes = normalize(
            &ImageInput::luma8(luma.data(), luma.width(), luma.height()),
            &Limits::default(),
        )
        .unwrap();
        let (detection, key) = primary_of(&planes);
        let (verified, _) = judged(&planes.luma, &detection, &key, ScoreDepth::Full, &[], &[]);
        let (stream, version, ec, mask) = as_sampled(&planes.luma);
        assert!(
            matches!(
                crate::rescue::rederive(&stream, version, ec, mask, Acceptance::Engine),
                Rederived::Unverified(Unverified::NothingVerifies)
            ),
            "precondition: the as-sampled stream does not error-correct"
        );
        assert_ne!(
            (Some(ec), Some(mask)),
            (detection.ec, detection.mask),
            "precondition: as sampled, the format word reads another EC level and mask"
        );
        let misread = MergedDetection {
            masked_stream: Some(stream),
            version: Some(version),
            ec: Some(ec),
            mask: Some(mask),
            ..detection.clone()
        };
        let (score, hints) = judged(&planes.luma, &misread, &key, ScoreDepth::Full, &[], &[]);
        assert!(score.uec.is_none(), "no margin from it: {:?}", score.uec);
        assert!(
            score
                .iso15415
                .is_some_and(|iso| iso.unused_error_correction.is_none()),
            "{:?}",
            score.iso15415
        );
        assert_eq!(
            (score.value, score.grade),
            (verified.value, verified.grade),
            "no margin, no cap"
        );
        assert!(
            !hints
                .iter()
                .any(|h| matches!(h, Hint::LowCorrectionMargin { .. })),
            "{hints:?}"
        );
        // the S5 rescue's stream keeps its errors-only count and its hint
        let (luma, detection, key) = rescue_example();
        let (score, hints) = judged(&luma, &detection, &key, ScoreDepth::Full, &[], &[]);
        let uec = score.uec.expect("the rescued stream keeps its margin");
        assert_eq!(
            (uec.margin, uec.worst_block_errors, uec.worst_block_capacity),
            (0.0, 11, 22)
        );
        assert!(hints.contains(&Hint::LowCorrectionMargin {
            errors: 11,
            capacity: 22
        }));
    }

    /// The unbounded Full scan of an in-memory render.
    fn scan_luma(luma: &LumaImage) -> crate::report::ScanReport {
        let mut config = ScanConfig::full();
        config.budget_ms = None;
        crate::Scanner::builder()
            .profile(crate::ScanProfile::Custom(config))
            .build()
            .scan(ImageInput::luma8(luma.data(), luma.width(), luma.height()))
            .unwrap()
    }

    /// A clean code scores the same mirrored as upright: the score measures
    /// the stream of the orientation the decode verified, with that
    /// orientation's EC level and mask — margin 1.0, no margin hint, no
    /// cap. Twins of a no-ECI URL (a pristine 100) and of an ECI 26 URL; and
    /// a symbol whose format word is EC L, mask 0, which reads as another
    /// word when sampled unmirrored: mirrored, it publishes L / 0 and the
    /// full margin. (Its stress cells may differ from its upright twin's:
    /// mirrored, the centred glare lands on other data modules, and EC L
    /// leaves little to spare.)
    #[test]
    fn a_mirrored_symbol_scores_like_its_upright_twin() {
        use crate::report::UecGrade;
        use crate::rescue::test_symbols::{
            from_segments, from_segments_mirrored, masked_colors, mirror, render,
        };
        let url = |b: &mut qrcode::bits::Bits| b.push_byte_data(b"https://qrc-ai.com/x03");
        let eci26 = |b: &mut qrcode::bits::Bits| {
            b.push_eci_designator(26)?;
            b.push_byte_data(b"https://qrc-ai.com/x02")
        };
        let (v3, m) = (qrcode::Version::Normal(3), qrcode::EcLevel::M);
        let (l0, width) = masked_colors(
            qrcode::Version::Normal(2),
            qrcode::EcLevel::L,
            qrcode::canvas::MaskPattern::Checkerboard,
            |b| b.push_byte_data(b"https://qrc-ai.com/x5"),
        );
        let twins = [
            (
                "no-ECI URL",
                from_segments(v3, m, url),
                from_segments_mirrored(v3, m, url),
            ),
            (
                "ECI 26 URL",
                from_segments(v3, m, eci26),
                from_segments_mirrored(v3, m, eci26),
            ),
            (
                "format word L0",
                render(&l0, width),
                render(&mirror(&l0, width), width),
            ),
        ];
        for (name, upright, mirrored) in twins {
            let (up, down) = (scan_luma(&upright), scan_luma(&mirrored));
            let whole = up.score.expect("the upright twin is judged");
            let score = down.score.expect("the mirrored twin is judged");
            let uec = score.uec.expect("the verified stream measures a margin");
            assert_eq!((uec.margin, uec.grade), (1.0, UecGrade::A), "{name}");
            if name == "format word L0" {
                let meta = &down.detections[0].meta;
                assert_eq!((meta.ec_level, meta.mask), (Some(EcLevel::L), Some(0)));
                assert_eq!(score.uec, whole.uec, "{name}");
                assert!(
                    !down
                        .hints
                        .iter()
                        .any(|h| matches!(h, Hint::LowCorrectionMargin { .. })),
                    "{name}: {:?}",
                    down.hints
                );
                continue;
            }
            assert_eq!(
                (score.value, score.grade, &score.axes),
                (whole.value, whole.grade, &whole.axes),
                "{name}"
            );
            assert_eq!(
                (score.uec, score.structural, score.iso15415),
                (whole.uec, whole.structural, whole.iso15415),
                "{name}"
            );
            assert!(
                !down.hints.iter().any(|h| matches!(
                    h,
                    Hint::RaiseErrorCorrection { .. } | Hint::LowCorrectionMargin { .. }
                )),
                "{name}: {:?}",
                down.hints
            );
            assert_eq!(down.hints, up.hints, "{name}");
            if name == "no-ECI URL" {
                assert_eq!((score.value, score.grade), (100, Grade::Excellent));
            }
        }
    }

    /// The margin cap reads the MEASURED margin, published or not: the skip
    /// seam withholds the section and its two hints — never the cap, so
    /// `score_skip_checks` cannot move the value. The published pins
    /// (0.30 → 76 · 0.25 → 70 · 0.0 → 40) hold withheld too.
    #[test]
    fn uec_cap_reads_the_measured_margin_published_or_not() {
        use crate::report::{UecGrade, UecReport};
        let d = detection_with(Some(EcLevel::Q));
        let at = |margin: f32| UecReport {
            margin,
            grade: UecGrade::from_margin(margin),
            worst_block_errors: 12,
            worst_block_capacity: 24,
        };
        for margin in [1.0, 0.5, 0.45, 0.37, 0.30, 0.25, 0.2458, 0.10, 0.0] {
            let (shown, _) = compose(axes_with(&[]), None, Some(at(margin)), true, None, &d);
            let (withheld, hints) =
                compose(axes_with(&[]), None, Some(at(margin)), false, None, &d);
            assert_eq!(
                (withheld.value, withheld.grade, withheld.weights_run),
                (shown.value, shown.grade, shown.weights_run),
                "margin {margin}"
            );
            assert_eq!(withheld.value, uec_margin_cap(margin), "margin {margin}");
            assert!(shown.uec.is_some() && withheld.uec.is_none());
            assert!(
                !hints.iter().any(|h| matches!(
                    h,
                    Hint::RaiseErrorCorrection { .. } | Hint::LowCorrectionMargin { .. }
                )),
                "a withheld margin drives no hint (margin {margin}): {hints:?}"
            );
        }
        let value = |margin: f32| {
            compose(axes_with(&[]), None, Some(at(margin)), false, None, &d)
                .0
                .value
        };
        assert_eq!((value(0.30), value(0.25), value(0.0)), (76, 70, 40));
        // published at the RS limit, both margin hints speak
        let (_, hints) = compose(axes_with(&[]), None, Some(at(0.0)), true, None, &d);
        assert!(
            hints.contains(&Hint::LowCorrectionMargin {
                errors: 12,
                capacity: 24
            }) && hints.contains(&Hint::RaiseErrorCorrection {
                current: EcLevel::Q
            }),
            "{hints:?}"
        );
        // no measurement → no cap, published or not
        for publish in [true, false] {
            let (score, _) = compose(axes_with(&[]), None, None, publish, None, &d);
            assert_eq!(score.value, 100);
        }
    }

    /// Band edges of the cap and the raise-EC gate, for EC Q (the hint can
    /// fire) and EC H (it never can): margin 0.25 caps at exactly 70 (good),
    /// 0.2458 at 69 (acceptable). The `value < 70` gate reads the composite
    /// BEFORE the cap — a withheld margin's 69 drives no hint, while the
    /// published grade (D at 0.25 · F at 0.2458) does.
    #[test]
    fn uec_cap_band_edges_and_the_pre_cap_gate() {
        use crate::report::{UecGrade, UecReport};
        let raised = |hints: &[Hint]| {
            hints
                .iter()
                .any(|h| matches!(h, Hint::RaiseErrorCorrection { .. }))
        };
        let at = |margin: f32| UecReport {
            margin,
            grade: UecGrade::from_margin(margin),
            worst_block_errors: 0,
            worst_block_capacity: 0,
        };
        for ec in [EcLevel::Q, EcLevel::H] {
            let d = detection_with(Some(ec));
            for (margin, cap, grade, band) in [
                (0.25, 70, Grade::Good, UecGrade::D),
                (0.2458, 69, Grade::Acceptable, UecGrade::F),
            ] {
                assert_eq!(at(margin).grade, band);
                for publish in [true, false] {
                    let (score, hints) =
                        compose(axes_with(&[]), None, Some(at(margin)), publish, None, &d);
                    let case = format!("{ec:?} margin {margin} publish {publish}");
                    assert_eq!((score.value, score.grade), (cap, grade), "{case}");
                    assert_eq!(
                        raised(&hints),
                        publish && ec < EcLevel::H,
                        "{case}: {hints:?}"
                    );
                }
            }
        }
        // strict on the PRE-cap value: perspective + rotation 0/5 put the
        // composite at exactly 70 — the withheld 0.2458 margin caps it to 69,
        // which is no reason to raise EC on its own
        let d = detection_with(Some(EcLevel::Q));
        let edge = axes_with(&[
            (StressAxis::Perspective, 0, 5),
            (StressAxis::Rotation, 0, 5),
        ]);
        let (score, hints) = compose(edge, None, Some(at(0.2458)), false, None, &d);
        assert_eq!(score.value, 69);
        assert!(!raised(&hints), "{hints:?}");
        // a pre-cap value below 70 still fires with the margin withheld
        let sunk = axes_with(&[(StressAxis::Blur, 0, 5), (StressAxis::Perspective, 0, 5)]);
        let (score, hints) = compose(sunk, None, Some(at(0.2458)), false, None, &d);
        assert_eq!(score.value, 62);
        assert!(raised(&hints), "{hints:?}");
    }

    /// Dev diagnostic — `cargo nextest run -p qrcode-ai-scanner --run-ignored
    /// only --no-capture -E 'test(pdf417_stress_cells_repeatability)'`.
    /// Re-decodes the SAME pixels of every resolution cell of the PDF417
    /// fixture in one process, with the scoring filter: a pure function of
    /// the pixels prints 0 or RUNS everywhere. rxing's PDF417 decoder breaks
    /// codeword-confidence ties in `HashMap` iteration order (randomized per
    /// map), so the near-knee 256px cell lands in between; then the same
    /// pixels are walked as a published input. Prints, never asserts a rate.
    #[test]
    #[ignore = "dev diagnostic"]
    fn pdf417_stress_cells_repeatability() {
        const RUNS: usize = 100;
        let path = format!(
            "{}/../../fixtures/symbology/pdf417.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(&path).expect("fixture present");
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let (d, key) = primary_of(&planes);
        assert_eq!(d.symbology, crate::report::Symbology::Pdf417);
        let base = transform::downscale_to(&planes.luma, STRESS_BASE_SIDE);
        let probe = CellProbe::calibrate(&base, &key, d.symbology).expect("base decodes");
        let survives = |img: &LumaImage| {
            (0..RUNS)
                .filter(|_| detections_match(&cell_decode(img, d.symbology).detections, &key))
                .count()
        };
        let cancel = CancelToken::new();
        for side in [358u32, 256, 179, 128, 90] {
            let cell = transform::downscale_to(&base, side);
            let judged = (0..RUNS)
                .filter(|_| cell_passes(&cell, &key, probe, &mut Bound::new(&cancel, None)))
                .count();
            println!(
                "{side}px cell ({}x{}): direct {}/{RUNS} · otsu {}/{RUNS} · cell_passes {judged}/{RUNS}",
                cell.width(),
                cell.height(),
                survives(&cell),
                survives(&transform::otsu_threshold(&cell)),
            );
        }
        // the published side: the ladder's verdict over the full frame, then
        // over the 256px pixels handed in as an input of their own
        let walk = |planes: &crate::transform::SourcePlanes| {
            let mut seen = std::collections::BTreeMap::new();
            for _ in 0..RUNS {
                let o =
                    ladder::run(planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
                let texts: Vec<_> = o.merged.iter().map(|m| m.text.clone()).collect();
                let stages: Vec<_> = o
                    .trace
                    .stages
                    .iter()
                    .map(|s| (s.stage.clone(), s.transforms_tried, s.detections_found))
                    .collect();
                *seen.entry(format!("{texts:?} via {stages:?}")).or_insert(0) += 1;
            }
            seen
        };
        println!("full frame, {RUNS} ladder walks: {:#?}", walk(&planes));
        let cell = transform::downscale_to(&base, 256);
        let cell_input = ImageInput::luma8(cell.data(), cell.width(), cell.height());
        let cell_planes = normalize(&cell_input, &Limits::default()).unwrap();
        println!(
            "256px pixels as an input, {RUNS} ladder walks: {:#?}",
            walk(&cell_planes)
        );
    }
}
