//! The host gate.
//! The host is shared: a timing segment starts only when the one-minute
//! load average is at most [`LOAD_PER_CORE`] per logical core (6 on 12
//! cores), no sibling build runs, the host is on AC power outside Low Power
//! Mode, swap stays under the host floor, and this run holds the host-wide
//! bench lock. Load is recorded before and after; a segment that crosses
//! the gate is discarded, never adjusted.
//!
//! Every reading fails closed: an unreadable load, process table, swap use
//! or (where `pmset` exists) power state, a probe past its cap, or a lock
//! file that no longer names the held lock closes it.
//!
//! While a pass runs, its own busy threads count against it, but only as
//! much as they can have built: the one-minute load average is an
//! exponential average with a 60 s time constant, so T threads busy for t
//! seconds add at most T·(1 − e^(−t/60)) — the allowance. The start check
//! has none.
//!
//! A dry run enforces none of this but the host floor: it refuses to start,
//! and stops, while load1 > 30 or swap used > 8 GB (8 × 10⁹ bytes), or
//! while another bench run holds the lock.
//!
//! A *sibling build* is a cargo/rustc-family process (not one of our own
//! ancestors) whose arguments or working directory name a worktree of this
//! repository or carry a project marker (`qrscan`, the scanner crate
//! names), plus every build process descending from one. Arguments and
//! paths are read to classify, never printed: a match reports pid and
//! executable name only.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::env::{self, Power, probe};

/// Load-average ceiling per logical core (6 on 12 cores).
pub(crate) const LOAD_PER_CORE: f64 = 0.5;
/// Host floor: no bench step while load1 is above this…
pub(crate) const FLOOR_LOAD: f64 = 30.0;
/// …or swap use above 8 GB, 8 × 10⁹ bytes (decimal, not GiB).
pub(crate) const FLOOR_SWAP: u64 = 8_000_000_000;
/// The host-wide bench lock: one bench run at a time on this host. It
/// lives outside `/tmp`, which the system cleans.
pub(crate) const LOCK_PATH: &str = "/var/tmp/qrscan-bench.lock";
/// Substrings that tie a build process to this project.
const PROJECT_MARKERS: [&str; 3] = ["qrscan", "qrcode-ai-scanner", "qrcode_ai_scanner"];
/// Poll period while waiting for the gate.
const POLL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct Load {
    pub(crate) one: f64,
    pub(crate) five: f64,
    pub(crate) fifteen: f64,
}

/// `{ 10.23 9.54 11.21 }` (macOS `sysctl -n vm.loadavg`) or
/// `0.52 0.58 0.59 1/389 12345` (Linux `/proc/loadavg`).
pub(crate) fn parse_loadavg(text: &str) -> Option<Load> {
    let mut values = text
        .split_whitespace()
        .filter(|t| !matches!(*t, "{" | "}"))
        .map(str::parse::<f64>);
    Some(Load {
        one: values.next()?.ok()?,
        five: values.next()?.ok()?,
        fifteen: values.next()?.ok()?,
    })
}

pub(crate) fn read_load() -> Result<Load, String> {
    let text = if let Ok(text) = std::fs::read_to_string("/proc/loadavg") {
        text
    } else {
        probe(Command::new("sysctl").args(["-n", "vm.loadavg"]))
            .ok_or("sysctl vm.loadavg: no answer")?
    };
    parse_loadavg(&text).ok_or_else(|| format!("unreadable load average {:?}", text.trim()))
}

/// Used swap, bytes, from `sysctl -n vm.swapusage` (macOS: `total =
/// 6144.00M  used = 4969.69M  free = …`).
pub(crate) fn parse_swapusage(text: &str) -> Option<u64> {
    let value = text.split("used = ").nth(1)?.split_whitespace().next()?;
    let (number, unit) = value.split_at(value.find(|c: char| c.is_ascii_alphabetic())?);
    let number: f64 = number.parse().ok()?;
    let scale = match unit {
        "K" => 1024.0,
        "M" => 1_048_576.0,
        "G" => 1_073_741_824.0,
        _ => return None,
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a non-negative byte count far below 2^53"
    )]
    let bytes = (number * scale).round() as u64;
    Some(bytes)
}

/// Used swap from `/proc/meminfo` (Linux): `SwapTotal` − `SwapFree`.
fn parse_meminfo_swap(text: &str) -> Option<u64> {
    let field = |key: &str| -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()
    };
    Some(field("SwapTotal:")?.saturating_sub(field("SwapFree:")?) * 1024)
}

pub(crate) fn read_swap() -> Option<u64> {
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        return parse_meminfo_swap(&text);
    }
    parse_swapusage(&probe(Command::new("sysctl").args(["-n", "vm.swapusage"]))?)
}

/// The allowance a pass on `threads` busy threads has earned after
/// `elapsed`: what they can have added to the one-minute load average.
pub(crate) fn own_allowance(threads: usize, elapsed: Duration) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "thread counts are tiny")]
    let threads = threads as f64;
    threads * -(-elapsed.as_secs_f64() / 60.0).exp_m1()
}

/// One row of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Process {
    pub(crate) pid: u32,
    pub(crate) ppid: u32,
    pub(crate) args: String,
}

/// `ps -axww -o pid=,ppid=,args=` output.
pub(crate) fn parse_ps(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (id, rest) = line.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (parent, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            Some(Process {
                pid: id.parse().ok()?,
                ppid: parent.parse().ok()?,
                args: args.trim().to_owned(),
            })
        })
        .collect()
}

/// The build tool a command line runs, by executable name.
pub(crate) fn build_tool(args: &str) -> Option<String> {
    let exe = args.split_whitespace().next()?;
    let name = exe.rsplit('/').next()?;
    let tool = matches!(name, "cargo" | "rustc" | "rustdoc" | "clippy-driver")
        || name.starts_with("cargo-");
    tool.then(|| name.to_owned())
}

/// A sibling build process — reported by pid and executable only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SiblingBuild {
    pub(crate) pid: u32,
    pub(crate) tool: String,
}

/// Classify a process table. `cwd` reads a working directory (injected for
/// tests); `own` is this process — it and its ancestors never count.
pub(crate) fn sibling_builds_in(
    table: &[Process],
    markers: &[String],
    own: u32,
    cwd: &dyn Fn(u32) -> Option<String>,
) -> Vec<SiblingBuild> {
    let parent: BTreeMap<u32, u32> = table.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut ours = BTreeSet::from([own]);
    let mut at = own;
    while let Some(&up) = parent.get(&at) {
        if up == 0 || !ours.insert(up) {
            break;
        }
        at = up;
    }
    let names = |text: &str| markers.iter().any(|m| text.contains(m.as_str()));
    let candidates: Vec<(&Process, String)> = table
        .iter()
        .filter(|p| !ours.contains(&p.pid))
        .filter_map(|p| build_tool(&p.args).map(|tool| (p, tool)))
        .collect();
    let mut siblings: BTreeSet<u32> = candidates
        .iter()
        .filter(|(p, _)| names(&p.args) || cwd(p.pid).is_some_and(|dir| names(&dir)))
        .map(|(p, _)| p.pid)
        .collect();
    // a build process under a sibling build is part of it
    let descends = |pid: u32, siblings: &BTreeSet<u32>| {
        let mut seen = BTreeSet::new();
        let mut at = pid;
        while let Some(&up) = parent.get(&at) {
            if siblings.contains(&up) {
                return true;
            }
            if up == 0 || !seen.insert(up) {
                return false;
            }
            at = up;
        }
        false
    };
    let inherited: Vec<u32> = candidates
        .iter()
        .map(|(p, _)| p.pid)
        .filter(|pid| !siblings.contains(pid) && descends(*pid, &siblings))
        .collect();
    siblings.extend(inherited);
    candidates
        .into_iter()
        .filter(|(p, _)| siblings.contains(&p.pid))
        .map(|(p, tool)| SiblingBuild { pid: p.pid, tool })
        .collect()
}

/// A working directory through `lsof` (macOS has no `/proc`).
fn cwd_of(pid: u32) -> Option<String> {
    if let Ok(link) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
        return Some(link.to_string_lossy().into_owned());
    }
    probe(Command::new("lsof").args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"]))?
        .lines()
        .find_map(|l| l.strip_prefix('n').map(str::to_owned))
}

/// The project markers plus every worktree of this repository
/// (`git worktree list --porcelain`): parallel work builds inside them.
pub(crate) fn worktree_markers(repo: &Path) -> Vec<String> {
    let mut markers: Vec<String> = PROJECT_MARKERS.iter().map(|m| (*m).to_owned()).collect();
    if let Some(list) =
        probe(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["worktree", "list", "--porcelain"]),
        )
    {
        markers.extend(
            list.lines()
                .filter_map(|l| l.strip_prefix("worktree "))
                .map(str::to_owned),
        );
    }
    markers
}

#[cfg(unix)]
mod lock {
    use std::os::fd::AsRawFd as _;

    const LOCK_SH: i32 = 1;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;

    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    /// Try a non-blocking lock; false when another open file holds it.
    pub(super) fn try_lock(file: &std::fs::File, exclusive: bool) -> Result<bool, String> {
        let op = if exclusive { LOCK_EX } else { LOCK_SH } | LOCK_NB;
        // SAFETY: plain syscall on a descriptor `file` keeps open.
        if unsafe { flock(file.as_raw_fd(), op) } == 0 {
            return Ok(true);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(format!("flock: {err}"))
        }
    }
}

#[cfg(not(unix))]
mod lock {
    pub(super) fn try_lock(_file: &std::fs::File, _exclusive: bool) -> Result<bool, String> {
        Ok(true)
    }
}

/// The host-wide bench lock, held for a whole run (released when dropped,
/// or by the kernel when the process dies — never stale).
pub(crate) struct HostLock {
    file: std::fs::File,
}

/// A file's identity: device and inode.
#[cfg(unix)]
fn identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;
    (meta.dev(), meta.ino())
}

impl HostLock {
    fn open(path: &Path) -> Result<std::fs::File, String> {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("the host lock file: {}", e.kind()))
    }

    /// Take the lock; `None` while another process holds it.
    pub(crate) fn acquire(path: &Path) -> Result<Option<Self>, String> {
        let file = Self::open(path)?;
        if !lock::try_lock(&file, true)? {
            return Ok(None);
        }
        let note = format!(
            "qrscan bench run · pid {} · since {}\n",
            std::process::id(),
            env::utc(unix_now())
        );
        let _ = file.set_len(0);
        let _ = std::io::Write::write_all(&mut &file, note.as_bytes());
        Ok(Some(Self { file }))
    }

    /// The path still names the locked file: a cleanup that unlinked or
    /// replaced it would let a second run lock a fresh inode beside this
    /// one.
    #[cfg(unix)]
    pub(crate) fn intact(&self, path: &Path) -> bool {
        match (self.file.metadata(), std::fs::metadata(path)) {
            (Ok(held), Ok(named)) => identity(&held) == identity(&named),
            _ => false,
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn intact(&self, _path: &Path) -> bool {
        true
    }

    /// Whether another process holds the lock now.
    pub(crate) fn held_elsewhere(path: &Path) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }
        let file = Self::open(path)?;
        lock::try_lock(&file, false).map(|free| !free)
    }
}

/// What a reading enforces: the timing gate, or the dry runs' host floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Rule {
    Gate,
    Floor,
}

/// One gate reading, as logged in the receipt.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Reading {
    pub(crate) phase: String,
    pub(crate) rule: Rule,
    pub(crate) unix_s: u64,
    pub(crate) load: Option<Load>,
    pub(crate) cores: usize,
    pub(crate) threshold: f64,
    pub(crate) allowance: f64,
    pub(crate) sibling_builds: Vec<SiblingBuild>,
    pub(crate) swap_used: Option<u64>,
    pub(crate) power: Option<Power>,
    /// Set while another bench run holds the host-wide lock.
    pub(crate) lock_held_elsewhere: bool,
    pub(crate) open: bool,
    /// Why the gate could not be read (treated as closed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

impl Reading {
    fn on_battery(&self) -> bool {
        self.power
            .as_ref()
            .is_some_and(|p| p.source == "Battery Power" || p.low_power_mode == Some(true))
    }

    pub(crate) fn line(&self) -> String {
        let load = self.load.map_or_else(
            || String::from("load unreadable"),
            |l| format!("load {:.2} {:.2} {:.2}", l.one, l.five, l.fifteen),
        );
        let mut notes = Vec::new();
        if self.sibling_builds.is_empty() {
            notes.push(String::from("no sibling build"));
        } else {
            let list: Vec<String> = self
                .sibling_builds
                .iter()
                .map(|b| format!("{} {}", b.tool, b.pid))
                .collect();
            notes.push(format!("sibling builds: {}", list.join(", ")));
        }
        if self.on_battery() {
            notes.push(String::from("battery or low power"));
        }
        if self.swap_used.is_some_and(|s| s > FLOOR_SWAP) {
            notes.push(String::from("swap above 8 GB"));
        }
        if self.lock_held_elsewhere {
            notes.push(String::from("another bench run holds the lock"));
        }
        if let Some(error) = &self.error {
            notes.push(format!("unreadable: {error}"));
        }
        let ceiling = match self.rule {
            Rule::Gate => format!(
                "ceiling {:.2}{}",
                self.threshold,
                if self.allowance > 0.0 {
                    format!(" + {:.2} own", self.allowance)
                } else {
                    String::new()
                }
            ),
            Rule::Floor => format!("host floor {FLOOR_LOAD:.0}"),
        };
        format!(
            "{} {} [{}] · {load} · {ceiling} · {}",
            match self.rule {
                Rule::Gate => "gate",
                Rule::Floor => "floor",
            },
            if self.open { "OPEN" } else { "CLOSED" },
            self.phase,
            notes.join(" · ")
        )
    }
}

/// Whether a reading opens its rule. Both rules need every part read, no
/// other run's lock, and swap use read and at most 8 × 10⁹ bytes; the gate
/// also needs no sibling build, mains power outside Low Power Mode and
/// load1 within the ceiling plus the allowance, the floor load1 ≤ 30.
fn opens(reading: &Reading) -> bool {
    let load = reading.load.map(|l| l.one);
    reading.error.is_none()
        && !reading.lock_held_elsewhere
        && reading.swap_used.is_some_and(|s| s <= FLOOR_SWAP)
        && match reading.rule {
            Rule::Gate => {
                reading.sibling_builds.is_empty()
                    && !reading.on_battery()
                    && load.is_some_and(|l| l <= reading.threshold + reading.allowance)
            }
            Rule::Floor => load.is_some_and(|l| l <= FLOOR_LOAD),
        }
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The gate of this host.
pub(crate) struct Gate {
    pub(crate) cores: usize,
    pub(crate) threshold: f64,
    repo: PathBuf,
    /// Read on the first reading — never before the run holds the lock.
    markers: OnceLock<Vec<String>>,
    own: u32,
    lock_path: PathBuf,
    held: Mutex<Option<HostLock>>,
    poll: Duration,
    /// Test readings: each one pops an open (true) or closed reading.
    script: Option<Mutex<Vec<bool>>>,
}

impl Gate {
    pub(crate) fn new(repo: &Path) -> Self {
        Self::with_lock(repo, Path::new(LOCK_PATH))
    }

    /// This host's gate with its lock file at `lock_path` (tests use their
    /// own); nothing runs until the first reading.
    pub(crate) fn with_lock(repo: &Path, lock_path: &Path) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        #[expect(clippy::cast_precision_loss, reason = "core counts are tiny")]
        let threshold = LOAD_PER_CORE * cores as f64;
        Self {
            cores,
            threshold,
            repo: repo.to_path_buf(),
            markers: OnceLock::new(),
            own: std::process::id(),
            lock_path: lock_path.to_path_buf(),
            held: Mutex::new(None),
            poll: POLL,
            script: None,
        }
    }

    /// A gate whose readings follow `opens` in order (then stay closed),
    /// on a 12-core host, holding no lock.
    #[cfg(test)]
    pub(crate) fn scripted(opens: &[bool]) -> Self {
        let mut opens = opens.to_vec();
        opens.reverse();
        Self {
            cores: 12,
            threshold: 6.0,
            repo: PathBuf::new(),
            markers: OnceLock::from(Vec::new()),
            own: 0,
            lock_path: PathBuf::from("/nonexistent/qrscan-bench-test.lock"),
            held: Mutex::new(None),
            poll: Duration::ZERO,
            script: Some(Mutex::new(opens)),
        }
    }

    /// Take the host-wide lock for this run; true once held.
    pub(crate) fn acquire(&self) -> Result<bool, String> {
        if self.script.is_some() {
            return Ok(true);
        }
        let mut held = self.held.lock().map_err(|_| "lock state poisoned")?;
        if held.is_none() {
            *held = HostLock::acquire(&self.lock_path)?;
        }
        Ok(held.is_some())
    }

    /// Take the lock, polling until `deadline` (a dry run: once). A run
    /// takes it before anything else — environment probes, preflight
    /// hellos — so nothing of it executes beside another run.
    pub(crate) fn acquire_by(&self, deadline: Instant) -> Result<bool, String> {
        loop {
            if self.acquire()? {
                return Ok(true);
            }
            if Instant::now() + self.poll > deadline {
                return Ok(false);
            }
            std::thread::sleep(self.poll);
        }
    }

    fn holding(&self) -> bool {
        self.held.lock().is_ok_and(|h| h.is_some())
    }

    /// Whether this run holds the lock, and the lock file still names it.
    /// A replaced or removed file drops the stale lock and takes a fresh
    /// one; either way the caller's reading closes.
    fn lock_state(&self) -> Result<bool, String> {
        let mut held = self.held.lock().map_err(|_| "lock state poisoned")?;
        match held.as_ref() {
            None => Ok(false),
            Some(lock) if lock.intact(&self.lock_path) => Ok(true),
            Some(_) => {
                *held = None;
                *held = HostLock::acquire(&self.lock_path)?;
                Err(String::from(
                    "the host lock file was removed or replaced; the lock is taken again",
                ))
            }
        }
    }

    fn scripted_reading(&self, phase: &str, rule: Rule, allowance: f64) -> Option<Reading> {
        let mut script = self.script.as_ref()?.lock().ok()?;
        let open = script.pop().unwrap_or(false);
        let one = if open { 1.0 } else { 50.0 };
        Some(Reading {
            phase: phase.to_owned(),
            rule,
            unix_s: unix_now(),
            load: Some(Load {
                one,
                five: one,
                fifteen: one,
            }),
            cores: self.cores,
            threshold: self.threshold,
            allowance,
            sibling_builds: Vec::new(),
            swap_used: None,
            power: None,
            lock_held_elsewhere: false,
            open,
            error: None,
        })
    }

    /// One reading. It fails closed: any part that cannot be read — load,
    /// process table, swap, the power state under the gate rule, the lock —
    /// is an error, and an error closes it.
    fn measure(&self, phase: &str, rule: Rule, allowance: f64) -> Reading {
        if let Some(reading) = self.scripted_reading(phase, rule, allowance) {
            return reading;
        }
        let mut errors: Vec<String> = Vec::new();
        let load = read_load().map_err(|e| errors.push(e)).ok();
        let markers = self.markers.get_or_init(|| worktree_markers(&self.repo));
        let sibling_builds = if let Some(table) =
            probe(Command::new("ps").args(["-axww", "-o", "pid=,ppid=,args="]))
        {
            sibling_builds_in(&parse_ps(&table), markers, self.own, &cwd_of)
        } else {
            errors.push(String::from("ps: no answer"));
            Vec::new()
        };
        // a replaced lock file is an error of this reading; the lock taken
        // again on the new file is ours, not another run's
        let holding = self.lock_state().unwrap_or_else(|e| {
            errors.push(e);
            self.holding()
        });
        let lock_held_elsewhere = !holding
            && HostLock::held_elsewhere(&self.lock_path).unwrap_or_else(|e| {
                errors.push(e);
                true
            });
        let swap_used = read_swap();
        if swap_used.is_none() {
            errors.push(String::from("swap use"));
        }
        let power = env::power();
        if rule == Rule::Gate && !power.readable() {
            errors.push(String::from("power state"));
        }
        let mut reading = Reading {
            phase: phase.to_owned(),
            rule,
            unix_s: unix_now(),
            load,
            cores: self.cores,
            threshold: self.threshold,
            allowance,
            sibling_builds,
            swap_used,
            power: Some(power),
            lock_held_elsewhere,
            open: false,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
        };
        reading.open = opens(&reading);
        reading
    }

    /// One timing-gate reading: load, process table, power, swap, lock.
    pub(crate) fn read(&self, phase: &str, allowance: f64) -> Reading {
        self.measure(phase, Rule::Gate, allowance)
    }

    /// One host-floor reading (dry runs).
    pub(crate) fn floor(&self, phase: &str) -> Reading {
        self.measure(phase, Rule::Floor, 0.0)
    }

    /// Poll until the gate opens with the lock held, or `deadline` passes;
    /// every reading is printed, the first and the last are returned.
    pub(crate) fn wait(&self, phase: &str, deadline: Instant) -> (Reading, Vec<Reading>) {
        let mut kept = Vec::new();
        loop {
            let held = self.acquire();
            let mut reading = self.read(phase, 0.0);
            if let Err(e) = held {
                reading.error = Some(e);
                reading.open = false;
            } else if held == Ok(false) {
                reading.open = false;
            }
            println!("{}", reading.line());
            if kept.is_empty() {
                kept.push(reading.clone());
            }
            if reading.open || Instant::now() + self.poll > deadline {
                if kept.len() == 1
                    && (kept[0].unix_s != reading.unix_s || kept[0].open != reading.open)
                {
                    kept.push(reading.clone());
                }
                return (reading, kept);
            }
            std::thread::sleep(self.poll);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_averages_parse_on_both_platforms() {
        let mac = parse_loadavg("{ 10.23 9.54 11.21 }\n");
        assert_eq!(
            mac,
            Some(Load {
                one: 10.23,
                five: 9.54,
                fifteen: 11.21
            })
        );
        let linux = parse_loadavg("0.52 0.58 0.59 1/389 12345\n");
        assert_eq!(
            linux,
            Some(Load {
                one: 0.52,
                five: 0.58,
                fifteen: 0.59
            })
        );
        assert_eq!(parse_loadavg("{ }"), None);
    }

    #[test]
    fn swap_use_parses_on_both_platforms() {
        assert_eq!(
            parse_swapusage("total = 6144.00M  used = 4969.69M  free = 1174.31M  (encrypted)\n"),
            Some(5_211_097_661)
        );
        assert_eq!(
            parse_swapusage("total = 0.00M  used = 0.00M  free = 0.00M"),
            Some(0)
        );
        assert_eq!(
            parse_swapusage("total = 10.00G  used = 8.50G  free = 1.50G"),
            Some(9_126_805_504)
        );
        assert_eq!(parse_swapusage("garbage"), None);
        assert_eq!(
            parse_meminfo_swap("SwapTotal:       2097152 kB\nSwapFree:        1048576 kB\n"),
            Some(1_073_741_824)
        );
    }

    /// The own allowance T·(1 − e^(−t/60)): nothing at the start, 63 % of
    /// T after a minute, T in the limit.
    #[test]
    fn the_own_allowance_grows_with_the_load_average() {
        let at =
            |threads, secs| format!("{:.4}", own_allowance(threads, Duration::from_secs(secs)));
        assert_eq!(at(12, 0), "0.0000");
        assert_eq!(at(1, 60), "0.6321");
        assert_eq!(at(12, 60), "7.5854");
        assert_eq!(at(4, 600), "3.9998");
        assert_eq!(at(1, 30), "0.3935");
    }

    fn table() -> Vec<Process> {
        parse_ps(
            "    1     0 /sbin/launchd\n\
             100     1 /bin/zsh\n\
             200   100 /home/u/.cargo/bin/cargo run -p xtask -- bench run\n\
             210   200 /repo/target/release/xtask bench run\n\
             300     1 /home/u/.rustup/toolchains/1.97.0/bin/cargo build --release\n\
             310   300 /home/u/.rustup/toolchains/1.97.0/bin/rustc --crate-name rxing\n\
             400     1 /usr/bin/cargo build\n\
             410   400 rustc --crate-name serde --out-dir /tmp/other\n\
             500     1 rustc --crate-name qrcode_ai_scanner\n\
             600     1 /opt/homebrew/bin/node bench-node.mjs serve\n",
        )
    }

    #[test]
    fn process_rows_parse() {
        let rows = table();
        assert_eq!(rows.len(), 10);
        assert_eq!(
            rows[2],
            Process {
                pid: 200,
                ppid: 100,
                args: String::from("/home/u/.cargo/bin/cargo run -p xtask -- bench run")
            }
        );
        assert_eq!(
            build_tool("/a/b/cargo-nextest nextest run").as_deref(),
            Some("cargo-nextest")
        );
        assert_eq!(
            build_tool("clippy-driver --crate-name x").as_deref(),
            Some("clippy-driver")
        );
        assert_eq!(build_tool("/opt/homebrew/bin/node x.mjs"), None);
        assert_eq!(build_tool(""), None);
    }

    /// Our own `cargo run` ancestor never counts; a cargo found by its
    /// working directory in a worktree brings its rustc child; a crate-name
    /// marker in rustc arguments counts; an unrelated build does not.
    #[test]
    fn sibling_builds_are_classified_without_our_ancestors() {
        let markers = vec![
            String::from("qrcode_ai_scanner"),
            String::from("/work/scanner"),
        ];
        let cwd = |pid: u32| match pid {
            200 | 300 => Some(String::from("/work/scanner/other-worktree")),
            400 => Some(String::from("/elsewhere")),
            _ => None,
        };
        let found = sibling_builds_in(&table(), &markers, 210, &cwd);
        assert_eq!(
            found,
            vec![
                SiblingBuild {
                    pid: 300,
                    tool: String::from("cargo")
                },
                SiblingBuild {
                    pid: 310,
                    tool: String::from("rustc")
                },
                SiblingBuild {
                    pid: 500,
                    tool: String::from("rustc")
                },
            ]
        );
    }

    fn reading(power: Option<Power>) -> Reading {
        Reading {
            phase: String::from("start"),
            rule: Rule::Gate,
            unix_s: 0,
            load: Some(Load {
                one: 9.5,
                five: 8.0,
                fifteen: 7.0,
            }),
            cores: 12,
            threshold: 6.0,
            allowance: 0.0,
            sibling_builds: vec![SiblingBuild {
                pid: 42,
                tool: String::from("rustc"),
            }],
            swap_used: Some(9 << 30),
            power,
            lock_held_elsewhere: true,
            open: false,
            error: None,
        }
    }

    #[test]
    fn readings_print_without_arguments() {
        assert_eq!(
            reading(None).line(),
            "gate CLOSED [start] · load 9.50 8.00 7.00 · ceiling 6.00 · sibling builds: rustc 42 \
             · swap above 8 GB · another bench run holds the lock"
        );
        let battery = Power {
            source: String::from("Battery Power"),
            battery_percent: Some(80),
            low_power_mode: Some(false),
        };
        assert!(reading(Some(battery)).on_battery());
        let low_power = Power {
            source: String::from("AC Power"),
            battery_percent: None,
            low_power_mode: Some(true),
        };
        assert!(
            reading(Some(low_power)).on_battery(),
            "Low Power Mode closes the gate"
        );
        let mains = Power {
            source: String::from("AC Power"),
            battery_percent: Some(100),
            low_power_mode: Some(false),
        };
        assert!(!reading(Some(mains)).on_battery());
    }

    /// Readings fail closed — swap unread or above 8 × 10⁹ bytes, an
    /// unreadable part (power under the gate rule), a battery — and open
    /// exactly at the limits.
    #[test]
    fn readings_fail_closed() {
        let calm = |rule: Rule| Reading {
            rule,
            load: Some(Load {
                one: 6.0,
                five: 6.0,
                fifteen: 6.0,
            }),
            sibling_builds: Vec::new(),
            swap_used: Some(FLOOR_SWAP),
            power: Some(Power {
                source: String::from("AC Power"),
                battery_percent: Some(100),
                low_power_mode: Some(false),
            }),
            lock_held_elsewhere: false,
            ..reading(None)
        };
        assert!(
            opens(&calm(Rule::Gate)),
            "load1 6.00 at a 6.00 ceiling, swap at 8 GB"
        );
        assert!(opens(&calm(Rule::Floor)));
        for rule in [Rule::Gate, Rule::Floor] {
            let unread = Reading {
                swap_used: None,
                ..calm(rule)
            };
            assert!(!opens(&unread), "swap unread closes {rule:?}");
            let over = Reading {
                swap_used: Some(8_000_000_001),
                ..calm(rule)
            };
            assert!(!opens(&over), "one byte above 8 × 10^9 closes {rule:?}");
            let blind = Reading {
                error: Some(String::from("power state")),
                ..calm(rule)
            };
            assert!(!opens(&blind), "an unreadable part closes {rule:?}");
        }
        let battery = Reading {
            power: Some(Power {
                source: String::from("Battery Power"),
                battery_percent: Some(90),
                low_power_mode: Some(false),
            }),
            ..calm(Rule::Gate)
        };
        assert!(!opens(&battery));
        let unknown = Power {
            source: String::from("unknown"),
            battery_percent: None,
            low_power_mode: None,
        };
        assert_eq!(
            unknown.readable(),
            !cfg!(target_os = "macos"),
            "where pmset exists, an unread power state is unreadable"
        );
    }

    /// Another holder of the host-wide lock is seen; our own release frees
    /// it. The lock lives in `/var/tmp`, which the `/tmp` cleanups spare.
    #[test]
    #[cfg(unix)]
    fn the_host_lock_admits_one_run() {
        assert_eq!(LOCK_PATH, "/var/tmp/qrscan-bench.lock");
        let path =
            std::env::temp_dir().join(format!("qrscan-bench-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(
            !HostLock::held_elsewhere(&path).expect("probe"),
            "no file, no holder"
        );
        let first = HostLock::acquire(&path).expect("open").expect("free");
        assert!(first.intact(&path));
        assert!(HostLock::held_elsewhere(&path).expect("probe"));
        assert!(
            HostLock::acquire(&path).expect("open").is_none(),
            "one holder at a time"
        );
        drop(first);
        assert!(!HostLock::held_elsewhere(&path).expect("probe"));
        let _ = std::fs::remove_file(&path);
    }

    /// A lock file a cleanup removed or replaced no longer names the held
    /// lock. The reading that finds it closes, the gate takes a fresh lock,
    /// and a second run can no longer lock a new inode beside this one.
    #[test]
    #[cfg(unix)]
    fn a_replaced_lock_file_closes_the_reading() {
        let path =
            std::env::temp_dir().join(format!("qrscan-bench-inode-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let gate = Gate::with_lock(&crate::repo_root(), &path);
        assert_eq!(gate.acquire_by(Instant::now()), Ok(true));
        std::fs::remove_file(&path).expect("a cleanup unlinks it");
        let reading = gate.floor("after the cleanup");
        assert!(!reading.open);
        assert!(
            reading
                .error
                .as_deref()
                .is_some_and(|e| e.contains("removed or replaced")),
            "{:?}",
            reading.error
        );
        assert_eq!(gate.lock_state(), Ok(true), "a fresh lock on the new file");
        assert!(
            HostLock::acquire(&path).expect("open").is_none(),
            "a second run cannot take the new file"
        );
        drop(gate);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn scripted_gates_replay_their_readings() {
        let gate = Gate::scripted(&[false, true]);
        assert!(!gate.read("a", 0.0).open);
        assert!(gate.read("b", 0.0).open);
        assert!(!gate.read("c", 0.0).open, "then closed");
        let waiting = Gate::scripted(&[false, false, true]);
        let (last, kept) = waiting.wait("start", Instant::now() + Duration::from_secs(60));
        assert!(last.open);
        assert_eq!(
            kept.iter().map(|r| r.open).collect::<Vec<_>>(),
            vec![false, true],
            "first and last reading"
        );
    }
}
