//! Score contract v3 — survival ramps · structural caps · hints.
//!
//! Output language: VALIDATION, never ISO verification (no calibrated
//! optics). Geometry-class signals (perspective/rotation survival, finder
//! integrity, quiet zone) transfer to uncalibrated digital images;
//! reflectance-class signals (contrast/lighting survival) are relative.
//!
//! Determinism: every stress cell is a pure transform of the normalized
//! luma; ramps stop at the first failure (the knee). The lighting set is
//! unordered — no knee-exit, though depth still picks the cell subset.
//! Same input + same depth ⇒ same score, always.

pub(crate) mod iso15415;
pub(crate) mod structural;
pub(crate) mod uec;
pub(crate) mod warp;

use web_time::Instant;

use crate::engine::{self};
use crate::error::Result;
use crate::input::LumaImage;
use crate::ladder::{CancelToken, MergedDetection, ScoreDepth};
use crate::report::{AxisScore, EcLevel, Grade, Hint, Score, StressAxis};
use crate::transform;

/// A stress-cell builder: base image + ramp index → transformed cell.
type RampBuilder<'a> = &'a dyn Fn(&LumaImage, usize) -> LumaImage;

/// Stress cells run on a ≤512px base — bounded cost, consistent geometry.
const STRESS_BASE_SIDE: u32 = 512;

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

fn detections_match(found: &[engine::RawDetection], expected_text: &str) -> bool {
    found
        .iter()
        .any(|d| engine::charset::resolve(&d.raw).0 == expected_text)
}

/// Stress cells decode ONLY the scored symbology — survival of the same
/// content in another symbology would be a false signal, and the filtered
/// pass keeps cell cost at one decoder instead of all of them.
fn cell_decode(img: &LumaImage, symbology: crate::report::Symbology) -> engine::EngineOutcome {
    engine::decode_filtered(img, engine::FormatFilter::Only(symbology))
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
    /// deep rung. `None` = the base does not decode at stress scale at
    /// all (score legitimately reads zero margin).
    pub(crate) fn calibrate(
        base: &LumaImage,
        expected_text: &str,
        symbology: crate::report::Symbology,
    ) -> Option<Self> {
        if detections_match(&cell_decode(base, symbology).detections, expected_text) {
            return Some(Self {
                rung: None,
                symbology,
            });
        }
        let binarized = transform::otsu_threshold(base);
        if detections_match(
            &cell_decode(&binarized, symbology).detections,
            expected_text,
        ) {
            return Some(Self {
                rung: None,
                symbology,
            });
        }
        for rung in crate::ladder::DEEP_RUNGS {
            let boosted = rung.apply(base);
            if detections_match(&cell_decode(&boosted, symbology).detections, expected_text) {
                return Some(Self {
                    rung: Some(rung),
                    symbology,
                });
            }
        }
        None
    }
}

/// One stress cell: did the (transformed) image still decode to the same
/// SYMBOL? Identity is the charset-RESOLVED text, not raw bytes — for
/// kanji-mode symbols rxing re-encodes text as UTF-8 while rqrr keeps the
/// original Shift-JIS bytes (the exact divergence the ladder merge handles;
/// raw-keyed cells silently failed every rxing-only kanji survival).
///
/// Probe = direct + otsu + the baseline's deep rung when it needed one —
/// the margin is measured RELATIVE to the symbol's own decode class, never
/// with the full ladder's recovery power (that asymmetry is the point).
pub(crate) fn cell_passes(img: &LumaImage, expected_text: &str, probe: CellProbe) -> bool {
    if detections_match(&cell_decode(img, probe.symbology).detections, expected_text) {
        return true;
    }
    let binarized = transform::otsu_threshold(img);
    if detections_match(
        &cell_decode(&binarized, probe.symbology).detections,
        expected_text,
    ) {
        return true;
    }
    match probe.rung {
        Some(rung) => {
            let boosted = rung.apply(img);
            detections_match(
                &cell_decode(&boosted, probe.symbology).detections,
                expected_text,
            )
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

/// Run one ordered ramp with early-stop at the first failure (the knee).
/// The shared scan deadline stops further cells (run-out reads as the knee:
/// budget cuts are wall-clock — determinism holds modulo budget, as
/// documented on the crate contract).
fn run_ramp(
    base: &LumaImage,
    expected_text: &str,
    cancel: &CancelToken,
    deadline: Option<Instant>,
    probe: CellProbe,
    indices: &[usize],
    build: impl Fn(&LumaImage, usize) -> LumaImage,
) -> Result<(u8, u8, Option<usize>)> {
    let mut passed = 0u8;
    let mut knee = None;
    for &i in indices {
        if cancel.is_cancelled() {
            return Err(crate::error::ScanError::Cancelled);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let cell = build(base, i);
        if cell_passes(&cell, expected_text, probe) {
            passed += 1;
        } else {
            knee = Some(i); // knee — ordered intensities, later cells only get harder
            break;
        }
    }
    Ok((passed, u8::try_from(indices.len()).unwrap_or(u8::MAX), knee))
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

/// Run the five ordered ramps + the lighting set at the given depth.
/// Axes in `skip` never run — their cells are never built (the integration
/// perf win) and they are absent from the returned list (the report
/// self-describes what was measured).
fn run_axes(
    base: &LumaImage,
    expected_text: &str,
    depth: ScoreDepth,
    skip: &[StressAxis],
    cancel: &CancelToken,
    deadline: Option<Instant>,
    probe: CellProbe,
) -> Result<Vec<AxisScore>> {
    // ---- ordered ramps (intensity grows with the index) ----
    let resolution_sides: [u32; 5] = [358, 256, 179, 128, 90];
    let blur_sigmas: [f32; 5] = [0.5, 1.0, 1.5, 2.0, 2.5];
    let contrast_factors: [f32; 5] = [0.7, 0.55, 0.4, 0.3, 0.2];
    let tilt_degrees: [f32; 5] = [10.0, 18.0, 26.0, 34.0, 42.0];
    let rotation_degrees: [f32; 5] = [10.0, 20.0, 30.0, 40.0, 50.0];

    let indices = depth_indices(depth);
    let mut axes = Vec::with_capacity(6);
    let ramps: [(StressAxis, RampBuilder<'_>); 5] = [
        (StressAxis::Resolution, &|b, i| {
            transform::downscale_to(b, resolution_sides[i])
        }),
        (StressAxis::Blur, &|b, i| {
            transform::gaussian_blur(b, blur_sigmas[i])
        }),
        (StressAxis::Contrast, &|b, i| {
            transform::contrast_boost(b, contrast_factors[i], 1.0)
        }),
        (StressAxis::Perspective, &|b, i| {
            warp::perspective_tilt(b, tilt_degrees[i])
        }),
        (StressAxis::Rotation, &|b, i| {
            warp::rotate(b, rotation_degrees[i])
        }),
    ];
    for (axis, build) in ramps {
        if skip.contains(&axis) {
            continue; // never built, never run — absent from the report
        }
        let (passed, total, knee) =
            run_ramp(base, expected_text, cancel, deadline, probe, indices, build)?;
        axes.push(AxisScore {
            axis,
            passed,
            total,
            failed_at: knee.map(|i| cell_label(axis, i).to_owned()),
        });
    }

    // ---- lighting: an unordered defect set ----
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
    let lighting_picks: &[usize] = match depth {
        ScoreDepth::Off => &[],
        ScoreDepth::Reduced => &[1, 2],
        ScoreDepth::Full => &[0, 1, 2, 3, 4],
    };
    if !skip.contains(&StressAxis::Lighting) {
        let mut lighting_passed = 0u8;
        let mut first_failed = None;
        for &i in lighting_picks {
            if cancel.is_cancelled() {
                return Err(crate::error::ScanError::Cancelled);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            if cell_passes(&lighting_cells[i](base), expected_text, probe) {
                lighting_passed += 1;
            } else if first_failed.is_none() {
                first_failed = Some(i);
            }
        }
        axes.push(AxisScore {
            axis: StressAxis::Lighting,
            passed: lighting_passed,
            total: u8::try_from(lighting_picks.len()).unwrap_or(u8::MAX),
            failed_at: first_failed.map(|i| cell_label(StressAxis::Lighting, i).to_owned()),
        });
    }
    Ok(axes)
}

/// Weighted composite + structural caps + the hint list.
fn compose(
    axes: Vec<AxisScore>,
    structural: Option<crate::report::StructuralReport>,
    uec: Option<crate::report::UecReport>,
    iso15415: Option<crate::report::Iso15415Report>,
    detection: &MergedDetection,
) -> (Score, Vec<Hint>) {
    // Renormalize over the axes that RAN: with the full six this divides by
    // 100 (Σ contract weights — byte-identical to the fixed divisor it
    // replaces); with skipped axes the remaining weights re-span 0-100.
    // run_axes never returns an empty list (evaluate() short-circuits the
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
        && (value < 70 || thin_margin)
    {
        hints.push(Hint::RaiseErrorCorrection { current });
    }
    // Margin ZERO is qualitatively different from thin: the worst block
    // consumed its entire correction budget, which is exactly the signature
    // of a Reed-Solomon miscorrection (caught live on the zxing blackbox
    // corpus: rqrr returned "photography" for a "photograph" ground truth
    // at 12/24 errors — this hint is the machine-readable distrust signal).
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
        axes,
        structural,
        uec,
        iso15415,
    };
    (score, hints)
}

/// Evaluate score v3 for the primary detection. `None` when the skip set
/// covers every axis — an axis-less composite would be fiction (callers
/// treat it exactly like `ScoreDepth::Off`).
pub(crate) fn evaluate(
    luma: &LumaImage,
    detection: &MergedDetection,
    depth: ScoreDepth,
    skip: &[StressAxis],
    skip_checks: &[crate::ladder::ScoreCheck],
    cancel: &CancelToken,
    deadline: Option<Instant>,
) -> Result<Option<(Score, Vec<Hint>)>> {
    if WEIGHTS.iter().all(|(axis, _)| skip.contains(axis)) {
        return Ok(None);
    }
    let base = transform::downscale_to(luma, STRESS_BASE_SIDE);
    // calibrate the cell probe on the unstressed base (its decode class)
    let probe =
        CellProbe::calibrate(&base, &detection.text, detection.symbology).unwrap_or(CellProbe {
            rung: None,
            symbology: detection.symbology,
        });
    let axes = run_axes(&base, &detection.text, depth, skip, cancel, deadline, probe)?;
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
    // Skipped checks never run — no UEC bitstream walk, no ISO parameter
    // sweep. Their absence cascades honestly: no uec → its hints can't fire
    // (compose reads the Options) and a still-present ISO block nulls its
    // unused_error_correction parameter.
    let skip_uec = skip_checks.contains(&crate::ladder::ScoreCheck::Uec);
    let skip_iso = skip_checks.contains(&crate::ladder::ScoreCheck::Iso15415);
    let uec_report = if skip_uec {
        None
    } else {
        match (
            &detection.masked_stream,
            detection.version,
            detection.ec,
            detection.mask,
        ) {
            (Some(stream), Some(version), Some(ec), Some(mask)) => {
                uec::compute(&stream.bits, stream.bit_len, version, ec, mask)
            }
            _ => None,
        }
    };
    let iso = if skip_iso {
        None
    } else {
        match (detection.corners, detection.version, &structural) {
            (Some(corners), Some(version), Some(s)) => {
                iso15415::compute(sample_luma, corners, version, s, uec_report.as_ref())
            }
            _ => None,
        }
    };
    Ok(Some(compose(axes, structural, uec_report, iso, detection)))
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

    fn axes_with(overrides: &[(StressAxis, u8, u8)]) -> Vec<AxisScore> {
        let mut axes: Vec<AxisScore> = WEIGHTS
            .iter()
            .map(|&(axis, _)| AxisScore {
                axis,
                passed: 5,
                total: 5,
                failed_at: None,
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
        let (score, _) = compose(axes_with(&[]), None, None, None, &d);
        assert_eq!(score.value, 100);

        // resolution 4/5: (22·80 + 78·100)/100 = 95
        let (score, _) = compose(
            axes_with(&[(StressAxis::Resolution, 4, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 95);

        // blur 0/5: (18·0 + 82·100)/100 = 82
        let (score, hints) = compose(axes_with(&[(StressAxis::Blur, 0, 5)]), None, None, None, &d);
        assert_eq!(score.value, 82);
        assert!(hints.contains(&Hint::ReduceArtTexture), "{hints:?}");

        // rotation 2/5: (10·40 + 90·100)/100 = 94
        let (score, _) = compose(
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
        let (score, hints) = compose(
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
        let (score, hints) = compose(
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
        let (score, hints) = compose(
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
        let (_, hints) = compose(
            axes_with(&[(StressAxis::Contrast, 2, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(hints.contains(&Hint::IncreaseContrast), "{hints:?}");
        let (_, hints) = compose(
            axes_with(&[(StressAxis::Contrast, 3, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(!hints.contains(&Hint::IncreaseContrast), "{hints:?}");

        // resolution likewise → EnlargeModules
        let (_, hints) = compose(
            axes_with(&[(StressAxis::Resolution, 2, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(hints.contains(&Hint::EnlargeModules), "{hints:?}");
        let (_, hints) = compose(
            axes_with(&[(StressAxis::Resolution, 3, 5)]),
            None,
            None,
            None,
            &d,
        );
        assert!(!hints.contains(&Hint::EnlargeModules), "{hints:?}");

        // blur partial survival (1/5) is NOT the texture hint (only 0/5 is)
        let (_, hints) = compose(axes_with(&[(StressAxis::Blur, 1, 5)]), None, None, None, &d);
        assert!(!hints.contains(&Hint::ReduceArtTexture), "{hints:?}");
    }

    #[test]
    fn raise_ec_hint_value_and_uec_paths() {
        use crate::report::{UecGrade, UecReport};
        let d = detection_with(Some(EcLevel::Q));

        // value 94 (rotation 2/5) + EC<H + healthy margin → no hint
        let (_, hints) = compose(
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
        let (score, hints) = compose(
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

        // thin UEC margin (grade D) fires even at value 100
        let thin = UecReport {
            margin: 0.30,
            grade: UecGrade::D,
            worst_block_errors: 6,
            worst_block_capacity: 18,
        };
        let (score, hints) = compose(axes_with(&[]), None, Some(thin), None, &d);
        assert_eq!(score.value, 100);
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
        let (_, hints) = compose(axes_with(&[]), None, Some(limit), None, &d);
        assert!(
            hints.contains(&Hint::LowCorrectionMargin {
                errors: 12,
                capacity: 24
            }),
            "{hints:?}"
        );
        // thin-but-nonzero margin does NOT fire the miscorrection hint
        let (_, hints) = compose(axes_with(&[]), None, Some(thin), None, &d);
        assert!(
            !hints
                .iter()
                .any(|h| matches!(h, Hint::LowCorrectionMargin { .. })),
            "{hints:?}"
        );

        // EC already H → never fires
        let dh = detection_with(Some(EcLevel::H));
        let (_, hints) = compose(
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

    #[test]
    fn depth_index_sets_pinned() {
        assert_eq!(depth_indices(ScoreDepth::Off), &[] as &[usize]);
        assert_eq!(depth_indices(ScoreDepth::Reduced), &[1, 3]);
        assert_eq!(depth_indices(ScoreDepth::Full), &[0, 1, 2, 3, 4]);
    }

    /// An axis can legitimately carry `total == 0` (`ScoreDepth::Off`, an empty
    /// ramp). Every `score.total > 0` guard protects a division BY that total,
    /// so a `>`→`>=` swap divides by zero. Under each mutant the matching
    /// compose below panics; under the real guard it yields the exact value.
    #[test]
    fn zero_total_axes_are_skipped_not_divided() {
        let d = detection_with(Some(EcLevel::H));

        // weighted-sum guard (:286) — resolution skipped ⇒ (100−22) = 78
        let (score, _) = compose(
            axes_with(&[(StressAxis::Resolution, 0, 0)]),
            None,
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 78);

        // contrast-hint guard (:322) — contrast skipped ⇒ (100−15) = 85, no hint
        let (score, hints) = compose(
            axes_with(&[(StressAxis::Contrast, 0, 0)]),
            None,
            None,
            None,
            &d,
        );
        assert_eq!(score.value, 85);
        assert!(!hints.contains(&Hint::IncreaseContrast), "{hints:?}");

        // resolution-hint guard (:328) — a 0/0 axis is not "≤40% survival"
        let (_, hints) = compose(
            axes_with(&[(StressAxis::Resolution, 0, 0)]),
            None,
            None,
            None,
            &d,
        );
        assert!(!hints.contains(&Hint::EnlargeModules), "{hints:?}");

        // blur-hint guard (:334) — total==0 must NOT read as "dies at the
        // mildest blur" (that guard is `t > 0`, then `p == 0`)
        let (score, hints) = compose(axes_with(&[(StressAxis::Blur, 0, 0)]), None, None, None, &d);
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
        let (score, hints) = compose(
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

    /// Decode a pristine QR and hand back the calibrated stress base + probe,
    /// so the private `run_axes` can be driven directly (its internals never
    /// had a value-level test — the lighting accumulator and deadline break
    /// only surface here).
    fn clean_base_and_probe() -> (LumaImage, String, CellProbe) {
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
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
        let d = &outcome.merged[0];
        let base = transform::downscale_to(&planes.luma, STRESS_BASE_SIDE);
        let probe = CellProbe::calibrate(&base, &d.text, d.symbology).unwrap();
        (base, d.text.clone(), probe)
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
        let (base, text, probe) = clean_base_and_probe();
        let axes = run_axes(
            &base,
            &text,
            ScoreDepth::Full,
            &[],
            &CancelToken::new(),
            None,
            probe,
        )
        .unwrap();
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
            let outcome =
                ladder::run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
            let detection = &outcome.merged[0];
            evaluate(
                &planes.luma,
                detection,
                ScoreDepth::Full,
                &[],
                &[],
                &CancelToken::new(),
                None,
            )
            .unwrap()
            .expect("builder renders always score")
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
        let text = "rotation probe: the frame must never eat the finders";
        let probe = CellProbe::calibrate(&base, text, crate::report::Symbology::QrCode)
            .expect("flawless base calibrates");
        let axes = run_axes(
            &base,
            text,
            ScoreDepth::Full,
            &[],
            &CancelToken::new(),
            None,
            probe,
        )
        .unwrap();
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
        let (base, text, probe) = clean_base_and_probe();
        let full = run_axes(
            &base,
            &text,
            ScoreDepth::Full,
            &[],
            &CancelToken::new(),
            None,
            probe,
        )
        .unwrap();
        let skipped = run_axes(
            &base,
            &text,
            ScoreDepth::Full,
            &[StressAxis::Perspective, StressAxis::Rotation],
            &CancelToken::new(),
            None,
            probe,
        )
        .unwrap();
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
        let (score_full, _) = compose(full, None, None, None, &d);
        let (score_skip, _) = compose(skipped, None, None, None, &d);
        assert_eq!(score_full.value, 100, "full six-axis composite");
        assert_eq!(score_skip.value, 100, "renormalized four-axis composite");
        // And the renormalization actually renormalizes: a half-passed
        // lighting axis alone must read 50, not 7 (its weight over 100).
        let lone = vec![AxisScore {
            axis: StressAxis::Lighting,
            passed: 1,
            total: 2,
            failed_at: None,
        }];
        let (score_lone, _) = compose(lone, None, None, None, &d);
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
        let probe =
            CellProbe::calibrate(&base, "canvas glare pin", crate::report::Symbology::QrCode)
                .expect("corner symbol calibrates");
        let axes = run_axes(
            &base,
            "canvas glare pin",
            ScoreDepth::Full,
            &[],
            &CancelToken::new(),
            None,
            probe,
        )
        .unwrap();
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
        let (base, text, probe) = clean_base_and_probe();
        let axes = run_axes(
            &base,
            &text,
            ScoreDepth::Full,
            &[],
            &CancelToken::new(),
            None,
            probe,
        )
        .unwrap();
        let lighting = axes
            .iter()
            .find(|a| a.axis == StressAxis::Lighting)
            .unwrap();
        assert!(
            lighting.passed >= 1,
            "clean symbol must survive ≥1 lighting cell: {lighting:?}"
        );
    }

    /// A deadline already in the past must break the lighting loop before any
    /// cell runs (`Instant::now() >= d`). The mutant `<` (:257) never fires, so
    /// it would process the set and report survivors — the ramps break either
    /// way, so the lighting axis is the discriminator.
    #[test]
    fn a_past_deadline_stops_the_lighting_set() {
        let (base, text, probe) = clean_base_and_probe();
        let past = Instant::now();
        let axes = run_axes(
            &base,
            &text,
            ScoreDepth::Full,
            &[],
            &CancelToken::new(),
            Some(past),
            probe,
        )
        .unwrap();
        let lighting = axes
            .iter()
            .find(|a| a.axis == StressAxis::Lighting)
            .unwrap();
        assert_eq!(
            lighting.passed, 0,
            "a past deadline must break before any lighting cell: {lighting:?}"
        );
    }

    /// Kanji-mode regression for the SCORING path (the ladder-side fix has
    /// its own test): stress cells must match by resolved text — raw-keyed
    /// cells silently failed every rxing-only survival on kanji symbols.
    #[test]
    fn kanji_symbol_scores_with_text_keyed_cells() {
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
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
        let detection = &outcome.merged[0];
        assert_eq!(detection.text, "こんにちは");

        let (score, _) = evaluate(
            &planes.luma,
            detection,
            ScoreDepth::Full,
            &[],
            &[],
            &CancelToken::new(),
            None,
        )
        .unwrap()
        .expect("empty skip set always scores");
        // a pristine 8px/module symbol survives the early cells of every
        // axis — raw-keyed matching scored this ZERO on rxing-only cells
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
    /// axis-based composite untouched — while the empty default stays
    /// byte-identical (uec + iso present on a clean rqrr decode).
    #[test]
    fn skip_checks_null_their_sections_and_leave_value_alone() {
        use crate::ladder::ScoreCheck;
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
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
        let detection = &outcome.merged[0];

        let run = |checks: &[crate::ladder::ScoreCheck]| {
            evaluate(
                &planes.luma,
                detection,
                ScoreDepth::Full,
                &[],
                checks,
                &CancelToken::new(),
                None,
            )
            .unwrap()
            .expect("axes always run here")
        };

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
}
