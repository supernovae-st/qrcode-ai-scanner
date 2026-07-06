//! Perspective geometry + the module-space grid sampler.
//!
//! A 3×3 projective homography maps the unit square (module space) onto the
//! detected symbol quad; `sample_bilinear` reads the luma under it. The
//! [`GridSampler`] wraps the two into "sample the center of module (mx, my)" —
//! the workhorse the structural checks, the ISO grade card, and the artistic
//! rescue confidence pass all sample through.
//!
//! Bounds convention (quirc heritage, source-verified rqrr 0.10.1
//! `prepare.rs:243`): the detection bounds span `grid_size + 1` grid cells, so
//! module (mx, my) of an N-wide symbol has its center at unit
//! `((mx+0.5)/(N+1), (my+0.5)/(N+1))`. All deterministic f32 math.

use crate::input::LumaImage;
use crate::report::Point;

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

/// Module-space sampler over a detected symbol.
pub(crate) struct GridSampler<'a> {
    img: &'a LumaImage,
    unit_to_image: Homography,
    modules: u32,
}

impl<'a> GridSampler<'a> {
    /// Build from detection corners (clockwise from top-left) + module count.
    pub(crate) fn new(img: &'a LumaImage, corners: [Point; 4], modules: u32) -> Option<Self> {
        let quad = corners.map(|p| (p.x, p.y));
        Some(Self {
            img,
            unit_to_image: Homography::unit_square_to_quad(quad)?,
            modules,
        })
    }

    /// Sample the center of module (mx, my). Off-grid coordinates are legal
    /// (negative / ≥ modules) — they probe the quiet zone.
    #[expect(
        clippy::cast_precision_loss,
        reason = "module indices ≤ 177, exact in f32"
    )]
    pub(crate) fn module(&self, mx: i32, my: i32) -> Option<u8> {
        // rqrr bounds cover N+1 grid cells (quirc heritage) — see module docs.
        let denom = self.modules as f32 + 1.0;
        let (ux, uy) = ((mx as f32 + 0.5) / denom, (my as f32 + 0.5) / denom);
        let (x, y) = self.unit_to_image.apply(ux, uy)?;
        Some(sample_bilinear(self.img, x, y))
    }
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

    // ---- exact-boundary pins for the three float-epsilon guards -------------
    // These guards are STRICT `<`, so they only differ from `<=`/`==` when the
    // magnitude equals the bound EXACTLY. f32 arithmetic is exact on small
    // integers and powers of two, so an EPSILON / 1e-12 magnitude IS
    // constructible (a diagonal product X·1·1 leaves X untouched).

    #[test]
    fn apply_at_exactly_epsilon_w_still_projects() {
        // sampler 32:20 `w.abs() < f32::EPSILON` — w = m6·x + m7·y + m8. With
        // m6=m7=0 and m8=EPSILON, w == f32::EPSILON EXACTLY for any (x,y). The
        // strict `<` treats this as finite (projects); a `<`→`<=` swap returns
        // None (divide-guard fires).
        let h = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, f32::EPSILON]);
        let got = h.apply(2.0, 4.0);
        assert!(got.is_some(), "w == EPSILON is finite under strict `<`");
        assert_eq!(got.unwrap(), (2.0 / f32::EPSILON, 4.0 / f32::EPSILON));
    }

    #[test]
    fn inverse_at_exactly_the_determinant_threshold_still_inverts() {
        // sampler 46:22 `det.abs() < 1e-12` — det = X·(1·1) with X = 1e-12_f32
        // (same literal the guard uses), so det == 1e-12 EXACTLY. Strict `<`
        // keeps it non-singular; `<`→`<=` would reject it as singular (None).
        let h = Homography([1e-12, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        assert!(
            h.inverse().is_some(),
            "det == 1e-12 is non-singular under the strict `<` guard"
        );
    }

    #[test]
    fn unit_square_to_quad_at_exactly_the_den_threshold_still_builds() {
        // sampler 75:22 `den.abs() < 1e-12`, two mutants: `<`→`<=` and `<`→`==`.
        // den = dx1·dy2 − dx2·dy1. With x2=y2=0, x1=1e-12,y1=0, x3=0,y3=1:
        // dx1=1e-12, dy2=1, dx2=0, dy1=0 → den == 1e-12 EXACTLY. Strict `<` is
        // false → Some; BOTH `<=` (true) and `==` (true) return None.
        let quad = [(1.0, 1.0), (1e-12, 0.0), (0.0, 0.0), (0.0, 1.0)];
        assert!(
            Homography::unit_square_to_quad(quad).is_some(),
            "den == 1e-12 is non-singular under strict `<`"
        );
        // control: a genuinely singular (collinear) quad still returns None —
        // this also traps `<`→`==` from the other side (0 != 1e-12).
        let collinear = [(0.0, 0.0), (2.0, 0.0), (4.0, 0.0), (6.0, 0.0)];
        assert!(Homography::unit_square_to_quad(collinear).is_none());
    }

    #[test]
    fn bilinear_bottom_row_read_and_weight_are_load_bearing() {
        // sampler line 122 (the `bottom` term). All four values distinct and
        // ≠ 255, so the bottom-row read is discriminated (the checker test hid
        // it: fetch(0,1)==fetch(0,-1)==255 by coincidence).
        //   (0,0)=10 (1,0)=20 / (0,1)=30 (1,1)=40
        let img = LumaImage::new(vec![10, 20, 30, 40], 2, 2);
        // center = mean of the four = 25 (first-principles bilinear midpoint).
        //   122:31 `fetch(ix, iy+1)`→`fetch(ix, iy-1)` reads OOB(255) → 81.
        //   122:73 `fetch(ix+1, iy+1) * fx`→`+ fx` → 35.
        assert_eq!(sample_bilinear(&img, 0.5, 0.5), 25);
        // three-quarters down the left column pins fy on the bottom term:
        // top(10)·0.25 + bottom(30)·0.75 = 25 (mutants → 193 / 55).
        assert_eq!(sample_bilinear(&img, 0.0, 0.75), 25);
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
}
