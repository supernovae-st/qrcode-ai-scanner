//! Score truth under skips and budgets: what ships is a WHOLE judgment or
//! nothing. Skipping a check withholds a section's why, never the verdict;
//! a wall-clock cut is absent, never a partial composite whose unrun cells
//! read as failures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{
    Hint, ImageInput, ScanConfig, ScanProfile, ScanReport, Scanner, Score, ScoreCheck, StressAxis,
};

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

fn qr_png(content: &str, module_px: u32) -> Vec<u8> {
    let code = qrcode::QrCode::with_error_correction_level(content, qrcode::EcLevel::Q).unwrap();
    let img = code
        .render::<image::Luma<u8>>()
        .module_dimensions(module_px, module_px)
        .build();
    let mut buf = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

fn scan(bytes: &[u8], config: ScanConfig) -> ScanReport {
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
        .scan(ImageInput::encoded(bytes))
        .unwrap()
}

/// The Full profile without its wall-clock budget — the reproducible mode.
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

/// The quiet-ring phase pair are REAL thin margins on the rqrr path — 5 and
/// 6 corrected errors of 16 in the worst block (margins 0.375 · 0.25),
/// capped at 85 and 70. Skipping `uec`, `iso15415` or both withholds those
/// sections and the hints the margin drives; value, grade, weights, axes
/// and the ISO `overall` (the minimum over every MEASURED parameter, a
/// withheld one included) never move. (While the cap read only the
/// PUBLISHED margin, `[uec]` lifted the pair to 100 and 88.)
#[test]
fn skip_checks_never_move_the_verdict() {
    for (rel, margin, value) in [
        ("degraded/quiet-ring-phase-1015.png", 0.375, 85),
        ("degraded/quiet-ring-phase-1023.png", 0.25, 70),
    ] {
        let bytes = fixture(rel);
        let shown = scan(&bytes, unbudgeted_full());
        let score = shown.score.as_ref().expect("the default checks judge");
        assert_honest(score);
        let uec = score
            .uec
            .expect("precondition: the rqrr stream measures the margin");
        assert!((uec.margin - margin).abs() < 1e-6, "{rel}: {uec:?}");
        assert_eq!(
            score.value, value,
            "{rel}: the measured margin caps the value"
        );

        for checks in [
            vec![ScoreCheck::Uec],
            vec![ScoreCheck::Iso15415],
            vec![ScoreCheck::Uec, ScoreCheck::Iso15415],
        ] {
            let mut config = unbudgeted_full();
            config.score_skip_checks.clone_from(&checks);
            let hidden = scan(&bytes, config);
            let skipped = hidden
                .score
                .as_ref()
                .expect("skipping a check still judges");
            assert_eq!(
                (skipped.value, skipped.grade, skipped.weights_run),
                (score.value, score.grade, score.weights_run),
                "{rel} {checks:?}: a skip never moves the verdict"
            );
            assert_eq!(skipped.axes, score.axes, "{rel} {checks:?}");
            let withheld = checks.contains(&ScoreCheck::Uec);
            assert_eq!(skipped.uec.is_none(), withheld, "{rel} {checks:?}");
            if let Some(iso) = skipped.iso15415 {
                assert_eq!(
                    iso.unused_error_correction.is_none(),
                    withheld,
                    "{rel} {checks:?}: the ISO parameter follows the section"
                );
                let published = score.iso15415.expect("the default checks grade ISO");
                assert_eq!(
                    iso.overall, published.overall,
                    "{rel} {checks:?}: a skip never moves the ISO overall"
                );
            }
            // the pair's axes alone compose ≥ 70, so the raise-EC hint here
            // is the margin's own — it speaks only through the section
            let expected: Vec<Hint> = shown
                .hints
                .iter()
                .filter(|h| {
                    !withheld
                        || !matches!(
                            h,
                            Hint::RaiseErrorCorrection { .. } | Hint::LowCorrectionMargin { .. }
                        )
                })
                .cloned()
                .collect();
            assert_eq!(hidden.hints, expected, "{rel} {checks:?}");
        }
    }
}

/// The Undecodable boundary: a symbol only the S5 rescue decodes (errors
/// and erasures past both engines' RS limits) has no stress-cell decode
/// class at all. That is a legitimately zero margin, not a missing
/// judgment: unbudgeted, the score is present and whole — every axis
/// judged, each failing at its first cell — and pins at 0 (poor).
#[test]
fn a_rescue_only_decode_is_still_judged_whole() {
    let report = scan(
        &fixture("degraded/logo-occluded-rescue.png"),
        unbudgeted_full(),
    );
    let detection = report.detections.first().expect("the rescue decodes");
    assert_eq!(
        detection.engines,
        vec![qrcode_ai_scanner::EngineKind::Rescue],
        "precondition: a rescue-only decode"
    );
    let score = report
        .score
        .as_ref()
        .expect("an undecodable base still judges");
    assert_honest(score);
    assert_eq!(score.weights_run, 100);
    for axis in &score.axes {
        assert_eq!(axis.passed, 0, "no stress cell reads it: {axis:?}");
        assert_eq!(
            axis.failed_at.as_deref(),
            Some(cell_labels(axis.axis)[0]),
            "it dies at the first cell: {axis:?}"
        );
    }
    assert_eq!(
        (score.value, score.grade),
        (0, qrcode_ai_scanner::Grade::Poor)
    );
}

/// Where a wall-clock cut lands depends on the machine, so the one
/// timing-independent truth is the SHAPE of what ships: under any budget a
/// scan reports either no score — and no score hints — or exactly the
/// unbudgeted judgment, a knee bisection the cut reached excepted. Never a
/// partial composite.
#[test]
fn budget_cuts_never_ship_a_partial_judgment() {
    // 3 px modules: ramps knee, so a bisection is at stake as well
    let bytes = qr_png("partial judgment pin", 3);
    let started = std::time::Instant::now();
    let reference = scan(&bytes, unbudgeted_full());
    let full_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let score = reference.score.as_ref().expect("unbudgeted scans judge");
    assert_honest(score);
    assert!(
        score.axes.iter().any(|a| a.refined_failed_at.is_some()),
        "precondition: a knee to bisect: {:?}",
        score.axes
    );
    let stages: Vec<&str> = reference
        .trace
        .stages
        .iter()
        .map(|s| s.stage.as_str())
        .collect();
    assert_eq!(
        stages,
        ["direct"],
        "precondition: ONE single-attempt stage decodes, so a budgeted scan \
         finds this exact detection or none"
    );

    let ceiling = (full_ms * 3 / 2).clamp(40, 2_000);
    let step = (ceiling / 30).max(1);
    let mut budget = 1;
    while budget <= ceiling {
        let mut config = ScanConfig::full();
        config.budget_ms = Some(budget);
        let report = scan(&bytes, config);
        let at = format!("budget {budget} ms");
        assert!(
            report.detections.is_empty() || report.detections == reference.detections,
            "{at}"
        );
        match &report.score {
            None => assert!(report.hints.is_empty(), "{at}: {:?}", report.hints),
            Some(cut) => {
                assert_eq!(report.detections, reference.detections, "{at}");
                assert_eq!(
                    (cut.value, cut.grade, cut.weights_run),
                    (score.value, score.grade, score.weights_run),
                    "{at}: {:?}",
                    cut.axes
                );
                assert_eq!(report.hints, reference.hints, "{at}");
                assert_eq!(
                    (cut.structural, cut.uec, cut.iso15415),
                    (score.structural, score.uec, score.iso15415),
                    "{at}"
                );
                assert_eq!(cut.axes.len(), score.axes.len(), "{at}");
                for (a, r) in cut.axes.iter().zip(&score.axes) {
                    assert_eq!(
                        (a.axis, a.passed, a.total, &a.failed_at),
                        (r.axis, r.passed, r.total, &r.failed_at),
                        "{at}"
                    );
                    assert!(
                        a.refined_failed_at.is_none() || a.refined_failed_at == r.refined_failed_at,
                        "{at}: {a:?} vs {r:?}"
                    );
                }
            }
        }
        budget += step;
    }
}

/// Dev diagnostic — `cargo nextest run -p qrcode-ai-scanner --run-ignored
/// only --no-capture -E 'test(pdf417_verdict_repeatability)'`. The same
/// unbudgeted Full scan of the PDF417 fixture, repeated in ONE process: a
/// deterministic pipeline prints a single verdict. rxing's PDF417 decoder
/// breaks codeword-confidence ties in `HashMap` iteration order (randomized
/// per map), so the near-knee 256px resolution cell — and the value and grade
/// with it — can differ between two scans of identical bytes. Prints the
/// verdict histogram; asserts only that every scan decodes.
#[test]
#[ignore = "dev diagnostic"]
fn pdf417_verdict_repeatability() {
    const SCANS: usize = 60;
    let bytes = fixture("symbology/pdf417.png");
    let mut verdicts = std::collections::BTreeMap::new();
    for _ in 0..SCANS {
        let report = scan(&bytes, unbudgeted_full());
        let detections: Vec<_> = report
            .detections
            .iter()
            .map(|d| (d.symbology, d.content.text.clone()))
            .collect();
        assert!(!detections.is_empty(), "the fixture decodes every time");
        let verdict = report.score.as_ref().map(|s| {
            let resolution = s.axes.iter().find(|a| a.axis == StressAxis::Resolution);
            (
                s.value,
                s.grade,
                resolution.map(|a| (a.passed, a.failed_at.clone())),
            )
        });
        *verdicts
            .entry(format!("{verdict:?} · {detections:?}"))
            .or_insert(0usize) += 1;
    }
    println!("{SCANS} unbudgeted Full scans of symbology/pdf417.png: {verdicts:#?}");
}
