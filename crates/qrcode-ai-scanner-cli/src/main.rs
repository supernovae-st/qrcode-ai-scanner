//! `qrscan` — scan and validate QR codes from the command line.
//!
//! JSON (the `ScanReport` contract) by default; `--pretty` for humans.
//! Exit codes: 0 = QR found · 1 = no QR found · 2 = invalid input/usage/error ·
//! 3 = --score-only: decoded, not judged within the budget.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use qrcode_ai_scanner::{
    AlphaBackground, Charset, DecodedContent, ImageInput, ScanConfig, ScanProfile, ScanReport,
    Scanner, ScoreCheck, ScoreDepth, ScorePreset, StressAxis,
};

mod term;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProfileArg {
    /// Full ladder + full stress score (default).
    Full,
    /// Reduced ladder + reduced score.
    Fast,
    /// Decode only, tight budget, no score.
    Frame,
}

impl From<ProfileArg> for ScanProfile {
    fn from(arg: ProfileArg) -> Self {
        match arg {
            ProfileArg::Full => Self::Full,
            ProfileArg::Fast => Self::Fast,
            ProfileArg::Frame => Self::Frame,
        }
    }
}

/// Scan a QR code image: decode + scannability score + hints.
#[derive(Parser, Debug)]
#[command(name = "qrscan", version, about, styles = term::clap_styles())]
struct Cli {
    /// Image file (PNG, JPEG, WebP, GIF).
    image: PathBuf,

    /// Scan profile.
    #[arg(long, value_enum, default_value = "full")]
    profile: ProfileArg,

    /// Override the profile's wall-clock budget in ms (0 = unbounded; unset =
    /// the profile's 4000 · 800 · 80 ms). Unbounded is the strictly
    /// reproducible mode: with a budget, WHERE the scan cuts is
    /// machine-dependent and a cut report says so (`trace.budget_exhausted:
    /// true`). With --score-only, a decoded QR the budget left unjudged exits
    /// 3 (spec/02, spec/03).
    #[arg(long)]
    budget_ms: Option<u32>,

    /// Stress axes to exclude from scoring (comma-separated wire names,
    /// e.g. `perspective,rotation`). Skipped axes never run and the
    /// composite renormalizes engine-side — the generated-preview
    /// integration config (spec/04 § skipping axes).
    #[arg(long, value_delimiter = ',', value_name = "AXIS,...")]
    score_skip_axes: Vec<String>,

    /// Named axis posture: `design` (generated previews — skips
    /// perspective+rotation, keeps lighting) or `capture` (all six).
    /// Mutually exclusive with --score-skip-axes.
    #[arg(long, value_name = "PRESET", conflicts_with = "score_skip_axes")]
    score_preset: Option<String>,

    /// Report SECTIONS to withhold (comma-separated: `uec`, `iso15415`,
    /// `alpha_envelope`). The wire carries null and the section-driven hints
    /// never fire; the UEC margin is still measured and caps the value, so
    /// the composite never moves (spec/04 § skipping checks).
    #[arg(long, value_delimiter = ',', value_name = "CHECK,...")]
    score_skip_checks: Vec<String>,

    /// Background flattened under transparent pixels: `auto` (default —
    /// the design's own content picks it, with an opposite-background
    /// retry on zero detections), `white`, `black`, `#rrggbb` (the real
    /// placement surface), or `none` (pre-0.9 drop-the-channel behavior).
    /// Opaque inputs are untouched (spec/01 § alpha).
    #[arg(long, value_name = "MODE")]
    alpha_background: Option<String>,

    /// Theme/brand colors probed by the placement envelope in the SAME
    /// scan (comma-separated: `white`, `black`, `#rrggbb`) — per-color
    /// verdicts land in `alpha.envelope.palette`.
    #[arg(long, value_delimiter = ',', value_name = "COLOR,...")]
    alpha_palette: Vec<String>,

    /// Human-readable summary instead of JSON.
    #[arg(long, short = 'p')]
    pretty: bool,

    /// Print only the composite score value (scripts). Exit codes: 0 = the
    /// value on stdout · 1 = no QR found · 2 = no score in this configuration,
    /// or an internal fault · 3 = a decoded QR the budget left unjudged
    /// (nothing on stdout; retry with a larger --budget-ms, 0 = unbounded).
    #[arg(long, short = 's')]
    score_only: bool,
}

/// What `--score-only` prints and exits with on the resolved configuration
/// (spec/03 § CLI exit codes) — the first match wins.
#[derive(Debug, PartialEq, Eq)]
enum ScoreOnly {
    /// The value alone on stdout (exit 0).
    Value(u8),
    /// No detection: a plain notice on stderr (exit 1).
    NotFound(String),
    /// No value to print: the reason on stderr after the error mark, and
    /// its exit code (2: no score in this configuration, or a fault · 3:
    /// decoded, not judged within the budget).
    Refused(u8, String),
}

/// The budget a cut ran under, as the messages name it.
fn budget_phrase(config: &ScanConfig) -> String {
    match config.budget_ms {
        Some(ms) => format!("the {ms} ms budget"),
        None => "the budget".to_owned(),
    }
}

/// Whether the report says the budget cut planned work.
fn cut(report: &ScanReport) -> bool {
    report.trace.budget_exhausted == Some(true)
}

/// `--score-only` on `report`, scanned under `config`: a value is a
/// complete judgment; a missing one names its cause, never prints a zero.
fn score_only(report: &ScanReport, config: &ScanConfig) -> ScoreOnly {
    if let Some(score) = &report.score {
        return ScoreOnly::Value(score.value);
    }
    if report.detections.is_empty() {
        return ScoreOnly::NotFound(if cut(report) {
            format!(
                "no QR code found within {} — no score (the search was cut: raise \
                 --budget-ms, 0 = unbounded)",
                budget_phrase(config)
            )
        } else {
            "no QR code found — no score".to_owned()
        });
    }
    if !config.computes_score() {
        return ScoreOnly::Refused(
            2,
            if config.score_depth == ScoreDepth::Off {
                "the frame profile computes no score (use --profile full|fast)".to_owned()
            } else {
                "every stress axis is skipped — no score to print (drop an axis from \
                 --score-skip-axes)"
                    .to_owned()
            },
        );
    }
    if cut(report) {
        return ScoreOnly::Refused(
            3,
            format!(
                "not judged: {} ran out before the score was complete — raise --budget-ms \
                 (0 = unbounded)",
                budget_phrase(config)
            ),
        );
    }
    ScoreOnly::Refused(
        2,
        "the score computation failed (contained internal fault) — please report it with \
         the image"
            .to_owned(),
    )
}

/// The `--pretty` line that stands for a missing score beside detections.
fn missing_score_line(report: &ScanReport, config: &ScanConfig) -> Option<String> {
    if report.score.is_some() || report.detections.is_empty() {
        return None;
    }
    Some(if !config.computes_score() {
        let reason = if config.score_depth == ScoreDepth::Off {
            "frame profile"
        } else {
            "every stress axis skipped"
        };
        format!("score     not computed ({reason})")
    } else if cut(report) {
        format!(
            "score     not judged — {} ran out (--budget-ms 0 = unbounded)",
            budget_phrase(config)
        )
    } else {
        "score     unavailable — internal fault (please report)".to_owned()
    })
}

/// The `--pretty` line of a scan that found nothing: final, or cut.
fn not_found_line(report: &ScanReport, config: &ScanConfig) -> String {
    if cut(report) {
        format!(
            "no QR code found within {} — not final (--budget-ms 0 = unbounded)",
            budget_phrase(config)
        )
    } else {
        "no QR code found".to_owned()
    }
}

/// The `--pretty` line of a report the budget cut that no other line says
/// is partial: detections, and no score because the configuration computes
/// none — the not-found, not-judged and budget lines say it themselves.
fn cut_line(report: &ScanReport, config: &ScanConfig) -> Option<String> {
    let said = report.detections.is_empty() || report.score.is_some() || config.computes_score();
    (cut(report) && !said).then(|| {
        let budget = config
            .budget_ms
            .map_or_else(|| "the budget".to_owned(), |ms| format!("{ms} ms"));
        format!("budget    {budget} exhausted — not final (--budget-ms 0 = unbounded)")
    })
}

/// The `--pretty` line of a complete score whose report the budget cut.
fn budget_line(report: &ScanReport, config: &ScanConfig) -> Option<String> {
    (report.score.is_some() && cut(report)).then(|| {
        let budget = config
            .budget_ms
            .map_or_else(|| "the budget".to_owned(), |ms| format!("{ms} ms"));
        format!(
            "budget    {budget} exhausted — the score is complete; knee refinements or \
             envelope probes were cut"
        )
    })
}

/// Decoded text is ATTACKER-CONTROLLED — never echo terminal controls
/// (ANSI/OSC escape injection) or spoofing format characters, and never
/// let a character pass for another: each is shown escaped as `\u{…}`, a
/// backslash as `\\`, and a U+FFFD the data encodes as `\u{fffd}` — only
/// under `unknown` is a U+FFFD the scanner's mark for an unread byte, shown
/// as the glyph. JSON output is safe (serde escapes); only this human path
/// needs it.
fn sanitize_terminal(text: &str, charset: Charset) -> String {
    // Cc/C1 controls + the format (Cf) spoofers: bidi overrides flip the
    // visual order of the displayed URL, zero-widths hide segments.
    fn is_spoofing_format(c: char) -> bool {
        matches!(c,
            '\u{200B}'..='\u{200F}' // zero-widths + LRM/RLM
            | '\u{202A}'..='\u{202E}' // bidi embedding/override
            | '\u{2060}'..='\u{2069}' // word-joiner + invisibles + bidi isolates
            | '\u{FEFF}')
    }
    let mut shown = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\\' {
            shown.push_str("\\\\");
        } else if (c.is_control() && c != '\t')
            || is_spoofing_format(c)
            || (c == char::REPLACEMENT_CHARACTER && !charset.is_lossy())
        {
            shown.extend(c.escape_unicode());
        } else {
            shown.push(c);
        }
    }
    shown
}

/// A wire name of the report (`charset`, `fnc1`), as JSON spells it.
fn wire_name(value: serde_json::Result<serde_json::Value>) -> String {
    value
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unrecognized".to_owned())
}

/// The pretty view's account of a content: a `charset` line when the
/// reading is not plain UTF-8 or a declaration was observed (the
/// designators and where each takes effect; a declaration signalled but
/// not located; no verified text), and an `fnc1` line when one was
/// observed.
fn content_lines(content: &DecodedContent) -> Vec<String> {
    let mut lines = Vec::new();
    if content.charset != Charset::Utf8 || content.eci.is_some() {
        let mut line = format!(
            "charset   {}",
            wire_name(serde_json::to_value(content.charset))
        );
        match content.eci.as_deref() {
            Some([]) => line.push_str(" · ECI signalled, not located"),
            Some(segments) => {
                let listed: Vec<String> = segments
                    .iter()
                    .map(|segment| format!("{}@{}", segment.designator, segment.start))
                    .collect();
                line.push_str(" · ECI ");
                line.push_str(&listed.join(" "));
            }
            None => {}
        }
        if content.charset.is_lossy() {
            line.push_str(" · no verified text (raw: in the JSON report, run without --pretty)");
        }
        lines.push(line);
    }
    if let Some(fnc1) = content.fnc1 {
        lines.push(format!(
            "fnc1      {}",
            wire_name(serde_json::to_value(fnc1))
        ));
    }
    lines
}

/// The alpha block of the pretty view: the flatten verdict line + the
/// placement-envelope strip (every probe a tested background).
fn render_alpha(alpha: &qrcode_ai_scanner::AlphaReport) {
    println!(
        "alpha     over {} · {:?} · coverage {:.0}% · content luma {}{}",
        alpha.background,
        alpha.mode,
        alpha.coverage * 100.0,
        alpha.content_luma,
        if alpha.fallback_used {
            " · rescued by the opposite background"
        } else {
            ""
        }
    );
    if let Some(envelope) = &alpha.envelope {
        let strip = envelope
            .probes
            .iter()
            .map(|p| {
                let mark = if p.decoded { '✓' } else { '✕' };
                format!("{mark}{}", p.background_luma)
            })
            .collect::<Vec<_>>()
            .join(" ");
        let bands = envelope
            .safe_luma
            .iter()
            .map(|b| format!("{}-{}", b[0], b[1]))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  placement {:?} · safe luma {} · probes {strip}",
            envelope.placement,
            if bands.is_empty() {
                "none".to_owned()
            } else {
                bands
            },
        );
        if !envelope.palette.is_empty() {
            let verdicts = envelope
                .palette
                .iter()
                .map(|p| {
                    let mark = if p.decoded { '✓' } else { '✕' };
                    format!("{mark}{}", p.background)
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("  palette   {verdicts}");
        }
    }
}

fn render_pretty(report: &ScanReport, config: &ScanConfig) {
    let th = term::Theme::auto();
    match report.detections.first() {
        Some(d) => {
            println!(
                "content   {}",
                sanitize_terminal(&d.content.text, d.content.charset)
            );
            for line in content_lines(&d.content) {
                println!("{line}");
            }
            println!("symbology {:?}", d.symbology);
            println!("payload   {:?}", d.payload);
            if let (Some(v), Some(m)) = (d.meta.version, d.meta.modules) {
                println!("symbol    v{v} · {m}x{m} modules");
            }
            println!(
                "engines   {}",
                d.engines
                    .iter()
                    .map(|e| format!("{e:?}").to_lowercase())
                    .collect::<Vec<_>>()
                    .join("+")
            );
        }
        None => println!("{}", th.warn(&not_found_line(report, config))),
    }
    if let Some(line) = missing_score_line(report, config) {
        println!("{line}");
    }
    if let Some(line) = cut_line(report, config) {
        println!("{}", th.warn(&line));
    }
    if let Some(score) = &report.score {
        let grade = format!("{:?}", score.grade);
        let coverage = if score.weights_run < 100 {
            format!(" \u{b7} {}% of contract", score.weights_run)
        } else {
            String::new()
        };
        println!(
            "score     {} ({}{})",
            th.grade_text(&grade, &format!("{}/100", score.value)),
            grade,
            coverage
        );
        for axis in &score.axes {
            let knee = match (&axis.refined_failed_at, &axis.failed_at) {
                (Some(refined), _) => format!(" \u{b7} dies at {refined}"),
                (None, Some(failed)) => format!(" \u{b7} failed {failed}"),
                _ => String::new(),
            };
            println!(
                "  {:12} {}/{}{}",
                format!("{:?}", axis.axis).to_lowercase(),
                axis.passed,
                axis.total,
                knee
            );
        }
        if let Some(uec) = score.uec {
            println!(
                "  uec margin {:.2} (grade {:?} · worst block {}/{} ec)",
                uec.margin, uec.grade, uec.worst_block_errors, uec.worst_block_capacity
            );
        }
        if let Some(line) = budget_line(report, config) {
            println!("{}", th.dim(&line));
        }
        if let Some(iso) = score.iso15415 {
            println!(
                "iso15415  overall {:?} (informed, not certified)",
                iso.overall
            );
            println!(
                "  contrast {:.2}/{:?} · modulation {:.2}/{:?} · axial {:.3}/{:?} · fixed-pattern {:.2}/{:?}",
                iso.symbol_contrast.value,
                iso.symbol_contrast.grade,
                iso.modulation.value,
                iso.modulation.grade,
                iso.axial_nonuniformity.value,
                iso.axial_nonuniformity.grade,
                iso.fixed_pattern_damage.value,
                iso.fixed_pattern_damage.grade,
            );
        }
    }
    if let Some(alpha) = &report.alpha {
        render_alpha(alpha);
    }
    for hint in &report.hints {
        println!("hint      {hint:?}");
    }
    println!(
        "{}",
        th.dim(&format!("took      {:.0}ms", report.trace.total_ms))
    );
}

/// Resolve `--score-preset` OR `--score-skip-axes` into the skip list
/// (clap's `conflicts_with` guarantees at most one arrived).
fn parse_axis_posture(axes: &[String], preset: Option<&str>) -> Result<Vec<StressAxis>, ExitCode> {
    if let Some(name) = preset {
        let Some(preset) = ScorePreset::from_name(name) else {
            let th = term::Theme::auto_stderr();
            eprintln!(
                "{} unknown score preset `{name}` — expected design | capture",
                th.err_strong("✖")
            );
            return Err(ExitCode::from(2));
        };
        return Ok(preset.skips());
    }
    parse_skip_axes(axes)
}

/// Parse `--score-skip-axes` names with the CLI's loud-typo posture.
fn parse_skip_axes(args: &[String]) -> Result<Vec<StressAxis>, ExitCode> {
    let mut skip = Vec::with_capacity(args.len());
    for name in args {
        let Some(axis) = StressAxis::from_name(name) else {
            let th = term::Theme::auto_stderr();
            eprintln!(
                "{} unknown stress axis `{name}` — expected resolution | blur | \
                 contrast | perspective | rotation | lighting",
                th.err_strong("✖")
            );
            return Err(ExitCode::from(2));
        };
        skip.push(axis);
    }
    Ok(skip)
}

/// Parse `--score-skip-checks` names with the CLI's loud-typo posture.
fn parse_skip_checks(args: &[String]) -> Result<Vec<ScoreCheck>, ExitCode> {
    let mut checks = Vec::with_capacity(args.len());
    for name in args {
        let Some(check) = ScoreCheck::from_name(name) else {
            let th = term::Theme::auto_stderr();
            eprintln!(
                "{} unknown score check `{name}` — expected uec | iso15415 | alpha_envelope",
                th.err_strong("✖")
            );
            return Err(ExitCode::from(2));
        };
        checks.push(check);
    }
    Ok(checks)
}

/// Parse `--alpha-palette` colors with the CLI's loud-typo posture.
fn parse_palette_args(args: &[String]) -> Result<Vec<[u8; 3]>, ExitCode> {
    if args.len() > qrcode_ai_scanner::ScanConfig::MAX_ALPHA_PALETTE {
        let th = term::Theme::auto_stderr();
        eprintln!(
            "{} alpha palette too large: {} colors (max {})",
            th.err_strong("✖"),
            args.len(),
            qrcode_ai_scanner::ScanConfig::MAX_ALPHA_PALETTE
        );
        return Err(ExitCode::from(2));
    }
    let mut palette = Vec::with_capacity(args.len());
    for name in args {
        let Some(color) = AlphaBackground::palette_color(name) else {
            let th = term::Theme::auto_stderr();
            eprintln!(
                "{} unknown palette color `{name}` — expected white | black | #rrggbb",
                th.err_strong("✖")
            );
            return Err(ExitCode::from(2));
        };
        palette.push(color);
    }
    Ok(palette)
}

/// Parse `--alpha-background` with the CLI's loud-typo posture.
fn parse_alpha_arg(arg: Option<&str>) -> Result<Option<AlphaBackground>, ExitCode> {
    let Some(name) = arg else {
        return Ok(None);
    };
    if let Some(mode) = AlphaBackground::from_name(name) {
        return Ok(Some(mode));
    }
    let th = term::Theme::auto_stderr();
    eprintln!(
        "{} unknown alpha background `{name}` — expected auto | white | black | none | #rrggbb",
        th.err_strong("✖")
    );
    Err(ExitCode::from(2))
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let bytes = match std::fs::read(&cli.image) {
        Ok(bytes) => bytes,
        Err(error) => {
            let th = term::Theme::auto_stderr();
            eprintln!(
                "{} cannot read {}: {error}",
                th.err_strong("✖"),
                cli.image.display()
            );
            return ExitCode::from(2);
        }
    };

    // --score-skip-axes: wire names, loud on a typo (a silently ignored
    // axis would score all six and drift the caller's contract).
    // preset and explicit list resolve in one seam (clap enforces exclusivity)
    let skip = match parse_axis_posture(&cli.score_skip_axes, cli.score_preset.as_deref()) {
        Ok(skip) => skip,
        Err(code) => return code,
    };
    // --score-skip-checks: same loud-typo posture for report sections.
    let checks = match parse_skip_checks(&cli.score_skip_checks) {
        Ok(checks) => checks,
        Err(code) => return code,
    };
    // --alpha-background: same loud posture — a silently-defaulted typo
    // would flip which image the whole ladder sees.
    let alpha = match parse_alpha_arg(cli.alpha_background.as_deref()) {
        Ok(alpha) => alpha,
        Err(code) => return code,
    };
    // --alpha-palette: per-color envelope verdicts, loud on a bad color.
    let palette = match parse_palette_args(&cli.alpha_palette) {
        Ok(palette) => palette,
        Err(code) => return code,
    };
    // --budget-ms overrides the preset's wall-clock budget (0 = unbounded) —
    // the same semantics as Node `budgetMs` / WASM / Python / UniFFI.
    let profile = if cli.budget_ms.is_none()
        && skip.is_empty()
        && checks.is_empty()
        && alpha.is_none()
        && palette.is_empty()
        && cli.score_preset.is_none()
    {
        cli.profile.into()
    } else {
        let mut config = ScanProfile::from(cli.profile).config();
        if let Some(ms) = cli.budget_ms {
            config.budget_ms = (ms > 0).then_some(u64::from(ms));
        }
        config.score_skip_axes = skip;
        config.score_skip_checks = checks;
        if let Some(alpha) = alpha {
            config.alpha_background = alpha;
        }
        config.alpha_palette = palette;
        ScanProfile::Custom(config)
    };
    // the configuration the scan runs under: what --score-only and --pretty
    // read a missing score against
    let config = profile.clone().config();
    let scanner = Scanner::builder().profile(profile).build();
    let report = match scanner.scan(ImageInput::encoded(&bytes)) {
        Ok(report) => report,
        Err(error) => {
            let th = term::Theme::auto_stderr();
            eprintln!("{} {} [{}]", th.err_strong("✖"), error, error.code());
            return ExitCode::from(2);
        }
    };

    if cli.score_only {
        // No QR found is exit 1 (the header contract), even score-only; a
        // missing score is never printed as a value — 0 would read as a
        // true zero score
        return match score_only(&report, &config) {
            ScoreOnly::Value(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            ScoreOnly::NotFound(notice) => {
                eprintln!("{notice}");
                ExitCode::from(1)
            }
            ScoreOnly::Refused(code, reason) => {
                let th = term::Theme::auto_stderr();
                eprintln!("{} {reason}", th.err_strong("✖"));
                ExitCode::from(code)
            }
        };
    }
    if cli.pretty {
        render_pretty(&report, &config);
    } else {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                let th = term::Theme::auto_stderr();
                eprintln!("{} serialization failed: {error}", th.err_strong("✖"));
                return ExitCode::from(2);
            }
        }
    }

    if report.detections.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use qrcode_ai_scanner::ScanConfig;

    /// A report as the wire carries it: one QR detection or none, a score
    /// or null, the budget flag.
    fn report(detected: bool, scored: bool, cut: bool) -> ScanReport {
        let detection = r#"{"symbology":"qr_code",
            "content":{"text":"hi","raw":"aGk=","charset":"utf8"},
            "payload":{"kind":"text"},"corners":null,
            "meta":{"version":null,"ec_level":null,"mask":null,"modules":null,"inverted":null},
            "engines":["rxing"]}"#;
        let score = r#"{"value":87,"grade":"excellent","weights_run":100,"axes":[],
            "structural":null,"uec":null,"iso15415":null}"#;
        let json = format!(
            r#"{{"detections":[{}],"score":{},"hints":[],
            "trace":{{"stages":[],"engine_panics":0,"total_ms":0.0,"budget_exhausted":{cut}}},
            "versions":{{"scanner":"0.9.0","pipeline":3,"score_contract":5}}}}"#,
            if detected { detection } else { "" },
            if scored { score } else { "null" },
        );
        serde_json::from_str(&json).unwrap()
    }

    fn every_axis_skipped() -> ScanConfig {
        let mut config = ScanConfig::full();
        config.score_skip_axes = [
            StressAxis::Resolution,
            StressAxis::Blur,
            StressAxis::Contrast,
            StressAxis::Perspective,
            StressAxis::Rotation,
            StressAxis::Lighting,
        ]
        .to_vec();
        config
    }

    #[test]
    fn score_only_follows_the_first_matching_reason() {
        let full = ScanConfig::full();
        // a present score is printed, cut or not
        for cut in [false, true] {
            assert_eq!(
                score_only(&report(true, true, cut), &full),
                ScoreOnly::Value(87)
            );
        }
        // no detection: the 0.9 notice, or the cut search
        assert_eq!(
            score_only(&report(false, false, false), &full),
            ScoreOnly::NotFound("no QR code found — no score".to_owned())
        );
        assert_eq!(
            score_only(&report(false, false, true), &full),
            ScoreOnly::NotFound(
                "no QR code found within the 4000 ms budget — no score (the search was cut: \
                 raise --budget-ms, 0 = unbounded)"
                    .to_owned()
            )
        );
        // a configuration that computes no score names its cause
        assert_eq!(
            score_only(&report(true, false, true), &ScanConfig::frame()),
            ScoreOnly::Refused(
                2,
                "the frame profile computes no score (use --profile full|fast)".to_owned()
            )
        );
        assert_eq!(
            score_only(&report(true, false, false), &every_axis_skipped()),
            ScoreOnly::Refused(
                2,
                "every stress axis is skipped — no score to print (drop an axis from \
                 --score-skip-axes)"
                    .to_owned()
            )
        );
        // decoded, not judged within the budget
        assert_eq!(
            score_only(&report(true, false, true), &ScanConfig::fast()),
            ScoreOnly::Refused(
                3,
                "not judged: the 800 ms budget ran out before the score was complete — \
                 raise --budget-ms (0 = unbounded)"
                    .to_owned()
            )
        );
        // decoded, nothing cut, still no score: a contained fault
        assert_eq!(
            score_only(&report(true, false, false), &full),
            ScoreOnly::Refused(
                2,
                "the score computation failed (contained internal fault) — please report \
                 it with the image"
                    .to_owned()
            )
        );
    }

    #[test]
    fn pretty_says_why_a_score_is_missing_and_when_the_budget_cut() {
        let full = ScanConfig::full();
        assert_eq!(
            missing_score_line(&report(true, false, false), &ScanConfig::frame()).as_deref(),
            Some("score     not computed (frame profile)")
        );
        assert_eq!(
            missing_score_line(&report(true, false, false), &every_axis_skipped()).as_deref(),
            Some("score     not computed (every stress axis skipped)")
        );
        assert_eq!(
            missing_score_line(&report(true, false, true), &full).as_deref(),
            Some("score     not judged — the 4000 ms budget ran out (--budget-ms 0 = unbounded)")
        );
        assert_eq!(
            missing_score_line(&report(true, false, false), &full).as_deref(),
            Some("score     unavailable — internal fault (please report)")
        );
        assert_eq!(missing_score_line(&report(true, true, true), &full), None);
        assert_eq!(missing_score_line(&report(false, false, true), &full), None);
        assert_eq!(
            not_found_line(&report(false, false, false), &full),
            "no QR code found"
        );
        assert_eq!(
            not_found_line(&report(false, false, true), &full),
            "no QR code found within the 4000 ms budget — not final (--budget-ms 0 = unbounded)"
        );
        assert_eq!(
            budget_line(&report(true, true, true), &full).as_deref(),
            Some(
                "budget    4000 ms exhausted — the score is complete; knee refinements or \
                 envelope probes were cut"
            )
        );
        assert_eq!(budget_line(&report(true, true, false), &full), None);
        assert_eq!(budget_line(&report(true, false, true), &full), None);
    }

    #[test]
    fn pretty_says_a_cut_once_whatever_else_it_prints() {
        // no score planned: the missing-score line names the configuration,
        // so the cut needs a line of its own
        assert_eq!(
            cut_line(&report(true, false, true), &ScanConfig::frame()).as_deref(),
            Some("budget    80 ms exhausted — not final (--budget-ms 0 = unbounded)")
        );
        assert_eq!(
            cut_line(&report(true, false, true), &every_axis_skipped()).as_deref(),
            Some("budget    4000 ms exhausted — not final (--budget-ms 0 = unbounded)")
        );
        // the not-found, not-judged and budget lines say it themselves, and a
        // final report has nothing to say
        let full = ScanConfig::full();
        for (detected, scored) in [(false, false), (true, false), (true, true)] {
            assert_eq!(
                cut_line(&report(detected, scored, true), &full),
                None,
                "detected {detected}, scored {scored}"
            );
        }
        assert_eq!(
            cut_line(&report(true, false, false), &ScanConfig::frame()),
            None
        );
    }
}
