//! rqrr wrapper — the geometry + metadata engine (quirc lineage).
//!
//! Always through `decode_to` raw bytes: rqrr's `decode()` forces UTF-8.
//! rqrr parses ECI designators and DISCARDS them (0.10.1 and 0.11.0), so the
//! adapter re-derives the segment structure from the sampled grid with the
//! scanner's own codeword path — interpretation is OUR job, once, in
//! `engine::content`. The verified stream decides the payload bytes too:
//! rqrr's own conversion is not the truth (0.10.1 writes the Kanji values
//! 5949-5951 as Shift JIS DF FD..DF FF — `value + 0x8140 <= 0x9FFC` —
//! where ISO 18004 §7.4.6 gives 9F FD..9F FF).

use super::content::{EciEvidence, Fnc1};
use super::{MaskedStream, RawDetection};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point, Symbology};
use crate::rescue::{Acceptance, Rederived, RescueCandidate};

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
    /// Payloads of decodes the scanner refused — the ladder's veto keys.
    pub(super) refused: Vec<Vec<u8>>,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "corner coordinates are bounded by Limits::max_dimension (10k) — exact in f32 (2^24)"
)]
pub(super) fn decode(luma: &LumaImage) -> Pass {
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
    for grid in prepared.detect_grids() {
        let mut decoded = Vec::new();
        let Ok(meta) = grid.decode_to(&mut decoded) else {
            // RS gave up — but the grid + format info may still read: that
            // stream is the S5 erasure-rescue input
            if let Ok((meta, raw_data)) = grid.get_raw_data()
                && let Ok(version) = u8::try_from(meta.version.0)
                && let Some(ec) = map_ec(meta.ecc_level)
                && let Ok(mask) = u8::try_from(meta.mask)
            {
                pass.rescue.push(RescueCandidate {
                    stream: masked(&raw_data),
                    corners: grid.bounds.map(|p| Point {
                        x: p.x as f32,
                        y: p.y as f32,
                    }),
                    version,
                    ec,
                    mask,
                    inverted: false, // stamped by the ladder per attempt
                });
            }
            continue;
        };
        // the grid as sampled, still masked — the synthetic-UEC input
        let sampled = grid.get_raw_data().ok();
        let (raw, eci, fnc1, parsed) =
            match account(decoded, rederive_decoded(&grid, sampled.as_ref())) {
                Account::Read {
                    raw,
                    eci,
                    fnc1,
                    parsed,
                } => (raw, eci, fnc1, parsed),
                Account::Refused(payloads) => {
                    pass.refused.extend(payloads);
                    continue;
                }
            };
        let corners = grid.bounds.map(|p| Point {
            x: p.x as f32,
            y: p.y as f32,
        });
        pass.found.push(RawDetection {
            symbology: Symbology::QrCode,
            raw,
            eci,
            masked_stream: sampled.map(|(_, raw_data)| masked(&raw_data)),
            corners: Some(corners),
            version: u8::try_from(meta.version.0).ok(),
            ec: map_ec(meta.ecc_level),
            mask: u8::try_from(meta.mask).ok(),
            fnc1,
            parsed,
            engine: EngineKind::Rqrr,
        });
    }
    pass
}

/// What the scanner makes of one rqrr decode.
#[derive(Debug, PartialEq, Eq)]
enum Account {
    /// Published: the payload and what was observed of its declarations.
    Read {
        raw: Vec<u8>,
        eci: EciEvidence,
        fnc1: Fnc1,
        parsed: bool,
    },
    /// Refused: the stream rqrr decoded is not one ISO 18004 defines (rqrr
    /// reads a `111xxxxx` designator as one byte, an assignment number past
    /// 999999 like any other). Carries the payloads the refusal vetoes:
    /// rqrr's bytes and what rxing's port transmits for the stream.
    Refused(Vec<Vec<u8>>),
}

/// rqrr decoded `decoded`; the re-derivation of its grid decides.
fn account(decoded: Vec<u8>, derived: Rederived) -> Account {
    match derived {
        // the RS-verified grid is the truth, bytes included: they can only
        // differ from rqrr's through rqrr's own conversion of the very same
        // codewords (rqrr keeps the first orientation its RS accepts, and
        // the re-derivation reproduces that acceptance)
        Rederived::Parsed(parsed) => Account::Read {
            raw: parsed.bytes,
            eci: EciEvidence::from_marks(parsed.eci),
            fnc1: parsed.fnc1,
            parsed: true,
        },
        // no orientation verifies: nothing observable of the declarations;
        // rqrr decoded the symbol, so its stream held no FNC1 indicator
        Rederived::Unverified => Account::Read {
            raw: decoded,
            eci: EciEvidence::Unobserved,
            fnc1: Fnc1::None,
            parsed: false,
        },
        Rederived::Malformed { port_payload } => {
            Account::Refused(std::iter::once(decoded).chain(port_payload).collect())
        }
    }
}

/// Re-derive what rqrr decoded: the grid as sampled, then — when that does
/// not verify — its transpose, the orientation rqrr's mirrored retry
/// decodes (`MirroredGrid` swaps x and y). rqrr keeps the sampled grid
/// whenever its RS accepts it, so only one orientation verifies: the one
/// rqrr read. Each is unmasked with the format information read from it.
fn rederive_decoded<G: rqrr::BitGrid>(
    grid: &rqrr::Grid<G>,
    sampled: Option<&(rqrr::MetaData, rqrr::RawData)>,
) -> Rederived {
    let derived = sampled.map_or(Rederived::Unverified, |(meta, raw_data)| {
        rederive_sampled(meta, raw_data)
    });
    if !matches!(derived, Rederived::Unverified) {
        return derived;
    }
    rqrr::Grid::new(Transposed(&grid.grid))
        .get_raw_data()
        .map_or(Rederived::Unverified, |(meta, raw_data)| {
            rederive_sampled(&meta, &raw_data)
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
        return Rederived::Unverified;
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

    use super::{Account, account, decode};
    use crate::engine::content::{EciEvidence, EciMark, Fnc1};
    use crate::input::LumaImage;
    use crate::report::{EcLevel, EngineKind, Symbology};
    use crate::rescue::bitstream::Parsed;
    use crate::rescue::{Rederived, test_symbols};

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
        let pass = decode(&luma);
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
        let pass = decode(&luma);
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
        let pass = decode(&blank);
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
        let pass = decode(&luma);
        let d = pass.found.first().expect("rqrr decodes the clean render");
        assert_eq!(d.raw, b"12345\xA1");
        assert_eq!(
            d.eci,
            EciEvidence::Located(vec![EciMark { value: 4, start: 5 }])
        );
    }

    #[test]
    fn the_verified_stream_decides_the_payload_and_its_account() {
        let parsed = |bytes: &[u8], eci: Vec<EciMark>| {
            Rederived::Parsed(Parsed {
                bytes: bytes.to_vec(),
                eci,
                fnc1: Fnc1::None,
                structured_append: false,
                hanzi: false,
            })
        };
        let read = |raw: &[u8], eci: EciEvidence, parsed: bool| Account::Read {
            raw: raw.to_vec(),
            eci,
            fnc1: Fnc1::None,
            parsed,
        };
        let eci4 = vec![EciMark { value: 4, start: 0 }];
        // equal bytes: the located declaration
        assert_eq!(
            account(vec![0xA1], parsed(&[0xA1], eci4.clone())),
            read(&[0xA1], EciEvidence::Located(eci4), true)
        );
        // rqrr's own conversion differs (its Kanji inversion): the verified
        // bytes are the payload, never rqrr's
        assert_eq!(
            account(vec![0xDF, 0xFD], parsed(&[0x9F, 0xFD], Vec::new())),
            read(&[0x9F, 0xFD], EciEvidence::NoEci, true)
        );
        // no orientation verifies: rqrr's bytes, nothing observable of the
        // declarations, and no FNC1 (rqrr decodes neither mode)
        assert_eq!(
            account(b"abc".to_vec(), Rederived::Unverified),
            read(b"abc", EciEvidence::Unobserved, false)
        );
        // malformed: refused, with the payloads the refusal vetoes
        assert_eq!(
            account(
                b"abc".to_vec(),
                Rederived::Malformed {
                    port_payload: Some(b"xyz".to_vec())
                }
            ),
            Account::Refused(vec![b"abc".to_vec(), b"xyz".to_vec()])
        );
        assert_eq!(
            account(b"abc".to_vec(), Rederived::Malformed { port_payload: None }),
            Account::Refused(vec![b"abc".to_vec()])
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
        let pass = decode(&luma);
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
        let pass = decode(&luma);
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

    #[test]
    fn a_malformed_designator_decode_is_refused() {
        // rqrr reads a 111xxxxx designator as one byte and returns "abc";
        // the verified stream violates ISO 18004 Table 4: no detection, no
        // rescue input (rqrr did decode it), its bytes are the veto key —
        // mirrored or not (the transposed grid is re-derived too)
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
            let pass = decode(&luma);
            assert!(pass.found.is_empty(), "{:?}", pass.found);
            assert!(pass.rescue.is_empty());
            assert_eq!(pass.refused, vec![b"abc".to_vec()]);
        }
        // above the register: rxing's port transmits "abc" too
        let data = test_symbols::stream(
            "0111 11001111 01000010 01000000 0100 00000011 01100001 01100010 01100011 0000",
        );
        let luma =
            test_symbols::from_codewords(qrcode::Version::Normal(1), qrcode::EcLevel::M, &data);
        let pass = decode(&luma);
        assert!(pass.found.is_empty());
        assert_eq!(pass.refused, vec![b"abc".to_vec(), b"abc".to_vec()]);
    }
}
