//! External-corpus truth gate.
//!
//! `corpus-external/` (zxing blackbox suites + qrcode-ai production gallery)
//! is NOT vendored — 30 MB of third-party and production images. What IS
//! committed is `corpus-external.tsv`: one line per file pinning its sha256
//! and, for images, the decode status measured by a real run. TSV over TOML
//! deliberately: 500+ entries at one line each stay grep-able and
//! diff-per-file, where `[[entry]]` blocks would be ~4× the mass for zero
//! extra structure.
//!
//! `verify` (via `corpus-report --external`) re-walks the tree, re-hashes,
//! re-scans, and fails LOUD on any divergence in either direction —
//! a regression AND a "capability gained" both exit non-zero so the pin is
//! flipped deliberately (same semantics as the vendored `expect = "fail"`
//! frontier). A missing `corpus-external/` (CI checkout) skips gracefully
//! but noisily: it prints exactly how many manifested files went unchecked.
//!
//! The files themselves are machine-bound (untracked) and CAN vanish or rot —
//! 2026-07-08: ten gallery files were found deleted on disk and the gate went
//! red exactly as designed. Restore procedure, proven that day: every gallery
//! image is a public CDN object — resolve
//! `https://assets.qrcode-ai.com/<bucket-path>` via the landing repo
//! (`payload/**` references most files; `dev-assets/_ASSET_INDEX.txt` maps
//! the full bucket, e.g. `index/type/<file>` for the index-type group),
//! download, and sha256-verify against THIS manifest before copying into
//! place. zxing suites re-clone from the zxing repo. A dated tarball of the
//! whole tree lives at `~/.olympus/backups/qrcodeai/corpus-external-<date>`
//! — refresh it whenever `gen-external-manifest` rewrites the pins.
//!
//! Scans run budget-free (`budget_ms: None`): the wall-clock cut point is
//! the one machine-dependent knob in the pipeline (lib contract), and an
//! offline truth gate must not encode the speed of the machine that
//! generated it. Scoring is off — decode truth is what the pins hold; the
//! score contract is pinned by the vendored corpus and the test suite.

use std::collections::BTreeSet;
use std::path::Path;

use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner, ScoreDepth};
use rayon::prelude::*;
use sha2::{Digest as _, Sha256};

/// Committed manifest, at the repo root next to `corpus.toml`.
const MANIFEST: &str = "corpus-external.tsv";
/// Gitignored corpus root, at the repo root.
const CORPUS_DIR: &str = "corpus-external";

/// zxing's own `mustPassCount` at rotation 0° (`QRCodeBlackBoxNTestCase`) —
/// printed as context next to our per-suite match counts.
const ZXING_REF: [(&str, u32); 6] = [
    ("zxing-blackbox/qrcode-1", 17),
    ("zxing-blackbox/qrcode-2", 31),
    ("zxing-blackbox/qrcode-3", 38),
    ("zxing-blackbox/qrcode-4", 36),
    ("zxing-blackbox/qrcode-5", 16),
    ("zxing-blackbox/qrcode-6", 15),
];

/// Everything the product accepts plus the zxing suite formats.
const IMG_EXT: [&str; 6] = ["png", "jpg", "jpeg", "webp", "gif", "bmp"];

/// Measured decode state of one corpus file — the value the manifest pins.
///
/// `match`/`decode` are the pass family, `wrong`/`blind` the fail family;
/// four states instead of two because "decoded the wrong text" (the
/// miscorrection class) and "stayed blind" are different truths whose
/// transitions mean different things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Decoded and the text equals the sibling `.txt` ground truth.
    Match,
    /// Decoded; no ground truth exists to compare against (gallery images).
    Decode,
    /// Decoded but the text differs from the ground truth.
    Wrong,
    /// No decode.
    Blind,
    /// Not an image (ground-truth `.txt`, suite metadata) — hash-pinned only.
    Aux,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Decode => "decode",
            Self::Wrong => "wrong",
            Self::Blind => "blind",
            Self::Aux => "aux",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "match" => Some(Self::Match),
            "decode" => Some(Self::Decode),
            "wrong" => Some(Self::Wrong),
            "blind" => Some(Self::Blind),
            "aux" => Some(Self::Aux),
            _ => None,
        }
    }
}

/// One manifest line.
#[derive(Debug, Clone)]
struct Row {
    status: Status,
    sha256: String,
    /// Forward-slash path relative to `corpus-external/`.
    path: String,
}

// ------------------------------------------------------------------ shared

fn is_image(rel: &str) -> bool {
    Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMG_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

/// All files under `dir`, as sorted forward-slash paths relative to `dir`.
/// Dotfiles (`.DS_Store` and friends) are ignored — they are OS noise, not
/// corpus content.
fn walk_sorted(dir: &Path) -> Vec<String> {
    fn recurse(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read corpus dir") {
            let entry = entry.expect("dir entry");
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                recurse(&path, base, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .expect("under base")
                    .to_str()
                    .expect("utf-8 corpus path")
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    recurse(dir, dir, &mut out);
    out.sort();
    out
}

fn sha256_hex(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let digest = Sha256::digest(&bytes);
    let mut hex = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        write!(hex, "{b:02x}").expect("write to string");
    }
    hex
}

/// Ground truth for a zxing image: the sibling `.txt`, decoded exactly the
/// way the blackbox harness reads it (UTF-8, then ISO-8859-1) — no trimming.
fn ground_truth(img_abs: &Path) -> Option<String> {
    let txt = img_abs.with_extension("txt");
    let raw = std::fs::read(txt).ok()?;
    Some(match String::from_utf8(raw) {
        Ok(s) => s,
        // ISO-8859-1: every byte maps to the same code point.
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    })
}

/// The budget-free, score-free scanner every external run uses.
fn scanner() -> Scanner {
    let mut config = ScanProfile::Full.config();
    config.budget_ms = None;
    config.score_depth = ScoreDepth::Off;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
}

/// Decode status + the report's caught-engine-panic count. The count rides
/// along so `verify` can NAME the panic carriers (visibility only, never a
/// gate — a caught panic is already the honest accounting the engine wrapper
/// records in `report.trace.engine_panics`; the anonymous stderr line from
/// the panic hook names no file, this does).
fn scan_status(scanner: &Scanner, abs: &Path) -> (Status, u8) {
    let bytes = std::fs::read(abs).unwrap_or_else(|e| panic!("{}: {e}", abs.display()));
    let report = scanner
        .scan(ImageInput::encoded(&bytes))
        .unwrap_or_else(|e| panic!("scan {}: {e}", abs.display()));
    let engine_panics = report.trace.engine_panics;
    let texts: Vec<&str> = report
        .detections
        .iter()
        .map(|d| d.content.text.as_str())
        .collect();
    if texts.is_empty() {
        return (Status::Blind, engine_panics);
    }
    let status = match ground_truth(abs) {
        Some(truth) => {
            if texts.iter().any(|t| *t == truth) {
                Status::Match
            } else {
                Status::Wrong
            }
        }
        None => Status::Decode,
    };
    (status, engine_panics)
}

fn parse_manifest(text: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let n = idx + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.splitn(3, '\t');
        let (Some(status), Some(sha256), Some(path)) = (cols.next(), cols.next(), cols.next())
        else {
            return Err(format!("line {n}: expected status<TAB>sha256<TAB>path"));
        };
        let status =
            Status::parse(status).ok_or_else(|| format!("line {n}: unknown status {status:?}"))?;
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("line {n}: malformed sha256"));
        }
        if path.is_empty() {
            return Err(format!("line {n}: empty path"));
        }
        rows.push(Row {
            status,
            sha256: sha256.to_owned(),
            path: path.to_owned(),
        });
    }
    Ok(rows)
}

// ----------------------------------------------------------------- summary

/// Per-group table + the headline totals, from measured statuses.
fn print_summary(rows: &[Row]) {
    use std::fmt::Write as _;

    #[derive(Default)]
    struct Counts {
        matched: u32,
        decoded: u32,
        wrong: u32,
        blind: u32,
    }

    /// Group by suite / gallery subdir when there is one.
    fn group_of(path: &str) -> String {
        let mut parts = path.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), Some(_)) => format!("{a}/{b}"),
            (Some(a), _, _) => a.to_owned(),
            _ => String::new(),
        }
    }

    let mut groups: std::collections::BTreeMap<String, Counts> = std::collections::BTreeMap::new();
    for row in rows {
        if row.status == Status::Aux {
            continue;
        }
        let counts = groups.entry(group_of(&row.path)).or_default();
        match row.status {
            Status::Match => counts.matched += 1,
            Status::Decode => counts.decoded += 1,
            Status::Wrong => counts.wrong += 1,
            Status::Blind => counts.blind += 1,
            Status::Aux => unreachable!("filtered above"),
        }
    }
    let mut table = String::from(
        "| group | match | decode | wrong | blind | total | zxing-ref |\n|---|---|---|---|---|---|---|\n",
    );
    for (group, c) in &groups {
        let total = c.matched + c.decoded + c.wrong + c.blind;
        let reference = ZXING_REF.iter().find(|(name, _)| name == group).map_or(
            String::from("—"),
            |&(_, n)| {
                let flag = if c.matched >= n { "ok" } else { "BELOW" };
                format!("{n} {flag}")
            },
        );
        writeln!(
            table,
            "| {group} | {} | {} | {} | {} | {total} | {reference} |",
            c.matched, c.decoded, c.wrong, c.blind
        )
        .expect("write to string");
    }
    println!("\n{table}");
    let (mut zx_match, mut zx_total, mut ga_decode, mut ga_total) = (0u32, 0u32, 0u32, 0u32);
    for (group, c) in &groups {
        let total = c.matched + c.decoded + c.wrong + c.blind;
        if group.starts_with("zxing-blackbox/") {
            zx_match += c.matched;
            zx_total += total;
        } else if group.starts_with("qrcode-ai/") {
            ga_decode += c.matched + c.decoded;
            ga_total += total;
        }
    }
    let zx_ref: u32 = ZXING_REF.iter().map(|&(_, n)| n).sum();
    println!(
        "zxing-blackbox exact-text match @ 0°: {zx_match}/{zx_total} (zxing reference: {zx_ref})"
    );
    println!("qrcode-ai gallery decoded: {ga_decode}/{ga_total}");
}

// --------------------------------------------------------------- generate

/// `gen-external-manifest` — pin the CURRENT measured state. Expectations
/// are never hand-typed; a deliberate capability change is recorded by
/// re-running this and committing the diff.
pub(crate) fn generate() {
    let root = crate::repo_root();
    let dir = root.join(CORPUS_DIR);
    if !dir.is_dir() {
        eprintln!(
            "{CORPUS_DIR}/ not present at the repo root — nothing to pin.\n\
             Fetch the corpora first (README « Reproducing the headline numbers »)."
        );
        std::process::exit(2);
    }
    let rels = walk_sorted(&dir);
    let scanner = scanner();
    let rows: Vec<Row> = rels
        .par_iter()
        .map(|rel| {
            let abs = dir.join(rel);
            let status = if is_image(rel) {
                // Pins hold decode truth only — panic counts move with engine
                // versions and would churn the manifest for zero pin value.
                scan_status(&scanner, &abs).0
            } else {
                Status::Aux
            };
            Row {
                status,
                sha256: sha256_hex(&abs),
                path: rel.clone(),
            }
        })
        .collect();

    let mut out = String::from(
        "# corpus-external.tsv — pinned truth for the NOT-vendored external corpora.\n\
         # Generated by `cargo run --release -p xtask -- gen-external-manifest` from a\n\
         # real budget-free run — never hand-edited. Columns: status<TAB>sha256<TAB>path\n\
         # (path relative to corpus-external/). Status vocabulary:\n\
         #   match  — decoded, text equals the sibling .txt ground truth\n\
         #   decode — decoded, no ground truth exists (gallery images)\n\
         #   wrong  — decoded, text differs from ground truth (miscorrection class)\n\
         #   blind  — no decode (the documented-blind-spot family)\n\
         #   aux    — not an image (ground truth / metadata), hash-pinned only\n\
         # `corpus-report --external` re-measures and fails on ANY divergence:\n\
         # a regression and a capability gained both exit 1 (flip pins deliberately).\n",
    );
    for row in &rows {
        out.push_str(row.status.as_str());
        out.push('\t');
        out.push_str(&row.sha256);
        out.push('\t');
        out.push_str(&row.path);
        out.push('\n');
    }
    let manifest_path = root.join(MANIFEST);
    std::fs::write(&manifest_path, out).expect("write manifest");
    println!(
        "pinned {} files ({} images) into {}",
        rows.len(),
        rows.iter().filter(|r| r.status != Status::Aux).count(),
        manifest_path.display()
    );
    print_summary(&rows);
    println!(
        "\nritual: re-tar corpus-external/ into ~/.olympus/backups/qrcodeai/ and\n\
         refresh the README headline table if numbers moved (module doc: procedure)"
    );
}

// ----------------------------------------------------------------- verify

/// Sort measured results into live rows + capability deltas; hash drift
/// voids a pin and lands in `problems` instead.
fn classify(
    measured: Vec<(&Row, String, Option<Status>)>,
    problems: &mut Vec<String>,
) -> (Vec<Row>, Vec<String>, Vec<String>) {
    let mut live_rows: Vec<Row> = Vec::new();
    let mut gained: Vec<String> = Vec::new();
    let mut regressed: Vec<String> = Vec::new();
    for (row, sha, live) in measured {
        if sha != row.sha256 {
            problems.push(format!(
                "sha256 drift (corpus file changed — regenerate or restore): {}",
                row.path
            ));
            continue;
        }
        let Some(live) = live else {
            continue; // aux — hash already verified
        };
        live_rows.push(Row {
            status: live,
            sha256: sha,
            path: row.path.clone(),
        });
        if live == row.status {
            continue;
        }
        let delta = format!(
            "{} — pinned {}, now {}",
            row.path,
            row.status.as_str(),
            live.as_str()
        );
        // Towards ground truth (or towards any decode where no truth exists)
        // is a gained capability; every other move is a regression.
        let improved =
            live == Status::Match || (live == Status::Decode && row.status == Status::Blind);
        if improved {
            gained.push(delta);
        } else {
            regressed.push(delta);
        }
    }
    (live_rows, gained, regressed)
}

/// `corpus-report --external` — re-measure and compare against the pins.
/// One pinned row re-measured on disk: live sha + `(status, engine_panics)`
/// when the pin was scannable (hash matches, image row).
type Measured<'a> = (&'a Row, String, Option<(Status, u8)>);

pub(crate) fn verify() {
    let root = crate::repo_root();
    let manifest_text = std::fs::read_to_string(root.join(MANIFEST)).unwrap_or_else(|e| {
        eprintln!("{MANIFEST} missing at the repo root ({e}) — run gen-external-manifest first");
        std::process::exit(2);
    });
    let pinned = parse_manifest(&manifest_text).unwrap_or_else(|e| {
        eprintln!("{MANIFEST}: {e}");
        std::process::exit(2);
    });

    let dir = root.join(CORPUS_DIR);
    if !dir.is_dir() {
        // Graceful for CI checkouts (the corpus is not vendored) but LOUD:
        // the exact count of unverified pins is printed, never silently 0.
        let images = pinned.iter().filter(|r| r.status != Status::Aux).count();
        println!("corpus-external/ not present — external corpus gate SKIPPED");
        println!(
            "  {} manifested files NOT verified ({images} images unscanned · {} aux)",
            pinned.len(),
            pinned.len() - images
        );
        println!(
            "  fetch the corpora to run this gate — README « Reproducing the headline numbers »"
        );
        return;
    }

    let on_disk = walk_sorted(&dir);
    let disk_set: BTreeSet<&str> = on_disk.iter().map(String::as_str).collect();
    let pinned_set: BTreeSet<&str> = pinned.iter().map(|r| r.path.as_str()).collect();

    let mut problems: Vec<String> = Vec::new();
    for extra in disk_set.difference(&pinned_set) {
        problems.push(format!(
            "unmanifested file on disk (regenerate the manifest): {extra}"
        ));
    }
    for missing in pinned_set.difference(&disk_set) {
        problems.push(format!("manifested file missing on disk: {missing}"));
    }

    let scanner = scanner();
    let measured: Vec<Measured<'_>> = pinned
        .par_iter()
        .filter(|row| disk_set.contains(row.path.as_str()))
        .map(|row| {
            let abs = dir.join(&row.path);
            let sha = sha256_hex(&abs);
            // A hash-drifted file's pin is void — report the drift, skip the scan.
            let status = (sha == row.sha256 && row.status != Status::Aux)
                .then(|| scan_status(&scanner, &abs));
            (row, sha, status)
        })
        .collect();

    let panic_carriers: Vec<String> = measured
        .iter()
        .filter_map(|(row, _, s)| match s {
            Some((_, n)) if *n > 0 => Some(format!("{} × {n}", row.path)),
            _ => None,
        })
        .collect();

    let measured: Vec<(&Row, String, Option<Status>)> = measured
        .into_iter()
        .map(|(row, sha, s)| (row, sha, s.map(|(status, _)| status)))
        .collect();

    let (live_rows, gained, regressed) = classify(measured, &mut problems);

    print_summary(&live_rows);

    if !panic_carriers.is_empty() {
        println!(
            "\nengine panics caught + counted (report.trace.engine_panics · non-gating): {}",
            panic_carriers.len()
        );
        for c in &panic_carriers {
            println!("  {c}");
        }
    }

    for p in &problems {
        eprintln!("problem: {p}");
    }
    for r in &regressed {
        eprintln!("REGRESSION: {r}");
    }
    for g in &gained {
        eprintln!("capability gained — regenerate the manifest to flip the pin: {g}");
    }
    println!(
        "\nexternal gate: {} pins checked · {} regressed · {} gained · {} problems",
        live_rows.len(),
        regressed.len(),
        gained.len(),
        problems.len()
    );
    if !(problems.is_empty() && regressed.is_empty() && gained.is_empty()) {
        std::process::exit(1);
    }
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed manifest itself is a fixture: parseable, sorted,
    /// duplicate-free, and shaped like the corpus it pins. Runs everywhere —
    /// no corpus needed.
    #[test]
    fn committed_manifest_is_well_formed() {
        let path = crate::repo_root().join(MANIFEST);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let rows = parse_manifest(&text).expect("manifest parses");
        assert!(!rows.is_empty(), "manifest must not be empty");
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "rows must be sorted by path");
        let unique: BTreeSet<&str> = paths.iter().copied().collect();
        assert_eq!(unique.len(), paths.len(), "duplicate paths");
        for row in &rows {
            assert_eq!(
                is_image(&row.path),
                row.status != Status::Aux,
                "{}: image files carry a decode status, aux files don't",
                row.path
            );
            assert!(
                row.path.starts_with("zxing-blackbox/") || row.path.starts_with("qrcode-ai/"),
                "{}: unexpected corpus root",
                row.path
            );
        }
        // every zxing suite the reference table names is represented
        for (suite, _) in ZXING_REF {
            assert!(
                rows.iter()
                    .any(|r| r.path.starts_with(&format!("{suite}/"))),
                "manifest lost suite {suite}"
            );
        }
    }

    #[test]
    fn manifest_parser_rejects_malformed_lines() {
        assert!(
            parse_manifest("match\tdeadbeef\tx.png").is_err(),
            "short sha"
        );
        assert!(parse_manifest("nope\t").is_err(), "missing columns");
        let ok = format!("blind\t{}\tzxing-blackbox/qrcode-1/1.png", "a".repeat(64));
        assert_eq!(parse_manifest(&ok).expect("parses").len(), 1);
        assert!(
            parse_manifest(&format!("maybe\t{}\tx.png", "a".repeat(64))).is_err(),
            "unknown status"
        );
    }
}
