//! Repo automation. Commands:
//!
//! - `gen-fixtures` — deterministic clean matrix + degraded variants (run
//!   once, outputs committed; re-running must be byte-identical).
//! - `corpus-report` — run the scanner over `corpus.toml`, print per-category
//!   results, and rewrite the README managed block (derived numbers are
//!   never hand-typed). `--external` runs the external-corpus truth gate
//!   against `corpus-external.tsv` instead (see `external.rs`).
//! - `gen-external-manifest` — pin the measured state of `corpus-external/`
//!   into the committed manifest (expectations from a real run, never typed).
//! - `baseline` — run the corpus through a v0.2 CLI binary (`--bin <path>`)
//!   and write `docs/baseline-v02.json` (the Phase A exit-gate comparator).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)] // automation tool: fail loud, bounded pixel math

mod external;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Corpus {
    entry: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    path: String,
    category: String,
    /// Ground-truth text. Absent = expected NOT to decode (negative sample).
    expected: Option<String>,
    /// Frontier pin. `"fail"` marks a documented blind spot we cannot decode
    /// yet: staying blind is GREEN (frontier held), a fresh decode is a loud
    /// RED ("capability gained — flip expect to pass"). Absent / `"pass"` = the
    /// normal contract (decode must match `expected`).
    expect: Option<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("gen-fixtures") => gen_fixtures(),
        Some("gen-symbology-fixtures") => gen_symbology_fixtures(),
        Some("gen-external-manifest") => external::generate(),
        Some("corpus-report") => {
            let flags: Vec<String> = args.collect();
            let write = flags.iter().any(|f| f == "--write");
            let ext = flags.iter().any(|f| f == "--external");
            if flags.iter().any(|f| f != "--write" && f != "--external") || (write && ext) {
                // --write manages the vendored README block only; the external
                // gate never rewrites prose.
                eprintln!("usage: xtask corpus-report [--write | --external]");
                std::process::exit(2);
            }
            if ext {
                external::verify();
            } else {
                corpus_report(write);
            }
        }
        Some("baseline") => {
            let bin = args
                .nth(1)
                .expect("usage: xtask baseline --bin <path-to-v02-cli>");
            baseline(&bin);
        }
        _ => {
            eprintln!(
                "usage: xtask <gen-fixtures | gen-external-manifest | corpus-report [--write | --external] | baseline --bin <p>>"
            );
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------- fixtures

fn save_luma(img: &image::GrayImage, path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::DynamicImage::ImageLuma8(img.clone())
        .save(path)
        .unwrap();
    println!("wrote {}", path.display());
}

/// Render every non-QR symbology rxing can WRITE into fixtures/symbology/
/// (decoder ≠ encoder modules, so the roundtrip still exercises the real
/// detect path; EAN-13/Code 128 get independent cross-checks in tests).
fn gen_symbology_fixtures() {
    use rxing::{BarcodeFormat, EncodeHints, Writer};
    let dir = std::path::Path::new("fixtures/symbology");
    std::fs::create_dir_all(dir).expect("mkdir fixtures/symbology");

    let cases: &[(&str, BarcodeFormat, &str, i32, i32)] = &[
        (
            "datamatrix",
            BarcodeFormat::DATA_MATRIX,
            "https://qrcode-ai.com/dm",
            240,
            240,
        ),
        (
            "aztec",
            BarcodeFormat::AZTEC,
            "https://qrcode-ai.com/aztec",
            240,
            240,
        ),
        (
            "pdf417",
            BarcodeFormat::PDF_417,
            "https://qrcode-ai.com/pdf417",
            400,
            160,
        ),
        ("ean13", BarcodeFormat::EAN_13, "9506000134352", 320, 140),
        ("ean8", BarcodeFormat::EAN_8, "96385074", 240, 140),
        ("upca", BarcodeFormat::UPC_A, "036000291452", 320, 140),
        ("code128", BarcodeFormat::CODE_128, "QRCAI-0042", 360, 140),
        ("code39", BarcodeFormat::CODE_39, "SCANNER-39", 360, 140),
        ("itf14", BarcodeFormat::ITF, "15400141288763", 360, 140),
        (
            "datamatrix-gs1",
            BarcodeFormat::DATA_MATRIX,
            "\u{1d}010950600013435210AB12",
            240,
            240,
        ),
    ];
    for (name, format, content, w, h) in cases {
        let writer = rxing::MultiFormatWriter;
        let hints = EncodeHints {
            Margin: Some("8".to_owned()),
            ..EncodeHints::default()
        };
        let bits = writer
            .encode_with_hints(content, format, *w, *h, &hints)
            .unwrap_or_else(|e| panic!("encode {name}: {e:?}"));
        // BitMatrix → luma png (dark = 0)
        let (bw, bh) = (bits.width(), bits.height());
        let mut img = image::GrayImage::new(bw, bh);
        for y in 0..bh {
            for x in 0..bw {
                img.put_pixel(x, y, image::Luma([if bits.get(x, y) { 0 } else { 255 }]));
            }
        }
        let path = dir.join(format!("{name}.png"));
        img.save(&path).expect("save fixture");
        println!("wrote {} ({}x{})", path.display(), bw, bh);
    }
}

fn gen_fixtures() {
    let root = repo_root();
    // clean matrix — versions × EC, deterministic content per cell
    for version in [2i16, 5, 10] {
        for (ec, ec_name) in [
            (qrcode::EcLevel::L, "l"),
            (qrcode::EcLevel::M, "m"),
            (qrcode::EcLevel::Q, "q"),
            (qrcode::EcLevel::H, "h"),
        ] {
            // sized to fit the smallest (H) capacity of each version
            let content = if version == 2 {
                format!("qrc.ai/v{version}{ec_name}")
            } else {
                format!("https://qrcode-ai.com/c/v{version}{ec_name}")
            };
            let code = qrcode::QrCode::with_version(
                content.as_bytes(),
                qrcode::Version::Normal(version),
                ec,
            )
            .unwrap();
            let img = code
                .render::<image::Luma<u8>>()
                .module_dimensions(8, 8)
                .build();
            save_luma(
                &img,
                &root.join(format!("fixtures/clean/gen_v{version}_{ec_name}.png")),
            );
        }
    }
    // degraded variants of the v5-q cell — deterministic transforms
    let base_path = root.join("fixtures/clean/gen_v5_q.png");
    let base = image::open(&base_path).unwrap().to_luma8();
    let blurred = image::imageops::blur(&base, 2.0);
    save_luma(&blurred, &root.join("fixtures/degraded/gen_v5_q_blur2.png"));
    let small = image::imageops::resize(
        &base,
        base.width() / 6,
        base.height() / 6,
        image::imageops::FilterType::Triangle,
    );
    save_luma(&small, &root.join("fixtures/degraded/gen_v5_q_tiny.png"));
    let low: image::GrayImage = image::ImageBuffer::from_fn(base.width(), base.height(), |x, y| {
        let px = f32::from(base.get_pixel(x, y)[0]);
        image::Luma([(128.0 + (px - 128.0) * 0.25) as u8])
    });
    save_luma(
        &low,
        &root.join("fixtures/degraded/gen_v5_q_lowcontrast.png"),
    );
}

// ------------------------------------------------------------------ report

#[derive(Debug, Default)]
struct CategoryStats {
    total: u32,
    decoded_ok: u32,
    wrong_or_missed: u32,
    /// Frontier (`expect="fail"`) entries that stayed blind — the GREEN state.
    frontier_held: u32,
    /// Frontier entries that unexpectedly decoded — the LOUD RED state.
    frontier_gained: u32,
    total_ms: f64,
}

/// Corpus results plus the list of frontier fixtures that newly decoded — a
/// non-empty list means a blind spot was crossed and the run must go red.
struct CorpusRun {
    stats: BTreeMap<String, CategoryStats>,
    gained: Vec<String>,
}

fn run_corpus() -> CorpusRun {
    let root = repo_root();
    let manifest: Corpus =
        toml::from_str(&std::fs::read_to_string(root.join("corpus.toml")).unwrap()).unwrap();
    let scanner = Scanner::builder().profile(ScanProfile::Full).build();

    let mut stats: BTreeMap<String, CategoryStats> = BTreeMap::new();
    let mut gained: Vec<String> = Vec::new();
    for entry in &manifest.entry {
        let bytes =
            std::fs::read(root.join(&entry.path)).unwrap_or_else(|e| panic!("{}: {e}", entry.path));
        let report = scanner.scan(ImageInput::encoded(&bytes)).unwrap();
        let stat = stats.entry(entry.category.clone()).or_default();
        stat.total += 1;
        stat.total_ms += report.trace.total_ms;
        let got = report.detections.first().map(|d| d.content.text.as_str());
        if entry.expect.as_deref() == Some("fail") {
            // Frontier pin. With a known ground truth, "gained" means we finally
            // reached it; without one, ANY decode of a pinned blind spot is a
            // gain worth surfacing. Refusing (no decode) is the expected GREEN.
            let now_passes = match &entry.expected {
                Some(expected) => got == Some(expected.as_str()),
                None => got.is_some(),
            };
            if now_passes {
                stat.frontier_gained += 1;
                gained.push(format!("{} — decoded {got:?}", entry.path));
            } else {
                stat.frontier_held += 1;
            }
        } else {
            match (&entry.expected, got) {
                (Some(expected), Some(text)) if text == expected => stat.decoded_ok += 1,
                (None, None) => stat.decoded_ok += 1, // negative sample stayed negative
                _ => {
                    stat.wrong_or_missed += 1;
                    println!(
                        "MISS {} — expected {:?}, got {:?}",
                        entry.path, entry.expected, got
                    );
                }
            }
        }
    }
    CorpusRun { stats, gained }
}

fn corpus_report(write_readme: bool) {
    let CorpusRun { stats, gained } = run_corpus();
    let mut table =
        String::from("| category | pass | total | rate | avg ms |\n|---|---|---|---|---|\n");
    for (category, s) in &stats {
        let avg = s.total_ms / f64::from(s.total);
        if s.frontier_held + s.frontier_gained > 0 {
            // Frontier rows pin blind spots (expect=fail): the honest metric is
            // how many are still HELD (blind, as pinned), not a decode rate.
            // `held` in the rate column marks the row as expect-fail.
            writeln!(
                table,
                "| {category} | {} | {} | held | {avg:.0} |",
                s.frontier_held, s.total
            )
            .unwrap();
        } else {
            let rate = f64::from(s.decoded_ok) * 100.0 / f64::from(s.total);
            writeln!(
                table,
                "| {category} | {} | {} | {rate:.0}% | {avg:.0} |",
                s.decoded_ok, s.total
            )
            .unwrap();
        }
    }
    println!("\n{table}");

    if write_readme {
        let readme_path = repo_root().join("README.md");
        let readme = std::fs::read_to_string(&readme_path).unwrap();
        let (start, end) = ("<!-- corpus-report:begin -->", "<!-- corpus-report:end -->");
        let begin = readme.find(start).expect("README managed block start");
        let stop = readme.find(end).expect("README managed block end");
        let mut updated = readme[..begin + start.len()].to_owned();
        updated.push('\n');
        updated.push_str(&table);
        updated.push_str(&readme[stop..]);
        std::fs::write(&readme_path, updated).unwrap();
        println!("README corpus block updated");
    }

    // A crossed frontier is progress, but it must be LOUD and fail the run so
    // CI forces the pin to be flipped (expect=fail → pass) deliberately.
    for msg in &gained {
        eprintln!("capability gained — flip expect to pass: {msg}");
    }
    let failed: u32 = stats.values().map(|s| s.wrong_or_missed).sum();
    if failed > 0 || !gained.is_empty() {
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------- baseline

fn baseline(bin: &str) {
    let root = repo_root();
    let manifest: Corpus =
        toml::from_str(&std::fs::read_to_string(root.join("corpus.toml")).unwrap()).unwrap();
    let mut results = Vec::new();
    for entry in &manifest.entry {
        let out = std::process::Command::new(bin)
            .arg("-d") // decode-only: success-rate comparison, not scoring
            .arg("-j")
            .arg(root.join(&entry.path))
            .output()
            .expect("run v0.2 cli");
        let decoded = out.status.success();
        results.push(serde_json::json!({
            "path": entry.path,
            "category": entry.category,
            "decoded": decoded,
        }));
    }
    let payload = serde_json::to_string_pretty(&serde_json::json!({
        "tool": bin,
        "results": results,
    }))
    .unwrap();
    let out_path = root.join("docs/baseline-v02.json");
    std::fs::write(&out_path, payload).unwrap();
    println!("baseline written to {}", out_path.display());
}
