//! Threaded determinism — the lib contract says same bytes + same config ⇒
//! the same report, bit for bit, with `budget_ms: None` closing the one
//! machine-dependent knob (the wall-clock cut point). This pins the claim
//! where it actually gets stressed: one shared `Scanner` (`Send + Sync`,
//! no interior state) scanned from several OS threads under contention,
//! serialized, and compared byte-for-byte against a sequential baseline.
//!
//! Wall-clock trace fields (`trace.total_ms`, per-stage `ms`) are the one
//! DOCUMENTED nondeterminism in a report — normalized to zero before
//! comparison; everything else (detections, corners, score, hints, stage
//! sequence, attempt counts) must be identical.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use pretty_assertions::assert_eq;
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};

const THREADS: usize = 4;

fn fixture(rel: &str) -> Vec<u8> {
    let path = format!("{}/../../fixtures/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("missing fixture {path}: {e}"))
}

/// Serialize with the wall-clock fields zeroed — the only report content
/// allowed to differ between two runs of the same scan.
fn normalized_json(mut report: qrcode_ai_scanner::ScanReport) -> String {
    report.trace.total_ms = 0.0;
    for stage in &mut report.trace.stages {
        stage.ms = 0.0;
    }
    serde_json::to_string(&report).unwrap()
}

/// One clean decode, one artistic decode (deep-ladder + full stress score),
/// one frontier blind spot (the whole ladder runs and finds nothing) — the
/// three report shapes the pipeline can produce.
#[test]
fn concurrent_scans_yield_byte_identical_reports() {
    let cases = [
        ("clean/OK_68ms_100_4e875a2c.png", true),
        ("artistic/blob-style-monkey-logo.webp", true),
        ("frontier/scene-photo-overlay.webp", false),
    ];
    // The budget cut point is wall-clock and thus load-dependent — under
    // thread contention it WOULD flake (the documented caveat). `None`
    // is the reproducible mode; everything else is the stock Full profile.
    let mut config = ScanProfile::Full.config();
    config.budget_ms = None;
    let scanner = Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build();

    for (rel, decodes) in cases {
        let bytes = fixture(rel);
        let baseline = scanner.scan(ImageInput::encoded(&bytes)).unwrap();
        // Semantic anchor first — equality of the WRONG reports proves nothing.
        assert_eq!(
            !baseline.detections.is_empty(),
            decodes,
            "{rel}: unexpected decode outcome"
        );
        let baseline = normalized_json(baseline);

        let concurrent: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let scanner = &scanner;
                    let bytes = &bytes;
                    scope.spawn(move || {
                        normalized_json(scanner.scan(ImageInput::encoded(bytes)).unwrap())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("scan thread panicked"))
                .collect()
        });

        for (thread, report) in concurrent.iter().enumerate() {
            assert_eq!(
                report, &baseline,
                "{rel}: thread {thread} diverged from the sequential baseline"
            );
        }
    }
}
