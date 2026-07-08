//! Decode-under-rotation truth on the real external corpus.
//!
//! `corpus-report --external` (and the manifest pins, and the README
//! headline) measure at 0° only — zxing's own blackbox harness runs every
//! suite at 0°/90°/180°/270°. This instrument closes that blind spot: each
//! zxing-blackbox image is rotated by exact cardinal remapping (pure index
//! permutation — no interpolation, no resampling, the pixels are
//! bit-identical) and scanned with the same budget-free score-off scanner
//! the external gate uses. Any 90/180/270 count sitting far below its 0°
//! sibling is an ENGINE orientation gap, not image damage — the rotation is
//! lossless by construction.
//!
//! Non-gating (prints the table, always exits 0): cardinal-rotation truth
//! moves with engine versions, so it reads as a dashboard, not a pin. The
//! per-suite `zxing-ref` column is zxing's `mustPassCount` at 0° — their
//! per-rotation thresholds differ per suite and are not mirrored here; our
//! own 0° column is the baseline that matters.

use std::path::Path;

use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner, ScoreDepth};
use rayon::prelude::*;

/// Exact cardinal rotation of a luma8 buffer — index permutation only.
fn rot_luma(data: &[u8], w: u32, h: u32, quarter_turns: u8) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    let idx = |x: usize, y: usize| y * w + x;
    match quarter_turns {
        // 90° CW: dst(x,y) ← src(y, h-1-x) · dst dims (h, w)
        1 => {
            let mut out = vec![0u8; w * h];
            for y in 0..w {
                for x in 0..h {
                    out[y * h + x] = data[idx(y, h - 1 - x)];
                }
            }
            (out, h as u32, w as u32)
        }
        // 180°: dst(x,y) ← src(w-1-x, h-1-y) · dims unchanged
        2 => {
            let mut out: Vec<u8> = data.to_vec();
            out.reverse();
            (out, w as u32, h as u32)
        }
        // 270° CW: dst(x,y) ← src(w-1-y, x) · dst dims (h, w)
        3 => {
            let mut out = vec![0u8; w * h];
            for y in 0..w {
                for x in 0..h {
                    out[y * h + x] = data[idx(w - 1 - y, x)];
                }
            }
            (out, h as u32, w as u32)
        }
        _ => (data.to_vec(), w as u32, h as u32),
    }
}

/// Arbitrary-angle bilinear rotation into the grown bounding box, white
/// background — the same geometry the score axis uses post-fix (canvas
/// grows, out-of-source paints background). xtask-local on purpose: the
/// lib's warp is score-internal (`pub(crate)`), and an instrument carrying
/// its own 40-line copy beats widening the lib surface for a dashboard.
fn rot_bilinear(data: &[u8], w: u32, h: u32, angle_deg: f32) -> (Vec<u8>, u32, u32) {
    let (sin64, cos64) = f64::from(angle_deg).to_radians().sin_cos();
    let out_w = (f64::from(w) * cos64.abs() + f64::from(h) * sin64.abs() - 1e-6).ceil() as u32;
    let out_h = (f64::from(w) * sin64.abs() + f64::from(h) * cos64.abs() - 1e-6).ceil() as u32;
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    #[expect(
        clippy::cast_precision_loss,
        reason = "corpus image dimensions fit f32 exactly"
    )]
    let (scx, scy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    #[expect(
        clippy::cast_precision_loss,
        reason = "corpus image dimensions fit f32 exactly"
    )]
    let (dcx, dcy) = ((out_w as f32 - 1.0) / 2.0, (out_h as f32 - 1.0) / 2.0);
    let (sw, sh) = (scx * 2.0, scy * 2.0);
    let mut out = Vec::with_capacity((out_w * out_h) as usize);
    for y in 0..out_h {
        for x in 0..out_w {
            #[expect(
                clippy::cast_precision_loss,
                reason = "pixel coordinates fit f32 exactly"
            )]
            let (vx, vy) = (x as f32 - dcx, y as f32 - dcy);
            // inverse rotation: dst → src
            let sx = cos * vx + sin * vy + scx;
            let sy = -sin * vx + cos * vy + scy;
            let px = if sx >= -0.5 && sx <= sw + 0.5 && sy >= -0.5 && sy <= sh + 0.5 {
                bilinear(data, w, h, sx, sy)
            } else {
                255
            };
            out.push(px);
        }
    }
    (out, out_w, out_h)
}

fn bilinear(data: &[u8], w: u32, h: u32, x: f32, y: f32) -> u8 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "clamped to image bounds before casting; corpus dims fit f32"
    )]
    let sample = |xi: f32, yi: f32| -> f32 {
        let xi = xi.clamp(0.0, (w - 1) as f32) as u32;
        let yi = yi.clamp(0.0, (h - 1) as f32) as u32;
        f32::from(data[(yi * w + xi) as usize])
    };
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let top = sample(x0, y0) * (1.0 - fx) + sample(x0 + 1.0, y0) * fx;
    let bot = sample(x0, y0 + 1.0) * (1.0 - fx) + sample(x0 + 1.0, y0 + 1.0) * fx;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "luma interpolation stays within 0..=255"
    )]
    let px = (top * (1.0 - fy) + bot * fy).round() as u8;
    px
}

/// The budget-free, score-free scanner (same shape as the external gate's).
fn scanner() -> Scanner {
    let mut config = ScanProfile::Full.config();
    config.budget_ms = None;
    config.score_depth = ScoreDepth::Off;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
}

fn matches_truth(scanner: &Scanner, luma: &[u8], w: u32, h: u32, truth: &str) -> bool {
    scanner
        .scan(ImageInput::luma8(luma, w, h))
        .is_ok_and(|report| report.detections.iter().any(|d| d.content.text == truth))
}

/// Arbitrary-angle probe points — interpolated (trend), unlike the cardinals.
const ANGLES: [f32; 3] = [15.0, 30.0, 45.0];

/// Sorted decodable images of one suite (sibling `.txt` ground truth exists).
fn suite_images(suite_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut images: Vec<std::path::PathBuf> = std::fs::read_dir(suite_dir)
        .expect("read suite")
        .filter_map(|e| {
            let p = e.expect("dir entry").path();
            let ext = p
                .extension()
                .and_then(|x| x.to_str())
                .map(str::to_ascii_lowercase);
            (matches!(ext.as_deref(), Some("png" | "jpg" | "jpeg" | "gif"))
                && ground_truth(&p).is_some())
            .then_some(p)
        })
        .collect();
    images.sort();
    images
}

/// Per-suite match counts: run `per_image(base, w, h, truth)` over every
/// image in parallel and sum the N-column hit rows.
fn tally<const N: usize>(
    images: &[std::path::PathBuf],
    per_image: impl Fn(&[u8], u32, u32, &str) -> [u32; N] + Sync,
) -> [u32; N] {
    images
        .par_iter()
        .map(|img_path| {
            let truth = ground_truth(img_path).expect("filtered on Some");
            let luma = image::open(img_path)
                .unwrap_or_else(|e| panic!("{}: {e}", img_path.display()))
                .to_luma8();
            let (w, h) = (luma.width(), luma.height());
            per_image(&luma.into_raw(), w, h, &truth)
        })
        .reduce(
            || [0u32; N],
            |mut acc, hits| {
                for (a, h) in acc.iter_mut().zip(hits) {
                    *a += h;
                }
                acc
            },
        )
}

fn print_total<const N: usize>(grand_images: u32, totals: [u32; N]) {
    let cells: Vec<String> = totals.iter().map(|t| format!("**{t}**")).collect();
    println!("| **total** | **{grand_images}** | {} |", cells.join(" | "));
}

pub(crate) fn run() {
    let root = crate::repo_root();
    let dir = root.join("corpus-external").join("zxing-blackbox");
    if !dir.is_dir() {
        eprintln!(
            "corpus-external/zxing-blackbox/ not present — fetch the corpora first \
             (README « Reproducing the headline numbers »)."
        );
        std::process::exit(2);
    }

    let mut suites: Vec<String> = std::fs::read_dir(&dir)
        .expect("read zxing-blackbox")
        .filter_map(|e| {
            let e = e.expect("dir entry");
            e.path()
                .is_dir()
                .then(|| e.file_name().to_string_lossy().into_owned())
        })
        .collect();
    suites.sort();

    println!("| suite | images | 0° | 90° | 180° | 270° |");
    println!("|---|---|---|---|---|---|");
    let mut totals = [0u32; 4];
    let mut grand_images = 0u32;
    for suite in &suites {
        let images = suite_images(&dir.join(suite));
        let counts = tally::<4>(&images, |base, w, h, truth| {
            let scanner = scanner();
            let mut hits = [0u32; 4];
            for (turn, hit) in hits.iter_mut().enumerate() {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "turn is 0..4 by construction"
                )]
                let (buf, rw, rh) = rot_luma(base, w, h, turn as u8);
                *hit = u32::from(matches_truth(&scanner, &buf, rw, rh, truth));
            }
            hits
        });
        #[expect(
            clippy::cast_possible_truncation,
            reason = "suite sizes are double digits"
        )]
        let n = images.len() as u32;
        grand_images += n;
        for (t, c) in totals.iter_mut().zip(counts) {
            *t += c;
        }
        println!(
            "| {suite} | {n} | {} | {} | {} | {} |",
            counts[0], counts[1], counts[2], counts[3]
        );
    }
    print_total(grand_images, totals);
    println!(
        "\nrotations are exact index permutations (lossless) — a 90/180/270 column \
         below its 0° sibling is an engine orientation gap, never image damage"
    );

    // ---- arbitrary angles (interpolated — a TREND, not a verdict) ----------
    println!("\n| suite | images | 0° | 15° | 30° | 45° |");
    println!("|---|---|---|---|---|---|");
    let mut arb_totals = [0u32; 4];
    for suite in &suites {
        let images = suite_images(&dir.join(suite));
        let counts = tally::<4>(&images, |base, w, h, truth| {
            let scanner = scanner();
            let mut hits = [0u32; 4];
            hits[0] = u32::from(matches_truth(&scanner, base, w, h, truth));
            for (angle, hit) in ANGLES.iter().zip(hits[1..].iter_mut()) {
                let (buf, rw, rh) = rot_bilinear(base, w, h, *angle);
                *hit = u32::from(matches_truth(&scanner, &buf, rw, rh, truth));
            }
            hits
        });
        for (t, c) in arb_totals.iter_mut().zip(counts) {
            *t += c;
        }
        println!(
            "| {suite} | {} | {} | {} | {} | {} |",
            images.len(),
            counts[0],
            counts[1],
            counts[2],
            counts[3]
        );
    }
    print_total(grand_images, arb_totals);
    println!(
        "\narbitrary angles are BILINEAR (interpolated · grown canvas · white \
         background) — drops mix resampling loss with engine tolerance; read \
         this table as a trend line, never a gate"
    );
}

/// Sibling `.txt` ground truth, decoded the way the zxing harness reads it.
fn ground_truth(img_abs: &Path) -> Option<String> {
    let txt = img_abs.with_extension("txt");
    let raw = std::fs::read(txt).ok()?;
    Some(match String::from_utf8(raw) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn bilinear_rotation_grows_the_canvas_and_preserves_flat_fields() {
        // A uniform field is rotation-invariant under ANY resampling: every
        // bilinear sample of a constant image is that constant, and grown
        // corners paint the white background — so a white field stays pure
        // white at every angle (catches sign slips that would sample the
        // clamp edge) and the canvas dims match w·|cos|+h·|sin|.
        let flat = vec![255u8; 20 * 10];
        for angle in [15.0f32, 30.0, 45.0] {
            let (out, w, h) = rot_bilinear(&flat, 20, 10, angle);
            assert!(out.iter().all(|&p| p == 255), "{angle}° flat stays flat");
            let (s, c) = f64::from(angle).to_radians().sin_cos();
            let exp_w = (20.0 * c.abs() + 10.0 * s.abs() - 1e-6).ceil() as u32;
            let exp_h = (20.0 * s.abs() + 10.0 * c.abs() - 1e-6).ceil() as u32;
            assert_eq!((w, h), (exp_w, exp_h), "{angle}° bbox");
        }
        // A black field rotated 45° shows white in the grown corners.
        let black = vec![0u8; 16 * 16];
        let (out, w, h) = rot_bilinear(&black, 16, 16, 45.0);
        assert_eq!(out[0], 255, "grown corner is background");
        let centre = out[(h / 2 * w + w / 2) as usize];
        assert_eq!(centre, 0, "source content survives at the centre");
    }

    #[test]
    fn cardinal_rotations_are_exact_permutations() {
        // 3×2 with distinct bytes — every slot discriminated.
        let src = [1u8, 2, 3, 4, 5, 6]; // rows: [1 2 3] / [4 5 6]
        let (r90, w90, h90) = rot_luma(&src, 3, 2, 1);
        assert_eq!((w90, h90), (2, 3));
        assert_eq!(r90, [4, 1, 5, 2, 6, 3]); // columns bottom-up
        let (r180, w180, h180) = rot_luma(&src, 3, 2, 2);
        assert_eq!((w180, h180), (3, 2));
        assert_eq!(r180, [6, 5, 4, 3, 2, 1]);
        let (r270, w270, h270) = rot_luma(&src, 3, 2, 3);
        assert_eq!((w270, h270), (2, 3));
        assert_eq!(r270, [3, 6, 2, 5, 1, 4]); // columns top-down from the right
        // Four quarter turns compose to identity.
        let (once, w1, h1) = rot_luma(&src, 3, 2, 1);
        let (twice, w2, h2) = rot_luma(&once, w1, h1, 1);
        let (thrice, w3, h3) = rot_luma(&twice, w2, h2, 1);
        let (full, w4, h4) = rot_luma(&thrice, w3, h3, 1);
        assert_eq!((w4, h4), (3, 2));
        assert_eq!(full, src);
    }
}
