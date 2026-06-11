//! rxing wrapper — the robustness engine (`ZXing` lineage + zxing-cpp detector).
//!
//! Built QR-only (workspace features). The luma helper defaults `TryHarder` on;
//! we add `AlsoInverted`. Raw payload bytes come from the `BYTE_SEGMENTS`
//! metadata when present (byte-mode), else the decoded text — `getRawBytes()`
//! would be codewords, NOT payload. Version/corners are NOT provided by the
//! 0.9.1 QR path (verified in source): rqrr supplies geometry.

use rxing::RXingResultMetadataType as MetaKey;
use rxing::RXingResultMetadataValue as MetaValue;

use super::{EngineOptions, RawDetection};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind};

fn parse_ec(s: &str) -> Option<EcLevel> {
    match s {
        "L" => Some(EcLevel::L),
        "M" => Some(EcLevel::M),
        "Q" => Some(EcLevel::Q),
        "H" => Some(EcLevel::H),
        _ => None,
    }
}

pub(super) fn decode(luma: &LumaImage, opts: EngineOptions) -> Vec<RawDetection> {
    let mut hints = rxing::DecodeHints {
        AlsoInverted: Some(opts.also_inverted),
        ..Default::default()
    };

    let Ok(results) = rxing::helpers::detect_multiple_in_luma_with_hints(
        luma.data().to_vec(),
        luma.width(),
        luma.height(),
        &mut hints,
    ) else {
        // NotFound and friends — a miss, not an error.
        return Vec::new();
    };

    results
        .into_iter()
        .map(|result| {
            let text = result.getText().to_owned();
            let metadata = result.getRXingResultMetadata();

            let raw = match metadata.get(&MetaKey::BYTE_SEGMENTS) {
                Some(MetaValue::ByteSegments(segments)) => segments.concat(),
                _ => text.clone().into_bytes(),
            };
            let ec = match metadata.get(&MetaKey::ERROR_CORRECTION_LEVEL) {
                Some(MetaValue::ErrorCorrectionLevel(level)) => parse_ec(level),
                _ => None,
            };

            RawDetection {
                raw,
                text_hint: Some(text),
                corners: None,
                version: None,
                ec,
                mask: None,
                engine: EngineKind::Rxing,
            }
        })
        .collect()
}
