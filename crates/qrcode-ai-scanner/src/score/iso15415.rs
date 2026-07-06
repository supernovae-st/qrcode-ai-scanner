//! ISO/IEC 15415-informed grade card — the software-measurable subset.
//!
//! See [`Iso15415Report`] for the honesty contract. Everything here is
//! deterministic: module means via the same [`GridSampler`] homography the
//! structural checks use, percentile statistics instead of extremes
//! (noise robustness on photographic inputs), grade bands verbatim from
//! ISO 15415 (cross-checked against the GS1 2D verification guideline).

use crate::input::LumaImage;
use crate::matrix::sampler::GridSampler;
use crate::report::{Iso15415Report, IsoGrade, IsoParameter, Point, StructuralReport, UecReport};

/// Grade a "higher is better" value against descending band floors
/// `[a, b, c, d]`.
fn grade_floor(value: f32, floors: [f32; 4]) -> IsoGrade {
    if value >= floors[0] {
        IsoGrade::A
    } else if value >= floors[1] {
        IsoGrade::B
    } else if value >= floors[2] {
        IsoGrade::C
    } else if value >= floors[3] {
        IsoGrade::D
    } else {
        IsoGrade::F
    }
}

/// Grade a "lower is better" value against ascending band ceilings
/// `[a, b, c, d]`.
fn grade_ceiling(value: f32, ceilings: [f32; 4]) -> IsoGrade {
    if value <= ceilings[0] {
        IsoGrade::A
    } else if value <= ceilings[1] {
        IsoGrade::B
    } else if value <= ceilings[2] {
        IsoGrade::C
    } else if value <= ceilings[3] {
        IsoGrade::D
    } else {
        IsoGrade::F
    }
}

/// Sorted-slice percentile (nearest-rank; `p` in 0.0..=1.0).
fn percentile(sorted: &[u8], p: f32) -> u8 {
    debug_assert!(!sorted.is_empty());
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "index math over ≤177² module samples, p clamped"
    )]
    let idx = ((sorted.len() - 1) as f32 * p.clamp(0.0, 1.0)).round() as usize;
    sorted[idx]
}

/// Symbol Contrast + Modulation from the module-mean distribution.
fn contrast_and_modulation(samples: &[u8]) -> Option<(IsoParameter, IsoParameter)> {
    if samples.len() < 16 {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let r_low = f32::from(percentile(&sorted, 0.02));
    let r_high = f32::from(percentile(&sorted, 0.98));
    let span = r_high - r_low;
    let sc_value = span / 255.0;
    let symbol_contrast = IsoParameter {
        value: sc_value,
        grade: grade_floor(sc_value, [0.70, 0.55, 0.40, 0.20]),
    };
    if span <= f32::EPSILON {
        // flat image: contrast F, modulation has no denominator — floor it
        return Some((
            symbol_contrast,
            IsoParameter {
                value: 0.0,
                grade: IsoGrade::F,
            },
        ));
    }
    let global_threshold = f32::midpoint(r_high, r_low);
    // per-module MOD = 2·|R − GT| / SC, clamped to 1 (percentile clipping
    // can push outliers past the nominal range)
    let mut mods: Vec<f32> = samples
        .iter()
        .map(|&r| ((2.0 * (f32::from(r) - global_threshold).abs()) / span).min(1.0))
        .collect();
    mods.sort_unstable_by(f32::total_cmp);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "index math over ≤177² module samples"
    )]
    let mod_value = mods[((mods.len() - 1) as f32 * 0.05).round() as usize];
    let modulation = IsoParameter {
        value: mod_value,
        grade: grade_floor(mod_value, [0.50, 0.40, 0.30, 0.20]),
    };
    Some((symbol_contrast, modulation))
}

fn dist(a: Point, b: Point) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// Axial Nonuniformity from the corner quad (clockwise from top-left).
pub(super) fn axial_nonuniformity(corners: [Point; 4]) -> IsoParameter {
    let x_span = f32::midpoint(dist(corners[0], corners[1]), dist(corners[3], corners[2]));
    let y_span = f32::midpoint(dist(corners[0], corners[3]), dist(corners[1], corners[2]));
    let mean = f32::midpoint(x_span, y_span);
    let value = if mean <= f32::EPSILON {
        1.0
    } else {
        (x_span - y_span).abs() / mean
    };
    IsoParameter {
        value,
        grade: grade_ceiling(value, [0.06, 0.08, 0.10, 0.12]),
    }
}

/// Fixed Pattern Damage approximation from the structural checks.
fn fixed_pattern_damage(structural: &StructuralReport) -> IsoParameter {
    let worst = structural
        .finder_integrity
        .iter()
        .copied()
        .fold(1.0f32, f32::min);
    let mut grade = grade_floor(worst, [0.95, 0.90, 0.80, 0.70]);
    if !structural.quiet_zone_ok {
        grade = grade.max(IsoGrade::D); // Ord: A < … < F, max = worse
    }
    IsoParameter {
        value: worst,
        grade,
    }
}

/// Build the grade card. `None` when the sampler can't be constructed or the
/// symbol is degenerate (sub-16 module sample).
pub(crate) fn compute(
    img: &LumaImage,
    corners: [Point; 4],
    version: u8,
    structural: &StructuralReport,
    uec: Option<&UecReport>,
) -> Option<Iso15415Report> {
    let modules = u32::from(version) * 4 + 17;
    let sampler = GridSampler::new(img, corners, modules)?;
    #[expect(clippy::cast_possible_wrap, reason = "modules ≤ 177")]
    let n = modules as i32;
    let mut module_means = Vec::with_capacity((modules * modules) as usize);
    for my in 0..n {
        for mx in 0..n {
            if let Some(v) = sampler.module(mx, my) {
                module_means.push(v);
            }
        }
    }
    let (symbol_contrast, modulation) = contrast_and_modulation(&module_means)?;
    let anu = axial_nonuniformity(corners);
    let fpd = fixed_pattern_damage(structural);
    let uec_param = uec.map(|u| IsoParameter {
        value: u.margin,
        grade: grade_floor(u.margin, [0.62, 0.50, 0.37, 0.25]),
    });

    // ISO rule: overall = the LOWEST individual parameter grade
    // (IsoGrade Ord: A < B < … < F, so "lowest grade" = max in Ord terms)
    let overall = [
        symbol_contrast.grade,
        modulation.grade,
        anu.grade,
        fpd.grade,
    ]
    .into_iter()
    .chain(uec_param.map(|p| p.grade))
    .max()
    .unwrap_or(IsoGrade::F);

    Some(Iso15415Report {
        symbol_contrast,
        modulation,
        axial_nonuniformity: anu,
        fixed_pattern_damage: fpd,
        unused_error_correction: uec_param,
        overall,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    /// The module count drives the sample grid: `modules = version*4+17`
    /// (line 151). 151:38 mutates the `*` (`*`→`/` gives `version/4+17`,
    /// `*`→`+` gives `version+4+17`) and 151:42 the `+` (`+`→`*` gives
    /// `version*4*17`). A 5-px checkerboard sampled through `compute()` reads
    /// modulation D at the CORRECT version-1 grid (21 modules, modulation
    /// 0.255): the module centres sit inside cells, only a fixed boundary-cell
    /// fraction is mid-gray. Each mutated module count re-phases the grid and
    /// shifts that fraction, moving modulation off D:
    ///   *→/  → modules 17 → modulation A (0.937)
    ///   +→*  → modules 68 → modulation F (0.082)
    ///   *→+  → modules 22 → modulation F (0.004)
    /// Fully deterministic (no decode) — same image + corners ⇒ same grade.
    #[test]
    fn modules_formula_drives_the_sample_grid() {
        let side = 210u32;
        let data: Vec<u8> = (0..side * side)
            .map(|i| {
                let (x, y) = (i % side, i / side);
                if (x / 5 + y / 5) % 2 == 0 { 0 } else { 255 }
            })
            .collect();
        let img = LumaImage::new(data, side, side);
        let corners = [
            p(0.0, 0.0),
            p((side - 1) as f32, 0.0),
            p((side - 1) as f32, (side - 1) as f32),
            p(0.0, (side - 1) as f32),
        ];
        let structural = StructuralReport {
            finder_integrity: [1.0, 1.0, 1.0],
            quiet_zone_ok: true,
        };
        let report = compute(&img, corners, 1, &structural, None).expect("gradeable");
        assert_eq!(report.symbol_contrast.grade, IsoGrade::A, "{report:?}");
        assert_eq!(
            report.modulation.grade,
            IsoGrade::D,
            "correct modules=21 → modulation D; a mutated version*4+17 re-phases the grid: {:?}",
            report.modulation
        );
    }

    #[test]
    fn grade_bands_hit_their_exact_thresholds() {
        // floors (higher better) — SC bands
        let sc = [0.70, 0.55, 0.40, 0.20];
        assert_eq!(grade_floor(0.70, sc), IsoGrade::A);
        assert_eq!(grade_floor(0.699, sc), IsoGrade::B);
        assert_eq!(grade_floor(0.55, sc), IsoGrade::B);
        assert_eq!(grade_floor(0.40, sc), IsoGrade::C);
        assert_eq!(grade_floor(0.20, sc), IsoGrade::D);
        assert_eq!(grade_floor(0.199, sc), IsoGrade::F);
        // ceilings (lower better) — ANU bands
        let anu = [0.06, 0.08, 0.10, 0.12];
        assert_eq!(grade_ceiling(0.06, anu), IsoGrade::A);
        assert_eq!(grade_ceiling(0.061, anu), IsoGrade::B);
        assert_eq!(grade_ceiling(0.12, anu), IsoGrade::D);
        assert_eq!(grade_ceiling(0.121, anu), IsoGrade::F);
    }

    #[test]
    fn anu_square_is_a_and_two_to_one_is_f() {
        let square =
            axial_nonuniformity([p(0.0, 0.0), p(100.0, 0.0), p(100.0, 100.0), p(0.0, 100.0)]);
        assert_eq!(square.grade, IsoGrade::A);
        assert!(square.value < 0.001, "{}", square.value);

        // 200×100: |200−100|/150 = 0.666…
        let stretched =
            axial_nonuniformity([p(0.0, 0.0), p(200.0, 0.0), p(200.0, 100.0), p(0.0, 100.0)]);
        assert_eq!(stretched.grade, IsoGrade::F);
        assert!(
            (stretched.value - 2.0 / 3.0).abs() < 0.001,
            "{}",
            stretched.value
        );
    }

    #[test]
    fn contrast_and_modulation_on_a_clean_bimodal_distribution() {
        // half pure black, half pure white module means
        let samples: Vec<u8> = (0..200).map(|i| if i % 2 == 0 { 0 } else { 255 }).collect();
        let (sc, modulation) = contrast_and_modulation(&samples).unwrap();
        assert_eq!(sc.grade, IsoGrade::A);
        assert!((sc.value - 1.0).abs() < 0.01, "{}", sc.value);
        assert_eq!(modulation.grade, IsoGrade::A);
        assert!(
            (modulation.value - 1.0).abs() < 0.01,
            "{}",
            modulation.value
        );
    }

    #[test]
    fn weak_modulation_grades_down_even_with_decent_contrast() {
        // extremes far apart, but 10% of modules hug the threshold
        let mut samples: Vec<u8> = (0..180)
            .map(|i| if i % 2 == 0 { 10 } else { 245 })
            .collect();
        samples.extend(std::iter::repeat_n(128u8, 20));
        let (sc, modulation) = contrast_and_modulation(&samples).unwrap();
        assert_eq!(sc.grade, IsoGrade::A, "{sc:?}");
        // 5th percentile lands inside the mid-gray block → near-zero MOD
        assert_eq!(modulation.grade, IsoGrade::F, "{modulation:?}");
    }

    #[test]
    fn flat_image_is_double_f_not_a_panic() {
        let samples = vec![128u8; 64];
        let (sc, modulation) = contrast_and_modulation(&samples).unwrap();
        assert_eq!(sc.grade, IsoGrade::F);
        assert_eq!(modulation.grade, IsoGrade::F);
    }

    /// The percentile index is `round((len − 1) · p)`; a `-`→`/` swap (:54,
    /// `len / 1 == len`) shifts it up by one. With 196 low values then 4 high
    /// ones, the 98th percentile lands on index 195 (still low) — a one-index
    /// slip reads the outlier instead, flipping span 0 → 200 (contrast F → A).
    #[test]
    fn percentile_index_uses_len_minus_one() {
        let mut samples = vec![40u8; 196];
        samples.extend([240u8; 4]);
        // r_high = sorted[195] = 40, r_low = sorted[4] = 40 → span 0 → flat
        let (sc, modulation) = contrast_and_modulation(&samples).unwrap();
        assert_eq!(sc.grade, IsoGrade::F, "{sc:?}");
        assert_eq!(modulation.grade, IsoGrade::F, "{modulation:?}");
    }

    /// The sub-sample guard is `< 16` (strict): exactly 16 module means is
    /// enough to grade. A `<`→`<=` or `<`→`==` swap (:60) would refuse 16.
    #[test]
    fn sixteen_module_means_is_the_minimum_admitted() {
        let sixteen: Vec<u8> = (0..16).map(|i| if i % 2 == 0 { 0 } else { 255 }).collect();
        assert!(contrast_and_modulation(&sixteen).is_some());
        // 15 is genuinely too few → None (also traps the `<`→`==` swap, which
        // would otherwise let 15 samples through)
        let fifteen: Vec<u8> = (0..15).map(|i| if i % 2 == 0 { 0 } else { 255 }).collect();
        assert!(contrast_and_modulation(&fifteen).is_none());
    }

    /// The MODULATION 5th-percentile index is `round((mods.len() − 1) · 0.05)`
    /// (line 97 — a SEPARATE `len − 1` from the `percentile` helper at line 54).
    /// 97:39 mutates that `−`: `−`→`/` (`len / 1` == `len`) and `−`→`+`
    /// (`len + 1`). With 30 module means the original index is
    /// `round(29·0.05) = 1`; both mutants shift it to `round(30·0.05) = 2`
    /// (and `round(31·0.05) = 2`). Values engineered so mods\[1] is grade D and
    /// mods\[2] is grade B — the one-index slip flips the modulation grade.
    #[test]
    fn modulation_percentile_index_uses_len_minus_one() {
        // 14×black + 13×white keep span = 255 (contrast A, GT = 127.5). Three
        // mid-tones give the three smallest MODs, in distinct grade bands:
        //   103 → MOD 0.192 (F, index 0)   ← never selected
        //    95 → MOD 0.255 (D, index 1)   ← original 5th-percentile pick
        //    70 → MOD 0.451 (B, index 2)   ← mutant pick
        let mut samples = vec![0u8; 14];
        samples.extend(std::iter::repeat_n(255u8, 13));
        samples.extend([70u8, 95u8, 103u8]); // total = 30
        let (sc, modulation) = contrast_and_modulation(&samples).unwrap();
        assert_eq!(sc.grade, IsoGrade::A, "span 255 → contrast A: {sc:?}");
        assert_eq!(
            modulation.grade,
            IsoGrade::D,
            "5th-pct index 1 (mods[1]=0.255 → D), not index 2 (mods[2]=0.451 → B): {modulation:?}"
        );
    }

    #[test]
    fn fixed_pattern_damage_caps_at_d_on_quiet_zone_violation() {
        let pristine = StructuralReport {
            finder_integrity: [1.0, 1.0, 1.0],
            quiet_zone_ok: false,
        };
        let fpd = fixed_pattern_damage(&pristine);
        assert_eq!(fpd.grade, IsoGrade::D, "{fpd:?}");
        let damaged = StructuralReport {
            finder_integrity: [1.0, 0.82, 1.0],
            quiet_zone_ok: true,
        };
        assert_eq!(fixed_pattern_damage(&damaged).grade, IsoGrade::C);
    }
}
