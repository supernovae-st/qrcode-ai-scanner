//! The environment receipt: host, power, toolchain, source tree and the
//! run's date. Every probe degrades to `unknown` — an unreadable fact is
//! recorded as such, never fatal. No absolute path and no user name lands
//! here.
//!
//! Probes run under caps like any child ([`probe`]): a hung `pmset` or
//! `lsof` cannot hold the host lock forever.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use super::proc::{self, Caps, Exit};

/// The caps of one probe: a few seconds and a little memory.
const PROBE_CAPS: Caps = Caps {
    rss: 512 << 20,
    wall: Duration::from_secs(10),
};

/// A probe's trimmed stdout, under [`PROBE_CAPS`] ([`proc::run_probe`]: a
/// setuid tool such as macOS `ps` runs too); `None` when it failed, was
/// capped, or printed nothing.
pub(crate) fn probe(command: &mut Command) -> Option<String> {
    let done = proc::run_probe(command, PROBE_CAPS).ok()?;
    (done.failure.is_none() && done.usage.exit == Exit::Code(0))
        .then(|| String::from_utf8_lossy(&done.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn stdout_of(program: &str, args: &[&str]) -> Option<String> {
    probe(Command::new(program).args(args))
}

fn sysctl(key: &str) -> Option<String> {
    stdout_of("sysctl", &["-n", key])
}

fn unknown(value: Option<String>) -> String {
    value.unwrap_or_else(|| String::from("unknown"))
}

/// Power source and state, from `pmset` (macOS).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Power {
    /// `AC Power` · `Battery Power` · `unknown`.
    pub(crate) source: String,
    pub(crate) battery_percent: Option<u8>,
    pub(crate) low_power_mode: Option<bool>,
}

impl Power {
    /// Both the source and Low Power Mode were read. Where `pmset`
    /// exists (macOS), an unreadable state closes the gate; elsewhere power
    /// is not read.
    pub(crate) fn readable(&self) -> bool {
        !cfg!(target_os = "macos") || (self.source != "unknown" && self.low_power_mode.is_some())
    }
}

pub(crate) fn parse_pmset_batt(text: &str) -> (String, Option<u8>) {
    let source = text
        .lines()
        .find_map(|l| {
            let start = l.find("drawing from '")? + "drawing from '".len();
            let rest = &l[start..];
            Some(rest[..rest.find('\'')?].to_owned())
        })
        .unwrap_or_else(|| String::from("unknown"));
    let percent = text.lines().find_map(|l| {
        let end = l.find("%;")?;
        let digits: String = l[..end]
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits.parse().ok()
    });
    (source, percent)
}

/// The power state now (read at run start and in every gate reading).
pub(crate) fn power() -> Power {
    let (source, battery_percent) = stdout_of("pmset", &["-g", "batt"])
        .map_or_else(|| (String::from("unknown"), None), |t| parse_pmset_batt(&t));
    let low_power_mode = stdout_of("pmset", &["-g"]).and_then(|t| {
        t.lines().find_map(|l| {
            let mut words = l.split_whitespace();
            (words.next() == Some("lowpowermode")).then(|| words.next() == Some("1"))
        })
    });
    Power {
        source,
        battery_percent,
        low_power_mode,
    }
}

/// The checked-out tree, read from git at run time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Source {
    pub(crate) git_sha: String,
    /// `clean` · `dirty` (uncommitted or untracked changes) · `unknown`.
    pub(crate) tree: String,
}

pub(crate) fn source(repo: &Path) -> Source {
    let git = |args: &[&str]| {
        let done = proc::run_once(
            Command::new("git").arg("-C").arg(repo).args(args),
            PROBE_CAPS,
        )
        .ok()?;
        (done.failure.is_none() && done.usage.exit == Exit::Code(0))
            .then(|| String::from_utf8_lossy(&done.stdout).into_owned())
    };
    let git_sha = git(&["rev-parse", "HEAD"])
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| String::from("unknown"));
    let tree = match git(&["status", "--porcelain"]) {
        Some(status) if status.trim().is_empty() => "clean",
        Some(_) => "dirty",
        None => "unknown",
    };
    Source {
        git_sha,
        tree: tree.to_owned(),
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` from Unix seconds (proleptic Gregorian, UTC).
pub(crate) fn utc(unix_s: u64) -> String {
    let days = i64::try_from(unix_s / 86_400).unwrap_or(0);
    let secs = unix_s % 86_400;
    // days → civil date (H. Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// The toolchain directory a cargo path lives in
/// (`…/toolchains/1.97.0-aarch64-apple-darwin/bin/cargo` → its name) —
/// identifies cargo's release without spawning a second cargo process.
pub(crate) fn toolchain_of(cargo: &str) -> Option<String> {
    let parts: Vec<&str> = cargo.split(['/', '\\']).collect();
    let at = parts.iter().position(|p| *p == "toolchains")?;
    parts.get(at + 1).map(|s| (*s).to_owned())
}

/// The environment facts of one run.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Facts {
    pub(crate) date_utc: String,
    pub(crate) host_model: String,
    pub(crate) cpu: String,
    pub(crate) cores_logical: usize,
    pub(crate) cores_physical: String,
    pub(crate) cores_performance: String,
    pub(crate) cores_efficiency: String,
    pub(crate) ram_bytes: Option<u64>,
    pub(crate) os: String,
    pub(crate) kernel: String,
    pub(crate) power: Power,
    pub(crate) rustc: String,
    /// The toolchain of the cargo that built this harness.
    pub(crate) cargo_toolchain: String,
    /// This harness's build profile (the measured binaries carry their own).
    pub(crate) harness_profile: &'static str,
    pub(crate) rayon_num_threads: Option<String>,
    pub(crate) node: String,
    /// The tree at run time — the harness binary may be older.
    pub(crate) source: Source,
    /// sha256 of the running harness binary itself, whatever the tree
    /// holds now.
    pub(crate) harness_sha256: String,
}

pub(crate) fn collect(repo: &Path, node: &str) -> Facts {
    let os = match (
        stdout_of("sw_vers", &["-productName"]),
        stdout_of("sw_vers", &["-productVersion"]),
        stdout_of("sw_vers", &["-buildVersion"]),
    ) {
        (Some(name), Some(version), Some(build)) => format!("{name} {version} ({build})"),
        _ => format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    };
    let rustc = stdout_of("rustc", &["-vV"]).map(|v| {
        let field = |key: &str| {
            v.lines()
                .find_map(|l| l.strip_prefix(key))
                .unwrap_or("?")
                .trim()
                .to_owned()
        };
        format!("{} ({})", field("release:"), field("commit-hash:"))
    });
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| env!("CARGO").to_owned());
    Facts {
        date_utc: utc(super::gate::unix_now()),
        host_model: unknown(sysctl("hw.model")),
        cpu: unknown(sysctl("machdep.cpu.brand_string")),
        cores_logical: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        cores_physical: unknown(sysctl("hw.physicalcpu")),
        cores_performance: unknown(sysctl("hw.perflevel0.physicalcpu")),
        cores_efficiency: unknown(sysctl("hw.perflevel1.physicalcpu")),
        ram_bytes: sysctl("hw.memsize").and_then(|v| v.parse().ok()),
        os,
        kernel: unknown(sysctl("kern.osrelease").or_else(|| stdout_of("uname", &["-r"]))),
        power: power(),
        rustc: unknown(rustc),
        cargo_toolchain: unknown(toolchain_of(&cargo)),
        harness_profile: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        rayon_num_threads: std::env::var("RAYON_NUM_THREADS").ok(),
        node: unknown(stdout_of(node, &["--version"])),
        source: source(repo),
        harness_sha256: unknown(
            std::env::current_exe()
                .and_then(std::fs::read)
                .ok()
                .map(|bytes| crate::external::sha256_bytes(&bytes)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates_on_fixed_instants() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc(4_107_542_399), "2100-02-28T23:59:59Z");
    }

    #[test]
    fn pmset_battery_lines_parse() {
        let batt = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1234)\t87%; charged; 0:00 remaining present: true\n";
        assert_eq!(parse_pmset_batt(batt), (String::from("AC Power"), Some(87)));
        assert_eq!(parse_pmset_batt(""), (String::from("unknown"), None));
    }

    #[test]
    fn toolchain_names_come_from_the_cargo_path() {
        assert_eq!(
            toolchain_of("/home/u/.rustup/toolchains/1.97.0-aarch64-apple-darwin/bin/cargo")
                .as_deref(),
            Some("1.97.0-aarch64-apple-darwin")
        );
        assert_eq!(toolchain_of("/usr/bin/cargo"), None);
    }

    #[test]
    fn the_tree_identity_reads_git_or_unknown() {
        let here = source(&crate::repo_root());
        assert!(
            here.git_sha == "unknown" || here.git_sha.len() == 40,
            "{here:?}"
        );
        let nowhere = source(Path::new("/nonexistent/qrscan-bench-no-repo"));
        assert_eq!(
            nowhere,
            Source {
                git_sha: String::from("unknown"),
                tree: String::from("unknown")
            }
        );
    }
}
