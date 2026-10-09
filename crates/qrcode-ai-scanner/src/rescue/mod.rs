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
//!
//! The codeword half of this module (unmask → de-interleave → RS → segment
//! parse) is also the scanner's own view of streams the engines DID decode:
//! [`rederive`] recovers the ECI structure rqrr discards, and
//! [`version_of_data`] lets rxing's codeword output be parsed.

pub(crate) mod bitstream;
mod ee;

use std::sync::OnceLock;

use crate::engine::MaskedStream;
use crate::engine::content::{EciMark, Fnc1};
use crate::input::LumaImage;
use crate::matrix::deinterleave::{Block, deinterleave};
use crate::matrix::sampler::GridSampler;
use crate::matrix::version_db::format_bits_index;
use crate::matrix::zigzag::{unmask_to_codewords, zigzag_positions};
use crate::report::{EcLevel, Point};
use bitstream::{Parsed, PortRead};

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
    /// The stream was read from the transposed grid (a mirrored symbol);
    /// the corners stay the detected grid's, so the rescue samples module
    /// margins transposed too.
    pub transposed: bool,
}

/// A successful rescue — feeds the normal merge/report path.
#[derive(Debug, Clone)]
pub(crate) struct Rescued {
    /// Payload bytes, ECI designators excluded.
    pub raw: Vec<u8>,
    /// The FNC1 position the stream declared.
    pub fnc1: Fnc1,
    /// The ECI designators the stream declared, with their payload offsets.
    pub eci: Vec<EciMark>,
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
        // a transposed stream's module (row y, column x) is the detected
        // grid's (row x, column y)
        let (column, row) = if candidate.transposed { (y, x) } else { (x, y) };
        all.push(sampler.module(column as i32, row as i32)?);
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

/// Attempt the rescue. `luma` and the candidate's corners must share
/// the same coordinate space.
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
    let data = corrected_data(&codewords, candidate.version, candidate.ec, p, |block| {
        let len = block.bytes.len();
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
        let budget = block.npar.saturating_sub(p).saturating_sub(1);
        ranked
            .iter()
            .take(budget)
            .filter(|(_, margin)| *margin < 0.30)
            .map(|(pos, _)| len - 1 - pos) // byte index → power position
            .collect()
    })?;

    let parsed = bitstream::parse(&data, candidate.version)?;
    // The rescue publishes only streams it fully accounts for: a structured-
    // append part or a Hanzi segment stays refused here, exactly as before
    // the shared parser learned them (the engines read those symbols when
    // they can).
    if parsed.bytes.is_empty() || parsed.structured_append || parsed.hanzi {
        return None;
    }
    Some(Rescued {
        raw: parsed.bytes,
        fnc1: parsed.fnc1,
        eci: parsed.eci,
    })
}

/// RS-correct every block and concatenate its data codewords — the data
/// stream of ISO 18004 §8.6. `erasures_for` marks each block's erasure
/// power positions; `reserved` is the Annex B misdecode-protection count
/// the acceptance keeps out of the correction budget. `None` when a block
/// is beyond capacity or past that budget (`2t + e + reserved ≤ npar`).
fn corrected_data(
    codewords: &[u8],
    version: u8,
    ec: EcLevel,
    reserved: usize,
    mut erasures_for: impl FnMut(&Block) -> Vec<usize>,
) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    for block in deinterleave(codewords, usize::from(version), format_bits_index(ec)) {
        let erasures = erasures_for(&block);
        let npar = block.npar;
        let mut bytes = block.bytes;
        let (errors, used) = ee::correct(&mut bytes, npar, &erasures)?;
        if 2 * errors + used + reserved > npar {
            return None; // Annex B guard
        }
        bytes.truncate(bytes.len().saturating_sub(npar));
        data.extend_from_slice(&bytes);
    }
    Some(data)
}

/// Who accepts a re-derived stream — it sets the RS budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Acceptance {
    /// The scanner itself (a rescue candidate no engine decoded): the
    /// ISO/IEC 18004 Annex B misdecode-protection codewords stay reserved,
    /// `2t + p ≤ npar` — the guard of [`attempt`].
    Scanner,
    /// An engine already decoded these codewords with plain RS (rqrr): the
    /// re-derivation corrects within `2t ≤ npar` too — only `p` is relaxed
    /// (v1-L 3, v1-M 2, v1-Q/H 1, v2-L 1, v3-L 1 codewords). Sound for the
    /// orientation the engine decoded, and for no other: a block is within
    /// `⌊npar/2⌋` of at most one codeword, which rqrr's decoder (Berlekamp-
    /// Massey, then a syndrome re-check) takes too, so a stream verifying
    /// in that orientation reproduces the codewords the engine accepted and
    /// the misdecode risk Annex B reserves against is the engine's, already
    /// taken. Which orientation that was is checked, never assumed (the rqrr
    /// adapter's `settle`): a grid verifying both ways stays unverified.
    Engine,
}

/// What re-deriving a sampled stream found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rederived {
    /// The RS-verified data codewords, parsed.
    Parsed(Parsed),
    /// The codewords verify, but the segment stream is not one ISO 18004
    /// defines (a `111xxxxx` designator, an assignment number past
    /// 999999, FNC1 in both positions…) — refuse the symbol, as zbar does.
    /// `port`: what rxing's port reports for it when that reader tolerates
    /// the violation — the content a refusal vetoes.
    Malformed {
        /// See [`bitstream::port_read`].
        port: Option<PortRead>,
    },
    /// No verified codewords: a stream shorter than its version, a block
    /// beyond RS capacity or past the acceptance's budget — e.g. the
    /// unmirrored read of a symbol the engine decoded mirrored.
    Unverified,
}

/// Re-derive the segment structure of a sampled stream — the clean path of
/// [`attempt`]: unmask · de-interleave · RS with errors only (no erasure
/// confidence) under `acceptance`'s budget · the shared parser.
pub(crate) fn rederive(
    stream: &MaskedStream,
    version: u8,
    ec: EcLevel,
    mask: u8,
    acceptance: Acceptance,
) -> Rederived {
    if !(1..=40).contains(&version) {
        return Rederived::Unverified;
    }
    let Some(codewords) =
        unmask_to_codewords(&stream.bits, stream.bit_len, usize::from(version), mask)
    else {
        return Rederived::Unverified;
    };
    let reserved = match acceptance {
        Acceptance::Scanner => protection(version, ec),
        Acceptance::Engine => 0,
    };
    let Some(data) = corrected_data(&codewords, version, ec, reserved, |_| Vec::new()) else {
        return Rederived::Unverified;
    };
    match bitstream::parse(&data, version) {
        Some(parsed) => Rederived::Parsed(parsed),
        None => Rederived::Malformed {
            port: bitstream::port_read(&data, version),
        },
    }
}

/// The QR version whose RS-corrected data stream under `ec` is exactly
/// `len` codewords. rxing's general reader returns that stream without
/// its version; at a fixed EC level the data capacity strictly grows with
/// the version, so the length identifies it (and with it the character-
/// count widths the parser needs).
pub(crate) fn version_of_data(len: usize, ec: EcLevel) -> Option<u8> {
    let column = format_bits_index(ec);
    let table = data_capacity();
    (1..=40u8).find(|&version| table[usize::from(version)][column] == len)
}

/// Data codewords per `[version][format-bits EC index]`, from the same
/// block table the de-interleave uses (built once: the shapes are all it
/// needs, so a zero symbol of the largest size stands in for codewords).
fn data_capacity() -> &'static [[usize; 4]; 41] {
    static TABLE: OnceLock<[[usize; 4]; 41]> = OnceLock::new();
    TABLE.get_or_init(|| {
        const LARGEST_SYMBOL: usize = 3706; // version 40 codewords
        let zeros = vec![0u8; LARGEST_SYMBOL];
        let mut table = [[0; 4]; 41];
        for (version, row) in table.iter_mut().enumerate().skip(1) {
            for (column, cell) in row.iter_mut().enumerate() {
                *cell = deinterleave(&zeros, version, column)
                    .iter()
                    .map(|block| block.bytes.len() - block.npar)
                    .sum();
            }
        }
        table
    })
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
    use crate::engine::content::{self, EciEvidence};

    /// Parse data codewords into payload + designators, then read the
    /// payload under them — and, separately, the raw input as unmarked and
    /// as signalled-only content (the interpretation's other attacker-shaped
    /// entries). Never panics: refusal is `None`, a reading is total.
    pub(crate) fn parse_bitstream(data: &[u8], version: u8) {
        if let Some(parsed) = super::bitstream::parse(data, version) {
            let _ = content::read(&parsed.bytes, &EciEvidence::from_marks(parsed.eci));
        }
        let _ = super::bitstream::port_read(data, version);
        let _ = content::read(data, &EciEvidence::NoEci);
        let _ = content::read(data, &EciEvidence::Signalled);
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
                transposed: false,
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
                    transposed: false,
                };
                let rescued = attempt(luma, &candidate)
                    .unwrap_or_else(|| panic!("rescue pipeline failed on clean {path:?}"));
                assert_eq!(
                    rescued.raw, engine_raw,
                    "byte divergence vs engine on {path:?}"
                );
                // the clean re-derivation (no erasures) is the same path
                match rederive(
                    &candidate.stream,
                    candidate.version,
                    candidate.ec,
                    candidate.mask,
                    Acceptance::Engine,
                ) {
                    Rederived::Parsed(parsed) => assert_eq!(
                        parsed.bytes, engine_raw,
                        "re-derivation diverges on {path:?}"
                    ),
                    other => panic!("re-derivation failed on clean {path:?}: {other:?}"),
                }
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
pub(crate) mod test_symbols {
    //! Symbols for codeword-level tests — rendered from an explicit segment
    //! stream (the `qrcode` Bits API) or from explicit DATA CODEWORDS (the
    //! encoder's own RS, placement and masking around any bit stream,
    //! including ones its Bits API refuses to write, like a `111xxxxx`
    //! designator).
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use crate::input::LumaImage;

    /// 6 px modules, 4-module quiet zone.
    pub(crate) fn render(colors: &[qrcode::Color], width: usize) -> LumaImage {
        const SCALE: usize = 6;
        const QUIET: usize = 4;
        let side = (width + 2 * QUIET) * SCALE;
        let mut data = vec![255u8; side * side];
        for (i, color) in colors.iter().enumerate() {
            if *color == qrcode::Color::Dark {
                let (mx, my) = (i % width + QUIET, i / width + QUIET);
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        data[(my * SCALE + dy) * side + mx * SCALE + dx] = 0;
                    }
                }
            }
        }
        let side = u32::try_from(side).unwrap();
        LumaImage::new(data, side, side)
    }

    /// Module colors mirrored left-right (module x becomes width-1-x).
    pub(crate) fn mirror(colors: &[qrcode::Color], width: usize) -> Vec<qrcode::Color> {
        (0..width * width)
            .map(|i| colors[(i / width) * width + (width - 1 - i % width)])
            .collect()
    }

    fn segments(
        version: qrcode::Version,
        ec: qrcode::EcLevel,
        push: impl FnOnce(&mut qrcode::bits::Bits) -> qrcode::types::QrResult<()>,
    ) -> qrcode::QrCode {
        let mut bits = qrcode::bits::Bits::new(version);
        push(&mut bits).unwrap();
        bits.push_terminator(ec).unwrap();
        qrcode::QrCode::with_bits(bits, ec).unwrap()
    }

    /// A symbol from an explicit segment stream (terminator + padding added).
    pub(crate) fn from_segments(
        version: qrcode::Version,
        ec: qrcode::EcLevel,
        push: impl FnOnce(&mut qrcode::bits::Bits) -> qrcode::types::QrResult<()>,
    ) -> LumaImage {
        let code = segments(version, ec, push);
        render(&code.to_colors(), code.width())
    }

    /// [`from_segments`], left-right mirrored.
    pub(crate) fn from_segments_mirrored(
        version: qrcode::Version,
        ec: qrcode::EcLevel,
        push: impl FnOnce(&mut qrcode::bits::Bits) -> qrcode::types::QrResult<()>,
    ) -> LumaImage {
        let code = segments(version, ec, push);
        render(&mirror(&code.to_colors(), code.width()), code.width())
    }

    /// A symbol whose data codewords are exactly `data`, padded with the
    /// ISO 18004 pad codewords (0xEC, 0x11) to the version's capacity.
    pub(crate) fn from_codewords(
        version: qrcode::Version,
        ec: qrcode::EcLevel,
        data: &[u8],
    ) -> LumaImage {
        from_corrupted_codewords(version, ec, data, &[])
    }

    /// [`from_codewords`], left-right mirrored.
    pub(crate) fn from_codewords_mirrored(
        version: qrcode::Version,
        ec: qrcode::EcLevel,
        data: &[u8],
    ) -> LumaImage {
        let (colors, width) = codeword_colors(version, ec, data, &[]);
        render(&mirror(&colors, width), width)
    }

    /// [`from_codewords`] with the codewords at `corrupt` (indices into the
    /// encoder's data codewords, then its EC codewords) inverted AFTER the
    /// RS parity was computed — errors every decoder must correct.
    pub(crate) fn from_corrupted_codewords(
        version: qrcode::Version,
        ec: qrcode::EcLevel,
        data: &[u8],
        corrupt: &[usize],
    ) -> LumaImage {
        let (colors, width) = codeword_colors(version, ec, data, corrupt);
        render(&colors, width)
    }

    fn codeword_colors(
        version: qrcode::Version,
        ec: qrcode::EcLevel,
        data: &[u8],
        corrupt: &[usize],
    ) -> (Vec<qrcode::Color>, usize) {
        let capacity = qrcode::bits::Bits::new(version).max_len(ec).unwrap() / 8;
        let mut padded = data.to_vec();
        let mut pads = [0xEC, 0x11].into_iter().cycle();
        while padded.len() < capacity {
            padded.push(pads.next().unwrap());
        }
        let (mut data_codewords, mut ec_codewords) =
            qrcode::ec::construct_codewords(&padded, version, ec).unwrap();
        for &index in corrupt {
            match index.checked_sub(data_codewords.len()) {
                None => data_codewords[index] ^= 0xFF,
                Some(parity) => ec_codewords[parity] ^= 0xFF,
            }
        }
        let mut canvas = qrcode::canvas::Canvas::new(version, ec);
        canvas.draw_all_functional_patterns();
        canvas.draw_data(&data_codewords, &ec_codewords);
        let width = usize::try_from(version.width()).unwrap();
        (canvas.apply_best_mask().into_colors(), width)
    }

    /// Hand-assembled MSB-first bit string → bytes (zero-padded to a byte).
    pub(crate) fn stream(bits: &str) -> Vec<u8> {
        let clean: Vec<u8> = bits.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        clean
            .chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0u8, |byte, (i, &b)| byte | (u8::from(b == b'1') << (7 - i)))
            })
            .collect()
    }
}

#[cfg(test)]
mod codewords {
    //! The shared codeword path on symbols an engine never misreads: the
    //! clean re-derivation, the rescue's view of FNC1 symbols rqrr refuses,
    //! and the data-capacity table rxing's general reader needs.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::test_symbols::{from_codewords, from_corrupted_codewords, from_segments, stream};
    use super::*;
    use crate::engine::content::EciMark;

    /// Every grid rqrr detects: its own decode result + the rescue input.
    fn grids(luma: &LumaImage) -> Vec<(Option<Vec<u8>>, RescueCandidate)> {
        let width = luma.width() as usize;
        let data = luma.data();
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
                data[y * width + x]
            });
        prepared
            .detect_grids()
            .into_iter()
            .map(|grid| {
                let mut decoded = Vec::new();
                let engine = grid.decode_to(&mut decoded).ok().map(|_| decoded);
                let (meta, raw_data) = grid.get_raw_data().unwrap();
                let candidate = RescueCandidate {
                    stream: MaskedStream {
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
                    transposed: false,
                };
                (engine, candidate)
            })
            .collect()
    }

    fn rederived(candidate: &RescueCandidate, acceptance: Acceptance) -> Rederived {
        rederive(
            &candidate.stream,
            candidate.version,
            candidate.ec,
            candidate.mask,
            acceptance,
        )
    }

    #[test]
    fn rederivation_recovers_the_declaration_rqrr_discards() {
        let luma = from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
            b.push_eci_designator(4)?;
            b.push_byte_data(&[0xA1])
        });
        let grids = grids(&luma);
        assert_eq!(grids.len(), 1);
        let (engine, candidate) = &grids[0];
        assert_eq!(engine.as_deref(), Some(&[0xA1][..]), "rqrr: bytes only");
        let Rederived::Parsed(parsed) = rederived(candidate, Acceptance::Engine) else {
            panic!("a clean stream re-derives");
        };
        assert_eq!(parsed.bytes, vec![0xA1], "same bytes as rqrr");
        assert_eq!(parsed.eci, vec![EciMark { value: 4, start: 0 }]);
    }

    #[test]
    fn a_malformed_designator_is_refused_by_the_rederivation_too() {
        // ECI with a 111xxxxx first codeword, then byte "abc": rqrr reads
        // the designator as one byte and decodes "abc"; the shared parser —
        // here behind real RS-verified codewords — refuses the stream, and
        // so does rxing's port: no read to veto.
        let data = stream("0111 11100000 0100 00000011 01100001 01100010 01100011 0000");
        let luma = from_codewords(qrcode::Version::Normal(1), qrcode::EcLevel::M, &data);
        let found = grids(&luma);
        assert_eq!(found.len(), 1);
        let (engine, candidate) = &found[0];
        assert_eq!(engine.as_deref(), Some(&b"abc"[..]), "rqrr guesses");
        assert_eq!(
            rederived(candidate, Acceptance::Engine),
            Rederived::Malformed { port: None }
        );
        // above the register, the port still signals a designator and reads
        // "abc": that content is the veto
        let data =
            stream("0111 11001111 01000010 01000000 0100 00000011 01100001 01100010 01100011 0000");
        let luma = from_codewords(qrcode::Version::Normal(1), qrcode::EcLevel::M, &data);
        assert_eq!(
            rederived(&grids(&luma)[0].1, Acceptance::Engine),
            Rederived::Malformed {
                port: Some(PortRead {
                    bytes: b"abc".to_vec(),
                    signalled: true,
                    fnc1: Fnc1::None,
                })
            },
            "an assignment number past 999999 is malformed"
        );
        // control: the same stream with a well-formed designator (ECI 4)
        let data = stream("0111 00000100 0100 00000011 01100001 01100010 01100011 0000");
        let luma = from_codewords(qrcode::Version::Normal(1), qrcode::EcLevel::M, &data);
        let found = grids(&luma);
        let Rederived::Parsed(parsed) = rederived(&found[0].1, Acceptance::Engine) else {
            panic!("a well-formed designator re-derives");
        };
        assert_eq!(parsed.bytes, b"abc");
        assert_eq!(parsed.eci, vec![EciMark { value: 4, start: 0 }]);
    }

    #[test]
    fn a_stream_beyond_rs_capacity_is_unverified_not_malformed() {
        // a tampered sampled stream (every codeword bit flipped) proves
        // nothing: the re-derivation declares nothing from it
        let luma = from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
            b.push_eci_designator(4)?;
            b.push_byte_data(&[0xA1])
        });
        let mut candidate = grids(&luma).remove(0).1;
        for byte in &mut candidate.stream.bits {
            *byte = !*byte;
        }
        for acceptance in [Acceptance::Scanner, Acceptance::Engine] {
            assert!(matches!(
                rederived(&candidate, acceptance),
                Rederived::Unverified
            ));
        }
        // and a version outside the table never reaches the parser
        candidate.version = 41;
        assert!(matches!(
            rederived(&candidate, Acceptance::Engine),
            Rederived::Unverified
        ));
    }

    #[test]
    fn the_annex_b_reserve_binds_the_scanners_acceptance_not_an_engines() {
        // v1-M (10 parity codewords, p = 2), ECI 4 + byte A1, five codewords
        // corrupted: plain RS corrects them (rqrr decodes), but 2·5 + 2 > 10
        let data = stream("0111 00000100 0100 00000001 10100001 0000");
        let corrupt = [6, 8, 10, 12, 14];
        let luma = from_corrupted_codewords(
            qrcode::Version::Normal(1),
            qrcode::EcLevel::M,
            &data,
            &corrupt,
        );
        let (engine, candidate) = grids(&luma).remove(0);
        assert_eq!(engine.as_deref(), Some(&[0xA1][..]), "rqrr corrects five");
        assert!(
            matches!(
                rederived(&candidate, Acceptance::Scanner),
                Rederived::Unverified
            ),
            "the scanner's own acceptance keeps the reserve"
        );
        let Rederived::Parsed(parsed) = rederived(&candidate, Acceptance::Engine) else {
            panic!("rqrr's acceptance: plain RS");
        };
        assert_eq!(parsed.bytes, vec![0xA1]);
        assert_eq!(parsed.eci, vec![EciMark { value: 4, start: 0 }]);
        // four errors fit the reserve: 2·4 + 2 = 10
        let luma = from_corrupted_codewords(
            qrcode::Version::Normal(1),
            qrcode::EcLevel::M,
            &data,
            &corrupt[..4],
        );
        let (_, candidate) = grids(&luma).remove(0);
        assert!(matches!(
            rederived(&candidate, Acceptance::Scanner),
            Rederived::Parsed(_)
        ));
    }

    /// A grid with x and y swapped — how a mirrored symbol reads right.
    struct Transposed<'g, G>(&'g G);

    impl<G: rqrr::BitGrid> rqrr::BitGrid for Transposed<'_, G> {
        fn size(&self) -> usize {
            self.0.size()
        }

        fn bit(&self, y: usize, x: usize) -> bool {
            self.0.bit(x, y)
        }
    }

    /// A v2-M symbol whose every module has its own luma (dark 10-69,
    /// light 190-249, by module index), mirrored or not: margins differ
    /// from codeword to codeword, and follow the module wherever it lands.
    fn graded(mirrored: bool) -> LumaImage {
        const SCALE: usize = 6;
        const QUIET: usize = 4;
        let code = qrcode::QrCode::with_version(
            b"graded margins",
            qrcode::Version::Normal(2),
            qrcode::EcLevel::M,
        )
        .unwrap();
        let width = code.width();
        let colors = code.to_colors();
        let side = (width + 2 * QUIET) * SCALE;
        let mut data = vec![255u8; side * side];
        for (i, color) in colors.iter().enumerate() {
            let (row, column) = (i / width, i % width);
            let luma = if *color == qrcode::Color::Dark {
                10 + (i * 7) % 60
            } else {
                190 + (i * 5) % 60
            };
            let x = if mirrored { width - 1 - column } else { column };
            for dy in 0..SCALE {
                for dx in 0..SCALE {
                    data[((row + QUIET) * SCALE + dy) * side + (x + QUIET) * SCALE + dx] =
                        u8::try_from(luma).unwrap();
                }
            }
        }
        let side = u32::try_from(side).unwrap();
        LumaImage::new(data, side, side)
    }

    #[test]
    fn a_transposed_candidate_samples_its_margins_transposed() {
        // the mirrored render's transposed stream is the unmirrored symbol's
        // stream: its S5 margins must be the unmirrored candidate's, value
        // for value — and reading them untransposed measures other modules
        let upright = graded(false);
        let (_, sampled) = grids(&upright).remove(0);
        let codewords = 44; // v2: 44 codewords
        let expected = codeword_margins(&upright, &sampled, codewords).unwrap();
        let mirrored = graded(true);
        let width = mirrored.width() as usize;
        let data = mirrored.data();
        let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(
            width,
            mirrored.height() as usize,
            |x, y| data[y * width + x],
        );
        let grid = prepared.detect_grids().remove(0);
        let (meta, raw_data) = rqrr::Grid::new(Transposed(&grid.grid))
            .get_raw_data()
            .unwrap();
        let mut transposed = RescueCandidate {
            stream: MaskedStream {
                bits: raw_data.data[..raw_data.len.div_ceil(8)].to_vec(),
                bit_len: raw_data.len,
            },
            corners: grid.bounds.map(|p| Point {
                x: p.x as f32,
                y: p.y as f32,
            }),
            version: u8::try_from(meta.version.0).unwrap(),
            ec: sampled.ec,
            mask: u8::try_from(meta.mask).unwrap(),
            inverted: false,
            transposed: true,
        };
        // the same symbol read the same way (rqrr's thresholding of graded
        // modules may flip a bit or two — the margins read the luma itself)
        assert_eq!(
            (transposed.version, transposed.mask),
            (sampled.version, sampled.mask)
        );
        let margins = codeword_margins(&mirrored, &transposed, codewords).unwrap();
        assert_eq!(margins, expected);
        transposed.transposed = false;
        let untransposed = codeword_margins(&mirrored, &transposed, codewords).unwrap();
        assert_ne!(untransposed, expected);
    }

    #[test]
    fn rescue_reads_fnc1_second_with_its_indicator_and_double_percent_once() {
        // rqrr refuses both FNC1 modes, so these are rescue inputs; the
        // rescue publishes what rxing's port and ISO 15424 transmit
        let luma = from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
            b.push_fnc1_second_position(37)?;
            b.push_byte_data(b"abc")
        });
        let (engine, candidate) = grids(&luma).remove(0);
        assert!(engine.is_none(), "rqrr refuses FNC1 symbols");
        let rescued = attempt(&luma, &candidate).expect("a clean stream rescues");
        assert_eq!(rescued.raw, b"37abc");
        assert_eq!(rescued.fnc1, Fnc1::Second, "an AIM application, not GS1");

        let luma = from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
            b.push_fnc1_first_position()?;
            b.push_alphanumeric_data(b"10AB%%C")
        });
        let (_, candidate) = grids(&luma).remove(0);
        let rescued = attempt(&luma, &candidate).expect("a clean stream rescues");
        assert_eq!(rescued.raw, b"10AB%C", "%% is one literal %, never two GS");
        assert_eq!(rescued.fnc1, Fnc1::First);
    }

    #[test]
    fn data_capacity_identifies_the_version_of_a_codeword_stream() {
        // independent oracle: the encoder's own data-bit capacity per cell
        for v in 1..=40i16 {
            for (ours, theirs) in [
                (EcLevel::L, qrcode::EcLevel::L),
                (EcLevel::M, qrcode::EcLevel::M),
                (EcLevel::Q, qrcode::EcLevel::Q),
                (EcLevel::H, qrcode::EcLevel::H),
            ] {
                let bits = qrcode::bits::Bits::new(qrcode::Version::Normal(v))
                    .max_len(theirs)
                    .unwrap();
                assert_eq!(
                    version_of_data(bits / 8, ours),
                    Some(u8::try_from(v).unwrap()),
                    "v{v} {ours:?}"
                );
            }
        }
        assert_eq!(version_of_data(0, EcLevel::M), None);
        assert_eq!(version_of_data(17, EcLevel::M), None, "no v-M has 17");
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
            transposed: false,
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
