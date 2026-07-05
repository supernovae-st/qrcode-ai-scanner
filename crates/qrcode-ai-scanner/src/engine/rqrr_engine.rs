//! rqrr wrapper — the geometry + metadata engine (quirc lineage).
//!
//! Always through `decode_to` raw bytes: rqrr's `decode()` forces UTF-8 and
//! silently discards ECI (verified in rqrr 0.10.1 source) — charset is OUR
//! job, once, in `engine::charset`.

use super::{MaskedStream, RawDetection};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point, Symbology};
use crate::rescue::RescueCandidate;

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
        let corners = grid.bounds.map(|p| Point {
            x: p.x as f32,
            y: p.y as f32,
        });
        found.push(RawDetection {
            symbology: Symbology::QrCode,
            raw,
            masked_stream,
            corners: Some(corners),
            version: u8::try_from(meta.version.0).ok(),
            ec: map_ec(meta.ecc_level),
            mask: u8::try_from(meta.mask).ok(),
            // rqrr cannot decode FNC1 symbols (mode dispatch rejects 0x5/0x9)
            fnc1: false,
            engine: EngineKind::Rqrr,
        });
    }
    (found, rescue)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::decode;
    use crate::input::LumaImage;
    use crate::report::{EcLevel, EngineKind, Symbology};

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
        assert!(!d.fnc1, "a plain byte-mode QR is not FNC1");
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
}
