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
//! D ≥0.25 · F. Known limit (documented, deliberate): ISO measures against
//! `d − p`; the Annex B reserve `p` of v1, v2-L and v3-L is not subtracted,
//! so a block with errors there reads above ISO's margin (spec/04-score.md).
//!
//! The stream measured is the one of the orientation the decode verified,
//! under that orientation's version, EC level and mask. A margin is read
//! only when EVERY block error-corrects errors-only there (`2t ≤ d` and
//! every error inside the block): a block that does not has no error count
//! to read — the locator degree of an uncorrectable word is noise, and a
//! wrong-orientation stream (a mirrored symbol read unmirrored, whose format
//! word decodes to another EC level and mask) once read margin 0 and capped
//! a clean code. The S5 rescue's stream, which needed erasures, keeps its
//! errors-only count ([`Stream::Rescued`]).
//!
//! The matrix substrate this builds on — the zigzag/unmask walk, the RS
//! de-interleave, the version database, and the GF(256) syndromes — lives in
//! `crate::matrix` (shared with the rescue stage). Only the error COUNT
//! (Berlekamp-Massey + a Chien search) and the margin arithmetic are
//! UEC-specific.

use crate::matrix::deinterleave::deinterleave;
use crate::matrix::gf256::{gf_inv, gf_mul, gf_pow, syndromes};
use crate::matrix::version_db::format_bits_index;
use crate::matrix::zigzag::unmask_to_codewords;
use crate::report::{EcLevel, UecGrade, UecReport};

/// Where a measured stream comes from — it decides what its error count
/// proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stream {
    /// The stream of the orientation an engine decoded, or of a grid the
    /// scanner re-derived: measured only when every block error-corrects
    /// errors-only under the detection's version, EC level and mask.
    Decoded,
    /// The S5 rescue's stream: it needed erasures, so its errors-only count
    /// stands as read — typically margin 0, the distrust signal its hint
    /// carries.
    Rescued,
}

/// Berlekamp-Massey over GF(256): the error locator Λ(x) (ascending powers)
/// and the length L of the shortest LFSR generating the syndromes — the
/// number of errors whenever the block error-corrects errors-only, which
/// [`corrected_errors`] checks.
fn berlekamp_massey(synd: &[u8]) -> (Vec<u8>, usize) {
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
    (locator, degree)
}

/// The locator length alone — the count of errors the block carries when it
/// error-corrects, and the count a rescued stream keeps as read.
fn error_count(synd: &[u8]) -> usize {
    berlekamp_massey(synd).1
}

/// Value of an ascending-power polynomial at `x` (Horner from the top).
fn poly_at(poly: &[u8], x: u8) -> u8 {
    poly.iter().rev().fold(0, |acc, &c| gf_mul(acc, x) ^ c)
}

/// The errors a block of `len` codewords corrects errors-only, or `None`
/// when it does not: more errors than half its parity, or a locator whose
/// roots do not all name positions inside the block (a Chien search over
/// `block[len − 1 − j]` ↔ `x^j`) — the decoder-failure signature. A locator
/// of length `t ≤ npar/2` with `t` distinct roots there names exactly one
/// correction, which yields a codeword.
fn corrected_errors(synd: &[u8], len: usize, npar: usize) -> Option<usize> {
    let (locator, errors) = berlekamp_massey(synd);
    if 2 * errors > npar {
        return None;
    }
    let roots = (0..len)
        .filter(|&j| poly_at(&locator, gf_inv(gf_pow(j))) == 0)
        .count();
    (roots == errors).then_some(errors)
}

/// Compute the synthetic UEC for a decoded symbol.
///
/// Inputs come from the rqrr path: the still-masked stream of the
/// orientation the decode verified + its format metadata. `None` when the
/// version is out of range, the stream is short, or — for a
/// [`Stream::Decoded`] stream — a block does not error-correct errors-only
/// under that format: never a garbage margin.
pub(crate) fn compute(
    masked: &[u8],
    bit_len: usize,
    version: u8,
    ec: EcLevel,
    mask: u8,
    stream: Stream,
) -> Option<UecReport> {
    let version = usize::from(version);
    if !(1..=40).contains(&version) {
        return None;
    }
    let codewords = unmask_to_codewords(masked, bit_len, version, mask)?;
    let blocks = deinterleave(&codewords, version, format_bits_index(ec));

    let mut counted = Vec::with_capacity(blocks.len());
    for block in &blocks {
        let synd = syndromes(&block.bytes, block.npar);
        let t = if synd.iter().all(|&s| s == 0) {
            0
        } else {
            match stream {
                Stream::Decoded => corrected_errors(&synd, block.bytes.len(), block.npar)?,
                Stream::Rescued => error_count(&synd),
            }
        };
        counted.push((t, block.npar));
    }
    let (margin, worst_errors, worst_capacity) = worst_block(counted);
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

    /// A lone LATE syndrome spike — every round quiet until the last — drives
    /// the flip-branch update fold at maximum `shift` (= round + 1): the
    /// window `len + 1 - shift` narrows to exactly ONE write. The mutant
    /// `len + 1 → len - 1` of that bound turns the window negative and
    /// PANICS on usize underflow (len − 1 − shift, shift = len). Degree pin:
    /// a lone spike at the final round flips degree to round + 1 = len
    /// (off-exactness regime — pinned behavior, not an ISO guarantee).
    /// Its `+→*` sibling and the else-branch bound's `+→*` are census-proven
    /// equivalents (see .cargo/mutants.toml); the else-branch `+→-` drops a
    /// locator write the Chien check reads (`only_a_correctable_block_counts_its_errors`).
    #[test]
    fn late_spike_syndrome_runs_the_fold_at_maximum_shift() {
        assert_eq!(error_count(&[0, 1]), 2, "npar=2 lone late spike");
        assert_eq!(error_count(&[0, 0, 0, 0, 7]), 5, "npar=5 lone late spike");
    }

    use crate::matrix::zigzag::zigzag_positions;

    #[test]
    fn inconsistent_bit_len_vs_buffer_is_none_not_panic() {
        // the doc promise: "None when the stream is short — never a garbage
        // margin". A caller deriving bit_len independently of the buffer
        // (future binding) must not hit index-out-of-bounds.
        for stream in [Stream::Decoded, Stream::Rescued] {
            assert!(compute(&[0u8; 4], 32_000, 2, EcLevel::Q, 3, stream).is_none());
            assert!(compute(&[], 8, 1, EcLevel::L, 0, stream).is_none());
        }
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
            Stream::Decoded,
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
            Stream::Decoded,
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
            Stream::Decoded,
        )
        .unwrap();
        assert_eq!(report.worst_block_errors, 1, "{report:?}");
    }

    #[test]
    fn short_stream_returns_none_never_garbage() {
        for stream in [Stream::Decoded, Stream::Rescued] {
            assert!(compute(&[0u8; 4], 32, 2, EcLevel::Q, 3, stream).is_none());
            assert!(compute(&[], 0, 1, EcLevel::L, 0, stream).is_none());
            assert!(compute(&[0u8; 4000], 32_000, 0, EcLevel::L, 0, stream).is_none());
            assert!(compute(&[0u8; 4000], 32_000, 41, EcLevel::L, 0, stream).is_none());
        }
    }

    /// The grid rqrr detects in `luma`, read as sampled — unmirrored — with
    /// the format word it decodes that way: the still-masked stream, its bit
    /// length, the version, EC level and mask.
    fn as_sampled(luma: &crate::input::LumaImage) -> (Vec<u8>, usize, u8, EcLevel, u8) {
        let width = luma.width() as usize;
        let data = luma.data();
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
                data[y * width + x]
            });
        let grid = prepared.detect_grids().remove(0);
        let (meta, raw) = grid.get_raw_data().expect("the sampled format decodes");
        // rqrr's ecc_level is the format word's two-bit field
        let ec = [EcLevel::M, EcLevel::L, EcLevel::H, EcLevel::Q][usize::from(meta.ecc_level)];
        (
            raw.data[..raw.len.div_ceil(8)].to_vec(),
            raw.len,
            u8::try_from(meta.version.0).unwrap(),
            ec,
            u8::try_from(meta.mask).unwrap(),
        )
    }

    /// A mirrored symbol read unmirrored: its format word still decodes (to
    /// another EC level and mask) while its stream holds the wrong
    /// orientation's bits, which do not error-correct. As a decoded stream
    /// it measures nothing, rather than the zero margin its locator noise
    /// reads; as a rescued one it keeps that errors-only count.
    #[test]
    fn a_stream_that_does_not_error_correct_measures_no_margin() {
        let luma = crate::rescue::test_symbols::from_segments_mirrored(
            qrcode::Version::Normal(3),
            qrcode::EcLevel::M,
            |b| b.push_byte_data(b"https://qrc-ai.com/mirrored"),
        );
        let (bits, bit_len, version, ec, mask) = as_sampled(&luma);
        let stream = crate::engine::MaskedStream {
            bits: bits.clone(),
            bit_len,
        };
        assert!(
            matches!(
                crate::rescue::rederive(
                    &stream,
                    version,
                    ec,
                    mask,
                    crate::rescue::Acceptance::Engine
                ),
                crate::rescue::Rederived::Unverified(crate::rescue::Unverified::NothingVerifies)
            ),
            "precondition: no block of it error-corrects"
        );
        assert_eq!(
            compute(&bits, bit_len, version, ec, mask, Stream::Decoded),
            None
        );
        assert!(compute(&bits, bit_len, version, ec, mask, Stream::Rescued).is_some());
    }

    /// The Chien check against the decoder's own word: a block that
    /// error-corrects counts its errors exactly, one past half its parity
    /// counts nothing, and a lone late syndrome spike (a locator of length
    /// 2 with a repeated root) is no correction either.
    #[test]
    fn only_a_correctable_block_counts_its_errors() {
        let npar = 10;
        let mut block = vec![0u8; 26];
        for (placed, pos) in [3usize, 9, 14, 20, 25, 0].into_iter().enumerate() {
            block[pos] = u8::try_from(placed + 7).unwrap();
            let errors = placed + 1;
            let synd = syndromes(&block, npar);
            let expected = (2 * errors <= npar).then_some(errors);
            assert_eq!(
                corrected_errors(&synd, block.len(), npar),
                expected,
                "{errors} errors of {npar} parity"
            );
        }
        assert_eq!(corrected_errors(&[0, 1], 4, 2), None);
        // two parity codewords, one error: the locator is whole only after
        // the last round's else-branch fold — without it, the first round's
        // locator names no position of the block for this error value
        let mut short = vec![0u8; 4];
        short[1] = gf_pow(100);
        assert_eq!(
            corrected_errors(&syndromes(&short, 2), short.len(), 2),
            Some(1)
        );
    }

    /// More errors than half the parity is no correction, even when the
    /// locator names as many positions as its length: five syndromes whose
    /// locator of length 3 has three distinct roots in a 255-codeword block
    /// (2 · 3 > 5).
    #[test]
    fn a_locator_longer_than_half_the_parity_is_no_correction() {
        let (r1, r2) = (gf_inv(gf_pow(2)), gf_inv(gf_pow(9)));
        let r3 = r1 ^ r2;
        // Λ(x) = s·(x + r1)(x + r2)(x + r3) = 1 + c1·x + s·x³: r3 = r1 + r2
        // leaves no x² term
        let s = gf_inv(gf_mul(gf_mul(r1, r2), r3));
        let c1 = gf_mul(s, gf_mul(r1, r2) ^ gf_mul(r1, r3) ^ gf_mul(r2, r3));
        let synd = [0, 0, s, gf_mul(c1, s), gf_mul(c1, gf_mul(c1, s))];
        let (locator, length) = berlekamp_massey(&synd);
        assert_eq!((length, &locator[..4]), (3, &[1, c1, 0, s][..]));
        let named = (0..255)
            .filter(|&j| poly_at(&locator, gf_inv(gf_pow(j))) == 0)
            .count();
        assert_eq!(named, 3, "precondition: three distinct roots in the block");
        assert_eq!(corrected_errors(&synd, 255, 5), None);
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
