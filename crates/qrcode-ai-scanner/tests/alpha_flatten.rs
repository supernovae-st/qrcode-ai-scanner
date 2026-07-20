//! Alpha-transparency acceptance — the four host-contract tests plus the
//! exporter-invariance class the live repro surfaced (the same visual
//! design used to score 100, 74 or nothing depending on which tool
//! exported it, because the verdict read the STORED RGB under the
//! transparency).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use qrcode_ai_scanner::{
    AlphaBackground, AlphaPlacement, Hint, ImageInput, ScanConfig, ScanProfile, Scanner, ScoreCheck,
};

/// Rendered luma plane of a clean generated QR (includes its quiet zone).
fn qr_luma(content: &str) -> image::GrayImage {
    let code = qrcode::QrCode::with_error_correction_level(content, qrcode::EcLevel::Q).unwrap();
    code.render::<image::Luma<u8>>()
        .module_dimensions(6, 6)
        .build()
}

/// Map a rendered QR onto RGBA: dark pixels become `module`, light pixels
/// (background + quiet zone) become `background` — exact control over the
/// RGB stored UNDER the alpha, the axis exporters disagree on.
fn qr_rgba(luma: &image::GrayImage, module: [u8; 4], background: [u8; 4]) -> image::RgbaImage {
    let mut out = image::RgbaImage::new(luma.width(), luma.height());
    for (x, y, px) in luma.enumerate_pixels() {
        let color = if px.0[0] < 128 { module } else { background };
        out.put_pixel(x, y, image::Rgba(color));
    }
    out
}

fn png(img: &image::RgbaImage) -> Vec<u8> {
    let mut buf = Vec::new();
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

fn scanner(alpha_background: AlphaBackground) -> Scanner {
    let mut config = ScanConfig::full();
    config.alpha_background = alpha_background;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
}

const URL: &str = "https://qrcode-ai.com/alpha";

/// Brief test 1 — dark modules on a canvas-style transparent background
/// (RGB black stored under alpha 0) must score EXACTLY like the same
/// design flattened over white by the host. The baseline goes through
/// `ImageInput::rgba8` so both sides share the bt601 luma path: equality
/// is structural, not approximate.
#[test]
fn dark_on_transparent_scores_like_flattened_white() {
    let luma = qr_luma(URL);
    let transparent = qr_rgba(&luma, [0, 0, 0, 255], [0, 0, 0, 0]);
    let flattened = qr_rgba(&luma, [0, 0, 0, 255], [255, 255, 255, 255]);

    let scanner = Scanner::default();
    let bytes = png(&transparent);
    let report = scanner.scan(ImageInput::encoded(&bytes)).unwrap();
    let baseline = scanner
        .scan(ImageInput::rgba8(
            flattened.as_raw(),
            flattened.width(),
            flattened.height(),
        ))
        .unwrap();

    assert_eq!(report.detections[0].content.text, URL);
    let alpha = report
        .alpha
        .as_ref()
        .expect("transparent input carries the block");
    assert_eq!(
        alpha.background, "white",
        "dark content flattens over white"
    );
    assert!(!alpha.fallback_used);
    assert_eq!(
        report.score.as_ref().unwrap().value,
        baseline.score.as_ref().unwrap().value,
        "the flatten IS the host-side flatten — same image, same score"
    );
    // a clean synthetic render scores 100 on both sides (the 0.8.0 pin)
    assert_eq!(report.score.as_ref().unwrap().value, 100);
}

/// Brief test 2 — LIGHT modules on transparent must decode over black
/// (auto reads the content as light), never come back empty.
#[test]
fn light_on_transparent_decodes_over_black() {
    let luma = qr_luma(URL);
    let transparent = qr_rgba(&luma, [255, 255, 255, 255], [0, 0, 0, 0]);
    let bytes = png(&transparent);

    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert_eq!(
        report.detections.first().map(|d| d.content.text.as_str()),
        Some(URL),
        "a light design is NEVER a false no-detection"
    );
    let alpha = report.alpha.as_ref().unwrap();
    assert_eq!(
        alpha.background, "black",
        "light content flattens over black"
    );
    assert!(u32::from(alpha.content_luma) > 128);
}

/// Brief test 3 — opaque inputs never carry the block: the serialized
/// report has NO `alpha` key at all (byte-parity with pre-0.9 consumers).
#[test]
fn opaque_input_carries_no_alpha_key() {
    let luma = qr_luma(URL);
    let opaque = qr_rgba(&luma, [0, 0, 0, 255], [255, 255, 255, 255]);
    let bytes = png(&opaque);

    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert!(report.alpha.is_none());
    let json = serde_json::to_value(&report).unwrap();
    assert!(
        json.get("alpha").is_none(),
        "the wire omits the key entirely — not even null"
    );
}

/// Brief test 4 — semi-transparent modules composite proportionally and
/// decode (the anti-aliased-edge / soft-export class), no crash.
#[test]
fn semi_transparent_modules_composite_and_decode() {
    let luma = qr_luma(URL);
    // alpha 160 modules over full transparency: composited over white the
    // modules read (0·160 + 255·95 + 127)/255 ≈ 95 — enough contrast.
    let soft = qr_rgba(&luma, [0, 0, 0, 160], [0, 0, 0, 0]);
    let bytes = png(&soft);

    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert_eq!(report.detections[0].content.text, URL);
    assert!(report.alpha.is_some());
}

/// The class the live repro surfaced: the RGB stored UNDER full
/// transparency (black for canvas exports, white or the original color for
/// editors) must never change the verdict — same visual design, same
/// report, whatever tool exported it.
#[test]
fn exporter_invariance_under_alpha_rgb() {
    let luma = qr_luma(URL);
    let scanner = Scanner::default();
    let exports = [
        [0, 0, 0, 0],       // canvas: black under alpha
        [255, 255, 255, 0], // editor: white under alpha
        [128, 0, 255, 0],   // exotic: whatever the tool left there
    ];
    let reports: Vec<_> = exports
        .iter()
        .map(|&under| {
            let bytes = png(&qr_rgba(&luma, [0, 0, 0, 255], under));
            scanner.scan(ImageInput::encoded(&bytes)).unwrap()
        })
        .collect();
    for report in &reports[1..] {
        assert_eq!(report.detections[0].content.text, URL);
        assert_eq!(
            report.score.as_ref().unwrap().value,
            reports[0].score.as_ref().unwrap().value
        );
        let (a, b) = (
            reports[0].alpha.as_ref().unwrap(),
            report.alpha.as_ref().unwrap(),
        );
        assert_eq!(a.background, b.background);
        assert_eq!(a.content_luma, b.content_luma);
        assert_eq!(a.coverage, b.coverage);
    }
}

/// The placement envelope of a dark design: light backgrounds decode, dark
/// ones cannot — `light_only`, tested endpoints, and the actionable hint.
#[test]
fn envelope_reads_light_only_for_a_dark_design() {
    let luma = qr_luma(URL);
    let transparent = qr_rgba(&luma, [0, 0, 0, 255], [0, 0, 0, 0]);
    let bytes = png(&transparent);

    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let envelope = report
        .alpha
        .as_ref()
        .unwrap()
        .envelope
        .as_ref()
        .expect("full profile sweeps the envelope");
    assert_eq!(envelope.placement, AlphaPlacement::LightOnly);
    assert!(!envelope.safe_luma.is_empty());
    let last = envelope.safe_luma.last().unwrap();
    assert_eq!(last[1], 255, "the lightest tested background decodes");
    assert!(
        report
            .hints
            .iter()
            .any(|h| matches!(h, Hint::AlphaBackgroundDependent { .. })),
        "a narrower-than-any envelope is machine-actionable"
    );
}

/// The envelope rides the standard skip seam — skipping never touches the
/// flatten or the score, it only drops the sweep and its hint.
#[test]
fn envelope_is_skippable_like_any_check() {
    let luma = qr_luma(URL);
    let transparent = qr_rgba(&luma, [0, 0, 0, 255], [0, 0, 0, 0]);
    let bytes = png(&transparent);

    let mut config = ScanConfig::full();
    config.score_skip_checks = vec![ScoreCheck::AlphaEnvelope];
    let report = Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let alpha = report.alpha.as_ref().unwrap();
    assert!(alpha.envelope.is_none());
    assert_eq!(report.score.as_ref().unwrap().value, 100, "score untouched");
    assert!(
        !report
            .hints
            .iter()
            .any(|h| matches!(h, Hint::AlphaBackgroundDependent { .. })),
        "no envelope → its hint structurally cannot fire"
    );
}

/// A forced host background (`#rrggbb`) is the truest verdict when the
/// embedder knows the placement surface.
#[test]
fn forced_custom_background_is_used_and_echoed() {
    let luma = qr_luma(URL);
    let transparent = qr_rgba(&luma, [0, 0, 0, 255], [0, 0, 0, 0]);
    let bytes = png(&transparent);

    let mode = AlphaBackground::from_name("#c8c8c8").expect("valid hex");
    let report = scanner(mode).scan(ImageInput::encoded(&bytes)).unwrap();
    assert_eq!(report.detections[0].content.text, URL);
    let alpha = report.alpha.as_ref().unwrap();
    assert_eq!(alpha.background, "#c8c8c8");
    assert!(
        !alpha.fallback_used,
        "a forced background never second-guesses"
    );
}

/// `none` restores the 0.8.x path: alpha dropped, no block — the verdict
/// depends on the stored RGB again (the documented escape hatch).
#[test]
fn none_mode_drops_alpha_and_carries_no_block() {
    let luma = qr_luma(URL);
    // black modules + BLACK stored under alpha: dropping alpha yields a
    // black-on-black plane — the historical false no-detection.
    let transparent = qr_rgba(&luma, [0, 0, 0, 255], [0, 0, 0, 0]);
    let bytes = png(&transparent);

    let report = scanner(AlphaBackground::None)
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert!(report.alpha.is_none(), "none carries no block at all");
    assert!(
        report.detections.is_empty(),
        "the 0.8.x behavior reproduced exactly — that IS the escape hatch"
    );
}

/// The auto retry: a design whose alpha-weighted mean mis-calls the
/// background (white modules + a big opaque black plate drags the mean
/// dark → auto flattens white → white-on-white) must be rescued by the
/// opposite-background walk, marked `fallback_used`.
#[test]
fn auto_fallback_rescues_a_mis_called_mean() {
    let luma = qr_luma(URL);
    let (w, h) = (luma.width(), luma.height());
    // left: white modules on transparency · right: an opaque black plate
    // twice the QR's area (the visible mass reads dark on average)
    let mut img = image::RgbaImage::new(w * 3, h);
    for (x, y, px) in luma.enumerate_pixels() {
        let color = if px.0[0] < 128 {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 0]
        };
        img.put_pixel(x, y, image::Rgba(color));
    }
    for x in w..w * 3 {
        for y in 0..h {
            img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
        }
    }
    let bytes = png(&img);

    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let alpha = report.alpha.as_ref().unwrap();
    assert!(
        u32::from(alpha.content_luma) < 128,
        "the plate drags the mean dark (the mis-call premise)"
    );
    assert_eq!(
        report.detections.first().map(|d| d.content.text.as_str()),
        Some(URL),
        "the opposite-background walk rescues the scan"
    );
    assert!(alpha.fallback_used);
    assert_eq!(
        alpha.background, "black",
        "the report names the rescuing background"
    );
}

/// Raw `Rgba8` inputs (browser `ImageData`) walk the same flatten as encoded
/// PNGs — one seam, every binding.
#[test]
fn raw_rgba8_input_flattens_identically() {
    let luma = qr_luma(URL);
    let transparent = qr_rgba(&luma, [0, 0, 0, 255], [0, 0, 0, 0]);
    let bytes = png(&transparent);

    let scanner = Scanner::default();
    let from_png = scanner.scan(ImageInput::encoded(&bytes)).unwrap();
    let from_raw = scanner
        .scan(ImageInput::rgba8(
            transparent.as_raw(),
            transparent.width(),
            transparent.height(),
        ))
        .unwrap();
    assert_eq!(
        from_png.score.as_ref().unwrap().value,
        from_raw.score.as_ref().unwrap().value
    );
    assert_eq!(
        from_png.alpha.as_ref().unwrap().background,
        from_raw.alpha.as_ref().unwrap().background
    );
    assert_eq!(
        from_png.alpha.as_ref().unwrap().coverage,
        from_raw.alpha.as_ref().unwrap().coverage
    );
}
