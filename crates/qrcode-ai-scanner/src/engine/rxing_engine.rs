//! rxing wrapper — the robustness engine (`ZXing` lineage + zxing-cpp detector).
//!
//! The luma helper defaults `TryHarder` on; we add `AlsoInverted`. rxing
//! tries two QR readers in turn and `getRawBytes()` means something
//! different for each (verified in the 0.9.1 and 0.9.3 sources):
//!
//! - its zxing-cpp port (the first try for every image — it runs its
//!   general finder-pattern detector unless `PureBarcode` is set — and the
//!   only Micro QR / rMQR reader): the ORIGINAL payload bytes, ECI
//!   designators excluded — exact, but the designators themselves are not
//!   exposed (only their presence, through the AIM modifier `]Q2`/`]Q4`/
//!   `]Q6`);
//! - its `ZXing`-lineage reader (when the port fails): the RS-corrected
//!   data codewords — parsed here with the scanner's segment parser.
//!
//! The decoded text is neither the payload nor the reading: content is
//! interpreted once, in `engine::content`. Version/corners are NOT provided
//! by the QR paths: rqrr supplies geometry.

use rxing::RXingResultMetadataType as MetaKey;
use rxing::RXingResultMetadataValue as MetaValue;

use super::RawDetection;
use super::content::{EciEvidence, Fnc1};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Symbology};
use crate::rescue::bitstream;

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

    results.iter().filter_map(observe).collect()
}

/// Which rxing QR reader produced a result — it decides what
/// `getRawBytes()` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reader {
    /// The zxing-cpp port: `getRawBytes()` is the payload itself.
    Port,
    /// The `ZXing`-lineage reader: `getRawBytes()` is the RS-corrected data
    /// codeword stream of a symbol at this EC level.
    Codewords(EcLevel),
}

/// Tell the readers apart by what each deterministically writes (rxing
/// 0.9.1-0.9.3): the port states the EC level as a letter, the
/// `ZXing`-lineage reader as its format-bits value (M=0 · L=1 · H=2 · Q=3).
/// Micro QR and rMQR exist only on the port. `None`: neither shape — the
/// bytes' meaning is unknown.
fn reader(symbology: Symbology, ec_level: Option<&str>) -> Option<Reader> {
    if symbology != Symbology::QrCode {
        return Some(Reader::Port);
    }
    match ec_level? {
        "L" | "M" | "Q" | "H" => Some(Reader::Port),
        "0" => Some(Reader::Codewords(EcLevel::M)),
        "1" => Some(Reader::Codewords(EcLevel::L)),
        "2" => Some(Reader::Codewords(EcLevel::H)),
        "3" => Some(Reader::Codewords(EcLevel::Q)),
        _ => None,
    }
}

/// Map one rxing result to an observation. `None`: a format we refuse to
/// publish, or a QR-family result whose original bytes cannot be
/// established — never published with a stand-in.
fn observe(result: &rxing::RXingResult) -> Option<RawDetection> {
    let symbology = map_symbology(*result.getBarcodeFormat())?;
    let metadata = result.getRXingResultMetadata();
    let ec_level = match metadata.get(&MetaKey::ERROR_CORRECTION_LEVEL) {
        Some(MetaValue::ErrorCorrectionLevel(level)) => Some(level.as_str()),
        _ => None,
    };
    let identifier = match metadata.get(&MetaKey::SYMBOLOGY_IDENTIFIER) {
        Some(MetaValue::SymbologyIdentifier(id)) => id.as_str(),
        _ => "",
    };
    let mut detection = RawDetection {
        symbology,
        raw: Vec::new(),
        eci: EciEvidence::Unobserved,
        masked_stream: None,
        corners: None,
        version: None,
        ec: ec_level.and_then(parse_ec),
        mask: None,
        fnc1: Fnc1::Unobserved,
        parsed: false,
        engine: EngineKind::Rxing,
    };
    if !symbology.is_qr_family() {
        // outside the QR content contract: byte segments, else the text
        detection.raw = match metadata.get(&MetaKey::BYTE_SEGMENTS) {
            Some(MetaValue::ByteSegments(segments)) => segments.concat(),
            _ => result.getText().as_bytes().to_vec(),
        };
        // GS1 carriers per ISO/IEC 15424 symbology identifiers: DataMatrix
        // "]d2" (ECC200 FNC1 first) · Code 128 "]C1" (GS1-128); any other
        // identifier leaves the position unknown
        if matches!(identifier, "]d2" | "]C1") {
            detection.fnc1 = Fnc1::First;
        }
        return Some(detection);
    }
    match reader(symbology, ec_level)? {
        Reader::Port => {
            detection.raw = result.getRawBytes().to_vec();
            // AIM modifier (ISO 18004 Annex F): 2 · 4 · 6 = ECI in use,
            // 3/4 = FNC1 first (GS1), 5/6 = FNC1 second (an AIM application),
            // 0-2 = no FNC1 — observed; an identifier that does not parse
            // observes neither
            let modifier = identifier
                .strip_prefix("]Q")
                .and_then(|m| m.parse::<u8>().ok());
            detection.eci = match modifier {
                Some(2 | 4 | 6) => EciEvidence::Signalled,
                Some(_) => EciEvidence::NoEci,
                None => EciEvidence::Unobserved,
            };
            detection.fnc1 = match modifier {
                Some(3 | 4) => Fnc1::First,
                Some(5 | 6) => Fnc1::Second,
                Some(_) => Fnc1::None,
                None => Fnc1::Unobserved,
            };
        }
        Reader::Codewords(ec) => {
            let codewords = result.getRawBytes();
            let parsed = crate::rescue::version_of_data(codewords.len(), ec)
                .and_then(|version| bitstream::parse(codewords, version))?;
            detection.raw = parsed.bytes;
            detection.eci = EciEvidence::from_marks(parsed.eci);
            detection.fnc1 = parsed.fnc1;
            detection.parsed = true;
            // the EC level this reader states is not published: the
            // detection metadata stays exactly what it was before
            detection.ec = None;
        }
    }
    Some(detection)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{MetaKey, MetaValue, Reader, decode, observe, reader};
    use crate::engine::FormatFilter;
    use crate::engine::content::{EciEvidence, EciMark, Fnc1};
    use crate::input::LumaImage;
    use crate::report::{EcLevel, EngineKind, Symbology};
    use crate::rescue::test_symbols;

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
        assert_eq!(
            d.fnc1,
            Fnc1::None,
            "]Q1 observes: plain byte-mode QR is not FNC1"
        );
        assert!(!d.parsed, "the port states its account, never exposes it");
    }

    #[test]
    fn surfaces_the_fnc1_flag_on_a_gs1_symbol() {
        // rxing is THE GS1 engine: it reads the ]Q3 symbology identifier and
        // states FNC1 first so the payload router sends the bytes to the
        // element-string classifier. The raw bytes are the port's payload.
        let luma = render_fnc1(b"010950600013435210ABC123\x1d17261231");
        let found = decode(&luma, FormatFilter::All);
        let d = found
            .iter()
            .find(|d| d.symbology == Symbology::QrCode)
            .expect("rxing decodes the GS1 QR");
        assert_eq!(d.fnc1, Fnc1::First, "the ]Q3 identifier states FNC1 first");
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

    /// The QR-family detection of a clean render.
    fn qr(found: &[super::RawDetection]) -> &super::RawDetection {
        found
            .iter()
            .find(|d| d.symbology.is_qr_family())
            .expect("rxing decodes the clean render")
    }

    #[test]
    fn the_port_reports_the_original_bytes_and_eci_presence() {
        // ECI 4 + byte A1: rxing's text is 'Ą'; the payload stays [A1] and
        // the ]Q2 modifier says a designator exists (position unexposed)
        let luma =
            test_symbols::from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            });
        let found = decode(&luma, FormatFilter::All);
        let d = qr(&found);
        assert_eq!(d.raw, vec![0xA1], "never the UTF-8 of rxing's text");
        assert_eq!(d.eci, EciEvidence::Signalled);
        // a plain symbol: ]Q1 — no designator, stated
        let found = decode(&render(b"plain", qrcode::EcLevel::M), FormatFilter::All);
        assert_eq!(qr(&found).eci, EciEvidence::NoEci);
    }

    #[test]
    fn micro_qr_raw_is_the_symbol_byte_not_its_text() {
        // Micro QR decodes through the port only — exact bytes there too
        let luma =
            test_symbols::from_segments(qrcode::Version::Micro(3), qrcode::EcLevel::M, |b| {
                b.push_byte_data(&[0xE9])
            });
        let found = decode(&luma, FormatFilter::All);
        let d = qr(&found);
        assert_eq!(d.symbology, Symbology::MicroQrCode);
        assert_eq!(d.raw, vec![0xE9]);
        assert_eq!(d.eci, EciEvidence::NoEci);
    }

    #[test]
    fn fnc1_second_keeps_its_indicator_and_is_not_gs1() {
        let luma =
            test_symbols::from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
                b.push_fnc1_second_position(37)?;
                b.push_byte_data(b"abc")
            });
        let found = decode(&luma, FormatFilter::All);
        let d = qr(&found);
        assert_eq!(d.raw, b"37abc");
        assert_eq!(d.fnc1, Fnc1::Second, "]Q5: an AIM application, never GS1");
    }

    /// Data codewords of `12345` (numeric) + `abc` (byte) in a v1-M symbol.
    fn mixed_codewords() -> Vec<u8> {
        let mut bits = qrcode::bits::Bits::new(qrcode::Version::Normal(1));
        bits.push_numeric_data(b"12345").unwrap();
        bits.push_byte_data(b"abc").unwrap();
        bits.push_terminator(qrcode::EcLevel::M).unwrap();
        bits.into_bytes()
    }

    /// A result shaped like rxing's `ZXing`-lineage reader output.
    fn general_reader_result(codewords: Vec<u8>, ec_digit: &str) -> rxing::RXingResult {
        let mut result = rxing::RXingResult::new(
            "12345abc",
            codewords,
            Vec::new(),
            rxing::BarcodeFormat::QR_CODE,
        );
        result.putMetadata(
            MetaKey::BYTE_SEGMENTS,
            MetaValue::ByteSegments(vec![b"abc".to_vec()]),
        );
        result.putMetadata(
            MetaKey::ERROR_CORRECTION_LEVEL,
            MetaValue::ErrorCorrectionLevel(ec_digit.to_owned()),
        );
        result.putMetadata(
            MetaKey::SYMBOLOGY_IDENTIFIER,
            MetaValue::SymbologyIdentifier("]Q1".to_owned()),
        );
        result
    }

    #[test]
    fn general_reader_codewords_reparse_to_the_whole_payload() {
        // the general reader's byte segments hold 'abc' only; its raw bytes
        // are the data codewords — re-parsed, the payload is all of it
        let d = observe(&general_reader_result(mixed_codewords(), "0")).expect("parses");
        assert_eq!(d.raw, b"12345abc", "never the byte segments alone");
        assert_eq!(d.eci, EciEvidence::NoEci, "observed: no designator");
        assert_eq!(d.fnc1, Fnc1::None, "observed: no FNC1 indicator");
        assert!(d.parsed, "the scanner's parser read the codewords");
        assert_eq!(d.ec, None, "the reader's EC digit stays unpublished");
        // a stream that does not parse is no observation — never a stand-in
        assert!(observe(&general_reader_result(vec![0xFF; 16], "0")).is_none());
        // a length no version has at that EC level: unknown structure
        assert!(observe(&general_reader_result(vec![0x40; 17], "0")).is_none());
        // an EC field of neither reader's shape: the bytes' meaning is unknown
        assert!(observe(&general_reader_result(mixed_codewords(), "?")).is_none());
    }

    /// The data codewords of a symbol built from `push`, padded to the
    /// capacity of `version` at `ec` — what the general reader returns.
    fn codewords(
        version: i16,
        ec: qrcode::EcLevel,
        push: impl FnOnce(&mut qrcode::bits::Bits) -> qrcode::types::QrResult<()>,
    ) -> Vec<u8> {
        let mut bits = qrcode::bits::Bits::new(qrcode::Version::Normal(version));
        push(&mut bits).unwrap();
        bits.push_terminator(ec).unwrap();
        bits.into_bytes()
    }

    #[test]
    fn general_reader_codewords_expose_declarations_and_fnc1_positions() {
        let m = qrcode::EcLevel::M;
        // ECI 4 + byte A1: the designator is located where it takes effect
        let d = observe(&general_reader_result(
            codewords(1, m, |b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            }),
            "0",
        ))
        .expect("parses");
        assert_eq!(d.raw, vec![0xA1]);
        assert_eq!(
            d.eci,
            EciEvidence::Located(vec![EciMark { value: 4, start: 0 }])
        );
        assert_eq!(d.fnc1, Fnc1::None);
        // FNC1 first + alphanumeric "10AB%%C": '%%' transmits one '%'
        let d = observe(&general_reader_result(
            codewords(1, m, |b| {
                b.push_fnc1_first_position()?;
                b.push_alphanumeric_data(b"10AB%%C")
            }),
            "0",
        ))
        .expect("parses");
        assert_eq!(d.raw, b"10AB%C");
        assert_eq!(d.fnc1, Fnc1::First);
        // FNC1 second (37) + byte "abc": the indicator leads, AIM not GS1
        let d = observe(&general_reader_result(
            codewords(1, m, |b| {
                b.push_fnc1_second_position(37)?;
                b.push_byte_data(b"abc")
            }),
            "0",
        ))
        .expect("parses");
        assert_eq!(d.raw, b"37abc");
        assert_eq!(d.fnc1, Fnc1::Second);
        assert!(d.parsed);
    }

    #[test]
    fn general_reader_ec_digits_resolve_to_their_level_and_version() {
        // each stream's length exists at its own level only (v1-L 19, v3-M
        // 44, v1-H 9, v1-Q 13 data codewords): under any other digit no
        // version matches and the read is refused, so a remapped digit can
        // never parse at the wrong version
        use qrcode::EcLevel as L;
        let cases = [
            ("0", L::M, 3),
            ("1", L::L, 1),
            ("2", L::H, 1),
            ("3", L::Q, 1),
        ];
        for (digit, level, version) in cases {
            let stream = codewords(version, level, |b| b.push_byte_data(b"abc"));
            let d = observe(&general_reader_result(stream.clone(), digit))
                .unwrap_or_else(|| panic!("digit {digit}: v{version} {level:?} parses"));
            assert_eq!(d.raw, b"abc", "digit {digit}");
            for (other, ..) in cases.iter().filter(|(other, ..)| *other != digit) {
                assert!(
                    observe(&general_reader_result(stream.clone(), other)).is_none(),
                    "the v{version} {level:?} stream read under digit {other}"
                );
            }
        }
    }

    #[test]
    fn an_unparsable_identifier_observes_neither_eci_nor_fnc1() {
        let mut result = rxing::RXingResult::new(
            "abc",
            b"abc".to_vec(),
            Vec::new(),
            rxing::BarcodeFormat::QR_CODE,
        );
        result.putMetadata(
            MetaKey::ERROR_CORRECTION_LEVEL,
            MetaValue::ErrorCorrectionLevel("M".to_owned()),
        );
        let d = observe(&result).expect("a port read");
        assert_eq!((d.eci, d.fnc1), (EciEvidence::Unobserved, Fnc1::Unobserved));
        result.putMetadata(
            MetaKey::SYMBOLOGY_IDENTIFIER,
            MetaValue::SymbologyIdentifier("]Q1".to_owned()),
        );
        let d = observe(&result).expect("a port read");
        assert_eq!((d.eci, d.fnc1), (EciEvidence::NoEci, Fnc1::None));
    }

    #[test]
    fn reader_discrimination_is_pinned_on_real_rxing_readers() {
        // Invoke both QR readers directly on one render: the port states the
        // EC level as a letter, the general reader as its format-bits digit
        // (M = 0) — and both map to the same payload bytes.
        use rxing::Reader as _;
        let luma =
            test_symbols::from_segments(qrcode::Version::Normal(1), qrcode::EcLevel::M, |b| {
                b.push_numeric_data(b"12345")?;
                b.push_byte_data(b"abc")
            });
        let bitmap = || {
            let (w, h) = (luma.width() as usize, luma.height() as usize);
            let source = rxing::PlanarYUVLuminanceSource::new_with_all(
                luma.data().to_vec(),
                w,
                h,
                0,
                0,
                w,
                h,
                false,
                false,
            )
            .unwrap();
            rxing::BinaryBitmap::new(rxing::common::HybridBinarizer::new(source))
        };
        let port = rxing::qrcode::cpp_port::QrReader
            .decode(&mut bitmap())
            .unwrap();
        let general = rxing::qrcode::QRCodeReader.decode(&mut bitmap()).unwrap();
        let ec_level = |result: &rxing::RXingResult| match result
            .getRXingResultMetadata()
            .get(&MetaKey::ERROR_CORRECTION_LEVEL)
        {
            Some(MetaValue::ErrorCorrectionLevel(level)) => level.clone(),
            _ => String::new(),
        };
        assert_eq!(
            reader(Symbology::QrCode, Some(&ec_level(&port))),
            Some(Reader::Port)
        );
        assert_eq!(
            reader(Symbology::QrCode, Some(&ec_level(&general))),
            Some(Reader::Codewords(EcLevel::M))
        );
        assert_eq!(port.getRawBytes(), b"12345abc", "the port: the payload");
        assert_eq!(
            general.getRawBytes(),
            mixed_codewords(),
            "general: codewords"
        );
        assert_eq!(observe(&port).unwrap().raw, b"12345abc");
        assert_eq!(observe(&general).unwrap().raw, b"12345abc");
    }

    #[test]
    fn a_blank_image_yields_no_detection_and_never_panics() {
        // NotFound is a miss, not an error — the adapter returns empty. (The
        // catch_unwind boundary itself lives in engine::run_isolated, mod.rs.)
        let blank = LumaImage::new(vec![255u8; 48 * 48], 48, 48);
        assert!(decode(&blank, FormatFilter::All).is_empty());
    }

    #[test]
    fn parse_ec_maps_every_level_and_rejects_the_rest() {
        // parse_ec 47:9 / 48:9 / 50:9 — deleting the "L"/"M"/"H" arms drops the
        // level to the `_ => None` catch-all. Only "Q" was pinned (the clean-QR
        // decode uses Q); the other three arms were unexercised.
        assert_eq!(super::parse_ec("L"), Some(EcLevel::L));
        assert_eq!(super::parse_ec("M"), Some(EcLevel::M));
        assert_eq!(super::parse_ec("Q"), Some(EcLevel::Q));
        assert_eq!(super::parse_ec("H"), Some(EcLevel::H));
        assert_eq!(super::parse_ec("Z"), None);
        assert_eq!(super::parse_ec(""), None);
        assert_eq!(super::parse_ec("l"), None, "case-sensitive: only uppercase");
    }

    #[test]
    fn only_filter_actually_excludes_other_symbologies() {
        // decode 92:55 — `Some(formats_for(sym)).filter(|set| !set.is_empty())`.
        // Deleting the `!` inverts the filter: the (never-empty) format set is
        // dropped, so `possible` becomes None → the pass decodes EVERYTHING.
        // The prior Only(QrCode) test could not catch this (a QR still decodes
        // unrestricted). Restrict a QR image to a NON-QR format: the original
        // returns the QR to nobody; the mutant leaks it through.
        let luma = render(b"only-qr", qrcode::EcLevel::M);
        let found = decode(&luma, FormatFilter::Only(Symbology::Ean13));
        assert!(
            found.iter().all(|d| d.symbology != Symbology::QrCode),
            "Only(Ean13) must NOT return a QR (mutant drops the restriction): {:?}",
            found.iter().map(|d| d.symbology).collect::<Vec<_>>()
        );
    }
}
