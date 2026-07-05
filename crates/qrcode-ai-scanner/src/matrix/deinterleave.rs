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
