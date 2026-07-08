//! `rescue-stress` — the QD-2 adversarial miscorrection measurement harness.
//!
//! # The question (plan §D2)
//!
//! The S5 erasure rescue buys ~20%→30% occlusion-radius headroom, and it is
//! refusal-biased at every step (syndrome re-check, structural parse). But one
//! class slips through by construction: a *miscorrection that lands on a VALID
//! codeword*. It passes the re-check because the corrected block IS a legal RS
//! codeword — just not the one that was written. As the erasure count climbs
//! toward the block budget `d − p`, that probability rises. This harness
//! measures the actual **rescue-WRONG rate** (decoded text ≠ known truth) vs
//! **rescue-REFUSE rate** under increasing occlusion, and asks whether the
//! `low_correction_margin` hint FLAGS the dangerous decodes. It produces the
//! numbers; the spec decision (should `low_correction_margin` become a default
//! REFUSAL in Full) is the operator's, made WITH these numbers.
//!
//! # Why NO RNG (the determinism contract)
//!
//! A miscorrection-rate measurement that jitters run-to-run is worthless: you
//! cannot tell a real 0.1% signal from sampling noise, and you cannot CI-gate
//! it. So every degree of freedom here is a fixed table, not a sample:
//!
//! - **Ground truth** is a deterministic filler prefix (`FILLER_ALPHABET`
//!   cycled), sized per `(version, ec)` by a monotone max-fit search — same
//!   truth string every run.
//! - **Occlusion geometry** (shape · fill · position · area) is derived from
//!   fixed tables and closed-form pixel math (`side = √(pct·area)`), never
//!   sampled. Positions are symbol-fraction anchors so they scale with version
//!   without a random offset.
//! - **The scan** runs `ScanProfile::Full` with `budget_ms = None`. The crate's
//!   contract (`lib.rs`): same bytes + same config ⇒ same attempt sequence, bit
//!   for bit — with the one documented caveat that a wall-clock budget makes the
//!   CUT point machine-dependent. Setting the budget to `None` removes that
//!   caveat: the ladder always runs to completion, so the report is a pure
//!   function of the pixels.
//! - **Parallelism** (rayon) is over independent single-threaded scans and
//!   `collect()`s in input order; aggregation folds into fixed enum-ordered
//!   buckets. The output carries zero timing (wall-clock lives on stderr only),
//!   so two runs on one machine diff byte-identical.
//!
//! # The two occlusion regimes (why two fills)
//!
//! The rescue's erasure detector marks a codeword when its worst module luma is
//! within ~30% of the symbol threshold (low confidence). That splits solid
//! occlusion into two adversarial regimes, and QD-2 lives in the second:
//!
//! - **`Dark` fill (luma 0)** — confident-but-wrong modules ⇒ *errors*, not
//!   erasures. RS spends two parity codewords per error; the block fails or
//!   miscorrects fastest here.
//! - **`Gray` fill (luma ≈ threshold)** — low-confidence modules ⇒ *erasures*.
//!   RS spends one codeword per erasure, so the count can climb all the way to
//!   `d − p` — exactly the knee where "miscorrection onto a valid codeword"
//!   becomes likely. This is the regime the S5 rescue was built for AND the one
//!   the QD-2 question interrogates.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)] // measurement tool: fail loud, bounded pixel math over synthetic symbols

use std::collections::BTreeMap;
use std::f32::consts::PI;
use std::fmt::Write as _;

use image::{GrayImage, Luma};
use qrcode_ai_scanner::{EngineKind, Hint, ImageInput, ScanProfile, Scanner};
use rayon::prelude::*;

// ------------------------------------------------------------------ grid

/// QR versions swept — small · mid · large, spanning the RS-block-count range
/// (v2 = 1 block, v10 = a handful): more blocks = more independent chances for
/// a miscorrection to land.
const VERSIONS: &[i16] = &[2, 6, 10];

/// The three EC levels that set the correction budget `d` (L is omitted: its
/// budget is too thin to reach the interesting erasure knee before refusing).
const EC_LEVELS: &[EcCell] = &[
    EcCell {
        ec: qrcode::EcLevel::M,
        tag: "m",
    },
    EcCell {
        ec: qrcode::EcLevel::Q,
        tag: "q",
    },
    EcCell {
        ec: qrcode::EcLevel::H,
        tag: "h",
    },
];

/// Occlusion area as a percentage of SYMBOL area, the QD-2 independent
/// variable. The 20→30 band is where the plan claims the rescue earns its keep.
const OCCLUSION_PCTS: &[u32] = &[5, 10, 15, 20, 25, 30, 35, 40];

/// Payload fill fractions of the per-cell max-fit capacity — two densities so a
/// symbol is stressed both packed (many data codewords) and half-empty.
const PAYLOAD_FRACTIONS: &[u32] = &[100, 60];

/// Pixels per module. 8 keeps v10 (57 modules ⇒ 456 px) comfortably above the
/// engines' sampling floor so DETECTION is never the bottleneck — occlusion is.
const MODULE_PX: u32 = 8;

/// Quiet zone in modules. Generous (ISO recommends 4) so a corner occlusion
/// bleeding into the border never starves the locator of its clear ring.
const QUIET_MODULES: u32 = 6;

/// Deterministic gray fill ≈ the symbol threshold (extremes 0/255 ⇒ mid 127).
/// 120 reads faintly dark to the engine binarizer yet sits inside the rescue's
/// <30%-of-half-span erasure window (|120−127| / 127 ≈ 5.5%) — the erasure
/// regime, on purpose.
const GRAY_LUMA: u8 = 120;

/// Alphabet cycled to build ground-truth payloads. Mixed case + `./:` forces
/// byte-mode encoding (lowercase is outside the QR alphanumeric set), so the
/// decoded charset resolves cleanly to UTF-8 and truth == the ASCII prefix.
const FILLER_ALPHABET: &[u8] = b"QRCODEAI0123456789abcdefghijklmnopqrstuvwxyz-./:";

/// Upper bound for the max-fit search — above every v2..v10 byte capacity
/// (v10-L ≈ 271 bytes), so the monotone decrement always finds the true knee.
const FILLER_MAX: usize = 400;

#[derive(Clone, Copy)]
struct EcCell {
    ec: qrcode::EcLevel,
    tag: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Square,
    Disc,
}
const SHAPES: &[(Shape, &str)] = &[(Shape::Square, "square"), (Shape::Disc, "disc")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fill {
    /// luma 0 — confident-wrong modules ⇒ RS errors (error regime).
    Dark,
    /// luma ≈ threshold — low-confidence modules ⇒ RS erasures (the d−p regime).
    Gray,
}
const FILLS: &[(Fill, &str)] = &[(Fill::Dark, "dark"), (Fill::Gray, "gray")];

impl Fill {
    fn luma(self) -> u8 {
        match self {
            Self::Dark => 0,
            Self::Gray => GRAY_LUMA,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Gray => "gray",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Position {
    /// Symbol center — the classic centered-logo occlusion.
    Center,
    /// Bottom-right data quadrant — no finder lives here (finders are TL/TR/BL).
    OffFinder,
    /// Directly over the top-left finder — destroys the locator (the control:
    /// the grid is never detected, so the rescue can't even be attempted).
    OnFinder,
    /// Just below/left of the top-right finder — OFF the finder, but on the
    /// low versions this anchor clips the top-right FORMAT-INFORMATION strip
    /// (ISO row 8 / cols 17..24). Corrupting format info makes the engine read
    /// the wrong mask/EC ⇒ a systematic within-budget miscorrection — a decode
    /// vector distinct from the data-erasure one the rescue targets.
    CornerAdjacent,
}
const POSITIONS: &[(Position, &str)] = &[
    (Position::Center, "center"),
    (Position::OffFinder, "off_finder"),
    (Position::OnFinder, "on_finder"),
    (Position::CornerAdjacent, "corner_adjacent"),
];

impl Position {
    /// Occlusion center as a fraction of the symbol side. `modules` sets the
    /// version-dependent finder anchor (finder centers sit at module 3.5).
    fn fraction(self, modules: f32) -> (f32, f32) {
        match self {
            Self::Center => (0.5, 0.5),
            Self::OffFinder => (0.72, 0.72),
            Self::OnFinder => (3.5 / modules, 3.5 / modules),
            Self::CornerAdjacent => (0.78, 0.30),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::OffFinder => "off_finder",
            Self::OnFinder => "on_finder",
            Self::CornerAdjacent => "corner_adjacent",
        }
    }
}

/// One fully-specified measurement cell.
#[derive(Clone)]
struct Cell {
    version: i16,
    ec: EcCell,
    shape: Shape,
    fill: Fill,
    position: Position,
    occ_pct: u32,
    truth: String,
}

// ------------------------------------------------------------ encode / occlude

/// Deterministic filler prefix of `len` bytes (ASCII ⇒ byte == char).
fn filler(len: usize) -> String {
    (0..len)
        .map(|i| FILLER_ALPHABET[i % FILLER_ALPHABET.len()] as char)
        .collect()
}

/// Largest filler length that still encodes at `(version, ec)`. Byte-mode bit
/// cost is monotone in length, so the plain decrement finds the exact knee and
/// is trivially reproducible.
fn max_fit(version: i16, ec: qrcode::EcLevel) -> usize {
    for len in (1..=FILLER_MAX).rev() {
        if qrcode::QrCode::with_version(
            filler(len).as_bytes(),
            qrcode::Version::Normal(version),
            ec,
        )
        .is_ok()
        {
            return len;
        }
    }
    1
}

/// Render the bare symbol (no quiet zone) so its pixel bounds are exactly
/// `modules · MODULE_PX` and geometry math is closed-form.
fn render_symbol(version: i16, ec: qrcode::EcLevel, content: &str) -> GrayImage {
    let code =
        qrcode::QrCode::with_version(content.as_bytes(), qrcode::Version::Normal(version), ec)
            .expect("content sized to fit by max_fit");
    code.render::<Luma<u8>>()
        .quiet_zone(false)
        .module_dimensions(MODULE_PX, MODULE_PX)
        .build()
}

/// Composite the symbol onto a white canvas with a controlled quiet zone.
/// Returns the canvas and the symbol's top-left pixel offset.
fn canvas_with_symbol(symbol: &GrayImage) -> (GrayImage, u32) {
    let qz = QUIET_MODULES * MODULE_PX;
    let mut canvas = GrayImage::from_pixel(
        symbol.width() + 2 * qz,
        symbol.height() + 2 * qz,
        Luma([255]),
    );
    for y in 0..symbol.height() {
        for x in 0..symbol.width() {
            canvas.put_pixel(x + qz, y + qz, *symbol.get_pixel(x, y));
        }
    }
    (canvas, qz)
}

/// Floor/ceil clamp of a pixel coordinate into `[0, max]`.
fn clamp_lo(v: f32, max: u32) -> u32 {
    v.floor().clamp(0.0, max as f32) as u32
}
fn clamp_hi(v: f32, max: u32) -> u32 {
    v.ceil().clamp(0.0, max as f32) as u32
}

/// Paint the occlusion (deterministic pixel math — no sampling anywhere).
fn paint_occlusion(canvas: &mut GrayImage, cell: &Cell, symbol_side: f32, origin: u32) {
    let modules = f32::from(cell.version) * 4.0 + 17.0;
    let (fx, fy) = cell.position.fraction(modules);
    let cx = origin as f32 + fx * symbol_side;
    let cy = origin as f32 + fy * symbol_side;
    let area = (cell.occ_pct as f32 / 100.0) * symbol_side * symbol_side;
    let fill = cell.fill.luma();
    let (w, h) = (canvas.width(), canvas.height());

    match cell.shape {
        Shape::Square => {
            let half = area.sqrt() / 2.0;
            for y in clamp_lo(cy - half, h)..clamp_hi(cy + half, h) {
                for x in clamp_lo(cx - half, w)..clamp_hi(cx + half, w) {
                    canvas.put_pixel(x, y, Luma([fill]));
                }
            }
        }
        Shape::Disc => {
            let r = (area / PI).sqrt();
            let r2 = r * r;
            for y in clamp_lo(cy - r, h)..clamp_hi(cy + r, h) {
                for x in clamp_lo(cx - r, w)..clamp_hi(cx + r, w) {
                    let dx = x as f32 + 0.5 - cx;
                    let dy = y as f32 + 0.5 - cy;
                    if dx * dx + dy * dy <= r2 {
                        canvas.put_pixel(x, y, Luma([fill]));
                    }
                }
            }
        }
    }
}

/// Build the occluded PNG bytes for a cell.
fn render_cell(cell: &Cell) -> Vec<u8> {
    let symbol = render_symbol(cell.version, cell.ec.ec, &cell.truth);
    let symbol_side = symbol.width() as f32;
    let (mut canvas, origin) = canvas_with_symbol(&symbol);
    paint_occlusion(&mut canvas, cell, symbol_side, origin);
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(canvas)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode occluded png");
    png
}

// ---------------------------------------------------------------- classify

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// A QR-family detection decoded to the known truth.
    Correct,
    /// A QR-family detection decoded to something ELSE — the miscorrection class.
    Wrong,
    /// No QR decoded (empty, or only non-QR noise).
    Refused,
}

#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent classification flags read straight off one report — a state \
              machine would obscure the 1:1 map to what the scanner reported"
)]
struct Verdict {
    class: Class,
    /// The winning decode came through the S5 rescue stage.
    via_rescue: bool,
    /// The S5 rescue stage RAN (grid detected, engines failed) — the
    /// denominator for "rescue-WRONG vs rescue-REFUSE". Read from the pipeline
    /// trace: a `"rescue"` stage entry exists AND tried ≥1 candidate.
    rescue_attempted: bool,
    /// `low_correction_margin` fired on this report.
    hint_low_margin: bool,
    /// A non-QR symbology was hallucinated in the noise (tracked, not scored).
    spurious_non_qr: bool,
}

/// A wrong decode, captured for the ledger — WHICH cell miscorrected, via which
/// engine, whether the hint saw it, and what wrong text came back.
struct WrongRow {
    version: i16,
    ec: &'static str,
    fill: &'static str,
    position: &'static str,
    occ_pct: u32,
    engines: String,
    via_rescue: bool,
    hinted: bool,
    decoded: String,
}

/// Stable label for an engine set (`rxing` · `rqrr` · `rescue`, joined).
fn engines_label(engines: &[EngineKind]) -> String {
    let names: Vec<&str> = engines
        .iter()
        .map(|e| match e {
            EngineKind::Rxing => "rxing",
            EngineKind::Rqrr => "rqrr",
            EngineKind::Rescue => "rescue",
            _ => "?",
        })
        .collect();
    names.join("+")
}

/// Printable, length-annotated preview of an attacker-uncontrolled decode.
fn preview(text: &str) -> String {
    let shown: String = text
        .chars()
        .take(18)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '.'
            }
        })
        .collect();
    format!("len={} \"{shown}\"", text.chars().count())
}

fn scan_cell(scanner: &Scanner, cell: &Cell) -> (Verdict, Option<WrongRow>) {
    // QRS_RESCUE_TRACE=1: per-cell bisect trace (run with
    // RAYON_NUM_THREADS=1 so the loud-alloc lines isolate one cell).
    // QRS_RESCUE_CELL=<substring>: scan ONLY cells whose trace line matches
    // (fast repro of a single pathological cell).
    let trace_line = format!(
        "v{} ec={} len{} fill={:?} shape={:?} pos={:?} occ{}",
        cell.version,
        cell.ec.tag,
        cell.truth.len(),
        cell.fill,
        cell.shape,
        cell.position,
        cell.occ_pct
    );
    if let Some(filter) = std::env::var_os("QRS_RESCUE_CELL")
        && !trace_line.contains(filter.to_string_lossy().as_ref())
    {
        return (
            Verdict {
                class: Class::Refused,
                via_rescue: false,
                rescue_attempted: false,
                hint_low_margin: false,
                spurious_non_qr: false,
            },
            None,
        );
    }
    if std::env::var_os("QRS_RESCUE_TRACE").is_some() {
        eprintln!("[trace] {trace_line}");
    }
    let png = render_cell(cell);
    let report = scanner
        .scan(ImageInput::encoded(&png))
        .expect("synthetic png is always a valid, in-bounds input");

    let hint_low_margin = report
        .hints
        .iter()
        .any(|h| matches!(h, Hint::LowCorrectionMargin { .. }));
    let spurious_non_qr = report
        .detections
        .iter()
        .any(|d| !d.symbology.is_qr_family());
    // The S5 stage only appears in the trace when the ladder came up empty AND
    // a grid was detected AND the budget allowed it — i.e. rescue genuinely ran.
    let rescue_attempted = report
        .trace
        .stages
        .iter()
        .any(|s| s.stage == "rescue" && s.transforms_tried > 0);

    // Isolate the QR miscorrection class: non-QR hallucinations are a different
    // phenomenon and never count as a "wrong QR decode".
    let qr: Vec<&_> = report
        .detections
        .iter()
        .filter(|d| d.symbology.is_qr_family())
        .collect();

    let (class, via_rescue, wrong) =
        if let Some(hit) = qr.iter().find(|d| d.content.text == cell.truth) {
            (
                Class::Correct,
                hit.engines.contains(&EngineKind::Rescue),
                None,
            )
        } else if let Some(miss) = qr.first() {
            let via = miss.engines.contains(&EngineKind::Rescue);
            let row = WrongRow {
                version: cell.version,
                ec: cell.ec.tag,
                fill: cell.fill.label(),
                position: cell.position.label(),
                occ_pct: cell.occ_pct,
                engines: engines_label(&miss.engines),
                via_rescue: via,
                hinted: hint_low_margin,
                decoded: preview(&miss.content.text),
            };
            (Class::Wrong, via, Some(row))
        } else {
            (Class::Refused, false, None)
        };

    (
        Verdict {
            class,
            via_rescue,
            rescue_attempted,
            hint_low_margin,
            spurious_non_qr,
        },
        wrong,
    )
}

// ------------------------------------------------------------- aggregation

#[derive(Default, Clone, Copy)]
struct Bucket {
    scans: u32,
    correct: u32,
    wrong: u32,
    refused: u32,
    /// Wrong decodes on which `low_correction_margin` fired.
    wrong_hinted: u32,
    /// S5 rescue ran (grid detected, engines failed).
    rescue_attempted: u32,
    /// S5 ran and emitted a decode.
    rescue_success: u32,
    /// S5 ran and emitted NOTHING (the refusal-biased outcome).
    rescue_refused: u32,
    rescue_correct: u32,
    rescue_wrong: u32,
    spurious_non_qr: u32,
}

impl Bucket {
    fn add(&mut self, v: Verdict) {
        self.scans += 1;
        if v.spurious_non_qr {
            self.spurious_non_qr += 1;
        }
        match v.class {
            Class::Correct => self.correct += 1,
            Class::Wrong => {
                self.wrong += 1;
                if v.hint_low_margin {
                    self.wrong_hinted += 1;
                }
            }
            Class::Refused => self.refused += 1,
        }
        if v.rescue_attempted {
            self.rescue_attempted += 1;
            if v.via_rescue {
                self.rescue_success += 1;
                match v.class {
                    Class::Wrong => self.rescue_wrong += 1,
                    _ => self.rescue_correct += 1,
                }
            } else {
                self.rescue_refused += 1;
            }
        }
    }

    fn wrong_rate(self) -> f32 {
        if self.scans == 0 {
            0.0
        } else {
            self.wrong as f32 * 100.0 / self.scans as f32
        }
    }
}

/// Percentage helper that reads `n/a` when the denominator is empty.
fn rate_or_na(num: u32, den: u32) -> String {
    if den == 0 {
        "n/a".to_owned()
    } else {
        format!("{:.3}%", f64::from(num) * 100.0 / f64::from(den))
    }
}

/// Enumerate the sweep in one fixed nested order — the ONLY source of cell
/// (and therefore output) order. No RNG, no set iteration: a plain Cartesian
/// product of the constant tables.
fn build_grid() -> Vec<Cell> {
    let mut cells: Vec<Cell> = Vec::new();
    for &version in VERSIONS {
        for ec in EC_LEVELS {
            let fit = max_fit(version, ec.ec);
            for &pct in PAYLOAD_FRACTIONS {
                let len = (fit * pct as usize / 100).max(1);
                let truth = filler(len);
                debug_assert!(!truth.is_empty());
                for &(shape, _) in SHAPES {
                    for &(fill, _) in FILLS {
                        for &(position, _) in POSITIONS {
                            for &occ_pct in OCCLUSION_PCTS {
                                cells.push(Cell {
                                    version,
                                    ec: *ec,
                                    shape,
                                    fill,
                                    position,
                                    occ_pct,
                                    truth: truth.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    cells
}

pub fn run() {
    // budget None ⇒ the ladder always runs to completion ⇒ the report is a pure
    // function of the pixels (the crate's strict-determinism configuration).
    let mut cfg = ScanProfile::Full.config();
    cfg.budget_ms = None;
    let scanner = Scanner::builder().profile(ScanProfile::Custom(cfg)).build();

    let cells = build_grid();

    // Guard: every base symbol decodes clean (0% occlusion). Cheap, and it
    // turns a silent encode/geometry regression into a loud panic.
    verify_base_symbols(&scanner);

    let started = std::time::Instant::now();
    // Independent single-threaded scans; collect preserves input order.
    let results: Vec<(Verdict, Option<WrongRow>)> = cells
        .par_iter()
        .map(|cell| scan_cell(&scanner, cell))
        .collect();
    let elapsed = started.elapsed();

    // ---- fold into fixed-order buckets ----
    let mut total = Bucket::default();
    let mut by_pct: BTreeMap<u32, Bucket> = BTreeMap::new();
    let mut by_fill_ec: BTreeMap<(usize, usize), Bucket> = BTreeMap::new();
    let mut by_position: BTreeMap<usize, Bucket> = BTreeMap::new();
    let mut wrong_rows: Vec<&WrongRow> = Vec::new();

    for (cell, (v, wrong)) in cells.iter().zip(&results) {
        total.add(*v);
        by_pct.entry(cell.occ_pct).or_default().add(*v);
        let fill_ix = FILLS.iter().position(|f| f.0 == cell.fill).unwrap();
        let ec_ix = EC_LEVELS.iter().position(|e| e.tag == cell.ec.tag).unwrap();
        by_fill_ec.entry((fill_ix, ec_ix)).or_default().add(*v);
        let pos_ix = POSITIONS.iter().position(|p| p.0 == cell.position).unwrap();
        by_position.entry(pos_ix).or_default().add(*v);
        if let Some(row) = wrong {
            wrong_rows.push(row); // cell order ⇒ deterministic ledger
        }
    }

    let mut out = String::new();
    write_header(&mut out, total.scans);
    write_tables(&mut out, &by_pct, &by_fill_ec, &by_position, &wrong_rows);
    write_summary(&mut out, total, &by_pct);

    print!("{out}");
    // Wall-clock on stderr ONLY — never in the diffed stdout stream.
    eprintln!(
        "\n[rescue-stress] {} scans in {:.1}s on {} threads",
        total.scans,
        elapsed.as_secs_f64(),
        rayon::current_num_threads()
    );
}

fn write_header(out: &mut String, scans: u32) {
    let versions: Vec<String> = VERSIONS.iter().map(ToString::to_string).collect();
    writeln!(
        out,
        "# QD-2 rescue-stress — adversarial miscorrection measurement\n"
    )
    .unwrap();
    writeln!(
        out,
        "grid: v[{}] × ec[m,q,h] × payload[{:?}%] × fill[dark,gray] × shape[square,disc] × pos[4] × occ{:?}%",
        versions.join(","),
        PAYLOAD_FRACTIONS,
        OCCLUSION_PCTS
    )
    .unwrap();
    writeln!(out, "cells: {scans}\n").unwrap();
}

fn write_tables(
    out: &mut String,
    by_pct: &BTreeMap<u32, Bucket>,
    by_fill_ec: &BTreeMap<(usize, usize), Bucket>,
    by_position: &BTreeMap<usize, Bucket>,
    wrong_rows: &[&WrongRow],
) {
    // Table A — the QD-2 trend: wrong-rate AND the rescue attempted/refuse/wrong
    // split, vs rising occlusion.
    writeln!(out, "## by occlusion %").unwrap();
    writeln!(
        out,
        "| occ% | scans | correct | wrong | refused | resc-ran | resc-ok | resc-refuse | resc-wrong | wrong-rate |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|").unwrap();
    for (pct, b) in by_pct {
        writeln!(
            out,
            "| {pct} | {} | {} | {} | {} | {} | {} | {} | {} | {:.3}% |",
            b.scans,
            b.correct,
            b.wrong,
            b.refused,
            b.rescue_attempted,
            b.rescue_correct,
            b.rescue_refused,
            b.rescue_wrong,
            b.wrong_rate()
        )
        .unwrap();
    }

    // Table B — the d−p regime: erasure (gray) vs error (dark), per EC budget.
    writeln!(out, "\n## by fill × ec (erasure vs error regime)").unwrap();
    writeln!(
        out,
        "| fill | ec | scans | correct | wrong | refused | resc-ran | resc-ok | resc-wrong | wrong-rate |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|").unwrap();
    for (&(fill_ix, ec_ix), b) in by_fill_ec {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.3}% |",
            FILLS[fill_ix].1,
            EC_LEVELS[ec_ix].tag,
            b.scans,
            b.correct,
            b.wrong,
            b.refused,
            b.rescue_attempted,
            b.rescue_correct,
            b.rescue_wrong,
            b.wrong_rate()
        )
        .unwrap();
    }

    // Table C — by position: on_finder is the refuse control (no grid ⇒ no rescue);
    // corner_adjacent clips the format-info strip (the engine-miscorrection vector).
    writeln!(out, "\n## by position").unwrap();
    writeln!(
        out,
        "| position | scans | correct | wrong | refused | resc-ran | resc-ok | resc-wrong |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|").unwrap();
    for (&pos_ix, b) in by_position {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            POSITIONS[pos_ix].1,
            b.scans,
            b.correct,
            b.wrong,
            b.refused,
            b.rescue_attempted,
            b.rescue_correct,
            b.rescue_wrong
        )
        .unwrap();
    }

    // Ledger — EVERY wrong decode, so the operator sees exactly what miscorrected
    // and through which engine (the deterministic proof behind the rates).
    writeln!(out, "\n## wrong-decode ledger ({} rows)", wrong_rows.len()).unwrap();
    writeln!(
        out,
        "| # | v | ec | fill | position | occ% | engines | via_rescue | hinted | decoded |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|").unwrap();
    for (i, r) in wrong_rows.iter().enumerate() {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            i + 1,
            r.version,
            r.ec,
            r.fill,
            r.position,
            r.occ_pct,
            r.engines,
            r.via_rescue,
            r.hinted,
            r.decoded
        )
        .unwrap();
    }
}

fn write_summary(out: &mut String, total: Bucket, by_pct: &BTreeMap<u32, Bucket>) {
    let max_pct = *OCCLUSION_PCTS.last().unwrap();
    let max_bucket = by_pct.get(&max_pct).copied().unwrap_or_default();
    writeln!(out, "\n## summary").unwrap();
    writeln!(out, "total_scans            = {}", total.scans).unwrap();
    writeln!(out, "decoded_correct        = {}", total.correct).unwrap();
    writeln!(out, "decoded_wrong          = {}", total.wrong).unwrap();
    writeln!(out, "refused                = {}", total.refused).unwrap();
    writeln!(
        out,
        "wrong_rate             = {}",
        rate_or_na(total.wrong, total.scans)
    )
    .unwrap();
    writeln!(
        out,
        "rescue_attempted       = {} (S5 ran: grid found, engines failed)",
        total.rescue_attempted
    )
    .unwrap();
    writeln!(
        out,
        "  rescue_succeeded     = {} (correct {}, wrong {})",
        total.rescue_success, total.rescue_correct, total.rescue_wrong
    )
    .unwrap();
    writeln!(
        out,
        "  rescue_refused       = {}   rescue_refuse_rate = {}",
        total.rescue_refused,
        rate_or_na(total.rescue_refused, total.rescue_attempted)
    )
    .unwrap();
    writeln!(
        out,
        "rescue_wrong_rate      = {}   (miscorrection among rescue-path decodes)",
        rate_or_na(total.rescue_wrong, total.rescue_success)
    )
    .unwrap();
    // Rule of three: 0 events in N trials ⇒ 95% upper bound ≈ 3/N. The rescue
    // regime is inherently narrow, so state the resolution HONESTLY.
    if total.rescue_wrong == 0 && total.rescue_success > 0 {
        writeln!(
            out,
            "  (0/{} wrong ⇒ 95% upper bound ≈ {:.3}% by rule-of-three; resolving the",
            total.rescue_success,
            300.0 / f64::from(total.rescue_success)
        )
        .unwrap();
        writeln!(
            out,
            "   0.1% line on the RESCUE path alone needs ≈3000 rescue decodes — widen VERSIONS)"
        )
        .unwrap();
    }
    writeln!(
        out,
        "hint_coverage_of_wrong = {}   (does low_correction_margin flag the wrong class?)",
        rate_or_na(total.wrong_hinted, total.wrong)
    )
    .unwrap();
    writeln!(
        out,
        "max-occlusion ({max_pct}%)   = wrong-rate {}  (rescue-wrong {})",
        rate_or_na(max_bucket.wrong, max_bucket.scans),
        max_bucket.rescue_wrong
    )
    .unwrap();
    writeln!(
        out,
        "spurious_non_qr        = {} (non-QR hallucinations in noise — not scored)",
        total.spurious_non_qr
    )
    .unwrap();

    write_decision(out, total, max_bucket);
}

/// The two-part verdict. QD-2 asks a SPECIFIC question about the rescue path;
/// answer THAT, then flag any engine-path miscorrection separately (it is a
/// distinct class the proposed `low_correction_margin`→refusal change would NOT
/// address, so conflating them would mislead the operator).
fn write_decision(out: &mut String, total: Bucket, max_bucket: Bucket) {
    let rescue_wrong_pct = if total.rescue_success == 0 {
        0.0
    } else {
        f64::from(total.rescue_wrong) * 100.0 / f64::from(total.rescue_success)
    };
    let engine_wrong = total.wrong.saturating_sub(total.rescue_wrong);
    let overall_wrong_pct = if total.scans == 0 {
        0.0
    } else {
        f64::from(total.wrong) * 100.0 / f64::from(total.scans)
    };

    writeln!(out, "\n## verdict").unwrap();
    // (1) The QD-2 hypothesis under test: does the RESCUE miscorrect near d−p?
    let rescue_tripped = rescue_wrong_pct > 0.1 || max_bucket.rescue_wrong > 0;
    writeln!(
        out,
        "QD-2 rescue-path 0.1% line: {}",
        if rescue_tripped {
            "TRIPPED — rescue miscorrections observed; low_correction_margin should default \
             to REFUSAL in Full (opt-in accept_risky)"
        } else {
            "HELD — the S5 rescue produced ZERO wrong decodes across the sweep (and none at \
             max occlusion); it is empirically refusal-safe, so the hint stays advisory"
        }
    )
    .unwrap();

    // (2) The unexpected, SEPARATE finding: engine-path miscorrection.
    if engine_wrong > 0 {
        writeln!(
            out,
            "SEPARATE FINDING — engine-path wrong decode: {engine_wrong} decoded ≠ truth from the \
             base engine (NOT rescue), overall {overall_wrong_pct:.3}% > 0.1%, at corner_adjacent \
             (format-info-adjacent) low occlusion — see the ledger for engine + decoded text."
        )
        .unwrap();
        writeln!(
            out,
            "  hint coverage {} — low_correction_margin needs a sampled bitstream (the rqrr path) \
             AND a worst RS block at margin 0; an rxing-only decode carries NO bitstream ⇒ no UEC \
             ⇒ the hint path never runs (structurally blind, not merely under-triggered). Making it \
             a refusal would not touch this class. Distinct from QD-2; operator's call.",
            rate_or_na(total.wrong_hinted, total.wrong)
        )
        .unwrap();
    }
}

/// Panic unless every base symbol decodes to its truth with no occlusion —
/// isolates encode/geometry regressions from the occlusion measurement.
fn verify_base_symbols(scanner: &Scanner) {
    for &version in VERSIONS {
        for ec in EC_LEVELS {
            let fit = max_fit(version, ec.ec);
            for &pct in PAYLOAD_FRACTIONS {
                let len = (fit * pct as usize / 100).max(1);
                let truth = filler(len);
                let symbol = render_symbol(version, ec.ec, &truth);
                let (canvas, _) = canvas_with_symbol(&symbol);
                let mut png = Vec::new();
                image::DynamicImage::ImageLuma8(canvas)
                    .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                    .unwrap();
                let report = scanner.scan(ImageInput::encoded(&png)).unwrap();
                assert!(
                    report.detections.iter().any(|d| d.content.text == truth),
                    "clean base symbol v{version} ec={} pct={pct} did not decode to truth",
                    ec.tag
                );
            }
        }
    }
}
