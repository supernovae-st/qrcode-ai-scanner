//! QR version database — total codewords · alignment centers · Reed-Solomon
//! parameters per EC level.
//!
//! Mechanically extracted from rqrr 0.10.1 `version_db.rs` (quirc heritage ·
//! Apache-2.0/MIT) — NOT hand-typed. Shared by the zigzag walk (function-
//! pattern map + capacity) and the RS de-interleave (block structure). Pinned
//! empirically by the clean-matrix test (any row error scrambles the RS
//! syndromes of that version).

use crate::report::EcLevel;

/// RS parameters for the SMALL blocks of one version × EC level.
#[derive(Debug, Clone, Copy)]
pub(super) struct Rs {
    /// Small block total size (data + ec codewords).
    pub(super) bs: usize,
    /// Small block data codewords.
    pub(super) dw: usize,
    /// Number of small blocks.
    pub(super) ns: usize,
}

/// Per-version database row.
#[derive(Debug, Clone, Copy)]
pub(super) struct VersionDb {
    /// Total codewords in the symbol.
    pub(super) total: usize,
    /// Alignment pattern center rows/columns (0-terminated).
    pub(super) apat: [usize; 7],
    /// RS parameters in FORMAT-BITS order `[M, L, H, Q]`.
    pub(super) ecc: [Rs; 4],
}

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

/// QR version database — total codewords · alignment rows · RS parameters
/// per EC level in FORMAT-BITS order `[M, L, H, Q]` (matching rqrr's
/// `ecc_level`). Index = version, entry 0 is a zero sentinel.
#[rustfmt::skip]
pub(super) const VERSIONS: [VersionDb; 41] = [
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

#[cfg(test)]
mod tests {
    #![allow(clippy::needless_range_loop)]

    use super::*;

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
