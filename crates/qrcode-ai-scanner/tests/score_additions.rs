//! The 0.9.0 score additions — the honesty integer (`weights_run`), the
//! bisected knee (`refined_failed_at`), and the named posture presets.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{
    ImageInput, ScanConfig, ScanProfile, Scanner, Score, ScorePreset, StressAxis,
};

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

/// The Full profile without its wall-clock budget: a judgment the budget
/// cannot complete is absent, so these pins must never depend on load.
fn unbudgeted_full() -> ScanConfig {
    let mut config = ScanConfig::full();
    config.budget_ms = None;
    config
}

/// The wire labels of an axis's five cells (spec/04-score.md).
fn cell_labels(axis: StressAxis) -> [&'static str; 5] {
    match axis {
        StressAxis::Resolution => ["358px", "256px", "179px", "128px", "90px"],
        StressAxis::Blur => ["blur 0.5", "blur 1.0", "blur 1.5", "blur 2.0", "blur 2.5"],
        StressAxis::Contrast => [
            "contrast 70%",
            "contrast 55%",
            "contrast 40%",
            "contrast 30%",
            "contrast 20%",
        ],
        StressAxis::Perspective => ["10°", "18°", "26°", "34°", "42°"],
        StressAxis::Rotation => ["10°", "20°", "30°", "40°", "50°"],
        StressAxis::Lighting => [
            "soft shadow",
            "hard shadow",
            "glare",
            "overexposure",
            "underexposure",
        ],
        other => panic!("unknown axis {other:?}"),
    }
}

/// A judgment is whole: `passed == total` exactly when nothing failed, and
/// a lost point names one of its axis's cells.
fn assert_honest(score: &Score) {
    for a in &score.axes {
        assert_eq!(a.passed == a.total, a.failed_at.is_none(), "{a:?}");
        if let Some(label) = a.failed_at.as_deref() {
            assert!(cell_labels(a.axis).contains(&label), "{a:?}");
        }
    }
}

fn generated_qr() -> Vec<u8> {
    let code =
        qrcode::QrCode::with_error_correction_level("https://qrcode-ai.com/w", qrcode::EcLevel::Q)
            .unwrap();
    let img = code
        .render::<image::Luma<u8>>()
        .module_dimensions(6, 6)
        .build();
    let mut buf = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

/// The full six-axis contract stands behind a default scan — and the wire
/// says so.
#[test]
fn full_contract_reads_weights_run_100() {
    let bytes = generated_qr();
    let report = Scanner::builder()
        .profile(ScanProfile::Custom(unbudgeted_full()))
        .build()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let score = report.score.unwrap();
    assert_honest(&score);
    assert_eq!(score.weights_run, 100);
}

/// Skipping axes renormalizes AND declares: perspective(20)+rotation(10)
/// out → 70 of the contract ran.
#[test]
fn skipped_axes_declare_their_weight() {
    let bytes = generated_qr();
    let mut config = unbudgeted_full();
    config.score_skip_axes = vec![StressAxis::Perspective, StressAxis::Rotation];
    let report = Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let score = report.score.unwrap();
    assert_honest(&score);
    assert_eq!(score.weights_run, 70);
    assert_eq!(score.axes.len(), 4);
}

/// The presets are the drift-proof spelling: `design` skips exactly
/// perspective+rotation (lighting STAYS — glare measures the design),
/// `capture` skips nothing, junk rejects.
#[test]
fn presets_spell_the_two_postures() {
    assert_eq!(
        ScorePreset::from_name("design").unwrap().skips(),
        vec![StressAxis::Perspective, StressAxis::Rotation]
    );
    assert!(
        ScorePreset::from_name("capture")
            .unwrap()
            .skips()
            .is_empty()
    );
    for junk in ["Design", "builder", ""] {
        assert!(ScorePreset::from_name(junk).is_none(), "{junk:?}");
    }
}

/// A knee at Full depth gains ONE bisected probe: `refined_failed_at` is
/// the tightest TESTED failing intensity. Clean 5/5 axes carry none.
#[test]
fn knee_axes_carry_a_refined_label() {
    let bytes = fixture("artistic/blob-style-monkey-logo.webp");
    let report = Scanner::builder()
        .profile(ScanProfile::Custom(unbudgeted_full()))
        .build()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let score = report.score.unwrap();
    assert_honest(&score);
    let kneed: Vec<_> = score
        .axes
        .iter()
        .filter(|a| a.failed_at.is_some() && a.axis != StressAxis::Lighting)
        .collect();
    assert!(
        !kneed.is_empty(),
        "the artistic fixture has knees (scores ~70)"
    );
    for axis in &kneed {
        let refined = axis
            .refined_failed_at
            .as_ref()
            .expect("every ordered-ramp knee is refined at Full depth");
        assert!(!refined.is_empty());
    }
    for axis in score.axes.iter().filter(|a| a.failed_at.is_none()) {
        assert!(
            axis.refined_failed_at.is_none(),
            "no knee → nothing to refine ({:?})",
            axis.axis
        );
    }
}

/// Pre-0.9 reports parse leniently: a score without `weights_run` reads 0.
#[test]
fn old_reports_parse_with_weights_run_default() {
    let old = r#"{"value":87,"grade":"excellent","axes":[],"structural":null,"uec":null,"iso15415":null}"#;
    let score: qrcode_ai_scanner::Score = serde_json::from_str(old).unwrap();
    assert_eq!(score.weights_run, 0);
}
