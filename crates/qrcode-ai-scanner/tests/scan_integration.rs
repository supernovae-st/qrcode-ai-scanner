//! End-to-end Scanner tests — legacy corpus images + the public contract.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss
)]

use qrcode_ai_scanner::{CancelToken, Grade, ImageInput, Payload, ScanProfile, Scanner};

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
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

#[test]
fn uec_margin_ships_in_the_report() {
    let bytes = generated_qr_png("uec e2e pin");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let score = report.score.expect("Full profile scores");
    let uec = score.uec.expect("rqrr stream → synthetic UEC");
    assert_eq!(uec.margin, 1.0, "pristine generated code: {uec:?}");
    assert_eq!(uec.worst_block_errors, 0);
}

// ---- security/correctness round 2 ----

#[test]
fn corners_are_in_original_image_coordinates_even_when_decoded_downscaled() {
    // 1400px image: S1 pyramid (512) decodes it — corners MUST come back
    // in 1400-space, not 512-space.
    let code = qrcode::QrCode::with_error_correction_level(
        "corner space pin".as_bytes(),
        qrcode::EcLevel::Q,
    )
    .unwrap();
    let small = code
        .render::<image::Luma<u8>>()
        .module_dimensions(8, 8)
        .build();
    // paste the QR centered on a large white canvas
    let mut big = image::GrayImage::from_pixel(1400, 1400, image::Luma([255u8]));
    let (off_x, off_y) = (500u32, 450u32);
    image::imageops::overlay(&mut big, &small, i64::from(off_x), i64::from(off_y));
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(big)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();

    let report = Scanner::default().scan(ImageInput::encoded(&png)).unwrap();
    assert_eq!(report.detections.len(), 1, "trace: {:?}", report.trace);
    let corners = report.detections[0].corners.expect("rqrr corners");
    let (qr_w, qr_h) = (small.width() as f32, small.height() as f32);
    for p in corners {
        assert!(
            p.x >= off_x as f32 - qr_w * 0.2 && p.x <= off_x as f32 + qr_w * 1.2,
            "corner x {} outside the symbol's original-space region (offset {off_x}, size {qr_w}) — corners are in downscaled space",
            p.x
        );
        assert!(
            p.y >= off_y as f32 - qr_h * 0.2 && p.y <= off_y as f32 + qr_h * 1.2,
            "corner y {} outside original-space region",
            p.y
        );
    }
}

#[test]
fn artistic_image_that_decodes_must_not_score_zero() {
    // The legacy artistic image decodes ONLY via the boost rungs. Stress
    // cells probing direct+otsu alone fail even UNSTRESSED → score 0 for a
    // decodable symbol (margin of nothing measured). Cells must probe the
    // decode class that the baseline itself needs.
    let bytes = fixture("artistic/OK_1069ms_85_8b6a54b3.png");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert_eq!(report.detections.len(), 1);
    let score = report.score.expect("Full profile scores");
    assert!(
        score.value > 0,
        "decodable artistic symbol scored zero margin: {:?}",
        score.axes
    );
}

#[test]
fn blob_style_template_decodes_and_scores() {
    // qrcode-ai.com production template: blob pixel style + center logo.
    // Only a morphological close (dark-dilate) + downscale decodes it —
    // the rung must exist in BOTH the ladder and the score probe (a decode
    // that scores 0/100 is the round-2 regression class).
    let bytes = fixture("artistic/blob-style-monkey-logo.webp");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert_eq!(report.detections.len(), 1, "trace: {:?}", report.trace);
    assert_eq!(
        report.detections[0].content.text,
        "https://qrc-ai.com/Uo54C"
    );
    let score = report.score.expect("Full profile scores");
    assert!(score.value > 0, "decodable symbol must not score 0");
}

#[test]
fn gs1_fnc1_qr_end_to_end_yields_conformant_gs1_payload() {
    // a real FNC1-in-first-position symbol: (01) GTIN + (10) batch + (17) expiry
    let mut bits = qrcode::bits::Bits::new(qrcode::Version::Normal(3));
    bits.push_fnc1_first_position().unwrap();
    bits.push_byte_data(b"010950600013435210ABC123\x1d17261231")
        .unwrap();
    bits.push_terminator(qrcode::EcLevel::M).unwrap();
    let code = qrcode::QrCode::with_bits(bits, qrcode::EcLevel::M).unwrap();
    let img = code
        .render::<image::Luma<u8>>()
        .module_dimensions(6, 6)
        .build();
    let mut buf = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();

    let report = Scanner::default().scan(ImageInput::encoded(&buf)).unwrap();
    assert_eq!(report.detections.len(), 1, "trace: {:?}", report.trace);
    let d = &report.detections[0];
    match &d.payload {
        Payload::Gs1 {
            elements,
            gtin,
            conformant,
            issues,
        } => {
            assert!(conformant, "issues: {issues:?}");
            assert_eq!(gtin.as_deref(), Some("09506000134352"));
            let ais: Vec<&str> = elements.iter().map(|e| e.ai.as_str()).collect();
            assert_eq!(ais, vec!["01", "10", "17"]);
            assert_eq!(elements[1].value, "ABC123");
        }
        other => panic!("expected Gs1 payload, got {other:?}"),
    }
}

#[test]
fn gs1_digital_link_qr_end_to_end() {
    // Sunrise-2027 retail form: plain QR carrying a Digital Link URI
    let bytes = generated_qr_png("https://id.gs1.org/01/09506000134352/10/LOT1?17=271231");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert_eq!(report.detections.len(), 1);
    match &report.detections[0].payload {
        Payload::Gs1DigitalLink {
            url,
            gtin,
            conformant,
            issues,
            ..
        } => {
            assert!(conformant, "issues: {issues:?}");
            assert!(url.starts_with("https://id.gs1.org/01/"));
            assert_eq!(gtin.as_deref(), Some("09506000134352"));
        }
        other => panic!("expected Gs1DigitalLink payload, got {other:?}"),
    }
}

#[test]
fn iso15415_grade_card_on_a_clean_symbol() {
    let bytes = generated_qr_png("https://qrcode-ai.com/iso-pin");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let score = report.score.expect("Full profile scores");
    let iso = score
        .iso15415
        .expect("rqrr geometry present on clean codes");

    // pristine black/white render: contrast + modulation + ANU are A-grade
    assert_eq!(
        iso.symbol_contrast.grade,
        qrcode_ai_scanner::IsoGrade::A,
        "{iso:?}"
    );
    assert_eq!(
        iso.axial_nonuniformity.grade,
        qrcode_ai_scanner::IsoGrade::A,
        "{iso:?}"
    );
    assert!(iso.symbol_contrast.value > 0.9, "{iso:?}");
    assert!(iso.axial_nonuniformity.value < 0.02, "{iso:?}");
    // UEC param mirrors the synthetic UEC report
    let uec = score.uec.expect("bitstream present");
    let uec_param = iso.unused_error_correction.expect("mirrored");
    assert!((uec_param.value - uec.margin).abs() < f32::EPSILON);
    // the ISO rule: overall is never better than any parameter
    for grade in [
        iso.symbol_contrast.grade,
        iso.modulation.grade,
        iso.axial_nonuniformity.grade,
        iso.fixed_pattern_damage.grade,
        uec_param.grade,
    ] {
        assert!(iso.overall >= grade, "overall must be the worst: {iso:?}");
    }
}

#[test]
fn scanner_is_send_sync_and_report_is_send() {
    // compile-time pins for the documented threading contract
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Scanner>();
    assert_send_sync::<qrcode_ai_scanner::ScanReport>();
    assert_send_sync::<CancelToken>();
}

#[test]
fn inverted_symbol_structural_checks_read_true_polarity() {
    // light-on-dark symbol: invert a clean render at the IMAGE level
    let code =
        qrcode::QrCode::with_error_correction_level("inverted pin", qrcode::EcLevel::Q).unwrap();
    let img = code
        .render::<image::Luma<u8>>()
        .module_dimensions(6, 6)
        .build();
    let inverted = image::ImageBuffer::from_fn(img.width(), img.height(), |x, y| {
        image::Luma([255 - img.get_pixel(x, y).0[0]])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageLuma8(inverted)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

    // skip S1/S2 (rxing AlsoInverted would win there with no geometry):
    // force the S3 path where rqrr decodes on the INVERT attempt and
    // becomes the geometry source.
    let mut config = qrcode_ai_scanner::ScanConfig::full();
    config.pyramid = false;
    config.direct = false;
    let scanner = Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build();
    let report = scanner.scan(ImageInput::encoded(&bytes)).unwrap();

    assert_eq!(report.detections.len(), 1, "trace: {:?}", report.trace);
    let d = &report.detections[0];
    assert_eq!(d.content.text, "inverted pin");
    assert_eq!(d.meta.inverted, Some(true), "measured polarity");
    assert!(d.corners.is_some(), "rqrr geometry present");

    let score = report.score.as_ref().expect("Full depth scores");
    let structural = score.structural.expect("geometry → structural checks");
    // pre-fix: polarity-blind sampling read finder integrity ≈ 0.33 and
    // capped a CLEAN symbol at 40 with bogus finder/quiet-zone hints
    for integrity in structural.finder_integrity {
        assert!(integrity > 0.85, "clean finders: {structural:?}");
    }
    assert!(structural.quiet_zone_ok, "{structural:?}");
    assert!(
        !report
            .hints
            .iter()
            .any(|h| matches!(h, qrcode_ai_scanner::Hint::FixFinderPattern { .. })),
        "no bogus finder hint: {:?}",
        report.hints
    );
    let iso = score.iso15415.expect("geometry → grade card");
    assert!(
        iso.fixed_pattern_damage.grade < qrcode_ai_scanner::IsoGrade::C,
        "FPD must not read damaged: {iso:?}"
    );
}

#[test]
fn logo_occluded_symbol_rescued_by_erasure_decoding() {
    // v5-H with a 22% mid-gray logo disk: both engines fail (RS error count
    // past t_max) but the S5 rescue recovers via errors-and-erasures —
    // low-confidence (mid-gray) codewords become half-price erasures.
    let bytes = fixture("degraded/logo-occluded-rescue.png");
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    assert_eq!(report.detections.len(), 1, "trace: {:?}", report.trace);
    let d = &report.detections[0];
    assert_eq!(d.content.text, "https://qrcode-ai.com/rescue-pin");
    assert_eq!(
        d.engines,
        vec![qrcode_ai_scanner::EngineKind::Rescue],
        "the rescue stage is the decode source"
    );
    assert!(d.meta.version.is_some() && d.corners.is_some());
    // rescue ran as a stage in the trace
    assert!(
        report.trace.stages.iter().any(|s| s.stage == "rescue"),
        "{:?}",
        report.trace.stages
    );
}
