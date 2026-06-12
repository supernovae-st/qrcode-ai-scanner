//! S5 rescue — erasure-aware decoding of symbols the engines gave up on.
//!
//! The artistic failure mode: a logo / texture zone corrupts more codewords
//! than plain RS tolerates (`t > ⌊(d−p)/2⌋`), but rqrr still reads the grid
//! geometry + format info (`get_raw_data` succeeds where `decode_to` dies).
//! This stage re-decodes that stream itself: per-codeword CONFIDENCE from
//! the module luma margins → low-confidence codewords become ERASURES →
//! errors-and-erasures RS (`e + 2t ≤ d − p`, twice the budget exactly where
//! the art lives) → bitstream parse. Refusal-biased at every step: a rescue
//! that cannot prove itself (syndrome re-check, structural parse) returns
//! nothing rather than a guess.
//!
//! Papers: Forney 1965 (10.1109/TIT.1965.1053825) · GMD ordering idea from
//! Forney 1966 (10.1109/TIT.1966.1053873) · center-submodule confidence per
//! Halftone QR, Chu et al. 2013 (10.1145/2508363.2508408).

mod bitstream;
mod ee;

use crate::engine::MaskedStream;
use crate::input::LumaImage;
use crate::report::{EcLevel, Point};
use crate::score::structural::GridSampler;
use crate::score::uec;

/// A grid the engines detected but could not decode — the rescue input.
#[derive(Debug, Clone)]
pub(crate) struct RescueCandidate {
    pub stream: MaskedStream,
    /// Corners in ORIGINAL image space (rescaled by the ladder).
    pub corners: [Point; 4],
    pub version: u8,
    pub ec: EcLevel,
    pub mask: u8,
    /// The geometry came from a photometrically inverting attempt.
    pub inverted: bool,
}

/// A successful rescue — feeds the normal merge/report path.
#[derive(Debug, Clone)]
pub(crate) struct Rescued {
    pub raw: Vec<u8>,
    pub fnc1: bool,
}

/// ISO/IEC 18004 Annex B misdecode-protection codewords `p` — only the low
/// versions reserve them; the erasure budget must subtract `p`.
fn protection(version: u8, ec: EcLevel) -> usize {
    match (version, ec) {
        (1, EcLevel::L) => 3,
        (1, EcLevel::M) => 2,
        (1, EcLevel::Q | EcLevel::H) | (2 | 3, EcLevel::L) => 1,
        _ => 0,
    }
}

/// Per-interleaved-codeword confidence: the worst |sample − threshold|
/// margin over the codeword's 8 modules, sampled at module CENTERS through
/// the same homography the structural checks use. Returns one margin per
/// codeword, 0.0 = the codeword crosses the threshold somewhere (prime
/// erasure candidate), larger = solid.
fn codeword_margins(
    luma: &LumaImage,
    candidate: &RescueCandidate,
    total_codewords: usize,
) -> Option<Vec<f32>> {
    let version = usize::from(candidate.version);
    let modules = u32::from(candidate.version) * 4 + 17;
    let sampler = GridSampler::new(luma, candidate.corners, modules)?;
    let positions = uec::zigzag_positions(version);
    if positions.len() < total_codewords * 8 {
        return None;
    }

    // symbol-local threshold: midpoint of the sampled extremes (percentile
    // trim for photographic noise)
    let mut all = Vec::with_capacity(total_codewords * 8);
    #[expect(
        clippy::cast_possible_wrap,
        clippy::cast_possible_truncation,
        reason = "module coords ≤ 177"
    )]
    for &(y, x) in positions.iter().take(total_codewords * 8) {
        all.push(sampler.module(x as i32, y as i32)?);
    }
    let mut sorted = all.clone();
    sorted.sort_unstable();
    let lo = f32::from(sorted[sorted.len() / 50]); // ~p2
    let hi = f32::from(sorted[sorted.len() - 1 - sorted.len() / 50]); // ~p98
    if hi - lo < 16.0 {
        return None; // flat symbol — no usable signal
    }
    let threshold = f32::midpoint(lo, hi);
    let span = hi - lo;

    let margins = all
        .chunks_exact(8)
        .map(|module_bytes| {
            module_bytes
                .iter()
                .map(|&v| (f32::from(v) - threshold).abs() / (span / 2.0))
                .fold(f32::INFINITY, f32::min)
        })
        .collect();
    Some(margins)
}

/// Attempt the rescue. `luma` is the ORIGINAL (engine-capped) plane the
/// candidate's corners live in.
pub(crate) fn attempt(luma: &LumaImage, candidate: &RescueCandidate) -> Option<Rescued> {
    let version = usize::from(candidate.version);
    if !(1..=40).contains(&version) {
        return None;
    }
    // photometric view: confidence must read the polarity the grid decoded in
    let inverted_view = candidate.inverted.then(|| crate::transform::invert(luma));
    let sample_luma = inverted_view.as_ref().unwrap_or(luma);

    let codewords = uec::unmask_to_codewords(
        &candidate.stream.bits,
        candidate.stream.bit_len,
        version,
        candidate.mask,
    )?;
    let margins = codeword_margins(sample_luma, candidate, codewords.len())?;

    let p = protection(candidate.version, candidate.ec);
    let blocks = uec::deinterleave(&codewords, version, uec::format_bits_index(candidate.ec));

    let mut data = Vec::new();
    for block in &blocks {
        let len = block.bytes.len();
        let npar = block.npar;
        // rank this block's codewords by confidence, worst first
        let mut ranked: Vec<(usize, f32)> = block
            .origins
            .iter()
            .enumerate()
            .map(|(pos, &origin)| (pos, margins.get(origin).copied().unwrap_or(0.0)))
            .collect();
        ranked.sort_by(|a, b| a.1.total_cmp(&b.1));

        // erasure budget: e ≤ npar − p − 1 (leave ≥1 slot of slack so a
        // stray unmarked error can still be located); flag only genuinely
        // weak codewords (margin under 30% of half-span)
        let budget = npar.saturating_sub(p).saturating_sub(1);
        let erasures: Vec<usize> = ranked
            .iter()
            .take(budget)
            .filter(|(_, margin)| *margin < 0.30)
            .map(|(pos, _)| len - 1 - pos) // byte index → power position
            .collect();

        let mut bytes = block.bytes.clone();
        let (errors, used) = ee::correct(&mut bytes, npar, &erasures)?;
        if 2 * errors + used + p > npar {
            return None; // Annex B guard
        }
        data.extend_from_slice(&bytes[..len - npar]);
    }

    let (raw, fnc1) = bitstream::parse(&data, candidate.version)?;
    if raw.is_empty() {
        return None;
    }
    Some(Rescued { raw, fnc1 })
}

#[cfg(test)]
mod probe {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::transform::normalize;

    #[test]
    #[ignore = "dev diagnostic, not a contract"]
    fn probe_rescue_steps_on_occluded_fixture() {
        let path = format!(
            "{}/../../fixtures/degraded/logo-occluded-rescue.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let bytes = std::fs::read(path).unwrap();
        let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
        let luma = &planes.luma;
        let width = luma.width() as usize;
        let data = luma.data();
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
                data[y * width + x]
            });
        for grid in prepared.detect_grids() {
            let (meta, raw_data) = grid.get_raw_data().unwrap();
            let candidate = RescueCandidate {
                stream: crate::engine::MaskedStream {
                    bits: raw_data.data[..raw_data.len.div_ceil(8)].to_vec(),
                    bit_len: raw_data.len,
                },
                corners: grid.bounds.map(|p| Point {
                    x: p.x as f32,
                    y: p.y as f32,
                }),
                version: u8::try_from(meta.version.0).unwrap(),
                ec: match meta.ecc_level {
                    0 => EcLevel::M,
                    1 => EcLevel::L,
                    2 => EcLevel::H,
                    _ => EcLevel::Q,
                },
                mask: u8::try_from(meta.mask).unwrap(),
                inverted: false,
            };
            println!(
                "candidate: v{} ec={:?} mask={} bit_len={}",
                candidate.version, candidate.ec, candidate.mask, candidate.stream.bit_len
            );
            let version = usize::from(candidate.version);
            let codewords = uec::unmask_to_codewords(
                &candidate.stream.bits,
                candidate.stream.bit_len,
                version,
                candidate.mask,
            )
            .expect("unmask");
            println!("codewords: {}", codewords.len());
            let margins = codeword_margins(luma, &candidate, codewords.len()).expect("margins");
            let weak = margins.iter().filter(|&&m| m < 0.30).count();
            println!("weak codewords (<0.30): {weak}/{}", margins.len());
            let blocks =
                uec::deinterleave(&codewords, version, uec::format_bits_index(candidate.ec));
            for (bi, block) in blocks.iter().enumerate() {
                let weak_in_block = block
                    .origins
                    .iter()
                    .filter(|&&o| margins.get(o).copied().unwrap_or(0.0) < 0.30)
                    .count();
                let synd = uec::syndromes(&block.bytes, block.npar);
                let dirty = synd.iter().any(|&s| s != 0);
                println!(
                    "block {bi}: len={} npar={} weak={} dirty={}",
                    block.bytes.len(),
                    block.npar,
                    weak_in_block,
                    dirty
                );
            }
            let outcome = attempt(luma, &candidate);
            println!(
                "attempt → {:?}",
                outcome.map(|r| String::from_utf8_lossy(&r.raw).into_owned())
            );
        }
    }
}

#[cfg(test)]
mod differential {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::transform::normalize;

    /// The whole rescue pipeline must agree byte-for-byte with rqrr's own
    /// decode on every clean corpus fixture — pins unmask + deinterleave +
    /// E&E (zero-correction path) + the bitstream parser in one sweep.
    #[test]
    fn rescue_pipeline_matches_engine_bytes_on_clean_corpus() {
        let dir = format!("{}/../../fixtures/clean", env!("CARGO_MANIFEST_DIR"));
        let mut checked = 0usize;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("png") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let planes = normalize(&ImageInput::encoded(&bytes), &Limits::default()).unwrap();
            let luma = &planes.luma;
            let width = luma.width() as usize;
            let data = luma.data();
            let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(
                width,
                luma.height() as usize,
                |x, y| data[y * width + x],
            );
            for grid in prepared.detect_grids() {
                let mut engine_raw = Vec::new();
                let Ok(meta) = grid.decode_to(&mut engine_raw) else {
                    continue;
                };
                let Ok((_, raw_data)) = grid.get_raw_data() else {
                    continue;
                };
                let candidate = RescueCandidate {
                    stream: crate::engine::MaskedStream {
                        bits: raw_data.data[..raw_data.len.div_ceil(8)].to_vec(),
                        bit_len: raw_data.len,
                    },
                    corners: grid.bounds.map(|p| Point {
                        x: p.x as f32,
                        y: p.y as f32,
                    }),
                    version: u8::try_from(meta.version.0).unwrap(),
                    ec: match meta.ecc_level {
                        0 => EcLevel::M,
                        1 => EcLevel::L,
                        2 => EcLevel::H,
                        _ => EcLevel::Q,
                    },
                    mask: u8::try_from(meta.mask).unwrap(),
                    inverted: false,
                };
                let rescued = attempt(luma, &candidate)
                    .unwrap_or_else(|| panic!("rescue pipeline failed on clean {path:?}"));
                assert_eq!(
                    rescued.raw, engine_raw,
                    "byte divergence vs engine on {path:?}"
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 10,
            "differential needs the corpus ({checked} symbols)"
        );
    }
}
