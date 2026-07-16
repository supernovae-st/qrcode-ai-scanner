//! `advisor` — logo-placement margin lab (QD-9).
//!
//! Answers, with data, one product question: does margin-aware logo
//! placement beat the center-default placement by more than one UEC grade
//! (median)? Today's competitors slap the logo in the center; the scanner is
//! a cost function that can tell us where the erasure hurts least.
//!
//! The lab is a full-factorial grid — 5 payloads × EC {M,H} × logo coverage
//! {~12%,~20% of symbol AREA} × 9 module-space positions (center · 4 quadrant
//! centers · 4 edge midpoints). Each cell renders the QR at a fixed
//! 10 px/module with a 4-module quiet zone, knocks a WHITE box (the realistic
//! worst case — a solid erasure, no fill-in) over the chosen position, and
//! scans the PNG through the PUBLIC scanner with the deepest ladder and NO
//! budget cutoff (a wall-clock budget would make the report machine-timed;
//! `budget_ms = None` is what the crate docs pin as strictly deterministic).
//!
//! Ranking metric is the synthetic UEC margin. If the scanner returns no
//! margin on decoded cells (an rxing-only decode has no bitstream), the lab
//! falls back to the composite `score.value` and says so.

use std::fmt::Write as _;
use std::io::Cursor;

use qrcode_ai_scanner::{Grade, ImageInput, ScanProfile, Scanner, UecGrade};

/// Rendered pixels per QR module.
const SCALE: u32 = 10;
/// Quiet-zone width in modules (ISO/IEC 18004 recommends 4).
const QUIET: u32 = 4;
/// Logo coverage as a percent of the symbol AREA.
const COVERAGE_PCT: [u32; 2] = [12, 20];
/// EC levels under test: the two the builder actually ships for logo codes.
const EC_LEVELS: [(&str, qrcode::EcLevel); 2] =
    [("M", qrcode::EcLevel::M), ("H", qrcode::EcLevel::H)];

/// The 9 placements as fractional symbol-center anchors in quarter-side units
/// (0..=4 → 0 · ¼ · ½ · ¾ · 1 of the symbol side). Index 0 is the center —
/// the baseline every case is measured against. The box is clamped inward so
/// it always stays inside the symbol (edge midpoints would otherwise spill
/// half into the quiet zone). Order is stable: it drives every table.
const POSITIONS: [(&str, u32, u32); 9] = [
    ("center", 2, 2),
    ("quad-tl", 1, 1),
    ("quad-tr", 3, 1),
    ("quad-bl", 1, 3),
    ("quad-br", 3, 3),
    ("edge-t", 2, 0),
    ("edge-b", 2, 4),
    ("edge-l", 0, 2),
    ("edge-r", 4, 2),
];

/// A logo box in module coordinates: top-left `(x0, y0)` and side `s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    s: u32,
}

/// What the scanner measured for one rendered cell.
#[derive(Debug, Clone, Copy)]
struct Measured {
    /// Decoded to the ORIGINAL content (a miscorrection to other text is a
    /// failure, not a success).
    decoded: bool,
    /// Composite 0-100 scannability, when the primary detection was scored.
    score: Option<u8>,
    /// Synthetic UEC margin, when a bitstream was available.
    margin: Option<f32>,
    /// ISO band for the UEC margin.
    uec_grade: Option<UecGrade>,
    /// Composite grade band (the `score.value` interpretation).
    grade: Option<Grade>,
}

/// One grid cell: the placement plus its measurement.
#[derive(Debug, Clone)]
struct Cell {
    payload: &'static str,
    ec: &'static str,
    cov: u32,
    pos: &'static str,
    modules: u32,
    m: Measured,
}

/// The 5 lab payloads: a short and a long URL, a Wi-Fi credential carrying the
/// builder's escaped reserved chars, a `vCard` 3.0, and plain text. Content is
/// the exact QR text — the scan is asserted to return it byte-for-byte.
fn lab_payloads() -> Vec<(&'static str, String)> {
    vec![
        ("short-url", "https://qrcode-ai.com/s/abc12345".to_owned()),
        (
            "long-url",
            "https://qrcode-ai.com/campaign/2026/summer?utm_source=qr&utm_medium=print\
             &utm_campaign=launch&ref=flyer&v=a1b2c3&lang=en"
                .to_owned(),
        ),
        (
            "wifi",
            format!(
                "WIFI:T:WPA;S:{};P:{};H:false;;",
                escape_wifi("Cafe;Wifi"),
                escape_wifi(r#"p:a,s"s"#)
            ),
        ),
        (
            "vcard",
            "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Doe\\;Jr;Jane\\,Ms\r\nFN:Jane Doe\r\n\
             NOTE:line one\\nline two\r\nEND:VCARD"
                .to_owned(),
        ),
        (
            "text",
            "The quick brown fox jumps over 13 lazy dogs near the QR lab.".to_owned(),
        ),
    ]
}

/// The builder's `WIFI:`/`MECARD:` escape (`ZXing` convention): a backslash
/// before each of `\ ; , : "`. Mirrors `escapeWifiField` in the landing repo
/// and the round-trip suite.
fn escape_wifi(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        if matches!(c, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Logo side in modules for a coverage percent of the symbol area:
/// `round(sqrt(coverage) · modules)`, kept in `1..modules`.
fn logo_side(modules: u32, coverage_pct: u32) -> u32 {
    let frac = f64::from(coverage_pct) / 100.0;
    let side = (frac.sqrt() * f64::from(modules)).round() as u32;
    side.clamp(1, modules.saturating_sub(1))
}

/// Box top-left for a quarter-side center anchor, clamped so the `s×s` box
/// stays inside the `n×n` symbol. Pure integer/float math, no rounding drift:
/// the inputs are small exact integers.
fn anchor(frac_times4: u32, n: u32, s: u32) -> u32 {
    let center = f64::from(frac_times4) * f64::from(n) / 4.0;
    let top_left = center - f64::from(s) / 2.0;
    let max = f64::from(n - s);
    top_left.round().clamp(0.0, max) as u32
}

/// The box for one placement in a symbol of `modules` side.
fn place(modules: u32, s: u32, ax4: u32, ay4: u32) -> Rect {
    Rect {
        x0: anchor(ax4, modules, s),
        y0: anchor(ay4, modules, s),
        s,
    }
}

/// Encode content at an EC level (auto version).
fn encode(
    content: &str,
    ec: qrcode::EcLevel,
) -> Result<qrcode::QrCode, Box<dyn std::error::Error>> {
    Ok(qrcode::QrCode::with_error_correction_level(content, ec)?)
}

/// Fill an axis-aligned pixel block with one luma value.
fn fill(img: &mut image::GrayImage, px: u32, py: u32, w: u32, h: u32, v: u8) {
    for y in py..py + h {
        for x in px..px + w {
            img.put_pixel(x, y, image::Luma([v]));
        }
    }
}

/// Render the code at `SCALE` px/module with a `QUIET`-module quiet zone, then
/// knock the WHITE logo box over `rect` (module coordinates → pixels).
fn render(code: &qrcode::QrCode, rect: Rect) -> image::GrayImage {
    let n = code.width() as u32;
    let side = (n + 2 * QUIET) * SCALE;
    let colors = code.to_colors();
    let mut img = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    for (my, row) in colors.chunks(n as usize).enumerate() {
        for (mx, color) in row.iter().enumerate() {
            if *color == qrcode::Color::Dark {
                let px = (QUIET + mx as u32) * SCALE;
                let py = (QUIET + my as u32) * SCALE;
                fill(&mut img, px, py, SCALE, SCALE, 0);
            }
        }
    }
    fill(
        &mut img,
        (QUIET + rect.x0) * SCALE,
        (QUIET + rect.y0) * SCALE,
        rect.s * SCALE,
        rect.s * SCALE,
        255,
    );
    img
}

/// PNG-encode a luma image (the scanner's `encoded` input path — same as the
/// round-trip suite).
fn to_png(img: &image::GrayImage) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut buf = Vec::new();
    image::DynamicImage::ImageLuma8(img.clone())
        .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)?;
    Ok(buf)
}

/// The deepest deterministic scanner: the Full ladder + full stress score,
/// with the wall-clock budget removed so the report is a pure function of the
/// input on a given build.
fn deep_scanner() -> Scanner {
    let mut config = ScanProfile::Full.config();
    config.budget_ms = None;
    Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build()
}

/// Scan one rendered PNG and pull the ranking signals.
fn scan_cell(
    scanner: &Scanner,
    png: &[u8],
    expected: &str,
) -> Result<Measured, Box<dyn std::error::Error>> {
    let report = scanner.scan(ImageInput::encoded(png))?;
    let decoded = report
        .detections
        .first()
        .is_some_and(|d| d.content.text.as_str() == expected);
    let score = report.score.as_ref();
    Ok(Measured {
        decoded,
        score: score.map(|s| s.value),
        margin: score.and_then(|s| s.uec).map(|u| u.margin),
        uec_grade: score.and_then(|s| s.uec).map(|u| u.grade),
        grade: score.map(|s| s.grade),
    })
}

/// Build the whole grid: 5 × 2 × 2 × 9 = 180 measured cells, deterministic
/// order (payload → EC → coverage → position).
fn build_grid() -> Result<Vec<Cell>, Box<dyn std::error::Error>> {
    let scanner = deep_scanner();
    let payloads = lab_payloads();
    let mut cells = Vec::with_capacity(payloads.len() * 2 * 2 * 9);
    for (pname, content) in &payloads {
        for &(ename, ec) in &EC_LEVELS {
            let code = encode(content, ec)?;
            let modules = code.width() as u32;
            for &cov in &COVERAGE_PCT {
                let s = logo_side(modules, cov);
                for &(pos, ax, ay) in &POSITIONS {
                    let png = to_png(&render(&code, place(modules, s, ax, ay)))?;
                    let m = scan_cell(&scanner, &png, content)?;
                    cells.push(Cell {
                        payload: pname,
                        ec: ename,
                        cov,
                        pos,
                        modules,
                        m,
                    });
                }
            }
        }
    }
    Ok(cells)
}

// -------------------------------------------------------------- analysis

/// UEC band as a step count (A=4 … F=0) — the "grade steps" unit.
fn uec_rank(g: UecGrade) -> i32 {
    match g {
        UecGrade::A => 4,
        UecGrade::B => 3,
        UecGrade::C => 2,
        UecGrade::D => 1,
        _ => 0, // F (and any future lowest band)
    }
}

/// Composite band as a step count (Excellent=4 … Poor=0) — the fallback
/// grade-steps unit when no UEC margin is available.
fn grade_rank(g: Grade) -> i32 {
    match g {
        Grade::Excellent => 4,
        Grade::Good => 3,
        Grade::Acceptable => 2,
        Grade::Fair => 1,
        _ => 0, // Poor
    }
}

/// The ranking key for a cell under the active metric (margin, or `score.value`
/// in fallback). `None` for cells that did not decode.
fn rank(m: &Measured, fallback: bool) -> Option<f64> {
    if fallback {
        m.score.map(f64::from)
    } else {
        m.margin.map(f64::from)
    }
}

/// The grade-step value for a cell under the active metric.
fn grade_steps(m: &Measured, fallback: bool) -> Option<i32> {
    if fallback {
        m.grade.map(grade_rank)
    } else {
        m.uec_grade.map(uec_rank)
    }
}

/// Per-case (payload × EC × coverage) center-vs-best summary.
#[derive(Debug, Clone)]
struct CaseSummary {
    payload: &'static str,
    ec: &'static str,
    cov: u32,
    modules: u32,
    center_decoded: bool,
    center_margin: Option<f32>,
    best_pos: &'static str,
    best_margin: Option<f32>,
    delta_rank: Option<f64>,
    delta_margin: Option<f32>,
    delta_grade: Option<i32>,
    center_optimal: bool,
    any_decoded: bool,
}

/// The whole computed picture, ready to print.
#[derive(Debug)]
struct Analysis {
    fallback: bool,
    cases: Vec<CaseSummary>,
    median_gain_rank: Option<f64>,
    median_gain_margin: Option<f64>,
    median_gain_grade: Option<f64>,
    center_suboptimal_pct: f64,
    suboptimal_cases: usize,
    denom_cases: usize,
    center_decode_cases: usize,
    best_decode_cases: usize,
    total_cases: usize,
    decoded_cells: usize,
    total_cells: usize,
    /// Decoded cells whose engine gave no bitstream (no UEC margin).
    marginless_decoded: usize,
}

/// Median of a sample (mean of the two middles for even length).
fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        f64::midpoint(v[n / 2 - 1], v[n / 2])
    })
}

/// Exact widen of a small count to f64 (the whole u32 range is representable,
/// and grid counts never exceed a few hundred).
fn as_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Percentage `100·num/den` (0 when `den` is 0).
fn pct(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        100.0 * as_f64(num) / as_f64(den)
    }
}

/// Reduce one case's 9 cells (center at index 0) to a center-vs-best summary.
fn summarize_case(chunk: &[Cell], fallback: bool) -> CaseSummary {
    let center = &chunk[0];
    // best = strictly-greater wins, so a tie keeps the earliest position
    // (center is first, so center is "optimal" whenever it ties the max).
    let mut best_i: Option<usize> = None;
    let mut best_rank = f64::NEG_INFINITY;
    for (i, c) in chunk.iter().enumerate() {
        if let Some(r) = rank(&c.m, fallback)
            && r > best_rank
        {
            best_rank = r;
            best_i = Some(i);
        }
    }
    let center_rank = rank(&center.m, fallback);
    let center_grade = grade_steps(&center.m, fallback);
    let best = best_i.map(|i| &chunk[i]);
    let best_margin = best.and_then(|b| b.m.margin);
    let best_grade = best.and_then(|b| grade_steps(&b.m, fallback));
    let delta_rank = match (center_rank, best_i.map(|_| best_rank)) {
        (Some(c), Some(b)) => Some(b - c),
        _ => None,
    };
    let delta_margin = match (center.m.margin, best_margin) {
        (Some(c), Some(b)) => Some(b - c),
        _ => None,
    };
    let delta_grade = match (center_grade, best_grade) {
        (Some(c), Some(b)) => Some(b - c),
        _ => None,
    };
    CaseSummary {
        payload: center.payload,
        ec: center.ec,
        cov: center.cov,
        modules: center.modules,
        center_decoded: center.m.decoded,
        center_margin: center.m.margin,
        best_pos: best.map_or("—", |b| b.pos),
        best_margin,
        delta_rank,
        delta_margin,
        delta_grade,
        center_optimal: best_i == Some(0),
        any_decoded: best_i.is_some(),
    }
}

/// Fold the grid into the decision-grade `Analysis`.
fn analyze(cells: &[Cell]) -> Analysis {
    // The ranking metric is the UEC margin. Fall back to `score.value` ONLY
    // when the margin does not materialize on the decoded cells (fewer than
    // half carry one) — a handful of rxing-only decodes (no bitstream ⇒ no
    // UEC) must not discard the margin signal the other 80-odd cells provide.
    let decoded_total = cells.iter().filter(|c| c.m.decoded).count();
    let marginless_decoded = cells
        .iter()
        .filter(|c| c.m.decoded && c.m.margin.is_none())
        .count();
    let fallback = decoded_total > 0 && (decoded_total - marginless_decoded) * 2 < decoded_total;
    let cases: Vec<CaseSummary> = cells
        .chunks(9)
        .map(|chunk| summarize_case(chunk, fallback))
        .collect();

    // Gain medians over cases where the center itself decoded (the "even when
    // center works, does moving help" question). Decode-loss below covers the
    // cases where center fails outright.
    let mut rank_gains = Vec::new();
    let mut margin_gains = Vec::new();
    let mut grade_gains = Vec::new();
    for c in &cases {
        if c.center_decoded && c.any_decoded {
            if let Some(d) = c.delta_rank {
                rank_gains.push(d);
            }
            if let Some(d) = c.delta_margin {
                margin_gains.push(f64::from(d));
            }
            if let Some(d) = c.delta_grade {
                grade_gains.push(f64::from(d));
            }
        }
    }

    let denom_cases = cases.iter().filter(|c| c.any_decoded).count();
    let suboptimal_cases = cases
        .iter()
        .filter(|c| c.any_decoded && !c.center_optimal)
        .count();
    let center_suboptimal_pct = pct(suboptimal_cases, denom_cases);

    Analysis {
        fallback,
        median_gain_rank: median(rank_gains),
        median_gain_margin: median(margin_gains),
        median_gain_grade: median(grade_gains),
        center_suboptimal_pct,
        suboptimal_cases,
        denom_cases,
        center_decode_cases: cases.iter().filter(|c| c.center_decoded).count(),
        best_decode_cases: denom_cases,
        total_cases: cases.len(),
        decoded_cells: cells.iter().filter(|c| c.m.decoded).count(),
        total_cells: cells.len(),
        marginless_decoded,
        cases,
    }
}

// --------------------------------------------------------------- printing

fn f_margin(m: Option<f32>) -> String {
    m.map_or_else(|| "—".to_owned(), |v| format!("{v:.3}"))
}
fn f_score(v: Option<u8>) -> String {
    v.map_or_else(|| "—".to_owned(), |v| v.to_string())
}
fn f_uec(g: Option<UecGrade>) -> String {
    g.map_or_else(|| "—".to_owned(), |g| format!("{g:?}"))
}
fn f_grade(g: Option<Grade>) -> String {
    g.map_or_else(|| "—".to_owned(), |g| format!("{g:?}"))
}
fn f_delta_i(d: Option<i32>) -> String {
    d.map_or_else(|| "—".to_owned(), |v| format!("{v:+}"))
}
fn f_delta_f(d: Option<f32>) -> String {
    d.map_or_else(|| "—".to_owned(), |v| format!("{v:+.3}"))
}
fn f_opt_f(d: Option<f64>) -> String {
    d.map_or_else(|| "n/a".to_owned(), |v| format!("{v:.3}"))
}

/// The active ranking-metric and grade-step labels (they switch under fallback).
fn metric_labels(a: &Analysis) -> (&'static str, &'static str) {
    if a.fallback {
        ("score.value", "composite grade steps")
    } else {
        ("UEC margin", "UEC grade steps")
    }
}

/// Write the headline summary block (the decision surface).
fn write_summary(out: &mut String, a: &Analysis) -> std::fmt::Result {
    let (unit, gunit) = metric_labels(a);
    writeln!(out, "\n## summary\n")?;
    writeln!(
        out,
        "- decoded cells: {}/{} ({:.0}%)",
        a.decoded_cells,
        a.total_cells,
        pct(a.decoded_cells, a.total_cells)
    )?;
    writeln!(
        out,
        "- ranking metric: `{unit}`  ·  grade-step metric: `{gunit}`"
    )?;
    writeln!(
        out,
        "- median gain best-vs-center ({unit}, center-decoded cases): {}",
        f_opt_f(a.median_gain_rank)
    )?;
    writeln!(
        out,
        "- median gain best-vs-center (margin units): {}",
        f_opt_f(a.median_gain_margin)
    )?;
    writeln!(
        out,
        "- median gain best-vs-center ({gunit}): {}",
        f_opt_f(a.median_gain_grade)
    )?;
    writeln!(
        out,
        "- center is NOT optimal: {}/{} decoding cases ({:.0}%)",
        a.suboptimal_cases, a.denom_cases, a.center_suboptimal_pct
    )?;
    writeln!(
        out,
        "- decode-loss center vs best: center decodes {}/{} cases ({:.0}%) · \
         best position decodes {}/{} cases ({:.0}%) · delta {:+.0} pts",
        a.center_decode_cases,
        a.total_cases,
        pct(a.center_decode_cases, a.total_cases),
        a.best_decode_cases,
        a.total_cases,
        pct(a.best_decode_cases, a.total_cases),
        pct(a.best_decode_cases, a.total_cases) - pct(a.center_decode_cases, a.total_cases),
    )?;
    let verdict = a.median_gain_grade.map_or_else(
        || "no gradeable cases".to_owned(),
        |g| {
            format!(
                "median grade gain {g:.2} {gunit} — margin-aware placement {} the >1-grade bar",
                if g > 1.0 { "BEATS" } else { "does NOT beat" }
            )
        },
    );
    writeln!(out, "- HYPOTHESIS (>1 grade median): {verdict}")
}

/// Render the full markdown report (raw grid + per-case summary + headline).
fn render_report(cells: &[Cell], a: &Analysis) -> Result<String, std::fmt::Error> {
    let (unit, _) = metric_labels(a);
    let mut out = String::new();

    writeln!(out, "# advisor — logo-placement margin lab (QD-9)\n")?;
    writeln!(
        out,
        "render {SCALE} px/module · {QUIET}-module quiet zone · white knockout box · \
         scanner profile Full · budget_ms=None (deterministic)"
    )?;
    writeln!(
        out,
        "grid {} cells · ranking metric `{unit}`{}\n",
        a.total_cells,
        if a.fallback {
            " (FALLBACK: some decoded cells had no UEC margin)"
        } else {
            ""
        }
    )?;

    // Raw grid.
    writeln!(out, "## raw grid ({} cells)\n", cells.len())?;
    writeln!(
        out,
        "| payload | ec | cov% | pos | modules | decoded | score | margin | uec | grade |"
    )?;
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|")?;
    for c in cells {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.payload,
            c.ec,
            c.cov,
            c.pos,
            c.modules,
            if c.m.decoded { "yes" } else { "no" },
            f_score(c.m.score),
            f_margin(c.m.margin),
            f_uec(c.m.uec_grade),
            f_grade(c.m.grade),
        )?;
    }

    // Per-case center-vs-best.
    writeln!(
        out,
        "\n## per-case center vs best ({} cases)\n",
        a.cases.len()
    )?;
    writeln!(
        out,
        "| payload | ec | cov% | modules | center dec | center margin | best pos | \
         best margin | Δmargin | Δgrade | center optimal |"
    )?;
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|---|")?;
    for c in &a.cases {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            c.payload,
            c.ec,
            c.cov,
            c.modules,
            if c.center_decoded { "yes" } else { "no" },
            f_margin(c.center_margin),
            c.best_pos,
            f_margin(c.best_margin),
            f_delta_f(c.delta_margin),
            f_delta_i(c.delta_grade),
            if c.center_optimal { "yes" } else { "no" },
        )?;
    }

    write_summary(&mut out, a)?;
    Ok(out)
}

/// Run the lab and print the report to stdout.
///
/// # Errors
/// Propagates QR encode, PNG encode, and scan faults — a fixed lab payload
/// that fails to encode or a rendered PNG the scanner rejects is a real bug,
/// not a data point.
pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cells = build_grid()?;
    let analysis = analyze(&cells);
    print!("{}", render_report(&cells, &analysis)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// (a) The placement grid is 9 unique, in-bounds boxes for a symbol size.
    #[test]
    fn placement_grid_is_nine_unique_in_bounds_rects() {
        let n = 33; // a representative symbol side
        let s = logo_side(n, 20);
        let rects: Vec<Rect> = POSITIONS
            .iter()
            .map(|&(_, ax, ay)| place(n, s, ax, ay))
            .collect();
        assert_eq!(rects.len(), 9);
        for r in &rects {
            assert!(
                r.x0 + r.s <= n && r.y0 + r.s <= n,
                "box out of bounds in a {n}-module symbol: {r:?}"
            );
        }
        let unique: std::collections::HashSet<(u32, u32, u32)> =
            rects.iter().map(|r| (r.x0, r.y0, r.s)).collect();
        assert_eq!(unique.len(), 9, "placements collapsed: {rects:?}");
    }

    /// (b) Determinism: the same cell scanned twice is bit-identical. Compares
    /// the margin by bit pattern — determinism means exactly identical, not
    /// approximately equal.
    #[test]
    fn scanning_a_cell_twice_is_identical() {
        let scanner = deep_scanner();
        let content = "https://qrcode-ai.com/s/det123";
        let code = encode(content, qrcode::EcLevel::M).unwrap();
        let n = code.width() as u32;
        let s = logo_side(n, 12);
        let png = to_png(&render(&code, place(n, s, 1, 1))).unwrap();
        let a = scan_cell(&scanner, &png, content).unwrap();
        let b = scan_cell(&scanner, &png, content).unwrap();
        assert_eq!(a.decoded, b.decoded);
        assert_eq!(a.score, b.score);
        assert_eq!(a.margin.map(f32::to_bits), b.margin.map(f32::to_bits));
        assert_eq!(a.uec_grade, b.uec_grade);
    }

    /// (c) Sanity: the instrument actually measures placement. On the pinned
    /// hard case (the `wifi` payload · EC-H · 20%), at least two positions differ.
    #[test]
    fn placements_actually_move_the_needle() {
        let scanner = deep_scanner();
        let content = lab_payloads()
            .into_iter()
            .find(|(name, _)| *name == "wifi")
            .map(|(_, c)| c)
            .expect("wifi payload present");
        let code = encode(&content, qrcode::EcLevel::H).unwrap();
        let n = code.width() as u32;
        let s = logo_side(n, 20);
        // center (misses finders) vs quad-tl (lands on the TL finder) vs edge-r.
        let probe = [POSITIONS[0], POSITIONS[1], POSITIONS[8]];
        let mut seen = std::collections::HashSet::new();
        for &(_, ax, ay) in &probe {
            let png = to_png(&render(&code, place(n, s, ax, ay))).unwrap();
            let cell = scan_cell(&scanner, &png, &content).unwrap();
            seen.insert((cell.decoded, cell.score, cell.margin.map(f32::to_bits)));
        }
        assert!(
            seen.len() >= 2,
            "placement must change the measurement — got {seen:?}"
        );
    }
}
