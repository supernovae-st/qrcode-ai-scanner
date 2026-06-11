//! Decode engines — isolated wrappers around third-party decoders.
//!
//! Contract: engines NEVER panic outward (`catch_unwind` boundary), never
//! decide charset (raw bytes are the truth, resolution happens once here in
//! `charset`), and only report what they actually measured — no stubs.

pub(crate) mod charset;
#[cfg(feature = "engine-rqrr")]
mod rqrr_engine;
#[cfg(feature = "engine-rxing")]
mod rxing_engine;

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point};

/// Still-masked codeword bitstream as the engine sampled it (rqrr only) —
/// feeds the synthetic UEC.
#[derive(Debug, Clone)]
pub(crate) struct MaskedStream {
    /// Packed bits, MSB-first per byte.
    pub bits: Vec<u8>,
    /// Stream length in bits.
    pub bit_len: usize,
}

/// What an engine actually measured — no stubs, `None` means "not provided".
#[derive(Debug, Clone)]
pub(crate) struct RawDetection {
    /// Decoded payload bytes (charset-independent truth).
    pub raw: Vec<u8>,
    /// Raw sampled bitstream when the engine exposes it (rqrr).
    pub masked_stream: Option<MaskedStream>,
    /// Symbol corners when the engine provides true bounds (rqrr).
    pub corners: Option<[Point; 4]>,
    /// Symbol version 1-40 when measured.
    pub version: Option<u8>,
    /// Error-correction level when measured.
    pub ec: Option<EcLevel>,
    /// Mask pattern 0-7 when measured.
    pub mask: Option<u8>,
    /// FNC1-in-first-position symbol (GS1 formatted data — symbology
    /// `]Q3`/`]Q4`). Only rxing surfaces this; rqrr cannot decode FNC1
    /// symbols at all (its mode dispatch rejects indicators 0x5/0x9).
    pub fnc1: bool,
    /// Producing engine.
    pub engine: EngineKind,
}

/// Aggregated result of one pass over all enabled engines.
#[derive(Debug, Clone, Default)]
pub(crate) struct EngineOutcome {
    /// Every detection from every engine (deduplication is the ladder's job).
    pub detections: Vec<RawDetection>,
    /// Engine panics caught and isolated during this pass.
    pub panics: u8,
}

/// Run a closure with a panic boundary. `None` = the closure panicked.
fn run_isolated<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

/// One decode pass: every enabled engine over the same luma image.
pub(crate) fn decode_all(luma: &LumaImage) -> EngineOutcome {
    let mut outcome = EngineOutcome::default();

    #[cfg(feature = "engine-rxing")]
    match run_isolated(|| rxing_engine::decode(luma)) {
        Some(found) => outcome.detections.extend(found),
        None => outcome.panics += 1,
    }

    #[cfg(feature = "engine-rqrr")]
    match run_isolated(|| rqrr_engine::decode(luma)) {
        Some(found) => outcome.detections.extend(found),
        None => outcome.panics += 1,
    }

    outcome
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::*;
    use crate::input::LumaImage;
    use crate::report::{Charset, EcLevel, EngineKind};

    /// Render a QR to luma bytes module-by-module (no image-crate coupling):
    /// `scale` px per module + 4-module quiet zone. Returns the symbol version.
    fn qr_luma(content: &[u8], ec: qrcode::EcLevel, scale: u32) -> (LumaImage, u8) {
        let code = qrcode::QrCode::with_error_correction_level(content, ec).unwrap();
        let modules = code.width();
        let colors = code.to_colors();
        let qz = 4u32;
        let side = (u32::try_from(modules).unwrap() + 2 * qz) * scale;
        let mut data = vec![255u8; (side * side) as usize];
        for my in 0..modules {
            for mx in 0..modules {
                if colors[my * modules + mx] == qrcode::Color::Dark {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let x = (u32::try_from(mx).unwrap() + qz) * scale + dx;
                            let y = (u32::try_from(my).unwrap() + qz) * scale + dy;
                            data[(y * side + x) as usize] = 0;
                        }
                    }
                }
            }
        }
        let version = match code.version() {
            qrcode::Version::Normal(v) => u8::try_from(v).unwrap(),
            qrcode::Version::Micro(_) => 0,
        };
        (LumaImage::new(data, side, side), version)
    }

    #[cfg(feature = "engine-rxing")]
    #[test]
    fn rxing_decodes_utf8_qr_with_text_and_ec() {
        let payload = "héllo → 🦋";
        let (luma, _) = qr_luma(payload.as_bytes(), qrcode::EcLevel::Q, 4);
        let outcome = decode_all(&luma);
        let d = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rxing)
            .expect("rxing must decode a clean generated QR");
        let (text, _) = charset::resolve(&d.raw);
        assert_eq!(text, payload);
        assert_eq!(d.raw, payload.as_bytes());
        assert_eq!(d.ec, Some(EcLevel::Q));
        assert_eq!(outcome.panics, 0);
    }

    #[cfg(feature = "engine-rqrr")]
    #[test]
    fn rqrr_provides_geometry_version_ec_mask() {
        let (luma, version) = qr_luma(b"https://qrcode-ai.com/scan", qrcode::EcLevel::Q, 4);
        let outcome = decode_all(&luma);
        let d = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rqrr)
            .expect("rqrr must decode a clean generated QR");
        assert_eq!(d.version, Some(version));
        assert_eq!(d.ec, Some(EcLevel::Q), "rqrr ecc_level 0-3 mapping");
        let mask = d.mask.expect("rqrr reports the mask");
        assert!(mask <= 7, "mask {mask}");
        let corners = d.corners.expect("rqrr reports bounds");
        let side = luma.width() as f32;
        for p in corners {
            assert!(
                p.x >= 0.0 && p.x < side && p.y >= 0.0 && p.y < side,
                "{p:?}"
            );
        }
    }

    #[cfg(feature = "engine-rqrr")]
    #[test]
    fn rqrr_ec_mapping_pinned_on_l_too() {
        let (luma, _) = qr_luma(b"ec level L pin", qrcode::EcLevel::L, 4);
        let outcome = decode_all(&luma);
        let d = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rqrr)
            .expect("decode");
        assert_eq!(d.ec, Some(EcLevel::L));
    }

    #[cfg(all(feature = "engine-rxing", feature = "engine-rqrr"))]
    #[test]
    fn engines_agree_on_raw_payload_bytes() {
        let payload = "parity check 1234";
        let (luma, _) = qr_luma(payload.as_bytes(), qrcode::EcLevel::M, 4);
        let outcome = decode_all(&luma);
        let rxing_raw = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rxing)
            .map(|d| d.raw.clone())
            .expect("rxing");
        let rqrr_raw = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rqrr)
            .map(|d| d.raw.clone())
            .expect("rqrr");
        assert_eq!(rxing_raw, rqrr_raw);
        assert_eq!(rxing_raw, payload.as_bytes());
    }

    #[cfg(feature = "engine-rqrr")]
    #[test]
    fn latin1_byte_mode_payload_resolves_via_raw_path() {
        // "Éé" in windows-1252 — invalid UTF-8, the rqrr decode() trap case.
        let (luma, _) = qr_luma(&[0xC9, 0xE9], qrcode::EcLevel::M, 4);
        let outcome = decode_all(&luma);
        let d = outcome
            .detections
            .iter()
            .find(|d| d.engine == EngineKind::Rqrr)
            .expect("rqrr must decode byte-mode latin1");
        assert_eq!(d.raw, vec![0xC9, 0xE9]);
        let (text, cs) = charset::resolve(&d.raw);
        assert_eq!(text, "Éé");
        assert_eq!(cs, Charset::Latin1);
    }

    #[test]
    fn charset_resolution_order_utf8_sjis_latin1() {
        let (t, c) = charset::resolve("plain ascii".as_bytes());
        assert_eq!((t.as_str(), c), ("plain ascii", Charset::Utf8));

        let (t, c) = charset::resolve("héllo🦋".as_bytes());
        assert_eq!((t.as_str(), c), ("héllo🦋", Charset::Utf8));

        // こんにちは in Shift-JIS
        let sjis: &[u8] = &[0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];
        let (t, c) = charset::resolve(sjis);
        assert_eq!((t.as_str(), c), ("こんにちは", Charset::ShiftJis));

        let (t, c) = charset::resolve(&[0xC9, 0xE9]);
        assert_eq!((t.as_str(), c), ("Éé", Charset::Latin1));
    }

    #[test]
    fn panics_are_isolated_not_propagated() {
        let caught: Option<u8> = run_isolated(|| panic!("engine exploded"));
        assert!(caught.is_none());
        let fine = run_isolated(|| 7u8);
        assert_eq!(fine, Some(7));
    }
}
