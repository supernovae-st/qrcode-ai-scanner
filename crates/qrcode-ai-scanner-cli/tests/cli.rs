//! Exit-code contract tests — the CLI's headline promise (header of main.rs):
//! 0 = QR found · 1 = no QR found · 2 = invalid input/usage error.
//! Zero-dep harness: cargo exposes the built binary via `CARGO_BIN_EXE`_*.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;

fn qrscan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_qrscan"))
}

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/{rel}"))
}

fn white_png() -> PathBuf {
    let path = std::env::temp_dir().join("qrscan-cli-white.png");
    let img = image::DynamicImage::ImageLuma8(image::ImageBuffer::from_pixel(
        64,
        64,
        image::Luma([255u8]),
    ));
    img.save(&path).unwrap();
    path
}

#[test]
fn found_is_exit_0_with_json() {
    let out = qrscan()
        .arg(fixture("clean/gen_v2_l.png"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["detections"].as_array().unwrap().len(), 1);
}

/// `--budget-ms` must actually thread into the scan: 0 (unbounded) and a
/// generous budget both decode the clean fixture; the flag parsing itself
/// is the contract (a typo'd flag would exit 2 before scanning).
#[test]
fn budget_ms_flag_threads_into_the_scan() {
    for budget in ["0", "60000"] {
        let out = qrscan()
            .arg(fixture("clean/gen_v2_l.png"))
            .args(["--budget-ms", budget])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "--budget-ms {budget}: clean fixture must decode"
        );
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["detections"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn not_found_is_exit_1_in_every_output_mode() {
    let blank = white_png();
    for extra in [&[][..], &["--pretty"][..], &["--score-only"][..]] {
        let out = qrscan().arg(&blank).args(extra).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(1),
            "mode {extra:?}: no QR must be exit 1, not an error"
        );
    }
}

/// Budget-free (`--budget-ms 0`): a score judgment the wall-clock budget
/// cannot complete is absent, and `--score-only` has no value to print —
/// under the default budget a loaded machine would flip this test.
#[test]
fn score_only_prints_bare_value_on_found() {
    let out = qrscan()
        .arg(fixture("clean/gen_v2_l.png"))
        .args(["--score-only", "--budget-ms", "0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let value: u8 = String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(value > 0);
}

#[test]
fn score_only_under_frame_profile_is_usage_error_2() {
    let out = qrscan()
        .arg(fixture("clean/gen_v2_l.png"))
        .args(["--score-only", "--profile", "frame"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "score suppressed by profile = usage error"
    );
}

#[test]
fn unreadable_input_is_exit_2() {
    let out = qrscan().arg("/nonexistent/nope.png").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

// ---- end-to-end wire contract -----------------------------------------------
// The type-parity gate holds the TYPE surfaces identical (rust ↔ ts ↔ schema ↔
// dart); nothing until here validated a real binary's real OUTPUT against the
// schema — the layer where serde attributes live (a wrong skip_serializing,
// a rename drift, absent-vs-null) and where the Dart int/double truncation
// class actually bit. These run the shipped binary end-to-end: encoded image
// in → JSON out → `spec/scan-report.schema.json` verdict. Budget-free
// (`--budget-ms 0`) so a loaded machine can never starve the ladder mid-run.

fn wire_schema() -> jsonschema::Validator {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/scan-report.schema.json");
    let raw = std::fs::read_to_string(&path).unwrap();
    let schema: serde_json::Value = serde_json::from_str(&raw).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn scan_json(rel: &str) -> serde_json::Value {
    let out = qrscan()
        .args(["--budget-ms", "0"])
        .arg(fixture(rel))
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{rel}: stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// One fixture per corpus category — decode shapes (1D GTIN meta, FNC1
/// element strings, artistic deep-rung decodes) exercise DIFFERENT report
/// branches; a schema violation names its exact JSON pointer.
#[test]
fn every_category_report_validates_against_the_wire_schema() {
    let validator = wire_schema();
    for rel in [
        "clean/gen_v2_l.png",                   // direct decode + score
        "degraded/gen_v5_q_tiny.png",           // upscale path
        "degraded/exif6-rotated-qr.jpg",        // EXIF orientation branch
        "artistic/OK_1069ms_85_8b6a54b3.png",   // boost-rung decode class
        "artistic/blob-style-monkey-logo.webp", // morph-rung + webp input
        "symbology/ean13.png",                  // retail GTIN payload branch
        "symbology/datamatrix-gs1.png",         // FNC1 element-string branch
        "symbology/microqr.png",                // micro-QR meta branch
    ] {
        let report = scan_json(rel);
        let errors: Vec<String> = validator
            .iter_errors(&report)
            .map(|e| format!("{} @ {}", e, e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{rel}: report violates the wire schema:\n{}",
            errors.join("\n")
        );
        assert_eq!(
            report["versions"],
            serde_json::json!({"scanner": env!("CARGO_PKG_VERSION"), "pipeline": 2, "score_contract": 4}),
            "{rel}: versions block is the wire's compatibility anchor"
        );
    }
}

/// The NOT-FOUND report is a wire shape too (empty detections · no score) —
/// exactly where absent-vs-null serde drift hides, and never validated
/// before because every schema-shaped test scanned a decodable image.
#[test]
fn not_found_report_also_validates_against_the_wire_schema() {
    let blank = white_png();
    let out = qrscan()
        .args(["--budget-ms", "0"])
        .arg(&blank)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let validator = wire_schema();
    let errors: Vec<String> = validator
        .iter_errors(&report)
        .map(|e| format!("{} @ {}", e, e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "not-found report violates the wire schema:\n{}",
        errors.join("\n")
    );
    assert_eq!(report["detections"].as_array().unwrap().len(), 0);
}

/// Null every wall-clock field (`ms` · `total_ms`) — the ONE documented
/// nondeterminism (lib contract §Deterministic) — recursively, then the
/// remaining tree must be byte-identical across runs THROUGH THE BINARY.
/// The lib pins this in-process; this pins it across process boundaries,
/// where env, locale, allocator and buffering could all have leaked in.
fn null_wall_clock(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if k == "ms" || k == "total_ms" {
                    *val = serde_json::Value::Null;
                } else {
                    null_wall_clock(val);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(null_wall_clock),
        _ => {}
    }
}

#[test]
fn cli_reports_are_deterministic_across_processes_modulo_wall_clock() {
    for rel in ["clean/gen_v2_l.png", "artistic/blob-style-monkey-logo.webp"] {
        let mut a = scan_json(rel);
        let mut b = scan_json(rel);
        null_wall_clock(&mut a);
        null_wall_clock(&mut b);
        assert_eq!(a, b, "{rel}: two binary runs diverged beyond wall-clock");
    }
}

// ---- the content lines of --pretty (spec/01 § Content) -----------------

/// A v3-M symbol from an explicit segment stream, rendered to a temp PNG
/// (6 px modules, 4-module quiet zone) named after the row.
fn symbol_png(
    name: &str,
    push: impl FnOnce(&mut qrcode::bits::Bits) -> qrcode::types::QrResult<()>,
) -> PathBuf {
    let (version, ec) = (qrcode::Version::Normal(3), qrcode::EcLevel::M);
    let mut bits = qrcode::bits::Bits::new(version);
    push(&mut bits).unwrap();
    bits.push_terminator(ec).unwrap();
    let code = qrcode::QrCode::with_bits(bits, ec).unwrap();
    let width = code.width();
    let side = u32::try_from((width + 8) * 6).unwrap();
    let mut img = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    for (i, color) in code.to_colors().iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let mx = u32::try_from(i % width + 4).unwrap();
            let my = u32::try_from(i / width + 4).unwrap();
            for dy in 0..6 {
                for dx in 0..6 {
                    img.put_pixel(mx * 6 + dx, my * 6 + dy, image::Luma([0]));
                }
            }
        }
    }
    let path = std::env::temp_dir().join(format!("qrscan-cli-content-{name}.png"));
    img.save(&path).unwrap();
    path
}

/// The `--pretty` lines of a frame-profile scan (piped stdout: no colour).
fn pretty_lines(path: &PathBuf) -> Vec<String> {
    let out = qrscan()
        .args(["--pretty", "--profile", "frame", "--budget-ms", "0"])
        .arg(path)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[track_caller]
fn assert_line(lines: &[String], expected: &str) {
    assert!(
        lines.iter().any(|l| l == expected),
        "want {expected:?} in {lines:#?}"
    );
}

#[test]
fn pretty_names_the_charset_and_the_declarations() {
    // ECI 3 [35 80] — the C1 control escaped, the declared reading named
    // with the designator and where it takes effect
    let lines = pretty_lines(&symbol_png("eci3", |b| {
        b.push_eci_designator(3)?;
        b.push_byte_data(&[0x35, 0x80])
    }));
    assert_line(&lines, "content   5\\u{80}");
    assert_line(&lines, "charset   eci · ECI 3@0");
    // ECI 999 [A1] — no reading: the replacement glyph stays a glyph, and
    // the line says the text is not verified
    let lines = pretty_lines(&symbol_png("eci999", |b| {
        b.push_eci_designator(999)?;
        b.push_byte_data(&[0xA1])
    }));
    assert_line(&lines, "content   \u{FFFD}");
    assert_line(
        &lines,
        "charset   unknown · ECI 999@0 · no verified text (raw: --json)",
    );
    // FNC1 in second position
    let lines = pretty_lines(&symbol_png("fnc1-second", |b| {
        b.push_fnc1_second_position(37)?;
        b.push_byte_data(b"abc")
    }));
    assert_line(&lines, "content   37abc");
    assert_line(&lines, "fnc1      second");
}

#[test]
fn pretty_escapes_what_the_data_encodes() {
    // a U+FFFD the data encodes is a character — escaped, so it never reads
    // as an unreadable byte; utf8 without a declaration has no charset line
    let lines = pretty_lines(&symbol_png("encoded-fffd", |b| {
        b.push_byte_data(&[0x41, 0xEF, 0xBF, 0xBD, 0x42])
    }));
    assert_line(&lines, "content   A\\u{fffd}B");
    assert!(
        !lines.iter().any(|l| l.starts_with("charset")),
        "{lines:#?}"
    );
    let lines = pretty_lines(&symbol_png("gb18030-fffd", |b| {
        b.push_eci_designator(32)?;
        b.push_byte_data(&[0x41, 0x84, 0x31, 0xA4, 0x37, 0x42])
    }));
    assert_line(&lines, "content   A\\u{fffd}B");
    assert_line(&lines, "charset   gb18030 · ECI 32@0");
    // a backslash doubles; a bidi override is escaped, never applied
    let lines = pretty_lines(&symbol_png("backslash", |b| b.push_byte_data(b"a\\b")));
    assert_line(&lines, "content   a\\\\b");
    let lines = pretty_lines(&symbol_png("rlo", |b| {
        b.push_byte_data("\u{202E}abc".as_bytes())
    }));
    assert_line(&lines, "content   \\u{202e}abc");
}

/// --score-skip-axes threads to the engine (axes absent from the wire) and a
/// typo'd axis is a loud exit-2, never a silent six-axis score.
#[test]
fn score_skip_axes_flag_threads_and_rejects_typos() {
    let out = qrscan()
        .args([
            "--budget-ms",
            "0",
            "--score-skip-axes",
            "perspective,rotation",
        ])
        .arg(fixture("clean/gen_v2_l.png"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let axes = report["score"]["axes"].as_array().unwrap();
    assert_eq!(axes.len(), 4, "six axes minus the two skipped: {axes:?}");
    assert!(axes.iter().all(|a| {
        let name = a["axis"].as_str().unwrap();
        name != "perspective" && name != "rotation"
    }));

    let out = qrscan()
        .args(["--score-skip-axes", "perspektive"])
        .arg(fixture("clean/gen_v2_l.png"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "typo'd axis = usage error");
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown stress axis"));
}

/// --score-skip-checks nulls the skipped sections on the wire (composite
/// untouched) and a typo'd check is a loud exit-2.
#[test]
fn score_skip_checks_flag_threads_and_rejects_typos() {
    let out = qrscan()
        .args(["--budget-ms", "0", "--score-skip-checks", "uec,iso15415"])
        .arg(fixture("clean/gen_v2_l.png"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(report["score"]["uec"].is_null(), "skipped uec must be null");
    assert!(
        report["score"]["iso15415"].is_null(),
        "skipped iso15415 must be null"
    );
    assert!(
        report["score"]["value"].is_number(),
        "composite still scores"
    );

    let out = qrscan()
        .args(["--score-skip-checks", "margin"])
        .arg(fixture("clean/gen_v2_l.png"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "typo'd check = usage error");
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown score check"));
}
