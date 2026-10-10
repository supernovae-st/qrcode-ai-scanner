//! Acceptance rows (07-pipeline § Acceptance), public API only: what the
//! scanner publishes on inputs whose reads the acceptance rules weigh. The
//! retail barcodes keep settling on their first read; a symbology the
//! readers can invent from noise is published only once a read at an
//! independent scale confirms it. Every input is built here, from committed
//! fixtures or in-test renders with a fixed noise generator.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{ImageInput, ScanConfig, ScanProfile, ScanReport, Scanner, Symbology};

/// The GTIN of the low-contrast EAN-13 render (and of `ean13.png`).
const EAN13: &str = "9506000134352";

/// The noise of the low-contrast EAN-13 render: the generator's seed and
/// the standard deviation, in luma steps.
const NOISE_SEED: u64 = 25;
const NOISE_SIGMA: f64 = 9.0;

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

fn png(img: &image::GrayImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .unwrap();
    bytes
}

/// A scan under `config` with no wall clock and no scoring: the walk is a
/// pure function of the pixels.
fn scan(bytes: &[u8], mut config: ScanConfig) -> ScanReport {
    config.budget_ms = None;
    config.score_depth = qrcode_ai_scanner::ScoreDepth::Off;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
        .scan(ImageInput::encoded(bytes))
        .unwrap()
}

/// Every detection as `(symbology, text)`.
fn published(report: &ScanReport) -> Vec<(Symbology, String)> {
    report
        .detections
        .iter()
        .map(|d| (d.symbology, d.content.text.clone()))
        .collect()
}

fn stages(report: &ScanReport) -> Vec<&str> {
    report
        .trace
        .stages
        .iter()
        .map(|s| s.stage.as_str())
        .collect()
}

/// The 95 modules of an EAN-13 symbol (`true` = bar), its check digit
/// computed.
fn ean13_modules(twelve: &str) -> (String, Vec<bool>) {
    const L: [&str; 10] = [
        "0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011",
        "0110111", "0001011",
    ];
    const PARITY: [&str; 10] = [
        "LLLLLL", "LLGLGG", "LLGGLG", "LLGGGL", "LGLLGG", "LGGLLG", "LGGGLL", "LGLGLG", "LGLGGL",
        "LGGLGL",
    ];
    let digits: Vec<usize> = twelve.bytes().map(|b| usize::from(b - b'0')).collect();
    let sum: usize = digits
        .iter()
        .enumerate()
        .map(|(i, d)| if i % 2 == 1 { 3 * d } else { *d })
        .sum();
    let check = (10 - sum % 10) % 10;
    let all: Vec<usize> = digits.iter().copied().chain([check]).collect();
    let right = |d: usize| -> String {
        L[d].chars()
            .map(|c| if c == '0' { '1' } else { '0' })
            .collect()
    };
    let mut bits = String::from("101");
    for (parity, &d) in PARITY[all[0]].chars().zip(&all[1..7]) {
        if parity == 'L' {
            bits.push_str(L[d]);
        } else {
            bits.extend(right(d).chars().rev());
        }
    }
    bits.push_str("01010");
    for &d in &all[7..] {
        bits.push_str(&right(d));
    }
    bits.push_str("101");
    let text = all.iter().map(ToString::to_string).collect();
    (text, bits.chars().map(|c| c == '1').collect())
}

/// The 51 modules of a UPC-E symbol of number system 0 (`true` = bar), its
/// check digit computed over the UPC-A it expands to; and its 8-digit text.
fn upce_modules(six: &str) -> (String, Vec<bool>) {
    const L: [&str; 10] = [
        "0001101", "0011001", "0010011", "0111101", "0100011", "0110001", "0101111", "0111011",
        "0110111", "0001011",
    ];
    // number system 0: the parity of each digit, by check digit (E = even)
    const PARITY: [&str; 10] = [
        "EEEOOO", "EEOEOO", "EEOOEO", "EEOOOE", "EOEEOO", "EOOEEO", "EOOOEE", "EOEOEO", "EOEOOE",
        "EOOEOE",
    ];
    let d: Vec<usize> = six.bytes().map(|b| usize::from(b - b'0')).collect();
    let upca: Vec<usize> = match d[5] {
        0..=2 => vec![0, d[0], d[1], d[5], 0, 0, 0, 0, d[2], d[3], d[4]],
        3 => vec![0, d[0], d[1], d[2], 0, 0, 0, 0, 0, d[3], d[4]],
        4 => vec![0, d[0], d[1], d[2], d[3], 0, 0, 0, 0, 0, d[4]],
        _ => vec![0, d[0], d[1], d[2], d[3], d[4], 0, 0, 0, 0, d[5]],
    };
    let sum: usize = upca
        .iter()
        .enumerate()
        .map(|(i, v)| if i % 2 == 0 { 3 * v } else { *v })
        .sum();
    let check = (10 - sum % 10) % 10;
    let mut bits = String::from("101");
    for (parity, &digit) in PARITY[check].chars().zip(&d) {
        if parity == 'O' {
            bits.push_str(L[digit]);
        } else {
            // even parity: the reversed complement of the odd code
            bits.extend(
                L[digit]
                    .chars()
                    .rev()
                    .map(|c| if c == '0' { '1' } else { '0' }),
            );
        }
    }
    bits.push_str("010101");
    let text = format!("0{six}{check}");
    (text, bits.chars().map(|c| c == '1').collect())
}

/// A clean render of a 1D symbol: `module` px per module, 10 quiet modules
/// each side, `height` px tall bars on white, in a canvas of `height + 40` px.
fn render_1d(modules: &[bool], module: u32, height: u32) -> image::GrayImage {
    let quiet = 10 * module;
    let width = u32::try_from(modules.len()).unwrap() * module + 2 * quiet;
    image::GrayImage::from_fn(width, height + 40, |x, y| {
        let bar = (20..20 + height).contains(&y)
            && x >= quiet
            && modules
                .get(((x - quiet) / module) as usize)
                .copied()
                .unwrap_or(false);
        image::Luma([if bar { 0 } else { 255 }])
    })
}

/// One draw of the fixed generator: a 64-bit linear congruential sequence;
/// the sum of twelve of its 16-bit uniforms, centred, is about N(0, 1).
struct Noise(u64);

impl Noise {
    fn uniform(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        f64::from(u16::try_from(self.0 >> 48).unwrap()) / 65_536.0
    }

    fn normal(&mut self) -> f64 {
        (0..12).map(|_| self.uniform()).sum::<f64>() - 6.0
    }
}

/// A separable 7-tap binomial blur (1 6 15 20 15 6 1, sigma about 1.22),
/// edges clamped, in integers.
fn blur(img: &image::GrayImage) -> image::GrayImage {
    const TAPS: [u32; 7] = [1, 6, 15, 20, 15, 6, 1];
    let (w, h) = (img.width(), img.height());
    let at = |v: u32, offset: usize, max: u32| -> u32 {
        (i64::from(v) + i64::try_from(offset).unwrap() - 3)
            .clamp(0, i64::from(max) - 1)
            .try_into()
            .unwrap()
    };
    let mut rows = vec![0u32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            rows[(y * w + x) as usize] = TAPS
                .iter()
                .enumerate()
                .map(|(i, k)| k * u32::from(img.get_pixel(at(x, i, w), y).0[0]))
                .sum();
        }
    }
    image::GrayImage::from_fn(w, h, |x, y| {
        let sum: u32 = TAPS
            .iter()
            .enumerate()
            .map(|(i, k)| k * rows[(at(y, i, h) * w + x) as usize])
            .sum();
        image::Luma([u8::try_from((sum + 2048) / 4096).unwrap()])
    })
}

/// A low-contrast EAN-13 capture: 8 px modules, bars at luma 100 and spaces
/// at 150 on a 140 paper, 1800 x 1000, blurred, with the fixed noise.
fn noisy_ean13(seed: u64, sigma: f64) -> image::GrayImage {
    let (_, modules) = ean13_modules(&EAN13[..12]);
    let (module, bar_h, quiet) = (8u32, 520u32, 11 * 8u32);
    let (w, h) = (1800u32, 1000u32);
    let symbol_w = 95 * module + 2 * quiet;
    let (x0, y0) = ((w - symbol_w) / 2, (h - bar_h) / 2);
    let mut img = image::GrayImage::from_pixel(w, h, image::Luma([140]));
    for y in y0 - 4 * module..y0 + bar_h + 4 * module {
        for x in x0..x0 + symbol_w {
            img.put_pixel(x, y, image::Luma([150]));
        }
    }
    for (i, &bar) in modules.iter().enumerate() {
        if bar {
            let left = x0 + quiet + u32::try_from(i).unwrap() * module;
            for y in y0..y0 + bar_h {
                for x in left..left + module {
                    img.put_pixel(x, y, image::Luma([100]));
                }
            }
        }
    }
    let mut noise = Noise(seed);
    let mut img = blur(&img);
    for pixel in img.pixels_mut() {
        let value = (f64::from(pixel.0[0]) + sigma * noise.normal()).round();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rounded and clamped to the luma range"
        )]
        let luma = value.clamp(0.0, 255.0) as u8;
        pixel.0[0] = luma;
    }
    img
}

/// A committed fixture at `factor` times its size (nearest neighbour).
fn upscaled_fixture(rel: &str, factor: u32) -> image::GrayImage {
    let img = image::load_from_memory(&fixture(rel)).unwrap().to_luma8();
    image::imageops::resize(
        &img,
        img.width() * factor,
        img.height() * factor,
        image::imageops::FilterType::Nearest,
    )
}

#[test]
fn the_encoders_write_the_symbols_they_name() {
    let (text, modules) = ean13_modules(&EAN13[..12]);
    assert_eq!(text, EAN13);
    assert_eq!(modules.len(), 95);
    let (text, modules) = upce_modules("123456");
    assert_eq!(text, "01234565");
    assert_eq!(modules.len(), 51);
}

#[test]
fn retail_barcodes_still_settle_on_their_first_read() {
    // EAN-13 and Code 128 are no symbology the readers invent: at twice their
    // size the 512 px pyramid reads them, and the walk ends there
    for (rel, symbology, truth) in [
        ("symbology/ean13.png", Symbology::Ean13, EAN13),
        ("symbology/code128.png", Symbology::Code128, "QRCAI-0042"),
    ] {
        let report = scan(&png(&upscaled_fixture(rel, 2)), ScanProfile::Full.config());
        assert_eq!(published(&report), [(symbology, truth.to_owned())], "{rel}");
        assert_eq!(stages(&report), ["pyramid"], "{rel}");
    }
}

#[test]
fn a_qr_beside_an_ean13_publishes_both() {
    let url = "https://qrc-ai.com/beside";
    let qr = qrcode::QrCode::new(url.as_bytes())
        .unwrap()
        .render::<image::Luma<u8>>()
        .module_dimensions(8, 8)
        .build();
    let ean = image::load_from_memory(&fixture("symbology/ean13.png"))
        .unwrap()
        .to_luma8();
    let mut canvas = image::GrayImage::from_pixel(
        qr.width() + ean.width() + 40,
        qr.height().max(ean.height()) + 40,
        image::Luma([255]),
    );
    image::imageops::replace(&mut canvas, &qr, 20, 20);
    image::imageops::replace(&mut canvas, &ean, i64::from(qr.width() + 20), 20);
    assert!(canvas.width() > 512, "the pyramid runs");
    let report = scan(&png(&canvas), ScanProfile::Full.config());
    assert_eq!(
        published(&report),
        [
            (Symbology::QrCode, url.to_owned()),
            (Symbology::Ean13, EAN13.to_owned())
        ]
    );
}

#[test]
fn a_upce_read_is_published_once_another_scale_reads_it() {
    // UPC-E is a symbology the readers invent from noise: a read waits for one
    // at an independent scale. A clean UPC-E large enough for the 512 px
    // pyramid is read there, and again by the decode of that plane at three
    // quarters of its size: published at the pyramid, where the walk ends
    let (text, modules) = upce_modules("123456");
    let img = render_1d(&modules, 12, 300);
    assert!(img.width() > 512, "the pyramid runs");
    let report = scan(&png(&img), ScanProfile::Full.config());
    assert_eq!(published(&report), [(Symbology::UpcE, text)]);
    assert_eq!(stages(&report), ["pyramid"]);
}

#[test]
fn a_low_contrast_ean13_only_the_pyramid_reads_still_settles_there() {
    // the full-resolution decode of this capture reads no EAN-13: the 512 px
    // pyramid's read is the only one, and an EAN-13 needs no second
    let bytes = png(&noisy_ean13(NOISE_SEED, NOISE_SIGMA));
    for profile in [ScanProfile::Full, ScanProfile::Frame] {
        let report = scan(&bytes, profile.clone().config());
        assert_eq!(
            published(&report),
            [(Symbology::Ean13, EAN13.to_owned())],
            "{profile:?}"
        );
        assert_eq!(stages(&report), ["pyramid"], "{profile:?}");
    }
}

#[test]
fn a_databar_expanded_read_out_of_noise_is_not_published() {
    // without the pyramid, the full-resolution decode of the same capture
    // reads a DataBar Expanded out of its noise. The decode of that plane at
    // three quarters of its size does not read it again: nothing stands
    let bytes = png(&noisy_ean13(NOISE_SEED, NOISE_SIGMA));
    let mut config = ScanProfile::Full.config();
    config.pyramid = false;
    config.enhance = false;
    config.deep = false;
    let report = scan(&bytes, config);
    assert_eq!(published(&report), Vec::<(Symbology, String)>::new());
    let direct = &report.trace.stages[0];
    assert_eq!(
        (direct.stage.as_str(), direct.detections_found),
        ("direct", 1)
    );
}
