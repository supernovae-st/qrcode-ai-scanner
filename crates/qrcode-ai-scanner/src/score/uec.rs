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
const VERSIONS: [VersionDb; 41] = [
    VersionDb {
        total: 0,
        apat: [0, 0, 0, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 0,
                dw: 0,
                ns: 0,
            },
            Rs {
                bs: 0,
                dw: 0,
                ns: 0,
            },
            Rs {
                bs: 0,
                dw: 0,
                ns: 0,
            },
            Rs {
                bs: 0,
                dw: 0,
                ns: 0,
            },
        ],
    },
    VersionDb {
        total: 26,
        apat: [0, 0, 0, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 26,
                dw: 16,
                ns: 1,
            },
            Rs {
                bs: 26,
                dw: 19,
                ns: 1,
            },
            Rs {
                bs: 26,
                dw: 9,
                ns: 1,
            },
            Rs {
                bs: 26,
                dw: 13,
                ns: 1,
            },
        ],
    },
    VersionDb {
        total: 44,
        apat: [6, 18, 0, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 44,
                dw: 28,
                ns: 1,
            },
            Rs {
                bs: 44,
                dw: 34,
                ns: 1,
            },
            Rs {
                bs: 44,
                dw: 16,
                ns: 1,
            },
            Rs {
                bs: 44,
                dw: 22,
                ns: 1,
            },
        ],
    },
    VersionDb {
        total: 70,
        apat: [6, 22, 0, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 70,
                dw: 44,
                ns: 1,
            },
            Rs {
                bs: 70,
                dw: 55,
                ns: 1,
            },
            Rs {
                bs: 35,
                dw: 13,
                ns: 2,
            },
            Rs {
                bs: 35,
                dw: 17,
                ns: 2,
            },
        ],
    },
    VersionDb {
        total: 100,
        apat: [6, 26, 0, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 50,
                dw: 32,
                ns: 2,
            },
            Rs {
                bs: 100,
                dw: 80,
                ns: 1,
            },
            Rs {
                bs: 25,
                dw: 9,
                ns: 4,
            },
            Rs {
                bs: 50,
                dw: 24,
                ns: 2,
            },
        ],
    },
    VersionDb {
        total: 134,
        apat: [6, 30, 0, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 67,
                dw: 43,
                ns: 2,
            },
            Rs {
                bs: 134,
                dw: 108,
                ns: 1,
            },
            Rs {
                bs: 33,
                dw: 11,
                ns: 2,
            },
            Rs {
                bs: 33,
                dw: 15,
                ns: 2,
            },
        ],
    },
    VersionDb {
        total: 172,
        apat: [6, 34, 0, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 43,
                dw: 27,
                ns: 4,
            },
            Rs {
                bs: 86,
                dw: 68,
                ns: 2,
            },
            Rs {
                bs: 43,
                dw: 15,
                ns: 4,
            },
            Rs {
                bs: 43,
                dw: 19,
                ns: 4,
            },
        ],
    },
    VersionDb {
        total: 196,
        apat: [6, 22, 38, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 49,
                dw: 31,
                ns: 4,
            },
            Rs {
                bs: 98,
                dw: 78,
                ns: 2,
            },
            Rs {
                bs: 39,
                dw: 13,
                ns: 4,
            },
            Rs {
                bs: 32,
                dw: 14,
                ns: 2,
            },
        ],
    },
    VersionDb {
        total: 242,
        apat: [6, 24, 42, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 60,
                dw: 38,
                ns: 2,
            },
            Rs {
                bs: 121,
                dw: 97,
                ns: 2,
            },
            Rs {
                bs: 40,
                dw: 14,
                ns: 4,
            },
            Rs {
                bs: 40,
                dw: 18,
                ns: 4,
            },
        ],
    },
    VersionDb {
        total: 292,
        apat: [6, 26, 46, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 58,
                dw: 36,
                ns: 3,
            },
            Rs {
                bs: 146,
                dw: 116,
                ns: 2,
            },
            Rs {
                bs: 36,
                dw: 12,
                ns: 4,
            },
            Rs {
                bs: 36,
                dw: 16,
                ns: 4,
            },
        ],
    },
    VersionDb {
        total: 346,
        apat: [6, 28, 50, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 69,
                dw: 43,
                ns: 4,
            },
            Rs {
                bs: 86,
                dw: 68,
                ns: 2,
            },
            Rs {
                bs: 43,
                dw: 15,
                ns: 6,
            },
            Rs {
                bs: 43,
                dw: 19,
                ns: 6,
            },
        ],
    },
    VersionDb {
        total: 404,
        apat: [6, 30, 54, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 80,
                dw: 50,
                ns: 1,
            },
            Rs {
                bs: 101,
                dw: 81,
                ns: 4,
            },
            Rs {
                bs: 36,
                dw: 12,
                ns: 3,
            },
            Rs {
                bs: 50,
                dw: 22,
                ns: 4,
            },
        ],
    },
    VersionDb {
        total: 466,
        apat: [6, 32, 58, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 58,
                dw: 36,
                ns: 6,
            },
            Rs {
                bs: 116,
                dw: 92,
                ns: 2,
            },
            Rs {
                bs: 42,
                dw: 14,
                ns: 7,
            },
            Rs {
                bs: 46,
                dw: 20,
                ns: 4,
            },
        ],
    },
    VersionDb {
        total: 532,
        apat: [6, 34, 62, 0, 0, 0, 0],
        ecc: [
            Rs {
                bs: 59,
                dw: 37,
                ns: 8,
            },
            Rs {
                bs: 133,
                dw: 107,
                ns: 4,
            },
            Rs {
                bs: 33,
                dw: 11,
                ns: 12,
            },
            Rs {
                bs: 44,
                dw: 20,
                ns: 8,
            },
        ],
    },
    VersionDb {
        total: 581,
        apat: [6, 26, 46, 66, 0, 0, 0],
        ecc: [
            Rs {
                bs: 64,
                dw: 40,
                ns: 4,
            },
            Rs {
                bs: 145,
                dw: 115,
                ns: 3,
            },
            Rs {
                bs: 36,
                dw: 12,
                ns: 11,
            },
            Rs {
                bs: 36,
                dw: 16,
                ns: 11,
            },
        ],
    },
    VersionDb {
        total: 655,
        apat: [6, 26, 48, 70, 0, 0, 0],
        ecc: [
            Rs {
                bs: 65,
                dw: 41,
                ns: 5,
            },
            Rs {
                bs: 109,
                dw: 87,
                ns: 5,
            },
            Rs {
                bs: 36,
                dw: 12,
                ns: 11,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 5,
            },
        ],
    },
    VersionDb {
        total: 733,
        apat: [6, 26, 50, 74, 0, 0, 0],
        ecc: [
            Rs {
                bs: 73,
                dw: 45,
                ns: 7,
            },
            Rs {
                bs: 122,
                dw: 98,
                ns: 5,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 3,
            },
            Rs {
                bs: 43,
                dw: 19,
                ns: 15,
            },
        ],
    },
    VersionDb {
        total: 815,
        apat: [6, 30, 54, 78, 0, 0, 0],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 10,
            },
            Rs {
                bs: 135,
                dw: 107,
                ns: 1,
            },
            Rs {
                bs: 42,
                dw: 14,
                ns: 2,
            },
            Rs {
                bs: 50,
                dw: 22,
                ns: 1,
            },
        ],
    },
    VersionDb {
        total: 901,
        apat: [6, 30, 56, 82, 0, 0, 0],
        ecc: [
            Rs {
                bs: 69,
                dw: 43,
                ns: 9,
            },
            Rs {
                bs: 150,
                dw: 120,
                ns: 5,
            },
            Rs {
                bs: 42,
                dw: 14,
                ns: 2,
            },
            Rs {
                bs: 50,
                dw: 22,
                ns: 17,
            },
        ],
    },
    VersionDb {
        total: 991,
        apat: [6, 30, 58, 86, 0, 0, 0],
        ecc: [
            Rs {
                bs: 70,
                dw: 44,
                ns: 3,
            },
            Rs {
                bs: 141,
                dw: 113,
                ns: 3,
            },
            Rs {
                bs: 39,
                dw: 13,
                ns: 9,
            },
            Rs {
                bs: 47,
                dw: 21,
                ns: 17,
            },
        ],
    },
    VersionDb {
        total: 1085,
        apat: [6, 34, 62, 90, 0, 0, 0],
        ecc: [
            Rs {
                bs: 67,
                dw: 41,
                ns: 3,
            },
            Rs {
                bs: 135,
                dw: 107,
                ns: 3,
            },
            Rs {
                bs: 43,
                dw: 15,
                ns: 15,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 15,
            },
        ],
    },
    VersionDb {
        total: 1156,
        apat: [6, 28, 50, 72, 92, 0, 0],
        ecc: [
            Rs {
                bs: 68,
                dw: 42,
                ns: 17,
            },
            Rs {
                bs: 144,
                dw: 116,
                ns: 4,
            },
            Rs {
                bs: 46,
                dw: 16,
                ns: 19,
            },
            Rs {
                bs: 50,
                dw: 22,
                ns: 17,
            },
        ],
    },
    VersionDb {
        total: 1258,
        apat: [6, 26, 50, 74, 98, 0, 0],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 17,
            },
            Rs {
                bs: 139,
                dw: 111,
                ns: 2,
            },
            Rs {
                bs: 37,
                dw: 13,
                ns: 34,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 7,
            },
        ],
    },
    VersionDb {
        total: 1364,
        apat: [6, 30, 54, 78, 102, 0, 0],
        ecc: [
            Rs {
                bs: 75,
                dw: 47,
                ns: 4,
            },
            Rs {
                bs: 151,
                dw: 121,
                ns: 4,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 16,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 11,
            },
        ],
    },
    VersionDb {
        total: 1474,
        apat: [6, 28, 54, 80, 106, 0, 0],
        ecc: [
            Rs {
                bs: 73,
                dw: 45,
                ns: 6,
            },
            Rs {
                bs: 147,
                dw: 117,
                ns: 6,
            },
            Rs {
                bs: 46,
                dw: 16,
                ns: 30,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 11,
            },
        ],
    },
    VersionDb {
        total: 1588,
        apat: [6, 32, 58, 84, 110, 0, 0],
        ecc: [
            Rs {
                bs: 75,
                dw: 47,
                ns: 8,
            },
            Rs {
                bs: 132,
                dw: 106,
                ns: 8,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 22,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 7,
            },
        ],
    },
    VersionDb {
        total: 1706,
        apat: [6, 30, 58, 86, 114, 0, 0],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 19,
            },
            Rs {
                bs: 142,
                dw: 114,
                ns: 10,
            },
            Rs {
                bs: 46,
                dw: 16,
                ns: 33,
            },
            Rs {
                bs: 50,
                dw: 22,
                ns: 28,
            },
        ],
    },
    VersionDb {
        total: 1828,
        apat: [6, 34, 62, 90, 118, 0, 0],
        ecc: [
            Rs {
                bs: 73,
                dw: 45,
                ns: 22,
            },
            Rs {
                bs: 152,
                dw: 122,
                ns: 8,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 12,
            },
            Rs {
                bs: 53,
                dw: 23,
                ns: 8,
            },
        ],
    },
    VersionDb {
        total: 1921,
        apat: [6, 26, 50, 74, 98, 122, 0],
        ecc: [
            Rs {
                bs: 73,
                dw: 45,
                ns: 3,
            },
            Rs {
                bs: 147,
                dw: 117,
                ns: 3,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 11,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 4,
            },
        ],
    },
    VersionDb {
        total: 2051,
        apat: [6, 30, 54, 78, 102, 126, 0],
        ecc: [
            Rs {
                bs: 73,
                dw: 45,
                ns: 21,
            },
            Rs {
                bs: 146,
                dw: 116,
                ns: 7,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 19,
            },
            Rs {
                bs: 53,
                dw: 23,
                ns: 1,
            },
        ],
    },
    VersionDb {
        total: 2185,
        apat: [6, 26, 52, 78, 104, 130, 0],
        ecc: [
            Rs {
                bs: 75,
                dw: 47,
                ns: 19,
            },
            Rs {
                bs: 145,
                dw: 115,
                ns: 5,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 23,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 15,
            },
        ],
    },
    VersionDb {
        total: 2323,
        apat: [6, 30, 56, 82, 108, 134, 0],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 2,
            },
            Rs {
                bs: 145,
                dw: 115,
                ns: 13,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 23,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 42,
            },
        ],
    },
    VersionDb {
        total: 2465,
        apat: [6, 34, 60, 86, 112, 138, 0],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 10,
            },
            Rs {
                bs: 145,
                dw: 115,
                ns: 17,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 19,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 10,
            },
        ],
    },
    VersionDb {
        total: 2611,
        apat: [6, 30, 58, 86, 114, 142, 0],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 14,
            },
            Rs {
                bs: 145,
                dw: 115,
                ns: 17,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 11,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 29,
            },
        ],
    },
    VersionDb {
        total: 2761,
        apat: [6, 34, 62, 90, 118, 146, 0],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 14,
            },
            Rs {
                bs: 145,
                dw: 115,
                ns: 13,
            },
            Rs {
                bs: 46,
                dw: 16,
                ns: 59,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 44,
            },
        ],
    },
    VersionDb {
        total: 2876,
        apat: [6, 30, 54, 78, 102, 126, 150],
        ecc: [
            Rs {
                bs: 75,
                dw: 47,
                ns: 12,
            },
            Rs {
                bs: 151,
                dw: 121,
                ns: 12,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 22,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 39,
            },
        ],
    },
    VersionDb {
        total: 3034,
        apat: [6, 24, 50, 76, 102, 128, 154],
        ecc: [
            Rs {
                bs: 75,
                dw: 47,
                ns: 6,
            },
            Rs {
                bs: 151,
                dw: 121,
                ns: 6,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 2,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 46,
            },
        ],
    },
    VersionDb {
        total: 3196,
        apat: [6, 28, 54, 80, 106, 132, 158],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 29,
            },
            Rs {
                bs: 152,
                dw: 122,
                ns: 17,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 24,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 49,
            },
        ],
    },
    VersionDb {
        total: 3362,
        apat: [6, 32, 58, 84, 110, 136, 162],
        ecc: [
            Rs {
                bs: 74,
                dw: 46,
                ns: 13,
            },
            Rs {
                bs: 152,
                dw: 122,
                ns: 4,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 42,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 48,
            },
        ],
    },
    VersionDb {
        total: 3532,
        apat: [6, 26, 54, 82, 110, 138, 166],
        ecc: [
            Rs {
                bs: 75,
                dw: 47,
                ns: 40,
            },
            Rs {
                bs: 147,
                dw: 117,
                ns: 20,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 10,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 43,
            },
        ],
    },
    VersionDb {
        total: 3706,
        apat: [6, 30, 58, 86, 114, 142, 170],
        ecc: [
            Rs {
                bs: 75,
                dw: 47,
                ns: 18,
            },
            Rs {
                bs: 148,
                dw: 118,
                ns: 19,
            },
            Rs {
                bs: 45,
                dw: 15,
                ns: 20,
            },
            Rs {
                bs: 54,
                dw: 24,
                ns: 34,
            },
        ],
    },
];

/// Format-bits index for the table lookup (M=0 · L=1 · H=2 · Q=3).
fn format_bits_index(ec: EcLevel) -> usize {
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
fn zigzag_positions(version: usize) -> Vec<(usize, usize)> {
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
fn unmask_to_codewords(masked: &[u8], bit_len: usize, version: usize, mask: u8) -> Option<Vec<u8>> {
    let total = VERSIONS[version].total;
    if bit_len < total * 8 {
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
struct Block {
    bytes: Vec<u8>,
    npar: usize,
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
fn deinterleave(codewords: &[u8], version: usize, ec_index: usize) -> Vec<Block> {
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
        for j in 0..dw {
            let idx = if j < small.dw {
                // rounds where every block participates
                j * block_count + i
            } else {
                // the extra round: only large blocks emit, in block order
                small.dw * block_count + (i - small.ns)
            };
            bytes.push(codewords[idx]);
        }
        for j in 0..(bs - dw) {
            bytes.push(codewords[ecc_offset + j * block_count + i]);
        }
        blocks.push(Block {
            bytes,
            npar: bs - dw,
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

fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    let idx = (usize::from(GF_LOG[a as usize]) + usize::from(GF_LOG[b as usize])) % 255;
    GF_EXP[idx]
}

/// α^n.
fn gf_pow(n: usize) -> u8 {
    GF_EXP[n % 255]
}

/// RS syndromes `S_i = c(α^i)` for `i ∈ 0..npar`, with `block[len−1−j]`
/// the coefficient of `x^j` (transmission order) — rqrr convention.
fn syndromes(block: &[u8], npar: usize) -> Vec<u8> {
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

fn gf_inv(a: u8) -> u8 {
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
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new()).unwrap();
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
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new()).unwrap();
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
        let damaged = crate::engine::decode_all(&flipped, crate::engine::EngineOptions::default());
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
        let outcome = ladder::run(&planes, &ScanConfig::full(), &CancelToken::new()).unwrap();
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
        let damaged = crate::engine::decode_all(&flipped, crate::engine::EngineOptions::default());
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
