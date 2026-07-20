//! `qrscan` — scan and validate QR codes from the command line.
//!
//! JSON (the `ScanReport` contract) by default; `--pretty` for humans.
//! Exit codes: 0 = QR found · 1 = no QR found · 2 = invalid input/error.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use qrcode_ai_scanner::{
    AlphaBackground, ImageInput, ScanProfile, ScanReport, Scanner, ScoreCheck, ScorePreset,
    StressAxis,
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

    /// Override the profile's wall-clock budget in ms (0 = unbounded).
    /// Unbounded is also the strictly-reproducible mode — with a budget set,
    /// WHERE the ladder cuts is machine-dependent (spec/02).
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

    /// Report SECTIONS to exclude at the source (comma-separated: `uec`,
    /// `iso15415`, `alpha_envelope`). Never computed, the wire carries
    /// null, the section-driven hints never fire — the composite value
    /// does not move (spec/04 § skipping checks).
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

    /// Print only the composite score value (scripts).
    #[arg(long, short = 's')]
    score_only: bool,
}

/// Decoded QR text is ATTACKER-CONTROLLED — strip terminal control bytes
/// (ANSI/OSC escape injection) before echoing. JSON output is safe (serde
/// escapes); only this human path needs it.
fn sanitize_terminal(text: &str) -> String {
    // Cc/C1 controls + the format (Cf) spoofers: bidi overrides flip the
    // visual order of the displayed URL, zero-widths hide segments.
    fn is_spoofing_format(c: char) -> bool {
        matches!(c,
            '\u{200B}'..='\u{200F}' // zero-widths + LRM/RLM
            | '\u{202A}'..='\u{202E}' // bidi embedding/override
            | '\u{2060}'..='\u{2069}' // word-joiner + invisibles + bidi isolates
            | '\u{FEFF}')
    }
    text.chars()
        .map(|c| {
            if (c.is_control() && c != '\t') || is_spoofing_format(c) {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            }
        })
        .collect()
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

fn render_pretty(report: &ScanReport) {
    let th = term::Theme::auto();
    match report.detections.first() {
        Some(d) => {
            println!("content   {}", sanitize_terminal(&d.content.text));
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
        None => println!("{}", th.warn("no QR code found")),
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
        let Some(score) = &report.score else {
            // No QR found is exit 1 (the header contract), even score-only.
            // A score genuinely suppressed by the profile (Frame) is a usage
            // error: 0 would be indistinguishable from a true zero score.
            if report.detections.is_empty() {
                eprintln!("no QR code found — no score");
                return ExitCode::from(1);
            }
            let th = term::Theme::auto_stderr();
            eprintln!(
                "{} the frame profile computes no score (use --profile full|fast)",
                th.err_strong("✖")
            );
            return ExitCode::from(2);
        };
        println!("{}", score.value);
    } else if cli.pretty {
        render_pretty(&report);
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
