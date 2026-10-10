//! Child processes under resource caps.
//!
//! Every measured process — a one-shot CLI scan, a persistent worker, a
//! WASM cold start — runs in its own process group under one watchdog: a
//! thread that samples the child's memory (the kernel's high-water mark
//! too, so a spike between two samples is still seen) and wall time every
//! [`POLL`] and kills the group past a cap. The child is reaped with
//! `wait4`, which reports the peak RSS and CPU time of exactly that child
//! (std offers neither, and `getrusage(RUSAGE_CHILDREN)` only keeps a
//! running maximum over all children). A call that returns with a kernel
//! peak above the memory cap, or after its wall cap, is a resource failure
//! all the same.
//!
//! The caps outlive the harness: [`guard_signals`] makes SIGTERM, SIGHUP
//! and SIGINT kill every live child's group before the harness exits; a
//! one-shot child carries a kernel CPU-time limit at its wall cap; workers
//! leave on their own when their parent changes (`worker.rs`, the Node
//! drivers), told which parent to keep through [`super::worker::PARENT_ENV`].
//! A child whose memory cannot be sampled is killed before it runs.
//!
//! No reused pid or group is ever signalled: the reaper first waits for
//! the exit without reaping (`waitid(WNOWAIT)` — the pid, and with it the
//! group id, stays reserved by the zombie), kills what is left of the
//! group, stops and joins the watchdog, drops the group from the stop
//! handler's table, then reaps.
//!
//! Children never inherit `RUST_BACKTRACE` / `RUST_LIB_BACKTRACE` (a caught
//! engine panic would capture a backtrace inside a timed call), a napi
//! library override, or `NODE_OPTIONS`. Their output pipes are read on
//! their own threads — a chatty child never blocks on a full pipe — and
//! stderr is reduced to counts, `QRS-` codes and panic locations: no stderr
//! text is kept.

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::worker::PARENT_ENV;

/// Watchdog sampling period: about every 100 ms.
pub(crate) const POLL: Duration = Duration::from_millis(100);
/// How long past its wall cap a request may stay unanswered before the
/// harness stops trusting its own watchdog and kills the worker itself.
const GRACE: Duration = Duration::from_secs(5);
/// How long a worker may take to answer `quit` and exit.
const QUIT_WALL: Duration = Duration::from_secs(10);
/// How long the pipes of an exited child may take to reach end of file
/// (only an orphaned grandchild holding them open makes it wait).
const PIPE_WAIT: Duration = Duration::from_secs(2);
/// Distinct `QRS-` codes and panic locations kept per stderr stream.
const STDERR_KEEP: usize = 16;
/// Environment never passed to a measured child.
const SCRUBBED: [&str; 4] = [
    "RUST_BACKTRACE",
    "RUST_LIB_BACKTRACE",
    "NAPI_RS_NATIVE_LIBRARY_PATH",
    "NODE_OPTIONS",
];

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    Code(i32),
    Signal(i32),
}

/// What the kernel accounted to one reaped child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Usage {
    /// Peak resident set, bytes; `None` where `wait4` is unavailable.
    pub(crate) max_rss: Option<u64>,
    pub(crate) user_us: Option<u64>,
    pub(crate) sys_us: Option<u64>,
    pub(crate) exit: Exit,
}

/// The caps one child runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caps {
    /// Memory, bytes: the resident size (on macOS the physical footprint
    /// when larger — compressed pages hide from the resident size).
    pub(crate) rss: u64,
    /// Wall time: from spawn for a one-shot child, from the request for a
    /// worker.
    pub(crate) wall: Duration,
}

/// What ended a call that did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Past the memory cap: killed by the watchdog, or returned with a
    /// kernel peak above it.
    Rss,
    /// Past the wall cap: killed, or returned after it.
    Wall,
    /// The child died by this signal.
    Signal(i32),
    /// The worker exited (this code) in the middle of a call.
    Exit(i32),
    /// The child's memory could no longer be sampled: killed, since the
    /// memory cap could not follow it.
    Unsampled,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Rss => "rss",
            Self::Wall => "wall",
            Self::Signal(_) => "signal",
            Self::Exit(_) => "exit",
            Self::Unsampled => "unsampled",
        }
    }

    pub(crate) fn code(self) -> Option<i32> {
        match self {
            Self::Signal(n) | Self::Exit(n) => Some(n),
            Self::Rss | Self::Wall | Self::Unsampled => None,
        }
    }
}

/// A resource failure: a call a cap, a signal or a crash ended. Never a
/// timing, memory, overrun or decode sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Failure {
    pub(crate) kind: Kind,
    /// The larger of the kernel's peak and the watchdog's peak sample.
    pub(crate) peak_rss: Option<u64>,
    pub(crate) wall_ns: u64,
}

pub(crate) fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// The resource failure that ended a call, if one did: the watchdog's
/// verdict, a signal, an exit in the middle of a call — or, for a call
/// that returned, a kernel peak above the memory cap or an elapsed time
/// past the wall cap.
pub(crate) fn classify(
    cause: Option<Kind>,
    exit: Option<Exit>,
    (peak, wall_ns): (u64, u64),
    caps: Caps,
    mid_call: bool,
) -> Option<Kind> {
    if cause.is_some() {
        return cause;
    }
    match exit {
        Some(Exit::Signal(n)) => return Some(Kind::Signal(n)),
        Some(Exit::Code(code)) if mid_call => return Some(Kind::Exit(code)),
        _ => {}
    }
    let wall = u64::try_from(caps.wall.as_nanos()).unwrap_or(u64::MAX);
    if peak > caps.rss {
        Some(Kind::Rss)
    } else if wall_ns > wall {
        Some(Kind::Wall)
    } else {
        None
    }
}

/// A stderr stream reduced to what may be kept: counts, distinct `QRS-`
/// codes and panic locations (file name, line, column).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Stderr {
    pub(crate) lines: u64,
    pub(crate) bytes: u64,
    pub(crate) codes: BTreeSet<String>,
    pub(crate) panics: BTreeSet<String>,
}

/// `name.rs:line:column` with a plain file name — anything else a panic
/// line carries (a message, decoded text) is not a location.
fn location(token: &str) -> Option<&str> {
    let short = token.rsplit(['/', '\\']).next()?;
    let mut parts = short.rsplitn(3, ':');
    let (column, line, file) = (parts.next()?, parts.next()?, parts.next()?);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let name = file.strip_suffix(".rs")?;
    let plain = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    (plain && digits(line) && digits(column)).then_some(short)
}

impl Stderr {
    fn take(&mut self, line: &str) {
        self.lines += 1;
        self.bytes += line.len() as u64 + 1;
        // codes: `[QRS-` and one to four digits, closed at once
        let mut rest = line;
        while let Some(start) = rest.find("[QRS-") {
            let after = &rest[start + 5..];
            let digits = after.bytes().take_while(u8::is_ascii_digit).count();
            if (1..=4).contains(&digits)
                && after.as_bytes().get(digits) == Some(&b']')
                && self.codes.len() < STDERR_KEEP
            {
                self.codes.insert(format!("QRS-{}", &after[..digits]));
            }
            rest = &after[digits..];
        }
        // locations: the token after Rust's `panicked at` or the worker
        // hook's `panic at`, kept only when it is a location
        for marker in ["panicked at ", "panic at "] {
            if let Some(at) = line.find(marker) {
                let token = line[at + marker.len()..]
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(':');
                if let Some(found) = location(token)
                    && self.panics.len() < STDERR_KEEP
                {
                    self.panics.insert(found.to_owned());
                }
                break;
            }
        }
    }

    /// The first `QRS-` code seen.
    pub(crate) fn code(&self) -> Option<&str> {
        self.codes.iter().next().map(String::as_str)
    }
}

/// Reduce a stderr pipe on a detached thread; the summary arrives at end
/// of file.
fn drain(pipe: impl std::io::Read + Send + 'static) -> mpsc::Receiver<Stderr> {
    let (send, receive) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name(String::from("bench-stderr"))
        .spawn(move || {
            let mut summary = Stderr::default();
            let mut reader = BufReader::new(pipe);
            let mut raw = Vec::new();
            while reader.read_until(b'\n', &mut raw).is_ok_and(|n| n > 0) {
                summary.take(String::from_utf8_lossy(&raw).trim_end_matches(['\n', '\r']));
                raw.clear();
            }
            let _ = send.send(summary);
        });
    receive
}

/// Read a stdout pipe to end of file on a detached thread.
fn slurp(
    mut pipe: impl std::io::Read + Send + 'static,
) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (send, receive) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name(String::from("bench-stdout"))
        .spawn(move || {
            let mut out = Vec::new();
            let _ = send.send(pipe.read_to_end(&mut out).map(|_| out));
        });
    receive
}

/// One memory sample of a child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Memory {
    /// Resident now (macOS: or the physical footprint, whichever is larger).
    pub(crate) now: u64,
    /// The kernel's high-water mark: the lifetime peak physical footprint
    /// on macOS, `VmHWM` (peak resident) on Linux.
    pub(crate) peak: u64,
    /// The child has exited and waits to be reaped.
    pub(crate) exited: bool,
}

impl Memory {
    pub(crate) fn most(self) -> u64 {
        self.now.max(self.peak)
    }
}

/// Where the caps hold: 64-bit macOS and Linux, whose `wait4`, `waitid`,
/// process groups, signals and memory probes this file codes.
#[cfg(all(
    any(target_os = "macos", target_os = "linux"),
    target_pointer_width = "64"
))]
mod sys {
    use super::{Exit, Memory, Usage};

    /// `struct timeval`: `suseconds_t` is 32-bit on macOS, 64-bit on Linux;
    /// both layouts are 16 bytes.
    #[repr(C)]
    #[derive(Default)]
    struct Timeval {
        sec: i64,
        #[cfg(target_os = "macos")]
        usec: i32,
        #[cfg(target_os = "macos")]
        _pad: i32,
        #[cfg(not(target_os = "macos"))]
        usec: i64,
    }

    /// `struct rusage` on 64-bit macOS and Linux: two timevals, 14 longs.
    #[repr(C)]
    #[derive(Default)]
    struct Rusage {
        utime: Timeval,
        stime: Timeval,
        maxrss: i64,
        _rest: [i64; 13],
    }

    /// `struct rlimit`: `rlim_t` is 64-bit on both.
    #[repr(C)]
    struct Rlimit {
        cur: u64,
        max: u64,
    }

    /// `idtype_t` `P_PID` and the `waitid` options of each platform.
    const P_PID: i32 = 1;
    const WEXITED: i32 = 4;
    #[cfg(target_os = "macos")]
    const WNOWAIT: i32 = 0x20;
    #[cfg(not(target_os = "macos"))]
    const WNOWAIT: i32 = 0x0100_0000;
    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGKILL: i32 = 9;
    const SIGTERM: i32 = 15;
    #[cfg(target_os = "macos")]
    const SIGCHLD: i32 = 20;
    #[cfg(not(target_os = "macos"))]
    const SIGCHLD: i32 = 17;
    const SIG_DFL: usize = 0;
    const SIG_ERR: usize = usize::MAX;
    const RLIMIT_CPU: i32 = 0;
    const RLIMIT_CORE: i32 = 4;

    unsafe extern "C" {
        fn wait4(pid: i32, status: *mut i32, options: i32, rusage: *mut Rusage) -> i32;
        fn waitid(idtype: i32, id: u32, info: *mut u64, options: i32) -> i32;
        fn kill(pid: i32, signal: i32) -> i32;
        fn signal(signum: i32, handler: usize) -> usize;
        fn write(fd: i32, buf: *const u8, count: usize) -> isize;
        fn _exit(status: i32) -> !;
        fn getrlimit(resource: i32, limit: *mut Rlimit) -> i32;
        fn setrlimit(resource: i32, limit: *const Rlimit) -> i32;
    }

    fn micros(t: &Timeval) -> u64 {
        let sec = u64::try_from(t.sec).unwrap_or(0);
        sec * 1_000_000 + u64::try_from(t.usec).unwrap_or(0)
    }

    fn pid_of(pid: u32) -> Result<i32, String> {
        i32::try_from(pid).map_err(|_| format!("pid {pid} out of range"))
    }

    /// Block until child `pid` has exited, leaving it unreaped.
    pub(super) fn wait_exited(pid: u32) -> Result<(), String> {
        // siginfo_t is 104 bytes on macOS, 128 on Linux
        let mut info = [0u64; 32];
        loop {
            // SAFETY: `info` outlives the call and is larger than any
            // siginfo_t; `pid` is our own child.
            let done = unsafe { waitid(P_PID, pid, info.as_mut_ptr(), WEXITED | WNOWAIT) };
            if done == 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("waitid {pid}: {err}"));
            }
        }
    }

    /// Reap `pid` (our own exited child) with its resource usage.
    pub(super) fn reap(pid: u32) -> Result<Usage, String> {
        let pid = pid_of(pid)?;
        let mut status = 0i32;
        let mut usage = Rusage::default();
        loop {
            // SAFETY: both pointers name live locals of the right type;
            // `pid` is a child this process spawned and has not reaped.
            let reaped = unsafe { wait4(pid, &raw mut status, 0, &raw mut usage) };
            if reaped == pid {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("wait4 {pid}: {err}"));
            }
        }
        // WIFEXITED: the low seven bits (the terminating signal) are zero
        let exit = if status.trailing_zeros() >= 7 {
            Exit::Code((status >> 8) & 0xff)
        } else {
            Exit::Signal(status & 0x7f)
        };
        // ru_maxrss: bytes on macOS, KiB on Linux
        let unit = if cfg!(target_os = "macos") { 1 } else { 1024 };
        Ok(Usage {
            max_rss: Some(u64::try_from(usage.maxrss).unwrap_or(0) * unit),
            user_us: Some(micros(&usage.utime)),
            sys_us: Some(micros(&usage.stime)),
            exit,
        })
    }

    /// `SIGKILL` the process group `pgid` leads. Async-signal-safe.
    pub(super) fn kill_group_raw(pgid: i32) {
        if pgid > 0 {
            // SAFETY: plain syscall; the callers only name groups whose
            // leader is our own unreaped child.
            unsafe { kill(-pgid, SIGKILL) };
        }
    }

    /// `SIGKILL` the group of an unreaped child (a zombie leader ignores
    /// it; what it left in the group dies).
    pub(super) fn kill_group(pid: u32) {
        if let Ok(pgid) = pid_of(pid) {
            kill_group_raw(pgid);
        }
    }

    /// Between fork and exec: a CPU-time limit (never above the inherited
    /// hard limit) and no core file. Only async-signal-safe syscalls.
    pub(super) fn limit_cpu(seconds: u64) -> std::io::Result<()> {
        let mut old = Rlimit { cur: 0, max: 0 };
        // SAFETY: plain syscalls on locals of the right type.
        unsafe {
            if getrlimit(RLIMIT_CPU, &raw mut old) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = seconds.min(old.max);
            let cpu = Rlimit {
                cur: limit,
                max: limit,
            };
            if setrlimit(RLIMIT_CPU, &raw const cpu) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let none = Rlimit { cur: 0, max: 0 };
            if setrlimit(RLIMIT_CORE, &raw const none) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// The stop handler: every live child's group is killed, then the
    /// harness exits with 128 + the signal. Only atomics and
    /// async-signal-safe calls.
    extern "C" fn on_stop(signal: i32) {
        super::live::kill_all();
        let note = b"bench: stopped by a signal; every child process group was killed\n";
        // SAFETY: write(2) and _exit are async-signal-safe; the buffer is
        // static.
        unsafe {
            let _ = write(2, note.as_ptr(), note.len());
            _exit(128 + signal);
        }
    }

    /// SIGTERM, SIGHUP and SIGINT stop the run through [`on_stop`];
    /// SIGCHLD goes back to its default, so the kernel never reaps a child
    /// behind our back and frees its pid.
    pub(super) fn guard_signals() -> Result<(), String> {
        // SAFETY: installing dispositions; the handler is async-signal-safe.
        unsafe {
            if signal(SIGCHLD, SIG_DFL) == SIG_ERR {
                return Err(String::from("signal(SIGCHLD)"));
            }
            for number in [SIGHUP, SIGINT, SIGTERM] {
                if signal(number, on_stop as *const () as usize) == SIG_ERR {
                    return Err(format!("signal({number})"));
                }
            }
        }
        Ok(())
    }

    /// `struct rusage_info_v4` (`<sys/resource.h>`): the uuid, then 35
    /// 64-bit counters.
    #[cfg(target_os = "macos")]
    #[repr(C)]
    struct RusageInfoV4 {
        uuid: [u8; 16],
        fields: [u64; 35],
    }

    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV4) -> i32;
    }

    /// The memory of `pid`: resident size or physical footprint, whichever
    /// is larger, and the kernel's lifetime peak footprint — still readable
    /// while the exited child waits to be reaped.
    #[cfg(target_os = "macos")]
    pub(super) fn memory(pid: u32) -> Option<Memory> {
        const RUSAGE_INFO_V4: i32 = 4;
        const RESIDENT: usize = 6;
        const FOOTPRINT: usize = 7;
        const EXITED_AT: usize = 9;
        const LIFETIME_PEAK: usize = 28;
        let pid = pid_of(pid).ok()?;
        let mut info = RusageInfoV4 {
            uuid: [0; 16],
            fields: [0; 35],
        };
        // SAFETY: flavor RUSAGE_INFO_V4 fills exactly this struct.
        let done = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V4, &raw mut info) };
        let fields = info.fields;
        (done == 0).then(|| Memory {
            now: fields[RESIDENT].max(fields[FOOTPRINT]),
            peak: fields[LIFETIME_PEAK],
            exited: fields[EXITED_AT] != 0,
        })
    }

    /// The memory of `pid` from `/proc/<pid>/status`: `VmRSS`, `VmHWM`, and
    /// a zombie's state (it has no memory left to report).
    #[cfg(not(target_os = "macos"))]
    pub(super) fn memory(pid: u32) -> Option<Memory> {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let kib = |key: &str| -> Option<u64> {
            let value: u64 = status
                .lines()
                .find_map(|l| l.strip_prefix(key))?
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .ok()?;
            Some(value * 1024)
        };
        let exited = status
            .lines()
            .find_map(|l| l.strip_prefix("State:"))
            .is_some_and(|s| matches!(s.trim().chars().next(), Some('Z' | 'X')));
        match (kib("VmRSS:"), kib("VmHWM:")) {
            (Some(now), Some(peak)) => Some(Memory { now, peak, exited }),
            _ if exited => Some(Memory {
                now: 0,
                peak: 0,
                exited,
            }),
            _ => None,
        }
    }
}

/// Without these syscalls the caps cannot be enforced: `bench run` refuses
/// to measure there ([`CAPS_ENFORCED`]); this keeps it compiling.
#[cfg(not(all(
    any(target_os = "macos", target_os = "linux"),
    target_pointer_width = "64"
)))]
mod sys {
    use super::{Memory, Usage};

    const UNSUPPORTED: &str = "resource caps need 64-bit macOS or Linux";

    pub(super) fn wait_exited(_pid: u32) -> Result<(), String> {
        Err(String::from(UNSUPPORTED))
    }

    pub(super) fn reap(_pid: u32) -> Result<Usage, String> {
        Err(String::from(UNSUPPORTED))
    }

    pub(super) fn kill_group_raw(_pgid: i32) {}

    pub(super) fn kill_group(_pid: u32) {}

    #[allow(dead_code, reason = "only unix spawns pre-exec limits")]
    pub(super) fn limit_cpu(_seconds: u64) -> std::io::Result<()> {
        Err(std::io::Error::other(UNSUPPORTED))
    }

    pub(super) fn guard_signals() -> Result<(), String> {
        Err(String::from(UNSUPPORTED))
    }

    pub(super) fn memory(_pid: u32) -> Option<Memory> {
        None
    }
}

/// Whether this platform enforces the caps (`bench run` refuses otherwise).
pub(crate) const CAPS_ENFORCED: bool = cfg!(all(
    any(target_os = "macos", target_os = "linux"),
    target_pointer_width = "64"
));

/// Install the stop handler: SIGTERM, SIGHUP and SIGINT kill every live
/// child's process group, then the harness exits with 128 + the signal.
/// Call once, before the first child.
pub(crate) fn guard_signals() -> Result<(), String> {
    sys::guard_signals()
}

/// Whether this process's own memory can be sampled — the precondition of
/// every memory cap.
pub(crate) fn memory_sampled() -> bool {
    sys::memory(std::process::id()).is_some()
}

/// The process groups the stop handler kills: one slot per live child,
/// filled at spawn and emptied before the reap.
mod live {
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::Ordering::{AcqRel, Acquire};

    const SLOTS: usize = 64;
    static GROUPS: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

    /// False when every slot is taken (the child must not run).
    pub(super) fn register(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        GROUPS
            .iter()
            .any(|slot| slot.compare_exchange(0, pid, AcqRel, Acquire).is_ok())
    }

    pub(super) fn deregister(pid: u32) {
        if let Ok(pid) = i32::try_from(pid) {
            for slot in &GROUPS {
                let _ = slot.compare_exchange(pid, 0, AcqRel, Acquire);
            }
        }
    }

    /// Async-signal-safe: atomic loads and `kill`.
    pub(super) fn kill_all() {
        for slot in &GROUPS {
            super::sys::kill_group_raw(slot.load(Acquire));
        }
    }

    #[cfg(test)]
    pub(super) fn holds(pid: u32) -> bool {
        i32::try_from(pid).is_ok_and(|pid| GROUPS.iter().any(|slot| slot.load(Acquire) == pid))
    }
}

const NO_DEADLINE: u64 = u64::MAX;
const KILLED_RSS: u8 = 1;
const KILLED_WALL: u8 = 2;
const KILLED_UNSAMPLED: u8 = 3;

/// State shared by a child's owner and its watchdog.
struct Watch {
    base: Instant,
    rss_cap: u64,
    /// Nanoseconds after `base`; [`NO_DEADLINE`] while idle.
    deadline: AtomicU64,
    stop: AtomicBool,
    killed: AtomicU8,
    /// Largest memory seen: samples and the kernel's high-water mark.
    peak: AtomicU64,
}

impl Watch {
    fn arm(&self, wall: Duration) {
        let wall = u64::try_from(wall.as_nanos()).unwrap_or(u64::MAX);
        self.deadline
            .store(nanos(self.base).saturating_add(wall), Release);
    }

    fn disarm(&self) {
        self.deadline.store(NO_DEADLINE, Release);
    }

    fn cause(&self) -> Option<Kind> {
        match self.killed.load(Acquire) {
            KILLED_RSS => Some(Kind::Rss),
            KILLED_WALL => Some(Kind::Wall),
            KILLED_UNSAMPLED => Some(Kind::Unsampled),
            _ => None,
        }
    }
}

/// A memory probe: the real one, or a test's.
type Sampler = fn(u32) -> Option<Memory>;

fn watchdog(pid: u32, watch: &Watch, sample: Sampler) {
    let mut exited = false;
    while !watch.stop.load(Acquire) {
        if !exited {
            let Some(memory) = sample(pid) else {
                watch.killed.store(KILLED_UNSAMPLED, Release);
                sys::kill_group(pid);
                return;
            };
            watch.peak.fetch_max(memory.most(), Relaxed);
            if memory.most() > watch.rss_cap {
                watch.killed.store(KILLED_RSS, Release);
                sys::kill_group(pid);
                return;
            }
            exited = memory.exited;
        }
        let deadline = watch.deadline.load(Acquire);
        if !exited && deadline != NO_DEADLINE && nanos(watch.base) >= deadline {
            watch.killed.store(KILLED_WALL, Release);
            sys::kill_group(pid);
            return;
        }
        std::thread::park_timeout(POLL);
    }
}

/// A spawned child under its watchdog, in its own process group. Dropped
/// unreaped, it is killed and reaped — never left a zombie.
struct Supervised {
    child: Child,
    watch: Arc<Watch>,
    dog: Option<JoinHandle<()>>,
    sample: Sampler,
    usage: Option<Usage>,
    /// The wait failed: the pid may be someone else's now, so it is never
    /// signalled again.
    lost: bool,
}

impl Supervised {
    /// `cpu`: a kernel CPU-time limit (one-shot children).
    fn spawn(command: &mut Command, rss_cap: u64, cpu: Option<Duration>) -> Result<Self, String> {
        Self::spawn_sampled(command, rss_cap, cpu, sys::memory)
    }

    fn spawn_sampled(
        command: &mut Command,
        rss_cap: u64,
        cpu: Option<Duration>,
        sample: Sampler,
    ) -> Result<Self, String> {
        for key in SCRUBBED {
            command.env_remove(key);
        }
        command.env(PARENT_ENV, std::process::id().to_string());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            // its own group: a stop kills what it spawned too, and terminal
            // signals reach the harness, whose handler stops the children
            command.process_group(0);
            if let Some(cpu) = cpu {
                let seconds = cpu.as_secs() + u64::from(cpu.subsec_nanos() > 0);
                // SAFETY: the hook runs between fork and exec and only makes
                // async-signal-safe syscalls; it allocates nothing.
                unsafe {
                    command.pre_exec(move || sys::limit_cpu(seconds));
                }
            }
        }
        #[cfg(not(unix))]
        let _ = cpu;
        let base = Instant::now();
        let child = command.spawn().map_err(|e| format!("spawn: {e}"))?;
        let pid = child.id();
        let registered = live::register(pid);
        let first = sample(pid);
        let watch = Arc::new(Watch {
            base,
            rss_cap,
            deadline: AtomicU64::new(NO_DEADLINE),
            stop: AtomicBool::new(false),
            killed: AtomicU8::new(0),
            peak: AtomicU64::new(first.map_or(0, Memory::most)),
        });
        let mut supervised = Self {
            child,
            watch,
            dog: None,
            sample,
            usage: None,
            lost: false,
        };
        if !registered || first.is_none() {
            // never run a child the caps cannot follow
            supervised.kill();
            let _ = supervised.wait();
            return Err(if registered {
                String::from("the child's memory cannot be sampled: not run")
            } else {
                String::from("too many live children")
            });
        }
        let shared = Arc::clone(&supervised.watch);
        let dog = std::thread::Builder::new()
            .name(String::from("bench-watchdog"))
            .spawn(move || watchdog(pid, &shared, sample))
            .map_err(|e| format!("watchdog: {e}"))?;
        supervised.dog = Some(dog);
        Ok(supervised)
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn kill(&self) {
        if self.usage.is_none() && !self.lost {
            sys::kill_group(self.pid());
        }
    }

    /// Block until the child exits, read the kernel's peak, kill what is
    /// left of its group, stop the watchdog, reap.
    fn wait(&mut self) -> Result<Usage, String> {
        if let Some(usage) = self.usage {
            return Ok(usage);
        }
        if self.lost {
            return Err(String::from("the child was lost"));
        }
        let pid = self.pid();
        let exited = sys::wait_exited(pid);
        if exited.is_ok() {
            if let Some(memory) = (self.sample)(pid) {
                self.watch.peak.fetch_max(memory.most(), Relaxed);
            }
            sys::kill_group(pid);
        }
        self.watch.stop.store(true, Release);
        if let Some(dog) = self.dog.take() {
            dog.thread().unpark();
            let _ = dog.join();
        }
        live::deregister(pid);
        let reaped = exited.and_then(|()| sys::reap(pid));
        match reaped {
            Ok(usage) => {
                self.usage = Some(usage);
                Ok(usage)
            }
            Err(e) => {
                self.lost = true;
                Err(e)
            }
        }
    }

    fn sampled_peak(&self) -> u64 {
        self.watch.peak.load(Relaxed)
    }

    fn peak(&self, usage: Option<Usage>) -> u64 {
        usage
            .and_then(|u| u.max_rss)
            .unwrap_or(0)
            .max(self.sampled_peak())
    }

    /// The call this child did not complete — see [`classify`].
    fn failure(&self, usage: Usage, wall_ns: u64, caps: Caps, mid_call: bool) -> Option<Failure> {
        let peak = self.peak(Some(usage));
        let kind = classify(
            self.watch.cause(),
            Some(usage.exit),
            (peak, wall_ns),
            caps,
            mid_call,
        )?;
        Some(Failure {
            kind,
            peak_rss: (peak > 0).then_some(peak),
            wall_ns,
        })
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if self.usage.is_none() && !self.lost {
            self.kill();
            let _ = self.wait();
        }
    }
}

/// A one-shot child: wall time from spawn to exit, its output, usage and
/// stderr summary, and the resource failure that ended it, if one did.
pub(crate) struct Finished {
    pub(crate) ns: u64,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Stderr,
    pub(crate) usage: Usage,
    pub(crate) failure: Option<Failure>,
}

/// Run `command` to completion under `caps`, with a kernel CPU-time limit
/// at the wall cap — it holds even if the harness dies.
pub(crate) fn run_once(command: &mut Command, caps: Caps) -> Result<Finished, String> {
    run_once_with(command, caps, sys::memory)
}

/// Run a short system probe (`ps`, `lsof`, `pmset`, `sysctl`, `git`…) like
/// [`run_once`] — its own process group, the wall cap, the CPU-time limit,
/// the kernel peak checked at exit — but even when its memory cannot be
/// sampled while it runs: macOS `ps` is setuid root and hides it from its
/// caller. Only the live RSS check is lost for such a probe; measured
/// children never come through here.
pub(crate) fn run_probe(command: &mut Command, caps: Caps) -> Result<Finished, String> {
    run_once_with(command, caps, probe_memory)
}

/// A probe's memory: an unreadable sample reads as none ([`run_probe`]).
#[expect(
    clippy::unnecessary_wraps,
    reason = "a Sampler, like sys::memory; only this one never fails"
)]
fn probe_memory(pid: u32) -> Option<Memory> {
    Some(sys::memory(pid).unwrap_or(Memory {
        now: 0,
        peak: 0,
        exited: false,
    }))
}

fn run_once_with(command: &mut Command, caps: Caps, sample: Sampler) -> Result<Finished, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = Supervised::spawn_sampled(command, caps.rss, Some(caps.wall), sample)?;
    child.watch.arm(caps.wall);
    let stderr = child.child.stderr.take().map(drain);
    let stdout = child.child.stdout.take().map(slurp);
    let usage = child.wait()?;
    let ns = nanos(started);
    let failure = child.failure(usage, ns, caps, false);
    let stdout = match stdout.map(|r| r.recv_timeout(PIPE_WAIT)) {
        Some(Ok(Ok(bytes))) => bytes,
        Some(Ok(Err(e))) if failure.is_none() => return Err(format!("read stdout: {e}")),
        _ => Vec::new(),
    };
    let stderr = stderr
        .and_then(|r| r.recv_timeout(PIPE_WAIT).ok())
        .unwrap_or_default();
    Ok(Finished {
        ns,
        stdout,
        stderr,
        usage,
        failure,
    })
}

/// Why a worker request did not return a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallError {
    /// The worker died, was killed, or answered past a cap (it is reaped).
    Died(Failure),
    /// The caller gave up (the gate closed); the worker is killed and reaped.
    Aborted,
    /// The pipe or the protocol broke — the harness's fault, not the scan's.
    Protocol(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Died(failure) => match failure.kind.code() {
                Some(code) => write!(f, "the worker died ({} {code})", failure.kind.as_str()),
                None => write!(f, "the worker died ({})", failure.kind.as_str()),
            },
            Self::Aborted => f.write_str("abandoned"),
            Self::Protocol(e) => f.write_str(e),
        }
    }
}

/// How a persistent worker ended.
#[derive(Debug, Clone)]
pub(crate) struct End {
    pub(crate) farewell: String,
    pub(crate) usage: Option<Usage>,
    pub(crate) sampled_peak: u64,
    pub(crate) stderr: Stderr,
    /// Set when a cap or a signal ended the worker during `quit`.
    pub(crate) failure: Option<Failure>,
}

impl End {
    /// Peak memory of the whole process: the kernel's peak RSS or the
    /// watchdog's largest sample.
    pub(crate) fn peak(&self) -> Option<u64> {
        let peak = self
            .usage
            .and_then(|u| u.max_rss)
            .unwrap_or(0)
            .max(self.sampled_peak);
        (peak > 0).then_some(peak)
    }
}

/// A persistent worker speaking the line protocol of `worker.rs`. Replies
/// arrive through a reader thread, so a wait can time out; the watchdog
/// bounds every request by its wall cap and the process by the memory cap.
pub(crate) struct Server {
    label: String,
    child: Supervised,
    stdin: Option<ChildStdin>,
    replies: mpsc::Receiver<String>,
    stderr: Option<mpsc::Receiver<Stderr>>,
}

impl Server {
    pub(crate) fn spawn(mut command: Command, label: &str, rss_cap: u64) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            Supervised::spawn(&mut command, rss_cap, None).map_err(|e| format!("{label}: {e}"))?;
        let stdin = child.child.stdin.take();
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or_else(|| format!("{label}: no stdout"))?;
        let stderr = child.child.stderr.take().map(drain);
        let (send, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name(String::from("bench-replies"))
            .spawn(move || {
                let mut lines = BufReader::new(stdout);
                let mut line = String::new();
                while lines.read_line(&mut line).is_ok_and(|n| n > 0) {
                    let reply = line.trim_end_matches(['\n', '\r']).to_owned();
                    if send.send(reply).is_err() {
                        return;
                    }
                    line.clear();
                }
            })
            .map_err(|e| format!("{label}: reader: {e}"))?;
        Ok(Self {
            label: label.to_owned(),
            child,
            stdin,
            replies,
            stderr,
        })
    }

    /// The worker's pid (tests and logs only).
    #[cfg(test)]
    pub(crate) fn pid(&self) -> u32 {
        self.child.pid()
    }

    /// One request line, one reply line, within `wall`.
    pub(crate) fn request(&mut self, line: &str, wall: Duration) -> Result<String, CallError> {
        self.request_watch(line, wall, Duration::MAX, &mut |_| true)
    }

    /// [`Server::request`], calling `tick` with the elapsed time about every
    /// `every` while waiting; `tick` returning false kills the worker and
    /// gives [`CallError::Aborted`]. A reply that comes past `wall`, or with
    /// the worker's kernel peak above the memory cap, is a resource failure:
    /// the worker is killed and reaped.
    pub(crate) fn request_watch(
        &mut self,
        line: &str,
        wall: Duration,
        every: Duration,
        tick: &mut dyn FnMut(Duration) -> bool,
    ) -> Result<String, CallError> {
        let started = Instant::now();
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(CallError::Protocol(format!("{}: stdin closed", self.label)));
        };
        self.child.watch.arm(wall);
        if writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .is_err()
        {
            return Err(self.died(started, wall));
        }
        let limit = wall.saturating_add(GRACE);
        let mut ticked = Instant::now();
        loop {
            match self.replies.recv_timeout(POLL) {
                Ok(reply) => {
                    self.child.watch.disarm();
                    return self.returned(reply, started, wall);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(self.died(started, wall)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            // killed by the watchdog: an orphaned grandchild may still hold
            // the pipe open, so the kill — not end of file — ends the wait
            if self.child.watch.cause().is_some() {
                return Err(self.died(started, wall));
            }
            if started.elapsed() >= limit {
                self.child.watch.killed.store(KILLED_WALL, Release);
                return Err(self.died(started, wall));
            }
            if ticked.elapsed() >= every {
                ticked = Instant::now();
                if !tick(started.elapsed()) {
                    self.child.kill();
                    let _ = self.child.wait();
                    return Err(CallError::Aborted);
                }
            }
        }
    }

    /// A reply arrived: still a resource failure when it came past the wall
    /// cap or the kernel's high-water mark passed the memory cap between
    /// two watchdog samples.
    fn returned(
        &mut self,
        reply: String,
        started: Instant,
        wall: Duration,
    ) -> Result<String, CallError> {
        let wall_ns = nanos(started);
        if let Some(memory) = (self.child.sample)(self.child.pid()) {
            self.child.watch.peak.fetch_max(memory.most(), Relaxed);
        }
        let peak = self.child.sampled_peak();
        let caps = Caps {
            rss: self.child.watch.rss_cap,
            wall,
        };
        let Some(kind) = classify(None, None, (peak, wall_ns), caps, false) else {
            return Ok(reply);
        };
        self.child.kill();
        let _ = self.child.wait();
        Err(CallError::Died(Failure {
            kind,
            peak_rss: Some(peak),
            wall_ns,
        }))
    }

    /// The worker went away during a call: kill what is left, reap, and
    /// describe the failure.
    fn died(&mut self, started: Instant, wall: Duration) -> CallError {
        self.child.kill();
        let caps = Caps {
            rss: self.child.watch.rss_cap,
            wall,
        };
        match self.child.wait() {
            Ok(usage) => self
                .child
                .failure(usage, nanos(started), caps, true)
                .map_or_else(
                    || CallError::Protocol(format!("{}: the worker left", self.label)),
                    CallError::Died,
                ),
            Err(e) => CallError::Protocol(format!("{}: {e}", self.label)),
        }
    }

    fn close(&mut self) -> Stderr {
        drop(self.stdin.take());
        self.stderr
            .take()
            .and_then(|r| r.recv_timeout(PIPE_WAIT).ok())
            .unwrap_or_default()
    }

    /// `quit`, close stdin, reap: the farewell line and the process usage.
    /// A worker that does not leave within [`QUIT_WALL`] is killed; a cap
    /// or a signal that ends it here is its [`End::failure`].
    pub(crate) fn finish(mut self) -> End {
        let started = Instant::now();
        let farewell = self.request("quit", QUIT_WALL).unwrap_or_default();
        drop(self.stdin.take());
        self.child.watch.arm(QUIT_WALL);
        let usage = self.child.wait().ok();
        let caps = Caps {
            rss: self.child.watch.rss_cap,
            wall: QUIT_WALL.saturating_add(QUIT_WALL),
        };
        let failure = usage.and_then(|u| self.child.failure(u, nanos(started), caps, false));
        End {
            farewell,
            usage,
            sampled_peak: self.child.sampled_peak(),
            stderr: self.close(),
            failure,
        }
    }

    /// Kill and reap: after a failure, or when an attempt is abandoned.
    pub(crate) fn kill(mut self) -> End {
        self.child.kill();
        let usage = self.child.wait().ok();
        End {
            farewell: String::new(),
            usage,
            sampled_peak: self.child.sampled_peak(),
            stderr: self.close(),
            failure: None,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stdin.take());
        self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux"),
    target_pointer_width = "64"
))]
pub(crate) mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    fn caps(rss_mib: u64, wall: Duration) -> Caps {
        Caps {
            rss: rss_mib * MIB,
            wall,
        }
    }

    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }

    /// The pid was reaped: signal 0 finds no such process.
    fn gone(pid: u32) -> bool {
        let pid = i32::try_from(pid).expect("pid");
        // SAFETY: signal 0 only probes for existence.
        unsafe { kill(pid, 0) != 0 }
    }

    /// Gone within `limit` (an orphan's reaping by init takes a moment).
    pub(crate) fn gone_within(pid: u32, limit: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < limit {
            if gone(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        gone(pid)
    }

    fn died(error: CallError) -> Failure {
        match error {
            CallError::Died(failure) => failure,
            other => panic!("expected a resource failure, got {other:?}"),
        }
    }

    /// This test binary run again as a helper process: only the named
    /// ignored test, told what to be by `QRSCAN_BENCH_TEST_HELPER`.
    pub(crate) fn helper(test: &str, role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        command
            .args([test, "--exact", "--ignored", "--nocapture"])
            .env("QRSCAN_BENCH_TEST_HELPER", role)
            .env_remove(PARENT_ENV);
        command
    }

    pub(crate) fn helper_role() -> Option<String> {
        std::env::var("QRSCAN_BENCH_TEST_HELPER").ok()
    }

    /// The first `<tag> <number>` line a helper prints.
    pub(crate) fn read_pid(out: impl std::io::Read, tag: &str) -> u32 {
        BufReader::new(out)
            .lines()
            .map_while(Result::ok)
            .find_map(|l| l.strip_prefix(tag).and_then(|n| n.trim().parse().ok()))
            .unwrap_or_else(|| panic!("no {tag} line"))
    }

    #[test]
    fn one_shot_children_report_exit_and_usage() {
        let done = run_once(
            &mut sh("printf out; printf 'oops [QRS-002] x\\n' 1>&2; exit 3"),
            caps(64, Duration::from_secs(10)),
        )
        .expect("sh runs");
        assert_eq!(done.stdout, b"out");
        assert_eq!(done.stderr.lines, 1);
        assert_eq!(done.stderr.code(), Some("QRS-002"));
        assert_eq!(done.usage.exit, Exit::Code(3));
        assert_eq!(done.failure, None, "an exit code is the caller's to read");
        assert!(done.ns > 0);
        assert!(done.usage.max_rss.is_some_and(|rss| rss > 0));
    }

    /// A pipe deadlock: 1 MiB on stderr before stdout would block both
    /// sides without the drain thread, which takes it.
    #[test]
    fn a_chatty_stderr_never_blocks() {
        let started = Instant::now();
        let done = run_once(
            &mut sh("head -c 1048576 /dev/zero | tr '\\0' 'x' 1>&2; echo out"),
            caps(256, Duration::from_secs(20)),
        )
        .expect("runs");
        assert_eq!(done.stdout, b"out\n");
        assert_eq!((done.stderr.lines, done.stderr.bytes), (1, 1_048_577));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_wall_cap_kills_and_reaps() {
        let started = Instant::now();
        let done =
            run_once(&mut sh("exec sleep 30"), caps(64, Duration::from_secs(1))).expect("runs");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(done.usage.exit, Exit::Signal(9));
        let failure = done.failure.expect("a resource failure");
        assert_eq!(failure.kind, Kind::Wall);
        assert!(failure.wall_ns >= 1_000_000_000);
    }

    /// A child allocating and touching 128 MiB under a 64 MiB cap.
    #[test]
    fn the_rss_cap_kills_and_keeps_the_peak() {
        let mut command = Command::new("python3");
        command.args([
            "-c",
            "b = bytearray(128 * 1024 * 1024)\nimport time\ntime.sleep(30)",
        ]);
        let started = Instant::now();
        let done = run_once(&mut command, caps(64, Duration::from_secs(20))).expect("runs");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(done.usage.exit, Exit::Signal(9));
        let failure = done.failure.expect("a resource failure");
        assert_eq!(failure.kind, Kind::Rss);
        assert!(
            failure.peak_rss.is_some_and(|p| p > 64 * MIB),
            "{failure:?}"
        );
    }

    /// A call that returned is still a resource failure when the kernel's
    /// peak passed the memory cap or the call outlasted its wall; the
    /// watchdog's verdict, a signal and a mid-call exit come first.
    #[test]
    fn returned_calls_past_a_cap_are_resource_failures() {
        let cap = caps(64, Duration::from_millis(500));
        let ok = Some(Exit::Code(0));
        assert_eq!(
            classify(None, ok, (64 * MIB, 500_000_000), cap, false),
            None
        );
        assert_eq!(
            classify(None, ok, (64 * MIB + 1, 1), cap, false),
            Some(Kind::Rss),
            "a spike between two samples, read from the kernel"
        );
        assert_eq!(
            classify(None, ok, (1, 500_000_001), cap, false),
            Some(Kind::Wall),
            "returned past the wall cap"
        );
        assert_eq!(
            classify(None, Some(Exit::Code(3)), (1, 1), cap, false),
            None
        );
        assert_eq!(
            classify(None, Some(Exit::Code(3)), (1, 1), cap, true),
            Some(Kind::Exit(3))
        );
        assert_eq!(
            classify(None, Some(Exit::Signal(11)), (1, 1), cap, false),
            Some(Kind::Signal(11))
        );
        assert_eq!(
            classify(Some(Kind::Unsampled), ok, (1, 1), cap, false),
            Some(Kind::Unsampled)
        );

        // a one-shot child that touches 96 MiB and exits at once under a
        // 64 MiB cap: killed by the watchdog or flagged on return, never a
        // sample
        let mut spike = Command::new("python3");
        spike.args([
            "-c",
            "b = bytearray(96 * 1024 * 1024)\nfor i in range(0, len(b), 4096): b[i] = 1",
        ]);
        let done = run_once(&mut spike, caps(64, Duration::from_secs(20))).expect("runs");
        assert_eq!(
            done.failure.map(|f| f.kind),
            Some(Kind::Rss),
            "{:?}",
            done.usage
        );

        // a worker answering after its wall: a resource failure, reaped
        let mut late = Server::spawn(
            sh("while read l; do sleep 0.3; echo \"re:$l\"; done"),
            "late",
            64 * MIB,
        )
        .expect("spawn");
        let pid = late.pid();
        let failure = died(
            late.request("x", Duration::from_millis(200))
                .expect_err("past the wall"),
        );
        assert_eq!(failure.kind, Kind::Wall);
        drop(late);
        assert!(gone(pid));
    }

    #[test]
    fn a_signal_is_a_resource_failure() {
        let done =
            run_once(&mut sh("kill -SEGV $$"), caps(64, Duration::from_secs(10))).expect("runs");
        assert_eq!(done.usage.exit, Exit::Signal(11));
        assert_eq!(done.failure.map(|f| f.kind), Some(Kind::Signal(11)));
    }

    /// A one-shot child carries a kernel CPU-time limit at its wall cap
    /// (rounded up) and writes no core file — limits that hold even if the
    /// harness dies.
    #[test]
    fn one_shot_children_carry_a_cpu_limit_at_the_wall_cap() {
        let done = run_once(
            &mut sh("printf '%s %s' \"$(ulimit -t)\" \"$(ulimit -c)\""),
            caps(64, Duration::from_millis(6500)),
        )
        .expect("runs");
        assert_eq!(String::from_utf8_lossy(&done.stdout), "7 0");
    }

    /// The child runs in its own process group: a cap kills what it
    /// spawned along with it.
    #[test]
    fn a_cap_kills_the_whole_group() {
        let done = run_once(
            &mut sh("sleep 30 & echo $!; exec sleep 30"),
            caps(64, Duration::from_secs(1)),
        )
        .expect("runs");
        assert_eq!(done.failure.map(|f| f.kind), Some(Kind::Wall));
        let grandchild: u32 = String::from_utf8_lossy(&done.stdout)
            .trim()
            .parse()
            .expect("the grandchild's pid");
        assert!(
            gone_within(grandchild, Duration::from_secs(2)),
            "the grandchild went with its group"
        );
    }

    /// A child whose memory cannot be sampled is killed before it runs,
    /// reaped, and dropped from the stop table.
    #[test]
    fn an_unsampleable_child_is_not_run() {
        let mut command = sh("exec sleep 30");
        let refused = Supervised::spawn_sampled(&mut command, 64 * MIB, None, |_| None);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("cannot be sampled")),
            "{:?}",
            refused.as_ref().err()
        );
        assert!(memory_sampled(), "this host samples its own memory");
    }

    /// macOS `ps` is setuid root: its memory cannot be sampled. Measured,
    /// it would be refused; as a probe it runs under its wall cap — the
    /// gate's process table comes from it, so refusing the probes too would
    /// close every reading.
    #[test]
    #[cfg(target_os = "macos")]
    fn setuid_probes_run_where_measured_children_would_not() {
        let me = std::process::id().to_string();
        let ps = || {
            let mut ps = Command::new("ps");
            ps.args(["-o", "pid=", "-p", &me]);
            ps
        };
        let measured = run_once(&mut ps(), caps(64, Duration::from_secs(10)));
        assert!(
            measured
                .as_ref()
                .is_err_and(|e| e.contains("cannot be sampled")),
            "{:?}",
            measured.as_ref().err()
        );
        let probe = run_probe(&mut ps(), caps(64, Duration::from_secs(10))).expect("runs");
        assert_eq!(probe.failure, None);
        assert_eq!(probe.usage.exit, Exit::Code(0));
        assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), me);
    }

    /// The kernel's high-water mark of a live child, and of one that has
    /// exited but waits to be reaped.
    #[test]
    fn memory_samples_carry_the_kernel_peak() {
        let mut command = sh("exec sleep 30");
        let child = Supervised::spawn(&mut command, 64 * MIB, None).expect("spawn");
        let live = sys::memory(child.pid()).expect("a live child is sampled");
        assert!(live.now > 0 && live.peak > 0 && !live.exited, "{live:?}");
        assert!(live::holds(child.pid()), "the stop handler knows it");
        let pid = child.pid();
        drop(child);
        assert!(!live::holds(pid), "out of the table before the reap");
    }

    #[test]
    fn servers_answer_line_by_line_and_are_reaped() {
        let mut server = Server::spawn(
            sh("while read l; do echo \"re:$l\"; done"),
            "echo",
            64 * MIB,
        )
        .expect("spawn");
        let wall = Duration::from_secs(10);
        assert_eq!(server.request("one", wall).as_deref(), Ok("re:one"));
        assert_eq!(server.request("two", wall).as_deref(), Ok("re:two"));
        let end = server.finish();
        assert_eq!(end.farewell, "re:quit");
        assert_eq!(end.usage.map(|u| u.exit), Some(Exit::Code(0)));
        assert_eq!(end.failure, None);
    }

    /// A worker that a signal ends during `quit`: the end is a failure.
    #[test]
    fn a_worker_killed_at_quit_reports_it() {
        let server = Server::spawn(
            sh("while read l; do case \"$l\" in quit) kill -KILL $$;; *) echo ok;; esac; done"),
            "quitter",
            64 * MIB,
        )
        .expect("spawn");
        let end = server.finish();
        assert_eq!(end.failure.map(|f| f.kind), Some(Kind::Signal(9)));
    }

    /// A worker that hangs is killed at the wall cap — even while an
    /// orphaned grandchild still holds its stdout — and a respawned worker
    /// answers the next request.
    #[test]
    fn a_hung_worker_is_killed_and_a_respawn_answers() {
        let script =
            "while read l; do case \"$l\" in hang) sleep 30;; *) echo \"re:$l\";; esac; done";
        let mut server = Server::spawn(sh(script), "hang", 64 * MIB).expect("spawn");
        let pid = server.pid();
        let started = Instant::now();
        let failure = died(
            server
                .request("hang", Duration::from_secs(1))
                .expect_err("killed"),
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(failure.kind, Kind::Wall);
        drop(server);
        assert!(gone(pid), "killed and reaped, no zombie");
        let mut respawned = Server::spawn(sh(script), "hang", 64 * MIB).expect("respawn");
        assert_eq!(
            respawned
                .request("next", Duration::from_secs(10))
                .as_deref(),
            Ok("re:next")
        );
    }

    #[test]
    fn a_worker_exiting_mid_call_is_attributed() {
        let mut server = Server::spawn(sh("read l; exit 7"), "crash", 64 * MIB).expect("spawn");
        let failure = died(
            server
                .request("scan", Duration::from_secs(10))
                .expect_err("died"),
        );
        assert_eq!(failure.kind, Kind::Exit(7));
    }

    #[test]
    fn a_caller_can_abandon_a_request() {
        let mut server =
            Server::spawn(sh("read l; exec sleep 30"), "slow", 64 * MIB).expect("spawn");
        let pid = server.pid();
        let mut ticks = 0;
        let error = server
            .request_watch(
                "go",
                Duration::from_secs(20),
                Duration::from_millis(50),
                &mut |_| {
                    ticks += 1;
                    ticks < 3
                },
            )
            .expect_err("abandoned");
        assert_eq!((error, ticks), (CallError::Aborted, 3));
        drop(server);
        assert!(gone(pid));
    }

    #[test]
    fn dropped_servers_leave_no_zombie() {
        let server = Server::spawn(sh("exec sleep 30"), "idle", 64 * MIB).expect("spawn");
        let pid = server.pid();
        drop(server);
        assert!(gone(pid));
    }

    /// The helper side of the stop test: the stop handler installed, one
    /// supervised child alive, then a long wait for the signal.
    #[test]
    #[ignore = "a helper process of a_stopped_harness_takes_its_children_along"]
    fn stop_helper() {
        if helper_role().as_deref() != Some("stop") {
            return;
        }
        guard_signals().expect("handlers");
        let server = Server::spawn(sh("exec sleep 30"), "victim", 64 * MIB).expect("spawn");
        println!("victim {}", server.pid());
        std::thread::sleep(Duration::from_secs(30));
        drop(server);
    }

    /// With a fake child: SIGTERM to the harness's pid alone — as
    /// `kill <pid>` sends it — kills the child's group before the harness
    /// exits (128 + 15).
    #[test]
    fn a_stopped_harness_takes_its_children_along() {
        let mut harness = helper("bench::proc::tests::stop_helper", "stop")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("helper");
        let victim = read_pid(harness.stdout.take().expect("stdout"), "victim ");
        assert!(!gone(victim), "the child runs");
        let pid = i32::try_from(harness.id()).expect("pid");
        // SAFETY: a plain signal to the helper we spawned and still hold.
        unsafe { kill(pid, 15) };
        let status = harness.wait().expect("the harness exits");
        assert_eq!(status.code(), Some(143), "128 + SIGTERM");
        assert!(
            gone_within(victim, Duration::from_secs(2)),
            "the child went with the harness"
        );
    }

    #[test]
    fn stderr_keeps_codes_and_panic_locations_only() {
        let mut summary = Stderr::default();
        summary.take(
            "thread 'main' panicked at /home/u/.cargo/registry/src/x/rxing-0.9.1/src/oned/rss_14_reader.rs:184:13:",
        );
        summary.take("attempt to subtract with overflow: secret-payload-text");
        summary
            .take("qrscan-bench-worker: panic at crates/qrcode-ai-scanner/src/engine/mod.rs:61:9");
        summary.take("image too large [QRS-002] then [QRS-004]");
        // markers carrying anything but a code or a location keep nothing
        summary.take("[QRS-12345] [QRS-] [QRS-7 secret] [QRS-x]");
        summary.take("thread 'x' panicked at secret-payload-text: index out of bounds");
        summary.take("panic at secret payload.rs:1:2");
        summary.take("panicked at weird name.rs:1:2:");
        summary.take("panicked at src/lib.rs:12:");
        assert_eq!(summary.lines, 9);
        assert_eq!(
            summary.panics.iter().cloned().collect::<Vec<_>>(),
            vec![
                String::from("mod.rs:61:9"),
                String::from("rss_14_reader.rs:184:13")
            ]
        );
        assert_eq!(
            summary.codes.iter().cloned().collect::<Vec<_>>(),
            vec![String::from("QRS-002"), String::from("QRS-004")]
        );
        assert!(
            !format!("{summary:?}").contains("secret"),
            "no stderr text is kept"
        );
    }

    #[test]
    fn children_never_inherit_backtrace_settings() {
        let mut command = sh(
            "printf '%s|%s|%s' \"${RUST_BACKTRACE:-unset}\" \"${NODE_OPTIONS:-unset}\" \"$QRSCAN_BENCH_PARENT\"",
        );
        command
            .env("RUST_BACKTRACE", "1")
            .env("NODE_OPTIONS", "--inspect");
        let done = run_once(&mut command, caps(64, Duration::from_secs(10))).expect("runs");
        assert_eq!(
            String::from_utf8_lossy(&done.stdout),
            format!("unset|unset|{}", std::process::id()),
            "and every child learns which parent to keep"
        );
    }
}
