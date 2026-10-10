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
//!   `]Q6`). A plain (`]Q1`) read of ASCII text carries that text's bytes,
//!   or a byte-order mark and the bytes rxing decoded the text from; a read
//!   that carries anything else is no payload of this reader and is
//!   refused;
//! - its `ZXing`-lineage reader (when the port fails): the RS-corrected
//!   data codewords — parsed here with the scanner's segment parser, and
//!   kept only when the parse's byte-mode segments are the reader's own.
//!
//! The decoded text is neither the payload nor the reading: content is
//! interpreted once, in `engine::content` (a port read's text only says
//! which complete double-byte decoding the engine chose, rule 1e). Version
//! and corners are NOT published from the QR paths — rqrr supplies
//! geometry; the result points stay internal (they tell symbols apart and
//! frame the re-read). Other symbologies carry the ISO/IEC 8859-1 bytes of
//! the engine's text when it fits, else its UTF-8 (spec/01 § raw), and the
//! FNC1 position their symbology identifier states. A DataBar read is a
//! GTIN-14 or nothing; the reads of the symbologies rxing invents from
//! noise (and of thinly protected PDF417 symbols) wait for confirmation at
//! an independent scale (07-pipeline § Acceptance).

use rxing::RXingResultMetadataType as MetaKey;
use rxing::RXingResultMetadataValue as MetaValue;

use super::RawDetection;
use super::content::{EciEvidence, EngineReading, Fnc1, Segments};
use super::locate::{Frame, LocateHint};
use crate::input::LumaImage;
use crate::report::{EcLevel, EngineKind, Point, Symbology};
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
        segments: Segments::NotObserved,
        engine_reading: EngineReading::None,
        location: None,
        locate: None,
        needs_confirmation: false,
    };
    if !symbology.is_qr_family() {
        let text = result.getText();
        // DataBar (RSS-14) carries a GTIN-14: any other text is no DataBar
        // the reader decoded
        if symbology == Symbology::DataBar
            && !(text.len() == 14 && text.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        // outside the QR content contract: the engine's reading, never its
        // byte segments (they omit every non-byte encodation)
        detection.raw = non_qr_raw(text);
        detection.fnc1 = identifier_fnc1(identifier);
        detection.needs_confirmation = needs_confirmation(symbology, ec_level);
        return Some(detection);
    }
    detection.location = location(result.getPoints());
    match reader(symbology, ec_level)? {
        Reader::Port => {
            let raw = result.getRawBytes();
            // a plain read of ASCII text carries that text's bytes: anything
            // else is not the port's payload (another reader's shape)
            if identifier == "]Q1"
                && result.getText().is_ascii()
                && !reads_as(raw, result.getText())
            {
                return None;
            }
            detection.raw = raw.to_vec();
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
            detection.engine_reading = EngineReading::of(&detection.raw, result.getText());
            detection.locate = locate_hint(&detection, result, modifier);
        }
        Reader::Codewords(ec) => {
            let codewords = result.getRawBytes();
            let parsed = crate::rescue::version_of_data(codewords.len(), ec)
                .and_then(|version| bitstream::parse(codewords, version))?;
            // the parse must be the reader's own: its byte-mode segments,
            // in order (none with none)
            let segments: Vec<&[u8]> = parsed
                .byte_segments
                .iter()
                .filter_map(|range| parsed.bytes.get(range.clone()))
                .collect();
            let reader_segments: Vec<&[u8]> = match metadata.get(&MetaKey::BYTE_SEGMENTS) {
                Some(MetaValue::ByteSegments(own)) => own.iter().map(Vec::as_slice).collect(),
                _ => Vec::new(),
            };
            if segments != reader_segments {
                return None;
            }
            detection.raw = parsed.bytes;
            detection.eci = EciEvidence::from_marks(parsed.eci);
            detection.fnc1 = parsed.fnc1;
            detection.segments = Segments::Observed(parsed.segments);
            detection.parsed = true;
            // the EC level this reader states is not published: the
            // detection metadata stays exactly what it was before
            detection.ec = None;
        }
    }
    Some(detection)
}

/// Whether a port read's bytes are its text: the text's own bytes or, behind
/// a byte-order mark, the text rxing decodes them to — its decoders sniff
/// and strip the mark (`encoding_rs`'s `Encoding::decode`, the call rxing
/// makes).
fn reads_as(raw: &[u8], text: &str) -> bool {
    match raw {
        [0xEF, 0xBB, 0xBF, ..] | [0xFE, 0xFF, ..] | [0xFF, 0xFE, ..] => {
            encoding_rs::UTF_8.decode(raw).0 == text
        }
        _ => raw == text.as_bytes(),
    }
}

/// Whether a non-QR read waits for a read at an independent scale before the
/// walk publishes it: UPC-E, DataBar and DataBar Expanded — rxing reads them
/// out of module noise, and a check digit or a checksum is all that guards
/// their payload — and PDF417 at error-correction level 0 or 1, or at no
/// stated level: `2^(level + 1)` EC codewords, two or four, barely refuse a
/// misread.
fn needs_confirmation(symbology: Symbology, ec_level: Option<&str>) -> bool {
    match symbology {
        Symbology::UpcE | Symbology::DataBar | Symbology::DataBarExpanded => true,
        Symbology::Pdf417 => ec_level
            .and_then(|level| level.parse::<u8>().ok())
            .is_none_or(|level| level < 2),
        _ => false,
    }
}

/// A non-QR read's bytes (spec/01 § raw): the ISO/IEC 8859-1 encoding of
/// the engine's text when every character is at most U+00FF — exactly the
/// symbol's bytes on the engine's paths without ECI — else its UTF-8.
fn non_qr_raw(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).ok())
        .collect::<Option<Vec<u8>>>()
        .unwrap_or_else(|| text.as_bytes().to_vec())
}

/// The FNC1 position an ISO/IEC 15424 symbology identifier states: Data
/// Matrix `]d2`/`]d5` and GS1-128 `]C1` first, `]d3`/`]d6`/`]C2` second,
/// `]d1`/`]d4`/`]C0`/`]C4` none; any other identifier says nothing of it.
fn identifier_fnc1(identifier: &str) -> Fnc1 {
    match identifier {
        "]d2" | "]d5" | "]C1" => Fnc1::First,
        "]d3" | "]d6" | "]C2" => Fnc1::Second,
        "]d1" | "]d4" | "]C0" | "]C4" => Fnc1::None,
        _ => Fnc1::Unobserved,
    }
}

/// A QR-family result's points as four corners: the port's four outer
/// corners, or the general reader's finder centres completed to a
/// parallelogram (its fourth point, when present, is an alignment pattern
/// near the missing corner).
fn location(points: &[rxing::Point]) -> Option<[Point; 4]> {
    let at = |i: usize| points.get(i).map(|p| Point { x: p.x, y: p.y });
    match points.len() {
        3 => {
            let (a, b, c) = (at(0)?, at(1)?, at(2)?);
            Some([
                a,
                b,
                c,
                Point {
                    x: a.x + c.x - b.x,
                    y: a.y + c.y - b.y,
                },
            ])
        }
        n if n >= 4 => Some([at(0)?, at(1)?, at(2)?, at(3)?]),
        _ => None,
    }
}

/// What the re-read needs for a port read of a QR Code symbol whose
/// modifier leaves its declarations or segments unverified: an ECI or FNC1
/// indicator (`]Q2`-`]Q6`), or no indicator over a payload that is not
/// UTF-8 (Kanji segments to find). Micro QR and rMQR are never re-read.
fn locate_hint(
    detection: &RawDetection,
    result: &rxing::RXingResult,
    modifier: Option<u8>,
) -> Option<LocateHint> {
    let modifier = modifier?;
    let unverified = match modifier {
        2..=6 => true,
        0 | 1 => std::str::from_utf8(&detection.raw).is_err(),
        _ => false,
    };
    if detection.symbology != Symbology::QrCode || !unverified || result.getPoints().len() != 4 {
        return None;
    }
    let inverted = matches!(
        result.getRXingResultMetadata().get(&MetaKey::IS_INVERTED),
        Some(MetaValue::IsInverted(true))
    );
    Some(LocateHint {
        points: detection.location?,
        frame: Frame::OuterCorners,
        inverted,
        ec: detection.ec?,
        version: None,
        modifier: Some(modifier),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

    use super::{MetaKey, MetaValue, Reader, decode, observe, reader};
    use crate::engine::FormatFilter;
    use crate::engine::content::{EciEvidence, EciMark, Fnc1, Segments};
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

    /// A result shaped like rxing's `ZXing`-lineage reader output, with the
    /// byte segments it lists (none: no metadata, as rxing omits it).
    fn general_reader_result(
        codewords: Vec<u8>,
        ec_digit: &str,
        byte_segments: &[&[u8]],
    ) -> rxing::RXingResult {
        let mut result = rxing::RXingResult::new(
            "12345abc",
            codewords,
            Vec::new(),
            rxing::BarcodeFormat::QR_CODE,
        );
        if !byte_segments.is_empty() {
            result.putMetadata(
                MetaKey::BYTE_SEGMENTS,
                MetaValue::ByteSegments(byte_segments.iter().map(|s| s.to_vec()).collect()),
            );
        }
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
        let abc: &[&[u8]] = &[b"abc"];
        let d = observe(&general_reader_result(mixed_codewords(), "0", abc)).expect("parses");
        assert_eq!(d.raw, b"12345abc", "never the byte segments alone");
        assert_eq!(d.eci, EciEvidence::NoEci, "observed: no designator");
        assert_eq!(d.fnc1, Fnc1::None, "observed: no FNC1 indicator");
        assert_eq!(d.segments, Segments::Observed(Vec::new()));
        assert!(d.parsed, "the scanner's parser read the codewords");
        assert_eq!(d.ec, None, "the reader's EC digit stays unpublished");
        assert!(d.locate.is_none(), "a parsed read needs no re-read");
        // a stream that does not parse is no observation — never a stand-in
        assert!(observe(&general_reader_result(vec![0xFF; 16], "0", abc)).is_none());
        // a length no version has at that EC level: unknown structure
        assert!(observe(&general_reader_result(vec![0x40; 17], "0", abc)).is_none());
        // an EC field of neither reader's shape: the bytes' meaning is unknown
        assert!(observe(&general_reader_result(mixed_codewords(), "?", abc)).is_none());
    }

    #[test]
    fn general_reader_codewords_must_be_the_readers_own() {
        // the parse's byte-mode segments equal the reader's own, in order,
        // or the codewords are not what it decoded — no observation
        for segments in [
            &[][..],
            &[&b"ab"[..]][..],
            &[&b"abc"[..], &b""[..]][..],
            &[&b"12345"[..], &b"abc"[..]][..],
        ] {
            assert!(
                observe(&general_reader_result(mixed_codewords(), "0", segments)).is_none(),
                "{segments:?}"
            );
        }
        // numeric data alone: none with none
        let digits = codewords(1, qrcode::EcLevel::M, |b| b.push_numeric_data(b"12345"));
        let d = observe(&general_reader_result(digits.clone(), "0", &[])).expect("parses");
        assert_eq!(d.raw, b"12345");
        assert!(observe(&general_reader_result(digits, "0", &[b"12345"])).is_none());
    }

    #[test]
    fn general_reader_codewords_must_carry_the_readers_own_byte_text() {
        // equal segment count and length, other bytes: not what it decoded
        assert!(observe(&general_reader_result(mixed_codewords(), "0", &[b"abd"])).is_none());
        let d = observe(&general_reader_result(mixed_codewords(), "0", &[b"abc"])).expect("parses");
        assert_eq!(d.raw, b"12345abc");
    }

    #[test]
    fn class_crossing_lengths_parse_only_at_their_own_level() {
        // 154 data codewords are v8-M and v10-Q, 1276 are v25-L and v40-H:
        // under the other level's digit the length names another version,
        // whose character-count widths misread the numeric segment
        use qrcode::EcLevel as L;
        let mixed = |b: &mut qrcode::bits::Bits| {
            b.push_numeric_data(b"12345")?;
            b.push_byte_data(b"abc")
        };
        for (version, level, right, wrong) in [(10, L::Q, "3", "0"), (40, L::H, "2", "1")] {
            let stream = codewords(version, level, mixed);
            assert!(
                observe(&general_reader_result(stream.clone(), wrong, &[b"abc"])).is_none(),
                "v{version} {level:?} read under digit {wrong}"
            );
            let d = observe(&general_reader_result(stream, right, &[b"abc"]))
                .unwrap_or_else(|| panic!("v{version} {level:?} parses under digit {right}"));
            assert_eq!(d.raw, b"12345abc");
        }
    }

    /// A result shaped like rxing's zxing-cpp port output: the EC level as a
    /// letter, the modifier `identifier`.
    fn port_result(text: &str, raw: Vec<u8>, identifier: &str) -> rxing::RXingResult {
        let mut result =
            rxing::RXingResult::new(text, raw, Vec::new(), rxing::BarcodeFormat::QR_CODE);
        result.putMetadata(
            MetaKey::ERROR_CORRECTION_LEVEL,
            MetaValue::ErrorCorrectionLevel("M".to_owned()),
        );
        result.putMetadata(
            MetaKey::SYMBOLOGY_IDENTIFIER,
            MetaValue::SymbologyIdentifier(identifier.to_owned()),
        );
        result
    }

    #[test]
    fn a_plain_port_read_is_kept_only_when_its_bytes_are_its_text() {
        // ]Q1 and an ASCII text: the bytes are the text's own, or the codewords
        // of a misread reader shape (refused)
        assert!(observe(&port_result("12345abc", mixed_codewords(), "]Q1")).is_none());
        let d = observe(&port_result("12345abc", b"12345abc".to_vec(), "]Q1")).expect("its text");
        assert_eq!(d.raw, b"12345abc");
        // behind a byte-order mark: the text rxing decodes the bytes to (it
        // sniffs and strips the mark), the bytes kept whole
        let bom = |rest: &[u8]| [&[0xEF, 0xBB, 0xBF][..], rest].concat();
        let url = b"https://qrc-ai.com/K1Ng2";
        let d = observe(&port_result("https://qrc-ai.com/K1Ng2", bom(url), "]Q1")).expect("BOM");
        assert_eq!(d.raw, bom(url));
        let utf16 = vec![0xFE, 0xFF, 0x00, b'a', 0x00, b'b', 0x00, b'c'];
        assert!(observe(&port_result("abc", utf16, "]Q1")).is_some());
        let utf16le = vec![0xFF, 0xFE, b'a', 0x00, b'b', 0x00, b'c', 0x00];
        assert!(observe(&port_result("abc", utf16le, "]Q1")).is_some());
        assert!(observe(&port_result("abc", bom(b"abd"), "]Q1")).is_none());
        // the rule reads only plain reads of ASCII text: a declared ECI or a
        // non-ASCII text is the content contract's to decide
        assert!(observe(&port_result("\u{104}", vec![0xA1], "]Q1")).is_some());
        assert!(observe(&port_result("abc", vec![0xA1], "]Q2")).is_some());
    }

    /// Cell 825 of the rescue-stress grid: a v6-M symbol of a 120-byte filler
    /// under a gray square over 10 % of its area, below and left of the
    /// top-right finder. rxing's port cannot read it; its `ZXing`-lineage
    /// reader can.
    fn lineage_only() -> (LumaImage, String) {
        const ALPHABET: &[u8] = b"QRCODEAI0123456789abcdefghijklmnopqrstuvwxyz-./:";
        let truth: String = (0..120)
            .map(|i| char::from(ALPHABET[i % ALPHABET.len()]))
            .collect();
        let code = qrcode::QrCode::with_version(
            truth.as_bytes(),
            qrcode::Version::Normal(6),
            qrcode::EcLevel::M,
        )
        .unwrap();
        let symbol = code
            .render::<image::Luma<u8>>()
            .quiet_zone(false)
            .module_dimensions(8, 8)
            .build();
        let side = symbol.width() as f32;
        let quiet = 48;
        let canvas_side = symbol.width() + 2 * quiet;
        let mut canvas = image::GrayImage::from_pixel(canvas_side, canvas_side, image::Luma([255]));
        image::imageops::replace(&mut canvas, &symbol, i64::from(quiet), i64::from(quiet));
        let (cx, cy) = (quiet as f32 + 0.78 * side, quiet as f32 + 0.30 * side);
        let half = (0.10 * side * side).sqrt() / 2.0;
        let span = |lo: f32, hi: f32| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "pixel bounds inside the canvas"
            )]
            let bounds = (lo.floor() as u32, hi.ceil() as u32);
            bounds.0..bounds.1
        };
        for y in span(cy - half, cy + half) {
            for x in span(cx - half, cx + half) {
                canvas.put_pixel(x, y, image::Luma([120]));
            }
        }
        let (w, h) = (canvas.width(), canvas.height());
        (LumaImage::new(canvas.into_raw(), w, h), truth)
    }

    #[test]
    fn qr_family_reads_never_depend_on_the_format_set_order() {
        // rxing walks the QR-family format set in std HashSet order, and every
        // pass builds a set with fresh random keys: each format calls the same
        // stateless port reader with the same hints, and only QR_CODE then
        // tries the lineage reader, so every order reads the same
        let port = render(b"https://qrc-ai.com/order", qrcode::EcLevel::M);
        let (lineage, truth) = lineage_only();
        let reads = |luma: &LumaImage| format!("{:?}", decode(luma, FormatFilter::QrFamily));
        for (name, luma) in [("port", &port), ("lineage", &lineage)] {
            let first = reads(luma);
            for _ in 1..16 {
                assert_eq!(reads(luma), first, "{name}");
            }
        }
        let found = decode(&lineage, FormatFilter::QrFamily);
        let d = qr(&found);
        assert!(d.parsed, "the lineage reader's codewords");
        assert_eq!(d.raw, truth.as_bytes());
    }

    #[test]
    fn identifiers_state_the_fnc1_position_of_other_symbologies() {
        // ISO/IEC 15424 symbology identifiers
        for (identifier, fnc1) in [
            ("]d2", Fnc1::First),
            ("]d5", Fnc1::First),
            ("]C1", Fnc1::First),
            ("]d3", Fnc1::Second),
            ("]d6", Fnc1::Second),
            ("]C2", Fnc1::Second),
            ("]d1", Fnc1::None),
            ("]d4", Fnc1::None),
            ("]C0", Fnc1::None),
            ("]C4", Fnc1::None),
            ("]e0", Fnc1::Unobserved),
            ("]z0", Fnc1::Unobserved),
            ("]L2", Fnc1::Unobserved),
            ("", Fnc1::Unobserved),
        ] {
            assert_eq!(super::identifier_fnc1(identifier), fnc1, "{identifier:?}");
        }
    }

    #[test]
    fn other_symbologies_carry_the_engines_reading() {
        // the ISO/IEC 8859-1 bytes of the text when they fit, else its
        // UTF-8 — never the byte segments
        assert_eq!(super::non_qr_raw("日本"), "日本".as_bytes());
        assert_eq!(super::non_qr_raw("Ã©"), vec![0xC3, 0xA9]);
        assert_eq!(super::non_qr_raw("Café"), vec![b'C', b'a', b'f', 0xE9]);
        let mut result = rxing::RXingResult::new(
            "Ã©",
            vec![0xFF],
            Vec::new(),
            rxing::BarcodeFormat::DATA_MATRIX,
        );
        result.putMetadata(
            MetaKey::BYTE_SEGMENTS,
            MetaValue::ByteSegments(vec![vec![0xFF]]),
        );
        result.putMetadata(
            MetaKey::SYMBOLOGY_IDENTIFIER,
            MetaValue::SymbologyIdentifier("]d2".to_owned()),
        );
        let d = observe(&result).expect("a Data Matrix read");
        assert_eq!(d.raw, vec![0xC3, 0xA9]);
        assert_eq!((d.eci, d.fnc1), (EciEvidence::Unobserved, Fnc1::First));
        assert!(d.location.is_none() && d.locate.is_none());
    }

    /// An rxing result of `format` with `text`, its identifier and, when
    /// given, its error-correction level as rxing's PDF417 reader states it.
    fn other_result(
        format: rxing::BarcodeFormat,
        text: &str,
        identifier: &str,
        ec_level: Option<&str>,
    ) -> rxing::RXingResult {
        let mut result =
            rxing::RXingResult::new(text, text.as_bytes().to_vec(), Vec::new(), format);
        result.putMetadata(
            MetaKey::SYMBOLOGY_IDENTIFIER,
            MetaValue::SymbologyIdentifier(identifier.to_owned()),
        );
        if let Some(level) = ec_level {
            result.putMetadata(
                MetaKey::ERROR_CORRECTION_LEVEL,
                MetaValue::ErrorCorrectionLevel(level.to_owned()),
            );
        }
        result
    }

    #[test]
    fn a_databar_text_is_exactly_fourteen_digits() {
        // DataBar (RSS-14) carries a GTIN-14: anything else is no DataBar the
        // reader decoded (0.9.3 can write a saturated fifteenth character)
        let read = |text: &str| {
            observe(&other_result(
                rxing::BarcodeFormat::RSS_14,
                text,
                "]e0",
                None,
            ))
        };
        let d = read("09506000134352").expect("a GTIN-14");
        assert_eq!(d.raw, b"09506000134352");
        for text in [
            "095060001343520",
            "0950600013435",
            "0950600013435a",
            "09506000134352 ",
            "",
        ] {
            assert!(read(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn invention_prone_reads_wait_for_confirmation() {
        use rxing::BarcodeFormat as F;
        let waits = |format, text: &str, identifier: &str, level: Option<&str>| {
            observe(&other_result(format, text, identifier, level))
                .expect("a read")
                .needs_confirmation
        };
        assert!(waits(F::UPC_E, "04917052", "]E0", None));
        assert!(waits(F::RSS_14, "09506000134352", "]e0", None));
        assert!(waits(F::RSS_EXPANDED, "(01)09955188000001", "]e0", None));
        // PDF417 at level 0 or 1 (2 or 4 EC codewords) or no stated level
        for level in [Some("0"), Some("1"), None] {
            assert!(waits(F::PDF_417, "pdf", "]L2", level), "{level:?}");
        }
        for level in ["2", "5", "8"] {
            assert!(!waits(F::PDF_417, "pdf", "]L2", Some(level)), "{level}");
        }
        // the retail and 2D symbologies keep settling on one read
        assert!(!waits(F::EAN_13, "9506000134352", "]E0", None));
        assert!(!waits(F::CODE_128, "QRCAI-0042", "]C0", None));
        assert!(!waits(F::DATA_MATRIX, "abc", "]d1", None));
        // and QR-family reads never wait: their margins decide
        let found = decode(&render(b"plain", qrcode::EcLevel::M), FormatFilter::All);
        assert!(!qr(&found).needs_confirmation);
    }

    #[test]
    fn the_pdf417_fixtures_settle_on_one_read() {
        // rxing's writer default (level 2) and zxing-cpp's: neither is a
        // symbol the walk holds for confirmation
        for name in ["pdf417.png", "pdf417-utf8-cafe.png"] {
            let path = format!(
                "{}/../../fixtures/symbology/{name}",
                env!("CARGO_MANIFEST_DIR")
            );
            let bytes = std::fs::read(&path).unwrap();
            let img = image::load_from_memory(&bytes).unwrap().to_luma8();
            let (w, h) = (img.width(), img.height());
            let found = decode(&LumaImage::new(img.into_raw(), w, h), FormatFilter::All);
            let d = found
                .iter()
                .find(|d| d.symbology == Symbology::Pdf417)
                .unwrap_or_else(|| panic!("{name}: a PDF417 read"));
            assert!(!d.needs_confirmation, "{name}");
        }
    }

    #[test]
    fn rxing_points_are_outer_corners_plus_half() {
        // the locked rxing release, 6 px modules, 4-module quiet zone: the
        // port's result points sit half a pixel inside the outer corners
        let luma = render(b"https://qrc-ai.com/points", qrcode::EcLevel::M);
        let found = decode(&luma, FormatFilter::All);
        let d = qr(&found);
        let modules = ((luma.width() / 6) - 8) as f32;
        let (lo, hi) = (24.5, 24.5 + 6.0 * modules);
        let expected = [(lo, lo), (hi, lo), (hi, hi), (lo, hi)];
        let points = d.location.expect("the port's corner points");
        for (point, (x, y)) in points.iter().zip(expected) {
            assert!(
                (point.x - x).abs() < 0.01 && (point.y - y).abs() < 0.01,
                "{points:?} vs {expected:?}"
            );
        }
    }

    #[test]
    fn only_unverified_port_reads_get_a_re_read() {
        // ]Q1 ASCII and ]Q1 UTF-8 need none; ]Q2 does (at the port's
        // corners, in the polarity it read); Micro QR never
        let plain = decode(&render(b"plain", qrcode::EcLevel::M), FormatFilter::All);
        assert!(qr(&plain).locate.is_none());
        let utf8 = decode(
            &render("café".as_bytes(), qrcode::EcLevel::M),
            FormatFilter::All,
        );
        assert!(qr(&utf8).locate.is_none());
        let luma =
            test_symbols::from_segments(qrcode::Version::Normal(3), qrcode::EcLevel::M, |b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            });
        let found = decode(&luma, FormatFilter::All);
        let hint = qr(&found)
            .locate
            .clone()
            .expect("]Q2: declarations to locate");
        assert_eq!(hint.modifier, Some(2));
        assert_eq!(hint.ec, EcLevel::M);
        assert!(!hint.inverted);
        assert_eq!(Some(hint.points), qr(&found).location);
        let micro =
            test_symbols::from_segments(qrcode::Version::Micro(3), qrcode::EcLevel::M, |b| {
                b.push_byte_data(&[0xE9])
            });
        let found = decode(&micro, FormatFilter::All);
        assert!(qr(&found).locate.is_none());
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
            &[&[0xA1]],
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
            &[],
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
            &[b"abc"],
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
            let d = observe(&general_reader_result(stream.clone(), digit, &[b"abc"]))
                .unwrap_or_else(|| panic!("digit {digit}: v{version} {level:?} parses"));
            assert_eq!(d.raw, b"abc", "digit {digit}");
            for (other, ..) in cases.iter().filter(|(other, ..)| *other != digit) {
                assert!(
                    observe(&general_reader_result(stream.clone(), other, &[b"abc"])).is_none(),
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
