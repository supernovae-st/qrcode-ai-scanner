//! Stress geometry + lighting defects — perspective/rotation warps and local
//! lighting transforms for the scoring stress axes.
//!
//! All deterministic f32 math, built on the shared perspective primitives
//! ([`Homography`], [`sample_bilinear`]) in `crate::matrix::sampler`.

use crate::input::LumaImage;
use crate::matrix::sampler::{Homography, sample_bilinear};

/// Warp `img` through the inverse of `h_fwd` into an `(out_w, out_h)` canvas
/// (destination ← source lookup), bilinear, white background. Lookups more
/// than half a pixel outside the source rect paint background: affine maps
/// never hit the projective-horizon `None` arm, so without this guard a
/// rotation's out-of-frame corners clamp-streak the source edges instead of
/// showing the white a real backdrop provides.
pub(crate) fn warp_into(img: &LumaImage, h_fwd: &Homography, out_w: u32, out_h: u32) -> LumaImage {
    let Some(h_inv) = h_fwd.inverse() else {
        return img.clone();
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "pixel coordinates bounded by Limits::max_dimension — exact in f32"
    )]
    let (sw, sh) = (img.width() as f32 - 1.0, img.height() as f32 - 1.0);
    let mut data = Vec::with_capacity((out_w * out_h) as usize);
    #[expect(
        clippy::cast_precision_loss,
        reason = "pixel coordinates bounded by Limits::max_dimension — exact in f32"
    )]
    for y in 0..out_h {
        for x in 0..out_w {
            let sampled = match h_inv.apply(x as f32, y as f32) {
                Some((sx, sy)) if sx >= -0.5 && sx <= sw + 0.5 && sy >= -0.5 && sy <= sh + 0.5 => {
                    sample_bilinear(img, sx, sy)
                }
                _ => 255,
            };
            data.push(sampled);
        }
    }
    LumaImage::new(data, out_w, out_h)
}

/// Warp within the SAME canvas — for maps that keep content inside the
/// frame (perspective tilt contracts inward; identity).
pub(crate) fn warp(img: &LumaImage, h_fwd: &Homography) -> LumaImage {
    warp_into(img, h_fwd, img.width(), img.height())
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

/// Rotation around the image centre INTO the rotated bounding box, white
/// background. The canvas GROWS (`w·|cos| + h·|sin|` per side): rotating
/// within the source frame amputates the corners — a v10's corner radius
/// (≈0.62·w against a 0.5·w half-frame) leaves a square frame by the second
/// 10° ramp step — and the rotation axis then measures frame cropping
/// instead of engine tolerance. A camera reframes; it never crops the
/// subject out of existence.
pub(crate) fn rotate(img: &LumaImage, angle_deg: f32) -> LumaImage {
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "dimensions bounded by Limits::max_dimension; ceil of a non-negative bound"
    )]
    {
        let (w, h) = (img.width() as f32, img.height() as f32);
        let (sin, cos) = angle_deg.to_radians().sin_cos();
        // Bounding box in f64 with a snap-epsilon before ceil: cardinal
        // angles are not exact in floating trig (f32 cos 90° ≈ −4.4e-8,
        // f64 sin 180° ≈ 1.2e-16) and would bump a pure shape swap one
        // pixel up. 1e-6 dwarfs the trig noise at any Limits-legal size
        // while no non-cardinal angle can land that close to an integer.
        let (sin64, cos64) = f64::from(angle_deg).to_radians().sin_cos();
        let (w64, h64) = (f64::from(img.width()), f64::from(img.height()));
        let out_w = (w64 * cos64.abs() + h64 * sin64.abs() - 1e-6).ceil() as u32;
        let out_h = (w64 * sin64.abs() + h64 * cos64.abs() - 1e-6).ceil() as u32;
        // pixel-centre convention: the source grid spins around
        // ((w-1)/2, (h-1)/2) and lands centred on ((out-1)/2, (out-1)/2).
        let (cx, cy) = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
        let (dx, dy) = ((out_w as f32 - 1.0) / 2.0, (out_h as f32 - 1.0) / 2.0);
        // forward: translate(-c_src) → rotate → translate(+c_dst)
        let fwd = Homography([
            cos,
            -sin,
            dx - cos * cx + sin * cy,
            sin,
            cos,
            dy - sin * cx - cos * cy,
            0.0,
            0.0,
            1.0,
        ]);
        warp_into(img, &fwd, out_w, out_h)
    }
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
        // pixel-centre convention: (0,0) rotates around (4,4) exactly onto
        // (8,0) — the w−1 edge, in-bounds, no clamping involved
        assert!(
            out.data()[8] < 128,
            "rotated corner dark: {:?}",
            &out.data()[..9]
        );
        assert!(out.data()[0] > 128, "origin now white");
    }

    #[test]
    fn rotate_expands_the_canvas_to_the_rotated_bounding_box() {
        // Same-canvas rotation amputates whatever lives near the corners —
        // a camera reframes, it never crops the subject out of existence.
        // The canvas must grow to the rotated bounding box:
        // 15·(cos40° + sin40°) = 15·1.40883… ≈ 21.13 → 22.
        let img = checker(15);
        let out = rotate(&img, 40.0);
        assert_eq!((out.width(), out.height()), (22, 22));
        // Cardinal 90° of a square is a pure shape swap — no growth.
        let out90 = rotate(&img, 90.0);
        assert_eq!((out90.width(), out90.height()), (15, 15));
        // Non-square discriminates w·|cos|+h·|sin| from its swapped twin:
        // 15×9 @ 40° → (ceil(11.49+5.79), ceil(9.64+6.89)) = (18, 17).
        let rect = LumaImage::new(vec![255u8; 15 * 9], 15, 9);
        let out_r = rotate(&rect, 40.0);
        assert_eq!((out_r.width(), out_r.height()), (18, 17));
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

    // ---- mutant pins for the stress synthesizers -----------------------------
    // The homography roundtrip / grid-sampling tests moved to matrix::sampler.
    // These five exercise the warp *builders* (perspective_tilt · rotate ·
    // compose · shadow_gradient · glare_blob) with hand-computed geometry so
    // every arithmetic operator in them is load-bearing.

    #[test]
    fn compose_is_exact_matrix_product() {
        // `compose(a, identity)` must reproduce `a` bit-for-bit: the left
        // operand is read as m[row*3 + k], k∈{0,1,2}. Distinct primes make
        // every slot discriminated. Traps `row * 3` → `row / 3` at 88:45
        // (the m[row*3+1] term) and 88:75 (the m[row*3+2] term): `row / 3`
        // collapses to 0 for rows 0..2, so rows 1 and 2 would misread
        // m[1],m[2] instead of m[4],m[5],m[7],m[8].
        let a = Homography([2.0, 3.0, 5.0, 7.0, 11.0, 13.0, 17.0, 19.0, 23.0]);
        let out = compose(&a, &Homography::identity());
        assert_eq!(out.0, a.0, "compose ∘ identity is a no-op on `a`");
    }

    #[test]
    fn shadow_gradient_ramp_is_normalized_column() {
        // strength = 1.0 ⇒ factor = 1 − (1 − x) = x = column / width. A 4-wide
        // row of 240 must ramp 240·{0, ¼, ½, ¾} = [0, 60, 120, 180] EXACTLY
        // (rational, no rounding). Traps 109:43 both ways:
        //   `/` → `*` : column·width blows factor up → clamps to 255.
        //   `/` → `%` : column % width == column (un-normalized) → [0,240,255,255].
        let img = LumaImage::new(vec![240u8; 4], 4, 1);
        let out = shadow_gradient(&img, 1.0);
        assert_eq!(out.data(), &[0, 60, 120, 180]);
    }

    #[test]
    fn glare_blob_distance_metric_is_pinned() {
        // 5×3 black canvas, unit-radius blob centred on pixel (col 3, row 1).
        // boost = 255·exp(−d²/r²); d² = (x−cx)² + (y−cy)² with x = i%w, y = i/w.
        let img = LumaImage::new(vec![0u8; 5 * 3], 5, 3);
        let out = glare_blob(&img, 3.0, 1.0, 1.0);
        let px = |c: usize, r: usize| out.data()[r * 5 + c];
        // The centre is the unique d²=0 peak → boost 255 exactly.
        //   131:31 `%`→`/`: x becomes the ROW (i/w) → centre d²≠0 → below 255.
        //   132:31 `/`→`%`: y becomes the COLUMN (i%w) → centre d²≠0 → below 255.
        assert_eq!(px(3, 1), 255, "blob centre peaks at 255");
        // (col 0,row 1): d²=9 → ~0. Under (x−cx)→(x/cx) the x-term is
        // (0/3)·(0−3)=0 ⇒ d²=0 ⇒ 255. Traps 133:25 and 133:36 (identical value).
        assert!(px(0, 1) < 30, "far pixel stays dark: {}", px(0, 1));
        // (col 3,row 0): on the centre column, d²=1. Under `+`→`*` (133:42)
        // d²=(Δx)²·(Δy)²=0·1=0 ⇒ 255. Traps 133:42.
        assert!(px(3, 0) < 150, "above-centre not blown out: {}", px(3, 0));
        // (col 3,row 2): centre column below, d²=1. Under (y−cy)→(y/cy) (133:47)
        // d²=(2/1)·(2−1)=2 ⇒ dimmer than d²=1. Traps 133:47.
        assert!(px(3, 2) > 60, "below-centre stays lit: {}", px(3, 2));
    }

    #[test]
    fn rotate_matrix_terms_are_pinned() {
        // 15×15 white with a solid 7×7 black block at [2..=8]². A 40° rotation
        // (cos and sin both large — unlike the 90° test where cos≈0 hides the
        // cos-terms) lands in a ceil(15·(cos40°+sin40°)) = 22×22 canvas:
        // src centre (7,7) → dst (10.5,10.5), block centre (5,5) → ≈(10.25,7.68).
        // Hand-derived: cos40 = 0.766, sin40 = 0.643, tx = dx−cos·cx+sin·cy
        // = 10.5−5.362+4.500 = 9.638, ty = dy−sin·cx−cos·cy = 0.638.
        let mut data = vec![255u8; 15 * 15];
        for y in 2..9 {
            for x in 2..9 {
                data[y * 15 + x] = 0;
            }
        }
        let img = LumaImage::new(data, 15, 15);
        let out = rotate(&img, 40.0);
        assert_eq!((out.width(), out.height()), (22, 22));
        let px = |c: usize, r: usize| out.data()[r * 22 + c];
        // Deep interior of the rotated block. Every tx/ty sign-or-factor slip —
        // and the −sin→sin flip — displaces the whole block off these cells,
        // leaving white background.
        assert!(px(10, 7) < 64, "block interior dark: {}", px(10, 7));
        assert!(px(10, 8) < 64, "block interior dark: {}", px(10, 8));
        // Background cells each mutant variant drags the block INTO (true
        // geometry keeps every one of them ≥1.5px outside the block):
        //   −sin → sin (the m[1] flip): block centre x 10.25 → 16.68.
        assert!(
            px(17, 8) > 200,
            "sin-flip cell stays background: {}",
            px(17, 8)
        );
        //   tx: dx − cos·cx → dx + cos·cx: centre x → 20.97.
        assert!(
            px(21, 8) > 200,
            "tx-sign cell stays background: {}",
            px(21, 8)
        );
        //   tx: cos·cx → cos/cx: centre x → 15.51.
        assert!(
            px(16, 8) > 200,
            "tx-ratio cell stays background: {}",
            px(16, 8)
        );
        //   ty: dy − sin·cx → dy + sin·cx: centre y 7.68 → 16.68.
        assert!(
            px(10, 17) > 200,
            "ty-sign cell stays background: {}",
            px(10, 17)
        );
        //   ty: cos·cy → cos/cy: centre y → 12.94.
        assert!(
            px(10, 13) > 200,
            "ty-ratio cell stays background: {}",
            px(10, 13)
        );
    }

    #[test]
    fn perspective_tilt_geometry_is_pinned() {
        // 31×31 all-black canvas, exaggerated 72° tilt so each ±1 / ×-slip in
        // the trapezoid parameters crosses a whole pixel boundary. Every
        // assertion below is a clean 0↔255 flip whose geometric boundary sits
        // ~0.25px from the pixel centre — orders of magnitude above f32
        // sin-rounding noise, so it holds identically across platforms.
        // The tilt contracts the black top edge inward (span x≈[14.3, 15.7]).
        let img = LumaImage::new(vec![0u8; 31 * 31], 31, 31);
        let out = perspective_tilt(&img, 72.0);
        let px = |c: usize, r: usize| out.data()[r * 31 + c];
        // (16,0) is just past the top-edge right boundary → background.
        //   42:38 `-`→`/`  makes w = width (+1) → inset grows, edge passes 16 → black.
        assert!(px(16, 0) > 128, "top-right boundary bg: {}", px(16, 0));
        // (15,0) is just inside the black top edge.
        //   42:38 `-`→`+`  makes w = width+1 (+2) → edge slides off 15 → white.
        assert!(px(15, 0) < 128, "top-edge interior black: {}", px(15, 0));
        // (0,0) corner is pulled to background by the inset.
        //   43:51 `*`→`/` (inset = w·sin·2) and `*`→`+` (inset = w·(sin+0.5))
        //   both blow the inset past w → the quad inverts → corner turns black.
        assert!(px(0, 0) > 128, "inset-vacated corner bg: {}", px(0, 0));
        // Vertical map depends on h = height-1.
        //   42:65 `-`→`/`  makes h = height (+1) → bottom-left (0,30) lightens.
        assert!(px(0, 30) < 128, "bottom-left stays black: {}", px(0, 30));
        //   42:65 `-`→`+`  makes h = height+1 (+2) → interior (5,20) lightens.
        assert!(px(5, 20) < 128, "interior stays black: {}", px(5, 20));
        // The true top edge only spans x∈[≈14.3,15.7]; (20,0) is far background.
        //   44:34 `-`→`+`  sends the top-right corner to w+inset≈44 → (20,0) black.
        assert!(px(20, 0) > 128, "far top background: {}", px(20, 0));
    }
}
