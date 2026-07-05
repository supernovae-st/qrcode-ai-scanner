//! Reed-Solomon block de-interleave — the ISO 18004 §8.6 round-robin split of
//! the interleaved codeword stream back into per-block `data ++ ec`.
//!
//! Shared substrate: the UEC margin (`score::uec`) counts errors per block, and
//! the artistic rescue stage (`rescue`) corrects them — both start here.

use super::version_db::VERSIONS;

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
    debug_assert!(
        (1..=40).contains(&version),
        "deinterleave: QR version {version} out of 1..=40"
    );
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

#[cfg(test)]
mod tests {
    #![allow(clippy::needless_range_loop)]

    use super::*;

    /// Structural pin across ALL 160 (version, ec) cells: the de-interleave
    /// of an identity stream must be a PERMUTATION of 0..total with exactly
    /// the table's block shapes (ns small blocks of bs, the rest bs+1) and a
    /// constant npar. Kills the `large_count` divisor mutants at :35
    /// (`small.bs + 1` → `- 1` / `* 1`) that the version-table test misses —
    /// it re-derives the same formula instead of calling `deinterleave()`,
    /// and a formula duplicated into a test can never kill its own mutation.
    /// Any arithmetic slip here breaks coverage, shape, or npar on some cell
    /// with large blocks (first mixed cell: v5-Q).
    #[test]
    fn deinterleave_is_a_shaped_permutation_on_every_version_cell() {
        for version in 1..=40usize {
            let ver = &VERSIONS[version];
            for ec_index in 0..4 {
                let small = ver.ecc[ec_index];
                let large_count = (ver.total - small.bs * small.ns) / (small.bs + 1);
                #[expect(clippy::cast_possible_truncation, reason = "identity mod 256")]
                let stream: Vec<u8> = (0..ver.total).map(|i| i as u8).collect();
                let blocks = deinterleave(&stream, version, ec_index);

                assert_eq!(
                    blocks.len(),
                    small.ns + large_count,
                    "v{version} ec{ec_index}: block count"
                );
                let mut seen = vec![false; ver.total];
                for (b, block) in blocks.iter().enumerate() {
                    let expect_bs = if b < small.ns { small.bs } else { small.bs + 1 };
                    assert_eq!(
                        block.bytes.len(),
                        expect_bs,
                        "v{version} ec{ec_index} block {b}: size"
                    );
                    assert_eq!(
                        block.npar,
                        expect_bs - if b < small.ns { small.dw } else { small.dw + 1 },
                        "v{version} ec{ec_index} block {b}: npar"
                    );
                    for (&byte, &origin) in block.bytes.iter().zip(&block.origins) {
                        assert_eq!(
                            usize::from(byte),
                            origin % 256,
                            "v{version} ec{ec_index} block {b}: byte↔origin"
                        );
                        assert!(
                            !std::mem::replace(&mut seen[origin], true),
                            "v{version} ec{ec_index}: origin {origin} emitted twice"
                        );
                    }
                }
                assert!(
                    seen.iter().all(|&s| s),
                    "v{version} ec{ec_index}: stream not fully covered"
                );
            }
        }
    }
}
