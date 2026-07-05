//! Synthetic Unused Error Correction — the ISO 15415 robustness margin,
//! computed from rqrr's own sampled bitstream. No pure-Rust competitor
//! ships this.
//!
//! Route (research-locked 2026-06-11, supersedes the design's homography
//! re-sampling idea — strictly better alignment): rqrr `get_raw_data()`
//! returns the raw, still-masked bitstream exactly as the engine sampled
//! the grid. We replay the deterministic zigzag traversal to recover each
//! bit's module position, unmask, de-interleave into RS blocks, then count
//! errors per block via syndromes + Berlekamp-Massey DEGREE — no decoding,
//! no re-encoding, hence zero encoder-segmentation ambiguity.
//!
//! `UEC = 1 − 2t/d` per block (no erasures in camera/image scanning),
//! worst block wins. ISO grade bands A ≥0.62 · B ≥0.50 · C ≥0.37 ·
//! D ≥0.25 · F. Known limit (documented, deliberate): the ISO `p`
//! correction (misdecode-protection codewords on the very low versions)
//! is not subtracted — margins on v1-v2 read marginally optimistic.
//!
//! The matrix substrate this builds on — the zigzag/unmask walk, the RS
//! de-interleave, the version database, and the GF(256) syndromes — lives in
//! `crate::matrix` (shared with the rescue stage). Only the error COUNT
//! (Berlekamp-Massey degree) and the margin arithmetic are UEC-specific.

use crate::matrix::deinterleave::deinterleave;
use crate::matrix::gf256::{gf_inv, gf_mul, syndromes};
use crate::matrix::version_db::format_bits_index;
use crate::matrix::zigzag::unmask_to_codewords;
use crate::report::{EcLevel, UecGrade, UecReport};

/// Berlekamp-Massey over GF(256): the error-locator degree L = number of
/// errors, exact whenever `t ≤ npar/2` (guaranteed here — the symbol
/// decoded upstream).
fn error_count(synd: &[u8]) -> usize {
    let len = synd.len();
    let mut locator = vec![0u8; len + 1]; // C(x) — current error locator
    let mut prev = vec![0u8; len + 1]; // B(x) — locator before last length change
    locator[0] = 1;
    prev[0] = 1;
    let mut degree = 0usize; // L — current locator degree
    let mut shift = 1usize; // m — rounds since last length change
    let mut prev_discrepancy = 1u8; // b
    for round in 0..len {
        let mut discrepancy = synd[round];
        for k in 1..=degree {
            discrepancy ^= gf_mul(locator[k], synd[round - k]);
        }
        if discrepancy == 0 {
            shift += 1;
        } else if 2 * degree <= round {
            let snapshot = locator.clone();
            let coef = gf_mul(discrepancy, gf_inv(prev_discrepancy));
            for k in 0..(len + 1 - shift) {
                locator[k + shift] ^= gf_mul(coef, prev[k]);
            }
            degree = round + 1 - degree;
            prev = snapshot;
            prev_discrepancy = discrepancy;
            shift = 1;
        } else {
            let coef = gf_mul(discrepancy, gf_inv(prev_discrepancy));
            for k in 0..(len + 1 - shift) {
                locator[k + shift] ^= gf_mul(coef, prev[k]);
            }
            shift += 1;
        }
    }
    degree
}

/// Compute the synthetic UEC for a decoded symbol.
///
/// Inputs come from the rqrr path: the still-masked stream as sampled by
/// the engine + the format metadata. `None` when the version is out of
/// range or the stream is short (never report a garbage margin).
pub(crate) fn compute(
    masked: &[u8],
    bit_len: usize,
    version: u8,
    ec: EcLevel,
    mask: u8,
) -> Option<UecReport> {
    let version = usize::from(version);
    if !(1..=40).contains(&version) {
        return None;
    }
    let codewords = unmask_to_codewords(masked, bit_len, version, mask)?;
    let blocks = deinterleave(&codewords, version, format_bits_index(ec));

    let (margin, worst_errors, worst_capacity) = worst_block(blocks.iter().map(|block| {
        let synd = syndromes(&block.bytes, block.npar);
        let t = if synd.iter().all(|&s| s == 0) {
            0
        } else {
            error_count(&synd)
        };
        (t, block.npar)
    }));
    Some(UecReport {
        margin,
        grade: UecGrade::from_margin(margin),
        worst_block_errors: worst_errors,
        worst_block_capacity: worst_capacity,
    })
}

/// From each block's `(errors-corrected t, parity capacity npar)`, pick the block
/// CLOSEST to RS failure (lowest correction margin) and return its
/// `(margin, errors, capacity)` — all three describe the SAME block. The
/// `LowCorrectionMargin` hint depends on this coupling (a prior bug reported the
/// most-errors block's errors/capacity alongside the worst-margin block). `<=` so an
/// all-clean symbol (every margin == 1.0) still reports a real capacity, not 0.
fn worst_block(blocks: impl IntoIterator<Item = (usize, usize)>) -> (f32, u8, u8) {
    let mut worst_margin = 1.0f32;
    let mut errors = 0u8;
    let mut capacity = 0u8;
    for (t, npar) in blocks {
        #[expect(clippy::cast_precision_loss, reason = "npar ≤ 68, t ≤ 34")]
        let margin = 1.0 - (2.0 * t as f32) / npar as f32;
        if margin <= worst_margin {
            worst_margin = margin;
            errors = u8::try_from(t).unwrap_or(u8::MAX);
            capacity = u8::try_from(npar).unwrap_or(u8::MAX);
        }
    }
    (worst_margin.max(0.0), errors, capacity)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::float_cmp,
        clippy::needless_range_loop
    )]

    use super::*;
    use crate::matrix::zigzag::zigzag_positions;

    #[test]
    fn inconsistent_bit_len_vs_buffer_is_none_not_panic() {
        // the doc promise: "None when the stream is short — never a garbage
        // margin". A caller deriving bit_len independently of the buffer
        // (future binding) must not hit index-out-of-bounds.
        assert!(compute(&[0u8; 4], 32_000, 2, crate::report::EcLevel::Q, 3).is_none());
        assert!(compute(&[], 8, 1, crate::report::EcLevel::L, 0).is_none());
    }

    use crate::input::{ImageInput, Limits};
    use crate::ladder::{self, CancelToken, ScanConfig};
    use crate::transform::normalize;

    fn qr_png(content: &str, ec: qrcode::EcLevel, version: Option<i16>) -> Vec<u8> {
        let code = match version {
            Some(v) => {
                qrcode::QrCode::with_version(content.as_bytes(), qrcode::Version::Normal(v), ec)
                    .unwrap()
            }
            None => qrcode::QrCode::with_error_correction_level(content.as_bytes(), ec).unwrap(),
        };
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let mut buf = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn uec_of(png: &[u8]) -> UecReport {
        let planes = normalize(&ImageInput::encoded(png), &Limits::default()).unwrap();
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
        let d = &outcome.merged[0];
        let stream = d.masked_stream.as_ref().expect("rqrr stream captured");
        compute(
            &stream.bits,
            stream.bit_len,
            d.version.unwrap(),
            d.ec.unwrap(),
            d.mask.unwrap(),
        )
        .expect("uec computable")
    }

    #[test]
    fn pristine_matrix_versions_x_ec_is_grade_a_margin_one() {
        // Any table/zigzag/mask/de-interleave error scrambles the syndromes
        // of the affected version — this matrix pins all four mechanisms.
        // payloads sized to fit each version's H capacity — padding fills
        for (version, content) in [(1, "v1"), (2, "v2 pin"), (5, "v5 pin"), (7, "v7 pin")] {
            for ec in [
                qrcode::EcLevel::L,
                qrcode::EcLevel::M,
                qrcode::EcLevel::Q,
                qrcode::EcLevel::H,
            ] {
                let png = qr_png(content, ec, Some(version));
                let report = uec_of(&png);
                assert_eq!(
                    (report.margin, report.grade),
                    (1.0, UecGrade::A),
                    "v{version} {ec:?}: {report:?}"
                );
                assert_eq!(report.worst_block_errors, 0);
            }
        }
    }

    #[test]
    fn single_flipped_module_counts_exactly_one_error() {
        // Flip ONE module in the data region (image space): exactly one
        // codeword of one block corrupts → t = 1 on that block.
        let png = qr_png("flip one module", qrcode::EcLevel::Q, Some(2));
        let planes = normalize(&ImageInput::encoded(&png), &Limits::default()).unwrap();
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
        let d = &outcome.merged[0];
        let version = usize::from(d.version.unwrap());

        // first emitted data bit position → its module, in image space
        let (my, mx) = zigzag_positions(version)[0];
        let corners = d.corners.unwrap();
        let modules = version * 4 + 17;
        let module_px = (corners[1].x - corners[0].x) / (modules as f32 + 1.0);
        let (px, py) = (
            (corners[0].x + (mx as f32 + 0.5) * module_px) as u32,
            (corners[0].y + (my as f32 + 0.5) * module_px) as u32,
        );
        let mut data = planes.luma.data().to_vec();
        let w = planes.luma.width();
        let half = (module_px / 2.0).max(1.0) as u32;
        for y in py.saturating_sub(half)..(py + half).min(planes.luma.height()) {
            for x in px.saturating_sub(half)..(px + half).min(w) {
                let idx = (y * w + x) as usize;
                data[idx] = 255 - data[idx]; // invert the module
            }
        }
        let flipped = crate::input::LumaImage::new(data, w, planes.luma.height());

        // re-scan the damaged image through rqrr only
        let damaged = crate::engine::decode_all(&flipped);
        let dd = damaged
            .detections
            .iter()
            .find(|x| x.masked_stream.is_some())
            .expect("still decodes (Q level, one module)");
        let stream = dd.masked_stream.as_ref().unwrap();
        let report = compute(
            &stream.bits,
            stream.bit_len,
            dd.version.unwrap(),
            dd.ec.unwrap(),
            dd.mask.unwrap(),
        )
        .unwrap();

        assert_eq!(report.worst_block_errors, 1, "{report:?}");
        let d_cap = f32::from(report.worst_block_capacity);
        let expected = 1.0 - 2.0 / d_cap;
        assert!(
            (report.margin - expected).abs() < 1e-6,
            "margin {} vs expected {expected}",
            report.margin
        );
    }

    #[test]
    fn three_flips_in_one_codeword_still_count_one_error() {
        // RS errors are per-CODEWORD: corrupting 3 bits of the same byte
        // is still t=1.
        let png = qr_png("cw0", qrcode::EcLevel::Q, Some(2));
        let planes = normalize(&ImageInput::encoded(&png), &Limits::default()).unwrap();
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new(), None).unwrap();
        let d = &outcome.merged[0];
        let version = usize::from(d.version.unwrap());
        let positions = zigzag_positions(version);
        let corners = d.corners.unwrap();
        let modules = version * 4 + 17;
        let module_px = (corners[1].x - corners[0].x) / (modules as f32 + 1.0);

        let mut data = planes.luma.data().to_vec();
        let w = planes.luma.width();
        // bits 0,3,6 all live in codeword 0
        for &bit_index in &[0usize, 3, 6] {
            let (my, mx) = positions[bit_index];
            let (px, py) = (
                (corners[0].x + (mx as f32 + 0.5) * module_px) as u32,
                (corners[0].y + (my as f32 + 0.5) * module_px) as u32,
            );
            let half = (module_px / 2.0).max(1.0) as u32;
            for y in py.saturating_sub(half)..(py + half).min(planes.luma.height()) {
                for x in px.saturating_sub(half)..(px + half).min(w) {
                    let idx = (y * w + x) as usize;
                    data[idx] = 255 - data[idx];
                }
            }
        }
        let flipped = crate::input::LumaImage::new(data, w, planes.luma.height());
        let damaged = crate::engine::decode_all(&flipped);
        let dd = damaged
            .detections
            .iter()
            .find(|x| x.masked_stream.is_some())
            .expect("still decodes");
        let stream = dd.masked_stream.as_ref().unwrap();
        let report = compute(
            &stream.bits,
            stream.bit_len,
            dd.version.unwrap(),
            dd.ec.unwrap(),
            dd.mask.unwrap(),
        )
        .unwrap();
        assert_eq!(report.worst_block_errors, 1, "{report:?}");
    }

    #[test]
    fn short_stream_returns_none_never_garbage() {
        assert!(compute(&[0u8; 4], 32, 2, EcLevel::Q, 3).is_none());
        assert!(compute(&[], 0, 1, EcLevel::L, 0).is_none());
        assert!(compute(&[0u8; 4000], 32_000, 0, EcLevel::L, 0).is_none());
        assert!(compute(&[0u8; 4000], 32_000, 41, EcLevel::L, 0).is_none());
    }

    #[test]
    fn grade_bands_pinned() {
        assert_eq!(UecGrade::from_margin(1.0), UecGrade::A);
        assert_eq!(UecGrade::from_margin(0.62), UecGrade::A);
        assert_eq!(UecGrade::from_margin(0.61), UecGrade::B);
        assert_eq!(UecGrade::from_margin(0.50), UecGrade::B);
        assert_eq!(UecGrade::from_margin(0.49), UecGrade::C);
        assert_eq!(UecGrade::from_margin(0.37), UecGrade::C);
        assert_eq!(UecGrade::from_margin(0.30), UecGrade::D);
        assert_eq!(UecGrade::from_margin(0.25), UecGrade::D);
        assert_eq!(UecGrade::from_margin(0.24), UecGrade::F);
        assert_eq!(UecGrade::from_margin(0.10), UecGrade::F);
    }

    #[test]
    fn worst_block_tracks_lowest_margin_not_most_errors() {
        // Block A: 5 errors / 20 parity → margin 1 - 10/20 = 0.50
        // Block B: 3 errors /  8 parity → margin 1 -  6/8  = 0.25 (worse margin, FEWER errors)
        // Regression (P0): worst_block reports the worst-MARGIN block (B), not most-errors (A).
        let (margin, errors, capacity) = worst_block([(5, 20), (3, 8)]);
        assert!((margin - 0.25).abs() < 1e-6, "margin {margin}");
        assert_eq!(
            errors, 3,
            "errors must be the worst-margin block's (the bug reported 5)"
        );
        assert_eq!(
            capacity, 8,
            "capacity must be the worst-margin block's (the bug reported 20)"
        );

        // order-independent
        let (_, e, c) = worst_block([(3, 8), (5, 20)]);
        assert_eq!((e, c), (3, 8));

        // all-clean (every margin == 1.0) still reports a real capacity, not 0
        let (m, ze, zc) = worst_block([(0, 10), (0, 14)]);
        assert!((m - 1.0).abs() < 1e-6);
        assert_eq!(
            (ze, zc),
            (0, 14),
            "all-clean must report a real block capacity, not 0"
        );
    }

    /// An all-zero block is a valid RS codeword, so its syndromes are the
    /// error polynomial's evaluations alone: inject `e` byte errors into one
    /// and Berlekamp-Massey's locator degree must be EXACTLY `e` (the crate
    /// invariant: exact for e ≤ npar/2). The e2e fixtures only ever reach
    /// t ≤ 1, which left BM's multi-error branch structure unpinned — the
    /// mutant-harvest survivors (run 28740297335) live there:
    /// - e=0 → 0: a `-> 1` stub reports a phantom error
    /// - e=1, value 1, coeff x⁰ (block END: syndromes reads block[len−1−j]):
    ///   `S_i` = 1 ∀i — round 0 must set degree = round + 1 − degree = 1; the
    ///   `+`→`*` swap leaves 0 and every later discrepancy cancels → returns 0
    /// - e=2 EQUAL values: `S_0` = v⊕v = 0, so round 0 exercises the
    ///   discrepancy-0 branch's `shift += 1` BEFORE any length change;
    ///   a stale shift (`*= 1`) mis-slides the update window of every round
    ///   that follows
    /// - e=3 asymmetric: any surviving bookkeeping slip drifts the degree
    #[test]
    fn berlekamp_massey_degree_counts_injected_errors_exactly() {
        let npar = 10;
        let clean = vec![0u8; 26];
        assert_eq!(error_count(&syndromes(&clean, npar)), 0, "no errors");

        let mut one = vec![0u8; 26];
        one[25] = 1; // coefficient of x^0
        assert_eq!(error_count(&syndromes(&one, npar)), 1, "single error");

        let mut pair = vec![0u8; 26];
        pair[3] = 0xA5;
        pair[17] = 0xA5;
        assert_eq!(error_count(&syndromes(&pair, npar)), 2, "equal-value pair");

        let mut triple = vec![0u8; 26];
        triple[0] = 5;
        triple[9] = 200;
        triple[21] = 33;
        assert_eq!(error_count(&syndromes(&triple, npar)), 3, "three errors");
    }

    /// Deterministic LCG sweep — 90 error patterns across npar 4..=20 with
    /// 1 ≤ e ≤ npar/2. BM is exact on every one, so any slip in the update
    /// fold, the update-window bound (`len + 1 − shift`) or the shift
    /// bookkeeping shifts some pattern's locator degree. Mirrors the morph
    /// deque-vs-naive sweep convention.
    #[test]
    fn berlekamp_massey_degree_matches_error_count_on_lcg_sweep() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |bound: u32| -> u32 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as u32 % bound
        };
        for case in 0..90 {
            let npar = (4 + next(17)) as usize; // 4..=20
            let len = npar + 10 + next(20) as usize;
            let e = (1 + next(npar as u32 / 2)) as usize; // 1..=npar/2
            let mut block = vec![0u8; len];
            let mut placed = 0;
            while placed < e {
                let pos = next(len as u32) as usize;
                if block[pos] == 0 {
                    block[pos] = (1 + next(255)) as u8;
                    placed += 1;
                }
            }
            let synd = syndromes(&block, npar);
            assert_eq!(
                error_count(&synd),
                e,
                "case {case}: npar={npar} len={len} e={e} block={block:?}"
            );
        }
    }
}
