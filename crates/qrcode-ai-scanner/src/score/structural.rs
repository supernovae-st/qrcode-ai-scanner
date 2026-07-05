//! Structural checks — finder-pattern integrity + quiet zone.
//!
//! Works in module space via [`GridSampler`] (`crate::matrix::sampler`): the
//! detection corners define a homography from the unit square onto the rqrr
//! bounds quad, and every check samples module centers through it. All
//! thresholds are local and deterministic.

use crate::input::LumaImage;
use crate::matrix::sampler::GridSampler;
use crate::report::{Point, StructuralReport};

/// The canonical 7×7 finder pattern (true = dark module).
const FINDER: [[bool; 7]; 7] = {
    let mut pattern = [[false; 7]; 7];
    let mut y = 0;
    while y < 7 {
        let mut x = 0;
        while x < 7 {
            let ring = y == 0 || y == 6 || x == 0 || x == 6;
            let core = y >= 2 && y <= 4 && x >= 2 && x <= 4;
            pattern[y][x] = ring || core;
            x += 1;
        }
        y += 1;
    }
    pattern
};

/// Score one finder region: fraction of the 49 modules matching the canonical
/// pattern under a LOCAL threshold (mean of the region's samples — robust to
/// global lighting, deterministic).
fn finder_score(sampler: &GridSampler<'_>, origin_x: i32, origin_y: i32) -> f32 {
    let mut region = [[0u8; 7]; 7];
    let mut sum = 0u32;
    for (dy, row) in region.iter_mut().enumerate() {
        for (dx, cell) in row.iter_mut().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_possible_wrap,
                reason = "dx/dy ∈ 0..7"
            )]
            let value = sampler
                .module(origin_x + dx as i32, origin_y + dy as i32)
                .unwrap_or(255);
            *cell = value;
            sum += u32::from(value);
        }
    }
    let threshold = sum / 49;
    let mut matching = 0u32;
    for (dy, row) in region.iter().enumerate() {
        for (dx, &value) in row.iter().enumerate() {
            let observed_dark = u32::from(value) < threshold;
            if observed_dark == FINDER[dy][dx] {
                matching += 1;
            }
        }
    }
    #[expect(clippy::cast_precision_loss, reason = "0..=49")]
    {
        matching as f32 / 49.0
    }
}

/// Fraction of quiet-zone probes that read "light" relative to the symbol's
/// own dark/light split (mean of the three finder regions' means is too
/// entangled — we reuse the simple global mean of probed border samples vs
/// the symbol interior mean).
fn quiet_zone_ok(sampler: &GridSampler<'_>, modules: i32) -> bool {
    // interior reference: mean over a coarse grid of the symbol
    let mut interior_sum = 0u32;
    let mut interior_n = 0u32;
    let step = (modules / 8).max(1);
    let mut my = 0;
    while my < modules {
        let mut mx = 0;
        while mx < modules {
            if let Some(v) = sampler.module(mx, my) {
                interior_sum += u32::from(v);
                interior_n += 1;
            }
            mx += step;
        }
        my += step;
    }
    if interior_n == 0 {
        return false;
    }
    let interior_mean = interior_sum / interior_n;

    // probe a 2-module ring outside each edge
    let mut light = 0u32;
    let mut total = 0u32;
    for offset in [-2i32, -1] {
        for m in -2..=(modules + 1) {
            for (mx, my) in [
                (m, offset),               // top
                (m, modules - 1 - offset), // bottom (offset mirrored out)
                (offset, m),               // left
                (modules - 1 - offset, m), // right
            ] {
                if let Some(v) = sampler.module(mx, my) {
                    total += 1;
                    if u32::from(v) > interior_mean {
                        light += 1;
                    }
                }
            }
        }
    }
    total > 0 && light * 100 >= total * 80
}

/// Run the structural checks. `None` when geometry is unusable.
pub(crate) fn check(img: &LumaImage, corners: [Point; 4], version: u8) -> Option<StructuralReport> {
    let modules = u32::from(version) * 4 + 17;
    let sampler = GridSampler::new(img, corners, modules)?;
    #[expect(clippy::cast_possible_wrap, reason = "modules ≤ 177")]
    let n = modules as i32;

    let finder_integrity = [
        finder_score(&sampler, 0, 0),     // top-left
        finder_score(&sampler, n - 7, 0), // top-right
        finder_score(&sampler, 0, n - 7), // bottom-left
    ];
    Some(StructuralReport {
        finder_integrity,
        quiet_zone_ok: quiet_zone_ok(&sampler, n),
    })
}

#[cfg(test)]
pub(super) mod tests_support {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::ladder::{self, CancelToken, ScanConfig};
    use crate::transform::normalize;

    /// Generate, decode via the ladder (real rqrr corners), return everything.
    pub(super) fn decoded_clean_for_probe() -> (LumaImage, [Point; 4], u8) {
        let code =
            qrcode::QrCode::with_error_correction_level(b"structural pin", qrcode::EcLevel::Q)
                .unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(8, 8)
            .build();
        let mut buf = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        let planes = normalize(&ImageInput::encoded(&buf), &Limits::default()).unwrap();
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
        let detection = &outcome.merged[0];
        (
            planes.luma.clone(),
            detection.corners.unwrap(),
            detection.version.unwrap(),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use super::tests_support::decoded_clean_for_probe as decoded_clean;
    use super::*;
    use crate::input::LumaImage;

    #[test]
    fn clean_qr_has_intact_finders_and_quiet_zone() {
        let (luma, corners, version) = decoded_clean();
        let report = check(&luma, corners, version).unwrap();
        for (i, integrity) in report.finder_integrity.iter().enumerate() {
            assert!(*integrity > 0.85, "finder {i}: {integrity}");
        }
        assert!(report.quiet_zone_ok);
    }

    #[test]
    fn painted_over_finder_scores_low_on_that_corner_only() {
        let (luma, corners, version) = decoded_clean();
        // paint the TOP-LEFT finder region white in image space
        let modules = u32::from(version) * 4 + 17;
        let mut data = luma.data().to_vec();
        // wash an image-space rect covering the TL finder (module 0..7)
        let (x0, y0) = (corners[0].x, corners[0].y);
        let module_px = (corners[1].x - corners[0].x) / modules as f32;
        let span = (module_px * 7.5) as u32;
        for y in y0 as u32..(y0 as u32 + span).min(luma.height()) {
            for x in x0 as u32..(x0 as u32 + span).min(luma.width()) {
                data[(y * luma.width() + x) as usize] = 255;
            }
        }
        let damaged = LumaImage::new(data, luma.width(), luma.height());
        let report = check(&damaged, corners, version).unwrap();
        assert!(
            report.finder_integrity[0] < 0.6,
            "TL damaged: {:?}",
            report.finder_integrity
        );
        assert!(
            report.finder_integrity[1] > 0.85 && report.finder_integrity[2] > 0.85,
            "TR/BL intact: {:?}",
            report.finder_integrity
        );
    }

    /// The probe ring is EXACTLY 2 modules past each edge — ink in the ring
    /// trips, ink one module beyond it must NOT (pins the probe coordinates
    /// against off-by-N mutations).
    #[test]
    fn quiet_zone_probe_ring_geometry() {
        let (luma, corners, version) = decoded_clean();
        let modules = f32::from(version) * 4.0 + 17.0;
        let module_px = (corners[1].x - corners[0].x) / (modules + 1.0);
        let w = luma.width();

        let paint_bottom_band = |from_module: f32, to_module: f32| -> LumaImage {
            let mut data = luma.data().to_vec();
            let y0 = (corners[0].y + from_module * module_px).max(0.0) as u32;
            let y1 = ((corners[0].y + to_module * module_px) as u32).min(luma.height());
            let x0 = corners[0].x as u32;
            let x1 = (corners[1].x as u32).min(w);
            for y in y0..y1 {
                for x in x0..x1 {
                    data[(y * w + x) as usize] = 0;
                }
            }
            LumaImage::new(data, w, luma.height())
        };

        // band over the probe ring (modules .. modules+2) → must trip
        let inked_ring = paint_bottom_band(modules, modules + 2.0);
        let report = check(&inked_ring, corners, version).unwrap();
        assert!(!report.quiet_zone_ok, "ink IN the 2-module ring must trip");

        // band beyond the ring (modules+2.5 .. modules+3.5) → must NOT trip
        let inked_far = paint_bottom_band(modules + 2.5, modules + 3.5);
        let report = check(&inked_far, corners, version).unwrap();
        assert!(
            report.quiet_zone_ok,
            "ink BEYOND the probe ring must not trip (probe coordinates drifted)"
        );
    }

    /// Synthetic samplers pin the interior-mean arithmetic of `quiet_zone_ok`
    /// that a real high-contrast QR leaves robust (its 255-vs-0 probes sit far
    /// from the interior mean). The affine quad (25,25)-(115,115) at modules=8
    /// lands every module center on an exact integer pixel (30 + 10·k), so the
    /// bilinear sampler reads the pixel back verbatim.
    #[test]
    fn quiet_zone_interior_mean_is_load_bearing() {
        let corners = [
            crate::report::Point { x: 25.0, y: 25.0 },
            crate::report::Point { x: 115.0, y: 25.0 },
            crate::report::Point { x: 115.0, y: 115.0 },
            crate::report::Point { x: 25.0, y: 115.0 },
        ];

        // (A) a UNIFORM image: no probe is brighter than the interior mean, so
        // the quiet zone is NOT ok. `interior_sum *= v` (:114) and `sum % n`
        // (:124) both collapse the mean to 0 (every probe then reads "light"),
        // and `v >= mean` (:139) lets the equal probes count — all three would
        // wrongly pass.
        let uniform = LumaImage::new(vec![90u8; 130 * 130], 130, 130);
        let sampler = GridSampler::new(&uniform, corners, 8).unwrap();
        assert!(
            !quiet_zone_ok(&sampler, 8),
            "a uniform image has no distinguishable quiet zone"
        );

        // (B) interior 95 with a single dark module (0,0)=0, quiet ring 90.
        // Correct mean = 5985/64 = 93, so the 90-probes read dark → NOT ok.
        // `step = modules*8` (:108) samples only module (0,0)=0 → mean 0 →
        // every probe flips to "light" → would wrongly pass.
        let mut data = vec![90u8; 130 * 130];
        for y in 25..115 {
            for x in 25..115 {
                data[y * 130 + x] = 95;
            }
        }
        data[30 * 130 + 30] = 0; // module (0,0) center pixel
        let img_b = LumaImage::new(data, 130, 130);
        let sampler_b = GridSampler::new(&img_b, corners, 8).unwrap();
        assert!(
            !quiet_zone_ok(&sampler_b, 8),
            "a dark interior corner must not invert the quiet-zone verdict"
        );
    }

    /// A mid-gray finder region pins `finder_score`'s local threshold as the
    /// MEAN of the 49 samples (`sum / 49`, :84). Real QR fixtures are 0/255
    /// polarized, so both `sum / 49` (≈83) and the harvest survivor `sum % 49`
    /// (≈13) classify them identically — here the two values straddle the
    /// classes. Same exact-integer affine quad as the quiet-zone pin: modules
    /// = 8 puts module (mx, my) center on pixel (30+10mx, 30+10my) verbatim.
    /// FINDER-dark cells read 90, light cells 110 → sum = 33·90 + 16·110 =
    /// 4730 → threshold 96 splits them (90 dark · 110 light) → score 1.0.
    /// The mutant threshold 4730 % 49 = 26 reads EVERYTHING light → 16/49.
    #[test]
    fn finder_score_threshold_is_the_region_mean() {
        let corners = [
            crate::report::Point { x: 25.0, y: 25.0 },
            crate::report::Point { x: 115.0, y: 25.0 },
            crate::report::Point { x: 115.0, y: 115.0 },
            crate::report::Point { x: 25.0, y: 115.0 },
        ];
        let mut data = vec![110u8; 130 * 130];
        for (dy, row) in FINDER.iter().enumerate() {
            for (dx, &dark) in row.iter().enumerate() {
                if dark {
                    data[(30 + 10 * dy) * 130 + (30 + 10 * dx)] = 90;
                }
            }
        }
        let img = LumaImage::new(data, 130, 130);
        let sampler = GridSampler::new(&img, corners, 8).unwrap();
        let score = finder_score(&sampler, 0, 0);
        assert!(
            (score - 1.0).abs() < f32::EPSILON,
            "mean threshold 96 must split 90-dark from 110-light: {score}"
        );
    }

    #[test]
    fn ink_in_the_margin_breaks_quiet_zone() {
        let (luma, corners, version) = decoded_clean();
        let mut data = luma.data().to_vec();
        // paint black columns immediately left of the symbol (the margin)
        let x_start = (corners[0].x
            - (corners[1].x - corners[0].x) / f32::from(version * 4 + 17) * 3.0)
            .max(0.0) as u32;
        let x_end = corners[0].x.max(0.0) as u32;
        for y in corners[0].y as u32..corners[3].y as u32 {
            for x in x_start..x_end {
                data[(y * luma.width() + x) as usize] = 0;
            }
        }
        let inked = LumaImage::new(data, luma.width(), luma.height());
        let report = check(&inked, corners, version).unwrap();
        assert!(!report.quiet_zone_ok, "margin ink must trip the check");
    }
}
