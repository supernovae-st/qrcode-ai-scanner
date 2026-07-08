//! `qrscan` — scan and validate QR codes from the command line.
//!
//! JSON (the `ScanReport` contract) by default; `--pretty` for humans.
//! Exit codes: 0 = QR found · 1 = no QR found · 2 = invalid input/error.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use qrcode_ai_scanner::{ImageInput, ScanProfile, ScanReport, Scanner, StressAxis};

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
#[command(name = "qrscan", version, about)]
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

fn render_pretty(report: &ScanReport) {
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
        None => println!("no QR code found"),
    }
    if let Some(score) = &report.score {
        println!("score     {}/100 ({:?})", score.value, score.grade);
        for axis in &score.axes {
            println!(
                "  {:12} {}/{}",
                format!("{:?}", axis.axis).to_lowercase(),
                axis.passed,
                axis.total
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
    for hint in &report.hints {
        println!("hint      {hint:?}");
    }
    println!("took      {:.0}ms", report.trace.total_ms);
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let bytes = match std::fs::read(&cli.image) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("error: cannot read {}: {error}", cli.image.display());
            return ExitCode::from(2);
        }
    };

    // --score-skip-axes: wire names, loud on a typo (a silently ignored
    // axis would score all six and drift the caller's contract).
    let mut skip = Vec::with_capacity(cli.score_skip_axes.len());
    for name in &cli.score_skip_axes {
        let Some(axis) = StressAxis::from_name(name) else {
            eprintln!(
                "error: unknown stress axis `{name}` — expected resolution | blur | \
                 contrast | perspective | rotation | lighting"
            );
            return ExitCode::from(2);
        };
        skip.push(axis);
    }
    // --budget-ms overrides the preset's wall-clock budget (0 = unbounded) —
    // the same semantics as Node `budgetMs` / WASM / Python / UniFFI.
    let profile = if cli.budget_ms.is_none() && skip.is_empty() {
        cli.profile.into()
    } else {
        let mut config = ScanProfile::from(cli.profile).config();
        if let Some(ms) = cli.budget_ms {
            config.budget_ms = (ms > 0).then_some(u64::from(ms));
        }
        config.score_skip_axes = skip;
        ScanProfile::Custom(config)
    };
    let scanner = Scanner::builder().profile(profile).build();
    let report = match scanner.scan(ImageInput::encoded(&bytes)) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {} [{}]", error, error.code());
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
            eprintln!("error: the frame profile computes no score (use --profile full|fast)");
            return ExitCode::from(2);
        };
        println!("{}", score.value);
    } else if cli.pretty {
        render_pretty(&report);
    } else {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("error: serialization failed: {error}");
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
