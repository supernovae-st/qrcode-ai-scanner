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

/// Every symbology fixture but the PDF417 ones: rxing's PDF417 decoder
/// breaks codeword-confidence ties in randomized `HashMap` order, the one
/// documented engine exception (spec/04 § Determinism). Other symbologies'
/// readers carry no such order (std `RandomState` re-keys every map, so
/// in-process repetition alone would expose one).
fn symbology_fixtures() -> Vec<String> {
    let dir = format!("{}/../../fixtures/symbology", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| {
            let image = std::path::Path::new(name).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("png") || ext.eq_ignore_ascii_case("jpg")
            });
            image && !name.starts_with("pdf417")
        })
        .collect();
    names.sort();
    assert!(names.len() >= 20, "the symbology set: {names:?}");
    names
        .into_iter()
        .map(|name| format!("symbology/{name}"))
        .collect()
}

/// The `DataBar` fixture's native size reads at one scale only, and a lone
/// `DataBar` read waits for one at an independent scale (07-pipeline
/// § Acceptance): it publishes nothing there.
const DATABAR: &str = "symbology/databar-gtin.png";

/// The `DataBar` fixture at twice its size (nearest neighbour): the 512 px
/// pyramid and the 768 px plane both read it, so it is published and its
/// reader's stress cells run.
fn databar_twice() -> Vec<u8> {
    let native = image::load_from_memory(&fixture(DATABAR))
        .unwrap()
        .to_luma8();
    let twice = image::imageops::resize(
        &native,
        native.width() * 2,
        native.height() * 2,
        image::imageops::FilterType::Nearest,
    );
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(twice)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

/// One clean decode, one artistic decode (deep-ladder + full stress score),
/// one frontier blind spot (the whole ladder runs and finds nothing) — the
/// three report shapes the pipeline can produce — and every non-PDF417
/// symbology fixture (their readers, their stress cells; the `DataBar`'s
/// at twice its size).
#[test]
fn concurrent_scans_yield_byte_identical_reports() {
    let mut cases: Vec<(String, Vec<u8>, bool)> = [
        ("clean/OK_68ms_100_4e875a2c.png", true),
        ("artistic/blob-style-monkey-logo.webp", true),
        ("frontier/scene-photo-overlay.webp", false),
    ]
    .into_iter()
    .map(|(rel, decodes)| (rel.to_owned(), fixture(rel), decodes))
    .collect();
    cases.extend(symbology_fixtures().into_iter().map(|rel| {
        let decodes = rel != DATABAR;
        let bytes = fixture(&rel);
        (rel, bytes, decodes)
    }));
    cases.push((
        format!("{DATABAR} at twice its size"),
        databar_twice(),
        true,
    ));
    // The budget cut point is wall-clock and thus load-dependent — under
    // thread contention it WOULD flake (the documented caveat). `None`
    // is the reproducible mode; everything else is the stock Full profile.
    let mut config = ScanProfile::Full.config();
    config.budget_ms = None;
    let scanner = Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build();

    for (rel, bytes, decodes) in cases {
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
