//! Stress geometry + lighting defects — perspective/rotation warps and local
//! lighting transforms for the scoring stress axes.
//!
//! All deterministic f32 math, built on the shared perspective primitives
//! ([`Homography`], [`sample_bilinear`]) in `crate::matrix::sampler`.

use crate::input::LumaImage;
use crate::matrix::sampler::{Homography, sample_bilinear};

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
}
