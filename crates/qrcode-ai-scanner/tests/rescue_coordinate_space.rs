//! Rescue geometry stays aligned when ordinary engine inputs are downscaled.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use image::imageops::FilterType;
use image::{GrayImage, Luma, Rgba, RgbaImage};
use qrcode_ai_scanner::{
    AlphaBackground, EngineKind, ImageInput, Point, ScanConfig, ScanProfile, ScanReport, Scanner,
    ScoreDepth, StructuralReport,
};

const RESCUE_PAYLOAD: &str = "https://qrcode-ai.com/rescue-pin";

/// The committed rescue fixture renders its version-5 symbol at 8 px per
/// module (37 modules from x = 32 to x = 327 of the 360 px image).
const FIXTURE_MODULE_PX: f32 = 8.0;

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

#[test]
fn rescue_samples_capped_plane_and_reports_original_corners() {
    let bytes = fixture("degraded/logo-occluded-rescue.png");
    let small = image::load_from_memory(&bytes).unwrap().into_luma8();
    assert_eq!((small.width(), small.height()), (360, 360));

    let mut config = ScanConfig::full();
    config.budget_ms = None;
    config.score_depth = ScoreDepth::Off;
    config.max_engine_side = 360;
    let scanner = Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build();
    let baseline = scanner
        .scan(ImageInput::luma8(small.as_raw(), 360, 360))
        .unwrap();
    assert_eq!(baseline.detections.len(), 1);
    assert_eq!(baseline.detections[0].engines, vec![EngineKind::Rescue]);
    assert_eq!(
        baseline.detections[0].content.text,
        "https://qrcode-ai.com/rescue-pin"
    );

    // Integer nearest scaling followed by the engine cap recreates the same
    // working image; only the original-space candidate geometry differs.
    let large = image::imageops::resize(&small, 2880, 2880, image::imageops::FilterType::Nearest);
    let expanded = scanner
        .scan(ImageInput::luma8(large.as_raw(), 2880, 2880))
        .unwrap();
    assert_eq!(expanded.detections.len(), 1, "trace: {:?}", expanded.trace);
    assert_eq!(expanded.detections[0].engines, vec![EngineKind::Rescue]);
    assert_eq!(
        expanded.detections[0].content.text,
        "https://qrcode-ai.com/rescue-pin"
    );
    assert_eq!(
        expanded.detections[0].content.raw,
        baseline.detections[0].content.raw
    );
    let baseline_corners = baseline.detections[0].corners.as_ref().unwrap();
    let expanded_corners = expanded.detections[0].corners.as_ref().unwrap();
    for (small_corner, large_corner) in baseline_corners.iter().zip(expanded_corners) {
        assert!((large_corner.x - small_corner.x * 8.0).abs() < 1e-3);
        assert!((large_corner.y - small_corner.y * 8.0).abs() < 1e-3);
    }
}

fn rescue_fixture() -> GrayImage {
    let bytes = fixture("degraded/logo-occluded-rescue.png");
    let small = image::load_from_memory(&bytes).unwrap().into_luma8();
    assert_eq!((small.width(), small.height()), (360, 360));
    small
}

/// Unbudgeted Full configuration with an explicit engine cap.
fn capped_config(max_engine_side: u32, score_depth: ScoreDepth) -> ScanConfig {
    let mut config = ScanConfig::full();
    config.budget_ms = None;
    config.score_depth = score_depth;
    config.max_engine_side = max_engine_side;
    config
}

fn scanner_for(config: ScanConfig) -> Scanner {
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
}

fn capped_scanner(max_engine_side: u32, score_depth: ScoreDepth) -> Scanner {
    scanner_for(capped_config(max_engine_side, score_depth))
}

fn scan(scanner: &Scanner, image: &GrayImage) -> ScanReport {
    scanner
        .scan(ImageInput::luma8(
            image.as_raw(),
            image.width(),
            image.height(),
        ))
        .unwrap()
}

fn nearest(image: &GrayImage, width: u32, height: u32) -> GrayImage {
    image::imageops::resize(image, width, height, FilterType::Nearest)
}

fn inverted(image: &GrayImage) -> GrayImage {
    let mut inverted = image.clone();
    image::imageops::invert(&mut inverted);
    inverted
}

/// `symbol` pasted at (`x`, `y`) on a white canvas.
fn on_white_canvas(width: u32, height: u32, symbol: &GrayImage, x: u32, y: u32) -> GrayImage {
    let mut canvas = GrayImage::from_pixel(width, height, Luma([255]));
    image::imageops::overlay(&mut canvas, symbol, i64::from(x), i64::from(y));
    canvas
}

/// Test dimensions stay far below 2^24, so they are exact in `f32`.
#[expect(
    clippy::cast_precision_loss,
    reason = "test dimensions stay below 2^24"
)]
fn px(value: u32) -> f32 {
    value as f32
}

/// Exact replica of the engine cap (`transform::downscale_to`): integer box
/// average over floor-bounded source spans.
fn engine_cap_replica(image: &GrayImage, max_side: u32) -> GrayImage {
    let (width, height) = (u64::from(image.width()), u64::from(image.height()));
    let longest = width.max(height);
    let max_side = u64::from(max_side);
    if longest <= max_side {
        return image.clone();
    }
    let out_width = (width * max_side / longest).max(1);
    let out_height = (height * max_side / longest).max(1);
    let source = image.as_raw();
    let mut data = Vec::new();
    for out_row in 0..out_height {
        let top = out_row * height / out_height;
        let bottom = ((out_row + 1) * height / out_height).max(top + 1);
        for out_col in 0..out_width {
            let left = out_col * width / out_width;
            let right = ((out_col + 1) * width / out_width).max(left + 1);
            let mut sum = 0u64;
            for row in top..bottom {
                for col in left..right {
                    sum += u64::from(source[usize::try_from(row * width + col).unwrap()]);
                }
            }
            data.push(u8::try_from(sum / ((bottom - top) * (right - left))).unwrap());
        }
    }
    GrayImage::from_raw(
        u32::try_from(out_width).unwrap(),
        u32::try_from(out_height).unwrap(),
        data,
    )
    .unwrap()
}

/// Asserts that the report holds exactly the rescue fixture's symbol and that
/// S5 recovered it: rescue is the stage whose sampling crosses from
/// input-space candidate corners onto the capped plane, so a case decoded
/// earlier would not exercise that conversion.
fn assert_rescued<'a>(report: &'a ScanReport, raw: &[u8], case: &str) -> &'a [Point; 4] {
    assert_eq!(report.detections.len(), 1, "{case}: {:?}", report.trace);
    let detection = &report.detections[0];
    assert_eq!(detection.content.text, RESCUE_PAYLOAD, "{case}");
    assert_eq!(detection.content.raw, raw, "{case}");
    assert_eq!(detection.engines, vec![EngineKind::Rescue], "{case}");
    let last = report.trace.stages.last().unwrap();
    assert_eq!(
        (last.stage.as_str(), last.detections_found),
        ("rescue", 1),
        "{case}"
    );
    detection.corners.as_ref().unwrap()
}

/// The rescued symbol's raw bytes and corners from `scanner` on `image`.
fn rescued_reference(scanner: &Scanner, image: &GrayImage, case: &str) -> (Vec<u8>, [Point; 4]) {
    let report = scan(scanner, image);
    assert_eq!(report.detections.len(), 1, "{case}: {:?}", report.trace);
    let raw = report.detections[0].content.raw.clone();
    let corners = *assert_rescued(&report, &raw, case);
    (raw, corners)
}

/// The 360-space reference: the committed fixture scanned as-is.
fn reference() -> (Vec<u8>, [Point; 4]) {
    rescued_reference(
        &capped_scanner(360, ScoreDepth::Off),
        &rescue_fixture(),
        "reference",
    )
}

/// Asserts every published corner lies at `reference · scale + offset`
/// (per axis), within `tolerance` input pixels.
fn assert_corners_near(
    case: &str,
    actual: &[Point; 4],
    reference: &[Point; 4],
    scale: (f32, f32),
    offset: (f32, f32),
    tolerance: f32,
) {
    for (index, (got, base)) in actual.iter().zip(reference).enumerate() {
        let want = (base.x * scale.0 + offset.0, base.y * scale.1 + offset.1);
        assert!(
            (got.x - want.0).abs() <= tolerance && (got.y - want.1).abs() <= tolerance,
            "{case}: corner {index} at ({}, {}), expected ({}, {}) ± {tolerance}",
            got.x,
            got.y,
            want.0,
            want.1
        );
    }
}

/// Half a module of the symbol enlarged by `scale`, in input pixels. Corners
/// within it still sample the right modules. A missing or doubled rescale
/// misses the far corners by hundreds of pixels.
fn half_module(scale: f32) -> f32 {
    FIXTURE_MODULE_PX * scale / 2.0
}

/// The report with its wall-clock fields zeroed. Everything else is
/// deterministic by contract (pipeline spec, normative property 1).
fn without_wall_clock(mut report: ScanReport) -> ScanReport {
    report.trace.total_ms = 0.0;
    for stage in &mut report.trace.stages {
        stage.ms = 0.0;
    }
    report
}

fn structural<'a>(report: &'a ScanReport, case: &str) -> &'a StructuralReport {
    let score = report
        .score
        .as_ref()
        .unwrap_or_else(|| panic!("{case}: no score (a scoring panic degrades to none)"));
    score
        .structural
        .as_ref()
        .unwrap_or_else(|| panic!("{case}: no structural check"))
}

/// Asserts that scoring read the finders through the published corners on
/// the input-resolution plane as it does on the 360 px reference.
fn assert_structural_agrees(report: &ScanReport, reference: &ScanReport, case: &str) {
    let (got, want) = (structural(report, case), structural(reference, case));
    for (got_finder, want_finder) in got.finder_integrity.iter().zip(&want.finder_integrity) {
        assert!(
            (got_finder - want_finder).abs() <= 0.05,
            "{case}: {got:?} vs {want:?}"
        );
    }
    assert_eq!(got.quiet_zone_ok, want.quiet_zone_ok, "{case}");
}

#[test]
fn capped_scan_matches_a_scan_of_its_own_working_plane() {
    // Scanning the capped plane directly runs the same attempts on the same
    // pixels, so any difference besides the corner scale is a coordinate
    // defect, not a yield difference.
    let cases = [
        (2880, 2880, 360, false),
        (1000, 1003, 360, false),
        (1003, 1000, 360, true),
        (2883, 2880, 360, false),
        (4000, 2500, 2048, false),
    ];
    let upright = rescue_fixture();
    let light_on_dark = inverted(&upright);
    let mut rescued_cases = 0;
    for (width, height, cap, invert) in cases {
        let case = format!("{width}x{height} cap {cap} inverted {invert}");
        let source = if invert { &light_on_dark } else { &upright };
        let large = nearest(source, width, height);
        let work = engine_cap_replica(&large, cap);
        let scanner = capped_scanner(cap, ScoreDepth::Off);
        let capped = scan(&scanner, &large);
        let direct = scan(&scanner, &work);
        assert_eq!(
            capped.detections.len(),
            direct.detections.len(),
            "{case}: {:?} vs {:?}",
            capped.trace,
            direct.trace
        );
        let scale = (px(width) / px(work.width()), px(height) / px(work.height()));
        for (got, want) in capped.detections.iter().zip(&direct.detections) {
            assert_eq!(got.engines, want.engines, "{case}");
            assert_eq!(got.content.raw, want.content.raw, "{case}");
            assert_eq!(got.meta.inverted, want.meta.inverted, "{case}");
            match (&got.corners, &want.corners) {
                (Some(got_corners), Some(want_corners)) => {
                    assert_corners_near(&case, got_corners, want_corners, scale, (0.0, 0.0), 1e-2);
                }
                (None, None) => {}
                _ => panic!("{case}: corners {:?} vs {:?}", got.corners, want.corners),
            }
            if got.engines.contains(&EngineKind::Rescue) {
                rescued_cases += 1;
            }
        }
    }
    assert!(
        rescued_cases > 0,
        "no case reached S5: the check is vacuous"
    );
}

#[test]
fn rescue_corners_survive_a_non_integer_engine_cap() {
    let (raw, reference_corners) = reference();
    // 1000 / 360 is not an integer, so the cap's box filter blends module
    // edges: the capped plane is no longer the committed fixture.
    let input = nearest(&rescue_fixture(), 1000, 1000);
    let report = scan(&capped_scanner(360, ScoreDepth::Off), &input);
    let corners = assert_rescued(&report, &raw, "1000 px");
    let scale = 1000.0 / 360.0;
    assert_corners_near(
        "1000 px",
        corners,
        &reference_corners,
        (scale, scale),
        (0.0, 0.0),
        half_module(scale),
    );
}

#[test]
fn rescue_corners_stay_in_input_space_on_non_square_canvases() {
    let (raw, reference_corners) = reference();
    let symbol = nearest(&rescue_fixture(), 1800, 1800);
    let tolerance = half_module(5.0);

    // Landscape, cap 576 on the 2880 px side: exactly ×1/5. S1 also runs on
    // a 512 px view of that plane (576 > pyramid side), so candidate geometry
    // may cross three spaces (pyramid view → input → capped plane).
    let landscape = on_white_canvas(2880, 1800, &symbol, 540, 0);
    let report = scan(&capped_scanner(576, ScoreDepth::Off), &landscape);
    let corners = assert_rescued(&report, &raw, "landscape 2880x1800");
    assert_corners_near(
        "landscape 2880x1800",
        corners,
        &reference_corners,
        (5.0, 5.0),
        (540.0, 0.0),
        tolerance,
    );

    // Portrait, cap 360 on the 2880 px side (×1/8). The offset now sits on y,
    // so swapped axes would move the symbol by 540 px.
    let portrait = on_white_canvas(1800, 2880, &symbol, 0, 540);
    let report = scan(&capped_scanner(360, ScoreDepth::Off), &portrait);
    let corners = assert_rescued(&report, &raw, "portrait 1800x2880");
    assert_corners_near(
        "portrait 1800x2880",
        corners,
        &reference_corners,
        (5.0, 5.0),
        (0.0, 540.0),
        tolerance,
    );
}

#[test]
fn rescue_corners_cross_the_pyramid_view_exactly() {
    // A 720 px cap over a pyramid side of 360 makes the S1 view of the ×8
    // input the committed fixture itself (2880 → 720 → 360). S1 runs first,
    // so its candidate is the one S5 samples on the 720 px plane.
    let mut config = capped_config(720, ScoreDepth::Off);
    config.enhance = false;
    config.deep = false;
    config.pyramid_side = 360;
    let scanner = scanner_for(config);
    let small = rescue_fixture();
    let (raw, reference_corners) = rescued_reference(&scanner, &small, "pyramid 360");

    let report = scan(&scanner, &nearest(&small, 2880, 2880));
    assert!(
        report
            .trace
            .stages
            .iter()
            .any(|stage| stage.stage == "pyramid"),
        "{:?}",
        report.trace
    );
    let corners = assert_rescued(&report, &raw, "pyramid x8");
    assert_corners_near(
        "pyramid x8",
        corners,
        &reference_corners,
        (8.0, 8.0),
        (0.0, 0.0),
        1e-3,
    );
}

#[test]
fn inverted_rescue_keeps_polarity_and_input_corners_under_the_cap() {
    // Light modules on dark: only inverting attempts read the grid, and the
    // polarity travels with the geometry into rescue sampling.
    let light_on_dark = inverted(&rescue_fixture());
    let scanner = capped_scanner(360, ScoreDepth::Off);
    let baseline = scan(&scanner, &light_on_dark);
    assert_eq!(baseline.detections.len(), 1, "{:?}", baseline.trace);
    let raw = baseline.detections[0].content.raw.clone();
    let baseline_corners = *assert_rescued(&baseline, &raw, "inverted 360");
    assert_eq!(baseline.detections[0].meta.inverted, Some(true));

    let report = scan(&scanner, &nearest(&light_on_dark, 2880, 2880));
    let corners = assert_rescued(&report, &raw, "inverted x8");
    assert_eq!(report.detections[0].meta.inverted, Some(true));
    // ×8 nearest, then the ×1/8 cap, rebuilds the inverted fixture exactly.
    assert_corners_near(
        "inverted x8",
        corners,
        &baseline_corners,
        (8.0, 8.0),
        (0.0, 0.0),
        1e-3,
    );
}

#[test]
fn full_profile_scan_of_the_enlarged_fixture_is_deterministic() {
    let mut config = ScanConfig::full();
    config.budget_ms = None;
    let scanner = scanner_for(config);
    let small = rescue_fixture();
    let baseline = scan(&scanner, &small);
    assert_eq!(baseline.detections.len(), 1, "{:?}", baseline.trace);
    let reference_corners = baseline.detections[0].corners.unwrap();

    let input = nearest(&small, 2880, 2880);
    let first = scan(&scanner, &input);
    let second = scan(&scanner, &input);
    assert_eq!(first.trace.engine_panics, 0, "{:?}", first.trace);
    assert_eq!(first.detections.len(), 1, "{:?}", first.trace);
    let detection = &first.detections[0];
    assert_eq!(detection.content.text, RESCUE_PAYLOAD);
    assert_eq!(detection.content.raw, baseline.detections[0].content.raw);
    // The shipped 2048 px cap is a ×0.711 ratio, not an integer, so the
    // corners carry the precision of whichever pass read the grid.
    assert_corners_near(
        "full x8",
        detection.corners.as_ref().unwrap(),
        &reference_corners,
        (8.0, 8.0),
        (0.0, 0.0),
        half_module(8.0),
    );
    assert_structural_agrees(&first, &baseline, "full x8");
    assert_eq!(without_wall_clock(first), without_wall_clock(second));
}

#[test]
fn rescued_corners_feed_full_scoring_in_input_space() {
    let scanner = capped_scanner(360, ScoreDepth::Full);
    let small = rescue_fixture();
    let baseline = scan(&scanner, &small);
    assert_eq!(baseline.detections.len(), 1, "{:?}", baseline.trace);
    let raw = baseline.detections[0].content.raw.clone();
    let baseline_corners = *assert_rescued(&baseline, &raw, "scored 360");

    let input = nearest(&small, 2880, 2880);
    let first = scan(&scanner, &input);
    let second = scan(&scanner, &input);
    assert_eq!(first.trace.engine_panics, 0, "{:?}", first.trace);
    let corners = assert_rescued(&first, &raw, "scored x8");
    assert_corners_near(
        "scored x8",
        corners,
        &baseline_corners,
        (8.0, 8.0),
        (0.0, 0.0),
        1e-3,
    );
    // Scoring samples the 2880 px input plane through the rescued corners.
    assert_structural_agrees(&first, &baseline, "scored x8");
    assert_eq!(without_wall_clock(first), without_wall_clock(second));
}

#[test]
fn engine_cap_gates_the_ladder_for_a_large_clean_fixture() {
    let bytes = fixture("clean/gen_v5_q.png");
    let clean = image::load_from_memory(&bytes).unwrap().into_luma8();
    assert_eq!((clean.width(), clean.height()), (360, 360));
    let reference = scan(&capped_scanner(360, ScoreDepth::Off), &clean);
    assert_eq!(reference.detections.len(), 1, "{:?}", reference.trace);
    let reference_corners = reference.detections[0].corners.unwrap();
    let large = nearest(&clean, 2880, 2880);

    // No public field reports the capped plane's size. The S1 gate exposes
    // it: the pyramid runs only when the plane the ladder walks is longer
    // than `pyramid_side` (512).
    let capped = scan(&capped_scanner(360, ScoreDepth::Off), &large);
    let stages: Vec<&str> = capped
        .trace
        .stages
        .iter()
        .map(|stage| stage.stage.as_str())
        .collect();
    assert_eq!(stages, ["direct"], "360 px plane: no S1, decoded in S2");
    assert_eq!(capped.detections.len(), 1);
    assert_eq!(
        capped.detections[0].content.text,
        "https://qrcode-ai.com/c/v5q"
    );
    // The capped plane is the committed fixture, so the corners are exact.
    assert_corners_near(
        "clean x8 capped",
        capped.detections[0].corners.as_ref().unwrap(),
        &reference_corners,
        (8.0, 8.0),
        (0.0, 0.0),
        1e-3,
    );

    let uncapped = scan(&capped_scanner(4096, ScoreDepth::Off), &large);
    assert_eq!(uncapped.trace.stages[0].stage, "pyramid", "2880 px plane");
}

#[test]
fn engine_cap_bounds_every_pass() {
    // The 8 px modules decode at full resolution. A ×1/16 cap shrinks them to
    // half a pixel, which no pass can read, so a decode under the cap would
    // mean some pass saw the uncapped plane.
    let bytes = fixture("clean/gen_v2_q.png");
    let symbol = image::load_from_memory(&bytes).unwrap().into_luma8();
    let input = on_white_canvas(2048, 2048, &symbol, 892, 892);

    let uncapped = scan(&capped_scanner(2048, ScoreDepth::Off), &input);
    assert_eq!(uncapped.detections.len(), 1, "{:?}", uncapped.trace);
    assert_eq!(uncapped.detections[0].content.text, "qrc.ai/v2q");

    let capped = scan(&capped_scanner(128, ScoreDepth::Off), &input);
    assert!(capped.detections.is_empty(), "{:?}", capped.trace);
    let stages: Vec<&str> = capped
        .trace
        .stages
        .iter()
        .map(|stage| stage.stage.as_str())
        .collect();
    assert_eq!(stages, ["direct", "enhance", "deep"]);
}

/// A clean version-5 H rescue-pin symbol (8 px modules, 360 px) whose centred
/// disc has dark modules `(g, g, g, 255)` and light modules
/// `(255, 255, 255, g)`. Over black the disc composites to flat `g`, a logo
/// occlusion like the fixture's. Over white its dark modules stay readable.
/// The 92 px radius sits mid-band for `g = 128`: below about 82 px an engine
/// still reads the symbol over black, and rescue still decodes at 105 px.
fn translucent_disc_symbol(g: u8) -> RgbaImage {
    let code = qrcode::QrCode::with_version(
        RESCUE_PAYLOAD.as_bytes(),
        qrcode::Version::Normal(5),
        qrcode::EcLevel::H,
    )
    .unwrap();
    let modules = code.render::<Luma<u8>>().module_dimensions(8, 8).build();
    assert_eq!(modules.dimensions(), (360, 360));
    RgbaImage::from_fn(360, 360, |x, y| {
        let dark = modules.get_pixel(x, y)[0] < 128;
        let (dx, dy) = (i64::from(x) - 180, i64::from(y) - 180);
        let in_disc = dx * dx + dy * dy <= 92 * 92;
        match (in_disc, dark) {
            (true, true) => Rgba([g, g, g, 255]),
            (true, false) => Rgba([255, 255, 255, g]),
            (false, true) => Rgba([0, 0, 0, 255]),
            (false, false) => Rgba([255, 255, 255, 255]),
        }
    })
}

#[test]
fn capped_rescue_on_the_auto_background_pre_empts_the_opposite_background_walk() {
    let symbol = translucent_disc_symbol(128);
    let input = image::imageops::resize(&symbol, 2880, 2880, FilterType::Nearest);
    let scan_rgba = |config: ScanConfig| {
        scanner_for(config)
            .scan(ImageInput::rgba8(input.as_raw(), 2880, 2880))
            .unwrap()
    };

    // Light content picks black. The pipeline runs S5 before the
    // opposite-background walk, so a rescue on the capped plane settles it.
    let report = scan_rgba(capped_config(360, ScoreDepth::Off));
    assert_eq!(report.detections.len(), 1, "{:?}", report.trace);
    assert_eq!(report.detections[0].content.text, RESCUE_PAYLOAD);
    assert_eq!(report.detections[0].engines, vec![EngineKind::Rescue]);
    let alpha = report.alpha.as_ref().unwrap();
    assert_eq!(alpha.background, "black");
    assert!(!alpha.fallback_used, "{alpha:?}");

    // The premise: over white an engine decodes it, so the Auto walk would
    // have fallen back there had S5 failed on the capped plane.
    let mut config = capped_config(360, ScoreDepth::Off);
    config.alpha_background = AlphaBackground::White;
    let white = scan_rgba(config);
    assert_eq!(white.detections.len(), 1, "{:?}", white.trace);
    assert_eq!(white.detections[0].content.text, RESCUE_PAYLOAD);
    assert!(
        !white.detections[0].engines.contains(&EngineKind::Rescue),
        "{:?}",
        white.detections[0].engines
    );
}
