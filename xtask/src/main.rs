//! Repo automation. Commands:
//!
//! - `gen-fixtures` — deterministic clean matrix + degraded variants + the
//!   generated golden sources of `spec/README.md` § Examples (run once,
//!   outputs committed; re-running must be byte-identical).
//! - `corpus-report` — run the scanner over `corpus.toml`, print per-category
//!   results, and rewrite the README managed block (derived numbers are
//!   never hand-typed). `--external` runs the external-corpus truth gate
//!   against `corpus-external.tsv` instead (see `external.rs`).
//! - `gen-external-manifest` — pin the measured state of `corpus-external/`
//!   into the committed manifest (expectations from a real run, never typed).
//! - `rescue-stress` — QD-2 adversarial miscorrection measurement: a
//!   deterministic synthetic-occlusion sweep reporting the S5 rescue's
//!   wrong-vs-refuse rate under rising occlusion (see `rescue_stress.rs`).
//! - `resource-probe` — one scoring resource regression cell, one scan per
//!   process, for an external wall/RSS/CPU guard (see `resource_probe.rs`).
//! - `rotation-sweep` — decode-under-rotation truth on the zxing blackbox
//!   corpus at 0°/90°/180°/270° via exact index-permutation rotations
//!   (see `rotation_sweep.rs`). Non-gating dashboard.
//! - `baseline` — run the corpus through a v0.2 CLI binary (`--bin <path>`)
//!   and write `docs/baseline-v02.json` (the Phase A exit-gate comparator).
//! - `bench` — latency, memory, allocations, throughput and WASM cost,
//!   base versus candidate, under the host gate (see `bench.rs`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)] // automation tool: fail loud, bounded pixel math

mod bench;
mod external;
mod oracle;
mod rescue_stress;
mod resource_probe;
mod rotation_sweep;
mod sync_version;
mod truth_amendments;

/// Loud allocator — every single allocation request ≥ 1 GiB prints its size
/// before being served. The CI runner died on a silent 14 GiB request from
/// the scan path (deep-checks run 28862018546); on 64 GB dev machines that
/// request SUCCEEDS untouched (no RSS trace), so the only reliable witness
/// is the request itself. Zero overhead beyond one branch per alloc.
struct LoudAlloc;

const LOUD_ALLOC_THRESHOLD: usize = 1 << 30;

std::thread_local! {
    /// Re-entrancy guard: capturing a backtrace allocates through this same
    /// allocator — without the flag the hook would recurse forever.
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// SAFETY: delegates verbatim to `System`; the diagnostics run behind a
// re-entrancy guard so their own allocations skip the hook.
unsafe impl std::alloc::GlobalAlloc for LoudAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if layout.size() >= LOUD_ALLOC_THRESHOLD && !IN_HOOK.with(std::cell::Cell::get) {
            IN_HOOK.with(|f| f.set(true));
            eprintln!("[loud-alloc] single request of {} bytes", layout.size());
            if layout.size() >= (8 << 30) {
                eprintln!(
                    "[loud-alloc] backtrace:\n{}",
                    std::backtrace::Backtrace::force_capture()
                );
            }
            IN_HOOK.with(|f| f.set(false));
        }
        // SAFETY: caller upholds GlobalAlloc's contract; layout forwarded as-is.
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: ptr came from this allocator with the same layout.
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: LoudAlloc = LoudAlloc;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use qrcode_ai_scanner::ImageInput;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Corpus {
    entry: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    path: String,
    category: String,
    /// Ground-truth text. Absent = negative, except an unlabelled frontier
    /// (`expect = "fail"`). A blind spot is not proof of a negative sample.
    expected: Option<String>,
    /// Frontier pin. `"fail"` marks a documented blind spot we cannot decode
    /// yet: staying blind is GREEN (frontier held), a fresh decode is a loud
    /// RED (review observation and truth before changing the pin).
    /// Absent / `"pass"` = the normal contract (decode must match `expected`).
    expect: Option<String>,
    /// The payload bytes (lowercase hex) when they are known independently
    /// of the scanner under test: the generator's payload or an independent
    /// reader's bytes. Present = a structural truth, judged by content
    /// identity (`spec/01-report.md` § Comparing contents).
    raw_hex: Option<String>,
    /// The ECI designators the symbol declares, located as the report lists
    /// them. Requires `raw_hex`.
    eci: Option<Vec<oracle::Eci>>,
    /// The symbol's FNC1 position (`"first"` · `"second"`). Requires
    /// `raw_hex`.
    fnc1: Option<qrcode_ai_scanner::Fnc1Position>,
}

fn repo_root() -> PathBuf {
    let configured = std::env::var_os("QRSCAN_REPO_ROOT");
    let embedded = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    resolve_repo_root(configured.as_deref(), &embedded)
        .and_then(|root| {
            if configured.is_some() {
                validate_repo_root(&root)?;
            }
            Ok(root)
        })
        .unwrap_or_else(|error| {
            eprintln!("xtask: {error}");
            std::process::exit(2);
        })
}

/// Runtime selection is separate from filesystem validation so tests never
/// mutate the process environment. The compiled default remains unchanged.
fn resolve_repo_root(configured: Option<&OsStr>, embedded: &Path) -> Result<PathBuf, String> {
    let Some(configured) = configured else {
        return Ok(embedded.to_path_buf());
    };
    let root = PathBuf::from(configured);
    if !root.is_absolute() {
        return Err(String::from("QRSCAN_REPO_ROOT must be an absolute path"));
    }
    Ok(root)
}

/// A downloaded instrument must read a source checkout's manifests, never
/// silently fall back to the build runner's embedded checkout on a bad path.
fn validate_repo_root(root: &Path) -> Result<(), String> {
    if !root.is_dir() {
        return Err(String::from(
            "QRSCAN_REPO_ROOT must name an existing directory",
        ));
    }
    let read = |name: &str| {
        std::fs::read_to_string(root.join(name))
            .map_err(|_| format!("QRSCAN_REPO_ROOT must contain readable {name}"))
    };
    validate_repo_manifests(&read("corpus.toml")?, &read(external::MANIFEST)?)
}

fn validate_repo_manifests(vendored: &str, external_text: &str) -> Result<(), String> {
    let corpus: Corpus = toml::from_str(vendored)
        .map_err(|_| String::from("QRSCAN_REPO_ROOT corpus.toml is invalid"))?;
    oracle::vendored_truths(&corpus).map_err(|e| format!("QRSCAN_REPO_ROOT corpus.toml: {e}"))?;
    let rows = external::parse_manifest(external_text)
        .map_err(|e| format!("QRSCAN_REPO_ROOT corpus-external.tsv: {e}"))?;
    if corpus.entry.is_empty() || rows.is_empty() {
        return Err(String::from(
            "QRSCAN_REPO_ROOT corpus manifests must not be empty",
        ));
    }
    Ok(())
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("gen-fixtures") => gen_fixtures(),
        Some("gen-symbology-fixtures") => gen_symbology_fixtures(),
        Some("gen-external-manifest") => external::generate(),
        Some("oracle") => oracle::run(args.collect()),
        Some("bench") => bench::run(args.collect()),
        Some("rescue-stress") => rescue_stress::run(),
        Some("resource-probe") => resource_probe::run(&args.collect::<Vec<_>>()),
        Some("rotation-sweep") => rotation_sweep::run(),
        Some("sync-version") => {
            let flags: Vec<String> = args.collect();
            let check = flags.iter().any(|f| f == "--check");
            if flags.iter().any(|f| f != "--check") {
                eprintln!("usage: xtask sync-version [--check]");
                std::process::exit(2);
            }
            sync_version::run(check);
        }
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
                "usage: xtask <gen-fixtures | gen-symbology-fixtures | gen-external-manifest | corpus-report [--write | --external] | oracle [--json <path>] [--corpus-root <abs path>] | bench <prepare | prepare-base | stamp | inspect | gate | run> | rescue-stress | resource-probe --mode full|fast|frame|design | rotation-sweep | sync-version [--check] | baseline --bin <p>>"
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
    gen_content_fixtures(&root);
    gen_payload_fixtures(&root);
}

/// The golden sources of the content contract (spec/README § Examples):
/// each symbol is the explicit segment stream an `exact_content` row pins,
/// so its bytes, declarations and FNC1 position are known exactly.
fn gen_content_fixtures(root: &Path) {
    use qrcode::{EcLevel, Version};
    // ECI 4 (ISO/IEC 8859-2) over byte A1: 'Ą'.
    let eci4 = v3_m(|b| {
        b.push_eci_designator(4)?;
        b.push_byte_data(&[0xA1])
    });
    save_modules(
        &eci4.to_colors(),
        eci4.width(),
        8,
        &root.join("fixtures/eci/eci4-a1.png"),
    );
    // ECI 899 (binary) over four bytes no text encoding reads.
    let binary = v3_m(|b| {
        b.push_eci_designator(899)?;
        b.push_byte_data(&[0x00, 0xFF, 0x80, 0x7F])
    });
    save_modules(
        &binary.to_colors(),
        binary.width(),
        8,
        &root.join("fixtures/eci/eci899-binary.png"),
    );
    // ECI 26 + FNC1 in first position + a GTIN element string (numeric).
    let gs1 = v3_m(|b| {
        b.push_eci_designator(26)?;
        b.push_fnc1_first_position()?;
        b.push_numeric_data(b"0109506000134352")
    });
    save_modules(
        &gs1.to_colors(),
        gs1.width(),
        8,
        &root.join("fixtures/clean/gs1-fnc1.png"),
    );
    // 中文 (GB 2312 D6 D0 CE C4) in one Hanzi segment, GB/T 18284 subset 1:
    // a mode the encoder's Bits API cannot write, so the data codewords are
    // assembled here (v2-M, as the exact_content rows render it).
    let mut stream: Vec<bool> = Vec::new();
    let mut push = |value: u32, n: u32| stream.extend((0..n).rev().map(|i| value >> i & 1 == 1));
    push(0b1101, 4);
    push(1, 4);
    push(2, 8);
    for pair in [[0xD6u8, 0xD0], [0xCE, 0xC4]] {
        let p = u32::from(pair[0]) << 8 | u32::from(pair[1]);
        let v = if p <= 0xAAFE { p - 0xA1A1 } else { p - 0xA6A1 };
        push((v >> 8) * 0x60 + (v & 0xFF), 13);
    }
    push(0, 4);
    let (colors, width) = stream_symbol(Version::Normal(2), EcLevel::M, &stream);
    save_modules(
        &colors,
        width,
        8,
        &root.join("fixtures/eci/hanzi-zhongwen.png"),
    );
}

/// A version-3 / EC M symbol from an explicit segment stream (terminator
/// and padding added).
fn v3_m(
    push: impl FnOnce(&mut qrcode::bits::Bits) -> qrcode::types::QrResult<()>,
) -> qrcode::QrCode {
    let mut bits = qrcode::bits::Bits::new(qrcode::Version::Normal(3));
    push(&mut bits).unwrap();
    bits.push_terminator(qrcode::EcLevel::M).unwrap();
    qrcode::QrCode::with_bits(bits, qrcode::EcLevel::M).unwrap()
}

/// The golden sources of the payload kinds that have no corpus image: one
/// symbol per kind at the smallest version its EC level fits, 6 px per
/// module.
fn gen_payload_fixtures(root: &Path) {
    use qrcode::{EcLevel, QrCode};
    let payloads: [(&str, &str, EcLevel); 5] = [
        (
            "wifi",
            "WIFI:T:WPA;S:QRCodeAI Guest;P:sunrise2027;;",
            EcLevel::Q,
        ),
        (
            "me_card",
            "MECARD:N:Doe,Jane;TEL:+33600000000;URL:https://qrcode-ai.com;;",
            EcLevel::Q,
        ),
        (
            "crypto-bitcoin",
            "bitcoin:bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq?amount=0.01",
            EcLevel::Q,
        ),
        ("sms", "SMSTO:+33600000000:Hello from a QR", EcLevel::H),
        (
            "gs1-digital-link",
            "https://id.gs1.org/01/09506000134352/10/LOT1?17=271231",
            EcLevel::Q,
        ),
    ];
    for (name, text, ec) in payloads {
        let code = QrCode::with_error_correction_level(text, ec).unwrap();
        save_modules(
            &code.to_colors(),
            code.width(),
            6,
            &root.join(format!("fixtures/payloads/{name}.png")),
        );
    }
}

/// The module colors of a symbol whose data codewords are `stream` (MSB
/// first, terminator included), zero-padded to a byte and then filled with
/// the ISO pad codewords 0xEC / 0x11: the encoder's own error correction,
/// placement and masking around a stream its Bits API refuses to write.
fn stream_symbol(
    version: qrcode::Version,
    ec: qrcode::EcLevel,
    stream: &[bool],
) -> (Vec<qrcode::Color>, usize) {
    let mut data: Vec<u8> = stream
        .chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0u8, |byte, (i, &bit)| byte | (u8::from(bit) << (7 - i)))
        })
        .collect();
    let capacity = qrcode::bits::Bits::new(version).max_len(ec).unwrap() / 8;
    assert!(data.len() <= capacity, "the stream fits {version:?} {ec:?}");
    let mut pads = [0xEC, 0x11].into_iter().cycle();
    while data.len() < capacity {
        data.push(pads.next().unwrap());
    }
    let (data_codewords, ec_codewords) =
        qrcode::ec::construct_codewords(&data, version, ec).unwrap();
    let mut canvas = qrcode::canvas::Canvas::new(version, ec);
    canvas.draw_all_functional_patterns();
    canvas.draw_data(&data_codewords, &ec_codewords);
    let colors = canvas.apply_best_mask().into_colors();
    (colors, usize::try_from(version.width()).unwrap())
}

/// Module colors (row-major, `width` per row) as a luma PNG: `px` pixels per
/// module inside the 4-module quiet zone, dark modules black on white.
fn save_modules(colors: &[qrcode::Color], width: usize, px: u32, path: &Path) {
    let modules = u32::try_from(width).unwrap();
    let side = (modules + 8) * px;
    let mut img = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    for (i, color) in colors.iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let i = u32::try_from(i).unwrap();
            let (mx, my) = (4 + i % modules, 4 + i / modules);
            for dy in 0..px {
                for dx in 0..px {
                    img.put_pixel(mx * px + dx, my * px + dy, image::Luma([0]));
                }
            }
        }
    }
    save_luma(&img, path);
}

// ------------------------------------------------------------------ report

#[derive(Debug, Default)]
struct CategoryStats {
    total: u32,
    truth: u32,
    exact: u32,
    negatives: u32,
    negative_held: u32,
    unlabelled: u32,
    unlabelled_decoded: u32,
    unlabelled_blind: u32,
    scan_errors: u32,
    wrong_or_missed: u32,
    /// Frontier (`expect="fail"`) entries that stayed blind — the GREEN state.
    frontier_held: u32,
    /// Frontier entries that unexpectedly decoded — the LOUD RED state.
    frontier_gained: u32,
    /// Sum of the ladder-only `trace.total_ms` (no image decoding, no
    /// scoring) — exploratory context for the `avg ms` column, never a gate.
    total_ms: f64,
}

impl CategoryStats {
    fn include(&mut self, entry: &Entry) {
        self.total += 1;
        match oracle::vendored_expectation(entry)
            .unwrap_or_else(|e| panic!("corpus.toml: {}: {e}", entry.path))
            .as_deref()
        {
            None => self.unlabelled += 1,
            Some([]) => self.negatives += 1,
            Some(_) => self.truth += 1,
        }
    }

    fn record(&mut self, state: EntryState, outcome: Option<oracle::Outcome>) {
        match outcome {
            Some(oracle::Outcome::Exact) => self.exact += 1,
            Some(oracle::Outcome::NegativeHeld) => self.negative_held += 1,
            None if state == EntryState::Held => self.unlabelled_blind += 1,
            None if state == EntryState::Gained => self.unlabelled_decoded += 1,
            _ => {}
        }
        match state {
            EntryState::Held => self.frontier_held += 1,
            EntryState::Gained => self.frontier_gained += 1,
            EntryState::Miss => self.wrong_or_missed += 1,
            EntryState::Pass => {}
        }
    }
}

/// Corpus results plus the list of frontier fixtures that newly decoded — a
/// non-empty list means a blind spot was crossed and the run must go red.
struct CorpusRun {
    stats: BTreeMap<String, CategoryStats>,
    gained: Vec<String>,
}

/// `[qr_code "…", ean13 "…"]` — observed detections for a report line,
/// every text `{:?}`-escaped (decoded text is attacker-controlled, even
/// here).
fn described(observed: &[oracle::Seen]) -> String {
    let groups: Vec<String> = observed
        .iter()
        .map(|seen| format!("{} {:?}", oracle::wire(&seen.symbology), seen.text))
        .collect();
    format!("[{}]", groups.join(", "))
}

fn run_corpus() -> CorpusRun {
    let root = repo_root();
    let manifest: Corpus =
        toml::from_str(&std::fs::read_to_string(root.join("corpus.toml")).unwrap()).unwrap();
    // The oracle's budget-free, score-free scanner: a wall-clock cut point
    // made this verdict depend on host load (a busy machine cut artistic
    // fixtures mid-ladder), and decode truth needs no score.
    let scanner = external::scanner();

    let mut stats: BTreeMap<String, CategoryStats> = BTreeMap::new();
    let mut gained: Vec<String> = Vec::new();
    for entry in &manifest.entry {
        let bytes =
            std::fs::read(root.join(&entry.path)).unwrap_or_else(|e| panic!("{}: {e}", entry.path));
        let stat = stats.entry(entry.category.clone()).or_default();
        stat.include(entry);
        let report = match scanner.scan(ImageInput::encoded(&bytes)) {
            Ok(report) => report,
            Err(e) => {
                stat.wrong_or_missed += 1;
                stat.scan_errors += 1;
                println!("MISS {} — scan error {}: {e}", entry.path, e.code());
                continue;
            }
        };
        stat.total_ms += report.trace.total_ms;
        let observed = oracle::observations(&report.detections);
        let (classification, outcome) = entry_state(entry, &observed);
        stat.record(classification, outcome);
        match (classification, outcome) {
            (EntryState::Gained, _) => {
                gained.push(format!("{} — decoded {}", entry.path, described(&observed)));
            }
            (EntryState::Miss, outcome) => {
                println!(
                    "MISS {} — {}: expected {:?}, got {}",
                    entry.path,
                    outcome.map_or("unlabelled", oracle::Outcome::as_str),
                    entry.expected,
                    described(&observed)
                );
            }
            _ => {}
        }
    }
    CorpusRun { stats, gained }
}

/// Where one `corpus.toml` entry lands in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    /// Exact truth, or a negative sample that stayed negative.
    Pass,
    /// Frontier pin still blind — the expected GREEN.
    Held,
    /// Frontier pin now decodes — a LOUD RED observation requiring review.
    Gained,
    /// Anything else: missed, wrong, extra, partial, mixed, false positive.
    Miss,
}

/// The strict verdict of one entry: its COMPLETE detection list judged by
/// `oracle::judge` against the `corpus.toml` truth (any symbology; by
/// content identity when the entry carries `raw_hex`, else by text), so a
/// wrong or extra symbol next to the expected one is a miss, and so is an
/// `unread` one. Also returns the oracle outcome for the report line
/// (`None` for an unlabelled frontier pin).
fn entry_state(entry: &Entry, observed: &[oracle::Seen]) -> (EntryState, Option<oracle::Outcome>) {
    let expected = oracle::vendored_expectation(entry)
        .unwrap_or_else(|e| panic!("corpus.toml: {}: {e}", entry.path));
    let outcome = expected
        .as_deref()
        .map(|units| oracle::judge_detections(units, observed));
    let state = match (entry.expect.as_deref() == Some("fail"), outcome) {
        // Frontier pin: refusing (no decode) is the expected GREEN. With a
        // known ground truth, "gained" means we finally reached it exactly;
        // without one, ANY decode of a pinned blind spot is a gain worth
        // surfacing. Other output against a known truth is a miss.
        (true, _) if observed.is_empty() => EntryState::Held,
        (true, None | Some(oracle::Outcome::Exact)) => EntryState::Gained,
        (_, Some(oracle::Outcome::Exact | oracle::Outcome::NegativeHeld)) => EntryState::Pass,
        _ => EntryState::Miss,
    };
    (state, outcome)
}

fn corpus_table(stats: &BTreeMap<String, CategoryStats>) -> String {
    // Unknown truth must stay visible without entering either correctness
    // denominator, even when one category contains several label kinds.
    let mut table = String::from(
        "| category | images | exact / truth | negative held / negatives | unlabelled decoded / blind / total | frontier held / gained | scan errors | avg ms (ladder-only trace.total_ms, exploratory, not a gate) |\n|---|---|---|---|---|---|---|---|\n",
    );
    for (category, s) in stats {
        let measured = s.total - s.scan_errors;
        let avg = if measured == 0 {
            String::from("n/a")
        } else {
            format!("{:.0}", s.total_ms / f64::from(measured))
        };
        writeln!(
            table,
            "| {category} | {} | {} / {} | {} / {} | {} / {} / {} | {} / {} | {} | {avg} |",
            s.total,
            s.exact,
            s.truth,
            s.negative_held,
            s.negatives,
            s.unlabelled_decoded,
            s.unlabelled_blind,
            s.unlabelled,
            s.frontier_held,
            s.frontier_gained,
            s.scan_errors,
        )
        .unwrap();
    }
    table
}

fn corpus_report(write_readme: bool) {
    let CorpusRun { stats, gained } = run_corpus();
    let table = corpus_table(&stats);
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
    // CI forces a deliberate review. An unknown payload cannot become
    // ground truth merely because the scanner now produces an observation.
    for msg in &gained {
        eprintln!(
            "capability gained — review observation and independent truth before changing expect: {msg}"
        );
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

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use qrcode_ai_scanner::Charset;
    use qrcode_ai_scanner::Symbology::{self, Ean13, QrCode};

    #[test]
    fn runtime_repo_root_selection_is_explicit_and_absolute() {
        let embedded = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let relocated = std::env::temp_dir().join("qrscan-relocated-source");
        assert_eq!(resolve_repo_root(None, &embedded), Ok(embedded.clone()));
        assert_eq!(
            resolve_repo_root(Some(relocated.as_os_str()), &embedded),
            Ok(relocated)
        );
        for relative in ["", ".", "source", "../source"] {
            assert_eq!(
                resolve_repo_root(Some(OsStr::new(relative)), &embedded),
                Err(String::from("QRSCAN_REPO_ROOT must be an absolute path"))
            );
        }
    }

    const ROOT_CORPUS: &str =
        "[[entry]]\npath = 'fixtures/x.png'\ncategory = 'frontier'\nexpect = 'fail'\n";

    #[test]
    fn runtime_repo_root_requires_valid_nonempty_manifests() {
        let external = format!("aux\t{}\tx.txt\n", "a".repeat(64));
        assert_eq!(validate_repo_manifests(ROOT_CORPUS, &external), Ok(()));
        assert_eq!(
            validate_repo_manifests("[broken", &external),
            Err(String::from("QRSCAN_REPO_ROOT corpus.toml is invalid"))
        );
        assert_eq!(
            validate_repo_manifests(&ROOT_CORPUS.replace("'fail'", "'FAIL'"), &external),
            Err(String::from(
                "QRSCAN_REPO_ROOT corpus.toml: fixtures/x.png: expect must be \"pass\" or \"fail\""
            ))
        );
        for (vendored, external) in [("entry = []", external.as_str()), (ROOT_CORPUS, "")] {
            assert_eq!(
                validate_repo_manifests(vendored, external),
                Err(String::from(
                    "QRSCAN_REPO_ROOT corpus manifests must not be empty"
                ))
            );
        }
        assert_eq!(
            validate_repo_manifests(ROOT_CORPUS, "not a manifest"),
            Err(String::from(
                "QRSCAN_REPO_ROOT corpus-external.tsv: line 1: expected status<TAB>sha256<TAB>path"
            ))
        );
    }

    #[test]
    fn runtime_repo_root_never_falls_back_when_configured_files_are_missing() {
        let root = std::env::temp_dir().join(format!("qrscan-runtime-root-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("test directory");
        assert_eq!(
            validate_repo_root(&root.join("absent")),
            Err(String::from(
                "QRSCAN_REPO_ROOT must name an existing directory"
            ))
        );
        assert_eq!(
            validate_repo_root(&root),
            Err(String::from(
                "QRSCAN_REPO_ROOT must contain readable corpus.toml"
            ))
        );
        std::fs::write(root.join("corpus.toml"), ROOT_CORPUS).expect("manifest");
        assert_eq!(
            validate_repo_root(&root),
            Err(String::from(
                "QRSCAN_REPO_ROOT must contain readable corpus-external.tsv"
            ))
        );
        let external = format!("aux\t{}\tx.txt\n", "a".repeat(64));
        std::fs::write(root.join("corpus-external.tsv"), external).expect("external manifest");
        assert_eq!(validate_repo_root(&root), Ok(()));
        assert_eq!(
            validate_repo_root(&root.join("corpus.toml")),
            Err(String::from(
                "QRSCAN_REPO_ROOT must name an existing directory"
            ))
        );
        std::fs::remove_dir_all(root).expect("remove test directory");
    }

    fn entry(expected: Option<&str>, expect: Option<&str>) -> Entry {
        Entry {
            path: String::from("fixtures/x.png"),
            category: String::from("clean"),
            expected: expected.map(str::to_owned),
            expect: expect.map(str::to_owned),
            raw_hex: None,
            eci: None,
            fnc1: None,
        }
    }

    /// Plain detections: each text's UTF-8 bytes, read `utf8`, nothing
    /// declared.
    fn seen(observed: &[(Symbology, &str)]) -> Vec<oracle::Seen> {
        observed
            .iter()
            .map(|(symbology, text)| oracle::plain(*symbology, text))
            .collect()
    }

    /// The complete list is judged: `[E, W]` passed first-detection judging
    /// whenever E came first — it is now a miss (`extra`).
    #[test]
    fn truth_rows_pass_only_on_the_exact_set() {
        let truth = entry(Some("E"), None);
        let state = |observed: &[(Symbology, &str)]| entry_state(&truth, &seen(observed)).0;
        assert_eq!(state(&[(QrCode, "E")]), EntryState::Pass);
        assert_eq!(
            state(&[(Ean13, "E")]),
            EntryState::Pass,
            "text-only truth accepts any symbology"
        );
        assert_eq!(state(&[(QrCode, "E"), (QrCode, "W")]), EntryState::Miss);
        assert_eq!(state(&[(QrCode, "W")]), EntryState::Miss);
        assert_eq!(state(&[]), EntryState::Miss);
        assert_eq!(
            entry_state(&truth, &seen(&[(QrCode, "E"), (QrCode, "W")])).1,
            Some(oracle::Outcome::Extra)
        );
    }

    /// corpus.toml truth is text-only (any symbology), but ONE unit: the
    /// same text under a second symbology is extra output — a miss.
    #[test]
    fn a_text_only_truth_is_met_once() {
        use qrcode_ai_scanner::Symbology::MicroQrCode;
        let truth = entry(Some("E"), None);
        assert_eq!(
            entry_state(&truth, &seen(&[(QrCode, "E"), (MicroQrCode, "E")])),
            (EntryState::Miss, Some(oracle::Outcome::Extra))
        );
        assert_eq!(
            entry_state(&truth, &seen(&[(MicroQrCode, "E")])).0,
            EntryState::Pass
        );
    }

    /// An entry with `raw_hex` passes only on the identity it names: equal
    /// text over other bytes is a miss (`wrong`), and so is a read whose
    /// charset is `unknown` (`unread`, not exact).
    #[test]
    fn a_byte_truth_passes_only_on_its_identity() {
        let mut truth = entry(Some("A"), None);
        truth.raw_hex = Some(String::from("41"));
        assert_eq!(
            entry_state(&truth, &seen(&[(QrCode, "A")])),
            (EntryState::Pass, Some(oracle::Outcome::Exact))
        );
        let mut other_bytes = oracle::plain(QrCode, "A");
        if let Some(content) = other_bytes.content.as_mut() {
            content.raw = vec![0xEF, 0xBB, 0xBF, 0x41];
        }
        assert_eq!(
            entry_state(&truth, &[other_bytes]),
            (EntryState::Miss, Some(oracle::Outcome::Wrong))
        );
        let mut unread = oracle::plain(QrCode, "\u{FFFD}");
        if let Some(content) = unread.content.as_mut() {
            (content.raw, content.charset) = (vec![0x41], Some(Charset::Unknown));
        }
        assert_eq!(
            entry_state(&truth, &[unread]),
            (EntryState::Miss, Some(oracle::Outcome::Unread))
        );
    }

    #[test]
    fn negative_rows_stay_negative() {
        let negative = entry(None, None);
        assert_eq!(entry_state(&negative, &[]).0, EntryState::Pass);
        assert_eq!(
            entry_state(&negative, &seen(&[(QrCode, "W")])),
            (EntryState::Miss, Some(oracle::Outcome::FalsePositive))
        );
    }

    /// Frontier pins keep held / capability-gained; a known truth must be
    /// reached exactly to count as gained, any other output is a miss.
    #[test]
    fn frontier_rows_hold_or_gain() {
        let unlabelled = entry(None, Some("fail"));
        assert_eq!(entry_state(&unlabelled, &[]), (EntryState::Held, None));
        assert_eq!(
            entry_state(&unlabelled, &seen(&[(QrCode, "X")])),
            (EntryState::Gained, None)
        );
        let labelled = entry(Some("E"), Some("fail"));
        assert_eq!(entry_state(&labelled, &[]).0, EntryState::Held);
        assert_eq!(
            entry_state(&labelled, &seen(&[(QrCode, "E")])).0,
            EntryState::Gained
        );
        assert_eq!(
            entry_state(&labelled, &seen(&[(QrCode, "W")])).0,
            EntryState::Miss
        );
    }

    #[test]
    fn mixed_categories_keep_each_truth_denominator_and_frontier_observation() {
        let mut stats = CategoryStats::default();
        for (entry, observed) in [
            (entry(Some("E"), None), seen(&[(QrCode, "E")])),
            (entry(Some("E"), None), seen(&[(QrCode, "W")])),
            (entry(None, None), vec![]),
            (entry(None, Some("fail")), vec![]),
            (entry(None, Some("fail")), seen(&[(QrCode, "probe")])),
            (entry(Some("E"), Some("fail")), seen(&[(QrCode, "E")])),
        ] {
            stats.include(&entry);
            let (classification, outcome) = entry_state(&entry, &observed);
            stats.record(classification, outcome);
        }
        // Errors remain in their input denominator, but have no timing sample.
        stats.include(&entry(None, Some("fail")));
        stats.scan_errors += 1;
        stats.wrong_or_missed += 1;
        stats.total_ms = 60.0;
        assert_eq!(stats.wrong_or_missed, 2);
        assert_eq!(
            corpus_table(&BTreeMap::from([(String::from("mixed"), stats)])),
            "| category | images | exact / truth | negative held / negatives | unlabelled decoded / blind / total | frontier held / gained | scan errors | avg ms (ladder-only trace.total_ms, exploratory, not a gate) |\n\
             |---|---|---|---|---|---|---|---|\n\
             | mixed | 7 | 2 / 3 | 1 / 1 | 1 / 1 / 3 | 1 / 2 | 1 | 10 |\n"
        );
    }

    #[test]
    fn frontier_only_and_failed_categories_do_not_report_correctness_or_zero_latency() {
        let mut frontier = CategoryStats::default();
        frontier.include(&entry(None, Some("fail")));
        frontier.record(EntryState::Held, None);
        let mut failed = CategoryStats::default();
        failed.include(&entry(None, Some("fail")));
        failed.scan_errors = 1;
        let table = corpus_table(&BTreeMap::from([
            (String::from("frontier"), frontier),
            (String::from("failed"), failed),
        ]));
        assert_eq!(
            table.lines().skip(2).collect::<Vec<_>>(),
            [
                "| failed | 1 | 0 / 0 | 0 / 0 | 0 / 0 / 1 | 0 / 0 | 1 | n/a |",
                "| frontier | 1 | 0 / 0 | 0 / 0 | 0 / 1 / 1 | 1 / 0 | 0 | 0 |",
            ]
        );
    }

    #[test]
    fn report_lines_escape_decoded_text() {
        assert_eq!(
            described(&seen(&[(QrCode, "a\u{1b}[31m")])),
            "[qr_code \"a\\u{1b}[31m\"]"
        );
    }
}
