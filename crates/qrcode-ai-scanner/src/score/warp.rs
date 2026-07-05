//! Geometry for stress + grid sampling — 3×3 homography, bilinear sampling,
//! perspective/rotation warps, local lighting defects.
//!
//! All deterministic f32 math. The same homography type serves the stress
//! axes and the structural module-grid sampling.

use crate::input::LumaImage;

/// Row-major 3×3 projective transform mapping source → destination.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Homography(pub [f32; 9]);

impl Homography {
    /// Identity transform (test geometry baseline).
    #[cfg(test)]
    pub(crate) fn identity() -> Self {
        Self([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0])
    }

    /// Apply to a point. Returns `None` when the point projects to infinity.
    pub(crate) fn apply(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let m = &self.0;
        let w = m[6] * x + m[7] * y + m[8];
        if w.abs() < f32::EPSILON {
            return None;
        }
        Some((
            (m[0] * x + m[1] * y + m[2]) / w,
            (m[3] * x + m[4] * y + m[5]) / w,
        ))
    }

    /// Inverse transform (adjugate / determinant). `None` when singular.
    pub(crate) fn inverse(&self) -> Option<Self> {
        let m = &self.0;
        let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6]);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        Some(Self([
            (m[4] * m[8] - m[5] * m[7]) * inv_det,
            (m[2] * m[7] - m[1] * m[8]) * inv_det,
            (m[1] * m[5] - m[2] * m[4]) * inv_det,
            (m[5] * m[6] - m[3] * m[8]) * inv_det,
            (m[0] * m[8] - m[2] * m[6]) * inv_det,
            (m[2] * m[3] - m[0] * m[5]) * inv_det,
            (m[3] * m[7] - m[4] * m[6]) * inv_det,
            (m[1] * m[6] - m[0] * m[7]) * inv_det,
            (m[0] * m[4] - m[1] * m[3]) * inv_det,
        ]))
    }

    /// Homography mapping the unit square (0,0)(1,0)(1,1)(0,1) onto a quad
    /// given clockwise from top-left. The grid-sampling workhorse: unit
    /// coordinates = module space, quad = detected symbol corners.
    pub(crate) fn unit_square_to_quad(quad: [(f32, f32); 4]) -> Option<Self> {
        let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = quad;
        let dx1 = x1 - x2;
        let dx2 = x3 - x2;
        let dy1 = y1 - y2;
        let dy2 = y3 - y2;
        let sx = x0 - x1 + x2 - x3;
        let sy = y0 - y1 + y2 - y3;
        let den = dx1 * dy2 - dx2 * dy1;
        if den.abs() < 1e-12 {
            return None;
        }
        let g = (sx * dy2 - dx2 * sy) / den;
        let h = (dx1 * sy - sx * dy1) / den;
        Some(Self([
            x1 - x0 + g * x1,
            x3 - x0 + h * x3,
            x0,
            y1 - y0 + g * y1,
            y3 - y0 + h * y3,
            y0,
            g,
            h,
            1.0,
        ]))
    }
}

/// Bilinear sample with white (255) outside — QR quiet-zone-like background.
#[expect(
    clippy::cast_possible_truncation,
    reason = "floor() output bounded by the f32 pixel-coordinate domain"
)]
pub(crate) fn sample_bilinear(img: &LumaImage, x: f32, y: f32) -> u8 {
    let (w, h) = (i64::from(img.width()), i64::from(img.height()));
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let (ix, iy) = (x0 as i64, y0 as i64);

    let fetch = |px: i64, py: i64| -> f32 {
        if px < 0 || py < 0 || px >= w || py >= h {
            255.0
        } else {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "bounds-checked above against u32-sized dimensions"
            )]
            let idx = (py as usize) * (img.width() as usize) + (px as usize);
            f32::from(img.data()[idx])
        }
    };

    let top = fetch(ix, iy) * (1.0 - fx) + fetch(ix + 1, iy) * fx;
    let bottom = fetch(ix, iy + 1) * (1.0 - fx) + fetch(ix + 1, iy + 1) * fx;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=255.0"
    )]
    {
        (top * (1.0 - fy) + bottom * fy).clamp(0.0, 255.0) as u8
    }
}

/// Warp an image through the inverse of `h_fwd` (destination ← source
/// lookup), bilinear, white background.
pub(crate) fn warp(img: &LumaImage, h_fwd: &Homography) -> LumaImage {
    let Some(h_inv) = h_fwd.inverse() else {
        return img.clone();
    };
    let (w, h) = (img.width(), img.height());
    let mut data = Vec::with_capacity((w * h) as usize);
    #[expect(
        clippy::cast_precision_loss,
        reason = "pixel coordinates bounded by Limits::max_dimension — exact in f32"
    )]
    for y in 0..h {
        for x in 0..w {
            let sampled = match h_inv.apply(x as f32, y as f32) {
                Some((sx, sy)) => sample_bilinear(img, sx, sy),
                None => 255,
            };
            data.push(sampled);
        }
    }
    LumaImage::new(data, w, h)
}

/// Perspective tilt: the top edge contracts by `sin(angle)·0.5` per side,
/// emulating a camera pitched off-axis. Deterministic stress geometry (not
/// exact camera optics — the grid-estimation margin is what it erodes).
pub(crate) fn perspective_tilt(img: &LumaImage, angle_deg: f32) -> LumaImage {
    #[expect(
        clippy::cast_precision_loss,
        reason = "dimensions bounded by Limits::max_dimension"
    )]
    let (w, h) = (img.width() as f32 - 1.0, img.height() as f32 - 1.0);
    let inset = w * (angle_deg.to_radians().sin() * 0.5);
    let quad = [(inset, 0.0), (w - inset, 0.0), (w, h), (0.0, h)];
    // unit→quad ∘ pixel→unit = source-rect → trapezoid forward map
    let Some(unit_to_quad) = Homography::unit_square_to_quad(quad) else {
        return img.clone();
    };
    let to_unit = Homography([1.0 / w, 0.0, 0.0, 0.0, 1.0 / h, 0.0, 0.0, 0.0, 1.0]);
    let fwd = compose(&unit_to_quad, &to_unit);
    warp(img, &fwd)
}

/// Rotation around the image center, white background.
pub(crate) fn rotate(img: &LumaImage, angle_deg: f32) -> LumaImage {
    #[expect(
        clippy::cast_precision_loss,
        reason = "dimensions bounded by Limits::max_dimension"
    )]
    // pixel-center convention: the grid rotates around ((w-1)/2, (h-1)/2)
    let (cx, cy) = (
        (img.width() as f32 - 1.0) / 2.0,
        (img.height() as f32 - 1.0) / 2.0,
    );
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    // forward: translate(-c) → rotate → translate(+c)
    let fwd = Homography([
        cos,
        -sin,
        cx - cos * cx + sin * cy,
        sin,
        cos,
        cy - sin * cx - cos * cy,
        0.0,
        0.0,
        1.0,
    ]);
    warp(img, &fwd)
}

/// Matrix product `a ∘ b` (apply `b` first).
pub(crate) fn compose(a: &Homography, b: &Homography) -> Homography {
    let (m, n) = (&a.0, &b.0);
    let mut out = [0.0f32; 9];
    for row in 0..3 {
        for col in 0..3 {
            out[row * 3 + col] =
                m[row * 3] * n[col] + m[row * 3 + 1] * n[3 + col] + m[row * 3 + 2] * n[6 + col];
        }
    }
    Homography(out)
}

/// Linear shadow: darkens from left (full strength) to right (none).
/// `strength` 0.0..=1.0 multiplies luma down to `1-strength` at the left edge.
pub(crate) fn shadow_gradient(img: &LumaImage, strength: f32) -> LumaImage {
    let w = img.width();
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bounded pixel math, result clamped"
    )]
    let data = img
        .data()
        .iter()
        .enumerate()
        .map(|(i, &px)| {
            let x = (i as u32 % w) as f32 / w.max(1) as f32;
            let factor = 1.0 - strength * (1.0 - x);
            (f32::from(px) * factor).clamp(0.0, 255.0) as u8
        })
        .collect();
    LumaImage::new(data, w, img.height())
}

/// Additive glare blob: a gaussian-falloff white spot at (cx, cy).
pub(crate) fn glare_blob(img: &LumaImage, cx: f32, cy: f32, radius: f32) -> LumaImage {
    let w = img.width();
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bounded pixel math, result clamped"
    )]
    let data = img
        .data()
        .iter()
        .enumerate()
        .map(|(i, &px)| {
            let x = (i as u32 % w) as f32;
            let y = (i as u32 / w) as f32;
            let d2 = (x - cx) * (x - cx) + (y - cy) * (y - cy);
            let boost = 255.0 * (-d2 / (radius * radius)).exp();
            (f32::from(px) + boost).clamp(0.0, 255.0) as u8
        })
        .collect();
    LumaImage::new(data, w, img.height())
}

/// Global exposure shift (positive = brighter), saturating.
pub(crate) fn exposure(img: &LumaImage, delta: i16) -> LumaImage {
    let data = img
        .data()
        .iter()
        .map(|&px| {
            let shifted = i16::from(px).saturating_add(delta).clamp(0, 255);
            #[expect(clippy::cast_sign_loss, reason = "clamped to 0..=255")]
            {
                shifted as u8
            }
        })
        .collect();
    LumaImage::new(data, img.width(), img.height())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_precision_loss,
        clippy::float_cmp
    )]

    use super::*;

    fn checker(side: u32) -> LumaImage {
        let data = (0..side * side)
            .map(|i| {
                let (x, y) = (i % side, i / side);
                if (x + y) % 2 == 0 { 0 } else { 255 }
            })
            .collect();
        LumaImage::new(data, side, side)
    }

    #[test]
    fn identity_warp_is_exact() {
        let img = checker(16);
        let out = warp(&img, &Homography::identity());
        assert_eq!(out.data(), img.data());
    }

    #[test]
    fn homography_inverse_roundtrips() {
        let h = Homography::unit_square_to_quad([(2.0, 1.0), (10.0, 0.5), (11.0, 9.0), (1.5, 8.0)])
            .unwrap();
        let inv = h.inverse().unwrap();
        for (x, y) in [(0.0, 0.0), (0.5, 0.5), (1.0, 0.0), (0.25, 0.75)] {
            let (fx, fy) = h.apply(x, y).unwrap();
            let (bx, by) = inv.apply(fx, fy).unwrap();
            assert!((bx - x).abs() < 1e-4 && (by - y).abs() < 1e-4, "({x},{y})");
        }
    }

    #[test]
    fn unit_square_maps_corners_onto_quad() {
        let quad = [(3.0, 2.0), (20.0, 3.0), (19.0, 22.0), (2.0, 20.0)];
        let h = Homography::unit_square_to_quad(quad).unwrap();
        let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        for (unit, expected) in corners.iter().zip(quad) {
            let (x, y) = h.apply(unit.0, unit.1).unwrap();
            assert!(
                (x - expected.0).abs() < 1e-3 && (y - expected.1).abs() < 1e-3,
                "unit {unit:?} → ({x},{y}) expected {expected:?}"
            );
        }
    }

    #[test]
    fn rotation_90_maps_top_left_to_top_right_region() {
        let mut data = vec![255u8; 9 * 9];
        data[0] = 0; // black px at (0,0)
        let img = LumaImage::new(data, 9, 9);
        let out = rotate(&img, 90.0);
        // (0,0) rotates around (4.5,4.5) to (9,0) — clamped sampling lands at (8,0)
        assert!(
            out.data()[8] < 128,
            "rotated corner dark: {:?}",
            &out.data()[..9]
        );
        assert!(out.data()[0] > 128, "origin now white");
    }

    #[test]
    fn perspective_tilt_zero_is_near_identity() {
        let img = checker(12);
        let out = perspective_tilt(&img, 0.0);
        // allow bilinear rounding at edges
        let diffs = img
            .data()
            .iter()
            .zip(out.data())
            .filter(|(a, b)| a != b)
            .count();
        assert!(diffs < 8, "near-identity expected, {diffs} diffs");
    }

    #[test]
    fn perspective_tilt_pulls_top_corners_inward() {
        // 6×6 black block at top-left (a lone pixel vanishes under the
        // horizontal compression of the top edge — block survives).
        let mut data = vec![255u8; 21 * 21];
        for y in 0..6 {
            for x in 0..6 {
                data[y * 21 + x] = 0;
            }
        }
        let img = LumaImage::new(data, 21, 21);
        let out = perspective_tilt(&img, 40.0);
        // origin now shows white background (top corners pulled inward)
        assert_eq!(out.data()[0], 255);
        // the dark mass survives, shifted right within the top rows
        assert!(out.data()[..42].iter().any(|&p| p < 100));
    }

    #[test]
    fn shadow_gradient_darkens_left_only() {
        let img = LumaImage::new(vec![200u8; 8], 8, 1);
        let out = shadow_gradient(&img, 0.5);
        assert!(out.data()[0] < 110, "left darkened: {}", out.data()[0]);
        assert!(
            out.data()[7] >= 185,
            "right nearly intact: {}",
            out.data()[7]
        );
        let mut prev = 0u8;
        for &px in out.data() {
            assert!(px >= prev, "monotonic left→right");
            prev = px;
        }
    }

    #[test]
    fn glare_blob_peaks_at_center() {
        let img = LumaImage::new(vec![60u8; 9 * 9], 9, 9);
        let out = glare_blob(&img, 4.0, 4.0, 2.0);
        let center = out.data()[4 * 9 + 4];
        let corner = out.data()[0];
        assert!(center > 200, "center blown out: {center}");
        assert!(corner < 80, "corner barely affected: {corner}");
    }

    #[test]
    fn exposure_saturates_both_ways() {
        let img = LumaImage::new(vec![10, 128, 250], 3, 1);
        assert_eq!(exposure(&img, 40).data(), &[50, 168, 255]);
        assert_eq!(exposure(&img, -40).data(), &[0, 88, 210]);
    }

    // ---- direct pins for `inverse` + `apply` -------------------------------
    // The roundtrip test above exercises `inverse` only through `apply`, which
    // is PROJECTIVELY SCALE-INVARIANT: multiplying the whole inverse matrix by
    // any nonzero constant leaves every `apply` result unchanged. So every
    // mutation of the determinant (a global scale on the adjugate) survived a
    // roundtrip test. These tests assert the matrix ELEMENTS directly, making
    // the exact determinant and adjugate load-bearing.

    #[test]
    fn inverse_matches_hand_computed_adjugate() {
        // Distinct primes in every slot — no entry is 0 or ±1, so every
        // sub-product/-difference in the determinant is discriminated (a `*`→`/`
        // or `*`→`+` swap always changes the value).
        let h = Homography([2.0, 3.0, 5.0, 7.0, 11.0, 13.0, 17.0, 19.0, 29.0]);
        // det = 2·(11·29 − 13·19) − 3·(7·29 − 13·17) + 5·(7·19 − 11·17)
        //     = 2·72 − 3·(−18) + 5·(−54) = 144 + 54 − 270 = −72
        // inverse = (adjugateᵀ) / det, adjugateᵀ = [72,8,−16,18,−27,9,−54,13,1]:
        let expected: [f32; 9] = [
            -1.0,         // 72  / −72
            -1.0 / 9.0,   // 8   / −72
            2.0 / 9.0,    // −16 / −72
            -0.25,        // 18  / −72
            0.375,        // −27 / −72
            -0.125,       // 9   / −72
            0.75,         // −54 / −72
            -13.0 / 72.0, // 13  / −72
            -1.0 / 72.0,  // 1   / −72
        ];
        let inv = h.inverse().expect("nonsingular");
        for (k, (&got, want)) in inv.0.iter().zip(expected).enumerate() {
            assert!(
                (got - want).abs() < 1e-4,
                "inv[{k}] = {got}, want {want} (a wrong det rescales EVERY element)"
            );
        }
    }

    #[test]
    fn inverse_of_singular_is_none() {
        // rows 1 and 2 are linearly dependent (row1 = 2·row0) → det = 0. The
        // guard `det.abs() < 1e-12` must refuse; a `<`→`==` swap would slip a
        // 1/0 = ∞ matrix through instead of returning None.
        let singular = Homography([1.0, 2.0, 3.0, 2.0, 4.0, 6.0, 1.0, 1.0, 1.0]);
        assert!(singular.inverse().is_none());
    }

    #[test]
    fn apply_projecting_to_infinity_returns_none() {
        // m[6]=1, m[7]=0, m[8]=0 ⇒ w = x. At x=0 the point projects to
        // infinity (w=0) → None; the `w.abs() < EPSILON` guard is what catches
        // it (a `<`→`==` swap would divide by zero instead).
        let h = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0]);
        assert!(h.apply(0.0, 5.0).is_none());
        // a finite point still maps: w = 2 → (2/2, 5/2)
        assert_eq!(h.apply(2.0, 5.0), Some((1.0, 2.5)));
    }

    #[test]
    fn unit_square_to_quad_projects_interior_points_exactly() {
        // A genuinely projective (non-affine) quad — exercises the perspective
        // divide, not just an affine scale. H = [2/3,0,0, 0,2/3,0, −1/3,−1/3,1],
        // so apply(u,v) divides by w = 1 − (u+v)/3.
        let quad = [(0.0, 0.0), (1.0, 0.0), (2.0, 2.0), (0.0, 1.0)];
        let h = Homography::unit_square_to_quad(quad).expect("nonsingular quad");
        let cases = [
            ((0.0, 0.0), (0.0, 0.0)), // corners
            ((1.0, 0.0), (1.0, 0.0)),
            ((1.0, 1.0), (2.0, 2.0)),
            ((0.0, 1.0), (0.0, 1.0)),
            ((0.5, 0.5), (0.5, 0.5)), // center: w=2/3
            ((0.5, 0.0), (0.4, 0.0)), // off-center: w=5/6 → (1/3)/(5/6)=0.4
            ((0.25, 0.25), (0.2, 0.2)),
        ];
        for ((u, v), (ex, ey)) in cases {
            let (x, y) = h.apply(u, v).expect("finite");
            assert!(
                (x - ex).abs() < 1e-5 && (y - ey).abs() < 1e-5,
                "({u},{v}) → ({x},{y}), want ({ex},{ey})"
            );
        }
    }

    #[test]
    fn bilinear_weights_average_the_checker() {
        // 2×2 checker: (0,0)=0 (1,0)=255 (0,1)=255 (1,1)=0
        let img = checker(2);
        // exact pixel centers read the pixel back
        assert_eq!(sample_bilinear(&img, 0.0, 0.0), 0);
        assert_eq!(sample_bilinear(&img, 1.0, 0.0), 255);
        // the shared midpoint averages all four: (0+255+255+0)/4 = 127.5 → 127
        assert_eq!(sample_bilinear(&img, 0.5, 0.5), 127);
        // quarter along the top row pins the fx weights: 0·0.75 + 255·0.25
        // = 63.75 → 63 (a +/- or */÷ weight swap misses this)
        assert_eq!(sample_bilinear(&img, 0.25, 0.0), 63);
    }
}
