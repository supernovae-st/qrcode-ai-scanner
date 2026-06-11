//! End-to-end Scanner tests — legacy corpus images + the public contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{CancelToken, Grade, ImageInput, Payload, ScanProfile, Scanner};

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../test-images/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

fn white_png(side: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageLuma8(image::ImageBuffer::from_pixel(
        side,
        side,
        image::Luma([255u8]),
    ));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

fn generated_qr_png(content: &str) -> Vec<u8> {
    let code = qrcode::QrCode::with_error_correction_level(content, qrcode::EcLevel::Q).unwrap();
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

#[test]
fn generated_url_qr_end_to_end() {
    let bytes = generated_qr_png("https://qrcode-ai.com/scan");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();

    assert_eq!(report.detections.len(), 1);
    let d = &report.detections[0];
    assert_eq!(d.content.text, "https://qrcode-ai.com/scan");
    assert_eq!(
        d.payload,
        Payload::Url {
            url: "https://qrcode-ai.com/scan".into()
        }
    );
    assert!(!d.engines.is_empty());
    // rqrr supplies geometry on clean codes — corners + version + modules.
    assert!(d.corners.is_some());
    let version = d.meta.version.expect("version measured");
    assert_eq!(d.meta.modules, Some(version * 4 + 17));
    assert_eq!(report.versions.score_contract, 3);
}

#[test]
fn legacy_clean_image_decodes_in_early_stages() {
    let bytes = fixture("clean/OK_68ms_100_4e875a2c.png");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert_eq!(report.detections.len(), 1, "trace: {:?}", report.trace);
    assert!(!report.detections[0].content.text.is_empty());
}

#[test]
fn legacy_artistic_image_decodes_within_full_ladder() {
    let bytes = fixture("artistic/OK_1069ms_85_8b6a54b3.png");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert_eq!(
        report.detections.len(),
        1,
        "artistic regression vs v0.2 — trace: {:?}",
        report.trace
    );
}

#[test]
fn legacy_degraded_image_is_ok_not_err() {
    let bytes = fixture("degraded/FAIL_1491ms_0_584c998c.png");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    // v0.2 failed this one; whatever the engines manage, the semantic
    // contract is: no panic, no Err — a report.
    assert!(report.trace.total_ms > 0.0);
}

#[test]
fn no_qr_is_ok_with_empty_detections() {
    let bytes = white_png(64);
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert!(report.detections.is_empty());
    assert!(report.score.is_none());
    assert!(!report.trace.stages.is_empty(), "ladder must have run");
}

#[test]
fn determinism_modulo_trace_timing() {
    let bytes = generated_qr_png("determinism pin");
    let scanner = Scanner::default();
    let a = scanner.scan(ImageInput::encoded(&bytes)).unwrap();
    let b = scanner.scan(ImageInput::encoded(&bytes)).unwrap();

    assert_eq!(a.detections, b.detections);
    assert_eq!(a.hints, b.hints);
    assert_eq!(a.score, b.score);
    assert_eq!(a.versions, b.versions);
    // Trace is deterministic in structure; only wall-clock fields vary.
    assert_eq!(a.trace.stages.len(), b.trace.stages.len());
    for (sa, sb) in a.trace.stages.iter().zip(&b.trace.stages) {
        assert_eq!(sa.stage, sb.stage);
        assert_eq!(sa.transforms_tried, sb.transforms_tried);
        assert_eq!(sa.detections_found, sb.detections_found);
    }
}

#[test]
fn precancelled_scan_is_qrs005() {
    let bytes = generated_qr_png("cancel pin");
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = Scanner::default()
        .scan_cancellable(ImageInput::encoded(&bytes), &cancel)
        .unwrap_err();
    assert_eq!(err.code(), "QRS-005");
}

#[test]
fn batch_matches_individual_scans() {
    let qr = generated_qr_png("batch pin");
    let blank = white_png(48);
    let scanner = Scanner::default();

    let inputs = [ImageInput::encoded(&qr), ImageInput::encoded(&blank)];
    let batch = scanner.scan_batch(&inputs);
    assert_eq!(batch.len(), 2);

    let solo_qr = scanner.scan(ImageInput::encoded(&qr)).unwrap();
    let batch_qr = batch[0].as_ref().unwrap();
    assert_eq!(solo_qr.detections, batch_qr.detections);
    assert!(batch[1].as_ref().unwrap().detections.is_empty());
}

#[test]
fn frame_profile_skips_scoring_and_respects_shape() {
    let bytes = generated_qr_png("frame profile pin");
    let scanner = Scanner::builder().profile(ScanProfile::Frame).build();
    let report = scanner.scan(ImageInput::encoded(&bytes)).unwrap();
    assert!(report.score.is_none());
    assert_eq!(report.detections.len(), 1);
}

#[test]
fn grade_surface_is_reexported() {
    // Compile-time pin that the score vocabulary ships from the crate root.
    assert_eq!(Grade::from_value(85), Grade::Excellent);
}

// ---- score contract v3 (task A8) ----

#[test]
fn clean_qr_scores_high_with_full_breakdown() {
    let bytes = generated_qr_png("https://qrcode-ai.com/score-pin");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let score = report.score.expect("Full profile scores");

    assert!(score.value >= 70, "clean generated QR: {score:?}");
    assert_eq!(score.axes.len(), 6, "all six axes present");
    for axis in &score.axes {
        assert_eq!(axis.total, 5, "Full depth = 5 cells: {axis:?}");
    }
    let structural = score.structural.expect("geometry came from rqrr");
    for integrity in structural.finder_integrity {
        assert!(integrity > 0.85, "intact finders: {structural:?}");
    }
    assert!(structural.quiet_zone_ok);
}

#[test]
fn degraded_copy_never_scores_higher_and_a_knee_crossing_scores_less() {
    // Thin margins on purpose: 3px/module, then a blur that eats real cells.
    // (At 6px/module a σ=1.6 blur crosses NO knee — both tie at the same
    // survival cells; the 5-cell scale is coarse by design.)
    let code = qrcode::QrCode::with_error_correction_level(
        "monotonicity pin".as_bytes(),
        qrcode::EcLevel::Q,
    )
    .unwrap();
    let img = code
        .render::<image::Luma<u8>>()
        .module_dimensions(3, 3)
        .build();
    let mut clean = Vec::new();
    image::DynamicImage::ImageLuma8(img.clone())
        .write_to(
            &mut std::io::Cursor::new(&mut clean),
            image::ImageFormat::Png,
        )
        .unwrap();
    let blurred_img = image::DynamicImage::ImageLuma8(image::imageops::blur(&img, 1.2));
    let mut blurred = Vec::new();
    blurred_img
        .write_to(
            &mut std::io::Cursor::new(&mut blurred),
            image::ImageFormat::Png,
        )
        .unwrap();

    let scanner = Scanner::default();
    let score_clean = scanner
        .scan(ImageInput::encoded(&clean))
        .unwrap()
        .score
        .expect("clean scores");
    let report_blurred = scanner.scan(ImageInput::encoded(&blurred)).unwrap();

    match report_blurred.score {
        Some(score_blurred) => {
            assert!(
                score_blurred.value <= score_clean.value,
                "degradation must never raise the score: blurred {} > clean {}",
                score_blurred.value,
                score_clean.value
            );
            let cells = |s: &qrcode_ai_scanner::Score| -> u32 {
                s.axes.iter().map(|a| u32::from(a.passed)).sum()
            };
            assert!(
                cells(&score_blurred) < cells(&score_clean),
                "thin-margin blur must cross ≥1 knee: blurred {:?} vs clean {:?}",
                score_blurred.axes,
                score_clean.axes
            );
        }
        None => {
            assert!(
                report_blurred.detections.is_empty(),
                "no score implies no detection"
            );
        }
    }
}

#[test]
fn fast_profile_runs_reduced_ramps() {
    let bytes = generated_qr_png("fast depth pin");
    let scanner = Scanner::builder().profile(ScanProfile::Fast).build();
    let report = scanner.scan(ImageInput::encoded(&bytes)).unwrap();
    let score = report.score.expect("Fast profile still scores");
    for axis in &score.axes {
        assert_eq!(axis.total, 2, "Reduced depth = 2 cells: {axis:?}");
    }
}

#[test]
fn score_is_deterministic() {
    let bytes = generated_qr_png("score determinism pin");
    let scanner = Scanner::default();
    let a = scanner.scan(ImageInput::encoded(&bytes)).unwrap().score;
    let b = scanner.scan(ImageInput::encoded(&bytes)).unwrap().score;
    assert_eq!(a, b);
}
