//! Score contract v3 — survival ramps · structural caps · hints.
//!
//! Output language: VALIDATION, never ISO verification (no calibrated
//! optics). Geometry-class signals (perspective/rotation survival, finder
//! integrity, quiet zone) transfer to uncalibrated digital images;
//! reflectance-class signals (contrast/lighting survival) are relative.
//!
//! Determinism: every stress cell is a pure transform of the normalized
//! luma; ramps stop at the first failure (the knee), the lighting set always
//! runs in full. Same input + same depth ⇒ same score, always.

pub(crate) mod structural;
pub(crate) mod warp;

use crate::engine::{self, EngineOptions};
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

/// One stress cell: did the (transformed) image still decode to `expected`?
fn cell_passes(img: &LumaImage, expected_raw: &[u8]) -> bool {
    // Fast subset: direct + otsu — the score measures the MARGIN, not the
    // full ladder's recovery power (that asymmetry is the point: a code
    // that needs the deep ladder after mild stress has no margin).
    let outcome = engine::decode_all(img, EngineOptions::default());
    if outcome.detections.iter().any(|d| d.raw == expected_raw) {
        return true;
    }
    let binarized = transform::otsu_threshold(img);
    let outcome = engine::decode_all(&binarized, EngineOptions::default());
    outcome.detections.iter().any(|d| d.raw == expected_raw)
}

/// Ramp cell indices at a given depth (Reduced takes the 2nd and 4th
/// intensities of each five-step ramp).
fn depth_indices(depth: ScoreDepth) -> &'static [usize] {
    match depth {
        ScoreDepth::Off => &[],
        ScoreDepth::Reduced => &[1, 3],
        _ => &[0, 1, 2, 3, 4],
    }
}

/// Run one ordered ramp with early-stop at the first failure (the knee).
fn run_ramp(
    base: &LumaImage,
    expected: &[u8],
    cancel: &CancelToken,
    indices: &[usize],
    build: impl Fn(&LumaImage, usize) -> LumaImage,
) -> Result<(u8, u8)> {
    let mut passed = 0u8;
    for &i in indices {
        if cancel.is_cancelled() {
            return Err(crate::error::ScanError::Cancelled);
        }
        let cell = build(base, i);
        if cell_passes(&cell, expected) {
            passed += 1;
        } else {
            break; // knee — ordered intensities, later cells only get harder
        }
    }
    Ok((passed, u8::try_from(indices.len()).unwrap_or(u8::MAX)))
}

/// Run the five ordered ramps + the lighting set at the given depth.
fn run_axes(
    base: &LumaImage,
    expected: &[u8],
    depth: ScoreDepth,
    cancel: &CancelToken,
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
        let (passed, total) = run_ramp(base, expected, cancel, indices, build)?;
        axes.push(AxisScore {
            axis,
            passed,
            total,
        });
    }

    // ---- lighting: an unordered defect set ----
    #[allow(
        clippy::cast_precision_loss,
        reason = "image dimensions bounded by Limits::max_dimension (lint presence varies by build shape)"
    )]
    let (cx, cy, radius) = (
        base.width() as f32 * 0.3,
        base.height() as f32 * 0.3,
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
        _ => &[0, 1, 2, 3, 4],
    };
    let mut lighting_passed = 0u8;
    for &i in lighting_picks {
        if cancel.is_cancelled() {
            return Err(crate::error::ScanError::Cancelled);
        }
        if cell_passes(&lighting_cells[i](base), expected) {
            lighting_passed += 1;
        }
    }
    axes.push(AxisScore {
        axis: StressAxis::Lighting,
        passed: lighting_passed,
        total: u8::try_from(lighting_picks.len()).unwrap_or(u8::MAX),
    });
    Ok(axes)
}

/// Weighted composite + structural caps + the hint list.
fn compose(
    axes: Vec<AxisScore>,
    structural: Option<crate::report::StructuralReport>,
    detection: &MergedDetection,
) -> (Score, Vec<Hint>) {
    let mut weighted = 0u32;
    for score in &axes {
        let weight = WEIGHTS
            .iter()
            .find(|(axis, _)| *axis == score.axis)
            .map_or(0, |(_, w)| *w);
        if score.total > 0 {
            weighted += weight * u32::from(score.passed) * 100 / u32::from(score.total);
        }
    }
    let mut value = u8::try_from((weighted / 100).min(100)).unwrap_or(100);

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
    if let Some(current) = detection.ec
        && current < EcLevel::H
        && value < 70
    {
        hints.push(Hint::RaiseErrorCorrection { current });
    }

    let score = Score {
        value,
        grade: Grade::from_value(value),
        axes,
        structural,
    };
    (score, hints)
}

/// Evaluate score v3 for the primary detection.
pub(crate) fn evaluate(
    luma: &LumaImage,
    detection: &MergedDetection,
    depth: ScoreDepth,
    cancel: &CancelToken,
) -> Result<(Score, Vec<Hint>)> {
    let base = transform::downscale_to(luma, STRESS_BASE_SIDE);
    let axes = run_axes(&base, &detection.raw, depth, cancel)?;
    let structural = match (detection.corners, detection.version) {
        (Some(corners), Some(version)) => structural::check(luma, corners, version),
        _ => None,
    };
    Ok(compose(axes, structural, detection))
}
