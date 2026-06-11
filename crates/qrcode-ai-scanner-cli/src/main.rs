//! `qrscan` — scan and validate QR codes from the command line.
//!
//! JSON (the `ScanReport` contract) by default; `--pretty` for humans.
//! Exit codes: 0 = QR found · 1 = no QR found · 2 = invalid input/error.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use qrcode_ai_scanner::{ImageInput, ScanProfile, ScanReport, Scanner};

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
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\t' {
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

    let scanner = Scanner::builder().profile(cli.profile.into()).build();
    let report = match scanner.scan(ImageInput::encoded(&bytes)) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {} ({})", error, error.code());
            return ExitCode::from(2);
        }
    };

    if cli.score_only {
        let Some(score) = &report.score else {
            // Frame profile / no detection: 0 would be indistinguishable
            // from a true zero score — refuse instead of papering.
            eprintln!("error: no score in this profile/outcome (use --profile full|fast)");
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
