//! rxing wrapper — the robustness engine (`ZXing` lineage + zxing-cpp detector).
//!
//! Built QR-only (workspace features). The luma helper defaults `TryHarder` on;
//! we add `AlsoInverted`. Raw payload bytes come from the `BYTE_SEGMENTS`
//! metadata when present (byte-mode), else the decoded text — `getRawBytes()`
//! would be codewords, NOT payload. Version/corners are NOT provided by the
//! 0.9.1 QR path (verified in source): rqrr supplies geometry.

use rxing::RXingResultMetadataType as MetaKey;
use rxing::RXingResultMetadataValue as MetaValue;

use super::RawDetection;
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Symbology};

/// rxing format → our wire symbology. `None` = formats we refuse to
/// publish unlabeled (extensions, unrecognized).
fn map_symbology(format: rxing::BarcodeFormat) -> Option<Symbology> {
    use rxing::BarcodeFormat as F;
    Some(match format {
        F::QR_CODE => Symbology::QrCode,
        F::MICRO_QR_CODE => Symbology::MicroQrCode,
        F::RECTANGULAR_MICRO_QR_CODE => Symbology::RectangularMicroQrCode,
        F::DATA_MATRIX => Symbology::DataMatrix,
        F::AZTEC => Symbology::Aztec,
        F::PDF_417 => Symbology::Pdf417,
        F::MAXICODE => Symbology::MaxiCode,
        F::EAN_13 => Symbology::Ean13,
        F::EAN_8 => Symbology::Ean8,
        F::UPC_A => Symbology::UpcA,
        F::UPC_E => Symbology::UpcE,
        F::CODE_128 => Symbology::Code128,
        F::CODE_39 => Symbology::Code39,
        F::CODE_93 => Symbology::Code93,
        F::CODABAR => Symbology::Codabar,
        F::ITF => Symbology::Itf,
        F::RSS_14 => Symbology::DataBar,
        F::RSS_EXPANDED => Symbology::DataBarExpanded,
        F::TELEPEN => Symbology::Telepen,
        // UPC_EAN_EXTENSION add-ons + UNSUPORTED_FORMAT: never published
        F::UPC_EAN_EXTENSION | F::DXFilmEdge | F::UNSUPORTED_FORMAT => return None,
    })
}

fn parse_ec(s: &str) -> Option<EcLevel> {
    match s {
        "L" => Some(EcLevel::L),
        "M" => Some(EcLevel::M),
        "Q" => Some(EcLevel::Q),
        "H" => Some(EcLevel::H),
        _ => None,
    }
}

/// Our symbology → the rxing format set to restrict a pass to.
fn formats_for(only: Symbology) -> std::collections::HashSet<rxing::BarcodeFormat> {
    use rxing::BarcodeFormat as F;
    let format = match only {
        Symbology::QrCode => F::QR_CODE,
        Symbology::MicroQrCode => F::MICRO_QR_CODE,
        Symbology::RectangularMicroQrCode => F::RECTANGULAR_MICRO_QR_CODE,
        Symbology::DataMatrix => F::DATA_MATRIX,
        Symbology::Aztec => F::AZTEC,
        Symbology::Pdf417 => F::PDF_417,
        Symbology::MaxiCode => F::MAXICODE,
        Symbology::Ean13 => F::EAN_13,
        Symbology::Ean8 => F::EAN_8,
        Symbology::UpcA => F::UPC_A,
        Symbology::UpcE => F::UPC_E,
        Symbology::Code128 => F::CODE_128,
        Symbology::Code39 => F::CODE_39,
        Symbology::Code93 => F::CODE_93,
        Symbology::Codabar => F::CODABAR,
        Symbology::Itf => F::ITF,
        Symbology::DataBar => F::RSS_14,
        Symbology::DataBarExpanded => F::RSS_EXPANDED,
        Symbology::Telepen => F::TELEPEN,
    };
    std::collections::HashSet::from([format])
}

pub(super) fn decode(luma: &LumaImage, filter: super::FormatFilter) -> Vec<RawDetection> {
    use rxing::BarcodeFormat as F;
    let possible = match filter {
        super::FormatFilter::All => None,
        super::FormatFilter::QrFamily => Some(std::collections::HashSet::from([
            F::QR_CODE,
            F::MICRO_QR_CODE,
            F::RECTANGULAR_MICRO_QR_CODE,
        ])),
        super::FormatFilter::Only(symbology) => {
            Some(formats_for(symbology)).filter(|set| !set.is_empty())
        }
    };
    // TryHarder defaults on in the helper; inverted symbols are real in the
    // artistic corpus — always on (a config knob nobody turns is debt).
    let mut hints = rxing::DecodeHints {
        AlsoInverted: Some(true),
        PossibleFormats: possible,
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
        .filter_map(|result| {
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
            // GS1 carriers per ISO/IEC 15424 symbology identifiers:
            // QR "]Q3"/"]Q4" (FNC1 first) · DataMatrix "]d2" (ECC200 FNC1
            // first) · Code 128 "]C1" (GS1-128).
            let fnc1 = match metadata.get(&MetaKey::SYMBOLOGY_IDENTIFIER) {
                Some(MetaValue::SymbologyIdentifier(id)) => {
                    matches!(id.as_str(), "]Q3" | "]Q4" | "]d2" | "]C1")
                }
                _ => false,
            };
            let symbology = map_symbology(*result.getBarcodeFormat())?;

            Some(RawDetection {
                symbology,
                raw,
                masked_stream: None,
                corners: None,
                version: None,
                ec,
                mask: None,
                fnc1,
                engine: EngineKind::Rxing,
            })
        })
        .collect()
}
