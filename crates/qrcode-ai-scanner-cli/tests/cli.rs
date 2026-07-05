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

#[test]
fn score_only_prints_bare_value_on_found() {
    let out = qrscan()
        .arg(fixture("clean/gen_v2_l.png"))
        .arg("--score-only")
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
