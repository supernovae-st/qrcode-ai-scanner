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
//!   cycled), sized per `(version, ec)` by a descending max-fit search — same
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
//!
//! # The capped pass (engine-cap coverage)
//!
//! Every render is at most 552 px (v10: 57 modules + 12 quiet ⇒ 69 · 8), so
//! the Full profile's 2048 px engine cap never fires and the S5 rescue's
//! capped branch — candidate geometry mapped into the downscaled sampling
//! plane — would go unmeasured. A second pass therefore scans the SAME grid
//! with a configuration that differs only by `max_engine_side = 400`. The v2
//! canvases (296 px) stay uncapped, while v6 (424 px) reach the engines at
//! ×0.943 and v10 (552 px) at ×0.725: non-integer ratios.
//!
//! The zero-wrong gate covers both passes: the process exits 1 when either
//! pass reports a rescue-path wrong decode. The uncapped summary keeps its
//! `rescue_succeeded … (correct C, wrong W)` line byte for byte (the line the
//! deep-checks workflow greps). The capped pass reports
//! `capped_rescue_decodes … (correct C, wrong W)` instead, a name that grep
//! can never match.
//!
//! # The dual-occluder pass (both regimes in one symbol)
//!
//! Each grid cell carries one regime, but erasure-assisted RS is weakest
//! where erasures and confident errors meet in ONE symbol: the erasures spend
//! the parity a decoder needs to see the errors. A third pass rescans every
//! gray cell of the uncapped grid with one dark blot added: a square of
//! `BLOT_MODULES` whole modules inside the data region, outside the gray
//! occluder and the function patterns, over 2-3 codewords that no gray pixel
//! touches. Those are data codewords, except on the cells whose occluder
//! leaves no such data-codeword position, where they are EC codewords. The
//! blot position is a pure function of the cell index. The pass prints its
//! own `dual_` lines, none of which the deep-checks greps can match.
//!
//! # The output gate (every published detection, every path)
//!
//! The tables above judge one QR detection per cell. The output gate counts
//! them all: every published detection of a cell is one output. The cell's
//! truth is its first `qr_code` output with the truth text (the grid renders
//! Model 2 QR only); every other output is a wrong output of one kind:
//!
//! - `qr_wrong` — any other QR-family engine read, in a cell with no truth
//!   output;
//! - `qr_extra` — the same beside a truth output (the tables still count
//!   that cell correct);
//! - `rescue_qr` — any other QR-family read through the S5 rescue;
//! - `non_qr` — any other symbology: every cell renders one QR symbol only.
//!
//! An output's path is `rescue` when its engines include the rescue, else
//! `engine`. `strict_correct` counts the cells whose outputs are exactly one
//! truth. `DISPOSITIONS_PATH` (compiled in) holds one row per tolerated wrong
//! output, with a public reason, and pins each pass's exact `strict_correct`
//! (its floor), its exact `engine_panics` sum and its exact `caught_panics`
//! count. A row holds only while its exact output occurs; a row whose output
//! no longer occurs is stale.
//!
//! The process exits 1 when any pass has an open output (no row), a stale
//! row, or a `strict_correct`, `engine_panics` or `caught_panics` off its pin
//! in either direction (a gain re-pins in its own change); when the uncapped
//! or the capped pass has a rescue-path wrong decode, or any rescue-path wrong
//! output, held or not; or when either of them has no rescue decode at all
//! (its zero-wrong line would measure nothing). Only the dual pass may hold a
//! rescue-path wrong output in a row. Every line the passes printed before the
//! gate existed is printed unchanged, first; the gate appends its own.

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
use std::io::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};

use image::{GrayImage, Luma};
use qrcode::canvas::{Canvas, MaskPattern, Module};
use qrcode_ai_scanner::{
    Detection, EngineKind, Hint, ImageInput, ScanProfile, ScanReport, Scanner,
};
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

/// Alphabet cycled to build ground-truth payloads. `qrcode`'s optimal
/// segmentation does NOT encode it in byte mode: it splits the filler into
/// alphanumeric, numeric and byte runs (the 120-byte v6-M truth is
/// A8 N10 B26 A12 N10 B26 A12 N10 B6). Every character is ASCII, so the
/// decoded text still equals the truth byte for byte.
const FILLER_ALPHABET: &[u8] = b"QRCODEAI0123456789abcdefghijklmnopqrstuvwxyz-./:";

/// Upper bound for the max-fit search — above every v2..v10 capacity the grid
/// sweeps (the largest, v10-M, fits 230 filler characters), so the monotone
/// decrement always finds the true knee.
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

impl Shape {
    fn label(self) -> &'static str {
        match self {
            Self::Square => "square",
            Self::Disc => "disc",
        }
    }
}

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
    /// Below and left of the top-right finder. The occluder overlaps that
    /// finder's lower rows in every v2 cell, in v6 from 10 % and in v10 from
    /// 15 % occlusion (all of it at 40 %), and reaches ISO row 8 beside it in
    /// every cell but v10 at 5 %: it clips that part of one format-information
    /// copy, while the copy beside the top-left finder stays intact. The wrong
    /// decodes measured at this anchor (10-15 % occlusion) were a publication
    /// defect, not a format miscorrection: rxing's `ZXing`-lineage reader read
    /// the format exactly and decoded the truth, and the scanner published
    /// only its byte-mode segments (`BYTE_SEGMENTS`).
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
    /// Position in `build_grid` order: the cell's identity in the ledger.
    index: usize,
    version: i16,
    ec: EcCell,
    /// The `PAYLOAD_FRACTIONS` entry the truth was sized with.
    payload_pct: u32,
    shape: Shape,
    fill: Fill,
    position: Position,
    occ_pct: u32,
    truth: String,
    /// The dual pass's dark blot; `None` in the uncapped and capped passes.
    blot: Option<Blot>,
    /// A hand-built damaged symbol that overrides the procedural render; set
    /// only on the dual pass's constructed S5 cells (`None` everywhere else).
    constructed: Option<Constructed>,
}

/// A symbol damaged at the codeword level, rendered once into a gray image.
/// It drives the S5 rescue down a path the procedural occlusion does not
/// reach, so the gate can see the rescue-miscorrection class it guards.
#[derive(Clone)]
struct Constructed {
    /// The damaged render (quiet zone included).
    image: GrayImage,
    /// Short label stem, e.g. `s5-v2m-solved`.
    name: &'static str,
}

/// Stable cell label for the ledger, e.g.
/// `0056_v2-m-p100-square-gray-corner_adjacent-o5`; a dual cell appends its
/// blot's top-left module (`-blot_c12_r9`); a constructed cell is
/// `<index>_<name>`.
fn cell_label(cell: &Cell) -> String {
    if let Some(constructed) = &cell.constructed {
        return format!("{:04}_{}", cell.index, constructed.name);
    }
    let mut label = format!(
        "{:04}_v{}-{}-p{}-{}-{}-{}-o{}",
        cell.index,
        cell.version,
        cell.ec.tag,
        cell.payload_pct,
        cell.shape.label(),
        cell.fill.label(),
        cell.position.label(),
        cell.occ_pct
    );
    if let Some(blot) = cell.blot {
        write!(label, "-blot_c{}_r{}", blot.col, blot.row).unwrap();
    }
    label
}

// ------------------------------------------------------------ encode / occlude

/// Deterministic filler prefix of `len` bytes (ASCII ⇒ byte == char).
fn filler(len: usize) -> String {
    (0..len)
        .map(|i| FILLER_ALPHABET[i % FILLER_ALPHABET.len()] as char)
        .collect()
}

/// Largest filler length that still encodes at `(version, ec)`. The search
/// walks down from `FILLER_MAX`, above every capacity swept, so it returns the
/// largest length that fits whatever segment mix the encoder picks — the same
/// length every run.
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

/// Paint a dual cell's blot: whole modules at luma 0, confident dark.
fn paint_blot(canvas: &mut GrayImage, blot: Blot, origin: u32) {
    let side = BLOT_MODULES as u32 * MODULE_PX;
    let (x0, y0) = (
        origin + blot.col as u32 * MODULE_PX,
        origin + blot.row as u32 * MODULE_PX,
    );
    for y in y0..y0 + side {
        for x in x0..x0 + side {
            canvas.put_pixel(x, y, Luma([0]));
        }
    }
}

/// The occluded canvas of a cell (its blot included), or the hand-built
/// damaged render of a constructed cell.
fn render_canvas(cell: &Cell) -> GrayImage {
    if let Some(constructed) = &cell.constructed {
        return constructed.image.clone();
    }
    let symbol = render_symbol(cell.version, cell.ec.ec, &cell.truth);
    let symbol_side = symbol.width() as f32;
    let (mut canvas, origin) = canvas_with_symbol(&symbol);
    paint_occlusion(&mut canvas, cell, symbol_side, origin);
    if let Some(blot) = cell.blot {
        paint_blot(&mut canvas, blot, origin);
    }
    canvas
}

/// Build the occluded PNG bytes for a cell.
fn render_cell(cell: &Cell) -> Vec<u8> {
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(render_canvas(cell))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode occluded png");
    png
}

// --------------------------------------------------------------- dual pass

/// Side of the dual pass's square blot, in modules.
const BLOT_MODULES: usize = 3;

/// Stride through a cell's candidate blot positions: neighbouring cells (the
/// occlusion steps of one geometry) place their blots apart, with no RNG.
const BLOT_STRIDE: usize = 97;

/// The dual pass's dark blot: a `BLOT_MODULES` square of whole modules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Blot {
    /// Top-left module column.
    col: usize,
    /// Top-left module row.
    row: usize,
    /// Its codewords are data codewords. `false` when the occluder leaves no
    /// valid data-codeword position: the blot then sits on EC codewords.
    on_data: bool,
}

/// Which codeword of the interleaved stream each module of one version
/// carries, read off the renderer itself: `qrcode`'s canvas places a stream
/// whose only set codeword is `k`, and the modules that turn dark are `k`'s.
struct CodewordMap {
    /// Modules per side.
    side: usize,
    /// Row-major; `None` for function patterns and remainder bits.
    owner: Vec<Option<usize>>,
    /// Data plus EC codewords.
    codewords: usize,
}

fn codeword_map(version: i16) -> CodewordMap {
    let qr_version = qrcode::Version::Normal(version);
    let side = usize::try_from(qr_version.width()).unwrap();
    let at = |canvas: &Canvas, m: usize| {
        canvas.get(
            i16::try_from(m % side).unwrap(),
            i16::try_from(m / side).unwrap(),
        )
    };
    // The EC level only matters for Micro QR's half codeword: placement is
    // a function of the version.
    let mut functional = Canvas::new(qr_version, qrcode::EcLevel::L);
    functional.draw_all_functional_patterns();
    let free = (0..side * side)
        .filter(|&m| at(&functional, m) == Module::Empty)
        .count();
    let codewords = free / 8; // the rest are remainder bits
    let mut owner = vec![None; side * side];
    for k in 0..codewords {
        let mut stream = vec![0u8; codewords];
        stream[k] = 0xFF;
        let mut canvas = functional.clone();
        canvas.draw_data(&stream, &[]);
        for (m, slot) in owner.iter_mut().enumerate() {
            if at(&canvas, m) == Module::Unmasked(qrcode::Color::Dark) {
                *slot = Some(k);
            }
        }
    }
    CodewordMap {
        side,
        owner,
        codewords,
    }
}

/// Data codewords of `(version, ec)`, from the encoder's own capacity table.
fn data_codewords(version: i16, ec: qrcode::EcLevel) -> usize {
    qrcode::bits::Bits::new(qrcode::Version::Normal(version))
        .max_len(ec)
        .unwrap()
        / 8
}

/// Modules (row-major over `side`) of which the cell's occluder paints at
/// least one pixel. `paint_occlusion` itself paints a blank canvas, so the
/// test is exactly the rendered geometry.
fn occluded_modules(cell: &Cell, side: usize) -> Vec<bool> {
    let symbol_px = side as u32 * MODULE_PX;
    let qz = QUIET_MODULES * MODULE_PX;
    let mut blank = GrayImage::from_pixel(symbol_px + 2 * qz, symbol_px + 2 * qz, Luma([255]));
    paint_occlusion(&mut blank, cell, symbol_px as f32, qz);
    let mut occluded = vec![false; side * side];
    let inside = qz..qz + symbol_px;
    for (x, y, px) in blank.enumerate_pixels() {
        if px.0[0] != 255 && inside.contains(&x) && inside.contains(&y) {
            let (mx, my) = ((x - qz) / MODULE_PX, (y - qz) / MODULE_PX);
            occluded[my as usize * side + mx as usize] = true;
        }
    }
    occluded
}

/// A cell's blot, a pure function of the cell (its index and its geometry):
/// the `index · BLOT_STRIDE`-th valid position in row-major order. Valid
/// means every module carries a codeword no occluder pixel touches (a
/// confident error, never an erasure) and the square touches 2-3 distinct
/// codewords. Data codewords first; EC codewords only when the occluder leaves
/// no valid data-codeword position.
fn place_blot(cell: &Cell, map: &CodewordMap) -> Option<Blot> {
    let side = map.side;
    let mut touched = vec![false; map.codewords];
    for (m, occluded) in occluded_modules(cell, side).into_iter().enumerate() {
        if occluded && let Some(k) = map.owner[m] {
            touched[k] = true;
        }
    }
    let data = data_codewords(cell.version, cell.ec.ec);
    let candidates = |on_data: bool| {
        let mut found = Vec::new();
        for row in 0..=side - BLOT_MODULES {
            'position: for col in 0..=side - BLOT_MODULES {
                let mut codewords: Vec<usize> = Vec::new();
                for y in row..row + BLOT_MODULES {
                    for x in col..col + BLOT_MODULES {
                        match map.owner[y * side + x] {
                            Some(k) if !touched[k] && (!on_data || k < data) => {
                                if !codewords.contains(&k) {
                                    codewords.push(k);
                                }
                            }
                            _ => continue 'position,
                        }
                    }
                }
                if (2..=3).contains(&codewords.len()) {
                    found.push(Blot { col, row, on_data });
                }
            }
        }
        found
    };
    let mut found = candidates(true);
    if found.is_empty() {
        found = candidates(false);
    }
    (!found.is_empty()).then(|| found[(cell.index * BLOT_STRIDE) % found.len()])
}

/// The dual pass's cells: every gray cell of `cells`, in order, with its
/// blot, then the constructed S5 cells (`build_s5_constructed`). The blotted
/// cells and the constructed cells are the two ways this pass drives mixed
/// erasure/error damage into one symbol.
fn build_dual_grid(cells: &[Cell]) -> Vec<Cell> {
    let maps: BTreeMap<i16, CodewordMap> = VERSIONS.iter().map(|&v| (v, codeword_map(v))).collect();
    let mut dual: Vec<Cell> = cells
        .iter()
        .filter(|cell| cell.fill == Fill::Gray)
        .map(|cell| Cell {
            blot: Some(
                place_blot(cell, &maps[&cell.version])
                    .expect("every gray cell leaves a valid blot position (unit-tested)"),
            ),
            ..cell.clone()
        })
        .collect();
    dual.extend(build_s5_constructed());
    dual
}

// --------------------------------------------------- constructed S5 cells

/// The truth payload of the constructed S5 cell — a neutral synthetic text.
/// The damage drives the rescue toward [`S5_NEAR`]: with today's erasure
/// budget of `npar − p − 1` codewords, the S5 rescue corrects the block onto
/// it at margin 0 and publishes a wrong payload. An erasure floor keeping two
/// spare parity codewords per block refuses it.
const S5_TRUTH: &str = "QRSCAN-FIXTURE-CASE-000009";

/// The near payload the construction overwrites two codewords with. It and
/// [`S5_TRUTH`] share every byte and the high nibble of the last one, so their
/// v2-M codewords differ only from the last data codeword on (it carries that
/// byte's low nibble and the terminator).
const S5_NEAR: &str = "QRSCAN-FIXTURE-CASE-000001";

/// The index the constructed cells start at — far above the `2304` grid, so
/// their 4-digit ledger labels never collide with a grid cell's.
const S5_CELL_BASE: usize = 9000;

/// The 44 interleaved codewords (28 data + 16 EC) of a byte-mode `payload`
/// at v2-M. v2 is a single RS block, so the interleaved order is just data
/// then EC.
fn v2m_codewords(payload: &str) -> Vec<u8> {
    let mut bits = qrcode::bits::Bits::new(qrcode::Version::Normal(2));
    bits.push_byte_data(payload.as_bytes())
        .expect("payload fits v2-M byte mode");
    bits.push_terminator(qrcode::EcLevel::M)
        .expect("terminator and padding fit");
    let data = bits.into_bytes();
    let (data_i, ec_i) =
        qrcode::ec::construct_codewords(&data, qrcode::Version::Normal(2), qrcode::EcLevel::M)
            .expect("v2-M codewords");
    [data_i, ec_i].concat()
}

/// Render a v2-M symbol from explicit `data` and `ec` codewords under `mask`,
/// then repaint the modules of the `gray` codeword indices to [`GRAY_LUMA`]
/// so the scanner reads them as low-confidence erasures. Everything else is a
/// confident module, so the overwritten codewords read as confident errors.
fn render_damaged_v2m(data: &[u8], ec: &[u8], mask: MaskPattern, gray: &[usize]) -> GrayImage {
    let mut canvas = Canvas::new(qrcode::Version::Normal(2), qrcode::EcLevel::M);
    canvas.draw_all_functional_patterns();
    canvas.draw_data(data, ec);
    canvas.apply_mask(mask);
    let map = codeword_map(2);
    let side = map.side;
    let colors = canvas.into_colors(); // row-major, after masking + format info
    let gray_module = |m: usize| map.owner[m].is_some_and(|k| gray.contains(&k));
    let qz = QUIET_MODULES * MODULE_PX;
    let px = side as u32 * MODULE_PX;
    let mut img = GrayImage::from_pixel(px + 2 * qz, px + 2 * qz, Luma([255]));
    for y in 0..side {
        for x in 0..side {
            let m = y * side + x;
            let luma = if gray_module(m) {
                GRAY_LUMA
            } else if colors[m] == qrcode::Color::Dark {
                0
            } else {
                255
            };
            for dy in 0..MODULE_PX {
                for dx in 0..MODULE_PX {
                    img.put_pixel(
                        qz + x as u32 * MODULE_PX + dx,
                        qz + y as u32 * MODULE_PX + dy,
                        Luma([luma]),
                    );
                }
            }
        }
    }
    img
}

/// A constructed margin-0 erasure+error case, rebuilt in Rust from its
/// parameters (no saved image): a v2-M symbol of [`S5_TRUTH`] whose last data
/// codeword and last EC codeword carry [`S5_NEAR`]'s values and whose first 14
/// EC codewords are grayed. An errors-only engine sees 16 wrong codewords
/// (over the t=8 budget) and refuses; the S5 rescue marks the 14 gray
/// codewords as erasures, within today's erasure budget `npar − p − 1`, finds
/// the one unmarked error left against [`S5_NEAR`] (`2·1 + 14 = npar`, margin
/// 0) and corrects onto that different valid payload. Returns no cell if the
/// encoder's codewords no longer place the difference where the construction
/// needs it (its ledger row then goes stale).
fn build_s5_constructed() -> Vec<Cell> {
    const NDATA: usize = 28;
    const NPAR: usize = 16;
    let truth = v2m_codewords(S5_TRUTH);
    let near = v2m_codewords(S5_NEAR);
    // The construction needs D0..D26 shared and the difference to start at the
    // last data codeword D27; otherwise the grayed EC set would not leave the
    // single phantom error the margin-0 case depends on.
    let first_diff = (0..truth.len()).find(|&i| truth[i] != near[i]);
    if first_diff != Some(NDATA - 1) {
        return Vec::new();
    }
    let mut data = truth[..NDATA].to_vec();
    let mut ec = truth[NDATA..].to_vec();
    data[NDATA - 1] = near[NDATA - 1]; // D27 ← near (a confident error)
    ec[NPAR - 1] = near[NDATA + NPAR - 1]; // E15 ← near (a confident error)
    let gray: Vec<usize> = (NDATA..NDATA + 14).collect(); // E0..E13 erased
    let image = render_damaged_v2m(&data, &ec, MaskPattern::VerticalLines, &gray);
    vec![Cell {
        index: S5_CELL_BASE,
        version: 2,
        ec: EC_LEVELS[0],
        payload_pct: 100,
        shape: Shape::Square,
        fill: Fill::Gray,
        position: Position::OffFinder,
        occ_pct: 0,
        truth: S5_TRUTH.to_owned(),
        blot: None,
        constructed: Some(Constructed {
            image,
            name: "s5-v2m-solved",
        }),
    }]
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

/// Everything one scan contributes to its pass.
struct CellScan {
    /// The first-QR verdict the tables and the rescue gate count.
    verdict: Verdict,
    wrong: Option<WrongRow>,
    /// Every published detection, accounted.
    outputs: CellOutputs,
    /// `report.trace.engine_panics`.
    engine_panics: u32,
}

fn scan_cell(scanner: &Scanner, cell: &Cell) -> CellScan {
    // QRS_RESCUE_TRACE=1: per-cell bisect trace (run with
    // RAYON_NUM_THREADS=1 so the loud-alloc lines isolate one cell).
    // QRS_RESCUE_CELL=<substring>: scan ONLY cells whose trace line matches
    // (fast repro of a single pathological cell; the output gate then fails,
    // since the skipped cells' disposition rows go stale).
    let mut trace_line = format!(
        "v{} ec={} len{} fill={:?} shape={:?} pos={:?} occ{}",
        cell.version,
        cell.ec.tag,
        cell.truth.len(),
        cell.fill,
        cell.shape,
        cell.position,
        cell.occ_pct
    );
    if let Some(blot) = cell.blot {
        write!(trace_line, " blot=c{}r{}", blot.col, blot.row).unwrap();
    }
    if let Some(filter) = std::env::var_os("QRS_RESCUE_CELL")
        && !trace_line.contains(filter.to_string_lossy().as_ref())
    {
        return CellScan {
            verdict: Verdict {
                class: Class::Refused,
                via_rescue: false,
                rescue_attempted: false,
                hint_low_margin: false,
                spurious_non_qr: false,
            },
            wrong: None,
            outputs: CellOutputs::default(),
            engine_panics: 0,
        };
    }
    if std::env::var_os("QRS_RESCUE_TRACE").is_some() {
        eprintln!("[trace] {trace_line}");
    }
    let png = render_cell(cell);
    let report = scanner
        .scan(ImageInput::encoded(&png))
        .expect("synthetic png is always a valid, in-bounds input");
    let (verdict, wrong) = classify(cell, &report);
    let published: Vec<Published> = report.detections.iter().map(Published::of).collect();
    CellScan {
        verdict,
        wrong,
        outputs: account(cell.index, &cell_label(cell), &cell.truth, &published),
        engine_panics: u32::from(report.trace.engine_panics),
    }
}

/// The first-QR verdict of one report, plus its wrong-decode ledger row.
fn classify(cell: &Cell, report: &ScanReport) -> (Verdict, Option<WrongRow>) {
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

// --------------------------------------------------------- output accounting

/// What makes a published output wrong (module docs, "The output gate").
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum OutputKind {
    /// Any other QR-family engine read; the cell has no truth output.
    QrWrong,
    /// Any other QR-family engine read beside the truth output.
    QrExtra,
    /// Any other QR-family read through the S5 rescue.
    RescueQr,
    /// Any non-QR symbology.
    NonQr,
}

impl OutputKind {
    const ALL: [Self; 4] = [Self::QrWrong, Self::QrExtra, Self::RescueQr, Self::NonQr];

    fn label(self) -> &'static str {
        match self {
            Self::QrWrong => "qr_wrong",
            Self::QrExtra => "qr_extra",
            Self::RescueQr => "rescue_qr",
            Self::NonQr => "non_qr",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.label() == name)
    }
}

/// The path that published an output.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum OutputPath {
    /// Its engines include the S5 rescue.
    Rescue,
    /// rxing and/or rqrr only.
    Engine,
}

impl OutputPath {
    fn label(self) -> &'static str {
        match self {
            Self::Rescue => "rescue",
            Self::Engine => "engine",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [Self::Rescue, Self::Engine]
            .into_iter()
            .find(|path| path.label() == name)
    }
}

/// One published detection, reduced to what the accounting reads.
#[derive(Debug)]
struct Published {
    /// Wire name (`qr_code`, `data_bar`).
    symbology: String,
    qr_family: bool,
    text: String,
    engines: Vec<EngineKind>,
}

impl Published {
    fn of(detection: &Detection) -> Self {
        Self {
            symbology: crate::oracle::wire(&detection.symbology),
            qr_family: detection.symbology.is_qr_family(),
            text: detection.content.text.clone(),
            engines: detection.engines.clone(),
        }
    }
}

/// The identity of a wrong output: every field a disposition row matches.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct OutputKey {
    cell: usize,
    label: String,
    kind: OutputKind,
    path: OutputPath,
    symbology: String,
    text_sha256: String,
    /// UTF-8 bytes.
    text_len: usize,
    engines: String,
}

/// A wrong output as a sweep recorded it.
#[derive(Clone, Debug)]
struct WrongOutput {
    key: OutputKey,
    /// The cell's truth length, in bytes.
    truth_len: usize,
    /// Byte offset of the published text inside the truth, when it is a
    /// substring of it.
    at: Option<usize>,
}

/// Every published detection of one cell, accounted.
#[derive(Default)]
struct CellOutputs {
    /// The cell published exactly one output, and it is the truth.
    strict_correct: bool,
    /// Its wrong outputs, in detection order.
    wrong: Vec<WrongOutput>,
}

/// Account one cell's published detections against its truth. The first
/// `qr_code` output with the truth text is the truth (the grid renders Model 2
/// QR only); every other output is wrong, a second copy of the truth text or
/// the truth text under another symbology included (one symbol, one output).
fn account(cell: usize, label: &str, truth: &str, published: &[Published]) -> CellOutputs {
    let truth_at = published
        .iter()
        .position(|p| p.symbology == "qr_code" && p.text == truth);
    let wrong = published
        .iter()
        .enumerate()
        .filter(|&(i, _)| Some(i) != truth_at)
        .map(|(_, p)| {
            let path = if p.engines.contains(&EngineKind::Rescue) {
                OutputPath::Rescue
            } else {
                OutputPath::Engine
            };
            let kind = match (p.qr_family, path, truth_at) {
                (false, _, _) => OutputKind::NonQr,
                (true, OutputPath::Rescue, _) => OutputKind::RescueQr,
                (true, OutputPath::Engine, Some(_)) => OutputKind::QrExtra,
                (true, OutputPath::Engine, None) => OutputKind::QrWrong,
            };
            WrongOutput {
                key: OutputKey {
                    cell,
                    label: label.to_owned(),
                    kind,
                    path,
                    symbology: p.symbology.clone(),
                    text_sha256: crate::external::sha256_bytes(p.text.as_bytes()),
                    text_len: p.text.len(),
                    engines: engines_label(&p.engines),
                },
                truth_len: truth.len(),
                at: truth.find(p.text.as_str()),
            }
        })
        .collect();
    CellOutputs {
        strict_correct: published.len() == 1 && truth_at == Some(0),
        wrong,
    }
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
                                    index: cells.len(),
                                    version,
                                    ec: *ec,
                                    payload_pct: pct,
                                    shape,
                                    fill,
                                    position,
                                    occ_pct,
                                    truth: truth.clone(),
                                    blot: None,
                                    constructed: None,
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

/// Engine cap of the capped pass: below the v6 and v10 canvas sides, so those
/// cells reach the engines (and the S5 rescue) downscaled at non-integer ratios.
const CAPPED_ENGINE_SIDE: u32 = 400;

/// One sweep of the grid under one scanner, folded into fixed-order buckets.
#[derive(Default)]
struct Pass {
    total: Bucket,
    by_pct: BTreeMap<u32, Bucket>,
    by_fill_ec: BTreeMap<(usize, usize), Bucket>,
    by_position: BTreeMap<usize, Bucket>,
    /// Every wrong decode, in cell order (deterministic ledger).
    wrong_rows: Vec<WrongRow>,
    /// Every wrong output, in cell then detection order.
    outputs: Vec<WrongOutput>,
    /// Cells whose outputs are exactly one truth.
    strict_correct: u32,
    /// Sum of `report.trace.engine_panics` over the pass.
    engine_panics: u32,
    /// Panics the process-wide hook caught during the pass
    /// ([`CAUGHT_PANICS`]). It includes the score-probe panics that
    /// `trace.engine_panics` drops (see [`install_panic_counter`]).
    caught_panics: u32,
    elapsed: std::time::Duration,
}

/// Every panic the process hook has seen. The engine catches its panics
/// (`catch_unwind` in `engine::run_engine`), but the process-wide hook still
/// runs first on the panicking thread, so this counts them all — including the
/// ones the score probe drops. `fetch_add` is atomic, so the parallel scans
/// of a pass tally without loss, and a before/after snapshot around each
/// sequential sweep is deterministic (the count is a function of the images).
static CAUGHT_PANICS: AtomicU32 = AtomicU32::new(0);

/// Install the hook that feeds [`CAUGHT_PANICS`], once. It chains the hook it
/// replaces, so every panic still prints its message on stderr (never the
/// diffed stdout): a failing harness assert or expect stays loud, and each
/// caught engine panic names its site beside the tally.
fn install_panic_counter() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        CAUGHT_PANICS.fetch_add(1, Ordering::Relaxed);
        previous(info);
    }));
}

fn sweep(scanner: &Scanner, cells: &[Cell]) -> Pass {
    let started = std::time::Instant::now();
    let panics_before = CAUGHT_PANICS.load(Ordering::Relaxed);
    // Independent single-threaded scans; collect preserves input order.
    let results: Vec<CellScan> = cells
        .par_iter()
        .map(|cell| scan_cell(scanner, cell))
        .collect();
    let elapsed = started.elapsed();
    // par_iter joined, so every caught panic of this pass is now counted.
    let caught_panics = CAUGHT_PANICS
        .load(Ordering::Relaxed)
        .saturating_sub(panics_before);

    let mut pass = Pass {
        caught_panics,
        elapsed,
        ..Pass::default()
    };
    for (cell, scan) in cells.iter().zip(results) {
        let v = scan.verdict;
        pass.total.add(v);
        pass.by_pct.entry(cell.occ_pct).or_default().add(v);
        let fill_ix = FILLS.iter().position(|f| f.0 == cell.fill).unwrap();
        let ec_ix = EC_LEVELS.iter().position(|e| e.tag == cell.ec.tag).unwrap();
        pass.by_fill_ec.entry((fill_ix, ec_ix)).or_default().add(v);
        let pos_ix = POSITIONS.iter().position(|p| p.0 == cell.position).unwrap();
        pass.by_position.entry(pos_ix).or_default().add(v);
        if let Some(row) = scan.wrong {
            pass.wrong_rows.push(row); // cell order ⇒ deterministic ledger
        }
        pass.strict_correct += u32::from(scan.outputs.strict_correct);
        pass.engine_panics += scan.engine_panics;
        pass.outputs.extend(scan.outputs.wrong);
    }
    pass
}

// ------------------------------------------------------------------ ledger

/// The dispositions ledger (module docs), compiled in.
const DISPOSITIONS: &str = include_str!("../data/rescue-stress-dispositions.tsv");

/// The ledger's repository path, for the report and its messages.
const DISPOSITIONS_PATH: &str = "xtask/data/rescue-stress-dispositions.tsv";

/// The ledger's column header: the row format, pinned.
const LEDGER_COLUMNS: &str =
    "pass\tcell\tlabel\tkind\tpath\tsymbology\ttext_sha256\ttext_len\tengines\treason";

/// The three passes the gate judges, in report order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum PassId {
    Uncapped,
    Capped,
    Dual,
}

impl PassId {
    const ALL: [Self; 3] = [Self::Uncapped, Self::Capped, Self::Dual];

    fn name(self) -> &'static str {
        match self {
            Self::Uncapped => "uncapped",
            Self::Capped => "capped",
            Self::Dual => "dual",
        }
    }

    /// Prefix of the pass's report keys; the uncapped pass keeps bare names.
    fn prefix(self) -> &'static str {
        match self {
            Self::Uncapped => "",
            Self::Capped => "capped_",
            Self::Dual => "dual_",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|pass| pass.name() == name)
    }

    fn ix(self) -> usize {
        self as usize
    }
}

/// One tolerated wrong output.
#[derive(Clone, Debug)]
struct Disposition {
    pass: PassId,
    key: OutputKey,
    reason: String,
    /// 1-based line in the ledger file.
    line: usize,
}

/// A pass's pins: the lowest accepted `strict_correct`, the exact
/// `engine_panics` sum, and the exact `caught_panics` count. `None` = not
/// pinned, which fails the gate.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Pins {
    floor: Option<u32>,
    engine_panics: Option<u32>,
    caught_panics: Option<u32>,
}

#[derive(Debug)]
struct Ledger {
    /// Indexed by `PassId::ix`.
    pins: [Pins; 3],
    rows: Vec<Disposition>,
}

/// Parse the ledger: comments (`#`) and blank lines anywhere, pin lines
/// (`floor|engine_panics|caught_panics <TAB> pass <TAB> count`) before the
/// column header, rows after it.
fn parse_ledger(src: &str) -> Result<Ledger, String> {
    let mut pins = [Pins::default(); 3];
    let mut rows = Vec::new();
    let mut in_rows = false;
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let parsed = if in_rows {
            parse_row(&fields, i + 1).map(|row| rows.push(row))
        } else if line == LEDGER_COLUMNS {
            in_rows = true;
            Ok(())
        } else {
            parse_pin(&fields, &mut pins)
        };
        parsed.map_err(|what| format!("{DISPOSITIONS_PATH}:{}: {what}", i + 1))?;
    }
    if !in_rows {
        return Err(format!("{DISPOSITIONS_PATH}: no column header line"));
    }
    Ok(Ledger { pins, rows })
}

fn parse_pin(fields: &[&str], pins: &mut [Pins; 3]) -> Result<(), String> {
    let [key, pass, value] = fields else {
        return Err(
            "expected a pin (`floor|engine_panics|caught_panics <TAB> pass <TAB> count`) or \
                    the column header"
                .to_owned(),
        );
    };
    let pass = PassId::parse(pass).ok_or("unknown pass")?;
    let value: u32 = value.parse().map_err(|_| "a pin value is a count")?;
    let slot = match *key {
        "floor" => &mut pins[pass.ix()].floor,
        "engine_panics" => &mut pins[pass.ix()].engine_panics,
        "caught_panics" => &mut pins[pass.ix()].caught_panics,
        _ => return Err("unknown pin".to_owned()),
    };
    if slot.replace(value).is_some() {
        return Err("pin repeated".to_owned());
    }
    Ok(())
}

fn parse_row(fields: &[&str], line: usize) -> Result<Disposition, String> {
    let [
        pass,
        cell,
        label,
        kind,
        path,
        symbology,
        sha,
        len,
        engines,
        reason,
    ] = fields
    else {
        return Err("a row has 10 tab-separated fields".to_owned());
    };
    let pass = PassId::parse(pass).ok_or("unknown pass")?;
    if cell.len() != 4 || !cell.bytes().all(|b| b.is_ascii_digit()) {
        return Err("cell is a 4-digit grid index".to_owned());
    }
    if !label.starts_with(&format!("{cell}_")) {
        return Err("label does not name its cell".to_owned());
    }
    let kind = OutputKind::parse(kind).ok_or("unknown kind")?;
    let path = OutputPath::parse(path).ok_or("unknown path")?;
    if pass != PassId::Dual && (kind == OutputKind::RescueQr || path == OutputPath::Rescue) {
        return Err("only the dual pass may hold a rescue-path output".to_owned());
    }
    if symbology.is_empty()
        || !symbology
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("symbology is a wire name".to_owned());
    }
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("text_sha256 is 64 lowercase hex digits".to_owned());
    }
    let text_len: usize = len.parse().map_err(|_| "text_len is a byte count")?;
    if engines
        .split('+')
        .any(|e| !matches!(e, "rxing" | "rqrr" | "rescue"))
    {
        return Err("engines are rxing, rqrr and rescue, joined by +".to_owned());
    }
    if reason.trim().is_empty() {
        return Err("a row carries a reason".to_owned());
    }
    Ok(Disposition {
        pass,
        key: OutputKey {
            cell: cell.parse().map_err(|_| "cell is a grid index")?,
            label: (*label).to_owned(),
            kind,
            path,
            symbology: (*symbology).to_owned(),
            text_sha256: (*sha).to_owned(),
            text_len,
            engines: (*engines).to_owned(),
        },
        reason: (*reason).to_owned(),
        line,
    })
}

/// One pass's outputs matched against its ledger rows and pins.
struct PassGate<'a> {
    pass: PassId,
    /// Each wrong output, and whether a row holds it.
    outputs: Vec<(&'a WrongOutput, bool)>,
    /// Rows whose output no longer occurs, in file order.
    stale: Vec<&'a Disposition>,
    strict_correct: u32,
    engine_panics: u32,
    caught_panics: u32,
    pins: Pins,
}

impl PassGate<'_> {
    fn count(&self, kind: OutputKind) -> usize {
        self.outputs
            .iter()
            .filter(|(o, _)| o.key.kind == kind)
            .count()
    }

    fn held(&self) -> usize {
        self.outputs.iter().filter(|(_, held)| *held).count()
    }

    fn open(&self) -> usize {
        self.outputs.len() - self.held()
    }

    /// Wrong outputs whose engines include the S5 rescue, held or not.
    fn via_rescue(&self) -> usize {
        self.outputs
            .iter()
            .filter(|(o, _)| o.key.path == OutputPath::Rescue)
            .count()
    }

    /// Why this pass fails the gate; empty when it passes.
    fn failures(&self) -> Vec<String> {
        let name = self.pass.name();
        let mut failures = Vec::new();
        let open = self.open();
        if open > 0 {
            failures.push(format!(
                "{name}: {open} wrong outputs without a disposition row"
            ));
        }
        if !self.stale.is_empty() {
            failures.push(format!(
                "{name}: {} stale disposition rows (their output no longer occurs)",
                self.stale.len()
            ));
        }
        // The uncapped and capped passes carry the zero-rescue-wrong claim, so
        // no row can excuse a rescue output there; only the dual pass holds one.
        let via_rescue = self.via_rescue();
        if self.pass != PassId::Dual && via_rescue > 0 {
            failures.push(format!(
                "{name}: {via_rescue} rescue-path wrong outputs (none allowed here, held or not)"
            ));
        }
        // An exact pin: a gain fails too, so the change that earns it re-pins.
        let strict = self.strict_correct;
        match self.pins.floor {
            None => failures.push(format!("{name}: strict_correct floor not pinned")),
            Some(floor) if strict < floor => failures.push(format!(
                "{name}: strict_correct {strict} below its floor {floor}"
            )),
            Some(floor) if strict > floor => failures.push(format!(
                "{name}: strict_correct {strict} above its floor {floor}: raise the floor to \
                 {strict}"
            )),
            Some(_) => {}
        }
        match self.pins.engine_panics {
            None => failures.push(format!("{name}: engine_panics not pinned")),
            Some(pin) if pin != self.engine_panics => failures.push(format!(
                "{name}: engine_panics {} differs from its pin {pin}",
                self.engine_panics
            )),
            Some(_) => {}
        }
        match self.pins.caught_panics {
            None => failures.push(format!("{name}: caught_panics not pinned")),
            Some(pin) if pin != self.caught_panics => failures.push(format!(
                "{name}: caught_panics {} differs from its pin {pin}",
                self.caught_panics
            )),
            Some(_) => {}
        }
        failures
    }
}

/// Match a pass's outputs against its rows. A row holds at most one output,
/// and only the output equal to it in every key field.
fn judge<'a>(pass: PassId, measured: &'a Pass, ledger: &'a Ledger) -> PassGate<'a> {
    let mut free: BTreeMap<&OutputKey, Vec<&Disposition>> = BTreeMap::new();
    for row in ledger.rows.iter().filter(|row| row.pass == pass) {
        free.entry(&row.key).or_default().push(row);
    }
    let outputs = measured
        .outputs
        .iter()
        .map(|output| {
            let held = free.get_mut(&output.key).is_some_and(|rows| {
                let taken = !rows.is_empty();
                if taken {
                    rows.remove(0);
                }
                taken
            });
            (output, held)
        })
        .collect();
    let mut stale: Vec<&Disposition> = free.into_values().flatten().collect();
    stale.sort_by_key(|row| row.line);
    PassGate {
        pass,
        outputs,
        stale,
        strict_correct: measured.strict_correct,
        engine_panics: measured.engine_panics,
        caught_panics: measured.caught_panics,
        pins: ledger.pins[pass.ix()],
    }
}

pub fn run() {
    // Count every caught engine panic, including the score-probe panics that
    // trace.engine_panics drops — installed before any scan.
    install_panic_counter();
    // Compiled in: a malformed ledger fails before any scan.
    let ledger = parse_ledger(DISPOSITIONS).unwrap_or_else(|e| {
        eprintln!("[rescue-stress] {e}");
        std::process::exit(2);
    });
    // budget None ⇒ the ladder always runs to completion ⇒ the report is a pure
    // function of the pixels (the crate's strict-determinism configuration).
    let mut cfg = ScanProfile::Full.config();
    cfg.budget_ms = None;
    // The capped pass differs ONLY by the engine cap (module docs).
    let mut capped_cfg = cfg.clone();
    capped_cfg.max_engine_side = CAPPED_ENGINE_SIDE;
    let scanner = Scanner::builder().profile(ScanProfile::Custom(cfg)).build();
    let capped_scanner = Scanner::builder()
        .profile(ScanProfile::Custom(capped_cfg))
        .build();

    let cells = build_grid();
    let dual_cells = build_dual_grid(&cells);

    // Guard: every base symbol decodes clean (0% occlusion) under BOTH
    // configurations. Cheap, and it turns a silent encode/geometry or cap
    // regression into a loud panic.
    verify_base_symbols(&scanner);
    verify_base_symbols(&capped_scanner);

    let uncapped = sweep(&scanner, &cells);
    let capped = sweep(&capped_scanner, &cells);
    let dual = sweep(&scanner, &dual_cells);

    let mut out = String::new();
    write_header(&mut out, uncapped.total.scans);
    write_tables(
        &mut out,
        &uncapped.by_pct,
        &uncapped.by_fill_ec,
        &uncapped.by_position,
        &uncapped.wrong_rows,
    );
    write_summary(&mut out, uncapped.total, &uncapped.by_pct);
    write_capped(&mut out, &capped);
    let on_ec = dual_cells
        .iter()
        .filter(|cell| cell.blot.is_some_and(|blot| !blot.on_data))
        .count();
    let constructed = dual_cells
        .iter()
        .filter(|cell| cell.constructed.is_some())
        .count();
    write_dual(&mut out, &dual, on_ec, constructed);

    let gates = [
        judge(PassId::Uncapped, &uncapped, &ledger),
        judge(PassId::Capped, &capped, &ledger),
        judge(PassId::Dual, &dual, &ledger),
    ];
    let mut failures: Vec<String> = Vec::new();
    for gate in &gates {
        let measured = match gate.pass {
            PassId::Uncapped => Some(&uncapped),
            PassId::Capped => Some(&capped),
            PassId::Dual => None,
        };
        if let Some(total) = measured.map(|pass| pass.total) {
            let name = gate.pass.name();
            if total.rescue_wrong > 0 {
                failures.push(format!(
                    "{name}: {} rescue-path wrong decodes",
                    total.rescue_wrong
                ));
            }
            if total.rescue_success == 0 {
                failures.push(format!(
                    "{name}: zero rescue decodes, so its zero-wrong line measures nothing"
                ));
            }
        }
        failures.extend(gate.failures());
    }
    write_gate(&mut out, &ledger, &gates, &failures);

    print!("{out}");
    // Wall-clock on stderr ONLY — never in the diffed stdout stream.
    eprintln!(
        "\n[rescue-stress] {} scans in {:.1}s, capped pass in {:.1}s, dual pass ({} scans) in \
         {:.1}s, on {} threads",
        uncapped.total.scans,
        uncapped.elapsed.as_secs_f64(),
        capped.elapsed.as_secs_f64(),
        dual.total.scans,
        dual.elapsed.as_secs_f64(),
        rayon::current_num_threads()
    );

    // The hard gate: zero rescue-path wrong decodes in EITHER existing pass,
    // then every output accounted for in every pass.
    let rescue_wrong = (uncapped.total.rescue_wrong, capped.total.rescue_wrong);
    if rescue_wrong != (0, 0) {
        eprintln!(
            "[rescue-stress] GATE FAILED: rescue-path wrong decodes, uncapped {}, capped {}",
            rescue_wrong.0, rescue_wrong.1
        );
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("[rescue-stress] GATE FAILED: {failure}");
        }
        std::io::stdout().flush().unwrap();
        std::process::exit(1);
    }
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
    wrong_rows: &[WrongRow],
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

    // Table C — by position: on_finder is the refuse control (no grid ⇒ no
    // rescue); corner_adjacent clips one format-information copy
    // (`Position::CornerAdjacent`).
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
    write_ledger(out, "wrong-decode ledger", wrong_rows);
}

fn write_ledger(out: &mut String, title: &str, rows: &[WrongRow]) {
    writeln!(out, "\n## {title} ({} rows)", rows.len()).unwrap();
    writeln!(
        out,
        "| # | v | ec | fill | position | occ% | engines | via_rescue | hinted | decoded |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|").unwrap();
    for (i, r) in rows.iter().enumerate() {
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

/// The counters of a prefixed pass (`capped_`, `dual_`), aligned the way the
/// capped block has always printed them.
fn write_prefixed_counts(out: &mut String, prefix: &str, total: Bucket) {
    let key = |name: &str| format!("{prefix}{name}");
    writeln!(out, "{:<24}= {}", key("total_scans"), total.scans).unwrap();
    writeln!(out, "{:<24}= {}", key("decoded_correct"), total.correct).unwrap();
    writeln!(out, "{:<24}= {}", key("decoded_wrong"), total.wrong).unwrap();
    writeln!(out, "{:<24}= {}", key("refused"), total.refused).unwrap();
    writeln!(
        out,
        "{:<24}= {} (S5 ran: grid found, engines failed)",
        key("rescue_attempted"),
        total.rescue_attempted
    )
    .unwrap();
    let decodes = format!("  {}", key("rescue_decodes"));
    writeln!(
        out,
        "{decodes:<24}= {} (correct {}, wrong {})",
        total.rescue_success, total.rescue_correct, total.rescue_wrong
    )
    .unwrap();
    let refused = format!("  {}", key("rescue_refused"));
    writeln!(
        out,
        "{refused:<24}= {}   {} = {}",
        total.rescue_refused,
        key("rescue_refuse_rate"),
        rate_or_na(total.rescue_refused, total.rescue_attempted)
    )
    .unwrap();
}

/// The capped pass, after the uncapped verdict: the same counters under
/// distinct keys. Its gate line is `capped_rescue_decodes … (correct C, wrong W)`,
/// never `rescue_succeeded`, so the uncapped deep-checks grep cannot match it.
fn write_capped(out: &mut String, pass: &Pass) {
    writeln!(
        out,
        "\n## capped pass (max_engine_side = {CAPPED_ENGINE_SIDE})"
    )
    .unwrap();
    writeln!(out, "capped_engine_scale     = {}", capped_scales()).unwrap();
    write_prefixed_counts(out, "capped_", pass.total);
    write_ledger(out, "capped wrong-decode ledger", &pass.wrong_rows);
}

/// The dual-occluder pass, after the capped one: the same counters under
/// `dual_` keys, which neither deep-checks grep can match.
fn write_dual(out: &mut String, pass: &Pass, on_ec: usize, constructed: usize) {
    writeln!(
        out,
        "\n## dual-occluder pass (every gray cell plus one dark {BLOT_MODULES}x{BLOT_MODULES}-module \
         blot, uncapped scanner)"
    )
    .unwrap();
    writeln!(
        out,
        "dual_blot               = 2-3 codewords no gray pixel touches, outside the function \
         patterns: data codewords, EC codewords in {on_ec} cells (no such data codeword left)"
    )
    .unwrap();
    writeln!(
        out,
        "dual_constructed        = {constructed} codeword-level S5 cells (a constructed margin-0 \
         erasure+error case: 14 grayed EC codewords, two confident errors)"
    )
    .unwrap();
    write_prefixed_counts(out, "dual_", pass.total);
    write_ledger(out, "dual wrong-decode ledger", &pass.wrong_rows);
}

/// The output gate: per-pass accounting lines, every wrong output, the stale
/// rows and the verdict.
fn write_gate(out: &mut String, ledger: &Ledger, gates: &[PassGate<'_>], failures: &[String]) {
    writeln!(
        out,
        "\n## output gate (every published detection, every path)"
    )
    .unwrap();
    writeln!(
        out,
        "dispositions_file = {DISPOSITIONS_PATH} ({} rows)",
        ledger.rows.len()
    )
    .unwrap();
    for gate in gates {
        let p = gate.pass.prefix();
        writeln!(
            out,
            "{p}wrong_outputs = {} (engine_qr {}, extra_qr {}, rescue_qr {}, non_qr {})",
            gate.outputs.len(),
            gate.count(OutputKind::QrWrong),
            gate.count(OutputKind::QrExtra),
            gate.count(OutputKind::RescueQr),
            gate.count(OutputKind::NonQr)
        )
        .unwrap();
        writeln!(out, "{p}wrong_outputs_open = {}", gate.open()).unwrap();
        writeln!(
            out,
            "{p}dispositions = {} held, {} stale",
            gate.held(),
            gate.stale.len()
        )
        .unwrap();
        writeln!(
            out,
            "{p}strict_correct = {} (floor {})",
            gate.strict_correct,
            pin_label(gate.pins.floor)
        )
        .unwrap();
        writeln!(
            out,
            "{p}engine_panics = {} (pinned {})",
            gate.engine_panics,
            pin_label(gate.pins.engine_panics)
        )
        .unwrap();
        writeln!(
            out,
            "{p}caught_panics = {} (pinned {})",
            gate.caught_panics,
            pin_label(gate.pins.caught_panics)
        )
        .unwrap();
    }
    write_output_ledger(out, gates);
    write_stale(out, gates);
    writeln!(out, "\n## gate verdict").unwrap();
    writeln!(out, "gate_failures = {}", failures.len()).unwrap();
    for failure in failures {
        writeln!(out, "  - {failure}").unwrap();
    }
}

fn pin_label(pin: Option<u32>) -> String {
    pin.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

/// Every wrong output of every pass, with the lengths and the truth offset
/// that tell a publication defect (a slice of the truth) from a miscorrection.
fn write_output_ledger(out: &mut String, gates: &[PassGate<'_>]) {
    let total: usize = gates.iter().map(|gate| gate.outputs.len()).sum();
    let held: usize = gates.iter().map(PassGate::held).sum();
    writeln!(
        out,
        "\n## wrong-output ledger ({total} outputs: {held} held, {} open)",
        total - held
    )
    .unwrap();
    writeln!(
        out,
        "| pass | cell | label | kind | path | symbology | engines | text_len | truth_len | at | \
         text_sha256 | state |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|---|").unwrap();
    for gate in gates {
        for (output, held) in &gate.outputs {
            let key = &output.key;
            writeln!(
                out,
                "| {} | {:04} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                gate.pass.name(),
                key.cell,
                key.label,
                key.kind.label(),
                key.path.label(),
                key.symbology,
                key.engines,
                key.text_len,
                output.truth_len,
                output
                    .at
                    .map_or_else(|| "-".to_owned(), |at| at.to_string()),
                key.text_sha256,
                if *held { "held" } else { "open" }
            )
            .unwrap();
        }
    }
}

/// Rows whose output no longer occurs: delete them with the change that
/// cleared them.
fn write_stale(out: &mut String, gates: &[PassGate<'_>]) {
    let stale: usize = gates.iter().map(|gate| gate.stale.len()).sum();
    writeln!(out, "\n## stale dispositions ({stale} rows)").unwrap();
    writeln!(
        out,
        "| pass | line | label | kind | path | symbology | engines | text_len | text_sha256 | \
         reason |"
    )
    .unwrap();
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|").unwrap();
    for row in gates.iter().flat_map(|gate| &gate.stale) {
        let key = &row.key;
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            row.pass.name(),
            row.line,
            key.label,
            key.kind.label(),
            key.path.label(),
            key.symbology,
            key.engines,
            key.text_len,
            key.text_sha256,
            row.reason
        )
        .unwrap();
    }
}

/// Each version's canvas side and the scale the capped pass hands the
/// engines, in closed form (the cap maps the longest side onto
/// `CAPPED_ENGINE_SIDE` when it is larger).
fn capped_scales() -> String {
    let parts: Vec<String> = VERSIONS
        .iter()
        .map(|&version| {
            let modules = u32::try_from(version).unwrap() * 4 + 17 + 2 * QUIET_MODULES;
            let side = modules * MODULE_PX;
            if side <= CAPPED_ENGINE_SIDE {
                format!("v{version} {side}px uncapped")
            } else {
                let scale = f64::from(CAPPED_ENGINE_SIDE) / f64::from(side);
                format!("v{version} {side}px ×{scale:.3}")
            }
        })
        .collect();
    parts.join(" · ")
}

/// Panic unless every base symbol, with no occlusion, publishes exactly one
/// output: its truth, as `qr_code` — isolates encode/geometry regressions from
/// the occlusion measurement, and no clean symbol publishes an extra output.
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
                let published: Vec<Published> =
                    report.detections.iter().map(Published::of).collect();
                assert!(
                    account(0, "base", &truth, &published).strict_correct,
                    "clean base symbol v{version} ec={} pct={pct} did not publish exactly its \
                     truth as qr_code: {published:?}",
                    ec.tag
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rescued(correct: u32, wrong: u32) -> Bucket {
        Bucket {
            rescue_attempted: correct + wrong,
            rescue_success: correct + wrong,
            rescue_correct: correct,
            rescue_wrong: wrong,
            ..Bucket::default()
        }
    }

    #[test]
    fn capped_pass_scales_only_the_canvases_above_the_cap() {
        assert_eq!(
            capped_scales(),
            "v2 296px uncapped · v6 424px ×0.943 · v10 552px ×0.725"
        );
    }

    #[test]
    fn uncapped_gate_line_keeps_the_deep_checks_format() {
        let mut out = String::new();
        write_summary(&mut out, rescued(3, 0), &BTreeMap::new());
        assert!(
            out.contains("\n  rescue_succeeded     = 3 (correct 3, wrong 0)\n"),
            "{out}"
        );
    }

    #[test]
    fn capped_gate_line_can_never_match_the_uncapped_grep() {
        let pass = Pass {
            total: rescued(2, 1),
            ..Pass::default()
        };
        let mut out = String::new();
        write_capped(&mut out, &pass);
        assert!(
            out.contains("\n  capped_rescue_decodes = 3 (correct 2, wrong 1)\n"),
            "{out}"
        );
        assert!(!out.contains("rescue_succeeded"), "{out}");
    }

    /// The capped block is now written through the shared prefixed writer:
    /// every byte the capped pass printed before stays as it was.
    #[test]
    fn capped_block_keeps_its_bytes() {
        let pass = Pass {
            total: Bucket {
                scans: 11,
                correct: 2,
                wrong: 3,
                refused: 6,
                rescue_attempted: 5,
                rescue_success: 3,
                rescue_refused: 2,
                rescue_correct: 2,
                rescue_wrong: 1,
                ..Bucket::default()
            },
            ..Pass::default()
        };
        let mut out = String::new();
        write_capped(&mut out, &pass);
        let expected = "\n## capped pass (max_engine_side = 400)\n\
            capped_engine_scale     = v2 296px uncapped · v6 424px ×0.943 · v10 552px ×0.725\n\
            capped_total_scans      = 11\n\
            capped_decoded_correct  = 2\n\
            capped_decoded_wrong    = 3\n\
            capped_refused          = 6\n\
            capped_rescue_attempted = 5 (S5 ran: grid found, engines failed)\n  \
            capped_rescue_decodes = 3 (correct 2, wrong 1)\n  \
            capped_rescue_refused = 2   capped_rescue_refuse_rate = 40.000%\n\
            \n## capped wrong-decode ledger (0 rows)\n\
            | # | v | ec | fill | position | occ% | engines | via_rescue | hinted | decoded |\n\
            |---|---|---|---|---|---|---|---|---|---|\n";
        assert_eq!(out, expected);
    }

    fn qr(text: &str, engines: &[EngineKind]) -> Published {
        Published {
            symbology: "qr_code".to_owned(),
            qr_family: true,
            text: text.to_owned(),
            engines: engines.to_vec(),
        }
    }

    fn other(symbology: &str, text: &str) -> Published {
        Published {
            symbology: symbology.to_owned(),
            qr_family: false,
            text: text.to_owned(),
            engines: vec![EngineKind::Rxing],
        }
    }

    fn sha(text: &str) -> String {
        crate::external::sha256_bytes(text.as_bytes())
    }

    /// One synthetic cell per kind: each wrong output lands in its kind and
    /// path, and only the truth-only cells are strictly correct.
    #[test]
    fn wrong_outputs_count_every_path() {
        use EngineKind::{Rescue, Rqrr, Rxing};
        let truth = "QRCODEAI0123456789abcdefghijklmnopqrstuvwxyz-./:";
        let cells = [
            vec![qr(truth, &[Rxing, Rqrr])],
            vec![qr("abcdefghijklmnopqrstuvwxyzabcdef", &[Rxing])],
            vec![qr(truth, &[Rxing]), qr("abcdef", &[Rxing])],
            vec![qr(
                "QRCODEAI0123456789abcdefghijklmnopqrstuvwxyz-./;",
                &[Rescue],
            )],
            vec![other("data_bar", "05566926102365")],
            vec![qr(truth, &[Rescue])],
            vec![],
        ];
        let mut kinds = Vec::new();
        let mut strict = Vec::new();
        for (i, published) in cells.iter().enumerate() {
            let outputs = account(i, &format!("{i:04}_t"), truth, published);
            if outputs.strict_correct {
                strict.push(i);
            }
            kinds.extend(
                outputs
                    .wrong
                    .iter()
                    .map(|w| (w.key.cell, w.key.kind, w.key.path)),
            );
        }
        assert_eq!(
            kinds,
            [
                (1, OutputKind::QrWrong, OutputPath::Engine),
                (2, OutputKind::QrExtra, OutputPath::Engine),
                (3, OutputKind::RescueQr, OutputPath::Rescue),
                (4, OutputKind::NonQr, OutputPath::Engine),
            ]
        );
        assert_eq!(strict, [0, 5]);

        let extra = account(2, "0002_t", truth, &cells[2]);
        let [w] = extra.wrong.as_slice() else {
            panic!("one wrong output")
        };
        assert_eq!(
            w.key,
            OutputKey {
                cell: 2,
                label: "0002_t".to_owned(),
                kind: OutputKind::QrExtra,
                path: OutputPath::Engine,
                symbology: "qr_code".to_owned(),
                text_sha256: sha("abcdef"),
                text_len: 6,
                engines: "rxing".to_owned(),
            }
        );
        assert_eq!((w.truth_len, w.at), (48, Some(18)));
        assert_eq!(
            account(1, "0001_t", truth, &cells[1]).wrong[0].at,
            None,
            "two byte segments glued together are no slice of the truth"
        );

        // One symbol, one output: the truth text a second time (another QR
        // family symbology) is an extra output, and the cell is not strict.
        let mut twice = vec![qr(truth, &[Rxing])];
        twice.push(Published {
            symbology: "micro_qr_code".to_owned(),
            ..qr(truth, &[Rqrr])
        });
        let outputs = account(9, "0009_t", truth, &twice);
        assert!(!outputs.strict_correct);
        assert_eq!(outputs.wrong.len(), 1);
        assert_eq!(outputs.wrong[0].key.kind, OutputKind::QrExtra);
        assert_eq!(outputs.wrong[0].key.symbology, "micro_qr_code");

        // The truth is a `qr_code` output: the truth text under another QR
        // family symbology alone is a wrong output, never the cell's truth.
        let outputs = account(10, "0010_t", truth, &twice[1..]);
        assert!(!outputs.strict_correct);
        assert_eq!(outputs.wrong.len(), 1);
        assert_eq!(outputs.wrong[0].key.kind, OutputKind::QrWrong);
        assert_eq!(outputs.wrong[0].key.symbology, "micro_qr_code");
    }

    fn output(cell: usize, kind: OutputKind, text: &str) -> WrongOutput {
        WrongOutput {
            key: OutputKey {
                cell,
                label: format!("{cell:04}_t"),
                kind,
                path: if kind == OutputKind::RescueQr {
                    OutputPath::Rescue
                } else {
                    OutputPath::Engine
                },
                symbology: "qr_code".to_owned(),
                text_sha256: sha(text),
                text_len: text.len(),
                engines: "rxing".to_owned(),
            },
            truth_len: 48,
            at: None,
        }
    }

    fn row(pass: &str, output: &WrongOutput) -> String {
        let key = &output.key;
        format!(
            "{pass}\t{:04}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\ta test reason",
            key.cell,
            key.label,
            key.kind.label(),
            key.path.label(),
            key.symbology,
            key.text_sha256,
            key.text_len,
            key.engines
        )
    }

    fn ledger(pins: &[&str], rows: &[String]) -> Ledger {
        let mut src = String::from("# a test ledger\n\n");
        for pin in pins {
            writeln!(src, "{pin}").unwrap();
        }
        writeln!(src, "{LEDGER_COLUMNS}").unwrap();
        for row in rows {
            writeln!(src, "{row}").unwrap();
        }
        parse_ledger(&src).unwrap()
    }

    const PINS: [&str; 3] = [
        "floor\tuncapped\t5",
        "engine_panics\tuncapped\t2",
        "caught_panics\tuncapped\t2",
    ];

    #[test]
    fn dispositions_hold_stale_and_open() {
        let held = output(1, OutputKind::QrWrong, "abc");
        let open = output(2, OutputKind::NonQr, "05566926102365");
        let gone = output(3, OutputKind::QrExtra, "abd");
        let measured = Pass {
            outputs: vec![held.clone(), open.clone()],
            strict_correct: 5,
            engine_panics: 2,
            caught_panics: 2,
            ..Pass::default()
        };
        let state = |book: &Ledger| {
            let gate = judge(PassId::Uncapped, &measured, book);
            (gate.held(), gate.open(), gate.stale.len(), gate.failures())
        };

        let book = ledger(&PINS, &[row("uncapped", &held), row("uncapped", &gone)]);
        assert_eq!(
            state(&book),
            (
                1,
                1,
                1,
                vec![
                    "uncapped: 1 wrong outputs without a disposition row".to_owned(),
                    "uncapped: 1 stale disposition rows (their output no longer occurs)".to_owned(),
                ]
            )
        );
        // Every output held and no row left over: the pass is clean.
        let book = ledger(&PINS, &[row("uncapped", &held), row("uncapped", &open)]);
        assert_eq!(state(&book), (2, 0, 0, Vec::<String>::new()));
        // Another pass's row never holds this pass's output.
        let book = ledger(&PINS, &[row("capped", &held), row("uncapped", &open)]);
        let counts = state(&book);
        assert_eq!((counts.0, counts.1, counts.2), (1, 1, 0));
        // A row holds one output: a duplicate row of a single output is stale.
        let book = ledger(
            &PINS,
            &[
                row("uncapped", &held),
                row("uncapped", &held),
                row("uncapped", &open),
            ],
        );
        let counts = state(&book);
        assert_eq!((counts.0, counts.1, counts.2), (2, 0, 1));
        // Exact match: a row differing in any key field holds nothing.
        let edits: [fn(&mut OutputKey); 6] = [
            |k| k.label.push('x'),
            |k| k.kind = OutputKind::QrExtra,
            |k| k.symbology = "micro_qr_code".to_owned(),
            |k| k.text_sha256 = sha("abd"),
            |k| k.text_len += 1,
            |k| k.engines = "rxing+rqrr".to_owned(),
        ];
        for edit in edits {
            let mut near = held.clone();
            edit(&mut near.key);
            let book = ledger(&PINS, &[row("uncapped", &near), row("uncapped", &open)]);
            let counts = state(&book);
            assert_eq!((counts.0, counts.1, counts.2), (1, 1, 1), "{:?}", near.key);
        }
        // The path too, in the one pass where a rescue-path row may exist.
        let mut near = held.clone();
        near.key.path = OutputPath::Rescue;
        let pins = PINS.map(|pin| pin.replace("uncapped", "dual"));
        let book = ledger(
            &pins.each_ref().map(String::as_str),
            &[row("dual", &near), row("dual", &open)],
        );
        let gate = judge(PassId::Dual, &measured, &book);
        assert_eq!((gate.held(), gate.open(), gate.stale.len()), (1, 1, 1));
    }

    #[test]
    fn every_pin_fails_in_either_direction() {
        let measured = Pass {
            strict_correct: 5,
            engine_panics: 2,
            caught_panics: 7,
            ..Pass::default()
        };
        // A fully-pinned, matching ledger for the dual pass.
        let ok: [&str; 3] = [
            "floor\tdual\t5",
            "engine_panics\tdual\t2",
            "caught_panics\tdual\t7",
        ];
        let with = |extra: &str| -> Vec<String> {
            let mut pins: Vec<&str> = ok.to_vec();
            // Replace the pin whose key matches `extra`'s key.
            let key = extra.split('\t').next().unwrap();
            pins.retain(|p| p.split('\t').next() != Some(key));
            pins.push(extra);
            judge(PassId::Dual, &measured, &ledger(&pins, &[])).failures()
        };
        assert!(with("floor\tdual\t5").is_empty());
        assert_eq!(
            with("floor\tdual\t4"),
            ["dual: strict_correct 5 above its floor 4: raise the floor to 5"],
            "a gain fails too, so the change that earns it re-pins the floor"
        );
        assert_eq!(
            with("floor\tdual\t6"),
            ["dual: strict_correct 5 below its floor 6"]
        );
        for pin in ["1", "3"] {
            assert_eq!(
                with(&format!("engine_panics\tdual\t{pin}")),
                [format!("dual: engine_panics 2 differs from its pin {pin}")]
            );
        }
        for pin in ["6", "8"] {
            assert_eq!(
                with(&format!("caught_panics\tdual\t{pin}")),
                [format!("dual: caught_panics 7 differs from its pin {pin}")]
            );
        }
        assert_eq!(
            judge(
                PassId::Dual,
                &measured,
                &ledger(&["floor\tuncapped\t5", "engine_panics\tuncapped\t2"], &[])
            )
            .failures(),
            [
                "dual: strict_correct floor not pinned",
                "dual: engine_panics not pinned",
                "dual: caught_panics not pinned"
            ],
            "another pass's pins pin nothing here"
        );
    }

    #[test]
    fn the_ledger_parser_rejects_malformed_files() {
        let good = output(1, OutputKind::QrWrong, "abc");
        let ok = row("uncapped", &good);
        let file = |pins: &str, rows: &str| format!("{pins}{LEDGER_COLUMNS}\n{rows}\n");
        let parses = |src: &str| parse_ledger(src).is_ok();
        assert!(parses(&file("floor\tuncapped\t1\n", &ok)));
        assert!(parses(&file("", &ok)), "pins are optional to the parser");
        assert!(
            !parses(&format!("{ok}\n")),
            "a row before the column header"
        );
        assert!(!parses("# only comments\n"), "no column header");
        for (pins, why) in [
            ("floor\tuncapped\t1\nfloor\tuncapped\t2\n", "a repeated pin"),
            ("floor\tnowhere\t1\n", "an unknown pass"),
            ("ceiling\tuncapped\t1\n", "an unknown pin"),
            ("floor\tuncapped\tmany\n", "a pin that is no count"),
        ] {
            assert!(!parses(&file(pins, &ok)), "{why}");
        }
        for (bad, why) in [
            (ok.replace("qr_wrong", "qr_odd"), "an unknown kind"),
            (ok.replace("\tengine\t", "\tscan\t"), "an unknown path"),
            (ok.replace(&good.key.text_sha256, "abc"), "a short sha256"),
            (ok.replace("\ta test reason", ""), "a missing field"),
            (ok.replace("a test reason", " "), "a blank reason"),
            (
                ok.replace("\t0001_t\t", "\t0002_t\t"),
                "a label of another cell",
            ),
            (ok.replace("\trxing\t", "\tzxing\t"), "an unknown engine"),
            (
                ok.replace("uncapped\t0001", "uncapped\t1"),
                "a short cell index",
            ),
        ] {
            assert!(!parses(&file("", &bad)), "{why}");
        }
        // Only the dual pass may hold a rescue-path output.
        let rescued = output(1, OutputKind::RescueQr, "abc");
        assert!(parses(&file("", &row("dual", &rescued))));
        for pass in ["uncapped", "capped"] {
            assert!(
                !parses(&file("", &row(pass, &rescued))),
                "{pass}: rescue_qr"
            );
            let via = row(pass, &good).replace("\tengine\t", "\trescue\t");
            assert!(!parses(&file("", &via)), "{pass}: a rescue-path row");
        }
    }

    /// The uncapped and capped passes carry the zero-rescue-wrong claim: a
    /// rescue-path wrong output fails them, held or not. Only the dual pass
    /// holds one with a row.
    #[test]
    fn only_the_dual_pass_holds_a_rescue_output() {
        let rescued = output(4, OutputKind::RescueQr, "abd");
        let measured = Pass {
            outputs: vec![rescued.clone()],
            strict_correct: 5,
            engine_panics: 2,
            caught_panics: 2,
            ..Pass::default()
        };
        let book = |pass: &str, rows: &[String]| {
            let pins = PINS.map(|pin| pin.replace("uncapped", pass));
            ledger(&pins.each_ref().map(String::as_str), rows)
        };
        let dual = book("dual", &[row("dual", &rescued)]);
        assert!(judge(PassId::Dual, &measured, &dual).failures().is_empty());
        for pass in [PassId::Uncapped, PassId::Capped] {
            let name = pass.name();
            let refused =
                format!("{name}: 1 rescue-path wrong outputs (none allowed here, held or not)");
            let mut book = book(name, &[]);
            assert_eq!(
                judge(pass, &measured, &book).failures(),
                [
                    format!("{name}: 1 wrong outputs without a disposition row"),
                    refused.clone()
                ]
            );
            // A row the parser refuses would still excuse nothing here.
            book.rows.push(Disposition {
                pass,
                key: rescued.key.clone(),
                reason: "a test reason".to_owned(),
                line: 1,
            });
            let gate = judge(pass, &measured, &book);
            assert_eq!(gate.held(), 1);
            assert_eq!(gate.failures(), [refused]);
        }
    }

    /// No tracker identifier (`ABC-123`) in a public reason.
    fn names_a_tracker_id(reason: &str) -> bool {
        let bytes = reason.as_bytes();
        bytes.iter().enumerate().any(|(i, &b)| {
            b == b'-'
                && i >= 2
                && bytes[..i]
                    .iter()
                    .rev()
                    .take_while(|c| c.is_ascii_uppercase())
                    .count()
                    >= 2
                && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
        })
    }

    #[test]
    fn the_committed_ledger_names_real_cells() {
        let book = parse_ledger(DISPOSITIONS).unwrap();
        for pass in PassId::ALL {
            let pins = book.pins[pass.ix()];
            assert!(
                pins.floor.is_some()
                    && pins.engine_panics.is_some()
                    && pins.caught_panics.is_some(),
                "{}: every pass pins its strict_correct floor, engine_panics and caught_panics",
                pass.name()
            );
        }
        let cells = build_grid();
        let dual = build_dual_grid(&cells);
        let dual_by_index: BTreeMap<usize, &Cell> =
            dual.iter().map(|cell| (cell.index, cell)).collect();
        for row in &book.rows {
            let cell = match row.pass {
                PassId::Dual => dual_by_index[&row.key.cell],
                PassId::Uncapped | PassId::Capped => &cells[row.key.cell],
            };
            assert_eq!(row.key.label, cell_label(cell), "line {}", row.line);
            assert_eq!(
                row.key.path == OutputPath::Rescue,
                row.key.kind == OutputKind::RescueQr,
                "line {}: a rescue_qr row, and only it, takes the rescue path",
                row.line
            );
            assert!(!names_a_tracker_id(&row.reason), "line {}", row.line);
        }
        assert!(names_a_tracker_id("see ABC-12"));
        assert!(!names_a_tracker_id(
            "rxing ZXing-lineage read published as BYTE_SEGMENTS"
        ));
    }

    #[test]
    fn codeword_map_reads_the_iso_layout() {
        for (version, codewords, remainder) in [(2, 44, 7), (6, 172, 7), (10, 346, 0)] {
            let map = codeword_map(version);
            let side = usize::try_from(version).unwrap() * 4 + 17;
            assert_eq!((map.side, map.codewords), (side, codewords));
            let mut per = vec![0; codewords];
            for &k in map.owner.iter().flatten() {
                per[k] += 1;
            }
            assert!(
                per.iter().all(|&n| n == 8),
                "v{version}: 8 modules a codeword"
            );
            let mut functional = Canvas::new(qrcode::Version::Normal(version), qrcode::EcLevel::L);
            functional.draw_all_functional_patterns();
            let free = (0..side * side)
                .filter(|&m| {
                    functional.get(
                        i16::try_from(m % side).unwrap(),
                        i16::try_from(m / side).unwrap(),
                    ) == Module::Empty
                })
                .count();
            assert_eq!(free, 8 * codewords + remainder, "v{version} remainder bits");
            // ISO placement starts at the bottom-right corner, upward in the
            // rightmost column pair: codeword 0 is its 2×4 bottom block.
            for y in side - 4..side {
                for x in side - 2..side {
                    assert_eq!(map.owner[y * side + x], Some(0), "v{version} ({x},{y})");
                }
            }
            for ec in EC_LEVELS {
                let data = data_codewords(version, ec.ec);
                let (d, e) = qrcode::ec::construct_codewords(
                    &vec![0; data],
                    qrcode::Version::Normal(version),
                    ec.ec,
                )
                .unwrap();
                assert_eq!((d.len(), d.len() + e.len()), (data, codewords));
            }
        }
        let capacities: Vec<usize> = VERSIONS
            .iter()
            .flat_map(|&v| EC_LEVELS.iter().map(move |ec| data_codewords(v, ec.ec)))
            .collect();
        assert_eq!(capacities, [28, 22, 16, 108, 76, 60, 216, 154, 122]);
    }

    #[test]
    fn dual_pass_is_deterministic() {
        let cells = build_grid();
        let dual = build_dual_grid(&cells);
        let again = build_dual_grid(&build_grid());
        let blots = |grid: &[Cell]| -> Vec<(usize, Option<Blot>)> {
            grid.iter().map(|cell| (cell.index, cell.blot)).collect()
        };
        assert_eq!(blots(&dual), blots(&again));
        let gray: Vec<usize> = cells
            .iter()
            .filter(|cell| cell.fill == Fill::Gray)
            .map(|cell| cell.index)
            .collect();
        assert_eq!(gray.len(), 1152);
        let blotted: Vec<&Cell> = dual.iter().filter(|cell| cell.blot.is_some()).collect();
        assert_eq!(
            blotted.iter().map(|cell| cell.index).collect::<Vec<_>>(),
            gray,
            "every gray cell is blotted, in order"
        );
        // The constructed S5 cells follow the blotted ones (no blot of their own).
        assert_eq!(dual.len() - blotted.len(), build_s5_constructed().len());

        let maps: BTreeMap<i16, CodewordMap> =
            VERSIONS.iter().map(|&v| (v, codeword_map(v))).collect();
        let mut on_ec = Vec::new();
        for cell in &blotted {
            let cell = *cell;
            let blot = cell.blot.unwrap();
            let map = &maps[&cell.version];
            let side = map.side;
            let occluded = occluded_modules(cell, side);
            let data = data_codewords(cell.version, cell.ec.ec);
            let mut codewords: Vec<usize> = Vec::new();
            for y in blot.row..blot.row + BLOT_MODULES {
                for x in blot.col..blot.col + BLOT_MODULES {
                    let m = y * side + x;
                    let k = map.owner[m].unwrap_or_else(|| {
                        panic!("{}: ({x},{y}) is a function module", cell_label(cell))
                    });
                    assert!(!occluded[m], "{}: ({x},{y}) is occluded", cell_label(cell));
                    if !codewords.contains(&k) {
                        codewords.push(k);
                    }
                }
            }
            assert!(
                (2..=3).contains(&codewords.len()),
                "{}: {codewords:?}",
                cell_label(cell)
            );
            assert_eq!(
                blot.on_data,
                codewords.iter().all(|&k| k < data),
                "{}",
                cell_label(cell)
            );
            for (m, &hit) in occluded.iter().enumerate() {
                assert!(
                    !hit || map.owner[m].is_none_or(|k| !codewords.contains(&k)),
                    "{}: a blot codeword is also an erasure",
                    cell_label(cell)
                );
            }
            if !blot.on_data {
                on_ec.push(cell_label(cell));
            }
        }
        // Only where the occluder covers every clean data-codeword position:
        // v2-H, off_finder, from 20 % occlusion.
        assert_eq!(on_ec.len(), 20, "{on_ec:?}");
        assert!(
            on_ec
                .iter()
                .all(|label| label.contains("_v2-h-") && label.contains("-off_finder-")),
            "{on_ec:?}"
        );
    }

    /// The dual render is the gray cell's render plus the blot, nothing else.
    #[test]
    fn dual_cells_add_only_their_blot() {
        let cells = build_grid();
        let dual = build_dual_grid(&cells);
        for cell in dual.iter().filter(|c| c.blot.is_some()).step_by(37) {
            let blot = cell.blot.unwrap();
            let base = render_canvas(&cells[cell.index]);
            let with_blot = render_canvas(cell);
            let qz = QUIET_MODULES * MODULE_PX;
            let side = BLOT_MODULES as u32 * MODULE_PX;
            let (x0, y0) = (
                qz + blot.col as u32 * MODULE_PX,
                qz + blot.row as u32 * MODULE_PX,
            );
            for (x, y, px) in with_blot.enumerate_pixels() {
                let in_blot = (x0..x0 + side).contains(&x) && (y0..y0 + side).contains(&y);
                let expected = if in_blot {
                    0
                } else {
                    base.get_pixel(x, y).0[0]
                };
                assert_eq!(px.0[0], expected, "{} ({x},{y})", cell_label(cell));
            }
            assert_eq!(
                cell_label(cell),
                format!(
                    "{}-blot_c{}_r{}",
                    cell_label(&cells[cell.index]),
                    blot.col,
                    blot.row
                )
            );
        }
    }

    /// The constructed S5 cell is deterministic and shaped the way its ledger
    /// row and the scanner both need. Whether the scanner then miscorrects is
    /// a sweep fact, recorded in the gate output and held by its ledger row.
    #[test]
    fn constructed_s5_cell_is_deterministic() {
        // A and B are v2-M single-block (28 data + 16 EC) and differ only from
        // the last data codeword on — the premise the construction asserts.
        let a = v2m_codewords(S5_TRUTH);
        let b = v2m_codewords(S5_NEAR);
        assert_eq!((a.len(), b.len()), (44, 44));
        let first_diff = (0..44).find(|&i| a[i] != b[i]);
        assert_eq!(first_diff, Some(27), "difference starts at D27");

        let built = build_s5_constructed();
        assert_eq!(built.len(), 1, "one constructed cell");
        let cell = &built[0];
        assert_eq!(cell.index, S5_CELL_BASE);
        assert_eq!(cell.truth, S5_TRUTH);
        assert!(cell.blot.is_none());
        assert_eq!(cell_label(cell), "9000_s5-v2m-solved");
        // Deterministic: two builds give byte-identical images.
        let image = &cell.constructed.as_ref().unwrap().image;
        let again = build_s5_constructed();
        assert_eq!(image, &again[0].constructed.as_ref().unwrap().image);
        // 25 modules + 2*6 quiet, 8 px each.
        let side = (2 * 4 + 17 + 2 * QUIET_MODULES as usize) * MODULE_PX as usize;
        assert_eq!(
            (image.width() as usize, image.height() as usize),
            (side, side)
        );
        // The 14 grayed EC codewords put GRAY_LUMA pixels on the canvas.
        assert!(
            image.pixels().any(|p| p.0[0] == GRAY_LUMA),
            "the erasure smudge is present"
        );
        // A clean v2-M symbol of A decodes to A (sanity on the payload/encoder),
        // scanned budget-free like `run()`: no wall-clock cut under load.
        let mut cfg = ScanProfile::Full.config();
        cfg.budget_ms = None;
        let scanner = Scanner::builder().profile(ScanProfile::Custom(cfg)).build();
        let clean = render_damaged_v2m(&a[..28], &a[28..], MaskPattern::VerticalLines, &[]);
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(clean)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let report = scanner.scan(ImageInput::encoded(&png)).unwrap();
        assert!(
            report.detections.iter().any(|d| d.content.text == S5_TRUTH),
            "the undamaged construction decodes to its truth"
        );
    }

    /// None of the lines the gate adds can satisfy (or break) a deep-checks
    /// grep: neither `rescue_succeeded` nor `capped_rescue_decodes` appears.
    #[test]
    fn new_lines_never_match_the_deep_checks_greps() {
        let pass = Pass {
            total: rescued(2, 1),
            outputs: vec![output(4, OutputKind::RescueQr, "abc")],
            ..Pass::default()
        };
        let book = ledger(&PINS, &[row("dual", &output(5, OutputKind::QrWrong, "x"))]);
        let gates: Vec<PassGate<'_>> = PassId::ALL
            .into_iter()
            .map(|id| judge(id, &pass, &book))
            .collect();
        let mut out = String::new();
        write_dual(&mut out, &pass, 3, 1);
        write_gate(&mut out, &book, &gates, &["a failure".to_owned()]);
        assert!(
            out.contains("\n  dual_rescue_decodes   = 3 (correct 2, wrong 1)\n"),
            "{out}"
        );
        assert!(
            out.contains("\ndual_constructed        = 1 codeword-level S5 cells"),
            "{out}"
        );
        assert!(
            out.contains("\ncapped_caught_panics = 0 (pinned none)\n"),
            "{out}"
        );
        assert!(
            out.contains("\nwrong_outputs = 1 (engine_qr 0, extra_qr 0, rescue_qr 1, non_qr 0)\n"),
            "{out}"
        );
        assert!(out.contains("\ncapped_wrong_outputs_open = 1\n"), "{out}");
        assert!(
            out.contains("\ndual_dispositions = 0 held, 1 stale\n"),
            "{out}"
        );
        assert!(out.contains("\nstrict_correct = 0 (floor 5)\n"), "{out}");
        assert!(
            out.contains("\ncapped_engine_panics = 0 (pinned none)\n"),
            "{out}"
        );
        assert!(
            out.contains("\ngate_failures = 1\n  - a failure\n"),
            "{out}"
        );
        assert!(!out.contains("rescue_succeeded"), "{out}");
        assert!(!out.contains("capped_rescue_decodes"), "{out}");
    }

    #[test]
    fn the_output_ledger_prints_lengths_and_the_truth_offset() {
        let truth = "QRCODEAI0123456789abcdefghijklmnopqrstuvwxyz-./:";
        let outputs = account(7, "0007_t", truth, &[qr("abcdef", &[EngineKind::Rxing])]);
        let pass = Pass {
            outputs: outputs.wrong,
            ..Pass::default()
        };
        let book = ledger(&[], &[]);
        let gates = [judge(PassId::Uncapped, &pass, &book)];
        let mut out = String::new();
        write_output_ledger(&mut out, &gates);
        let line = format!(
            "\n| uncapped | 0007 | 0007_t | qr_wrong | engine | qr_code | rxing | 6 | 48 | 18 | {} \
             | open |\n",
            sha("abcdef")
        );
        assert!(out.contains(&line), "{out}");
        assert!(out.starts_with("\n## wrong-output ledger (1 outputs: 0 held, 1 open)\n"));
    }
}
