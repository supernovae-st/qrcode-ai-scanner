//! rqrr wrapper — the geometry + metadata engine (quirc lineage).
//!
//! Always through `decode_to` raw bytes: rqrr's `decode()` forces UTF-8.
//! rqrr parses ECI designators and DISCARDS them (0.10.1 and 0.11.0), so the
//! adapter re-derives the segment structure from the sampled grid with the
//! scanner's own codeword path — interpretation is OUR job, once, in
//! `engine::content`. The verified stream decides the payload bytes too:
//! rqrr's own conversion is not the truth (0.10.1 writes the Kanji values
//! 5949-5951 as Shift JIS DF FD..DF FF — `value + 0x8140 <= 0x9FFC` —
//! where ISO 18004 §7.4.6 gives 9F FD..9F FF). Which orientation verifies is
//! checked, never assumed ([`settle`]); the read publishes the stream and
//! format of that orientation, and none when no orientation decides. A grid
//! rqrr cannot decode is the rescue input in the orientation whose codewords
//! verify ([`rescue_input`]: a mirrored FNC1 symbol), decided once for at
//! most [`MAX_CANDIDATES`] grids per pass.

use super::content::{EciEvidence, EngineReading, Fnc1, Segments};
use super::locate::{Frame, LocateHint};
use super::{MaskedStream, RawDetection, Refusal};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point, Symbology};
use crate::rescue::bitstream::PortRead;
use crate::rescue::{Acceptance, MAX_CANDIDATES, Margin, Rederived, RescueCandidate, Unverified};

/// rqrr `ecc_level` is the QR format-info two-bit field: M=0 · L=1 · H=2 · Q=3.
/// Pinned empirically by the engine tests (generated at known EC levels).
fn map_ec(level: u16) -> Option<EcLevel> {
    match level {
        0 => Some(EcLevel::M),
        1 => Some(EcLevel::L),
        2 => Some(EcLevel::H),
        3 => Some(EcLevel::Q),
        _ => None,
    }
}

/// One rqrr pass over an image.
pub(super) struct Pass {
    /// Decodes the scanner publishes.
    pub(super) found: Vec<RawDetection>,
    /// Grids detected but not decoded — the S5 rescue inputs.
    pub(super) rescue: Vec<RescueCandidate>,
    /// The decodes the scanner refused: what they withhold in the ladder.
    pub(super) refused: Vec<Refusal>,
}

/// One rqrr pass. `orient_candidates`: the caller consumes the rescue
/// candidates (the ladder), so the first [`MAX_CANDIDATES`] undecodable grids
/// get their orientation decided here — once, carried in the candidate. A
/// caller that discards them (score cells, envelope probes) pays no RS for
/// them: the grid as sampled, as before.
pub(super) fn decode(luma: &LumaImage, orient_candidates: bool) -> Pass {
    let width = luma.width() as usize;
    let data = luma.data();
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
            data[y * width + x]
        });

    let mut pass = Pass {
        found: Vec::new(),
        rescue: Vec::new(),
        refused: Vec::new(),
    };
    let mut oriented = 0;
    for grid in prepared.detect_grids() {
        let mut decoded = Vec::new();
        let Ok(meta) = grid.decode_to(&mut decoded) else {
            // RS or the parser gave up — but the grid + format info may
            // still read: that stream is the S5 erasure-rescue input
            let input = if orient_candidates && oriented < MAX_CANDIDATES {
                oriented += 1;
                rescue_input(&grid)
            } else {
                sampled_input(&grid)
            };
            pass.rescue.extend(input);
            continue;
        };
        let bounds = corners(&grid);
        let (derived, orientation) = rederive_decoded(&grid, &decoded);
        let (raw, eci, fnc1, segments, parsed, margin) = match account(decoded, derived) {
            Account::Read {
                raw,
                eci,
                fnc1,
                segments,
                parsed,
                margin,
            } => (raw, eci, fnc1, segments, parsed, margin),
            Account::Refused { rqrr, port } => {
                pass.refused
                    .extend(Refusal::new(port, Some(rqrr), Some(bounds)));
                continue;
            }
        };
        // a grid that decided nothing: the re-read may still locate the
        // declarations, at rqrr's bounds and measured version
        let locate = match (&eci, map_ec(meta.ecc_level)) {
            (EciEvidence::Unobserved, Some(ec)) => Some(LocateHint {
                points: bounds,
                frame: Frame::GridBounds,
                inverted: false,
                ec,
                version: u8::try_from(meta.version.0).ok(),
                modifier: None,
            }),
            _ => None,
        };
        let mut read = RawDetection {
            symbology: Symbology::QrCode,
            raw,
            eci,
            masked_stream: None,
            corners: Some(bounds),
            version: None,
            ec: None,
            mask: None,
            fnc1,
            parsed,
            engine: EngineKind::Rqrr,
            segments,
            engine_reading: EngineReading::None,
            location: Some(bounds),
            locate,
            needs_confirmation: false,
            margin,
        };
        publish_orientation(&mut read, orientation);
        pass.found.push(read);
    }
    pass
}

/// The stream and format of the orientation that decided an rqrr read —
/// none when nothing did: the synthetic-UEC input must error-correct under
/// the version/EC/mask published beside it.
fn publish_orientation(read: &mut RawDetection, orientation: Option<Oriented>) {
    let Some((meta, raw_data)) = orientation else {
        return;
    };
    read.masked_stream = Some(masked(&raw_data));
    read.version = u8::try_from(meta.version.0).ok();
    read.ec = map_ec(meta.ecc_level);
    read.mask = u8::try_from(meta.mask).ok();
}

/// The detected grid's corners, in attempt-image pixels.
#[expect(
    clippy::cast_precision_loss,
    reason = "corner coordinates are bounded by Limits::max_dimension (10k) — exact in f32 (2^24)"
)]
fn corners<G>(grid: &rqrr::Grid<G>) -> [Point; 4] {
    grid.bounds.map(|p| Point {
        x: p.x as f32,
        y: p.y as f32,
    })
}

/// A grid rqrr detected but could not decode, read in one orientation —
/// `None` when its format information does not read there.
fn candidate<G>(
    grid: &rqrr::Grid<G>,
    read: Result<(rqrr::MetaData, rqrr::RawData), rqrr::DeQRError>,
    transposed: bool,
) -> Option<RescueCandidate> {
    let (meta, raw_data) = read.ok()?;
    Some(RescueCandidate {
        stream: masked(&raw_data),
        corners: corners(grid),
        version: u8::try_from(meta.version.0).ok()?,
        ec: map_ec(meta.ecc_level)?,
        mask: u8::try_from(meta.mask).ok()?,
        inverted: false, // stamped by the ladder per attempt
        transposed,
        rederived: None,
    })
}

/// The S5 input of a grid the caller does not re-derive: as sampled, no
/// RS spent on it.
fn sampled_input<G: rqrr::BitGrid>(grid: &rqrr::Grid<G>) -> Option<RescueCandidate> {
    candidate(grid, grid.get_raw_data(), false)
}

/// The input of a grid rqrr detected but could not decode, in the
/// orientation whose codewords error-correct (plain RS, as every
/// re-derivation outside S5 accepts them), carrying that re-derivation so
/// the ladder never repeats it. rqrr's mirrored retry decodes a mirrored
/// symbol's transpose and its parser may then refuse it (FNC1 modes): the
/// candidate must be that orientation to locate the symbol's declarations or
/// prove it malformed. A sampled grid that parses stands; one that parses
/// malformed stands only when the transpose does not error-correct to
/// another result — two different verifying orientations decide nothing
/// (no parse, no refusal). When neither verifies — the erasure case S5
/// exists for — the sampled grid stays the input. Per grid: at most one
/// extra raw read of rqrr's own grid and two clean RS passes, no image
/// sampling.
fn rescue_input<G: rqrr::BitGrid>(grid: &rqrr::Grid<G>) -> Option<RescueCandidate> {
    let rederive = |candidate: &RescueCandidate| {
        crate::rescue::rederive(
            &candidate.stream,
            candidate.version,
            candidate.ec,
            candidate.mask,
            Acceptance::Engine,
        )
    };
    let decided = |mut candidate: RescueCandidate, derived: Rederived| {
        candidate.rederived = Some(derived);
        candidate
    };
    let sampled = sampled_input(grid);
    let sampled_derived = sampled.as_ref().map(rederive);
    if matches!(sampled_derived, Some(Rederived::Parsed { .. })) {
        return sampled
            .zip(sampled_derived)
            .map(|(candidate, derived)| decided(candidate, derived));
    }
    let transposed = candidate(
        grid,
        rqrr::Grid::new(Transposed(&grid.grid)).get_raw_data(),
        true,
    );
    let transposed_derived = transposed.as_ref().map(rederive);
    match (sampled, sampled_derived, transposed, transposed_derived) {
        // malformed as sampled: it stands unless the transpose verifies to
        // another result
        (Some(sampled), Some(malformed @ Rederived::Malformed { .. }), _, other) => {
            let stands = match &other {
                None | Some(Rederived::Unverified(_)) => true,
                Some(other) => *other == malformed,
            };
            Some(decided(
                sampled,
                if stands {
                    malformed
                } else {
                    Rederived::Unverified(Unverified::Ambiguous)
                },
            ))
        }
        // the sampled grid does not verify: the transpose when it does
        (_, _, Some(transposed), Some(derived)) if !matches!(derived, Rederived::Unverified(_)) => {
            Some(decided(transposed, derived))
        }
        (sampled, _, _, _) => sampled.map(|candidate| {
            decided(
                candidate,
                Rederived::Unverified(Unverified::NothingVerifies),
            )
        }),
    }
}

/// What the scanner makes of one rqrr decode.
#[derive(Debug, PartialEq, Eq)]
enum Account {
    /// Published: the payload, what was observed of its declarations and
    /// double-byte segments, and the correction margin of its word.
    Read {
        raw: Vec<u8>,
        eci: EciEvidence,
        fnc1: Fnc1,
        segments: Segments,
        parsed: bool,
        margin: Margin,
    },
    /// Refused: the stream rqrr decoded is not one ISO 18004 defines (rqrr
    /// reads a `111xxxxx` designator as one byte, an assignment number past
    /// 999999 like any other). Carries what the refusal withholds by: rqrr's
    /// own bytes, for rqrr's reads, and what rxing's port reports.
    Refused {
        rqrr: Vec<u8>,
        port: Option<PortRead>,
    },
}

/// rqrr decoded `decoded`; the re-derivation of its grid decides.
fn account(decoded: Vec<u8>, derived: Rederived) -> Account {
    match derived {
        // the RS-verified stream of the orientation rqrr decoded ([`settle`])
        // is the truth, bytes included — rqrr's own conversion is not
        Rederived::Parsed { parsed, margin } => Account::Read {
            raw: parsed.bytes,
            eci: EciEvidence::from_marks(parsed.eci),
            fnc1: parsed.fnc1,
            segments: Segments::Observed(parsed.segments),
            parsed: true,
            margin: Margin::Known(margin),
        },
        // no orientation verifies (or both do): nothing observable of the
        // declarations; rqrr decoded the symbol, so its stream held no FNC1
        // indicator. Its word is past the scanner's bounded decoder when no
        // orientation verifies — rqrr's correction has no degree bound — so
        // a known negative margin; two orientations that verify differently
        // say nothing of it
        Rederived::Unverified(reason) => Account::Read {
            raw: decoded,
            eci: EciEvidence::Unobserved,
            fnc1: Fnc1::None,
            segments: Segments::NotObserved,
            parsed: false,
            margin: match reason {
                Unverified::NothingVerifies => Margin::Known(-1),
                Unverified::Ambiguous => Margin::Unknown,
            },
        },
        Rederived::Malformed { port } => Account::Refused {
            rqrr: decoded,
            port,
        },
    }
}

/// One orientation of rqrr's grid as read: its format information and its
/// still-masked stream.
type Oriented = (rqrr::MetaData, rqrr::RawData);

/// Which orientation of rqrr's grid decided ([`settle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// The grid as sampled.
    Sampled,
    /// Its transpose — the orientation rqrr's mirrored retry decodes.
    Transposed,
    /// Neither: no orientation verifies, or both verify to different
    /// results.
    Neither,
}

/// Re-derive what rqrr decoded: the grid as sampled and, when [`settle`]
/// needs it, its transpose (`MirroredGrid` swaps x and y). Each is unmasked
/// with the format information read from it. Returns what stands and the
/// orientation that decided it, as read — its stream and format are the
/// ones published beside the read.
fn rederive_decoded<G: rqrr::BitGrid>(
    grid: &rqrr::Grid<G>,
    decoded: &[u8],
) -> (Rederived, Option<Oriented>) {
    let derive = |read: &Option<Oriented>| {
        read.as_ref().map_or(
            Rederived::Unverified(Unverified::NothingVerifies),
            |(meta, raw_data)| rederive_sampled(meta, raw_data),
        )
    };
    let sampled = grid.get_raw_data().ok();
    let mut transposed = None;
    let (derived, decision) = settle(
        derive(&sampled),
        || {
            transposed = rqrr::Grid::new(Transposed(&grid.grid)).get_raw_data().ok();
            derive(&transposed)
        },
        decoded,
    );
    let orientation = match decision {
        Decision::Sampled => sampled,
        Decision::Transposed => transposed,
        Decision::Neither => None,
    };
    (derived, orientation)
}

/// Which re-derivation of an rqrr decode stands. The RS-verified grid is
/// the truth: a sampled stream reproducing rqrr's bytes (up to rqrr's Kanji
/// conversion) is the one it decoded; otherwise the transpose is read too.
/// When exactly one orientation verifies, its parse decides — whatever rqrr
/// wrote: a transposed parse is deliberately NOT checked against rqrr's
/// bytes (rqrr's correction has no degree bound; the scanner's bounded RS
/// is the stricter evidence), and a malformed one refuses. The same result
/// both ways is no ambiguity (a transpose-symmetric grid): it stands. Two
/// different results decide nothing (`Unverified`, ambiguous) — never a
/// refusal on ambiguity; no orientation verifying decides nothing either
/// (`Unverified`, nothing verifies). The transpose is read only when needed.
fn settle(
    sampled: Rederived,
    transposed: impl FnOnce() -> Rederived,
    decoded: &[u8],
) -> (Rederived, Decision) {
    if matches!(&sampled, Rederived::Parsed { parsed, .. } if rqrr_converts_to(&parsed.bytes, decoded))
    {
        return (sampled, Decision::Sampled);
    }
    let other = transposed();
    match (sampled, other) {
        (Rederived::Unverified(_), Rederived::Unverified(_)) => (
            Rederived::Unverified(Unverified::NothingVerifies),
            Decision::Neither,
        ),
        (Rederived::Unverified(_), other) => (other, Decision::Transposed),
        (sampled, Rederived::Unverified(_)) => (sampled, Decision::Sampled),
        // one parse both ways: the word rqrr read is either, so the worse
        // of their margins
        (
            Rederived::Parsed { parsed, margin },
            Rederived::Parsed {
                parsed: theirs,
                margin: their_margin,
            },
        ) if parsed == theirs => (
            Rederived::Parsed {
                parsed,
                margin: margin.min(their_margin),
            },
            Decision::Sampled,
        ),
        (sampled, other) if other == sampled => (sampled, Decision::Sampled),
        _ => (
            Rederived::Unverified(Unverified::Ambiguous),
            Decision::Neither,
        ),
    }
}

/// Whether rqrr's conversion of the codewords that carry `parsed` is
/// `decoded`: equal, but for the Kanji values 5949-5951 — Shift JIS 9F
/// FD..9F FF by ISO 18004 §7.4.6, DF FD..DF FF by rqrr 0.10.1.
fn rqrr_converts_to(parsed: &[u8], decoded: &[u8]) -> bool {
    parsed.len() == decoded.len()
        && parsed
            .iter()
            .zip(decoded)
            .enumerate()
            .all(|(i, (&ours, &theirs))| {
                ours == theirs
                    || ((ours, theirs) == (0x9F, 0xDF)
                        && matches!(parsed.get(i + 1), Some(0xFD..=0xFF)))
            })
}

/// One orientation through the scanner's codeword path, under rqrr's own
/// acceptance (plain RS — see [`Acceptance::Engine`]).
fn rederive_sampled(meta: &rqrr::MetaData, raw_data: &rqrr::RawData) -> Rederived {
    let (Ok(version), Some(ec), Ok(mask)) = (
        u8::try_from(meta.version.0),
        map_ec(meta.ecc_level),
        u8::try_from(meta.mask),
    ) else {
        return Rederived::Unverified(Unverified::NothingVerifies);
    };
    crate::rescue::rederive(&masked(raw_data), version, ec, mask, Acceptance::Engine)
}

/// rqrr's sampled stream, still masked.
fn masked(raw_data: &rqrr::RawData) -> MaskedStream {
    MaskedStream {
        bits: raw_data.data[..raw_data.len.div_ceil(8)].to_vec(),
        bit_len: raw_data.len,
    }
}

/// A grid with x and y swapped — what rqrr's mirrored retry reads (a
/// left-right mirrored symbol samples as a transposed one).
struct Transposed<'g, G>(&'g G);

impl<G: rqrr::BitGrid> rqrr::BitGrid for Transposed<'_, G> {
    fn size(&self) -> usize {
        self.0.size()
    }

    fn bit(&self, y: usize, x: usize) -> bool {
        self.0.bit(x, y)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::{
        Account, Decision, Transposed, account, decode, rederive_sampled, rescue_input, settle,
    };
    use crate::engine::content::{ContentKey, EciEvidence, EciMark, Fnc1, Segments};
    use crate::input::LumaImage;
    use crate::report::{EcLevel, EngineKind, Symbology};
    use crate::rescue::bitstream::{Parsed, PortRead};
    use crate::rescue::{Acceptance, Margin, Rederived, Unverified, test_symbols};

    fn parsed_bytes(bytes: &[u8]) -> Rederived {
        Rederived::Parsed {
            parsed: Parsed {
                bytes: bytes.to_vec(),
                eci: Vec::new(),
                fnc1: Fnc1::None,
                structured_append: false,
                segments: Vec::new(),
                byte_segments: Vec::new(),
            },
            margin: 0,
        }
    }

    /// No orientation error-corrects.
    fn nothing() -> Rederived {
        Rederived::Unverified(Unverified::NothingVerifies)
    }

    /// Both orientations error-correct, to different results.
    fn ambiguous() -> Rederived {
        Rederived::Unverified(Unverified::Ambiguous)
    }

    fn malformed() -> Rederived {
        Rederived::Malformed { port: None }
    }

    /// (name, sampled, transposed — `None`: must not be read, rqrr's bytes,
    /// what stands)
    type SettleCase = (
        &'static str,
        Rederived,
        Option<Rederived>,
        Vec<u8>,
        Rederived,
    );

    /// A malformed re-derivation whose tolerant reading is `bytes`.
    fn malformed_reading(bytes: &[u8]) -> Rederived {
        Rederived::Malformed {
            port: Some(PortRead {
                bytes: bytes.to_vec(),
                signalled: true,
                fnc1: Fnc1::None,
            }),
        }
    }

    /// The orientation table: how the sampled grid and its transpose settle
    /// rqrr's decode.
    fn settle_cases() -> Vec<SettleCase> {
        [one_orientation_cases(), both_orientation_cases()].concat()
    }

    /// Cases one orientation settles (or neither does).
    fn one_orientation_cases() -> Vec<SettleCase> {
        let abc: &[u8] = b"abc";
        vec![
            (
                "reproduces",
                parsed_bytes(abc),
                None,
                abc.to_vec(),
                parsed_bytes(abc),
            ),
            (
                "rqrr's Kanji conversion",
                parsed_bytes(&[0x9F, 0xFD]),
                None,
                vec![0xDF, 0xFD],
                parsed_bytes(&[0x9F, 0xFD]),
            ),
            (
                "mirrored",
                nothing(),
                Some(parsed_bytes(abc)),
                abc.to_vec(),
                parsed_bytes(abc),
            ),
            (
                "mirrored, other bytes",
                nothing(),
                Some(parsed_bytes(b"xyz")),
                abc.to_vec(),
                parsed_bytes(b"xyz"),
            ),
            (
                "mirrored malformed",
                nothing(),
                Some(malformed()),
                abc.to_vec(),
                malformed(),
            ),
            (
                "neither",
                nothing(),
                Some(nothing()),
                abc.to_vec(),
                nothing(),
            ),
        ]
    }

    /// Cases where the sampled grid verifies without reproducing rqrr's
    /// decode, so the transpose is read too.
    fn both_orientation_cases() -> Vec<SettleCase> {
        let abc: &[u8] = b"abc";
        vec![
            (
                "malformed, transpose parses",
                malformed(),
                Some(parsed_bytes(abc)),
                abc.to_vec(),
                ambiguous(),
            ),
            (
                "malformed both ways, identical",
                malformed(),
                Some(malformed()),
                abc.to_vec(),
                malformed(),
            ),
            (
                "malformed both ways, different readings",
                malformed_reading(abc),
                Some(malformed_reading(b"xyz")),
                abc.to_vec(),
                ambiguous(),
            ),
            (
                "other bytes both ways, identical",
                parsed_bytes(b"xyz"),
                Some(parsed_bytes(b"xyz")),
                abc.to_vec(),
                parsed_bytes(b"xyz"),
            ),
            (
                "malformed, transpose unverified",
                malformed(),
                Some(nothing()),
                abc.to_vec(),
                malformed(),
            ),
            (
                "other bytes, transpose parses",
                parsed_bytes(b"xyz"),
                Some(parsed_bytes(abc)),
                abc.to_vec(),
                ambiguous(),
            ),
            (
                "other bytes, transpose malformed",
                parsed_bytes(b"xyz"),
                Some(malformed()),
                abc.to_vec(),
                ambiguous(),
            ),
            (
                "other bytes alone",
                parsed_bytes(b"xyz"),
                Some(nothing()),
                abc.to_vec(),
                parsed_bytes(b"xyz"),
            ),
            (
                // 9F FC is an ordinary Kanji pair rqrr converts right
                "not rqrr's Kanji conversion",
                parsed_bytes(&[0x9F, 0xFC]),
                Some(parsed_bytes(&[0xDF, 0xFC])),
                vec![0xDF, 0xFC],
                ambiguous(),
            ),
        ]
    }

    #[test]
    fn an_ambiguous_orientation_is_unverified_never_refused() {
        // The RS-verified grid is the truth: when one orientation verifies,
        // its parse decides — whatever rqrr wrote — and a malformed one
        // refuses. Nothing is decided when no orientation verifies, or when
        // both verify with DIFFERENT results (crafted grids only); never a
        // refusal on ambiguity. The same result both ways is no ambiguity (a
        // transpose-symmetric grid). The transpose is read only when the
        // sampled stream does not reproduce rqrr's decode.
        for (name, sampled, transposed, decoded, stands) in settle_cases() {
            let consulted = transposed.is_some();
            let (settled, decision) = settle(
                sampled.clone(),
                || {
                    transposed
                        .clone()
                        .unwrap_or_else(|| panic!("{name}: the transpose is not needed"))
                },
                &decoded,
            );
            assert_eq!(settled, stands, "{name} (transpose consulted: {consulted})");
            // the orientation published beside the read is the one that
            // decided: none when nothing did
            let expected = if matches!(settled, Rederived::Unverified(_)) {
                Decision::Neither
            } else if settled == sampled {
                Decision::Sampled
            } else {
                Decision::Transposed
            };
            assert_eq!(decision, expected, "{name}");
        }
    }

    #[test]
    fn one_parse_both_ways_stands_with_the_worse_margin() {
        // the same parse of two different words: the result stands as it
        // did, and the word rqrr read may be either
        let xyz = |margin| match parsed_bytes(b"xyz") {
            Rederived::Parsed { parsed, .. } => Rederived::Parsed { parsed, margin },
            other => other,
        };
        assert_eq!(
            settle(xyz(3), || xyz(-1), b"abc"),
            (xyz(-1), Decision::Sampled)
        );
        assert_eq!(settle(xyz(-1), || xyz(3), b"abc").0, xyz(-1));
    }

    /// For the one grid rqrr detects in `luma`: rqrr's bytes, then the
    /// re-derivations of its sampled grid and of the transpose.
    fn orientations(luma: &LumaImage) -> (Vec<u8>, Rederived, Rederived) {
        let width = luma.width() as usize;
        let data = luma.data();
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
                data[y * width + x]
            });
        let grids = prepared.detect_grids();
        assert_eq!(grids.len(), 1);
        let grid = &grids[0];
        let mut decoded = Vec::new();
        grid.decode_to(&mut decoded)
            .expect("rqrr decodes the render");
        let derive = |read: Result<(rqrr::MetaData, rqrr::RawData), rqrr::DeQRError>| {
            read.map_or(nothing(), |(meta, raw_data)| {
                rederive_sampled(&meta, &raw_data)
            })
        };
        let sampled = derive(grid.get_raw_data());
        let transposed = derive(rqrr::Grid::new(Transposed(&grid.grid)).get_raw_data());
        (decoded, sampled, transposed)
    }

    #[test]
    fn orientations_verify_exactly_as_rqrr_decoded_them() {
        // differential: on real renders exactly one orientation verifies —
        // the one rqrr decoded (its mirrored retry reads the transpose) —
        // and it reproduces rqrr's bytes up to rqrr's Kanji conversion
        let eci4 = |b: &mut qrcode::bits::Bits| {
            b.push_eci_designator(4)?;
            b.push_byte_data(&[0xA1])
        };
        let kanji = |b: &mut qrcode::bits::Bits| b.push_kanji_data(&[0x9F, 0xFD]);
        let (v3, m) = (qrcode::Version::Normal(3), qrcode::EcLevel::M);
        // v3-M: 26 parity codewords, no error — a margin of 13
        let located = |bytes: &[u8]| Rederived::Parsed {
            parsed: Parsed {
                bytes: bytes.to_vec(),
                eci: vec![EciMark { value: 4, start: 0 }],
                fnc1: Fnc1::None,
                structured_append: false,
                segments: Vec::new(),
                byte_segments: std::iter::once(0..bytes.len()).collect(),
            },
            margin: 13,
        };
        let kanji_pair = || Rederived::Parsed {
            parsed: Parsed {
                bytes: vec![0x9F, 0xFD],
                eci: Vec::new(),
                fnc1: Fnc1::None,
                structured_append: false,
                segments: vec![crate::engine::content::DoubleByteSpan {
                    mode: crate::engine::content::DoubleByteMode::Kanji,
                    start: 0,
                    end: 2,
                }],
                byte_segments: Vec::new(),
            },
            margin: 13,
        };
        let cases: [(&str, LumaImage, bool, Rederived, Vec<u8>); 4] = [
            (
                "ECI 4",
                test_symbols::from_segments(v3, m, eci4),
                false,
                located(&[0xA1]),
                vec![0xA1],
            ),
            (
                "mirrored ECI 4",
                test_symbols::from_segments_mirrored(v3, m, eci4),
                true,
                located(&[0xA1]),
                vec![0xA1],
            ),
            (
                "Kanji 9F FD",
                test_symbols::from_segments(v3, m, kanji),
                false,
                kanji_pair(),
                vec![0xDF, 0xFD],
            ),
            (
                "mirrored Kanji 9F FD",
                test_symbols::from_segments_mirrored(v3, m, kanji),
                true,
                kanji_pair(),
                vec![0xDF, 0xFD],
            ),
        ];
        for (name, luma, mirrored, verified, rqrr_bytes) in cases {
            let (decoded, sampled, transposed) = orientations(&luma);
            assert_eq!(decoded, rqrr_bytes, "{name}: rqrr's own bytes");
            let (right, wrong) = if mirrored {
                (transposed, sampled)
            } else {
                (sampled, transposed)
            };
            assert_eq!(right, verified, "{name}: the orientation rqrr decoded");
            assert_eq!(wrong, nothing(), "{name}: the other one");
            let (again_decoded, sampled, transposed) = orientations(&luma);
            assert_eq!(
                settle(sampled, || transposed, &again_decoded),
                (
                    verified,
                    if mirrored {
                        Decision::Transposed
                    } else {
                        Decision::Sampled
                    }
                ),
                "{name}"
            );
        }
    }

    /// Render a QR to a luma plane (default 4-module quiet zone, 6px modules)
    /// and report the symbol version the encoder chose.
    fn render(content: &[u8], ec: qrcode::EcLevel) -> (LumaImage, u8) {
        let code = qrcode::QrCode::with_error_correction_level(content, ec).unwrap();
        let version = match code.version() {
            qrcode::Version::Normal(v) => u8::try_from(v).unwrap(),
            qrcode::Version::Micro(_) => 0,
        };
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let (w, h) = (img.width(), img.height());
        (LumaImage::new(img.into_raw(), w, h), version)
    }

    #[test]
    fn decodes_a_clean_qr_with_geometry_version_and_meta() {
        // 21-byte payload overflows v1-M (14-byte cap), so the encoder picks
        // v2+. rqrr is the geometry engine: corners/version/mask/ec are all
        // measured here (the rxing QR path leaves them None).
        let (luma, version) = render(b"https://qrc-ai.com/v2", qrcode::EcLevel::M);
        assert!(version >= 2, "want a v2+ symbol, got v{version}");
        let pass = decode(&luma, true);
        let d = pass.found.first().expect("rqrr decodes a clean QR");
        assert_eq!(d.raw, b"https://qrc-ai.com/v2");
        assert_eq!(d.symbology, Symbology::QrCode);
        assert_eq!(d.engine, EngineKind::Rqrr);
        assert_eq!(d.version, Some(version));
        assert_eq!(d.ec, Some(EcLevel::M));
        assert!(d.mask.is_some_and(|m| m <= 7), "mask {:?}", d.mask);
        assert!(d.masked_stream.is_some(), "rqrr exposes the sampled stream");
        let corners = d.corners.expect("rqrr reports corners");
        let side = luma.width() as f32;
        assert!(
            corners
                .iter()
                .all(|p| p.x >= 0.0 && p.x < side && p.y >= 0.0 && p.y < side),
            "{corners:?}"
        );
        assert_eq!(d.fnc1, Fnc1::None, "a plain byte-mode QR is not FNC1");
        assert_eq!(d.eci, EciEvidence::NoEci, "observed: no designator");
        assert!(d.parsed, "the scanner's parser read the stream");
    }

    #[test]
    fn rejects_fnc1_symbols_so_rxing_owns_gs1() {
        // rqrr's mode dispatch rejects the FNC1 indicators (0x5/0x9), so a
        // GS1 FNC1-in-first-position symbol yields NO decoded payload here —
        // the documented reason rxing is the GS1 engine. The grid is still
        // detected (it lands in the erasure-rescue vec), just never decoded.
        let mut bits = qrcode::bits::Bits::new(qrcode::Version::Normal(3));
        bits.push_fnc1_first_position().unwrap();
        bits.push_byte_data(b"0109506000134352").unwrap();
        bits.push_terminator(qrcode::EcLevel::M).unwrap();
        let code = qrcode::QrCode::with_bits(bits, qrcode::EcLevel::M).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let (w, h) = (img.width(), img.height());
        let luma = LumaImage::new(img.into_raw(), w, h);
        let pass = decode(&luma, true);
        assert!(
            pass.found.is_empty(),
            "rqrr must not decode FNC1: {:?}",
            pass.found
        );
    }

    #[test]
    fn a_blank_image_yields_no_detection_and_never_panics() {
        // No finder patterns → detect_grids finds nothing. (The panic-isolation
        // boundary itself lives in engine::run_isolated, exercised in mod.rs;
        // the adapter's own job is to return empty on a QR-less image.)
        let blank = LumaImage::new(vec![255u8; 48 * 48], 48, 48);
        let pass = decode(&blank, true);
        assert!(pass.found.is_empty() && pass.rescue.is_empty() && pass.refused.is_empty());
    }

    #[test]
    fn rqrr_decodes_carry_the_rederived_declaration() {
        // rqrr drops the ECI 4 designator; the adapter re-derives it from
        // the sampled stream, and the bytes agree with rqrr's
        let luma =
            test_symbols::from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
                b.push_numeric_data(b"12345")?;
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            });
        let pass = decode(&luma, true);
        let d = pass.found.first().expect("rqrr decodes the clean render");
        assert_eq!(d.raw, b"12345\xA1");
        assert_eq!(
            d.eci,
            EciEvidence::Located(vec![EciMark { value: 4, start: 5 }])
        );
    }

    #[test]
    fn the_verified_stream_decides_the_payload_and_its_account() {
        let parsed = |bytes: &[u8], eci: Vec<EciMark>| Rederived::Parsed {
            parsed: Parsed {
                bytes: bytes.to_vec(),
                eci,
                fnc1: Fnc1::None,
                structured_append: false,
                segments: Vec::new(),
                byte_segments: Vec::new(),
            },
            margin: 2,
        };
        let read = |raw: &[u8], eci: EciEvidence, parsed: bool, margin: Margin| Account::Read {
            raw: raw.to_vec(),
            eci,
            fnc1: Fnc1::None,
            segments: if parsed {
                Segments::Observed(Vec::new())
            } else {
                Segments::NotObserved
            },
            parsed,
            margin,
        };
        let eci4 = vec![EciMark { value: 4, start: 0 }];
        // equal bytes: the located declaration
        assert_eq!(
            account(vec![0xA1], parsed(&[0xA1], eci4.clone())),
            read(&[0xA1], EciEvidence::Located(eci4), true, Margin::Known(2))
        );
        // rqrr's own conversion differs (its Kanji inversion): the verified
        // bytes are the payload, never rqrr's
        assert_eq!(
            account(vec![0xDF, 0xFD], parsed(&[0x9F, 0xFD], Vec::new())),
            read(&[0x9F, 0xFD], EciEvidence::NoEci, true, Margin::Known(2))
        );
        // no orientation verifies: rqrr's bytes, nothing observable of the
        // declarations, and no FNC1 (rqrr decodes neither mode); rqrr took a
        // word the scanner's bounded decoder refuses — a known negative
        // margin; two orientations that verify differently say nothing of it
        assert_eq!(
            account(b"abc".to_vec(), nothing()),
            read(b"abc", EciEvidence::Unobserved, false, Margin::Known(-1))
        );
        assert_eq!(
            account(b"abc".to_vec(), ambiguous()),
            read(b"abc", EciEvidence::Unobserved, false, Margin::Unknown)
        );
        // malformed: refused, with what the refusal withholds by — rqrr's
        // own bytes for rqrr's reads, and what the port reports
        let port = PortRead {
            bytes: b"xyz".to_vec(),
            signalled: true,
            fnc1: Fnc1::None,
        };
        assert_eq!(
            account(
                b"abc".to_vec(),
                Rederived::Malformed {
                    port: Some(port.clone())
                }
            ),
            Account::Refused {
                rqrr: b"abc".to_vec(),
                port: Some(port),
            }
        );
        assert_eq!(
            account(b"abc".to_vec(), malformed()),
            Account::Refused {
                rqrr: b"abc".to_vec(),
                port: None,
            }
        );
    }

    /// rqrr's own bytes for every grid of `luma` it decodes.
    fn rqrr_bytes(luma: &LumaImage) -> Vec<Vec<u8>> {
        let width = luma.width() as usize;
        let data = luma.data();
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
                data[y * width + x]
            });
        prepared
            .detect_grids()
            .into_iter()
            .filter_map(|grid| {
                let mut raw = Vec::new();
                grid.decode_to(&mut raw).ok().map(|_| raw)
            })
            .collect()
    }

    #[test]
    fn rqrrs_kanji_inversion_yields_to_the_verified_stream() {
        // Shift JIS 9F FD is the 13-bit Kanji value 5949 (ISO 18004 §7.4.6);
        // rqrr 0.10.1 writes it DF FD, the re-derivation 9F FD
        let luma =
            test_symbols::from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
                b.push_kanji_data(&[0x9F, 0xFD])
            });
        assert_eq!(rqrr_bytes(&luma), vec![vec![0xDF, 0xFD]], "rqrr's bytes");
        let pass = decode(&luma, true);
        let d = pass.found.first().expect("rqrr decodes the clean render");
        assert_eq!(d.raw, vec![0x9F, 0xFD]);
        assert_eq!(d.eci, EciEvidence::NoEci);
        assert!(d.parsed);
    }

    #[test]
    fn a_mirrored_decode_is_rederived_on_the_transposed_grid() {
        // rqrr decodes a mirrored symbol through its mirrored retry; the
        // sampled grid does not verify, its transpose does
        let luma = test_symbols::from_segments_mirrored(
            qrcode::Version::Normal(3),
            qrcode::EcLevel::M,
            |b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            },
        );
        let pass = decode(&luma, true);
        let d = pass
            .found
            .first()
            .expect("rqrr decodes the mirrored render");
        assert_eq!(d.raw, vec![0xA1]);
        assert_eq!(
            d.eci,
            EciEvidence::Located(vec![EciMark { value: 4, start: 0 }])
        );
        assert!(d.parsed);
    }

    /// The raw reads of the one grid rqrr detects in `luma`: as sampled and
    /// transposed.
    fn raw_reads(luma: &LumaImage) -> (Option<super::Oriented>, Option<super::Oriented>) {
        let width = luma.width() as usize;
        let data = luma.data();
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
                data[y * width + x]
            });
        let grid = prepared.detect_grids().remove(0);
        (
            grid.get_raw_data().ok(),
            rqrr::Grid::new(Transposed(&grid.grid)).get_raw_data().ok(),
        )
    }

    #[test]
    fn a_mirrored_decode_publishes_the_verified_stream() {
        // The read publishes the stream and format of the orientation that
        // decided — the transpose for a mirrored symbol — and they
        // error-correct errors-only under the published version/EC/mask
        // (the synthetic-UEC input). As sampled, a mirrored format word
        // still decodes, to another word (rqrr's format decoder corrects
        // past three bit errors: L/mask 0 reads as H/mask 2): the decode
        // published that format with the unmirrored stream before.
        let eci4 = |b: &mut qrcode::bits::Bits| {
            b.push_eci_designator(4)?;
            b.push_byte_data(&[0xA1])
        };
        let (v3, m) = (qrcode::Version::Normal(3), qrcode::EcLevel::M);
        let (l0, width) = test_symbols::masked_colors(
            qrcode::Version::Normal(2),
            qrcode::EcLevel::L,
            qrcode::canvas::MaskPattern::Checkerboard,
            eci4,
        );
        let cases = [
            ("upright", test_symbols::from_segments(v3, m, eci4), false),
            (
                "mirrored",
                test_symbols::from_segments_mirrored(v3, m, eci4),
                true,
            ),
            (
                "mirrored L0",
                test_symbols::render(&test_symbols::mirror(&l0, width), width),
                true,
            ),
        ];
        for (name, luma, mirrored) in cases {
            let (sampled, transposed) = raw_reads(&luma);
            let (_, expected) = (if mirrored { &transposed } else { &sampled })
                .as_ref()
                .unwrap();
            let pass = decode(&luma, true);
            let d = pass.found.first().expect("rqrr decodes the render");
            let stream = d.masked_stream.as_ref().expect("the verified stream");
            assert_eq!(stream.bits, super::masked(expected).bits, "{name}");
            let (version, ec, mask) = (d.version.unwrap(), d.ec.unwrap(), d.mask.unwrap());
            let Rederived::Parsed { parsed, .. } =
                crate::rescue::rederive(stream, version, ec, mask, Acceptance::Engine)
            else {
                panic!("{name}: the published stream error-corrects");
            };
            assert_eq!(parsed.bytes, vec![0xA1], "{name}");
            if name == "mirrored L0" {
                let (meta, _) = sampled.as_ref().expect("the sampled format decodes");
                assert_eq!(
                    (meta.ecc_level, meta.mask),
                    (2, 2),
                    "{name}: H/2 as sampled"
                );
                assert_eq!((ec, mask), (EcLevel::L, 0), "{name}");
            }
        }
    }

    #[test]
    fn a_read_nothing_decided_publishes_no_stream_or_format() {
        let mut read = crate::engine::RawDetection::qr(
            b"abc",
            EciEvidence::Unobserved,
            Fnc1::None,
            EngineKind::Rqrr,
        );
        super::publish_orientation(&mut read, None);
        assert!(read.masked_stream.is_none());
        assert_eq!((read.version, read.ec, read.mask), (None, None, None));
    }

    #[test]
    fn orientation_is_decided_for_at_most_four_grids_per_pass() {
        // Six FNC1 symbols rqrr detects but cannot decode: the first four
        // grids get their orientation decided (and carry that
        // re-derivation), the rest stay as sampled with no RS spent — and a
        // pass whose caller discards candidates spends none at all.
        let gs1 = |b: &mut qrcode::bits::Bits| {
            b.push_fnc1_first_position()?;
            b.push_numeric_data(b"0109506000134352")
        };
        let symbol = test_symbols::masked_colors(
            qrcode::Version::Normal(2),
            qrcode::EcLevel::M,
            qrcode::canvas::MaskPattern::Checkerboard,
            gs1,
        );
        let luma = test_symbols::row(&vec![symbol; 6]);
        let pass = decode(&luma, true);
        assert!(pass.found.is_empty());
        assert_eq!(pass.rescue.len(), 6, "rqrr detects every grid");
        let decided = pass
            .rescue
            .iter()
            .filter(|candidate| candidate.rederived.is_some())
            .count();
        assert_eq!(decided, crate::rescue::MAX_CANDIDATES);
        assert!(
            pass.rescue[..crate::rescue::MAX_CANDIDATES]
                .iter()
                .all(|candidate| matches!(candidate.rederived, Some(Rederived::Parsed { .. })))
        );
        let discarded = decode(&luma, false);
        assert_eq!(discarded.rescue.len(), 6);
        assert!(discarded.rescue.iter().all(|c| c.rederived.is_none()));
    }

    #[test]
    fn a_malformed_designator_decode_is_refused() {
        // rqrr reads a 111xxxxx designator as one byte and returns "abc";
        // the verified stream violates ISO 18004 Table 4: no detection, no
        // rescue input (rqrr did decode it), one refusal whose bytes withhold
        // rqrr's reads where the grid is — mirrored or not (the transposed
        // grid is re-derived too)
        let data =
            test_symbols::stream("0111 11100000 0100 00000011 01100001 01100010 01100011 0000");
        for luma in [
            test_symbols::from_codewords(qrcode::Version::Normal(1), qrcode::EcLevel::M, &data),
            test_symbols::from_codewords_mirrored(
                qrcode::Version::Normal(1),
                qrcode::EcLevel::M,
                &data,
            ),
        ] {
            let pass = decode(&luma, true);
            assert!(pass.found.is_empty(), "{:?}", pass.found);
            assert!(pass.rescue.is_empty());
            let [refusal] = &pass.refused[..] else {
                panic!("one refused stream: {:?}", pass.refused);
            };
            assert_eq!(refusal.rqrr.as_deref(), Some(&b"abc"[..]));
            assert_eq!(refusal.port, None, "the port refuses it too");
            assert!(refusal.place.is_some(), "the grid's bounds");
        }
        // above the register: rxing's port signals a designator and reads
        // "abc" — one record names that content too
        let data = test_symbols::stream(
            "0111 11001111 01000010 01000000 0100 00000011 01100001 01100010 01100011 0000",
        );
        let luma =
            test_symbols::from_codewords(qrcode::Version::Normal(1), qrcode::EcLevel::M, &data);
        let pass = decode(&luma, true);
        assert!(pass.found.is_empty());
        let [refusal] = &pass.refused[..] else {
            panic!("one refused stream: {:?}", pass.refused);
        };
        assert_eq!(refusal.rqrr.as_deref(), Some(&b"abc"[..]));
        assert_eq!(
            refusal.port,
            Some(ContentKey {
                raw: b"abc".to_vec(),
                eci: EciEvidence::Signalled,
                fnc1: Fnc1::None,
            })
        );
    }

    #[test]
    fn an_undecoded_grid_is_the_rescue_input_in_the_orientation_that_verifies() {
        // rqrr's parser refuses FNC1, so FNC1 symbols land in the rescue
        // inputs: unmirrored, the sampled grid verifies; mirrored, rqrr's
        // retry decoded the transpose — that orientation is the input, and
        // its clean re-derivation reads the symbol (or proves it malformed)
        let gs1 = |b: &mut qrcode::bits::Bits| {
            b.push_eci_designator(26)?;
            b.push_fnc1_first_position()?;
            b.push_numeric_data(b"0109506000134352")
        };
        let (v3, m) = (qrcode::Version::Normal(3), qrcode::EcLevel::M);
        let second_then_first = test_symbols::stream(
            "1001 00100101 0101 0100 00000011 01100001 01100010 01100011 0000",
        );
        let v1 = qrcode::Version::Normal(1);
        let cases = [
            ("ECI 26 GS1", test_symbols::from_segments(v3, m, gs1), false),
            (
                "mirrored ECI 26 GS1",
                test_symbols::from_segments_mirrored(v3, m, gs1),
                true,
            ),
            (
                "mirrored FNC1 second then first",
                test_symbols::from_codewords_mirrored(v1, m, &second_then_first),
                true,
            ),
        ];
        for (name, luma, transposed) in cases {
            let pass = decode(&luma, true);
            assert!(pass.found.is_empty() && pass.refused.is_empty(), "{name}");
            assert_eq!(pass.rescue.len(), 1, "{name}");
            let candidate = &pass.rescue[0];
            assert_eq!(candidate.transposed, transposed, "{name}");
            let derived = crate::rescue::rederive(
                &candidate.stream,
                candidate.version,
                candidate.ec,
                candidate.mask,
                Acceptance::Scanner,
            );
            if name.contains("second then first") {
                assert_eq!(
                    derived,
                    Rederived::Malformed {
                        port: Some(PortRead {
                            bytes: b"37abc".to_vec(),
                            signalled: false,
                            fnc1: Fnc1::First,
                        })
                    },
                    "{name}"
                );
            } else {
                let Rederived::Parsed { parsed, .. } = derived else {
                    panic!("{name}: {derived:?}");
                };
                assert_eq!(parsed.bytes, b"0109506000134352", "{name}");
                assert_eq!(
                    parsed.eci,
                    vec![EciMark {
                        value: 26,
                        start: 0
                    }],
                    "{name}"
                );
                assert_eq!(parsed.fnc1, Fnc1::First, "{name}");
            }
        }
        // a grid neither orientation verifies stays the sampled input
        let luma = test_symbols::from_corrupted_codewords(
            v1,
            m,
            &test_symbols::stream("0101 0100 00000011 01100001 01100010 01100011 0000"),
            &[0, 2, 4, 6, 8, 10],
        );
        let pass = decode(&luma, true);
        assert!(pass.found.is_empty(), "{:?}", pass.found);
        assert_eq!(pass.rescue.len(), 1);
        assert!(!pass.rescue[0].transposed);
        // the same input through the decode-failure branch directly
        let width = luma.width() as usize;
        let data = luma.data();
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
                data[y * width + x]
            });
        let grid = prepared.detect_grids().remove(0);
        let input = rescue_input(&grid).expect("the format information reads");
        assert!(!input.transposed);
    }
}
