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
use crate::matrix::deinterleave::deinterleave;
use crate::matrix::sampler::GridSampler;
use crate::matrix::version_db::format_bits_index;
use crate::matrix::zigzag::{unmask_to_codewords, zigzag_positions};
use crate::report::{EcLevel, Point};

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
    let positions = zigzag_positions(version);
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

    let codewords = unmask_to_codewords(
        &candidate.stream.bits,
        candidate.stream.bit_len,
        version,
        candidate.mask,
    )?;
    let margins = codeword_margins(sample_luma, candidate, codewords.len())?;

    let p = protection(candidate.version, candidate.ec);
    let blocks = deinterleave(&codewords, version, format_bits_index(candidate.ec));

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

/// Fuzz-only handles onto the two rescue internals that own a decode over
/// attacker-shaped bytes: the errors-and-erasures RS corrector (`ee::correct`)
/// and the data-codeword bitstream parser (`bitstream::parse`). A rescued
/// stream never reaches the engines, so these are the ONLY parsers over that
/// content — both must refuse (`None`) rather than panic on garbage. Compiled
/// only under `--cfg fuzzing` (cargo-fuzz builds the graph with it); this
/// surface does not exist in any normal build.
#[cfg(fuzzing)]
pub(crate) mod fuzz_api {
    /// Parse rescued data codewords into payload bytes; `None` on any
    /// malformed structure. Never panics — see `bitstream::parse`.
    pub(crate) fn parse_bitstream(data: &[u8], version: u8) -> Option<(Vec<u8>, bool)> {
        super::bitstream::parse(data, version)
    }

    /// Errors-and-erasures correct one RS block in place over the QR field;
    /// `None` when beyond capacity or inconsistent. Never panics — see
    /// `ee::correct`.
    pub(crate) fn correct_block(
        block: &mut [u8],
        npar: usize,
        erasures: &[usize],
    ) -> Option<(usize, usize)> {
        super::ee::correct(block, npar, erasures)
    }
}

#[cfg(test)]
mod probe {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::*;
    use crate::input::{ImageInput, Limits};
    use crate::matrix::gf256::syndromes;
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
            let codewords = unmask_to_codewords(
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
            let blocks = deinterleave(&codewords, version, format_bits_index(candidate.ec));
            for (bi, block) in blocks.iter().enumerate() {
                let weak_in_block = block
                    .origins
                    .iter()
                    .filter(|&&o| margins.get(o).copied().unwrap_or(0.0) < 0.30)
                    .count();
                let synd = syndromes(&block.bytes, block.npar);
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

#[cfg(test)]
mod pins {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation
    )]

    use super::*;

    // ── protection() · ISO/IEC 18004 Annex B misdecode-protection `p` ──

    /// Independent enumeration of the Annex B `p` codewords — derived from ISO
    /// 18004 (only version-1 across all EC levels, plus v2-L and v3-L, reserve
    /// any misdecode-protection codewords), NOT re-derived from the match's own
    /// arms. Pins every relevant cell, so `protection → 0`, `protection → 1`
    /// (whole-body replacements) and any single arm deletion move a cell off
    /// its value.
    #[test]
    fn protection_matches_iso_annex_b_table() {
        let table = [
            (1u8, EcLevel::L, 3usize),
            (1, EcLevel::M, 2),
            (1, EcLevel::Q, 1),
            (1, EcLevel::H, 1),
            (2, EcLevel::L, 1),
            (2, EcLevel::M, 0),
            (2, EcLevel::Q, 0),
            (2, EcLevel::H, 0),
            (3, EcLevel::L, 1),
            (3, EcLevel::M, 0),
            (3, EcLevel::H, 0),
            (4, EcLevel::L, 0), // boundary: v4-L reserves nothing (not in 2|3)
            (10, EcLevel::L, 0),
            (40, EcLevel::H, 0),
        ];
        for (v, ec, expected) in table {
            assert_eq!(protection(v, ec), expected, "p for v{v} {ec:?}");
        }
    }

    // ── codeword_margins() · percentile threshold + capacity/flat guards ──

    const V1_MODULES: u32 = 21; // version 1 · 21×21
    const SCALE: u32 = 16;
    const DIM: u32 = (V1_MODULES + 1) * SCALE; // 22*16 = 352 (sampler denom = N+1)

    /// Build a v1 luma whose 208 data-module CENTERS sample exactly `sampled[i]`
    /// (in zigzag/stream order). Each center lands at ≈(16x+8, 16y+8) and is
    /// wrapped in a uniform 15×15 block, so bilinear sampling returns the value
    /// regardless of sub-pixel rounding. The quad = the full image, so the
    /// unit→pixel map is a pure scale (module (x,y) → ((x+.5)/22·DIM, …)).
    fn v1_luma(sampled: &[u8]) -> (LumaImage, RescueCandidate) {
        let mut data = vec![128u8; (DIM * DIM) as usize];
        let positions = zigzag_positions(1);
        for (i, &(y, x)) in positions.iter().take(sampled.len()).enumerate() {
            let cx = 16 * x as u32 + 8;
            let cy = 16 * y as u32 + 8;
            for py in (cy - 7)..=(cy + 7) {
                for px in (cx - 7)..=(cx + 7) {
                    data[(py * DIM + px) as usize] = sampled[i];
                }
            }
        }
        let candidate = RescueCandidate {
            stream: MaskedStream {
                bits: Vec::new(),
                bit_len: 0,
            },
            corners: [
                Point { x: 0.0, y: 0.0 },
                Point {
                    x: DIM as f32,
                    y: 0.0,
                },
                Point {
                    x: DIM as f32,
                    y: DIM as f32,
                },
                Point {
                    x: 0.0,
                    y: DIM as f32,
                },
            ],
            version: 1,
            ec: EcLevel::M,
            mask: 0,
            inverted: false,
        };
        (LumaImage::new(data, DIM, DIM), candidate)
    }

    /// Strictly increasing sample set (0..208): `sorted == input`, so the
    /// p2/p98 picks are pinned literals — lo = sorted[208/50]=sorted[4]=4,
    /// hi = sorted[208-1-4]=sorted[203]=203. Any percentile-index mutation
    /// (:90 `/`→`%`, :91 `-`→`/`/`+`, :91 `/`→`%`) shifts lo/hi, moving the
    /// whole margin vector. Checked against an INDEPENDENT oracle (literal
    /// threshold/half-span, never `sorted[len/50]`).
    #[test]
    fn codeword_margins_pin_percentile_threshold() {
        assert_eq!(zigzag_positions(1).len(), 208, "v1 data-module count");
        let sampled: Vec<u8> = (0..208u32).map(|i| i as u8).collect();
        let (luma, cand) = v1_luma(&sampled);
        let got = codeword_margins(&luma, &cand, 26).expect("contrasty ⇒ Some");
        assert_eq!(got.len(), 26);

        let threshold = 103.5_f32; // midpoint(4, 203)
        let half_span = 99.5_f32; // (203 - 4) / 2
        for (c, &m) in got.iter().enumerate() {
            let expected = (0..8)
                .map(|k| (f32::from(sampled[8 * c + k]) - threshold).abs() / half_span)
                .fold(f32::INFINITY, f32::min);
            assert!(
                (m - expected).abs() < 1e-4,
                "codeword {c}: got {m}, expected {expected}"
            );
        }
    }

    /// Capacity guard `positions.len() < total_codewords * 8` (:73). v1 yields
    /// 208 positions; asking for 27 codewords (216 > 208) MUST refuse. `*`→`/`
    /// (27/8=3) or `*`→`+` (27+8=35) leave 208 ≥ them, so the mutant proceeds
    /// and returns Some. The exact-fit (26 ⇒ 208) still samples.
    #[test]
    fn codeword_margins_refuses_when_positions_too_few() {
        let sampled: Vec<u8> = (0..208u32).map(|i| i as u8).collect();
        let (luma, cand) = v1_luma(&sampled);
        assert!(codeword_margins(&luma, &cand, 27).is_none());
        assert!(codeword_margins(&luma, &cand, 26).is_some());
    }

    /// Flat-symbol guard (:92 `hi - lo < 16.0`). A uniform field has span 0 <
    /// 16 ⇒ None. `<`→`==` (`0 == 16` is false) would proceed and emit margins.
    #[test]
    fn codeword_margins_refuses_flat_symbol() {
        let (luma, cand) = v1_luma(&vec![128u8; 208]);
        assert!(codeword_margins(&luma, &cand, 26).is_none());
    }

    /// Boundary of the flat guard: span EXACTLY 16 is NOT flat (`16 < 16` is
    /// false) ⇒ Some. `<`→`<=` (:92) would reject it. Multiset gives sorted[4]
    /// = 100 (lo) and sorted[203] = 116 (hi) ⇒ hi − lo = 16.
    #[test]
    fn codeword_margins_span_exactly_16_is_not_flat() {
        let mut sampled = vec![108u8; 208];
        for s in sampled.iter_mut().take(5) {
            *s = 100; // sorted[0..5] = 100 ⇒ sorted[4] = lo = 100
        }
        for s in sampled.iter_mut().skip(203) {
            *s = 116; // sorted[203..208] = 116 ⇒ hi = 116
        }
        let (luma, cand) = v1_luma(&sampled);
        assert!(codeword_margins(&luma, &cand, 26).is_some());
    }
}
