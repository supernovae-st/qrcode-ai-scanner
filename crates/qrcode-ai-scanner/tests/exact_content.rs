//! Exact content — the content contract, row by row (spec/01 § Content,
//! 07-pipeline properties 2 and 6).
//!
//! Every QR fixture is encoded in-test from an explicit segment stream (the
//! `qrcode` Bits API, or hand-assembled data codewords for streams that API
//! refuses), so the bytes, the declared ECI, the FNC1 position and the mode
//! mix are known exactly; the non-QR rows read committed zxing-cpp / segno
//! renders under `fixtures/symbology/`. The assertions compare the WIRE shape
//! of `content` (and `payload`) with the contract's normative values, key for
//! key, never an engine's text — so they hold across engine releases.
//!
//! A row's lowercase prefix (`a1`, `d4`, `r1_to_r6`, …) is its label in this
//! file; rows without one are named by what they pin.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode::bits::Bits;
use qrcode::types::QrResult;
use qrcode::{EcLevel, QrCode, Version};
use qrcode_ai_scanner::{
    Detection, EngineKind, ImageInput, Payload, ScanProfile, ScanReport, Scanner, ScoreDepth,
    Symbology,
};
use serde_json::Value;

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

/// A low-contrast render filling the whole canvas: dark modules at luma
/// `dark`, everything else — the 4-module quiet zone included — at
/// `light`, `px` pixels per module (the 120/150 rows: rqrr reads them,
/// rxing does not; a white surround would defeat rqrr's threshold too).
fn png_contrast(colors: &[qrcode::Color], width: usize, px: u32, dark: u8, light: u8) -> Vec<u8> {
    let modules = u32::try_from(width).unwrap();
    let side = (modules + 8) * px;
    let mut img = image::GrayImage::from_pixel(side, side, image::Luma([light]));
    for (i, color) in colors.iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let i = u32::try_from(i).unwrap();
            let (mx, my) = (4 + i % modules, 4 + i / modules);
            for dy in 0..px {
                for dx in 0..px {
                    img.put_pixel(mx * px + dx, my * px + dy, image::Luma([dark]));
                }
            }
        }
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

/// The module colors of a symbol whose DATA CODEWORDS are the given
/// MSB-first bit string (zero-padded to a byte, then ISO pad codewords
/// 0xEC/0x11): the encoder's own RS, placement and masking around a stream
/// its Bits API refuses to write — like a `111xxxxx` ECI designator or a
/// Hanzi segment. The codewords at `corrupt` (data then EC, in block order)
/// are inverted after the RS parity is computed: errors the decoders must
/// correct.
fn stream_colors(
    version: Version,
    ec: EcLevel,
    bits: &str,
    corrupt: &[usize],
) -> (Vec<qrcode::Color>, usize) {
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
    assert!(data.len() <= capacity, "the stream fits {version:?} {ec:?}");
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

/// [`stream_colors`] for a v1-M symbol.
fn bit_stream_colors(bits: &str, corrupt: &[usize]) -> (Vec<qrcode::Color>, usize) {
    stream_colors(Version::Normal(1), EcLevel::M, bits, corrupt)
}

/// A v1-M symbol from an explicit data-codeword bit string.
fn png_from_bit_stream(bits: &str) -> Vec<u8> {
    let (colors, width) = bit_stream_colors(bits, &[]);
    png_modules(&colors, width, Version::Normal(1), false)
}

/// [`png_from_bit_stream`], left-right mirrored.
fn png_from_bit_stream_mirrored(bits: &str) -> Vec<u8> {
    let (colors, width) = bit_stream_colors(bits, &[]);
    png_modules(&mirror(&colors, width), width, Version::Normal(1), false)
}

/// [`png_from_bit_stream`], light-on-dark (every pixel inverted).
fn png_from_bit_stream_inverted(bits: &str) -> Vec<u8> {
    invert_png(&png_from_bit_stream(bits))
}

/// Every pixel of a PNG inverted.
fn invert_png(png: &[u8]) -> Vec<u8> {
    let mut img = image::load_from_memory(png).unwrap().into_luma8();
    image::imageops::invert(&mut img);
    encode_png(img)
}

/// Hand-assembled segment streams (versions 1-9 count widths) — for mode
/// mixes the `qrcode` Bits API cannot write (Hanzi, GB/T 18284 subset 1).
#[derive(Default)]
struct Stream(String);

impl Stream {
    fn bits(mut self, value: u32, n: u32) -> Self {
        for i in (0..n).rev() {
            self.0.push(if value >> i & 1 == 1 { '1' } else { '0' });
        }
        self
    }

    fn byte(self, data: &[u8]) -> Self {
        let mut s = self
            .bits(0b0100, 4)
            .bits(u32::try_from(data.len()).unwrap(), 8);
        for &b in data {
            s = s.bits(u32::from(b), 8);
        }
        s
    }

    /// Shift JIS pairs in Kanji mode (ISO/IEC 18004 §7.4.6).
    fn kanji(self, sjis: &[u8]) -> Self {
        let mut s = self
            .bits(0b1000, 4)
            .bits(u32::try_from(sjis.len() / 2).unwrap(), 8);
        for pair in sjis.chunks(2) {
            let p = u32::from(pair[0]) << 8 | u32::from(pair[1]);
            let v = if p <= 0x9FFC { p - 0x8140 } else { p - 0xC140 };
            s = s.bits((v >> 8) * 0xC0 + (v & 0xFF), 13);
        }
        s
    }

    /// GB 2312 pairs in Hanzi mode, subset 1 (GB/T 18284).
    fn hanzi(self, gb: &[u8]) -> Self {
        let mut s = self
            .bits(0b1101, 4)
            .bits(1, 4)
            .bits(u32::try_from(gb.len() / 2).unwrap(), 8);
        for pair in gb.chunks(2) {
            let p = u32::from(pair[0]) << 8 | u32::from(pair[1]);
            let v = if p <= 0xAAFE { p - 0xA1A1 } else { p - 0xA6A1 };
            s = s.bits((v >> 8) * 0x60 + (v & 0xFF), 13);
        }
        s
    }

    fn end(self) -> String {
        self.bits(0, 4).0
    }
}

/// A v2-M symbol from a [`Stream`].
fn stream_png(stream: Stream) -> Vec<u8> {
    let (colors, width) = stream_colors(Version::Normal(2), EcLevel::M, &stream.end(), &[]);
    png_modules(&colors, width, Version::Normal(2), false)
}

/// Content rows scan with the camera-frame stages and no wall clock: the
/// verdict is about content, and a budget cut would make it flaky.
fn scan_content(png: &[u8]) -> ScanReport {
    scan_with(png, ScanProfile::Frame, None)
}

/// The whole ladder (enhance, deep, rescue) with no wall clock and no
/// scoring — for rows whose content a later stage decides.
fn scan_walk(png: &[u8]) -> ScanReport {
    scan_with(png, ScanProfile::Full, None)
}

/// A scan under `profile` (scoring off) and an explicit budget.
fn scan_with(png: &[u8], profile: ScanProfile, budget_ms: Option<u64>) -> ScanReport {
    let mut config = profile.config();
    config.budget_ms = budget_ms;
    config.score_depth = ScoreDepth::Off;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
        .scan(ImageInput::encoded(png))
        .unwrap()
}

/// A committed fixture's bytes.
fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

/// Every detection as `(symbology, engines, content)` — the failure message.
fn summary(report: &ScanReport) -> Vec<(Symbology, Vec<EngineKind>, Value)> {
    report
        .detections
        .iter()
        .map(|d| (d.symbology, d.engines.clone(), content(d)))
        .collect()
}

/// Exactly one detection — the one physical symbol of the fixture.
#[track_caller]
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

#[track_caller]
fn assert_both_engines(d: &Detection) {
    assert!(
        d.engines.contains(&EngineKind::Rxing) && d.engines.contains(&EngineKind::Rqrr),
        "both engines read the symbol and reconcile: {:?}",
        d.engines
    );
}

/// The wire shape of a detection's content (spec/01 § Content).
fn content(d: &Detection) -> Value {
    serde_json::to_value(&d.content).expect("content serializes")
}

/// The wire shape of a detection's payload (spec/05).
fn payload(d: &Detection) -> Value {
    serde_json::to_value(&d.payload).expect("payload serializes")
}

/// A content object, written as its wire JSON.
fn wire(json: &str) -> Value {
    serde_json::from_str(json).expect("a valid JSON literal")
}

/// The detection's content equals the contract value, key for key (an
/// absent `eci` / `fnc1` key included).
#[track_caller]
fn assert_content(d: &Detection, expected: &str, ctx: &str) {
    assert_eq!(content(d), wire(expected), "{ctx}: engines {:?}", d.engines);
}

/// `n` U+FFFD — what text the scanner could not read becomes.
fn replacement(n: usize) -> String {
    "\u{FFFD}".repeat(n)
}

/// Two symbols side by side on one canvas (the first on the left).
fn png_pair(left: &QrCode, right: &QrCode) -> Vec<u8> {
    png_row(&[
        (left.to_colors(), left.width()),
        (right.to_colors(), right.width()),
    ])
}

/// Symbols side by side on one canvas, left to right, from raw module
/// colors (row-major, `width` per row): 4-module quiet zones around and
/// between them.
fn png_row(symbols: &[(Vec<qrcode::Color>, usize)]) -> Vec<u8> {
    let quiet = 4usize;
    let px = MODULE_PX as usize;
    let tallest = symbols.iter().map(|(_, width)| *width).max().unwrap_or(0);
    let width = (symbols
        .iter()
        .map(|(_, width)| width + quiet)
        .sum::<usize>()
        + quiet)
        * px;
    let height = (tallest + 2 * quiet) * px;
    let mut img = image::GrayImage::from_pixel(
        u32::try_from(width).unwrap(),
        u32::try_from(height).unwrap(),
        image::Luma([255]),
    );
    let mut x0 = quiet;
    for (colors, modules) in symbols {
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
        x0 += modules + quiet;
    }
    encode_png(img)
}

/// Symbols side by side, each in its own region (its 4-module quiet zone
/// included), a region rendered light-on-dark when flagged.
fn png_row_styled(symbols: &[(Vec<qrcode::Color>, usize, bool)]) -> Vec<u8> {
    let quiet = 4usize;
    let px = MODULE_PX as usize;
    let tallest = symbols
        .iter()
        .map(|(_, width, _)| *width)
        .max()
        .unwrap_or(0);
    let width = (symbols
        .iter()
        .map(|(_, width, _)| width + 2 * quiet)
        .sum::<usize>())
        * px;
    let height = (tallest + 2 * quiet) * px;
    let mut img = image::GrayImage::from_pixel(
        u32::try_from(width).unwrap(),
        u32::try_from(height).unwrap(),
        image::Luma([255]),
    );
    let mut x0 = 0usize;
    for (colors, modules, inverted) in symbols {
        let (dark, light) = if *inverted { (255, 0) } else { (0, 255) };
        let span = modules + 2 * quiet;
        for my in 0..(tallest + 2 * quiet) {
            for mx in 0..span {
                let inside = (quiet..quiet + modules).contains(&mx)
                    && (quiet..quiet + modules).contains(&my);
                let is_dark =
                    inside && colors[(my - quiet) * modules + (mx - quiet)] == qrcode::Color::Dark;
                let luma = if is_dark { dark } else { light };
                for dy in 0..px {
                    for dx in 0..px {
                        img.put_pixel(
                            u32::try_from((x0 + mx) * px + dx).unwrap(),
                            u32::try_from(my * px + dy).unwrap(),
                            image::Luma([luma]),
                        );
                    }
                }
            }
        }
        x0 += span;
    }
    encode_png(img)
}

/// A transparent design of a v1 symbol: dark modules opaque mid-gray
/// (luma 120 — dark content, so the `auto` flatten goes over white first),
/// every other pixel fully transparent. Over black — the fallback walk's
/// background — the same symbol reads light-on-dark.
fn png_transparent(colors: &[qrcode::Color], width: usize) -> Vec<u8> {
    let modules = u32::try_from(width).unwrap();
    let pad = 4 + MARGIN;
    let side = (modules + 2 * pad) * MODULE_PX;
    let mut img = image::RgbaImage::from_pixel(side, side, image::Rgba([0, 0, 0, 0]));
    for (i, color) in colors.iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let i = u32::try_from(i).unwrap();
            let (mx, my) = (pad + i % modules, pad + i / modules);
            for dy in 0..MODULE_PX {
                for dx in 0..MODULE_PX {
                    img.put_pixel(
                        mx * MODULE_PX + dx,
                        my * MODULE_PX + dy,
                        image::Rgba([120, 120, 120, 255]),
                    );
                }
            }
        }
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
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
    invert_png(&png(code, false))
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
/// [`URL`] as an ECI 26 content object.
const URL_ECI26: &str = r#"{"text":"https://qrc-ai.com/K1Ng2","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL0sxTmcy","charset":"utf8","eci":[{"designator":26,"start":0}]}"#;

/// An ECI 26 (UTF-8) symbol carrying [`URL`] in byte mode.
fn eci26_url() -> QrCode {
    v3(|b| {
        b.push_eci_designator(26)?;
        b.push_byte_data(URL.as_bytes())
    })
}

/// An ECI 4 symbol over byte A1 — 'Ą' in ISO/IEC 8859-2.
fn eci4_a1() -> QrCode {
    v3(|b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    })
}

/// [`eci4_a1`]'s content object.
const A1: &str = r#"{"text":"Ą","raw":"oQ==","charset":"eci","eci":[{"designator":4,"start":0}]}"#;

/// ISO/IEC 8859-5 bytes of "Привет".
const PRIVET_8859_5: [u8; 6] = [0xBF, 0xE0, 0xD8, 0xD2, 0xD5, 0xE2];
/// "こんにちは" in Shift JIS.
const KONNICHIWA_SJIS: [u8; 10] = [0x82, 0xB1, 0x82, 0xF1, 0x82, 0xC9, 0x82, 0xBF, 0x82, 0xCD];
/// 山田 · 日本 · テスト in Shift JIS.
const YAMADA_SJIS: [u8; 4] = [0x8E, 0x52, 0x93, 0x63];
const NIHON_SJIS: [u8; 4] = [0x93, 0xFA, 0x96, 0x7B];
const TESUTO_SJIS: [u8; 6] = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67];
/// 中文 in GB 2312.
const ZHONGWEN_GB: [u8; 4] = [0xD6, 0xD0, 0xCE, 0xC4];

/// A stream ISO 18004 does not define leaves no detection, whichever
/// stages run — no score, no hint either.
#[track_caller]
fn assert_refused(name: &str, png: &[u8]) {
    for (profile, report) in [("frame", scan_content(png)), ("walk", scan_walk(png))] {
        assert!(
            report.detections.is_empty(),
            "{name} ({profile}): {:?} (trace {:?})",
            summary(&report),
            report.trace.stages
        );
        assert!(report.score.is_none() && report.hints.is_empty(), "{name}");
    }
}

/// Byte mode "abc" after the designator, as a bit string.
const BYTE_ABC: &str = "0100 00000011 01100001 01100010 01100011 0000";

/// ECI 1000000 (the 110xxxxx form, above the register) then byte "abc":
/// rqrr decodes "abc", rxing's port signals a designator and reads "abc",
/// the scanner's parser refuses the stream.
fn eci_above_the_register_abc() -> String {
    format!("0111 11001111 01000010 01000000 {BYTE_ABC}")
}

/// FNC1 in second position (37), then in first position, then byte "abc":
/// rxing's port reads it as GS1 "37abc", the scanner refuses it.
fn fnc1_second_then_first_abc() -> String {
    format!("1001 00100101 0101 {BYTE_ABC}")
}

/// Byte mode "hello", as a bit string.
const BYTE_HELLO: &str = "0100 00000101 01101000 01100101 01101100 01101100 01101111 0000";

// ------------------------------------------------- A · declared ECI

#[test]
fn a1_eci4_byte_is_one_detection_with_the_declared_text() {
    // ECI 000004 = ISO/IEC 8859-2: byte A1 is 'Ą' (U+0104), labelled `eci`
    // (no decoder label states that reading), the designator listed.
    for stray in [false, true] {
        let report = scan_content(&png(&eci4_a1(), stray));
        let d = one(&report);
        assert_content(d, A1, &format!("stray={stray}"));
        assert_eq!(payload(d), wire(r#"{"kind":"text"}"#));
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
    // geometry and the sampled stream: structural, UEC and ISO sections all
    // present (they read the rqrr grid, not the stress matcher).
    let report = Scanner::default()
        .scan(ImageInput::encoded(&png(&eci4_a1(), false)))
        .unwrap();
    let d = one(&report);
    assert_content(d, A1, "Full");
    let score = report.score.as_ref().expect("Full scores the primary");
    assert!(score.structural.is_some(), "structural from geometry");
    assert!(score.uec.is_some(), "UEC from the sampled stream");
    assert!(score.iso15415.is_some(), "ISO card from geometry");
}

#[test]
#[ignore = "needs score matching by content identity"]
fn a2_eci4_axes_match_the_utf8_control() {
    // Same layout, same text 'Ą': ECI 4 [A1] versus ECI 26 [C4 84]. Stress
    // survival must not depend on which declaration carried the text.
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
    let (declared, control) = (scan(&eci4_a1()), scan(&eci26));
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
    assert_content(
        d,
        r#"{"text":"Привет","raw":"v+DY0tXi","charset":"eci","eci":[{"designator":7,"start":0}]}"#,
        "ECI 7",
    );
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
    assert_content(
        d,
        r#"{"text":"Ą€","raw":"oeKCrA==","charset":"eci","eci":[{"designator":4,"start":0},{"designator":26,"start":1}]}"#,
        "ECI 4 + ECI 26",
    );
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
    assert_content(
        d,
        r#"{"text":"12345Ą","raw":"MTIzNDWh","charset":"eci","eci":[{"designator":4,"start":5}]}"#,
        "numeric + ECI 4",
    );
    assert_both_engines(d);
}

#[test]
fn a6_eci3_is_strict_iso_8859_1_not_windows_1252() {
    // 0x80 under ECI 000003 is the C1 control U+0080 — Windows-1252 (the
    // `latin1` label) would read '€', so the label is `eci`.
    let code = v3(|b| {
        b.push_eci_designator(3)?;
        b.push_byte_data(&[0x35, 0x80])
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"5\u0080","raw":"NYA=","charset":"eci","eci":[{"designator":3,"start":0}]}"#,
        "ECI 3",
    );
    assert_both_engines(d);
}

#[test]
fn a7_unsupported_eci_keeps_raw_in_one_detection() {
    // No reading exists: one U+FFFD per byte — never a transcription.
    let code = v3(|b| {
        b.push_eci_designator(999)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"�","raw":"oQ==","charset":"unknown","eci":[{"designator":999,"start":0}]}"#,
        "ECI 999",
    );
    assert_eq!(payload(d), wire(r#"{"kind":"text"}"#));
}

#[test]
fn a8_undecodable_declared_span_never_publishes_empty_text() {
    // ECI 26 over a lone A1 (invalid UTF-8): rxing's own text drops the
    // span. The content keeps every byte and marks the invalid one.
    let code = v3(|b| {
        b.push_eci_designator(26)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&png(&code, false));
    assert_content(
        one(&report),
        r#"{"text":"�","raw":"oQ==","charset":"unknown","eci":[{"designator":26,"start":0}]}"#,
        "ECI 26 over A1",
    );
}

#[test]
fn a9_kanji_under_eci26_is_byte_compaction() {
    // ISO 18004 §7.4.2.1: under an ECI every mode only compacts bytes — the
    // Kanji pairs are UTF-8 bytes here: 'é' decodes, every ill-formed
    // subpart after it is marked (WHATWG), every byte kept.
    let code = v3(|b| {
        b.push_eci_designator(26)?;
        b.push_byte_data("é".as_bytes())?;
        b.push_kanji_data(&KONNICHIWA_SJIS)
    });
    let report = scan_content(&png(&code, false));
    assert_content(
        one(&report),
        r#"{"text":"é����ɂ���","raw":"w6mCsYLxgsmCv4LN","charset":"unknown","eci":[{"designator":26,"start":0}]}"#,
        "ECI 26 + é + Kanji",
    );
    // companion: a Kanji pair E3 81 then byte 82 is the UTF-8 of 'あ'
    let code = v3(|b| {
        b.push_eci_designator(26)?;
        b.push_kanji_data(&[0xE3, 0x81])?;
        b.push_byte_data(&[0x82])
    });
    let report = scan_content(&png(&code, false));
    assert_content(
        one(&report),
        r#"{"text":"あ","raw":"44GC","charset":"utf8","eci":[{"designator":26,"start":0}]}"#,
        "ECI 26 + Kanji E3 81 + byte 82",
    );
}

#[test]
fn a10_eci899_binary_keeps_exact_bytes() {
    // ECI 899 is binary: never text — one U+FFFD per byte.
    let code = v3(|b| {
        b.push_eci_designator(899)?;
        b.push_byte_data(&[0x00, 0xFF, 0x80, 0x7F])
    });
    let report = scan_content(&png(&code, false));
    assert_content(
        one(&report),
        r#"{"text":"����","raw":"AP+Afw==","charset":"unknown","eci":[{"designator":899,"start":0}]}"#,
        "ECI 899",
    );
}

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
    assert_eq!(content(d)["raw"], "YWJj");
    assert_both_engines(d);
}

#[test]
fn a11b_a_designator_above_999999_vetoes_the_port_read() {
    // The 110xxxxx form above the register: rxing's port maps the number
    // to "unknown" and still reads "abc" (designator signalled, never
    // exposed); rqrr decodes "abc" and the scanner's parser proves its
    // stream malformed. That proof refuses the symbol, upright and
    // mirrored: the port's read of the same content is withheld.
    for (name, designator) in [
        ("1000000", "11001111 01000010 01000000"),
        ("0x1FFFFF", "11011111 11111111 11111111"),
    ] {
        let bits = format!("0111 {designator} {BYTE_ABC}");
        assert_refused(name, &png_from_bit_stream(&bits));
        assert_refused(
            &format!("mirrored {name}"),
            &png_from_bit_stream_mirrored(&bits),
        );
    }
    // control: 999999, the last assignable number, reads (as unsupported)
    let report = scan_content(&png_from_bit_stream(&format!(
        "0111 11001111 01000010 00111111 {BYTE_ABC}"
    )));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"���","raw":"YWJj","charset":"unknown","eci":[{"designator":999999,"start":0}]}"#,
        "ECI 999999",
    );
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
    assert_eq!(content(d)["raw"], "YWJj");
    assert_both_engines(d);
}

#[test]
fn a11d_a_refusal_spares_a_well_formed_neighbour_with_equal_bytes() {
    // A refusal names the content rxing's port reports for the refused
    // stream (its bytes, a signalled designator, its FNC1 position) and,
    // for rqrr's own reads only, the bytes rqrr decoded — never the bytes
    // alone. A plain "abc" symbol beside a refused one is another content:
    // it keeps both engines, frame and walk alike.
    let plain = bit_stream_colors(BYTE_ABC, &[]);
    for (name, refused) in [
        ("ECI 1000000", eci_above_the_register_abc()),
        ("111xxxxx", format!("0111 11100000 {BYTE_ABC}")),
    ] {
        let png = png_row(&[bit_stream_colors(&refused, &[]), plain.clone()]);
        for (profile, report) in [("frame", scan_content(&png)), ("walk", scan_walk(&png))] {
            let d = one(&report);
            assert_content(
                d,
                r#"{"text":"abc","raw":"YWJj","charset":"utf8"}"#,
                &format!("{name} ({profile})"),
            );
            assert_both_engines(d);
        }
    }
}

#[test]
fn a_corrected_stream_above_the_register_is_refused() {
    // v1-L ECI 1000000 + "hello" with three corrupted codewords: plain RS
    // corrects them (the decoders read the symbol) where the Annex B
    // reserve allows two — rqrr's own grid still proves the stream
    // malformed, and the port's read of its content is withheld.
    let (colors, width) = stream_colors(
        Version::Normal(1),
        EcLevel::L,
        &format!("0111 11001111 01000010 01000000 {BYTE_HELLO}"),
        &[12, 14, 16],
    );
    assert_refused(
        "v1-L ECI 1000000 hello, 3 errors",
        &png_modules(&colors, width, Version::Normal(1), false),
    );
}

#[test]
fn a11e_a_numeric_group_past_999_is_refused() {
    // Numeric count 3, group value 1000: no digits ISO 18004 defines.
    // Upright and mirrored.
    let bits = "0001 0000000011 1111101000 0000";
    assert_refused("numeric 1000", &png_from_bit_stream(bits));
    assert_refused("mirrored numeric 1000", &png_from_bit_stream_mirrored(bits));
}

#[test]
fn a11f_fnc1_in_both_positions_with_designators_is_refused() {
    // The port modifiers the scanner predicts, pinned against rxing's real
    // output. FNC1 first then second (the port says ]Q5: second, the last
    // indicator) and ECI 1000000 then FNC1 first (]Q4: a designator
    // signalled, first position).
    assert_refused(
        "FNC1 first then second",
        &png_from_bit_stream(&format!("0101 1001 00100101 {BYTE_ABC}")),
    );
    assert_refused(
        "ECI 1000000 then FNC1 first",
        &png_from_bit_stream(&format!("0111 11001111 01000010 01000000 0101 {BYTE_ABC}")),
    );
}

/// A transpose-symmetric symbol (module rows; `#` dark) carrying a
/// malformed stream — a `111xxxxx` designator: both orientations
/// error-correct to the very same codewords. Constructed by a GF(2) solve
/// under the symmetry, RS and the prefix.
const SYMMETRIC_V2L: [&str; 25] = [
    "#######.#..##..#..#######",
    "#.....#..#.####...#.....#",
    "#.###.#.##..#####.#.###.#",
    "#.###.#.#..#.####.#.###.#",
    "#.###.#.######.##.#.###.#",
    "#.....#..#.###.##.#.....#",
    "#######.#.#.#.#.#.#######",
    ".........#..#...#........",
    "#.###.#.#.#.#..###..###.#",
    ".##.##.#.#.....#..#....##",
    "....#.#.#...#....#..#.##.",
    "##.###.......#..#..#..#.#",
    "###.#####.#....#..#..####",
    ".#####.....#..#..#..##..#",
    ".###..#......#..#..#.....",
    "#.####..##..#..#..#..#..#",
    "..#######..#..#.########.",
    "........#.#..#..#...#.#..",
    "#######..#..#..##.#.##.##",
    "#.....#....#..#.#...#..#.",
    "#.###.#.#.#..#..#####.#..",
    "#.###.#.#...##.##.#..#...",
    "#.###.#.#.###...##..#...#",
    "#.....#..##.#...#.##...##",
    "#######.##.###.#..#...###",
];

/// The same construction at v1-L (mask 3): `1` dark.
const SYMMETRIC_V1L: [&str; 21] = [
    "111111101100101111111",
    "100000100010001000001",
    "101110101010101011101",
    "101110101100001011101",
    "101110100101001011101",
    "100000100010101000001",
    "111111101010101111111",
    "000000000000000000000",
    "101100101000010001101",
    "100110000110100101110",
    "011001100101000001101",
    "000010000011101000010",
    "101001100101101101100",
    "000000001000000001001",
    "111111100001100010000",
    "100000100100100100100",
    "101110100000001001001",
    "101110101110110010011",
    "101110101110100100111",
    "100000100101000001110",
    "111111101010010011100",
];

/// Module rows → colors.
fn rows_colors(rows: &[&str]) -> (Vec<qrcode::Color>, usize) {
    let colors = rows
        .iter()
        .flat_map(|row| row.chars())
        .map(|c| {
            if c == '#' || c == '1' {
                qrcode::Color::Dark
            } else {
                qrcode::Color::Light
            }
        })
        .collect();
    (colors, rows.len())
}

#[test]
fn a11g_a_transpose_symmetric_malformed_grid_stays_refused() {
    // Identical re-derivations in both orientations are not ambiguous —
    // the malformed stream is refused, never published as rqrr's guess.
    for (name, rows) in [("v2-L", &SYMMETRIC_V2L[..]), ("v1-L", &SYMMETRIC_V1L[..])] {
        let (colors, width) = rows_colors(rows);
        let version = Version::Normal(i16::try_from((width - 17) / 4).unwrap());
        assert_refused(
            &format!("symmetric {name}"),
            &png_modules(&colors, width, version, false),
        );
    }
}

#[test]
fn a12_faithful_declarations_keep_their_legacy_label() {
    let cases: [(&str, QrCode, &str); 4] = [
        (
            "ECI 26 UTF-8",
            v3(|b| {
                b.push_eci_designator(26)?;
                b.push_byte_data("\u{104}".as_bytes())
            }),
            r#"{"text":"Ą","raw":"xIQ=","charset":"utf8","eci":[{"designator":26,"start":0}]}"#,
        ),
        (
            "ECI 20 Shift_JIS",
            v3(|b| {
                b.push_eci_designator(20)?;
                b.push_byte_data(&NIHON_SJIS)
            }),
            r#"{"text":"日本","raw":"k/qWew==","charset":"shift_jis","eci":[{"designator":20,"start":0}]}"#,
        ),
        (
            "ECI 3 A1",
            v3(|b| {
                b.push_eci_designator(3)?;
                b.push_byte_data(&[0xA1])
            }),
            r#"{"text":"¡","raw":"oQ==","charset":"latin1","eci":[{"designator":3,"start":0}]}"#,
        ),
        (
            "unmarked A1",
            v3(|b| b.push_byte_data(&[0xA1])),
            r#"{"text":"¡","raw":"oQ==","charset":"latin1"}"#,
        ),
    ];
    for (name, code, expected) in cases {
        let report = scan_content(&png(&code, false));
        let d = one(&report);
        assert_content(d, expected, name);
        assert_both_engines(d);
    }
}

#[test]
fn a13_a_light_on_dark_declaration_is_located_by_the_re_read() {
    // A light-on-dark symbol under the camera-frame stages: rxing's port
    // reads it (inverted) and only signals the designator; rqrr reads no
    // inverted symbol. The re-read samples the grid at rxing's own corner
    // points and locates the declaration: the declared reading, rxing's
    // read alone, no published geometry.
    let report = scan_content(&png_inverted(&eci4_a1()));
    let d = one(&report);
    assert_content(d, A1, "light-on-dark ECI 4");
    assert_eq!(d.engines, vec![EngineKind::Rxing], "{:?}", report.trace);
    assert!(d.corners.is_none(), "the re-read's geometry stays internal");
}

#[test]
fn a14_same_text_from_different_bytes_stays_two_detections() {
    // Identity is the content (symbology, raw, FNC1, compatible ECI), never
    // the text: a UTF-8 "é" symbol and a Windows-1252 "é" symbol are two
    // detections although both read "é".
    let utf8 = v3(|b| b.push_byte_data("é".as_bytes()));
    let latin1 = v3(|b| b.push_byte_data(&[0xE9]));
    let report = scan_content(&png_pair(&utf8, &latin1));
    let mut contents: Vec<String> = report
        .detections
        .iter()
        .map(|d| content(d).to_string())
        .collect();
    contents.sort();
    let mut expected = vec![
        wire(r#"{"text":"é","raw":"w6k=","charset":"utf8"}"#).to_string(),
        wire(r#"{"text":"é","raw":"6Q==","charset":"latin1"}"#).to_string(),
    ];
    expected.sort();
    assert_eq!(contents, expected, "{:?}", summary(&report));
}

#[test]
fn a15_mirrored_declarations_are_located_on_the_transposed_grid() {
    // rqrr decodes a mirrored symbol through its transposed grid; the
    // re-derivation reads that same grid, so the declaration is located and
    // the declared reading published — both engines, rqrr geometry.
    let cafe = "https://qrc-ai.com/café";
    let cases: [(&str, QrCode, &str, Option<&str>); 3] = [
        ("ECI 26 URL", eci26_url(), URL_ECI26, Some(URL)),
        ("ECI 4 A1", eci4_a1(), A1, None),
        (
            "ECI 26 café URL",
            v3(|b| {
                b.push_eci_designator(26)?;
                b.push_byte_data(cafe.as_bytes())
            }),
            r#"{"text":"https://qrc-ai.com/café","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL2NhZsOp","charset":"utf8","eci":[{"designator":26,"start":0}]}"#,
            Some(cafe),
        ),
    ];
    for (name, code, expected, url) in cases {
        let report = scan_content(&png_mirrored(&code));
        let d = one(&report);
        assert_content(d, expected, &format!("mirrored {name}"));
        if let Some(url) = url {
            assert!(
                matches!(&d.payload, Payload::Url { url: u } if u == url),
                "{name}: {:?}",
                d.payload
            );
        }
        assert_both_engines(d);
        assert!(d.corners.is_some(), "{name}");
    }
}

#[test]
fn a16_corrections_rqrr_accepted_inside_the_annex_b_reserve_are_located() {
    // v1-M ECI 4 + byte A1 with five corrupted codewords: plain RS (10
    // parity codewords) corrects them and both engines read the symbol, but
    // 2·5 + 2 exceeds the Annex B budget the scanner keeps for its own
    // decodes. rqrr already accepted these codewords, so its re-derivation
    // runs plain RS and locates the declaration.
    let (colors, width) = bit_stream_colors(
        "0111 00000100 0100 00000001 10100001 0000",
        &[6, 8, 10, 12, 14],
    );
    let report = scan_content(&png_modules(&colors, width, Version::Normal(1), false));
    let d = one(&report);
    assert_content(d, A1, "v1-M, 5 errors");
    assert_both_engines(d);
}

#[test]
fn a17_to_a20_low_contrast_and_corrected_declarations_rqrr_alone_reads() {
    // At contrast 120/150 rxing reads nothing and rqrr alone decodes —
    // mirrored or not, with codewords its plain RS corrected: its
    // re-derivation locates the declaration.
    let privet =
        r#"{"text":"Привет","raw":"v+DY0tXi","charset":"eci","eci":[{"designator":7,"start":0}]}"#;
    let eci7 =
        "0111 00000111 0100 00000110 10111111 11100000 11011000 11010010 11010101 11100010 0000";
    let eci4 = "0111 00000100 0100 00000001 10100001 0000";
    // A17: v1-L ECI 7, 3 corrupted codewords, mirrored, low contrast
    let (colors, width) = stream_colors(Version::Normal(1), EcLevel::L, eci7, &[0, 1, 2]);
    let report = scan_content(&png_contrast(&mirror(&colors, width), width, 10, 120, 150));
    let d = one(&report);
    assert_content(d, privet, "A17");
    assert_eq!(d.engines, vec![EngineKind::Rqrr], "A17");
    assert!(d.corners.is_some(), "A17");
    // A18: v1-L ECI 4, 3 corrupted codewords, upright, full contrast
    let (colors, width) = stream_colors(Version::Normal(1), EcLevel::L, eci4, &[10, 12, 14]);
    let report = scan_content(&png_modules(&colors, width, Version::Normal(1), false));
    let d = one(&report);
    assert_content(d, A1, "A18");
    assert_both_engines(d);
    // A19: mirrored ECI 4, low contrast
    let code = eci4_a1();
    let report = scan_content(&png_contrast(
        &mirror(&code.to_colors(), code.width()),
        code.width(),
        6,
        120,
        150,
    ));
    let d = one(&report);
    assert_content(d, A1, "A19");
    assert_eq!(d.engines, vec![EngineKind::Rqrr], "A19");
    // A20: v1-M ECI 4, 5 corrupted codewords, low contrast
    let (colors, width) = bit_stream_colors(eci4, &[6, 8, 10, 12, 14]);
    let report = scan_content(&png_contrast(&colors, width, 6, 120, 150));
    let d = one(&report);
    assert_content(d, A1, "A20");
    assert_eq!(d.engines, vec![EngineKind::Rqrr], "A20");
}

// ------------------------------------- J · rqrr Kanji 5949-5951 (c3)

#[test]
fn c3_kanji_9ffd_to_9fff_keep_their_original_bytes() {
    // ISO 18004 §7.4.6 (and rxing's port, and the scanner's parser) map the
    // 13-bit values 5949-5951 to Shift JIS 9F FD..9F FF; rqrr 0.10.1 writes
    // DF FD..DF FF. The RS-verified re-derivation of rqrr's own grid is the
    // truth: one detection with the original bytes, both engines, rqrr
    // geometry. Unmarked, the Kanji segment is observed and FD..FF is no
    // Shift_JIS trail: unknown, one U+FFFD for the lead and its non-ASCII
    // trail.
    for (low, raw, j2_text) in [
        (0xFD_u8, "n/0=", "\u{9f}ý"),
        (0xFE, "n/4=", "\u{9f}ţ"),
        (0xFF, "n/8=", "\u{9f}˙"),
    ] {
        let pair = [0x9F, low];
        let cases = [
            (
                "unmarked",
                v3(|b| b.push_kanji_data(&pair)),
                format!(r#"{{"text":"�","raw":"{raw}","charset":"unknown"}}"#),
            ),
            (
                "ECI 4",
                v3(|b| {
                    b.push_eci_designator(4)?;
                    b.push_kanji_data(&pair)
                }),
                serde_json::json!({
                    "text": j2_text,
                    "raw": raw,
                    "charset": "eci",
                    "eci": [{"designator": 4, "start": 0}]
                })
                .to_string(),
            ),
            (
                "ECI 26",
                v3(|b| {
                    b.push_eci_designator(26)?;
                    b.push_kanji_data(&pair)
                }),
                format!(
                    r#"{{"text":"��","raw":"{raw}","charset":"unknown","eci":[{{"designator":26,"start":0}}]}}"#
                ),
            ),
        ];
        for (name, code, expected) in cases {
            let report = scan_content(&png(&code, false));
            let d = one(&report);
            assert_content(d, &expected, &format!("{name} 9F {low:02X}"));
            assert_both_engines(d);
            assert!(d.corners.is_some(), "{name} 9F {low:02X}: rqrr geometry");
        }
    }
}

#[test]
fn j4_a_declared_byte_before_rqrrs_kanji_values() {
    for (low, expected) in [
        (
            0xFD_u8,
            r#"{"text":"Ą\u009fý","raw":"oZ/9","charset":"eci","eci":[{"designator":4,"start":0}]}"#,
        ),
        (
            0xFE,
            r#"{"text":"Ą\u009fţ","raw":"oZ/+","charset":"eci","eci":[{"designator":4,"start":0}]}"#,
        ),
        (
            0xFF,
            r#"{"text":"Ą\u009f˙","raw":"oZ//","charset":"eci","eci":[{"designator":4,"start":0}]}"#,
        ),
    ] {
        let code = v3(|b| {
            b.push_eci_designator(4)?;
            b.push_byte_data(&[0xA1])?;
            b.push_kanji_data(&[0x9F, low])
        });
        let report = scan_content(&png(&code, false));
        let d = one(&report);
        assert_content(d, expected, &format!("9F {low:02X}"));
        assert_both_engines(d);
    }
}

// --------------------------------------- B · unmarked bytes (heuristic)

#[test]
fn b1_unmarked_c1_byte_keeps_the_heuristic_reading() {
    let report = scan_content(&png(&v3(|b| b.push_byte_data(&[0x35, 0x80])), false));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"5€","raw":"NYA=","charset":"latin1"}"#,
        "[35 80]",
    );
    assert_both_engines(d);
}

#[test]
fn b2_unmarked_binary_is_one_detection_with_exact_bytes() {
    let report = scan_content(&png(
        &v3(|b| b.push_byte_data(&[0x00, 0xFF, 0x80, 0x7F])),
        false,
    ));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"\u0000ÿ€\u007f","raw":"AP+Afw==","charset":"latin1"}"#,
        "[00 FF 80 7F]",
    );
    assert_both_engines(d);
}

#[test]
fn b3_unmarked_utf8_and_windows_1252_keep_their_labels() {
    let cases: [(&[u8], &str); 3] = [
        (
            "héllo → 🦋".as_bytes(),
            r#"{"text":"héllo → 🦋","raw":"aMOpbGxvIOKGkiDwn6aL","charset":"utf8"}"#,
        ),
        (
            &[0x51, 0x75, 0xE9, 0x62, 0x65, 0x63],
            r#"{"text":"Québec","raw":"UXXpYmVj","charset":"latin1"}"#,
        ),
        (
            &KONNICHIWA_SJIS,
            r#"{"text":"こんにちは","raw":"grGC8YLJgr+CzQ==","charset":"shift_jis"}"#,
        ),
    ];
    for (bytes, expected) in cases {
        let report = scan_content(&png(&v3(|b| b.push_byte_data(bytes)), false));
        let d = one(&report);
        assert_content(d, expected, expected);
        assert_both_engines(d);
    }
}

#[test]
fn b4_short_unmarked_latin1_is_one_detection() {
    // rxing guesses a charset per unmarked segment; the scanner must not
    // let that guess split the symbol — the bytes are the identity.
    let cases: [(&[u8], &str); 3] = [
        (
            &[0xA9, 0x20, 0x32, 0x30, 0x32, 0x36],
            r#"{"text":"© 2026","raw":"qSAyMDI2","charset":"latin1"}"#,
        ),
        (
            &[0xA1, 0x48, 0x6F, 0x6C, 0x61, 0x21],
            r#"{"text":"¡Hola!","raw":"oUhvbGEh","charset":"latin1"}"#,
        ),
        (
            &[0xB1, 0xB2, 0xB3],
            r#"{"text":"ｱｲｳ","raw":"sbKz","charset":"shift_jis"}"#,
        ),
    ];
    for (bytes, expected) in cases {
        let report = scan_content(&png(&v3(|b| b.push_byte_data(bytes)), false));
        let d = one(&report);
        assert_content(d, expected, expected);
        assert_both_engines(d);
    }
}

#[test]
fn b5_a_byte_order_mark_is_bytes_not_an_encoding_switch() {
    // 0.9.0 read 'A' (shift_jis)
    let report = scan_content(&png(
        &v3(|b| b.push_byte_data(&[0xFF, 0xFE, 0x41, 0x00])),
        false,
    ));
    assert_content(
        one(&report),
        r#"{"text":"ÿþA\u0000","raw":"//5BAA==","charset":"latin1"}"#,
        "unmarked FF FE 41 00",
    );
}

#[test]
fn b6_a_byte_order_mark_under_a_declaration_is_a_character() {
    // Strict decoders: U+FEFF is text, never a byte-order switch.
    let cases: [(u32, &[u8], &str); 3] = [
        (
            25,
            &[0xFE, 0xFF, 0x00, 0x41],
            r#"{"text":"﻿A","raw":"/v8AQQ==","charset":"eci","eci":[{"designator":25,"start":0}]}"#,
        ),
        (
            26,
            &[0xEF, 0xBB, 0xBF, 0x41],
            r#"{"text":"﻿A","raw":"77u/QQ==","charset":"utf8","eci":[{"designator":26,"start":0}]}"#,
        ),
        (
            33,
            &[0xFF, 0xFE, 0x41, 0x00],
            r#"{"text":"﻿A","raw":"//5BAA==","charset":"eci","eci":[{"designator":33,"start":0}]}"#,
        ),
    ];
    for (designator, bytes, expected) in cases {
        let code = v3(|b| {
            b.push_eci_designator(designator)?;
            b.push_byte_data(bytes)
        });
        let report = scan_content(&png(&code, false));
        assert_content(one(&report), expected, &format!("ECI {designator}"));
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
        assert_content(
            d,
            r#"{"text":"12345abc","raw":"MTIzNDVhYmM=","charset":"utf8"}"#,
            &format!("stray={stray}"),
        );
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
    assert_content(
        d,
        r#"{"text":"abcこんにちは","raw":"YWJjgrGC8YLJgr+CzQ==","charset":"shift_jis"}"#,
        "byte + Kanji",
    );
    assert_both_engines(d);
}

// ------------------------------------- G · mode-aware unmarked reading

#[test]
fn kanji_segments_read_as_shift_jis() {
    // The Kanji segments the scanner observed in rqrr's verified stream
    // decide the reading (rule 1b): both engines, rqrr geometry.
    let cases: [(&str, QrCode, &str); 7] = [
        (
            "G1 山田",
            v3(|b| b.push_kanji_data(&YAMADA_SJIS)),
            r#"{"text":"山田","raw":"jlKTYw==","charset":"shift_jis"}"#,
        ),
        (
            "G2 テスト",
            v3(|b| b.push_kanji_data(&TESUTO_SJIS)),
            r#"{"text":"テスト","raw":"g2WDWINn","charset":"shift_jis"}"#,
        ),
        (
            "G3 MECARD",
            v3(|b| {
                b.push_alphanumeric_data(b"MECARD:N:")?;
                b.push_kanji_data(&YAMADA_SJIS)?;
                b.push_byte_data(b";;")
            }),
            r#"{"text":"MECARD:N:山田;;","raw":"TUVDQVJEOk46jlKTYzs7","charset":"shift_jis"}"#,
        ),
        (
            "G4 山田テスト本",
            v3(|b| {
                b.push_kanji_data(&[
                    0x8E, 0x52, 0x93, 0x63, 0x83, 0x65, 0x83, 0x58, 0x83, 0x67, 0x96, 0x7B,
                ])
            }),
            r#"{"text":"山田テスト本","raw":"jlKTY4Nlg1iDZ5Z7","charset":"shift_jis"}"#,
        ),
        (
            "G5 こんにちは",
            v3(|b| b.push_kanji_data(&KONNICHIWA_SJIS)),
            r#"{"text":"こんにちは","raw":"grGC8YLJgr+CzQ==","charset":"shift_jis"}"#,
        ),
        (
            "G5 ｱｲｳ + 日本",
            v3(|b| {
                b.push_byte_data(&[0xB1, 0xB2, 0xB3])?;
                b.push_kanji_data(&NIHON_SJIS)
            }),
            r#"{"text":"ｱｲｳ日本","raw":"sbKzk/qWew==","charset":"shift_jis"}"#,
        ),
        (
            "G5 Aｱ + 日本",
            v3(|b| {
                b.push_byte_data(&[0x41, 0xB1])?;
                b.push_kanji_data(&NIHON_SJIS)
            }),
            r#"{"text":"Aｱ日本","raw":"QbGT+pZ7","charset":"shift_jis"}"#,
        ),
    ];
    for (name, code, expected) in cases {
        let report = scan_content(&png(&code, false));
        let d = one(&report);
        assert_content(d, expected, name);
        assert_both_engines(d);
        assert!(d.corners.is_some(), "{name}");
        if name.starts_with("G3") {
            let p = payload(d);
            assert_eq!(p["kind"], "me_card", "{p}");
            assert_eq!(p["name"], "山田", "{p}");
        }
    }
}

#[test]
fn runs_beside_kanji_segments() {
    // Bytes outside the Kanji segments must read on their own under
    // Shift_JIS and must not be UTF-8 (rule 1b); otherwise rule 1d marks
    // what has no reading.
    let cases: [(&str, QrCode, &str); 4] = [
        (
            "G6 Qu E9 bec + 日本",
            v3(|b| {
                b.push_byte_data(b"Qu\xE9bec ")?;
                b.push_kanji_data(&NIHON_SJIS)
            }),
            r#"{"text":"Qu饕ec 日本","raw":"UXXpYmVjIJP6lns=","charset":"shift_jis"}"#,
        ),
        (
            "G7 UTF-8 Québec + 日本",
            v3(|b| {
                b.push_byte_data("Québec ".as_bytes())?;
                b.push_kanji_data(&NIHON_SJIS)
            }),
            r#"{"text":"Qu��bec 日本","raw":"UXXDqWJlYyCT+pZ7","charset":"unknown"}"#,
        ),
        (
            "G8 山田 + FF",
            v3(|b| {
                b.push_kanji_data(&YAMADA_SJIS)?;
                b.push_byte_data(&[0xFF])
            }),
            r#"{"text":"山田�","raw":"jlKTY/8=","charset":"unknown"}"#,
        ),
        (
            "G8 81 + 日本",
            v3(|b| {
                b.push_byte_data(&[0x81])?;
                b.push_kanji_data(&NIHON_SJIS)
            }),
            r#"{"text":"�日本","raw":"gZP6lns=","charset":"unknown"}"#,
        ),
    ];
    for (name, code, expected) in cases {
        let report = scan_content(&png(&code, false));
        let d = one(&report);
        assert_content(d, expected, name);
        assert_both_engines(d);
    }
}

#[test]
fn utf8_bytes_an_encoder_put_in_kanji_mode_stay_utf8() {
    // The `qrcode` optimizer puts some UTF-8 byte pairs in Kanji mode;
    // valid UTF-8 reads as UTF-8 (rule 1b's first step).
    for (text, raw) in [
        (
            "ありがとうございます",
            "44GC44KK44GM44Go44GG44GU44GW44GE44G+44GZ",
        ),
        ("お知らせ", "44GK55+l44KJ44Gb"),
        ("あい", "44GC44GE"),
    ] {
        let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M).unwrap();
        let report = scan_content(&png(&code, false));
        let expected = serde_json::json!({"text": text, "raw": raw, "charset": "utf8"}).to_string();
        assert_content(one(&report), &expected, text);
    }
}

#[test]
fn hanzi_segments_read_as_gb18030() {
    // rqrr cannot decode Hanzi mode; the grid it detected is re-derived by
    // the scanner (a candidate parse): its Hanzi segments decide the
    // reading (rule 1c), rxing's read is the only engine, the candidate's
    // grid the geometry.
    let cases: [(&str, Stream, &str); 7] = [
        (
            "G10 中文",
            Stream::default().hanzi(&ZHONGWEN_GB),
            r#"{"text":"中文","raw":"1tDOxA==","charset":"gb18030"}"#,
        ),
        (
            "G10 你好",
            Stream::default().hanzi(&[0xC4, 0xE3, 0xBA, 0xC3]),
            r#"{"text":"你好","raw":"xOO6ww==","charset":"gb18030"}"#,
        ),
        (
            "G10 二维码",
            Stream::default().hanzi(&[0xB6, 0xFE, 0xCE, 0xAC, 0xC2, 0xEB]),
            r#"{"text":"二维码","raw":"tv7OrMLr","charset":"gb18030"}"#,
        ),
        (
            "G11 ABC + 中文",
            Stream::default().byte(b"ABC ").hanzi(&ZHONGWEN_GB),
            r#"{"text":"ABC 中文","raw":"QUJDINbQzsQ=","charset":"gb18030"}"#,
        ),
        (
            "G11 堃 + 中文",
            Stream::default().byte(&[0x88, 0xD2]).hanzi(&ZHONGWEN_GB),
            r#"{"text":"堃中文","raw":"iNLW0M7E","charset":"gb18030"}"#,
        ),
        (
            "G12 UTF-8 é + 中文",
            Stream::default().byte(&[0xC3, 0xA9]).hanzi(&ZHONGWEN_GB),
            r#"{"text":"��中文","raw":"w6nW0M7E","charset":"unknown"}"#,
        ),
        (
            "G12 Qu E9 bec + 中文",
            Stream::default().byte(b"Qu\xE9bec ").hanzi(&ZHONGWEN_GB),
            r#"{"text":"Qu閎ec 中文","raw":"UXXpYmVjINbQzsQ=","charset":"gb18030"}"#,
        ),
    ];
    for (name, stream, expected) in cases {
        let report = scan_content(&stream_png(stream));
        let d = one(&report);
        assert_content(d, expected, name);
        assert_eq!(d.engines, vec![EngineKind::Rxing], "{name}");
        assert!(d.corners.is_some(), "{name}: the candidate's grid");
    }
    // G13: Kanji and Hanzi segments together — rule 1d reads each pair in
    // its own set and labels the whole unknown
    let report = scan_content(&stream_png(
        Stream::default().kanji(&NIHON_SJIS).hanzi(&ZHONGWEN_GB),
    ));
    assert_content(
        one(&report),
        r#"{"text":"日本中文","raw":"k/qWe9bQzsQ=","charset":"unknown"}"#,
        "G13",
    );
}

#[test]
fn twins_reconcile_their_observed_segments() {
    // A byte-mode twin observes no segment; one detection keeps the one
    // non-empty observation — and two different non-empty observations
    // conflict (rule 1d without pairs).
    let byte_only = v3(|b| b.push_byte_data(&YAMADA_SJIS));
    let report = scan_content(&png(&byte_only, false));
    assert_content(
        one(&report),
        r#"{"text":"ŽR“c","raw":"jlKTYw==","charset":"latin1"}"#,
        "G14 alone",
    );
    let kanji = v3(|b| b.push_kanji_data(&YAMADA_SJIS));
    let report = scan_content(&png_pair(&kanji, &byte_only));
    assert_content(
        one(&report),
        r#"{"text":"山田","raw":"jlKTYw==","charset":"shift_jis"}"#,
        "G14 with G1",
    );
    let report = scan_content(&png_pair(
        &v3(|b| b.push_kanji_data(&NIHON_SJIS)),
        &v3(|b| b.push_byte_data(&NIHON_SJIS)),
    ));
    assert_content(
        one(&report),
        r#"{"text":"日本","raw":"k/qWew==","charset":"shift_jis"}"#,
        "G15",
    );
    let pairs = [0xE0, 0xA1, 0xE0, 0xA1];
    let (kanji, kanji_width) = stream_colors(
        Version::Normal(2),
        EcLevel::M,
        &Stream::default().kanji(&pairs).end(),
        &[],
    );
    let (hanzi, hanzi_width) = stream_colors(
        Version::Normal(2),
        EcLevel::M,
        &Stream::default().hanzi(&pairs).end(),
        &[],
    );
    let report = scan_content(&png_row(&[(kanji, kanji_width), (hanzi, hanzi_width)]));
    assert_content(
        one(&report),
        r#"{"text":"����","raw":"4KHgoQ==","charset":"unknown"}"#,
        "G19",
    );
}

// ------------------------------------------------------------- D · FNC1

/// A GS1 payload: conformance, GTIN.
#[track_caller]
fn assert_gs1(d: &Detection, conformant: bool, gtin: Option<&str>) {
    match &d.payload {
        Payload::Gs1 {
            gtin: found,
            conformant: is,
            ..
        } => {
            assert_eq!(*is, conformant, "{:?}", d.payload);
            assert_eq!(found.as_deref(), gtin, "{:?}", d.payload);
        }
        other => panic!("not GS1: {other:?}"),
    }
}

#[test]
fn d1_fnc1_first_percent_is_the_gs_separator() {
    // rqrr cannot decode FNC1 symbols: the grid it detected is re-derived
    // (a candidate parse) — the declaration-free content gets the
    // candidate's geometry, rxing's read is the only engine.
    let code = v3(|b| {
        b.push_fnc1_first_position()?;
        b.push_alphanumeric_data(b"10ABC%2112")
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"10ABC\u001d2112","raw":"MTBBQkMdMjExMg==","charset":"utf8","fnc1":"first"}"#,
        "FNC1 first %",
    );
    assert!(matches!(d.payload, Payload::Gs1 { .. }), "{:?}", d.payload);
    assert_eq!(d.engines, vec![EngineKind::Rxing]);
    assert!(d.corners.is_some(), "the candidate's grid");
}

#[test]
fn d2_fnc1_first_double_percent_is_one_literal_percent() {
    // ISO 18004 §7.4.8.2: '%%' transmits as one '%'. Only the S5 stage
    // reads this render; the payload is exactly that — never two GS.
    let code = v3(|b| {
        b.push_fnc1_first_position()?;
        b.push_alphanumeric_data(b"10AB%%C")
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"10AB%C","raw":"MTBBQiVD","charset":"utf8","fnc1":"first"}"#,
        "FNC1 first %%",
    );
    assert_gs1(d, true, None);
    assert_eq!(d.engines, vec![EngineKind::Rescue], "{:?}", report.trace);
}

#[test]
fn d3_fnc1_second_keeps_the_application_indicator_and_is_not_gs1() {
    // ISO 18004 §7.4.8.3: the AIM application indicator is transmitted
    // first ("37"); FNC1-second data is not a GS1 element string. rqrr's
    // grid is re-derived: rxing alone, the candidate's corners.
    let code = v3(|b| {
        b.push_fnc1_second_position(37)?;
        b.push_byte_data(b"abc")
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"37abc","raw":"MzdhYmM=","charset":"utf8","fnc1":"second"}"#,
        "FNC1 second 37",
    );
    assert_eq!(payload(d), wire(r#"{"kind":"text"}"#));
    assert_eq!(d.engines, vec![EngineKind::Rxing]);
    assert!(d.corners.is_some(), "the candidate's grid");
    // a letter indicator: 'A' is encoded as 165 and transmitted as "A"
    let code = v3(|b| {
        b.push_fnc1_second_position(165)?;
        b.push_byte_data(b"abc")
    });
    let report = scan_content(&png(&code, false));
    assert_content(
        one(&report),
        r#"{"text":"Aabc","raw":"QWFiYw==","charset":"utf8","fnc1":"second"}"#,
        "FNC1 second A",
    );
    // occluded past both engines: the S5 stage reads it, same content
    let code = symbol(Version::Normal(5), EcLevel::H, |b| {
        b.push_fnc1_second_position(37)?;
        b.push_byte_data(b"abc")
    });
    let report = scan_content(&occluded_png(&code, RESCUE_ONLY_AREA_PCT));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"37abc","raw":"MzdhYmM=","charset":"utf8","fnc1":"second"}"#,
        "occluded FNC1 second",
    );
    assert_eq!(d.engines, vec![EngineKind::Rescue], "{:?}", report.trace);
}

/// The GTIN element string the GS1 rows carry.
const GS1_GTIN: &str = "0109506000134352";

/// ECI 26 + FNC1 in first position + the GTIN element string (numeric).
fn eci26_gs1() -> QrCode {
    v3(|b| {
        b.push_eci_designator(26)?;
        b.push_fnc1_first_position()?;
        b.push_numeric_data(GS1_GTIN.as_bytes())
    })
}

/// The located ECI 26 GS1 symbol: the element string, conformant, its
/// GTIN, the utf8 label (ECI 26 reproduces), rxing's read alone — the
/// candidate's re-derivation adds the declaration and the grid, not an
/// engine (`rescue` is the S5 stage) — with the candidate's geometry.
#[track_caller]
fn assert_located_gs1(d: &Detection) {
    assert_content(
        d,
        r#"{"text":"0109506000134352","raw":"MDEwOTUwNjAwMDEzNDM1Mg==","charset":"utf8","eci":[{"designator":26,"start":0}],"fnc1":"first"}"#,
        "ECI 26 GS1",
    );
    assert_gs1(d, true, Some("09506000134352"));
    assert_eq!(d.engines, vec![EngineKind::Rxing]);
    assert!(d.corners.is_some(), "the candidate's grid is the symbol");
}

#[test]
fn d4_eci26_gs1_is_located_through_rqrrs_grid() {
    // rqrr cannot decode FNC1 symbols, so its grid is a candidate, and
    // rxing's port only signals the ECI. Re-deriving that candidate locates
    // ECI 26 over the port's bytes; no `rescue` stage.
    let report = scan_content(&png(&eci26_gs1(), false));
    assert_located_gs1(one(&report));
    assert!(
        report.trace.stages.iter().all(|s| s.stage != "rescue"),
        "{:?}",
        report.trace.stages
    );
}

#[test]
fn gs1_mode_without_a_reading_is_not_parsed() {
    // FNC1 first under an unsupported designator: GS1 mode, but no verified
    // text — the element string is not parsed (spec/05).
    let code = v3(|b| {
        b.push_eci_designator(999)?;
        b.push_fnc1_first_position()?;
        b.push_numeric_data(b"0104012345678901")
    });
    let report = scan_content(&png(&code, false));
    let d = one(&report);
    let expected = serde_json::json!({
        "text": replacement(16),
        "raw": "MDEwNDAxMjM0NTY3ODkwMQ==",
        "charset": "unknown",
        "eci": [{"designator": 999, "start": 0}],
        "fnc1": "first"
    })
    .to_string();
    assert_content(d, &expected, "ECI 999 GS1");
    assert_eq!(
        payload(d),
        wire(
            r#"{"kind":"gs1","elements":[],"gtin":null,"conformant":false,"issues":["content has no verified text reading (charset unknown): element string not parsed"]}"#
        )
    );
}

#[test]
fn d5_fnc1_second_then_first_vetoes_the_port_read() {
    // FNC1 in second position, then FNC1 in first position: no symbol ISO
    // 18004 defines. rxing's port publishes it as GS1 with the AIM digits
    // leading the element string — confidently wrong; rqrr cannot decode
    // FNC1, and the scanner's parser proves its grid malformed. The read of
    // the same content is withheld: no detection.
    assert_refused(
        "FNC1 second then first",
        &png_from_bit_stream(&fnc1_second_then_first_abc()),
    );
    // control: FNC1 second alone reads, indicator first, never GS1
    let report = scan_content(&png_from_bit_stream(&format!("1001 00100101 {BYTE_ABC}")));
    let d = one(&report);
    assert_content(
        d,
        r#"{"text":"37abc","raw":"MzdhYmM=","charset":"utf8","fnc1":"second"}"#,
        "FNC1 second alone",
    );
    assert_eq!(payload(d), wire(r#"{"kind":"text"}"#));
}

#[test]
fn fnc1_first_beside_second_over_equal_bytes() {
    // Equal transmitted bytes, opposite FNC1 positions: two contents, one
    // GS1 element string, one AIM application text.
    let first = v3(|b| {
        b.push_fnc1_first_position()?;
        b.push_byte_data(b"37abc")
    });
    let second = v3(|b| {
        b.push_fnc1_second_position(37)?;
        b.push_byte_data(b"abc")
    });
    let report = scan_content(&png_pair(&first, &second));
    assert_eq!(report.detections.len(), 2, "{:?}", summary(&report));
    let by_position = |position: &str| {
        report
            .detections
            .iter()
            .find(|d| content(d)["fnc1"] == position)
            .unwrap_or_else(|| panic!("{position}: {:?}", summary(&report)))
    };
    assert!(matches!(by_position("first").payload, Payload::Gs1 { .. }));
    assert_eq!(payload(by_position("second")), wire(r#"{"kind":"text"}"#));
}

#[test]
fn a_gs1_data_matrix_reports_fnc1_first() {
    // ]d2: FNC1 in first position.
    let report = scan_content(&fixture("symbology/datamatrix-gs1.png"));
    let d = one(&report);
    assert_eq!(d.symbology, Symbology::DataMatrix);
    assert_eq!(content(d)["fnc1"], "first", "{}", content(d));
    assert!(content(d).get("eci").is_none());
    assert!(matches!(d.payload, Payload::Gs1 { .. }), "{:?}", d.payload);
}

#[test]
fn d8_a_refusal_spares_well_formed_fnc1_content_with_equal_bytes() {
    // ECI 1000000 + "abc" (refused) beside FNC1 first + "abc" (well-formed
    // GS1 data; rqrr cannot decode FNC1, so its grid is a candidate whose
    // stream parses), with and without a third symbol.
    // The port reads the GS1 symbol as another content than the refused
    // one: it is published, read by rxing, with the candidate's corners —
    // never handed to the rescue stage.
    let refused = bit_stream_colors(&eci_above_the_register_abc(), &[]);
    let gs1 = bit_stream_colors(&format!("0101 {BYTE_ABC}"), &[]);
    let hello = bit_stream_colors(BYTE_HELLO, &[]);
    for (name, symbols) in [
        (
            "refused + gs1 + hello",
            vec![refused.clone(), gs1.clone(), hello],
        ),
        ("refused + gs1", vec![refused, gs1]),
    ] {
        let png = png_row(&symbols);
        for (profile, report) in [("frame", scan_content(&png)), ("walk", scan_walk(&png))] {
            let ctx = format!("{name} ({profile}): {:?}", summary(&report));
            let abc: Vec<&Detection> = report
                .detections
                .iter()
                .filter(|d| d.content.raw == b"abc")
                .collect();
            assert_eq!(abc.len(), 1, "{ctx}");
            assert_content(
                abc[0],
                r#"{"text":"abc","raw":"YWJj","charset":"utf8","fnc1":"first"}"#,
                &ctx,
            );
            assert!(matches!(abc[0].payload, Payload::Gs1 { .. }), "{ctx}");
            assert_eq!(abc[0].engines, vec![EngineKind::Rxing], "{ctx}");
            assert!(abc[0].corners.is_some(), "{ctx}: the candidate's grid");
            if let Some(hello) = report.detections.iter().find(|d| d.content.raw == b"hello") {
                assert_content(
                    hello,
                    r#"{"text":"hello","raw":"aGVsbG8=","charset":"utf8"}"#,
                    &ctx,
                );
                assert_both_engines(hello);
            }
            assert_eq!(
                report.detections.len(),
                if name.contains("hello") { 2 } else { 1 },
                "{ctx}"
            );
        }
    }
}

#[test]
fn d9_a_mirrored_eci26_gs1_is_located_on_the_transposed_grid() {
    // rqrr reads a mirrored symbol's transpose; its RS accepts the FNC1
    // symbol there and its parser refuses FNC1. That transposed grid is the
    // candidate whose stream verifies: it locates ECI 26 over the port's
    // bytes, exactly as for the unmirrored symbol (d4).
    let report = scan_content(&png_mirrored(&eci26_gs1()));
    assert_located_gs1(one(&report));
}

#[test]
fn d10_a_mirrored_fnc1_second_then_first_is_refused() {
    // The transposed candidate proves the mirrored stream malformed.
    assert_refused(
        "mirrored FNC1 second then first",
        &png_from_bit_stream_mirrored(&fnc1_second_then_first_abc()),
    );
    // control: mirrored FNC1 second alone reads, indicator first
    let report = scan_content(&png_from_bit_stream_mirrored(&format!(
        "1001 00100101 {BYTE_ABC}"
    )));
    let d = one(&report);
    assert_eq!(content(d)["raw"], "MzdhYmM=");
    assert!(!matches!(d.payload, Payload::Gs1 { .. }), "{:?}", d.payload);
}

#[test]
fn d6_plain_and_fnc1_first_twins_stay_two_detections() {
    // Equal transmitted bytes, but one symbol declares no FNC1 and the
    // other FNC1 in first position: every QR read observes the position, so
    // they are two contents — one GS1 element string with the candidate's
    // geometry, one plain (both engines) whose GS1 syntax lacks the mode
    // header.
    let plain = v3(|b| b.push_numeric_data(GS1_GTIN.as_bytes()));
    let gs1 = v3(|b| {
        b.push_fnc1_first_position()?;
        b.push_numeric_data(GS1_GTIN.as_bytes())
    });
    let report = scan_content(&png_pair(&plain, &gs1));
    assert_eq!(report.detections.len(), 2, "{:?}", summary(&report));
    let first = report
        .detections
        .iter()
        .find(|d| content(d).get("fnc1").is_some())
        .expect("the FNC1 twin");
    assert_content(
        first,
        r#"{"text":"0109506000134352","raw":"MDEwOTUwNjAwMDEzNDM1Mg==","charset":"utf8","fnc1":"first"}"#,
        "FNC1 twin",
    );
    assert_gs1(first, true, Some("09506000134352"));
    assert_eq!(first.engines, vec![EngineKind::Rxing]);
    assert!(first.corners.is_some(), "the candidate's grid");
    let plain = report
        .detections
        .iter()
        .find(|d| content(d).get("fnc1").is_none())
        .expect("the plain twin");
    assert_content(
        plain,
        r#"{"text":"0109506000134352","raw":"MDEwOTUwNjAwMDEzNDM1Mg==","charset":"utf8"}"#,
        "plain twin",
    );
    assert_gs1(plain, false, Some("09506000134352"));
    assert_both_engines(plain);
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
    let mut contents: Vec<String> = report
        .detections
        .iter()
        .map(|d| content(d).to_string())
        .collect();
    contents.sort();
    let mut expected = vec![
        wire(r#"{"text":"37abc","raw":"MzdhYmM=","charset":"utf8"}"#).to_string(),
        wire(r#"{"text":"37abc","raw":"MzdhYmM=","charset":"utf8","fnc1":"second"}"#).to_string(),
    ];
    expected.sort();
    assert_eq!(contents, expected, "{:?}", summary(&report));
    assert!(
        report
            .detections
            .iter()
            .all(|d| !matches!(d.payload, Payload::Gs1 { .. }))
    );
}

#[test]
fn d11_a_refused_neighbour_never_hides_a_well_formed_symbol_of_equal_content() {
    // A malformed stream dark-on-light beside a well-formed symbol whose
    // port read carries the same content, light-on-dark (rqrr detects no
    // inverted grid at the camera-frame stages). The re-read locates the
    // well-formed one at its own corner points — a parsed read no refusal
    // withholds — and refuses the malformed one by its own grid.
    let well_formed = stream_colors(
        Version::Normal(1),
        EcLevel::M,
        "0101 0100 00000101 00110011 00110111 01100001 01100010 01100011 0000",
        &[],
    );
    let malformed = bit_stream_colors(&fnc1_second_then_first_abc(), &[]);
    let png = png_row_styled(&[
        (malformed.0, malformed.1, false),
        (well_formed.0, well_formed.1, true),
    ]);
    for (profile, report) in [("frame", scan_content(&png)), ("walk", scan_walk(&png))] {
        let d = one(&report);
        assert_content(
            d,
            r#"{"text":"37abc","raw":"MzdhYmM=","charset":"utf8","fnc1":"first"}"#,
            &format!("FNC1 ({profile})"),
        );
        assert!(matches!(d.payload, Payload::Gs1 { .. }), "{profile}");
        assert_eq!(d.engines, vec![EngineKind::Rxing], "{profile}");
    }
    // the same with a designator above the register beside ECI 4 + "abc"
    let well_formed = bit_stream_colors(&format!("0111 00000100 {BYTE_ABC}"), &[]);
    let malformed = bit_stream_colors(&eci_above_the_register_abc(), &[]);
    let png = png_row_styled(&[
        (malformed.0, malformed.1, false),
        (well_formed.0, well_formed.1, true),
    ]);
    for (profile, report) in [("frame", scan_content(&png)), ("walk", scan_walk(&png))] {
        let d = one(&report);
        assert_content(
            d,
            r#"{"text":"abc","raw":"YWJj","charset":"eci","eci":[{"designator":4,"start":0}]}"#,
            &format!("ECI ({profile})"),
        );
        assert_eq!(d.engines, vec![EngineKind::Rxing], "{profile}");
    }
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
    assert_eq!(d.symbology, Symbology::MicroQrCode);
    assert_content(
        d,
        r#"{"text":"é","raw":"6Q==","charset":"latin1"}"#,
        "M3-M E9",
    );
    assert_eq!(d.engines, vec![EngineKind::Rxing]);
    assert!(d.corners.is_none() && d.meta.version.is_none());
}

#[test]
fn e2_micro_qr_kanji_reads_through_the_engines_mode_aware_reading() {
    // The scanner never parses a Micro QR stream; the engine read it mode
    // by mode and its reading equals the Shift_JIS decoding of every byte,
    // with pairs (rule 1e).
    for (fixture_name, expected) in [
        (
            "microqr-kanji-yamada.png",
            r#"{"text":"山田","raw":"jlKTYw==","charset":"shift_jis"}"#,
        ),
        (
            "microqr-kanji-yamamoto.png",
            r#"{"text":"山本","raw":"jlKWew==","charset":"shift_jis"}"#,
        ),
        (
            "microqr-kanji-nihon.png",
            r#"{"text":"日本","raw":"k/qWew==","charset":"shift_jis"}"#,
        ),
        (
            "microqr-kanji-tesuto.png",
            r#"{"text":"テスト","raw":"g2WDWINn","charset":"shift_jis"}"#,
        ),
        (
            "microqr-kanji-nihongo.png",
            r#"{"text":"日本語","raw":"k/qWe4zq","charset":"shift_jis"}"#,
        ),
    ] {
        let report = scan_content(&fixture(&format!("symbology/{fixture_name}")));
        let d = one(&report);
        assert_eq!(d.symbology, Symbology::MicroQrCode, "{fixture_name}");
        assert_content(d, expected, fixture_name);
        assert_eq!(d.engines, vec![EngineKind::Rxing], "{fixture_name}");
    }
}

#[test]
fn e3_micro_qr_bytes_without_pairs_keep_the_legacy_reading() {
    // No pair in the engine's reading: rule 1a.
    let code = symbol(Version::Micro(3), EcLevel::M, |b| {
        b.push_byte_data(&[0x35, 0x80])
    });
    let report = scan_content(&png(&code, false));
    assert_content(
        one(&report),
        r#"{"text":"5€","raw":"NYA=","charset":"latin1"}"#,
        "Micro QR [35 80]",
    );
}

#[test]
fn an_rmqr_url_with_kanji() {
    // rMQR via rxing only, no designator: rule 1e, payload url.
    let report = scan_content(&fixture("symbology/rmqr-url-kanji.png"));
    let d = one(&report);
    assert_eq!(d.symbology, Symbology::RectangularMicroQrCode);
    assert_content(
        d,
        r#"{"text":"https://qrc-ai.com/日本","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL5P6lns=","charset":"shift_jis"}"#,
        "rMQR",
    );
    assert!(matches!(d.payload, Payload::Url { .. }), "{:?}", d.payload);
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
    encode_png(img)
}

/// Occluded area (percent of the symbol) where both engines give up on the
/// v5-H ECI fixture and the erasure rescue still recovers it.
const RESCUE_ONLY_AREA_PCT: f32 = 24.0;

#[test]
fn e4_rescue_only_eci4_reads_the_declaration() {
    // The S5 stage locates the declaration in the stream it recovers.
    let code = symbol(Version::Normal(5), EcLevel::H, |b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    let report = scan_content(&occluded_png(&code, RESCUE_ONLY_AREA_PCT));
    let d = one(&report);
    assert_eq!(d.engines, vec![EngineKind::Rescue], "{:?}", report.trace);
    assert_content(d, A1, "rescued ECI 4");
}

#[test]
fn e5_artistic_eci26_urls_are_located_by_the_re_read() {
    // At the direct stage rxing alone reads these renders and only signals
    // the ECI; the re-read at rxing's corner points locates it, so the
    // camera-frame stages already publish the URL — rxing's read alone, no
    // published geometry — and the walk settles there too.
    for (name, png) in [
        ("dots", png_dots(&eci26_url())),
        ("blur 3.5", png_blurred(&eci26_url(), 3.5)),
    ] {
        for (profile, report) in [("frame", scan_content(&png)), ("walk", scan_walk(&png))] {
            let d = one(&report);
            assert_content(d, URL_ECI26, &format!("{name} ({profile})"));
            assert!(
                matches!(&d.payload, Payload::Url { url } if url == URL),
                "{name}: {:?}",
                d.payload
            );
            assert_eq!(d.engines, vec![EngineKind::Rxing], "{name} ({profile})");
            assert_eq!(
                report.trace.stages.last().map(|s| s.stage.as_str()),
                Some("direct"),
                "{name} ({profile}): {:?}",
                report.trace.stages
            );
        }
    }
    // control: a lighter blur both engines already read at the direct stage
    let report = scan_content(&png_blurred(&eci26_url(), 2.5));
    let d = one(&report);
    assert_content(d, URL_ECI26, "blur 2.5");
    assert_both_engines(d);
}

// ------------------------------------------------------------- N · non-QR

#[test]
fn n1_to_n4_non_qr_raw_is_the_engines_reading() {
    // Data Matrix Base256 and PDF417 byte compaction read as ISO/IEC 8859-1
    // characters: their encoding is the symbol's bytes — the whole payload,
    // never the byte segments alone.
    for (fixture_name, symbology, expected, url) in [
        (
            "datamatrix-utf8-url-japanese.png",
            Symbology::DataMatrix,
            r#"{"text":"https://qrc-ai.com/日本語テスト","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL+aXpeacrOiqnuODhuOCueODiA==","charset":"utf8"}"#,
            true,
        ),
        (
            "datamatrix-utf8-cafe.png",
            Symbology::DataMatrix,
            r#"{"text":"Café crème brûlée","raw":"Q2Fmw6kgY3LDqG1lIGJyw7tsw6ll","charset":"utf8"}"#,
            false,
        ),
        (
            "pdf417-utf8-cafe.png",
            Symbology::Pdf417,
            r#"{"text":"Café crème brûlée","raw":"Q2Fmw6kgY3LDqG1lIGJyw7tsw6ll","charset":"utf8"}"#,
            false,
        ),
        (
            "datamatrix-utf8-url-cafe.png",
            Symbology::DataMatrix,
            r#"{"text":"https://qrc-ai.com/café","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL2NhZsOp","charset":"utf8"}"#,
            true,
        ),
    ] {
        let report = scan_content(&fixture(&format!("symbology/{fixture_name}")));
        let d = one(&report);
        assert_eq!(d.symbology, symbology, "{fixture_name}");
        assert_content(d, expected, fixture_name);
        assert_eq!(d.engines, vec![EngineKind::Rxing], "{fixture_name}");
        assert_eq!(
            matches!(d.payload, Payload::Url { .. }),
            url,
            "{fixture_name}: {:?}",
            d.payload
        );
    }
}

#[test]
fn n5_two_data_matrix_symbols_sharing_their_non_ascii_run() {
    // Two different symbols whose byte segments are equal stay two
    // detections once raw is the whole reading.
    let report = scan_content(&fixture("symbology/datamatrix-pair-japanese.png"));
    let mut raws: Vec<String> = report
        .detections
        .iter()
        .map(|d| content(d)["raw"].as_str().unwrap().to_owned())
        .collect();
    raws.sort();
    assert_eq!(
        raws,
        vec![
            "aHR0cHM6Ly9hLmV4YW1wbGUv5pel5pys6Kqe44OG44K544OI".to_owned(),
            "aHR0cHM6Ly9iLmV4YW1wbGUv5pel5pys6Kqe44OG44K544OI".to_owned(),
        ],
        "{:?}",
        summary(&report)
    );
}

#[test]
fn n8_a_databar_gtin_has_no_fnc1_key() {
    // ]e0: the position is not reported (DataBar's text is the AI-less
    // GTIN).
    let report = scan_content(&fixture("symbology/databar-gtin.png"));
    let d = one(&report);
    assert_eq!(d.symbology, Symbology::DataBar);
    assert_content(
        d,
        r#"{"text":"09506000134352","raw":"MDk1MDYwMDAxMzQzNTI=","charset":"utf8"}"#,
        "DataBar",
    );
}

// ------------------------------------------------------------ R · re-read

/// The rxing-only artistic ECI renders of the re-read rows (R1–R6):
/// dot-style modules rqrr does not read at the camera-frame stages.
fn artistic_eci_rows() -> Vec<(&'static str, QrCode, String)> {
    vec![
        (
            "R1 ECI 4",
            v3(|b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[
                    0xA1, 0x5A, 0x61, 0xBF, 0xF3, 0xB3, 0xE6, 0x20, 0x67, 0xEA, 0xB6, 0x6C, 0xB1,
                ])
            }),
            r#"{"text":"ĄZażółć gęślą","raw":"oVphv/Oz5iBn6rZssQ==","charset":"eci","eci":[{"designator":4,"start":0}]}"#.to_owned(),
        ),
        (
            "R2 ECI 20",
            v3(|b| {
                b.push_eci_designator(20)?;
                b.push_byte_data(&[
                    0x93, 0xFA, 0x96, 0x7B, 0x82, 0xCC, 0x95, 0xB6, 0x8E, 0x9A, 0x83, 0x52, 0x81,
                    0x5B, 0x83, 0x68,
                ])
            }),
            r#"{"text":"日本の文字コード","raw":"k/qWe4LMlbaOmoNSgVuDaA==","charset":"shift_jis","eci":[{"designator":20,"start":0}]}"#.to_owned(),
        ),
        (
            "R3 ECI 26",
            v3(|b| {
                b.push_eci_designator(26)?;
                b.push_byte_data("café ☕ crème".as_bytes())
            }),
            r#"{"text":"café ☕ crème","raw":"Y2Fmw6kg4piVIGNyw6htZQ==","charset":"utf8","eci":[{"designator":26,"start":0}]}"#.to_owned(),
        ),
        (
            "R4 ECI 999",
            v3(|b| {
                b.push_eci_designator(999)?;
                b.push_byte_data(b"AB unsupported")
            }),
            serde_json::json!({
                "text": replacement(14),
                "raw": "QUIgdW5zdXBwb3J0ZWQ=",
                "charset": "unknown",
                "eci": [{"designator": 999, "start": 0}]
            })
            .to_string(),
        ),
        (
            "R5 numeric + ECI 7",
            v3(|b| {
                b.push_numeric_data(b"12345")?;
                b.push_eci_designator(7)?;
                b.push_byte_data(&[0xBF, 0xE0, 0xD8, 0xD2, 0xD5, 0xE2, 0x20, 0xDC, 0xD8, 0xE0])
            }),
            r#"{"text":"12345Привет мир","raw":"MTIzNDW/4NjS1eIg3Njg","charset":"eci","eci":[{"designator":7,"start":5}]}"#.to_owned(),
        ),
        (
            "R6 ECI 4 + ECI 26",
            v3(|b| {
                b.push_eci_designator(4)?;
                b.push_byte_data(&[0xA1, 0xB1])?;
                b.push_eci_designator(26)?;
                b.push_byte_data("€ ok".as_bytes())
            }),
            r#"{"text":"Ąą€ ok","raw":"obHigqwgb2s=","charset":"eci","eci":[{"designator":4,"start":0},{"designator":26,"start":2}]}"#.to_owned(),
        ),
    ]
}

#[test]
fn r1_to_r6_rxing_only_artistic_declarations_are_located() {
    // rxing alone reads these renders; the re-read at its result points
    // locates every declaration (never `eci: []`): one detection, rxing's
    // read alone, no geometry published, no engine panic — light-on-dark
    // alike, and identical across repeated scans and a generous budget.
    for (name, code, expected) in artistic_eci_rows() {
        let dots = png_dots(&code);
        for (style, png) in [
            ("dark-on-light", dots.clone()),
            ("light-on-dark", invert_png(&dots)),
        ] {
            let report = scan_content(&png);
            let ctx = format!("{name} ({style})");
            let d = one(&report);
            assert_content(d, &expected, &ctx);
            assert_eq!(d.engines, vec![EngineKind::Rxing], "{ctx}");
            assert!(d.corners.is_none() && d.meta.version.is_none(), "{ctx}");
            assert_eq!(report.trace.engine_panics, 0, "{ctx}");
            let again = scan_content(&png);
            assert_eq!(again.detections, report.detections, "{ctx}: deterministic");
            let budgeted = scan_with(&png, ScanProfile::Frame, Some(10_000));
            assert_eq!(budgeted.detections, report.detections, "{ctx}: 10 s budget");
        }
    }
}

// ---------------------------------------------------- F · invariants

/// Every single-symbol fixture of this file, by name.
fn matrix() -> Vec<(&'static str, QrCode)> {
    vec![
        ("eci4", eci4_a1()),
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
        ("kanji", v3(|b| b.push_kanji_data(&YAMADA_SJIS))),
        ("kanji-9ffd", v3(|b| b.push_kanji_data(&[0x9F, 0xFD]))),
        ("eci26-gs1", eci26_gs1()),
    ]
}

/// What a decoder label promises (spec/01: that WHATWG decoder over all of
/// `raw`); `None` for `eci` / `unknown` or a decoder error.
fn decode_with_label(raw: &[u8], charset: &str) -> Option<String> {
    let strict = |encoding: &'static encoding_rs::Encoding| {
        encoding
            .decode_without_bom_handling_and_without_replacement(raw)
            .map(std::borrow::Cow::into_owned)
    };
    match charset {
        "utf8" => String::from_utf8(raw.to_vec()).ok(),
        "shift_jis" => strict(encoding_rs::SHIFT_JIS),
        "gb18030" => strict(encoding_rs::GB18030),
        "latin1" => Some(
            encoding_rs::WINDOWS_1252
                .decode_without_bom_handling(raw)
                .0
                .into_owned(),
        ),
        _ => None,
    }
}

/// The published-content invariants of spec/01: I1-I6 and I8, numbered in
/// the order its § Invariants lists them.
#[track_caller]
fn assert_invariants(report: &ScanReport, ctx: &str) {
    assert!(report.detections.len() <= 16, "{ctx}");
    for (i, a) in report.detections.iter().enumerate() {
        let c = content(a);
        let charset = c["charset"].as_str().unwrap();
        let text = a.content.text.as_str();
        let raw = &a.content.raw;
        // I3: text empty iff raw empty, then utf8
        assert_eq!(text.is_empty(), raw.is_empty(), "{ctx}: {c}");
        if raw.is_empty() {
            assert_eq!(charset, "utf8", "{ctx}: {c}");
        }
        // I1: eci ⇒ at least one entry; I2: [] ⇒ unknown, one U+FFFD per byte
        match c.get("eci").and_then(Value::as_array) {
            Some(list) if list.is_empty() => {
                assert_eq!(charset, "unknown", "{ctx}: {c}");
                assert_eq!(text, replacement(raw.len()), "{ctx}: {c}");
            }
            None => assert_ne!(charset, "eci", "{ctx}: {c}"),
            Some(_) => {}
        }
        // I4: a decoder label reproduces the text from all of raw
        if ["utf8", "shift_jis", "latin1", "gb18030"].contains(&charset) {
            assert_eq!(
                decode_with_label(raw, charset).as_deref(),
                Some(text),
                "{ctx}: {c}"
            );
        }
        // I5 (fixtures encode no U+FFFD): `eci` inserted no replacement;
        // I6: `unknown` inserted at most one per byte
        let marks = text.chars().filter(|&ch| ch == '\u{FFFD}').count();
        if charset == "eci" {
            assert_eq!(marks, 0, "{ctx}: {c}");
        }
        if charset == "unknown" {
            assert!((1..=raw.len()).contains(&marks), "{ctx}: {c}");
        }
        // engines always; geometry only for the QR family
        assert!(
            !a.engines.is_empty(),
            "{ctx}: every detection has an engine"
        );
        // I8: no two detections share an identity
        for b in &report.detections[i + 1..] {
            let d = content(b);
            let same = a.symbology == b.symbology
                && c["raw"] == d["raw"]
                && c.get("fnc1") == d.get("fnc1")
                && match (c.get("eci"), d.get("eci")) {
                    (None, None) => true,
                    (Some(x), Some(y)) => {
                        x == y
                            || x.as_array().is_some_and(Vec::is_empty)
                            || y.as_array().is_some_and(Vec::is_empty)
                    }
                    _ => false,
                };
            assert!(!same, "{ctx}: two detections, one identity: {c} / {d}");
        }
    }
}

#[test]
fn f1_global_invariants_over_the_matrix() {
    // Every published content of the single-symbol matrix, on pure and
    // non-pure renders, frame and walk: the content invariants, one
    // detection, and determinism.
    for (name, code) in matrix() {
        for stray in [false, true] {
            let bytes = png(&code, stray);
            for (profile, report) in [("frame", scan_content(&bytes)), ("walk", scan_walk(&bytes))]
            {
                let ctx = format!("{name} (stray={stray}, {profile})");
                assert_invariants(&report, &ctx);
                assert_eq!(report.detections.len(), 1, "{ctx}: {:?}", summary(&report));
            }
            let (first, again) = (scan_content(&bytes), scan_content(&bytes));
            assert_eq!(again.detections, first.detections, "{name}: deterministic");
        }
    }
}

/// Every stage of the trace as `(stage, detections_found)`.
fn found_per_stage(report: &ScanReport) -> Vec<(&str, u32)> {
    report
        .trace
        .stages
        .iter()
        .map(|s| (s.stage.as_str(), s.detections_found))
        .collect()
}

#[test]
fn f2_detections_found_counts_raw_engine_hits_before_any_veto() {
    // spec/01: `detections_found` is RAW engine hits, pre-merge. rxing's
    // port read of a refused symbol is such a hit — the scanner then
    // refuses it; rqrr's refused decode never becomes a hit.
    let report = scan_content(&png_from_bit_stream(&eci_above_the_register_abc()));
    assert!(report.detections.is_empty(), "{:?}", summary(&report));
    assert_eq!(found_per_stage(&report), vec![("direct", 1)]);
}

#[test]
fn f3_a_refusal_survives_the_alpha_fallback_walk() {
    // A transparent design: the auto flatten (over white) shows the symbol
    // dark-on-light, the scanner proves its stream malformed and the first
    // walk ends empty — so the fallback walk flattens over black, where the
    // symbol reads light-on-dark. The fallback walk starts with the first
    // walk's refusals: no detection, nothing rescued — and a withheld read
    // never settles it (its later stages still run).
    for (name, bits) in [
        ("ECI 1000000", eci_above_the_register_abc()),
        ("FNC1 second then first", fnc1_second_then_first_abc()),
    ] {
        let (colors, width) = bit_stream_colors(&bits, &[]);
        let png = png_transparent(&colors, width);
        for (profile, report) in [("frame", scan_content(&png)), ("walk", scan_walk(&png))] {
            let stages = found_per_stage(&report);
            let directs: Vec<usize> = stages
                .iter()
                .enumerate()
                .filter(|(_, (stage, _))| *stage == "direct")
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                directs.len(),
                2,
                "{name} ({profile}): both walks ran: {stages:?}"
            );
            assert!(
                stages[directs[1]].1 >= 1,
                "{name} ({profile}): the fallback walk reads the light-on-dark symbol: {stages:?}"
            );
            assert!(
                report.detections.is_empty(),
                "{name} ({profile}): {:?}",
                summary(&report)
            );
            let alpha = report.alpha.as_ref().expect("a transparent input");
            assert!(!alpha.fallback_used, "{name} ({profile})");
            assert_eq!(alpha.background, "white", "{name} ({profile})");
            if profile == "walk" {
                assert!(
                    stages[directs[1]..]
                        .iter()
                        .any(|(stage, _)| *stage == "enhance"),
                    "{name}: the fallback walk goes on past its withheld read: {stages:?}"
                );
            }
        }
    }
}

// ------------------------------------------- G · formerly known residuals

#[test]
fn g1_light_on_dark_malformed_streams_are_refused_by_the_re_read() {
    // rqrr detects no light-on-dark grid at the pyramid and direct stages;
    // the re-read samples rxing's own result points, error-corrects the
    // grid and proves it malformed (and the port's tolerant parse
    // reproduces the payload): the read is refused, frame and walk alike.
    assert_refused(
        "light-on-dark FNC1 second then first",
        &png_from_bit_stream_inverted(&fnc1_second_then_first_abc()),
    );
    assert_refused(
        "light-on-dark ECI 1000000",
        &png_from_bit_stream_inverted(&eci_above_the_register_abc()),
    );
}

/// A flyer: the dot-style ECI 26 URL render (e5) above the EAN-13 fixture.
fn png_flyer() -> Vec<u8> {
    let qr = image::load_from_memory(&png_dots(&eci26_url()))
        .unwrap()
        .into_luma8();
    let ean = image::load_from_memory(&fixture("symbology/ean13.png"))
        .unwrap()
        .into_luma8();
    let gap = 24;
    let mut flyer = image::GrayImage::from_pixel(
        qr.width().max(ean.width()),
        qr.height() + gap + ean.height(),
        image::Luma([255]),
    );
    image::imageops::overlay(&mut flyer, &qr, 0, 0);
    image::imageops::overlay(&mut flyer, &ean, 0, i64::from(qr.height() + gap));
    encode_png(flyer)
}

#[test]
fn g2_a_qr_beside_a_retail_barcode_is_located() {
    // The re-read locates the dot-style ECI 26 URL beside an EAN-13 at the
    // direct stage: two detections, the QR with its declared reading and
    // URL payload, the EAN-13 as a GTIN.
    let report = scan_walk(&png_flyer());
    assert_eq!(report.detections.len(), 2, "{:?}", summary(&report));
    let qr = report
        .detections
        .iter()
        .find(|d| d.symbology == Symbology::QrCode)
        .expect("the QR");
    assert_content(qr, URL_ECI26, "flyer QR");
    assert!(matches!(&qr.payload, Payload::Url { url } if url == URL));
    let ean = report
        .detections
        .iter()
        .find(|d| d.symbology == Symbology::Ean13)
        .expect("the EAN-13");
    assert_eq!(ean.content.text, "9506000134352");
    assert!(
        matches!(ean.payload, Payload::Gs1 { .. }),
        "{:?}",
        ean.payload
    );
}
