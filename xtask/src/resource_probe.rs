//! One deterministic resource-regression scan per process. Run ONLY under an
//! external wall/RSS/CPU guard that owns and cleans up the process group.
//! Library budgets are cooperative: this command cannot interrupt an in-flight
//! decoder. The flushed `start` record is not proof that a scan completed.
//!
//! `xtask resource-probe --mode full|fast|frame|design` uses library defaults;
//! design changes only Full's axes via `ScorePreset::Design`. No budget override,
//! grid scan, input file, baseline-symbol decode or score-off substitution.
//! JSONL stdout holds input identity/configuration then the complete report and
//! exact accounting. Keep receipts private. Build identity and resource usage
//! belong to the external guard, not to a runtime checkout's Git HEAD.
//!
//! Exit 0: returned report without wrong outputs, panics or unexplained score
//! absence (a budget-cut report still is partial, not a completed judgment).
//! Exit 1: scan error, panic or semantic failure. Exit 2: bad invocation/input
//! identity or receipt I/O failure. A killed process has no completion record.

use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use qrcode_ai_scanner::{
    Detection, ImageInput, ScanConfig, ScanProfile, ScanReport, Scanner, ScorePreset, Symbology,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const LABEL: &str = "2067_v10-h-p100-square-dark-on_finder-o20";
const PNG_SHA256: &str = "857f53e2cb67d50f8f252a2f402fc37d01bd5202cfe7535bcfc853631438bbae";
// Row-major L8 bytes, independently read from the retained original PNG.
const LUMA_SHA256: &str = "6a6eaa8c9d88cfdbfb730fbca70d38021c3af0903371de3de2228d5eebd1b47b";
const USAGE: &str = "usage: xtask resource-probe --mode full|fast|frame|design";
static PANICS: AtomicUsize = AtomicUsize::new(0);

fn config(args: &[String]) -> Result<ScanConfig, &'static str> {
    match args {
        [flag, mode] if flag == "--mode" => match mode.as_str() {
            "full" => Ok(ScanProfile::Full.config()),
            "fast" => Ok(ScanProfile::Fast.config()),
            "frame" => Ok(ScanProfile::Frame.config()),
            "design" => {
                let mut config = ScanProfile::Full.config();
                config.score_skip_axes = ScorePreset::Design.skips();
                Ok(config)
            }
            _ => Err(USAGE),
        },
        _ => Err(USAGE),
    }
}

fn exact(detection: &Detection, truth: &str) -> bool {
    detection.symbology == Symbology::QrCode
        && !detection.content.charset.is_lossy()
        && detection.content.raw == truth.as_bytes()
        && detection.content.text == truth
        && detection.content.eci.is_none()
        && detection.content.fnc1.is_none()
}

fn account(report: &ScanReport, truth: &str, computes_score: bool) -> Value {
    let matching = report.detections.iter().filter(|d| exact(d, truth)).count();
    // The producer writes ONE symbol: a repeated truth is still an extra output.
    let truth_outputs = matching.min(1);
    let non_qr = report
        .detections
        .iter()
        .filter(|d| !d.symbology.is_qr_family())
        .count();
    let score_state = if report.score.is_some() {
        "present"
    } else if report.detections.is_empty() {
        "no_detection"
    } else if !computes_score {
        "disabled"
    } else if report.trace.budget_exhausted == Some(true) {
        "interrupted"
    } else {
        "unexplained_absence"
    };
    json!({
        "total_outputs": report.detections.len(),
        "truth_outputs": truth_outputs,
        "wrong_outputs": report.detections.len() - truth_outputs,
        "non_qr_outputs": non_qr,
        "qr_wrong_or_extra_outputs": report.detections.len() - truth_outputs - non_qr,
        "strict_correct": report.detections.len() == 1 && truth_outputs == 1,
        "score_null": report.score.is_none(),
        "score_state": score_state,
        "budget_exhausted": report.trace.budget_exhausted,
        "engine_panics": report.trace.engine_panics,
    })
}

fn emit(record: &Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, record)?;
    writeln!(stdout)?;
    stdout.flush()
}

/// Execute one selected mode; the caller owns any external resource guard.
pub(crate) fn run(args: &[String]) {
    let code = execute(args).unwrap_or_else(|error| {
        eprintln!("resource-probe: {error}");
        2
    });
    std::process::exit(code);
}

fn execute(args: &[String]) -> Result<i32, String> {
    let config = config(args).map_err(str::to_owned)?;
    let (label, truth, png) = crate::rescue_stress::resource_case();
    let sha256 = format!("{:x}", Sha256::digest(&png));
    let image = image::load_from_memory(&png).map_err(|e| e.to_string())?;
    let luma = image.as_luma8().ok_or("generated input is not L8")?;
    let luma_sha256 = format!("{:x}", Sha256::digest(luma.as_raw()));
    let input_verified = label == LABEL && sha256 == PNG_SHA256 && luma_sha256 == LUMA_SHA256;
    emit(&json!({
        "event": "start",
        "cell": 2067,
        "label": label,
        "png_sha256": sha256,
        "png_bytes": png.len(),
        "luma_sha256": luma_sha256,
        "width": luma.width(),
        "height": luma.height(),
        "input_verified": input_verified,
        "truth_bytes": truth.len(),
        "truth_sha256": format!("{:x}", Sha256::digest(truth.as_bytes())),
        "mode": args[1],
        "budget_ms": config.budget_ms,
        "score_depth": format!("{:?}", config.score_depth),
        "score_skip_axes": config.score_skip_axes,
        "max_engine_side": config.max_engine_side,
    }))
    .map_err(|e| e.to_string())?;
    if !input_verified {
        return Err("input identity mismatch; scan not attempted (see start record)".into());
    }

    let computes_score = config.computes_score();
    let scanner = Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::Relaxed);
        previous(info);
    }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scanner.scan(ImageInput::encoded(&png))
    }));
    let caught_panics = PANICS.load(Ordering::Relaxed);
    let (report, accounting, error, scan_errors, scan_panics, failed) = match result {
        Ok(Ok(report)) => {
            let accounting = account(&report, &truth, computes_score);
            let failed = accounting["wrong_outputs"] != 0
                || accounting["score_state"] == "unexplained_absence"
                || report.trace.engine_panics != 0;
            (Some(report), accounting, Value::Null, 0, 0, failed)
        }
        Ok(Err(error)) => (
            None,
            Value::Null,
            json!({"code": error.code(), "message": error.to_string()}),
            1,
            0,
            true,
        ),
        Err(_) => (None, Value::Null, json!({"kind": "scan_panic"}), 0, 1, true),
    };
    let failed = failed || caught_panics != 0;
    emit(&json!({
        "event": "complete",
        "scans_attempted": 1,
        "reports_returned": usize::from(report.is_some()),
        "scan_errors": scan_errors,
        "scan_panics": scan_panics,
        "caught_panics": caught_panics,
        "accounting": accounting,
        "report": report,
        "error": error,
        "semantic_failure": failed,
    }))
    .map_err(|e| e.to_string())?;
    Ok(i32::from(failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_cell_identity_is_pinned_without_a_scan() {
        let (label, truth, png) = crate::rescue_stress::resource_case();
        assert_eq!(label, LABEL);
        assert_eq!(truth.len(), 130);
        assert_eq!(png.len(), 5899);
        assert_eq!(format!("{:x}", Sha256::digest(&png)), PNG_SHA256);
        let image = image::load_from_memory(&png).unwrap();
        assert_eq!((image.width(), image.height()), (552, 552));
        assert_eq!(image.color(), image::ColorType::L8);
        assert_eq!(
            format!("{:x}", Sha256::digest(image.as_luma8().unwrap().as_raw())),
            LUMA_SHA256
        );
    }

    #[test]
    fn only_explicit_default_modes_are_accepted() {
        for args in [
            vec![],
            vec!["--mode", "all"],
            vec!["--mode", "full", "--budget-ms", "0"],
        ] {
            assert_eq!(
                config(&args.into_iter().map(String::from).collect::<Vec<_>>()),
                Err(USAGE)
            );
        }
        for (mode, budget, depth, skipped) in [
            ("full", 4000, qrcode_ai_scanner::ScoreDepth::Full, 0),
            ("fast", 800, qrcode_ai_scanner::ScoreDepth::Reduced, 0),
            ("frame", 80, qrcode_ai_scanner::ScoreDepth::Off, 0),
            ("design", 4000, qrcode_ai_scanner::ScoreDepth::Full, 2),
        ] {
            let config = config(&["--mode".into(), mode.into()]).unwrap();
            assert_eq!(config.budget_ms, Some(budget));
            assert_eq!(config.score_depth, depth);
            assert_eq!(config.score_skip_axes.len(), skipped);
            if mode == "design" {
                assert_eq!(
                    config.score_skip_axes,
                    vec![
                        qrcode_ai_scanner::StressAxis::Perspective,
                        qrcode_ai_scanner::StressAxis::Rotation
                    ]
                );
            }
        }
    }

    #[test]
    fn accounting_checks_every_output_and_original_bytes() {
        let mut report: ScanReport =
            serde_json::from_str(include_str!("../../spec/examples/clean-url.json")).unwrap();
        let detection = report.detections[0].clone();
        let truth = detection.content.text.clone();
        assert_eq!(account(&report, &truth, true)["strict_correct"], true);
        let mut wrong = detection.clone();
        wrong.content.raw[0] ^= 1; // Same displayed text must not conceal different bytes.
        report.detections.push(wrong);
        let counts = account(&report, &truth, true);
        assert_eq!(counts["total_outputs"], 2);
        assert_eq!(counts["truth_outputs"], 1);
        assert_eq!(counts["wrong_outputs"], 1);
        assert_eq!(counts["qr_wrong_or_extra_outputs"], 1);
        assert_eq!(counts["strict_correct"], false);
        report.detections[1] = detection.clone();
        assert_eq!(account(&report, &truth, true)["wrong_outputs"], 1);
        report.detections[1].symbology = Symbology::DataBar;
        assert_eq!(account(&report, &truth, true)["non_qr_outputs"], 1);
        report.detections = vec![detection];
        report.detections[0].content.fnc1 = Some(qrcode_ai_scanner::Fnc1Position::First);
        assert_eq!(account(&report, &truth, true)["wrong_outputs"], 1);
        report.detections[0].content.fnc1 = None;
        report.detections[0].content.eci = Some(Vec::new());
        assert_eq!(account(&report, &truth, true)["wrong_outputs"], 1);
    }

    #[test]
    fn unknown_charset_is_not_an_exact_output_even_when_text_matches() {
        let mut report: ScanReport =
            serde_json::from_str(include_str!("../../spec/examples/clean-url.json")).unwrap();
        let truth = report.detections[0].content.text.clone();
        for (charset, truth_outputs, wrong_outputs, strict_correct) in [
            (qrcode_ai_scanner::Charset::Utf8, 1, 0, true),
            (qrcode_ai_scanner::Charset::Unknown, 0, 1, false),
        ] {
            // Change only the declaration: equal bytes/text cannot make a
            // lossy reading exact (the owning oracle calls it Unread).
            report.detections[0].content.charset = charset;
            let counts = account(&report, &truth, true);
            assert_eq!(counts["total_outputs"], 1);
            assert_eq!(counts["truth_outputs"], truth_outputs);
            assert_eq!(counts["wrong_outputs"], wrong_outputs);
            assert_eq!(counts["qr_wrong_or_extra_outputs"], wrong_outputs);
            assert_eq!(counts["non_qr_outputs"], 0);
            assert_eq!(counts["strict_correct"], strict_correct);
        }
    }

    #[test]
    fn absent_score_retains_budget_and_panic_evidence() {
        let mut report: ScanReport =
            serde_json::from_str(include_str!("../../spec/examples/clean-url.json")).unwrap();
        let truth = report.detections[0].content.text.clone();
        assert_eq!(account(&report, &truth, true)["score_state"], "present");
        report.score = None;
        report.trace.budget_exhausted = Some(true);
        report.trace.engine_panics = 2;
        let counts = account(&report, &truth, true);
        assert_eq!(counts["score_state"], "interrupted");
        assert_eq!(counts["score_null"], true);
        assert_eq!(counts["budget_exhausted"], true);
        assert_eq!(counts["engine_panics"], 2);
        report.trace.budget_exhausted = Some(false);
        assert_eq!(
            account(&report, &truth, true)["score_state"],
            "unexplained_absence"
        );
        assert_eq!(account(&report, &truth, false)["score_state"], "disabled");
        report.detections.clear();
        let counts = account(&report, &truth, true);
        assert_eq!(counts["score_state"], "no_detection");
        assert_eq!(counts["total_outputs"], 0);
        assert_eq!(counts["truth_outputs"], 0);
        assert_eq!(counts["wrong_outputs"], 0);
        assert_eq!(counts["strict_correct"], false);
    }
}
