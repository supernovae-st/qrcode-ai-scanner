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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::decode;
    use crate::engine::FormatFilter;
    use crate::input::LumaImage;
    use crate::report::{EcLevel, EngineKind, Symbology};

    /// Render a QR to a luma plane (default 4-module quiet zone, 6px modules).
    fn render(content: &[u8], ec: qrcode::EcLevel) -> LumaImage {
        let code = qrcode::QrCode::with_error_correction_level(content, ec).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let (w, h) = (img.width(), img.height());
        LumaImage::new(img.into_raw(), w, h)
    }

    /// Render an FNC1-in-first-position (GS1) symbol from an explicit bitstream.
    fn render_fnc1(byte_data: &[u8]) -> LumaImage {
        let mut bits = qrcode::bits::Bits::new(qrcode::Version::Normal(3));
        bits.push_fnc1_first_position().unwrap();
        bits.push_byte_data(byte_data).unwrap();
        bits.push_terminator(qrcode::EcLevel::M).unwrap();
        let code = qrcode::QrCode::with_bits(bits, qrcode::EcLevel::M).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let (w, h) = (img.width(), img.height());
        LumaImage::new(img.into_raw(), w, h)
    }

    #[test]
    fn decodes_a_clean_qr_with_raw_bytes_and_ec() {
        let luma = render(b"parity check 1234", qrcode::EcLevel::Q);
        let found = decode(&luma, FormatFilter::All);
        let d = found
            .iter()
            .find(|d| d.symbology == Symbology::QrCode)
            .expect("rxing decodes a clean QR");
        assert_eq!(d.raw, b"parity check 1234");
        assert_eq!(d.engine, EngineKind::Rxing);
        assert_eq!(d.ec, Some(EcLevel::Q));
        // the rxing 0.9.1 QR path supplies no geometry/stream — rqrr owns that
        assert_eq!(d.version, None);
        assert!(d.corners.is_none() && d.masked_stream.is_none() && d.mask.is_none());
        assert!(!d.fnc1, "plain byte-mode QR is not FNC1");
    }

    #[test]
    fn surfaces_the_fnc1_flag_on_a_gs1_symbol() {
        // rxing is THE GS1 engine: it reads the ]Q3 symbology identifier and
        // stamps fnc1=true so the payload router sends the bytes to the
        // element-string classifier. The raw bytes are the BYTE_SEGMENTS data.
        let luma = render_fnc1(b"010950600013435210ABC123\x1d17261231");
        let found = decode(&luma, FormatFilter::All);
        let d = found
            .iter()
            .find(|d| d.symbology == Symbology::QrCode)
            .expect("rxing decodes the GS1 QR");
        assert!(d.fnc1, "the ]Q3 identifier must surface as fnc1");
        assert!(
            d.raw.starts_with(b"0109506000134352"),
            "raw carries the element data: {:?}",
            d.raw
        );
    }

    #[test]
    fn decodes_an_inverted_symbol_via_also_inverted() {
        // AlsoInverted is forced on in the adapter (light-on-dark artistic
        // codes are real). Photometrically invert a clean render — rxing still
        // reads it, proving the hint is wired.
        let luma = render(b"inverted-ok", qrcode::EcLevel::M);
        let inverted = LumaImage::new(
            luma.data().iter().map(|&p| 255 - p).collect(),
            luma.width(),
            luma.height(),
        );
        let found = decode(&inverted, FormatFilter::All);
        let d = found
            .iter()
            .find(|d| d.symbology == Symbology::QrCode)
            .expect("AlsoInverted decodes a light-on-dark QR");
        assert_eq!(d.raw, b"inverted-ok");
    }

    #[test]
    fn only_filter_restricts_to_the_requested_symbology() {
        // FormatFilter::Only(QrCode) routes through formats_for → a one-format
        // hint set; a clean QR still decodes under the restriction.
        let luma = render(b"only-qr", qrcode::EcLevel::M);
        let found = decode(&luma, FormatFilter::Only(Symbology::QrCode));
        assert!(
            found
                .iter()
                .any(|d| d.symbology == Symbology::QrCode && d.raw == b"only-qr")
        );
    }

    #[test]
    fn a_blank_image_yields_no_detection_and_never_panics() {
        // NotFound is a miss, not an error — the adapter returns empty. (The
        // catch_unwind boundary itself lives in engine::run_isolated, mod.rs.)
        let blank = LumaImage::new(vec![255u8; 48 * 48], 48, 48);
        assert!(decode(&blank, FormatFilter::All).is_empty());
    }
}
