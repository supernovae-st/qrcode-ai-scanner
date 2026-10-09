//! Rescue geometry stays aligned when ordinary engine inputs are downscaled.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{EngineKind, ImageInput, ScanConfig, ScanProfile, Scanner, ScoreDepth};

#[test]
fn rescue_uses_original_space_after_engine_downscaling() {
    let bytes = include_bytes!("../../../fixtures/degraded/logo-occluded-rescue.png");
    let small = image::load_from_memory(bytes).unwrap().into_luma8();
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
