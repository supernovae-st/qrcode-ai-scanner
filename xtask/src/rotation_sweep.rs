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
        let suite_dir = dir.join(suite);
        let mut images: Vec<std::path::PathBuf> = std::fs::read_dir(&suite_dir)
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

        let counts: [u32; 4] = images
            .par_iter()
            .map(|img_path| {
                let truth = ground_truth(img_path).expect("filtered on Some");
                let luma = image::open(img_path)
                    .unwrap_or_else(|e| panic!("{}: {e}", img_path.display()))
                    .to_luma8();
                let (w, h) = (luma.width(), luma.height());
                let base = luma.into_raw();
                let scanner = scanner();
                let mut hits = [0u32; 4];
                for (turn, hit) in hits.iter_mut().enumerate() {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "turn is 0..4 by construction"
                    )]
                    let (buf, rw, rh) = rot_luma(&base, w, h, turn as u8);
                    if matches_truth(&scanner, &buf, rw, rh, &truth) {
                        *hit = 1;
                    }
                }
                hits
            })
            .reduce(
                || [0u32; 4],
                |mut acc, hits| {
                    for (a, h) in acc.iter_mut().zip(hits) {
                        *a += h;
                    }
                    acc
                },
            );

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
    println!(
        "| **total** | **{grand_images}** | **{}** | **{}** | **{}** | **{}** |",
        totals[0], totals[1], totals[2], totals[3]
    );
    println!(
        "\nrotations are exact index permutations (lossless) — a 90/180/270 column \
         below its 0° sibling is an engine orientation gap, never image damage"
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
