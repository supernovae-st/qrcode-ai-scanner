//! The budget flag: every report says whether the wall-clock budget cut
//! planned work (`trace.budget_exhausted`, spec/02 § Budget semantics). Where
//! a cut lands depends on the machine, so every assertion here holds for any
//! cut point: an unbounded scan is final; a final report is the unbounded
//! report, wall clock aside; a present score is the unbounded judgment, a
//! knee label excepted, and a missing label or judgment says it was cut.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{ImageInput, ScanConfig, ScanProfile, ScanReport, Scanner};

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

fn png(img: &image::DynamicImage) -> Vec<u8> {
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

fn white_png(side: u32) -> Vec<u8> {
    png(&image::DynamicImage::ImageLuma8(
        image::ImageBuffer::from_pixel(side, side, image::Luma([255u8])),
    ))
}

/// Dark modules on full transparency (the canvas-export class): the Full
/// profile flattens it over white and sweeps its placement envelope.
fn transparent_qr() -> Vec<u8> {
    let code = qrcode::QrCode::with_error_correction_level(
        "https://qrcode-ai.com/flag",
        qrcode::EcLevel::Q,
    )
    .unwrap();
    let luma = code
        .render::<image::Luma<u8>>()
        .module_dimensions(6, 6)
        .build();
    let mut img = image::RgbaImage::new(luma.width(), luma.height());
    for (x, y, px) in luma.enumerate_pixels() {
        let color = if px.0[0] < 128 {
            [0, 0, 0, 255]
        } else {
            [0, 0, 0, 0]
        };
        img.put_pixel(x, y, image::Rgba(color));
    }
    png(&image::DynamicImage::ImageRgba8(img))
}

/// White modules on transparency beside an opaque black plate twice the
/// symbol's area: the visible mass reads dark, so `auto` flattens over white
/// and finds nothing — only the opposite-background walk decodes it.
fn fallback_design() -> Vec<u8> {
    let code = qrcode::QrCode::with_error_correction_level(
        "https://qrcode-ai.com/fallback",
        qrcode::EcLevel::Q,
    )
    .unwrap();
    let luma = code
        .render::<image::Luma<u8>>()
        .module_dimensions(6, 6)
        .build();
    let (w, h) = (luma.width(), luma.height());
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
    png(&image::DynamicImage::ImageRgba8(img))
}

fn scan(bytes: &[u8], config: ScanConfig) -> ScanReport {
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
        .scan(ImageInput::encoded(bytes))
        .unwrap()
}

fn with_budget(profile: ScanProfile, budget_ms: Option<u64>) -> ScanConfig {
    let mut config = profile.config();
    config.budget_ms = budget_ms;
    config
}

/// The report minus its wall clock — the one documented nondeterminism.
fn untimed(mut report: ScanReport) -> ScanReport {
    report.trace.total_ms = 0.0;
    for stage in &mut report.trace.stages {
        stage.ms = 0.0;
    }
    report
}

#[test]
fn every_report_carries_the_flag() {
    let samples = [
        ("clean", fixture("clean/gen_v2_l.png")),
        ("blank", white_png(64)),
        ("transparent", transparent_qr()),
    ];
    for (name, bytes) in &samples {
        for profile in [ScanProfile::Full, ScanProfile::Fast, ScanProfile::Frame] {
            for budget in [None, Some(0), Some(1), Some(60_000)] {
                let report = scan(bytes, with_budget(profile.clone(), budget));
                let json = serde_json::to_value(&report).unwrap();
                assert!(
                    json["trace"]["budget_exhausted"].is_boolean(),
                    "{name} {profile:?} {budget:?}: {}",
                    json["trace"]
                );
            }
        }
    }
}

/// Without a deadline nothing planned can be prevented: every walk, every
/// judgment, every envelope runs whole — across report shapes (a direct
/// decode, a deep-rung decode, an S5 rescue, other symbologies, a scene
/// with no QR, a transparent design and its opposite-background walk).
#[test]
fn unbounded_scans_never_set_the_flag() {
    let samples = [
        ("clean/gen_v2_l.png", fixture("clean/gen_v2_l.png")),
        (
            "artistic/blob-style-monkey-logo.webp",
            fixture("artistic/blob-style-monkey-logo.webp"),
        ),
        (
            "degraded/logo-occluded-rescue.png",
            fixture("degraded/logo-occluded-rescue.png"),
        ),
        ("symbology/ean13.png", fixture("symbology/ean13.png")),
        (
            "symbology/datamatrix.png",
            fixture("symbology/datamatrix.png"),
        ),
        ("blank 64 px", white_png(64)),
        ("transparent design", transparent_qr()),
        ("opposite-background design", fallback_design()),
    ];
    for (name, bytes) in &samples {
        for profile in [ScanProfile::Full, ScanProfile::Fast, ScanProfile::Frame] {
            let report = scan(bytes, with_budget(profile.clone(), None));
            assert_eq!(
                report.trace.budget_exhausted,
                Some(false),
                "{name} {profile:?}: {:?}",
                report.trace
            );
        }
    }
}

/// The budgets of a sweep: from 1 ms up to past the unbounded scan's own
/// wall clock (at least 800 ms), roughly geometric — fine where cuts land
/// in the ladder, coarse where only the score or the envelope is left.
fn budgets(unbounded_ms: u64) -> Vec<u64> {
    let ceiling = (unbounded_ms * 3 / 2).max(800);
    let mut steps = vec![1, 2, 5, 10, 20, 50, 100, 200, 400, 800];
    let mut next = 1_200;
    while next <= ceiling {
        steps.push(next);
        next = next * 3 / 2;
    }
    steps.push(ceiling);
    steps.dedup();
    steps
}

/// One budget sweep of the Full profile over `bytes`, against its unbounded
/// report: every truth of the flag that holds wherever the cut lands.
fn sweep(name: &str, bytes: &[u8]) {
    let started = std::time::Instant::now();
    let reference = scan(bytes, with_budget(ScanProfile::Full, None));
    let unbounded_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    assert_eq!(reference.trace.budget_exhausted, Some(false), "{name}");
    let reference = untimed(reference);
    let (mut whole, mut partial) = (0, 0);
    for budget in budgets(unbounded_ms) {
        let report = untimed(scan(bytes, with_budget(ScanProfile::Full, Some(budget))));
        let at = format!("{name} at {budget} ms");
        let Some(cut) = report.trace.budget_exhausted else {
            panic!("{at}: every report carries the flag");
        };
        if !cut {
            whole += 1;
            assert_eq!(
                report, reference,
                "{at}: a final report is the unbounded one"
            );
            continue;
        }
        partial += 1;
        // partial: what is missing says so, and what is present is whole
        if report.detections != reference.detections {
            // only a cut walk publishes other detections — and the deadline
            // it passed leaves no step to the judgment that follows
            assert!(report.score.is_none(), "{at}: {:?}", report.score);
            continue;
        }
        match (&report.score, &reference.score) {
            (Some(score), Some(whole)) => {
                assert_eq!(
                    (score.value, score.grade, score.weights_run),
                    (whole.value, whole.grade, whole.weights_run),
                    "{at}"
                );
                assert_eq!(
                    (score.structural, score.uec, score.iso15415),
                    (whole.structural, whole.uec, whole.iso15415),
                    "{at}"
                );
                assert_eq!(score.axes.len(), whole.axes.len(), "{at}");
                for (a, r) in score.axes.iter().zip(&whole.axes) {
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
                let score_hints: Vec<_> =
                    report.hints.iter().filter(|h| !is_alpha_hint(h)).collect();
                let whole_hints: Vec<_> = reference
                    .hints
                    .iter()
                    .filter(|h| !is_alpha_hint(h))
                    .collect();
                assert_eq!(score_hints, whole_hints, "{at}");
            }
            (None, _) => {
                // an interrupted judgment ships no score hint
                assert!(
                    report.hints.iter().all(is_alpha_hint),
                    "{at}: {:?}",
                    report.hints
                );
            }
            (Some(_), None) => panic!("{at}: a cut scan judged what the unbounded one did not"),
        }
    }
    // where the cuts landed on this machine (shown with --no-capture)
    println!("{name}: unbounded {unbounded_ms} ms · {whole} final · {partial} partial");
}

fn is_alpha_hint(hint: &qrcode_ai_scanner::Hint) -> bool {
    matches!(
        hint,
        qrcode_ai_scanner::Hint::AlphaBackgroundDependent { .. }
            | qrcode_ai_scanner::Hint::AddBackgroundPlate { .. }
    )
}

/// A present score is the unbounded judgment at every budget — a clean
/// symbol, a deep-rung artistic decode whose ramps knee, and the boost-rung
/// class. (PDF417 is excluded from budget sweeps: its decoder breaks ties in
/// randomized order, 04-score § Determinism.)
#[test]
fn a_present_score_is_budget_independent() {
    for rel in [
        "clean/gen_v5_q.png",
        "artistic/blob-style-monkey-logo.webp",
        "artistic/OK_1069ms_85_8b6a54b3.png",
    ] {
        sweep(rel, &fixture(rel));
    }
}

/// A report that says it is final is the unbounded report: a scene with
/// nothing to find, a transparent design (its placement envelope), and a
/// design only the opposite-background walk decodes.
#[test]
fn flag_false_means_final() {
    sweep("blank 64 px", &white_png(64));
    sweep("transparent design", &transparent_qr());
    sweep("opposite-background design", &fallback_design());
}
