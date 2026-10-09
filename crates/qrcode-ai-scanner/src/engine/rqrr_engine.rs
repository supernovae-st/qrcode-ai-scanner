//! rqrr wrapper — the geometry + metadata engine (quirc lineage).
//!
//! Always through `decode_to` raw bytes: rqrr's `decode()` forces UTF-8.
//! rqrr parses ECI designators and DISCARDS them (0.10.1 and 0.11.0), so the
//! adapter re-derives the segment structure from the sampled stream with the
//! scanner's own codeword path and keeps it only when its payload equals
//! rqrr's bytes — interpretation is OUR job, once, in `engine::content`.

use super::content::{EciEvidence, Fnc1};
use super::{MaskedStream, RawDetection};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point, Symbology};
use crate::rescue::{Rederived, RescueCandidate};

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

#[expect(
    clippy::cast_precision_loss,
    reason = "corner coordinates are bounded by Limits::max_dimension (10k) — exact in f32 (2^24)"
)]
pub(super) fn decode(luma: &LumaImage) -> (Vec<RawDetection>, Vec<RescueCandidate>) {
    let width = luma.width() as usize;
    let data = luma.data();
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width, luma.height() as usize, |x, y| {
            data[y * width + x]
        });

    let mut found = Vec::new();
    let mut rescue = Vec::new();
    for grid in prepared.detect_grids() {
        let mut raw = Vec::new();
        let Ok(meta) = grid.decode_to(&mut raw) else {
            // RS gave up — but the grid + format info may still read: that
            // stream is the S5 erasure-rescue input
            if let Ok((meta, raw_data)) = grid.get_raw_data()
                && let Ok(version) = u8::try_from(meta.version.0)
                && let Some(ec) = map_ec(meta.ecc_level)
                && let Ok(mask) = u8::try_from(meta.mask)
            {
                rescue.push(RescueCandidate {
                    stream: MaskedStream {
                        bits: raw_data.data[..raw_data.len.div_ceil(8)].to_vec(),
                        bit_len: raw_data.len,
                    },
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
        // raw sampled bitstream (still masked) — the synthetic-UEC input
        let masked_stream = grid.get_raw_data().ok().map(|(_, raw_data)| MaskedStream {
            bits: raw_data.data[..raw_data.len.div_ceil(8)].to_vec(),
            bit_len: raw_data.len,
        });
        let version = u8::try_from(meta.version.0).ok();
        let ec = map_ec(meta.ecc_level);
        let mask = u8::try_from(meta.mask).ok();
        let Some(eci) = segment_evidence(&raw, masked_stream.as_ref(), version, ec, mask) else {
            // the RS-verified stream is not one ISO 18004 defines (rqrr reads
            // a 111xxxxx designator as one byte): refused, as rxing and zbar
            // refuse it — never a guessed payload
            continue;
        };
        let corners = grid.bounds.map(|p| Point {
            x: p.x as f32,
            y: p.y as f32,
        });
        found.push(RawDetection {
            symbology: Symbology::QrCode,
            raw,
            eci,
            masked_stream,
            corners: Some(corners),
            version,
            ec,
            mask,
            // rqrr cannot decode FNC1 symbols (mode dispatch rejects 0x5/0x9)
            fnc1: Fnc1::None,
            engine: EngineKind::Rqrr,
        });
    }
    (found, rescue)
}

/// ECI evidence for one rqrr decode: the segment structure re-derived from
/// the sampled stream, kept only when its payload equals rqrr's bytes.
/// `None` = the stream verifies but violates ISO 18004 — refuse the decode.
fn segment_evidence(
    raw: &[u8],
    stream: Option<&MaskedStream>,
    version: Option<u8>,
    ec: Option<EcLevel>,
    mask: Option<u8>,
) -> Option<EciEvidence> {
    let (Some(stream), Some(version), Some(ec), Some(mask)) = (stream, version, ec, mask) else {
        return Some(EciEvidence::Unobserved);
    };
    match crate::rescue::rederive(stream, version, ec, mask) {
        Rederived::Parsed(parsed) if parsed.bytes == raw => {
            Some(EciEvidence::from_marks(parsed.eci))
        }
        // rqrr may have decoded the MIRRORED grid while `get_raw_data` reads
        // it unmirrored: a stream that tells another story than the decoded
        // bytes declares nothing — never fabricate a declaration from it
        Rederived::Parsed(_) | Rederived::Unverified => Some(EciEvidence::Unobserved),
        Rederived::Malformed => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::{decode, segment_evidence};
    use crate::engine::content::{EciEvidence, EciMark, Fnc1};
    use crate::input::LumaImage;
    use crate::report::{EcLevel, EngineKind, Symbology};
    use crate::rescue::test_symbols;

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
        let (found, _rescue) = decode(&luma);
        let d = found.first().expect("rqrr decodes a clean QR");
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
        let (found, _rescue) = decode(&luma);
        assert!(found.is_empty(), "rqrr must not decode FNC1: {found:?}");
    }

    #[test]
    fn a_blank_image_yields_no_detection_and_never_panics() {
        // No finder patterns → detect_grids finds nothing. (The panic-isolation
        // boundary itself lives in engine::run_isolated, exercised in mod.rs;
        // the adapter's own job is to return empty on a QR-less image.)
        let blank = LumaImage::new(vec![255u8; 48 * 48], 48, 48);
        let (found, rescue) = decode(&blank);
        assert!(found.is_empty() && rescue.is_empty());
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
        let (found, _) = decode(&luma);
        let d = found.first().expect("rqrr decodes the clean render");
        assert_eq!(d.raw, b"12345\xA1");
        assert_eq!(
            d.eci,
            EciEvidence::Located(vec![EciMark { value: 4, start: 5 }])
        );
    }

    #[test]
    fn a_stream_that_tells_another_story_declares_nothing() {
        let luma =
            test_symbols::from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            });
        let (found, _) = decode(&luma);
        let d = found.first().expect("rqrr decodes the clean render");
        let stream = d.masked_stream.as_ref().expect("rqrr exposes the stream");
        // bytes that differ from the re-derived payload (what a mirrored
        // decode looks like): unobserved, never a borrowed declaration
        assert_eq!(
            segment_evidence(b"other", Some(stream), d.version, d.ec, d.mask),
            Some(EciEvidence::Unobserved)
        );
        // a tampered stream that no longer verifies
        let mut tampered = stream.clone();
        for byte in &mut tampered.bits {
            *byte = !*byte;
        }
        assert_eq!(
            segment_evidence(&d.raw, Some(&tampered), d.version, d.ec, d.mask),
            Some(EciEvidence::Unobserved)
        );
        // no stream or no format metadata: nothing observable
        assert_eq!(
            segment_evidence(&d.raw, None, d.version, d.ec, d.mask),
            Some(EciEvidence::Unobserved)
        );
        assert_eq!(
            segment_evidence(&d.raw, Some(stream), None, d.ec, d.mask),
            Some(EciEvidence::Unobserved)
        );
    }

    #[test]
    fn a_malformed_designator_decode_is_refused() {
        // rqrr reads a 111xxxxx designator as one byte and returns "abc";
        // the verified stream violates ISO 18004 Table 4: no detection (and
        // no rescue input — rqrr did decode it)
        let data =
            test_symbols::stream("0111 11100000 0100 00000011 01100001 01100010 01100011 0000");
        let luma =
            test_symbols::from_codewords(qrcode::Version::Normal(1), qrcode::EcLevel::M, &data);
        let (found, rescue) = decode(&luma);
        assert!(found.is_empty(), "{found:?}");
        assert!(rescue.is_empty());
    }
}
