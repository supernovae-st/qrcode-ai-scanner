//! Exact content — one honest observation per physical QR symbol.
//!
//! Every fixture is encoded in-test from an explicit segment stream (the
//! `qrcode` Bits API), so the bytes, the declared ECI and the mode mix are
//! known exactly. The assertions read the scanner's OWN interpretation —
//! `content.raw` / `content.text` / multiplicity — never an engine's text,
//! so they hold across engine releases.
//!
//! Contract under test (spec/01 · ISO/IEC 18004 §7.4):
//! - `raw` is the original payload bytes (ECI designators excluded) for the
//!   QR family, whichever engine read the symbol;
//! - a declared ECI decides the text (bytes before the first designator are
//!   ISO/IEC 8859-1); payloads without a designator keep the charset
//!   heuristic and its label;
//! - one physical symbol yields one detection.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode::bits::Bits;
use qrcode::types::QrResult;
use qrcode::{EcLevel, QrCode, Version};
use qrcode_ai_scanner::{
    Charset, Detection, EngineKind, ImageInput, Payload, ScanProfile, ScanReport, Scanner,
    ScoreDepth,
};

// ------------------------------------------------------------ fixtures

/// One symbol from an explicit segment stream (terminator + padding added).
fn symbol(version: Version, ec: EcLevel, push: impl FnOnce(&mut Bits) -> QrResult<()>) -> QrCode {
    let mut bits = Bits::new(version);
    push(&mut bits).expect("fixture segments fit the version");
    bits.push_terminator(ec).expect("terminator fits");
    QrCode::with_bits(bits, ec).expect("symbol builds")
}

/// A version-3 / EC M symbol — every content row fits it.
fn v3(push: impl FnOnce(&mut Bits) -> QrResult<()>) -> QrCode {
    symbol(Version::Normal(3), EcLevel::M, push)
}

/// Pixels per module of every render.
const MODULE_PX: u32 = 6;
/// Outer margin (modules) beyond the symbol's own quiet zone.
const MARGIN: u32 = 3;

/// Render to PNG: the symbol's quiet zone (4 modules, 2 for Micro QR) plus
/// an outer margin. `stray` paints one module-sized dark block in that
/// margin — outside the quiet zone, far from every finder — so the image
/// is no longer a pure symbol (its dark bounding box is not the symbol).
/// rxing 0.9.1 still reads such images with its zxing-cpp port (the pure
/// detector only runs under `PureBarcode`); the general reader that
/// returns data codewords is pinned in `rxing_engine.rs` by invoking it
/// directly.
fn png(code: &QrCode, stray: bool) -> Vec<u8> {
    png_modules(&code.to_colors(), code.width(), code.version(), stray)
}

/// [`png`] over raw module colors (row-major, `width` per row).
fn png_modules(colors: &[qrcode::Color], width: usize, version: Version, stray: bool) -> Vec<u8> {
    let quiet = match version {
        Version::Micro(_) => 2,
        Version::Normal(_) => 4,
    };
    let modules = u32::try_from(width).unwrap();
    let pad = quiet + MARGIN;
    let side = (modules + 2 * pad) * MODULE_PX;
    let mut img = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    let mut paint = |mx: u32, my: u32| {
        for dy in 0..MODULE_PX {
            for dx in 0..MODULE_PX {
                img.put_pixel(mx * MODULE_PX + dx, my * MODULE_PX + dy, image::Luma([0]));
            }
        }
    };
    for (i, color) in colors.iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let i = u32::try_from(i).unwrap();
            paint(pad + i % modules, pad + i / modules);
        }
    }
    if stray {
        paint(1, 1);
    }
    encode_png(img)
}

fn encode_png(img: image::GrayImage) -> Vec<u8> {
    let mut out = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// The module colors of a v1-M symbol whose DATA CODEWORDS are the given
/// MSB-first bit string (zero-padded to a byte, then ISO pad codewords
/// 0xEC/0x11): the encoder's own RS, placement and masking around a stream
/// its Bits API refuses to write — like a `111xxxxx` ECI designator. The
/// codewords at `corrupt` (data then EC, in block order) are inverted after
/// the RS parity is computed: errors the decoders must correct.
fn bit_stream_colors(bits: &str, corrupt: &[usize]) -> (Vec<qrcode::Color>, usize) {
    let (version, ec) = (Version::Normal(1), EcLevel::M);
    let clean: Vec<u8> = bits.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut data: Vec<u8> = clean
        .chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0u8, |byte, (i, &b)| byte | (u8::from(b == b'1') << (7 - i)))
        })
        .collect();
    let capacity = Bits::new(version).max_len(ec).unwrap() / 8;
    let mut pads = [0xEC, 0x11].into_iter().cycle();
    while data.len() < capacity {
        data.push(pads.next().unwrap());
    }
    let (mut data_codewords, mut ec_codewords) =
        qrcode::ec::construct_codewords(&data, version, ec).unwrap();
    for &index in corrupt {
        match index.checked_sub(data_codewords.len()) {
            None => data_codewords[index] ^= 0xFF,
            Some(parity) => ec_codewords[parity] ^= 0xFF,
        }
    }
    let mut canvas = qrcode::canvas::Canvas::new(version, ec);
    canvas.draw_all_functional_patterns();
    canvas.draw_data(&data_codewords, &ec_codewords);
    let colors = canvas.apply_best_mask().into_colors();
    (colors, usize::try_from(version.width()).unwrap())
}

/// A v1-M symbol from an explicit data-codeword bit string (see
/// [`bit_stream_colors`]).
fn png_from_bit_stream(bits: &str) -> Vec<u8> {
    let (colors, width) = bit_stream_colors(bits, &[]);
    png_modules(&colors, width, Version::Normal(1), false)
}

/// [`png_from_bit_stream`], left-right mirrored.
fn png_from_bit_stream_mirrored(bits: &str) -> Vec<u8> {
    let (colors, width) = bit_stream_colors(bits, &[]);
    png_modules(&mirror(&colors, width), width, Version::Normal(1), false)
}

/// Content rows scan with the camera-frame stages and no wall clock: the
/// verdict is about content, and a budget cut would make it flaky.
fn scan_content(png: &[u8]) -> ScanReport {
    let mut config = ScanProfile::Frame.config();
    config.budget_ms = None;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
        .scan(ImageInput::encoded(png))
        .unwrap()
}

/// The whole ladder (enhance, deep, rescue) with no wall clock and no
/// scoring — for rows whose content a later stage decides.
fn scan_walk(png: &[u8]) -> ScanReport {
    let mut config = ScanProfile::Full.config();
    config.budget_ms = None;
    config.score_depth = ScoreDepth::Off;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
        .scan(ImageInput::encoded(png))
        .unwrap()
}

/// Every detection as `(engines, raw, text)` — the failure message.
fn summary(report: &ScanReport) -> Vec<(Vec<EngineKind>, Vec<u8>, String)> {
    report
        .detections
        .iter()
        .map(|d| {
            (
                d.engines.clone(),
                d.content.raw.clone(),
                d.content.text.clone(),
            )
        })
        .collect()
}

/// Exactly one detection — the one physical symbol of the fixture.
fn one(report: &ScanReport) -> &Detection {
    assert_eq!(
        report.detections.len(),
        1,
        "one physical symbol must be ONE detection: {:?} (trace {:?})",
        summary(report),
        report.trace.stages
    );
    &report.detections[0]
}

fn assert_both_engines(d: &Detection) {
    assert!(
        d.engines.contains(&EngineKind::Rxing) && d.engines.contains(&EngineKind::Rqrr),
        "both engines read the symbol and reconcile: {:?}",
        d.engines
    );
}

/// Text that cannot be decoded is marked, never transcribed: `n` U+FFFD.
fn replacement(n: usize) -> String {
    "\u{FFFD}".repeat(n)
}

/// Two symbols side by side on one canvas (the first on the left).
fn png_pair(left: &QrCode, right: &QrCode) -> Vec<u8> {
    let (a, b) = (left.to_colors(), right.to_colors());
    let (wa, wb) = (left.width(), right.width());
    let quiet = 4usize;
    let px = MODULE_PX as usize;
    let height = (wa.max(wb) + 2 * quiet) * px;
    let width = (wa + wb + 3 * quiet) * px;
    let mut img = image::GrayImage::from_pixel(
        u32::try_from(width).unwrap(),
        u32::try_from(height).unwrap(),
        image::Luma([255]),
    );
    for (colors, modules, x0) in [(&a, wa, quiet), (&b, wb, wa + 2 * quiet)] {
        for (i, color) in colors.iter().enumerate() {
            if *color == qrcode::Color::Dark {
                let (mx, my) = (x0 + i % modules, quiet + i / modules);
                for dy in 0..px {
                    for dx in 0..px {
                        img.put_pixel(
                            u32::try_from(mx * px + dx).unwrap(),
                            u32::try_from(my * px + dy).unwrap(),
                            image::Luma([0]),
                        );
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// Module colors mirrored left-right (module x becomes width-1-x).
fn mirror(colors: &[qrcode::Color], width: usize) -> Vec<qrcode::Color> {
    (0..width * width)
        .map(|i| colors[(i / width) * width + (width - 1 - i % width)])
        .collect()
}

/// A left-right mirror image of `code`.
fn png_mirrored(code: &QrCode) -> Vec<u8> {
    let width = code.width();
    png_modules(
        &mirror(&code.to_colors(), width),
        width,
        code.version(),
        false,
    )
}

/// A light-on-dark render of `code` (every pixel inverted).
fn png_inverted(code: &QrCode) -> Vec<u8> {
    let mut img = image::load_from_memory(&png(code, false))
        .unwrap()
        .into_luma8();
    image::imageops::invert(&mut img);
    encode_png(img)
}

/// Pixels per module of the artistic-like renders below.
const ART_PX: u32 = 8;

/// Dot-style data modules (round dots of 4.8 px in 8 px cells), finder
/// patterns kept square — an artistic-like rendering.
fn png_dots(code: &QrCode) -> Vec<u8> {
    const QUIET: u32 = 4;
    const RADIUS: f32 = 2.4;
    let n = u32::try_from(code.width()).unwrap();
    let side = (n + 2 * QUIET) * ART_PX;
    let mut img = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    let finder = |r: u32, c: u32| (r < 8 && (c < 8 || c >= n - 8)) || (r >= n - 8 && c < 8);
    for (i, color) in code.to_colors().iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        let i = u32::try_from(i).unwrap();
        let (r, c) = (i / n, i % n);
        for dy in 0..ART_PX {
            for dx in 0..ART_PX {
                #[expect(clippy::cast_precision_loss, reason = "a few pixels")]
                let (fx, fy) = (
                    dx as f32 + 0.5 - ART_PX as f32 / 2.0,
                    dy as f32 + 0.5 - ART_PX as f32 / 2.0,
                );
                if finder(r, c) || fx * fx + fy * fy <= RADIUS * RADIUS {
                    img.put_pixel(
                        (c + QUIET) * ART_PX + dx,
                        (r + QUIET) * ART_PX + dy,
                        image::Luma([0]),
                    );
                }
            }
        }
    }
    encode_png(img)
}

/// A Gaussian-blurred render (8 px modules, `sigma` in pixels).
fn png_blurred(code: &QrCode, sigma: f32) -> Vec<u8> {
    const QUIET: u32 = 4;
    let n = u32::try_from(code.width()).unwrap();
    let side = (n + 2 * QUIET) * ART_PX;
    let mut img = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    for (i, color) in code.to_colors().iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let i = u32::try_from(i).unwrap();
            let (r, c) = (i / n, i % n);
            for dy in 0..ART_PX {
                for dx in 0..ART_PX {
                    img.put_pixel(
                        (c + QUIET) * ART_PX + dx,
                        (r + QUIET) * ART_PX + dy,
                        image::Luma([0]),
                    );
                }
            }
        }
    }
    encode_png(image::imageops::blur(&img, sigma))
}

/// The ASCII URL the artistic-like ECI 26 rows carry.
const URL: &str = "https://qrc-ai.com/K1Ng2";

/// An ECI 26 (UTF-8) symbol carrying [`URL`] in byte mode.
fn eci26_url() -> QrCode {
    v3(|b| {
        b.push_eci_designator(26)?;
        b.push_byte_data(URL.as_bytes())
    })
}

/// ISO/IEC 8859-5 bytes of "Привет".
const PRIVET_8859_5: [u8; 6] = [0xBF, 0xE0, 0xD8, 0xD2, 0xD5, 0xE2];
/// "こんにちは" in Shift JIS.
const KONNICHIWA_SJIS: [u8; 10] = [0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];

// ------------------------------------------------- A · declared ECI

#[test]
fn a1_eci4_byte_is_one_detection_with_the_declared_text() {
    // ECI 000004 = ISO/IEC 8859-2: byte A1 is 'Ą' (U+0104), never the
    // Windows-1252 '¡' the unmarked heuristic would pick.
    let code = v3(|b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    for stray in [false, true] {
        let report = scan_content(&png(&code, stray));
        let d = one(&report);
        assert_eq!(d.content.raw, vec![0xA1], "stray={stray}");
        assert_eq!(d.content.text, "\u{104}", "stray={stray}");
        assert_both_engines(d);
        assert!(
            d.corners.is_some() && d.meta.version == Some(3),
            "geometry reaches the reconciled detection (stray={stray}): {:?}",
            d.meta
        );
    }
}

#[test]
fn a2_eci4_full_profile_scores_with_geometry_sections() {
    // The scored primary must be the reconciled detection that carries
    // geometry and the sampled stream: structural, UEC and ISO sections
    // all present (they read the rqrr grid, not the stress matcher).
    let code = v3(|b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    let report = Scanner::default()
        .scan(ImageInput::encoded(&png(&code, false)))
        .unwrap();
    let d = one(&report);
    assert_eq!(d.content.text, "\u{104}");
    let score = report.score.as_ref().expect("Full scores the primary");
    assert!(score.structural.is_some(), "structural from geometry");
    assert!(score.uec.is_some(), "UEC from the sampled stream");
    assert!(score.iso15415.is_some(), "ISO card from geometry");
}

#[test]
#[ignore = "needs the stress-cell matcher to admit hits by content \
            (score/mod.rs detections_match → engine::content::ContentKey::admits)"]
fn a2_eci4_axes_match_the_utf8_control() {
    // Same layout, same text 'Ą': ECI 4 [A1] versus ECI 26 [C4 84]. Stress
    // survival must not depend on which declaration carried the text.
    let eci4 = v3(|b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    let eci26 = v3(|b| {
        b.push_eci_designator(26)?;
        b.push_byte_data("\u{104}".as_bytes())
    });
    let scan = |code: &QrCode| {
        Scanner::default()
            .scan(ImageInput::encoded(&png(code, false)))
            .unwrap()
            .score
            .expect("Full scores the primary")
    };
    let (declared, control) = (scan(&eci4), scan(&eci26));
    for axis in &control.axes {
        let mine = declared
            .axes
            .iter()
            .find(|a| a.axis == axis.axis)
            .expect("same axes ran");
        assert!(
            mine.passed >= axis.passed,
            "{:?}: ECI 4 passed {} vs control {}",
            axis.axis,
            mine.passed,
            axis.passed
        );
    }
}

#[test]
fn a3_eci7_cyrillic() {
    let code = v3(|b| {
        b.push_eci_designator(7)?;
        b.push_byte_data(&PRIVET_8859_5)
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, PRIVET_8859_5.to_vec());
    assert_eq!(d.content.text, "Привет");
    assert_both_engines(d);
}

#[test]
fn a4_two_declarations_each_govern_their_span() {
    let code = v3(|b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])?;
        b.push_eci_designator(26)?;
        b.push_byte_data("€".as_bytes())
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, vec![0xA1, 0xE2, 0x82, 0xAC]);
    assert_eq!(d.content.text, "\u{104}€");
    assert_both_engines(d);
}

#[test]
fn a5_numeric_before_the_designator_then_declared_byte() {
    let code = v3(|b| {
        b.push_numeric_data(b"12345")?;
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, b"12345\xA1".to_vec());
    assert_eq!(d.content.text, "12345\u{104}");
    assert_both_engines(d);
}

#[test]
fn a6_eci3_is_strict_iso_8859_1_not_windows_1252() {
    // 0x80 under ECI 000003 is the C1 control U+0080 — Windows-1252 would
    // read '€'. (The label assertion — never a cp1252 claim — needs the
    // additive charset contract; the text is already the declared one.)
    let code = v3(|b| {
        b.push_eci_designator(3)?;
        b.push_byte_data(&[0x35, 0x80])
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, vec![0x35, 0x80]);
    assert_eq!(d.content.text, "5\u{80}");
    assert_both_engines(d);
}

#[test]
fn a7_unsupported_eci_keeps_raw_in_one_detection() {
    let code = v3(|b| {
        b.push_eci_designator(999)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, vec![0xA1]);
    // no reading exists: one U+FFFD per byte — never a transcription
    assert_eq!(d.content.text, replacement(1));
}

#[test]
fn a8_undecodable_declared_span_never_publishes_empty_text() {
    // ECI 26 over a lone A1 (invalid UTF-8): rxing's own text drops the
    // span. The observation keeps every byte and marks the invalid one.
    let code = v3(|b| {
        b.push_eci_designator(26)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, vec![0xA1]);
    assert_eq!(d.content.text, replacement(1));
}

#[test]
fn a9_kanji_under_eci26_is_byte_compaction() {
    // ISO 18004 §7.4.2.1: under an ECI every mode only compacts bytes — the
    // Kanji pairs are UTF-8 bytes here: 'é' decodes, every ill-formed
    // subpart after it is marked (WHATWG replacement), every byte kept.
    let code = v3(|b| {
        b.push_eci_designator(26)?;
        b.push_byte_data("é".as_bytes())?;
        b.push_kanji_data(&KONNICHIWA_SJIS)
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    let mut raw = "é".as_bytes().to_vec();
    raw.extend_from_slice(&KONNICHIWA_SJIS);
    assert_eq!(d.content.raw, raw);
    // one U+FFFD per maximal ill-formed subpart (C9 82 happens to be 'ɂ')
    assert_eq!(d.content.text, String::from_utf8_lossy(&raw));
    assert!(d.content.text.starts_with('é') && d.content.text.contains('\u{FFFD}'));
}

#[test]
fn a10_eci899_binary_keeps_exact_bytes() {
    // ECI 899 is binary: never text — one U+FFFD per byte
    let bytes = [0x00, 0xFF, 0x80, 0x7F];
    let code = v3(|b| {
        b.push_eci_designator(899)?;
        b.push_byte_data(&bytes)
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, bytes.to_vec());
    assert_eq!(d.content.text, replacement(4));
}

/// A stream ISO 18004 does not define leaves no detection, whichever
/// stages run.
fn assert_refused(name: &str, png: &[u8]) {
    for (profile, report) in [("frame", scan_content(png)), ("walk", scan_walk(png))] {
        assert!(
            report.detections.is_empty(),
            "{name} ({profile}): {:?}",
            summary(&report)
        );
    }
}

/// Byte mode "abc" after the designator, as a bit string.
const BYTE_ABC: &str = "0100 00000011 01100001 01100010 01100011 0000";

#[test]
fn a11_a_111xxxxx_designator_leaves_no_detection() {
    // ECI whose first codeword is 111xxxxx (no ISO 18004 Table 4 form),
    // then byte "abc". rxing's port and zbar refuse it; rqrr reads it as a
    // one-byte designator and returns "abc" — a guess the scanner refuses
    // with them: the verified stream is not one ISO defines.
    assert_refused(
        "111xxxxx",
        &png_from_bit_stream(&format!("0111 11100000 {BYTE_ABC}")),
    );
    // control: the same stream with a well-formed designator (ECI 4) reads
    let report = scan_content(&png_from_bit_stream(&format!("0111 00000100 {BYTE_ABC}")));
    let d = one(&report);
    assert_eq!(d.content.raw, b"abc".to_vec());
    assert_both_engines(d);
}

#[test]
fn a11b_a_designator_above_999999_vetoes_the_port_read() {
    // The 110xxxxx form above the register: rxing's port maps the number
    // to "unknown" and still reads "abc" (designator signalled, never
    // exposed); rqrr decodes "abc" and the scanner's parser proves its
    // stream malformed. That proof refuses the symbol: the port's read of
    // the same bytes is vetoed — no detection.
    for (name, designator) in [
        ("1000000", "11001111 01000010 01000000"),
        ("0x1FFFFF", "11011111 11111111 11111111"),
    ] {
        assert_refused(
            name,
            &png_from_bit_stream(&format!("0111 {designator} {BYTE_ABC}")),
        );
    }
    // control: 999999, the last assignable number, reads (as unsupported)
    let report = scan_content(&png_from_bit_stream(&format!(
        "0111 11001111 01000010 00111111 {BYTE_ABC}"
    )));
    let d = one(&report);
    assert_eq!(d.content.raw, b"abc".to_vec());
    assert_eq!(d.content.text, replacement(3));
    assert_both_engines(d);
}

#[test]
fn a11c_a_mirrored_111xxxxx_designator_leaves_no_detection() {
    // rqrr decodes the mirrored symbol through its transposed grid; the
    // re-derivation reads that same grid and proves it malformed (rxing's
    // port refuses the designator in either orientation).
    assert_refused(
        "mirrored 111xxxxx",
        &png_from_bit_stream_mirrored(&format!("0111 11100000 {BYTE_ABC}")),
    );
    // control: the mirrored well-formed stream reads
    let report = scan_content(&png_from_bit_stream_mirrored(&format!(
        "0111 00000100 {BYTE_ABC}"
    )));
    let d = one(&report);
    assert_eq!(d.content.raw, b"abc".to_vec());
    assert_both_engines(d);
}

#[test]
fn a13_signalled_only_declaration_is_marked_not_guessed() {
    // A light-on-dark symbol under the camera-frame stages: rxing's port
    // reads it (inverted) and states that a designator exists without
    // locating it; rqrr reads no inverted symbol, so no stream locates the
    // designator. Until the publication rule for signalled-only content is
    // final, the bytes are marked unknown — one U+FFFD per byte, every byte
    // kept, no engine text taken on trust.
    let code = v3(|b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&png_inverted(&code));
    let d = one(&report);
    assert_eq!(d.engines, vec![EngineKind::Rxing], "{:?}", report.trace);
    assert_eq!(d.content.raw, vec![0xA1]);
    assert_eq!(d.content.text, replacement(1));
}

#[test]
fn a15_mirrored_declarations_are_located_on_the_transposed_grid() {
    // rqrr decodes a mirrored symbol through its transposed grid; the
    // re-derivation reads that same grid, so the declaration is located
    // and the declared reading published — both engines, rqrr geometry.
    let report = scan_content(&png_mirrored(&eci26_url()));
    let d = one(&report);
    assert_eq!(d.content.raw, URL.as_bytes().to_vec());
    assert_eq!(d.content.text, URL);
    assert_eq!(
        d.content.charset,
        Charset::Utf8,
        "ECI 26 reproduces as utf8"
    );
    assert!(
        matches!(&d.payload, Payload::Url { url } if url == URL),
        "{:?}",
        d.payload
    );
    assert_both_engines(d);
    assert!(d.corners.is_some());

    let code = v3(|b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&png_mirrored(&code));
    let d = one(&report);
    assert_eq!(d.content.raw, vec![0xA1]);
    assert_eq!(d.content.text, "\u{104}");
    assert_both_engines(d);
    assert!(d.corners.is_some());
}

#[test]
fn a16_corrections_rqrr_accepted_inside_the_annex_b_reserve_are_located() {
    // v1-M ECI 4 + byte A1 with five corrupted codewords: plain RS (10
    // parity codewords) corrects them and both engines read the symbol,
    // but 2·5 + 2 exceeds the Annex B budget the scanner keeps for its own
    // decodes. rqrr already accepted these codewords, so its re-derivation
    // runs plain RS and locates the declaration: 'Ą', never a marked text.
    let (colors, width) = bit_stream_colors(
        "0111 00000100 0100 00000001 10100001 0000",
        &[6, 8, 10, 12, 14],
    );
    let report = scan_content(&png_modules(&colors, width, Version::Normal(1), false));
    let d = one(&report);
    assert_eq!(d.content.raw, vec![0xA1]);
    assert_eq!(d.content.text, "\u{104}");
    assert_both_engines(d);
}

#[test]
fn a14_same_text_from_different_bytes_stays_two_detections() {
    // Merge identity is the content (symbology, raw, compatible ECI
    // evidence), not the text: a UTF-8 "é" symbol and a Windows-1252 "é"
    // symbol are two detections although both read "é".
    let utf8 = v3(|b| b.push_byte_data("é".as_bytes()));
    let latin1 = v3(|b| b.push_byte_data(&[0xE9]));
    let report = scan_content(&png_pair(&utf8, &latin1));
    let mut raws: Vec<Vec<u8>> = report
        .detections
        .iter()
        .map(|d| d.content.raw.clone())
        .collect();
    raws.sort();
    assert_eq!(
        raws,
        vec![vec![0xC3, 0xA9], vec![0xE9]],
        "{:?}",
        summary(&report)
    );
    assert!(report.detections.iter().all(|d| d.content.text == "é"));
}

#[test]
fn a12_faithful_declarations_keep_their_legacy_label() {
    let cases: [(&str, QrCode, &[u8], &str, Charset); 4] = [
        (
            "ECI 26 UTF-8",
            v3(|b| {
                b.push_eci_designator(26)?;
                b.push_byte_data("\u{104}".as_bytes())
            }),
            "\u{104}".as_bytes(),
            "\u{104}",
            Charset::Utf8,
        ),
        (
            "ECI 20 Shift_JIS",
            v3(|b| {
                b.push_eci_designator(20)?;
                b.push_byte_data(&KONNICHIWA_SJIS)
            }),
            &KONNICHIWA_SJIS,
            "こんにちは",
            Charset::ShiftJis,
        ),
        (
            "ECI 3 A1",
            v3(|b| {
                b.push_eci_designator(3)?;
                b.push_byte_data(&[0xA1])
            }),
            &[0xA1],
            "¡",
            Charset::Latin1,
        ),
        (
            "unmarked A1",
            v3(|b| b.push_byte_data(&[0xA1])),
            &[0xA1],
            "¡",
            Charset::Latin1,
        ),
    ];
    for (name, code, raw, text, charset) in cases {
        let report = scan_content(&png(&code, false));
        let d = one(&report);
        assert_eq!(d.content.raw, raw.to_vec(), "{name}");
        assert_eq!(d.content.text, text, "{name}");
        assert_eq!(d.content.charset, charset, "{name}");
        assert_both_engines(d);
    }
}

// --------------------------------------- B · unmarked bytes (heuristic)

#[test]
fn b1_unmarked_c1_byte_keeps_the_heuristic_reading() {
    let code = v3(|b| b.push_byte_data(&[0x35, 0x80]));
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, vec![0x35, 0x80]);
    assert_eq!(d.content.text, "5€");
    assert_eq!(d.content.charset, Charset::Latin1);
    assert_both_engines(d);
}

#[test]
fn b2_unmarked_binary_is_one_detection_with_exact_bytes() {
    let bytes = [0x00, 0xFF, 0x80, 0x7F];
    let code = v3(|b| b.push_byte_data(&bytes));
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, bytes.to_vec());
    assert_both_engines(d);
}

#[test]
fn b3_unmarked_utf8_and_windows_1252_keep_their_labels() {
    let utf8 = "héllo → 🦋";
    let report = scan_content(&png(&v3(|b| b.push_byte_data(utf8.as_bytes())), false));
    let d = one(&report);
    assert_eq!(
        (d.content.text.as_str(), d.content.charset),
        (utf8, Charset::Utf8)
    );
    assert_eq!(d.content.raw, utf8.as_bytes().to_vec());

    let quebec = [0x51, 0x75, 0xE9, 0x62, 0x65, 0x63]; // "Québec" in Windows-1252
    let report = scan_content(&png(&v3(|b| b.push_byte_data(&quebec)), false));
    let d = one(&report);
    assert_eq!(
        (d.content.text.as_str(), d.content.charset),
        ("Québec", Charset::Latin1)
    );
    assert_eq!(d.content.raw, quebec.to_vec());
}

#[test]
fn b4_short_unmarked_latin1_is_one_detection() {
    // rxing guesses a charset per unmarked segment; the scanner must not
    // let that guess split the symbol — the bytes are the identity.
    for (bytes, text) in [
        (&[0xA9, 0x20, 0x32, 0x30, 0x32, 0x36][..], "© 2026"),
        (&[0xA1, 0x48, 0x6F, 0x6C, 0x61, 0x21][..], "¡Hola!"),
    ] {
        let report = scan_content(&png(&v3(|b| b.push_byte_data(bytes)), false));
        let d = one(&report);
        assert_eq!(d.content.raw, bytes.to_vec(), "{text}");
        assert_eq!(d.content.text, text);
        assert_eq!(d.content.charset, Charset::Latin1, "{text}");
        assert_both_engines(d);
    }
}

// ------------------------------------------------------ C · mixed modes

#[test]
fn c1_numeric_then_byte_is_whole_on_pure_and_non_pure_renders() {
    // Pure and non-pure: whichever reader takes the symbol, raw is the
    // whole payload (the general reader's byte segments alone are 'abc').
    let code = v3(|b| {
        b.push_numeric_data(b"12345")?;
        b.push_byte_data(b"abc")
    });
    for stray in [false, true] {
        let report = scan_content(&png(&code, stray));
        let d = one(&report);
        assert_eq!(d.content.raw, b"12345abc".to_vec(), "stray={stray}");
        assert_eq!(d.content.text, "12345abc", "stray={stray}");
        assert_both_engines(d);
    }
}

#[test]
fn c2_byte_then_kanji_merges_as_shift_jis() {
    let code = v3(|b| {
        b.push_byte_data(b"abc")?;
        b.push_kanji_data(&KONNICHIWA_SJIS)
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    let mut raw = b"abc".to_vec();
    raw.extend_from_slice(&KONNICHIWA_SJIS);
    assert_eq!(d.content.raw, raw);
    assert_eq!(d.content.text, "abcこんにちは");
    assert_eq!(d.content.charset, Charset::ShiftJis);
    assert_both_engines(d);
}

#[test]
fn c3_kanji_9ffd_to_9fff_keep_their_original_bytes() {
    // ISO 18004 §7.4.6 (and rxing's port, and the scanner's parser) map
    // the 13-bit values 5949-5951 to Shift JIS 9F FD..9F FF; rqrr 0.10.1
    // tests `value + 0x8140 <= 0x9FFC` and writes DF FD..DF FF. The
    // RS-verified re-derivation of rqrr's own grid is the truth: one
    // detection with the original bytes, both engines, rqrr geometry —
    // unmarked and under a declaration alike.
    for low in [0xFD_u8, 0xFE, 0xFF] {
        let pair = [0x9F, low];
        let iso_8859_2 = encoding_rs::ISO_8859_2
            .decode_without_bom_handling(&pair)
            .0
            .into_owned();
        let cases = [
            (
                "unmarked",
                v3(|b| b.push_kanji_data(&pair)),
                encoding_rs::WINDOWS_1252
                    .decode_without_bom_handling(&pair)
                    .0
                    .into_owned(),
            ),
            (
                "ECI 4",
                v3(|b| {
                    b.push_eci_designator(4)?;
                    b.push_kanji_data(&pair)
                }),
                iso_8859_2,
            ),
            (
                "ECI 26",
                v3(|b| {
                    b.push_eci_designator(26)?;
                    b.push_kanji_data(&pair)
                }),
                String::from_utf8_lossy(&pair).into_owned(),
            ),
        ];
        for (name, code, text) in cases {
            let report = scan_content(&png(&code, false));
            let d = one(&report);
            assert_eq!(d.content.raw, pair.to_vec(), "{name} 9F {low:02X}");
            assert_eq!(d.content.text, text, "{name} 9F {low:02X}");
            assert_both_engines(d);
            assert!(d.corners.is_some(), "{name} 9F {low:02X}: rqrr geometry");
        }
    }
}

// ------------------------------------------------------------- D · FNC1

#[test]
fn d1_fnc1_first_percent_is_the_gs_separator() {
    let code = v3(|b| {
        b.push_fnc1_first_position()?;
        b.push_alphanumeric_data(b"10ABC%2112")
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, b"10ABC\x1d2112".to_vec());
    assert!(matches!(d.payload, Payload::Gs1 { .. }), "{:?}", d.payload);
}

#[test]
fn d2_fnc1_first_double_percent_is_one_literal_percent() {
    // ISO 18004 §7.4.8.2: '%%' transmits as one '%'. Whichever decode path
    // publishes the symbol, the payload is exactly that — never two GS.
    let code = v3(|b| {
        b.push_fnc1_first_position()?;
        b.push_alphanumeric_data(b"10AB%%C")
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, b"10AB%C".to_vec(), "{:?}", summary(&report));
    assert_eq!(d.content.text, "10AB%C");
}

#[test]
fn d3_fnc1_second_keeps_the_application_indicator_and_is_not_gs1() {
    // ISO 18004 §7.4.8.3: the AIM application indicator is transmitted
    // first ("37"); FNC1-second data is not a GS1 element string.
    let code = v3(|b| {
        b.push_fnc1_second_position(37)?;
        b.push_byte_data(b"abc")
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, b"37abc".to_vec());
    assert_eq!(d.content.text, "37abc");
    assert!(!matches!(d.payload, Payload::Gs1 { .. }), "{:?}", d.payload);
}

/// The GTIN element string the GS1 rows carry.
const GS1_GTIN: &str = "0109506000134352";

#[test]
fn d4_eci26_gs1_is_located_through_rqrrs_grid() {
    // rqrr cannot decode FNC1 symbols, so its grid is a rescue candidate,
    // and rxing's port only signals the ECI. Re-deriving that candidate
    // (clean RS) locates ECI 26 over the port's bytes: the element string
    // reads, the payload is GS1 with its GTIN.
    let code = v3(|b| {
        b.push_eci_designator(26)?;
        b.push_fnc1_first_position()?;
        b.push_numeric_data(GS1_GTIN.as_bytes())
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, GS1_GTIN.as_bytes().to_vec());
    assert_eq!(d.content.text, GS1_GTIN);
    match &d.payload {
        Payload::Gs1 {
            gtin, conformant, ..
        } => {
            assert_eq!(gtin.as_deref(), Some("09506000134352"));
            assert!(*conformant, "{:?}", d.payload);
        }
        other => panic!("not GS1: {other:?}"),
    }
    assert!(d.corners.is_some(), "the candidate's grid is the symbol");
}

#[test]
fn d5_fnc1_second_then_first_vetoes_the_port_read() {
    // FNC1 in second position, then FNC1 in first position: no symbol ISO
    // 18004 defines. rxing's port publishes it as GS1 with the AIM digits
    // leading the element string — confidently wrong; rqrr cannot decode
    // FNC1, and the scanner's parser proves its grid malformed. The proof
    // vetoes the port's read of the same bytes: no detection.
    assert_refused(
        "FNC1 second then first",
        &png_from_bit_stream(&format!("1001 00100101 0101 {BYTE_ABC}")),
    );
    // control: FNC1 second alone reads, indicator first, never GS1
    let report = scan_content(&png_from_bit_stream(&format!("1001 00100101 {BYTE_ABC}")));
    let d = one(&report);
    assert_eq!(d.content.raw, b"37abc".to_vec());
    assert!(!matches!(d.payload, Payload::Gs1 { .. }), "{:?}", d.payload);
}

#[test]
fn d6_plain_and_fnc1_first_twins_stay_two_detections() {
    // Equal transmitted bytes, but one symbol declares no FNC1 and the
    // other FNC1 in first position: every QR read observes the position,
    // so they are two contents — one GS1 element string, one plain.
    let plain = v3(|b| b.push_numeric_data(GS1_GTIN.as_bytes()));
    let gs1 = v3(|b| {
        b.push_fnc1_first_position()?;
        b.push_numeric_data(GS1_GTIN.as_bytes())
    });
    let report = scan_content(&png_pair(&plain, &gs1));
    assert_eq!(report.detections.len(), 2, "{:?}", summary(&report));
    assert!(
        report
            .detections
            .iter()
            .all(|d| d.content.raw == GS1_GTIN.as_bytes())
    );
    // the plain twin carries GS1 syntax without the mode header: the
    // classifier flags it non-conformant; only the FNC1 one conforms
    let conformant: Vec<bool> = report
        .detections
        .iter()
        .map(|d| {
            matches!(
                d.payload,
                Payload::Gs1 {
                    conformant: true,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        conformant.iter().filter(|&&c| c).count(),
        1,
        "{:?}",
        report
            .detections
            .iter()
            .map(|d| &d.payload)
            .collect::<Vec<_>>()
    );
}

#[test]
fn d7_plain_and_fnc1_second_twins_stay_two_detections() {
    // "37abc" in byte mode and FNC1 second (37) + "abc" transmit equal
    // bytes; the observed positions (none · second) keep them two.
    let plain = v3(|b| b.push_byte_data(b"37abc"));
    let aim = v3(|b| {
        b.push_fnc1_second_position(37)?;
        b.push_byte_data(b"abc")
    });
    let report = scan_content(&png_pair(&plain, &aim));
    assert_eq!(report.detections.len(), 2, "{:?}", summary(&report));
    assert!(
        report
            .detections
            .iter()
            .all(|d| d.content.raw == b"37abc" && !matches!(d.payload, Payload::Gs1 { .. }))
    );
}

// --------------------------------------------- E · single-engine paths

#[test]
fn e1_micro_qr_byte_keeps_the_original_byte() {
    // Micro QR decodes through rxing only — its raw must still be the
    // symbol's byte, not the UTF-8 of rxing's text. (M3-M: rxing 0.9.1
    // reads none of this encoder's M3-L symbols at all.)
    let code = symbol(Version::Micro(3), EcLevel::M, |b| b.push_byte_data(&[0xE9]));
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_eq!(d.content.raw, vec![0xE9], "{:?}", summary(&report));
    assert_eq!(d.content.text, "é");
    assert_eq!(d.content.charset, Charset::Latin1);
}

/// Mid-gray disk over the symbol center: modules under it read near the
/// binarization threshold — the erasure regime the rescue stage targets.
fn occluded_png(code: &QrCode, area_pct: f32) -> Vec<u8> {
    const PX: u32 = 8;
    const QUIET: u32 = 6;
    let modules = u32::try_from(code.width()).unwrap();
    let side = (modules + 2 * QUIET) * PX;
    let mut img = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    for (i, color) in code.to_colors().iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let i = u32::try_from(i).unwrap();
            let (mx, my) = (QUIET + i % modules, QUIET + i / modules);
            for dy in 0..PX {
                for dx in 0..PX {
                    img.put_pixel(mx * PX + dx, my * PX + dy, image::Luma([0]));
                }
            }
        }
    }
    #[expect(clippy::cast_precision_loss, reason = "a few hundred pixels")]
    let (center, symbol_side) = (side as f32 / 2.0, (modules * PX) as f32);
    let radius = (area_pct / 100.0 * symbol_side * symbol_side / std::f32::consts::PI).sqrt();
    for y in 0..side {
        for x in 0..side {
            #[expect(clippy::cast_precision_loss, reason = "a few hundred pixels")]
            let (dx, dy) = (x as f32 + 0.5 - center, y as f32 + 0.5 - center);
            if dx * dx + dy * dy <= radius * radius {
                img.put_pixel(x, y, image::Luma([120]));
            }
        }
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// Occluded area (percent of the symbol) where both engines give up on the
/// v5-H ECI fixture and the erasure rescue still recovers it.
const RESCUE_ONLY_AREA_PCT: f32 = 24.0;

#[test]
fn e4_rescue_only_eci4_reads_the_declaration() {
    let code = symbol(Version::Normal(5), EcLevel::H, |b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&occluded_png(&code, RESCUE_ONLY_AREA_PCT));
    let d = one(&report);
    assert_eq!(d.engines, vec![EngineKind::Rescue], "{:?}", report.trace);
    assert_eq!(d.content.raw, vec![0xA1]);
    assert_eq!(d.content.text, "\u{104}");
}

#[test]
fn e5_artistic_eci26_urls_are_located_by_a_later_stage() {
    // rxing reads these renders first and only signals the ECI; rqrr reads
    // them in a later stage. The walk does not stop on signalled-only
    // reads, so the declaration is located and the URL publishes.
    for (name, png) in [
        ("dots", png_dots(&eci26_url())),
        ("blur 2.5", png_blurred(&eci26_url(), 2.5)),
        ("blur 3.5", png_blurred(&eci26_url(), 3.5)),
    ] {
        let report = scan_walk(&png);
        let d = one(&report);
        assert_eq!(d.content.raw, URL.as_bytes().to_vec(), "{name}");
        assert_eq!(d.content.text, URL, "{name}: {:?}", report.trace);
        assert!(
            matches!(&d.payload, Payload::Url { url } if url == URL),
            "{name}: {:?}",
            d.payload
        );
    }
}

// ---------------------------------------------------- F · invariants

/// Every single-symbol fixture of this file, by name.
fn matrix() -> Vec<(&'static str, QrCode)> {
    vec![
        (
            "eci4",
            v3(|b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])
            }),
        ),
        (
            "eci7",
            v3(|b| {
                b.push_eci_designator(7)?;
                b.push_byte_data(&PRIVET_8859_5)
            }),
        ),
        (
            "eci4+eci26",
            v3(|b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1])?;
                b.push_eci_designator(26)?;
                b.push_byte_data("€".as_bytes())
            }),
        ),
        (
            "eci26-invalid",
            v3(|b| {
                b.push_eci_designator(26)?;
                b.push_byte_data(&[0xA1])
            }),
        ),
        (
            "eci899",
            v3(|b| {
                b.push_eci_designator(899)?;
                b.push_byte_data(&[0x00, 0xFF, 0x80, 0x7F])
            }),
        ),
        ("unmarked-c1", v3(|b| b.push_byte_data(&[0x35, 0x80]))),
        (
            "unmarked-binary",
            v3(|b| b.push_byte_data(&[0x00, 0xFF, 0x80, 0x7F])),
        ),
        (
            "numeric+byte",
            v3(|b| {
                b.push_numeric_data(b"12345")?;
                b.push_byte_data(b"abc")
            }),
        ),
        (
            "byte+kanji",
            v3(|b| {
                b.push_byte_data(b"abc")?;
                b.push_kanji_data(&KONNICHIWA_SJIS)
            }),
        ),
        (
            "eci20",
            v3(|b| {
                b.push_eci_designator(20)?;
                b.push_byte_data(&KONNICHIWA_SJIS)
            }),
        ),
    ]
}

/// Decode `raw` the way a legacy label promises.
fn decode_with_label(raw: &[u8], charset: Charset) -> Option<String> {
    match charset {
        Charset::Utf8 => String::from_utf8(raw.to_vec()).ok(),
        Charset::ShiftJis => encoding_rs::SHIFT_JIS
            .decode_without_bom_handling_and_without_replacement(raw)
            .map(std::borrow::Cow::into_owned),
        Charset::Latin1 => Some(
            encoding_rs::WINDOWS_1252
                .decode_without_bom_handling(raw)
                .0
                .into_owned(),
        ),
        _ => None,
    }
}

#[test]
fn f1_global_invariants_over_the_matrix() {
    // Fixtures whose text has a faithful legacy label; the declared-only
    // readings (ISO-8859-2/-5, multi-ECI, renderings) get their honest
    // label from the additive charset contract.
    let labelled = [
        "unmarked-c1",
        "unmarked-binary",
        "numeric+byte",
        "byte+kanji",
        "eci20",
    ];
    for (name, code) in matrix() {
        for stray in [false, true] {
            let bytes = png(&code, stray);
            let report = scan_content(&bytes);
            assert!(report.detections.len() <= 16);
            for (i, a) in report.detections.iter().enumerate() {
                assert!(
                    !a.content.text.is_empty() || a.content.raw.is_empty(),
                    "{name}: empty text over non-empty raw"
                );
                assert!(!a.content.raw.is_empty(), "{name}: empty raw");
                for b in &report.detections[i + 1..] {
                    assert!(
                        (a.symbology, &a.content.raw) != (b.symbology, &b.content.raw),
                        "{name} (stray={stray}): one symbol, two detections with equal raw"
                    );
                }
                if labelled.contains(&name) {
                    assert_eq!(
                        decode_with_label(&a.content.raw, a.content.charset).as_deref(),
                        Some(a.content.text.as_str()),
                        "{name}: label {:?} must reproduce the text",
                        a.content.charset
                    );
                }
            }
            let again = scan_content(&bytes);
            assert_eq!(
                again.detections, report.detections,
                "{name}: scans are deterministic"
            );
        }
    }
}
