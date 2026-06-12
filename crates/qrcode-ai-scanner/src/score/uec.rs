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
//! The traversal/mask/de-interleave mirror rqrr 0.10.1 `decode.rs`
//! semantics (quirc heritage · Apache-2.0/MIT); the version table is
//! mechanically extracted from its `version_db.rs`, never hand-typed.

use crate::report::{EcLevel, UecGrade, UecReport};

/// RS parameters for the SMALL blocks of one version × EC level.
#[derive(Debug, Clone, Copy)]
struct Rs {
    /// Small block total size (data + ec codewords).
    bs: usize,
    /// Small block data codewords.
    dw: usize,
    /// Number of small blocks.
    ns: usize,
}

/// Per-version database row.
#[derive(Debug, Clone, Copy)]
struct VersionDb {
    /// Total codewords in the symbol.
    total: usize,
    /// Alignment pattern center rows/columns (0-terminated).
    apat: [usize; 7],
    /// RS parameters in FORMAT-BITS order `[M, L, H, Q]`.
    ecc: [Rs; 4],
}

/// QR version database — total codewords · alignment rows · RS parameters
/// per EC level in FORMAT-BITS order `[M, L, H, Q]` (matching rqrr's
/// `ecc_level`). Mechanically extracted from rqrr 0.10.1 `version_db.rs`
/// (quirc heritage · Apache-2.0/MIT) — NOT hand-typed. Index = version,
/// entry 0 is a zero sentinel. Pinned empirically by the clean-matrix test
/// (any row error scrambles the RS syndromes of that version).
/// Compact row constructor — the 41-row table at one line per version
/// (the exploded struct form tripled the file size past the 1500-LOC cap).
const fn v(total: usize, apat: [usize; 7], ecc: [(usize, usize, usize); 4]) -> VersionDb {
    VersionDb {
        total,
        apat,
        ecc: [
            Rs {
                bs: ecc[0].0,
                dw: ecc[0].1,
                ns: ecc[0].2,
            },
            Rs {
                bs: ecc[1].0,
                dw: ecc[1].1,
                ns: ecc[1].2,
            },
            Rs {
                bs: ecc[2].0,
                dw: ecc[2].1,
                ns: ecc[2].2,
            },
            Rs {
                bs: ecc[3].0,
                dw: ecc[3].1,
                ns: ecc[3].2,
            },
        ],
    }
}

#[rustfmt::skip]
const VERSIONS: [VersionDb; 41] = [
    v(0, [0,0,0,0,0,0,0], [(0,0,0),(0,0,0),(0,0,0),(0,0,0)]),
    v(26, [0,0,0,0,0,0,0], [(26,16,1),(26,19,1),(26,9,1),(26,13,1)]),
    v(44, [6,18,0,0,0,0,0], [(44,28,1),(44,34,1),(44,16,1),(44,22,1)]),
    v(70, [6,22,0,0,0,0,0], [(70,44,1),(70,55,1),(35,13,2),(35,17,2)]),
    v(100, [6,26,0,0,0,0,0], [(50,32,2),(100,80,1),(25,9,4),(50,24,2)]),
    v(134, [6,30,0,0,0,0,0], [(67,43,2),(134,108,1),(33,11,2),(33,15,2)]),
    v(172, [6,34,0,0,0,0,0], [(43,27,4),(86,68,2),(43,15,4),(43,19,4)]),
    v(196, [6,22,38,0,0,0,0], [(49,31,4),(98,78,2),(39,13,4),(32,14,2)]),
    v(242, [6,24,42,0,0,0,0], [(60,38,2),(121,97,2),(40,14,4),(40,18,4)]),
    v(292, [6,26,46,0,0,0,0], [(58,36,3),(146,116,2),(36,12,4),(36,16,4)]),
    v(346, [6,28,50,0,0,0,0], [(69,43,4),(86,68,2),(43,15,6),(43,19,6)]),
    v(404, [6,30,54,0,0,0,0], [(80,50,1),(101,81,4),(36,12,3),(50,22,4)]),
    v(466, [6,32,58,0,0,0,0], [(58,36,6),(116,92,2),(42,14,7),(46,20,4)]),
    v(532, [6,34,62,0,0,0,0], [(59,37,8),(133,107,4),(33,11,12),(44,20,8)]),
    v(581, [6,26,46,66,0,0,0], [(64,40,4),(145,115,3),(36,12,11),(36,16,11)]),
    v(655, [6,26,48,70,0,0,0], [(65,41,5),(109,87,5),(36,12,11),(54,24,5)]),
    v(733, [6,26,50,74,0,0,0], [(73,45,7),(122,98,5),(45,15,3),(43,19,15)]),
    v(815, [6,30,54,78,0,0,0], [(74,46,10),(135,107,1),(42,14,2),(50,22,1)]),
    v(901, [6,30,56,82,0,0,0], [(69,43,9),(150,120,5),(42,14,2),(50,22,17)]),
    v(991, [6,30,58,86,0,0,0], [(70,44,3),(141,113,3),(39,13,9),(47,21,17)]),
    v(1085, [6,34,62,90,0,0,0], [(67,41,3),(135,107,3),(43,15,15),(54,24,15)]),
    v(1156, [6,28,50,72,92,0,0], [(68,42,17),(144,116,4),(46,16,19),(50,22,17)]),
    v(1258, [6,26,50,74,98,0,0], [(74,46,17),(139,111,2),(37,13,34),(54,24,7)]),
    v(1364, [6,30,54,78,102,0,0], [(75,47,4),(151,121,4),(45,15,16),(54,24,11)]),
    v(1474, [6,28,54,80,106,0,0], [(73,45,6),(147,117,6),(46,16,30),(54,24,11)]),
    v(1588, [6,32,58,84,110,0,0], [(75,47,8),(132,106,8),(45,15,22),(54,24,7)]),
    v(1706, [6,30,58,86,114,0,0], [(74,46,19),(142,114,10),(46,16,33),(50,22,28)]),
    v(1828, [6,34,62,90,118,0,0], [(73,45,22),(152,122,8),(45,15,12),(53,23,8)]),
    v(1921, [6,26,50,74,98,122,0], [(73,45,3),(147,117,3),(45,15,11),(54,24,4)]),
    v(2051, [6,30,54,78,102,126,0], [(73,45,21),(146,116,7),(45,15,19),(53,23,1)]),
    v(2185, [6,26,52,78,104,130,0], [(75,47,19),(145,115,5),(45,15,23),(54,24,15)]),
    v(2323, [6,30,56,82,108,134,0], [(74,46,2),(145,115,13),(45,15,23),(54,24,42)]),
    v(2465, [6,34,60,86,112,138,0], [(74,46,10),(145,115,17),(45,15,19),(54,24,10)]),
    v(2611, [6,30,58,86,114,142,0], [(74,46,14),(145,115,17),(45,15,11),(54,24,29)]),
    v(2761, [6,34,62,90,118,146,0], [(74,46,14),(145,115,13),(46,16,59),(54,24,44)]),
    v(2876, [6,30,54,78,102,126,150], [(75,47,12),(151,121,12),(45,15,22),(54,24,39)]),
    v(3034, [6,24,50,76,102,128,154], [(75,47,6),(151,121,6),(45,15,2),(54,24,46)]),
    v(3196, [6,28,54,80,106,132,158], [(74,46,29),(152,122,17),(45,15,24),(54,24,49)]),
    v(3362, [6,32,58,84,110,136,162], [(74,46,13),(152,122,4),(45,15,42),(54,24,48)]),
    v(3532, [6,26,54,82,110,138,166], [(75,47,40),(147,117,20),(45,15,10),(54,24,43)]),
    v(3706, [6,30,58,86,114,142,170], [(75,47,18),(148,118,19),(45,15,20),(54,24,34)]),
];

/// Format-bits index for the table lookup (M=0 · L=1 · H=2 · Q=3).
pub(crate) fn format_bits_index(ec: EcLevel) -> usize {
    match ec {
        EcLevel::M => 0,
        EcLevel::L => 1,
        EcLevel::H => 2,
        EcLevel::Q => 3,
    }
}

/// Function-pattern map — mirrors rqrr `reserved_cell` exactly.
fn reserved_cell(version: usize, i: usize, j: usize) -> bool {
    let ver = &VERSIONS[version];
    let size = version * 4 + 17;

    // finder + format: top-left · bottom-left · top-right
    if j < 9 && (i < 9 || i + 8 >= size) {
        return true;
    }
    if i < 9 && j + 8 >= size {
        return true;
    }
    // timing patterns
    if i == 6 || j == 6 {
        return true;
    }
    // version info (v7+)
    if version >= 7 && ((i < 6 && j + 11 >= size) || (i + 11 >= size && j < 6)) {
        return true;
    }
    // alignment patterns
    let mut ai = None;
    let mut aj = None;
    let mut len = 0;
    for (a, &pattern) in ver.apat.iter().take_while(|&&p| p != 0).enumerate() {
        len = a;
        if pattern.abs_diff(i) < 3 {
            ai = Some(a);
        }
        if pattern.abs_diff(j) < 3 {
            aj = Some(a);
        }
    }
    match (ai, aj) {
        (Some(x), Some(y)) if x == len && y == len => true,
        (Some(x), Some(_)) if 0 < x && x < len => true,
        (Some(_), Some(y)) if 0 < y && y < len => true,
        _ => false,
    }
}

/// Mask predicate — mirrors rqrr `mask_bit` (the 8 ISO formulas).
fn mask_bit(mask: u8, y: usize, x: usize) -> bool {
    match mask {
        0 => (y + x).is_multiple_of(2),
        1 => y.is_multiple_of(2),
        2 => x.is_multiple_of(3),
        3 => (y + x).is_multiple_of(3),
        4 => ((y / 2) + (x / 3)).is_multiple_of(2),
        5 => (y * x) % 2 + (y * x) % 3 == 0,
        6 => ((y * x) % 2 + (y * x) % 3).is_multiple_of(2),
        7 => ((y * x) % 3 + (y + x) % 2).is_multiple_of(2),
        _ => false,
    }
}

/// Module position of every emitted data bit, in stream order — replays
/// rqrr `read_data`'s zigzag exactly (down-up column pairs from the
/// bottom-right, column 6 skipped).
pub(crate) fn zigzag_positions(version: usize) -> Vec<(usize, usize)> {
    let size = version * 4 + 17;
    let mut positions = Vec::with_capacity(VERSIONS[version].total * 8);
    let mut y = size - 1;
    let mut x = size - 1;
    let mut neg_dir = true;
    while x > 0 {
        if x == 6 {
            x -= 1;
        }
        if !reserved_cell(version, y, x) {
            positions.push((y, x));
        }
        if !reserved_cell(version, y, x - 1) {
            positions.push((y, x - 1));
        }
        let (new_y, new_neg_dir) = match (y, neg_dir) {
            (0, true) => {
                x = x.saturating_sub(2);
                (0, false)
            }
            (yy, false) if yy == size - 1 => {
                x = x.saturating_sub(2);
                (size - 1, true)
            }
            (yy, true) => (yy - 1, true),
            (yy, false) => (yy + 1, false),
        };
        y = new_y;
        neg_dir = new_neg_dir;
    }
    positions
}

/// Unmask the still-masked stream into interleaved codeword bytes.
/// Returns `None` when the stream is shorter than the version's capacity.
pub(crate) fn unmask_to_codewords(
    masked: &[u8],
    bit_len: usize,
    version: usize,
    mask: u8,
) -> Option<Vec<u8>> {
    let total = VERSIONS[version].total;
    // BOTH the declared bit length AND the actual buffer must cover the
    // symbol: bit_len alone is caller-supplied and the loop below indexes
    // the buffer (doc promise: short stream ⇒ None, never garbage/panic).
    if bit_len < total * 8 || masked.len() < total {
        return None;
    }
    let positions = zigzag_positions(version);
    let mut codewords = vec![0u8; total];
    for (k, &(y, x)) in positions.iter().take(total * 8).enumerate() {
        // rqrr RawData packing: MSB-first within each byte
        let mut bit = (masked[k / 8] >> (7 - (k % 8))) & 1;
        if mask_bit(mask, y, x) {
            bit ^= 1;
        }
        codewords[k / 8] |= bit << (7 - (k % 8));
    }
    Some(codewords)
}

/// One de-interleaved RS block: `data ++ ec`, plus its parameters.
pub(crate) struct Block {
    pub(crate) bytes: Vec<u8>,
    pub(crate) npar: usize,
    /// Interleaved-stream index each byte came from — the erasure-marking
    /// bridge (confidence is measured per interleaved codeword).
    pub(crate) origins: Vec<usize>,
}

/// De-interleave per the ISO 18004 §8.6 round-robin: data round `j` emits
/// one codeword per block that still has a `j`-th data codeword (small
/// blocks exhaust first), then EC rounds cover every block uniformly.
///
/// NOTE — deliberately NOT a mirror of rqrr `codestream_ecc`: for the extra
/// data round of large blocks quirc/rqrr read `j*bc+i`, which lands past
/// the data segment (EC bytes) instead of the round-15 positions; RS
/// correction silently absorbs the slip (t=+1 per large block), which is
/// invisible to decoding but poisons an error-count margin. Pinned by the
/// pristine matrix test: mixed-block cells (e.g. v5-Q) read t=0 here.
pub(crate) fn deinterleave(codewords: &[u8], version: usize, ec_index: usize) -> Vec<Block> {
    let ver = &VERSIONS[version];
    let small = ver.ecc[ec_index];
    let large_count = (ver.total - small.bs * small.ns) / (small.bs + 1);
    let block_count = large_count + small.ns;
    // total data codewords = EC segment start
    let ecc_offset = small.dw * block_count + large_count;

    let mut blocks = Vec::with_capacity(block_count);
    for i in 0..block_count {
        let (bs, dw) = if i < small.ns {
            (small.bs, small.dw)
        } else {
            (small.bs + 1, small.dw + 1)
        };
        let mut bytes = Vec::with_capacity(bs);
        let mut origins = Vec::with_capacity(bs);
        for j in 0..dw {
            let idx = if j < small.dw {
                // rounds where every block participates
                j * block_count + i
            } else {
                // the extra round: only large blocks emit, in block order
                small.dw * block_count + (i - small.ns)
            };
            bytes.push(codewords[idx]);
            origins.push(idx);
        }
        for j in 0..(bs - dw) {
            let idx = ecc_offset + j * block_count + i;
            bytes.push(codewords[idx]);
            origins.push(idx);
        }
        blocks.push(Block {
            bytes,
            npar: bs - dw,
            origins,
        });
    }
    blocks
}

// ---- GF(256), poly 0x11D, generator α = 2 (the QR field) ----

#[expect(
    clippy::cast_possible_truncation,
    reason = "value is reduced mod 0x11D before the cast — always < 256"
)]
const GF_EXP: [u8; 256] = {
    let mut exp = [0u8; 256];
    let mut value: u16 = 1;
    let mut i = 0;
    while i < 256 {
        exp[i] = value as u8;
        value <<= 1;
        if value & 0x100 != 0 {
            value ^= 0x11D;
        }
        i += 1;
    }
    exp
};

#[expect(clippy::cast_possible_truncation, reason = "i < 255")]
const GF_LOG: [u8; 256] = {
    let mut log = [0u8; 256];
    let mut i = 0;
    while i < 255 {
        log[GF_EXP[i] as usize] = i as u8;
        i += 1;
    }
    log
};

pub(crate) fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let idx = (usize::from(GF_LOG[a as usize]) + usize::from(GF_LOG[b as usize])) % 255;
    GF_EXP[idx]
}

/// α^n.
pub(crate) fn gf_pow(n: usize) -> u8 {
    GF_EXP[n % 255]
}

/// RS syndromes `S_i = c(α^i)` for `i ∈ 0..npar`, with `block[len−1−j]`
/// the coefficient of `x^j` (transmission order) — rqrr convention.
pub(crate) fn syndromes(block: &[u8], npar: usize) -> Vec<u8> {
    let mut s = vec![0u8; npar];
    for (i, slot) in s.iter_mut().enumerate() {
        for j in 0..block.len() {
            let c = block[block.len() - 1 - j];
            *slot ^= gf_mul(c, gf_pow(i * j));
        }
    }
    s
}

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

pub(crate) fn gf_inv(a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    GF_EXP[(255 - usize::from(GF_LOG[a as usize])) % 255]
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

    let mut worst_margin = 1.0f32;
    let mut worst_errors = 0u8;
    let mut worst_capacity = 0u8;
    for block in &blocks {
        let synd = syndromes(&block.bytes, block.npar);
        let t = if synd.iter().all(|&s| s == 0) {
            0
        } else {
            error_count(&synd)
        };
        #[expect(clippy::cast_precision_loss, reason = "npar ≤ 68, t ≤ 34")]
        let margin = 1.0 - (2.0 * t as f32) / block.npar as f32;
        if margin < worst_margin {
            worst_margin = margin;
        }
        let errors = u8::try_from(t).unwrap_or(u8::MAX);
        let capacity = u8::try_from(block.npar).unwrap_or(u8::MAX);
        if errors >= worst_errors {
            worst_errors = errors;
            worst_capacity = capacity;
        }
    }
    let margin = worst_margin.max(0.0);
    Some(UecReport {
        margin,
        grade: UecGrade::from_margin(margin),
        worst_block_errors: worst_errors,
        worst_block_capacity: worst_capacity,
    })
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

    /// Hand-computed ISO 18004 mask-formula truth table — the pristine
    /// matrix only exercises whichever masks the generator picked; mutation
    /// testing showed masks 1 and 6 had no kill coverage.
    #[test]
    fn mask_formulas_truth_table() {
        // (mask, y, x, expected)
        let table: [(u8, usize, usize, bool); 24] = [
            (0, 0, 0, true),
            (0, 0, 1, false),
            (0, 1, 2, false),
            (0, 2, 2, true),
            (1, 0, 5, true),
            (1, 1, 5, false),
            (1, 2, 0, true),
            (2, 0, 0, true),
            (2, 0, 1, false),
            (2, 0, 3, true),
            (3, 0, 0, true),
            (3, 1, 2, true),
            (3, 2, 2, false),
            (4, 0, 0, true),
            (4, 2, 0, false),
            (4, 2, 3, true),
            (5, 0, 4, true),
            (5, 1, 1, false),
            (5, 2, 3, true),
            (6, 1, 1, true),
            (6, 1, 5, false),
            (6, 2, 3, true),
            (7, 0, 1, false),
            (7, 1, 5, true),
        ];
        for (mask, y, x, expected) in table {
            assert_eq!(
                mask_bit(mask, y, x),
                expected,
                "mask {mask} at (y={y}, x={x})"
            );
        }
        assert!(!mask_bit(8, 0, 0), "unknown mask is never set");
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
        assert_eq!(UecGrade::from_margin(0.10), UecGrade::F);
    }

    #[test]
    fn version_table_totals_are_consistent() {
        // total codewords must equal the de-interleave block sum, all
        // versions × all EC levels — catches any extraction slip.
        for version in 1..=40usize {
            let ver = &VERSIONS[version];
            for ec_index in 0..4 {
                let small = ver.ecc[ec_index];
                assert!(small.bs > small.dw, "v{version} ec{ec_index}");
                let large_count = (ver.total - small.bs * small.ns) / (small.bs + 1);
                let reconstructed = small.bs * small.ns + (small.bs + 1) * large_count;
                assert_eq!(
                    reconstructed, ver.total,
                    "v{version} ec{ec_index}: blocks don't tile the symbol"
                );
            }
        }
    }
}
