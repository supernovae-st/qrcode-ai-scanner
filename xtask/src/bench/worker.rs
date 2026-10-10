//! The in-process benchmark worker.
//!
//! Compiled here (lints and tests) and, through a `#[path]` module, into
//! the two binaries `xtask bench prepare` generates outside the workspace:
//! one linked against this tree, one against the published `=0.9.0` crate.
//! The same source must build against both, so it uses std and the
//! scanner's public API only — no other crate, nothing from `xtask`.
//!
//! Line protocol [`PROTOCOL`], tab-separated, one request per stdin line and
//! one reply per stdout line (shared with `scripts/bench-node.mjs` and
//! `scripts/bench-wasm.mjs`):
//!
//! - `hello` → `hello · protocol · scanner · pipeline · score_contract ·
//!   runtime · worker source sha256 · main source sha256`
//! - `scan · profile · budget · memory · path` → `ok · ns · total_ms ·
//!   engine_panics · peak_heap · allocs · alloc_bytes · retained · stages ·
//!   transforms · judgment · n · (symbology · text_hex)×n`, or
//!   `error · ns · code`, or `fail · reason`
//! - `throughput · profile · budget · threads · bound · list` → `ok · jobs ·
//!   ns · failed`
//! - `quit` → `bye`
//!
//! `profile` is `full | fast | frame`, `budget` is `default` (the preset's
//! wall clock) or `unbounded` (`budget_ms: None`, what every binding maps
//! `0` to). `ns` is the public call alone: the file is read before the
//! clock starts. `memory = 1` counts the call's heap through [`Counting`]
//! (the orchestrator asks for it on warm-up runs only, so timed runs pay
//! one relaxed load per allocation) and, keeping the window open while the
//! report is dropped, the bytes the call left allocated (`retained`).
//! `stages`, `transforms` (Σ `transforms_tried`) and `judgment` (the
//! score's [`judgment`] signature, `-` without a score) describe the walk:
//! the orchestrator compares them with the image's unbudgeted reference
//! walk to tell a truncated call from a complete one. Decoded text crosses
//! the pipe as hex and is hashed by the orchestrator on arrival — never
//! written anywhere.
//!
//! A worker leaves within about [`PARENT_POLL`] once its parent changes
//! ([`exit_with_parent`]): a harness killed outright cannot leave it
//! scanning, uncapped, on its own.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::io::{BufRead as _, Write as _};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use qrcode_ai_scanner::{ImageInput, ScanProfile, ScanReport, Scanner, Versions};

/// The worker protocol this file speaks; `hello` names it.
pub const PROTOCOL: &str = "qrscan-bench-worker/3";

/// The variable the harness sets on every child: its own pid, the parent
/// a worker must keep.
pub const PARENT_ENV: &str = "QRSCAN_BENCH_PARENT";
/// How often a worker checks that its parent is still there.
pub const PARENT_POLL: Duration = Duration::from_millis(50);

/// Leave at once — no unwinding, no exit handlers, no lock taken — when the
/// parent is not the one this worker must keep ([`PARENT_ENV`], else the
/// parent at start): checked now, then every [`PARENT_POLL`] on a thread of
/// its own, so a scan in flight cannot hold it up.
/// A harness killed outright never leaves a worker scanning, uncapped.
#[cfg(unix)]
pub fn exit_with_parent() {
    use std::os::unix::process::parent_id;
    let keep = std::env::var(PARENT_ENV)
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
        .unwrap_or_else(parent_id);
    if parent_id() != keep {
        leave();
    }
    let _ = std::thread::Builder::new()
        .name(String::from("bench-parent"))
        .spawn(move || {
            loop {
                std::thread::sleep(PARENT_POLL);
                if parent_id() != keep {
                    leave();
                }
            }
        });
}

/// No parent to lose where processes are not reparented.
#[cfg(not(unix))]
pub fn exit_with_parent() {}

#[cfg(unix)]
fn leave() -> ! {
    unsafe extern "C" {
        fn _exit(status: i32) -> !;
    }
    // SAFETY: _exit ends the process without running any user code, so it
    // is sound from any thread, even mid-scan or mid-allocation.
    unsafe { _exit(3) }
}

/// Counting global allocator: the worker binaries install it with
/// `#[global_allocator]`. Bookkeeping runs only while a window is open.
pub struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
/// Bytes allocated minus bytes freed since the window opened (frees of
/// older blocks can take it below zero).
static NET: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

fn signed(size: usize) -> isize {
    isize::try_from(size).unwrap_or(isize::MAX)
}

fn on_alloc(size: usize) {
    if COUNTING.load(Relaxed) {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(u64::try_from(size).unwrap_or(u64::MAX), Relaxed);
        let net = NET
            .fetch_add(signed(size), Relaxed)
            .saturating_add(signed(size));
        PEAK.fetch_max(net, Relaxed);
    }
}

fn on_free(size: usize) {
    if COUNTING.load(Relaxed) {
        NET.fetch_sub(signed(size), Relaxed);
    }
}

fn on_realloc(old: usize, new: usize) {
    if COUNTING.load(Relaxed) {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(u64::try_from(new).unwrap_or(u64::MAX), Relaxed);
        let delta = signed(new).saturating_sub(signed(old));
        let net = NET.fetch_add(delta, Relaxed).saturating_add(delta);
        PEAK.fetch_max(net, Relaxed);
    }
}

// SAFETY: every method forwards verbatim to `System`; the bookkeeping only
// touches atomics and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds GlobalAlloc's contract; layout as given.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            on_alloc(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            on_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator with this layout.
        unsafe { System.dealloc(ptr, layout) };
        on_free(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` came from this allocator with this layout.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            on_realloc(layout.size(), new_size);
        }
        moved
    }
}

/// Heap activity of one counted call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    /// Highest net heap growth during the call, in bytes.
    pub peak: u64,
    /// Allocation calls (`alloc`, `alloc_zeroed`, `realloc`).
    pub calls: u64,
    /// Bytes requested by those calls.
    pub bytes: u64,
}

/// The counting window. Single-threaded use only (one scan at a time — the
/// worker never counts during throughput runs).
pub mod window {
    use super::{BYTES, CALLS, COUNTING, HeapStats, NET, PEAK, Relaxed, SeqCst};

    /// Reset the counters and start counting.
    pub fn open() {
        NET.store(0, Relaxed);
        PEAK.store(0, Relaxed);
        CALLS.store(0, Relaxed);
        BYTES.store(0, Relaxed);
        COUNTING.store(true, SeqCst);
    }

    /// Stop counting; the call's heap figures so far.
    pub fn pause() -> HeapStats {
        COUNTING.store(false, SeqCst);
        HeapStats {
            peak: u64::try_from(PEAK.load(Relaxed)).unwrap_or(0),
            calls: CALLS.load(Relaxed),
            bytes: BYTES.load(Relaxed),
        }
    }

    pub fn resume() {
        COUNTING.store(true, SeqCst);
    }

    /// Stop counting; the bytes still allocated since [`open`].
    pub fn close() -> i64 {
        COUNTING.store(false, SeqCst);
        i64::try_from(NET.load(Relaxed)).unwrap_or(i64::MAX)
    }
}

/// Run `f` with the counters open.
pub fn counted<T>(f: impl FnOnce() -> T) -> (T, HeapStats) {
    window::open();
    let out = f();
    (out, window::pause())
}

/// SHA-256 (FIPS 180-4) as lowercase hex — the worker identifies its own
/// source with it and links nothing but the scanner.
#[allow(
    clippy::many_single_char_names,
    clippy::unreadable_literal,
    reason = "the FIPS 180-4 names and constants as printed"
)]
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bits = u64::try_from(data.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (&k, &wi) in K.iter().zip(&w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k)
                .wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, add) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(add);
        }
    }
    let mut out = String::with_capacity(64);
    for word in h {
        let _ = write!(out, "{word:08x}");
    }
    out
}

/// sha256 of this file as compiled into the binary: the worker-source
/// identity `hello` reports.
pub fn source_sha256() -> String {
    sha256_hex(include_bytes!("worker.rs"))
}

/// sha256 of the generated `main.rs`, once [`serve`] has it.
static MAIN_SHA256: OnceLock<String> = OnceLock::new();

/// The line the panic hook prints: the location only. A panic message can
/// carry decoded text (a char-boundary slice panic prints the string), and
/// the engine catches panics, so the default hook would print one for
/// every caught engine panic.
pub fn panic_note(location: Option<&std::panic::Location<'_>>) -> String {
    location.map_or_else(
        || String::from("qrscan-bench-worker: panic"),
        |l| {
            format!(
                "qrscan-bench-worker: panic at {}:{}:{}",
                l.file(),
                l.line(),
                l.column()
            )
        },
    )
}

/// Install the location-only panic hook (the generated main calls it).
pub fn quiet_panics() {
    std::panic::set_hook(Box::new(|info| {
        let _ = writeln!(std::io::stderr().lock(), "{}", panic_note(info.location()));
    }));
}

/// The scanner for one benchmark mode, built the way every binding builds
/// it: the preset itself, or the preset's config with the budget removed.
pub fn scanner_for(profile: &str, budget: &str) -> Result<Scanner, String> {
    let preset = match profile {
        "full" => ScanProfile::Full,
        "fast" => ScanProfile::Fast,
        "frame" => ScanProfile::Frame,
        other => return Err(format!("unknown profile {other:?}")),
    };
    let profile = match budget {
        "default" => preset,
        "unbounded" => {
            let mut config = preset.config();
            config.budget_ms = None;
            ScanProfile::Custom(config)
        }
        other => return Err(format!("unknown budget {other:?}")),
    };
    Ok(Scanner::builder().profile(profile).build())
}

/// The serde wire name of a unit variant (`rename_all = "snake_case"`)
/// from its `Debug` name — `QrCode` → `qr_code`, `Ean13` → `ean13`.
pub fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.char_indices() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // writing to a String cannot fail
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn fail(reason: &str) -> String {
    format!("fail\t{}", reason.replace(['\t', '\n', '\r'], " "))
}

fn hello() -> String {
    let versions = Versions::current();
    format!(
        "hello\t{PROTOCOL}\t{}\t{}\t{}\trust-worker\t{}\t{}",
        versions.scanner,
        versions.pipeline,
        versions.score_contract,
        source_sha256(),
        MAIN_SHA256.get().map_or("-", String::as_str)
    )
}

/// The judgment of one report as a comparable signature: `v<composite>`,
/// `w<contract weights that ran>`, then per axis, in report order, the
/// cells passed over the cells planned, `x` when a failing cell is named
/// (`v85w100,5/5,3/5x,…`); `-` when no score came back. A budget that cuts
/// the judgment drops it (this tree) or stops axes short (0.9.0): either
/// way the signature differs from the unbudgeted reference's. The Node and
/// WASM drivers and the CLI parser build the same string from the JSON.
pub fn judgment(report: &ScanReport) -> String {
    report.score.as_ref().map_or_else(
        || String::from("-"),
        |score| {
            let mut out = format!("v{}w{}", score.value, score.weights_run);
            for axis in &score.axes {
                let failed = if axis.failed_at.is_some() { "x" } else { "" };
                let _ = write!(out, ",{}/{}{failed}", axis.passed, axis.total);
            }
            out
        },
    )
}

/// The walk of one report: stages run, Σ `transforms_tried`, and the
/// [`judgment`] signature.
pub fn work_fields(report: &ScanReport) -> String {
    let transforms: u64 = report
        .trace
        .stages
        .iter()
        .map(|s| u64::from(s.transforms_tried))
        .sum();
    format!(
        "{}\t{transforms}\t{}",
        report.trace.stages.len(),
        judgment(report)
    )
}

fn detections_fields(report: &ScanReport) -> String {
    let mut line = report.detections.len().to_string();
    for detection in &report.detections {
        let symbology = snake_case(&format!("{:?}", detection.symbology));
        let _ = write!(
            line,
            "\t{symbology}\t{}",
            hex(detection.content.text.as_bytes())
        );
    }
    line
}

/// One `scan` request: `[profile, budget, memory, path]`.
pub fn scan_request(fields: &[&str]) -> String {
    let [profile, budget, memory, path] = fields else {
        return fail("scan takes profile, budget, memory, path");
    };
    let scanner = match scanner_for(profile, budget) {
        Ok(scanner) => scanner,
        Err(e) => return fail(&e),
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => return fail(&format!("read: {}", e.kind())),
    };
    let counting = *memory == "1";
    if counting {
        window::open();
    }
    let started = Instant::now();
    // An escaping panic costs this row, never the whole run.
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        scanner.scan(ImageInput::encoded(&bytes))
    }));
    let ns = nanos(started);
    let heap = counting.then(window::pause);
    // the reply is built outside the window; the report is then dropped
    // inside it, so `retained` is what the call left allocated
    let (lead, rest) = match &result {
        Ok(Ok(report)) => (
            match heap {
                Some(h) => format!(
                    "ok\t{ns}\t{}\t{}\t{}\t{}\t{}",
                    report.trace.total_ms, report.trace.engine_panics, h.peak, h.calls, h.bytes
                ),
                None => format!(
                    "ok\t{ns}\t{}\t{}\t-\t-\t-",
                    report.trace.total_ms, report.trace.engine_panics
                ),
            },
            Some(format!(
                "{}\t{}",
                work_fields(report),
                detections_fields(report)
            )),
        ),
        Ok(Err(e)) => (format!("error\t{ns}\t{}", e.code()), None),
        Err(_) => (format!("error\t{ns}\tpanic"), None),
    };
    let retained = if counting {
        window::resume();
        drop(result);
        Some(window::close())
    } else {
        None
    };
    match rest {
        Some(rest) => match retained {
            Some(r) => format!("{lead}\t{r}\t{rest}"),
            None => format!("{lead}\t-\t{rest}"),
        },
        None => lead,
    }
}

/// Scan every image once from a bounded queue on `threads` threads;
/// returns (jobs done, wall ns, failed jobs). The clock covers spawning,
/// feeding and draining — the images are already in memory, so the queue
/// bounds pending indices, not memory: in-flight scans equal the thread
/// count.
pub fn run_throughput(
    scanner: &Scanner,
    images: &[Vec<u8>],
    threads: usize,
    bound: usize,
) -> (u64, u64, u64) {
    let (feed, queue) = mpsc::sync_channel::<usize>(bound.max(1));
    let queue = Mutex::new(queue);
    let done = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| {
                loop {
                    let next = match queue.lock() {
                        Ok(queue) => queue.recv(),
                        Err(_) => return,
                    };
                    let Ok(index) = next else { return };
                    let ok = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        scanner.scan(ImageInput::encoded(&images[index])).is_ok()
                    }))
                    .unwrap_or(false);
                    if !ok {
                        failed.fetch_add(1, Relaxed);
                    }
                    done.fetch_add(1, Relaxed);
                }
            });
        }
        for index in 0..images.len() {
            if feed.send(index).is_err() {
                break;
            }
        }
        drop(feed);
    });
    (done.load(Relaxed), nanos(started), failed.load(Relaxed))
}

/// One `throughput` request: `[profile, budget, threads, bound, list]`,
/// `list` a file of image paths, one per line.
pub fn throughput_request(fields: &[&str]) -> String {
    let [profile, budget, threads, bound, list] = fields else {
        return fail("throughput takes profile, budget, threads, bound, list");
    };
    let (Ok(threads), Ok(bound)) = (threads.parse::<usize>(), bound.parse::<usize>()) else {
        return fail("threads and bound must be integers");
    };
    let scanner = match scanner_for(profile, budget) {
        Ok(scanner) => scanner,
        Err(e) => return fail(&e),
    };
    let paths = match std::fs::read_to_string(list) {
        Ok(text) => text,
        Err(e) => return fail(&format!("read the list: {}", e.kind())),
    };
    let mut images = Vec::new();
    for (line, path) in paths.lines().filter(|l| !l.is_empty()).enumerate() {
        match std::fs::read(path) {
            Ok(bytes) => images.push(bytes),
            Err(e) => return fail(&format!("read list line {}: {}", line + 1, e.kind())),
        }
    }
    let (jobs, ns, failed) = run_throughput(&scanner, &images, threads, bound);
    format!("ok\t{jobs}\t{ns}\t{failed}")
}

/// Answer one request line; `None` for `quit`.
pub fn answer(line: &str) -> Option<String> {
    let fields: Vec<&str> = line.split('\t').collect();
    match fields.first().copied() {
        Some("quit") => None,
        Some("hello") => Some(hello()),
        Some("scan") => Some(scan_request(&fields[1..])),
        Some("throughput") => Some(throughput_request(&fields[1..])),
        _ => Some(fail("unknown request")),
    }
}

/// The worker's main loop: stdin requests, stdout replies, until `quit`
/// or end of input — or the moment its parent goes. `main_source` is the
/// generated main, hashed for `hello`.
pub fn serve(main_source: &str) -> std::io::Result<()> {
    exit_with_parent();
    let _ = MAIN_SHA256.set(sha256_hex(main_source.as_bytes()));
    let stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lines() {
        let reply = answer(&line?).unwrap_or_else(|| String::from("bye"));
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
        if reply == "bye" {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counters are process-global: tests that open the window take
    /// turns (nextest isolates them anyway; `cargo test` shares a process).
    static WINDOW: Mutex<()> = Mutex::new(());

    fn qr_png(text: &str) -> Vec<u8> {
        let code = qrcode::QrCode::new(text.as_bytes()).expect("encode");
        let img = code
            .render::<image::Luma<u8>>()
            .module_dimensions(6, 6)
            .build();
        let mut out = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png");
        out
    }

    #[test]
    fn wire_names_follow_serde_snake_case() {
        for (debug, wire) in [
            ("QrCode", "qr_code"),
            ("MicroQrCode", "micro_qr_code"),
            ("RectangularMicroQrCode", "rectangular_micro_qr_code"),
            ("Ean13", "ean13"),
            ("Pdf417", "pdf417"),
            ("UpcA", "upc_a"),
            ("DataBarExpanded", "data_bar_expanded"),
        ] {
            assert_eq!(snake_case(debug), wire);
        }
        assert_eq!(hex(b"A\tz\xff"), "41097aff");
    }

    #[test]
    fn modes_build_the_binding_scanners() {
        for profile in ["full", "fast", "frame"] {
            for budget in ["default", "unbounded"] {
                assert!(scanner_for(profile, budget).is_ok(), "{profile}/{budget}");
            }
        }
        assert!(scanner_for("turbo", "default").is_err());
        assert!(scanner_for("full", "4000").is_err());
    }

    /// The std-only SHA-256 agrees with the `sha2` crate on the padding
    /// boundaries (55, 56, 64 bytes) and a multi-block input.
    #[test]
    fn the_worker_sha256_matches_sha2() {
        for len in [0usize, 3, 55, 56, 63, 64, 65, 1000] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
            assert_eq!(
                sha256_hex(&data),
                crate::external::sha256_bytes(&data),
                "{len} bytes"
            );
        }
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            source_sha256(),
            crate::external::sha256_bytes(include_bytes!("worker.rs"))
        );
    }

    /// The hook prints the location of the panic, never its message.
    #[test]
    fn the_panic_hook_prints_the_location_only() {
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = std::sync::Arc::clone(&seen);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(mut lines) = sink.lock() {
                lines.push(panic_note(info.location()));
            }
        }));
        let caught = std::panic::catch_unwind(|| panic!("secret-payload-text"));
        std::panic::set_hook(previous);
        assert!(caught.is_err());
        let lines = seen.lock().expect("lines").clone();
        assert!(
            lines.iter().any(
                |l| l.starts_with("qrscan-bench-worker: panic at ") && l.contains("worker.rs:")
            ),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains("secret-payload-text")),
            "{lines:?}"
        );
        assert_eq!(panic_note(None), "qrscan-bench-worker: panic");
    }

    /// A real scan through the protocol: the reply carries timing, the
    /// work facts, the detection as (wire symbology, hex text) and no heap
    /// fields unless asked. The counters only move inside the global
    /// allocator, which is not installed in this test binary, so memory
    /// reads zero here.
    #[test]
    fn scan_requests_reply_on_the_wire() {
        let _window = WINDOW.lock();
        let dir = std::env::temp_dir().join(format!("qrscan-bench-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("hello.png");
        std::fs::write(&path, qr_png("bench")).expect("write");
        let path = path.to_str().expect("utf-8 temp path");

        let reply = answer(&format!("scan\tfull\tunbounded\t0\t{path}")).expect("reply");
        let fields: Vec<&str> = reply.split('\t').collect();
        assert_eq!(fields[0], "ok", "{reply}");
        assert!(fields[1].parse::<u64>().is_ok_and(|ns| ns > 0));
        assert!(fields[2].parse::<f64>().is_ok());
        assert_eq!(
            fields[4..8].to_vec(),
            vec!["-", "-", "-", "-"],
            "no heap, no retained"
        );
        assert!(
            fields[8].parse::<u32>().is_ok_and(|stages| stages >= 1),
            "{reply}"
        );
        assert!(fields[9].parse::<u64>().is_ok_and(|t| t >= 1), "{reply}");
        // full scores: the composite, the weights that ran, one entry per
        // axis, each a complete ramp of an unbudgeted call
        let judgment = fields[10];
        assert!(
            judgment.starts_with('v') && judgment.contains('w'),
            "{reply}"
        );
        let axes: Vec<&str> = judgment.split(',').skip(1).collect();
        assert!(!axes.is_empty(), "{reply}");
        assert!(
            axes.iter().all(|a| a
                .trim_end_matches('x')
                .split_once('/')
                .is_some_and(|(p, t)| p.parse::<u8>().is_ok() && t.parse::<u8>().is_ok())),
            "{reply}"
        );
        let text = hex(b"bench");
        assert_eq!(fields[11..].to_vec(), vec!["1", "qr_code", text.as_str()]);

        let frame = answer(&format!("scan\tframe\tdefault\t1\t{path}")).expect("reply");
        let fields: Vec<&str> = frame.split('\t').collect();
        assert_eq!(fields[0], "ok");
        assert!(
            fields[4..7].iter().all(|f| f.parse::<u64>().is_ok()),
            "{frame}"
        );
        assert!(fields[7].parse::<i64>().is_ok(), "retained: {frame}");
        assert_eq!(fields[10], "-", "frame never scores");
        assert_eq!(
            answer(&format!("scan\tfull\tunbounded\t0\t{path}"))
                .map(|r| r.split('\t').nth(10).map(str::to_owned)),
            Some(Some(judgment.to_owned())),
            "an unbudgeted walk judges alike every time"
        );

        let missing = answer("scan\tfull\tdefault\t0\t/nonexistent/x.png").expect("reply");
        assert_eq!(
            missing, "fail\tread: entity not found",
            "no path in a reply"
        );
        assert!(answer("scan\tfull").expect("reply").starts_with("fail\t"));
        assert!(answer("bogus").expect("reply").starts_with("fail\t"));
        assert_eq!(answer("quit"), None);
        let hello = answer("hello").expect("reply");
        let fields: Vec<&str> = hello.split('\t').collect();
        assert_eq!(fields[..2].to_vec(), vec!["hello", PROTOCOL]);
        assert_eq!(fields[6], source_sha256());

        let list = dir.join("list.txt");
        std::fs::write(&list, format!("{path}\n{path}\n{path}\n")).expect("list");
        let list = list.to_str().expect("utf-8 temp path");
        let reply = answer(&format!("throughput\tframe\tdefault\t2\t4\t{list}")).expect("reply");
        let fields: Vec<&str> = reply.split('\t').collect();
        assert_eq!(
            (fields[0], fields[1], fields[3]),
            ("ok", "3", "0"),
            "{reply}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The helper side of the orphan test: a worker that keeps its parent.
    #[test]
    #[ignore = "a helper process of a_worker_leaves_when_its_parent_does"]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn orphan_helper() {
        if crate::bench::proc::tests::helper_role().as_deref() != Some("orphan") {
            return;
        }
        exit_with_parent();
        println!("worker {}", std::process::id());
        std::thread::sleep(Duration::from_secs(30));
    }

    /// With a fake child: a worker whose parent goes away — a shell that
    /// started it, then exits — leaves within about [`PARENT_POLL`] while
    /// it sleeps through a long call.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn a_worker_leaves_when_its_parent_does() {
        use crate::bench::proc::tests::{gone_within, read_pid};
        let exe = std::env::current_exe().expect("test binary");
        let mut shell = std::process::Command::new("sh")
            .args([
                "-c",
                "\"$0\" bench::worker::tests::orphan_helper --exact --ignored --nocapture & sleep 0.5",
                exe.to_str().expect("utf-8 path"),
            ])
            .env("QRSCAN_BENCH_TEST_HELPER", "orphan")
            .env_remove(PARENT_ENV)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("shell");
        let worker = read_pid(shell.stdout.take().expect("stdout"), "worker ");
        assert!(shell.wait().expect("shell exits").success());
        assert!(
            gone_within(worker, Duration::from_secs(1)),
            "the orphaned worker left"
        );
    }

    /// The bookkeeping, driven directly: net growth peaks at 150 bytes
    /// over three calls requesting 180 bytes; nothing moves while paused;
    /// resumed for the report's drop, the window closes on the 30 bytes the
    /// call left allocated.
    #[test]
    fn heap_counters_track_net_peak_calls_bytes_and_retained() {
        let _window = WINDOW.lock();
        window::open();
        on_alloc(100);
        on_alloc(50);
        on_free(100);
        on_realloc(50, 30);
        let stats = window::pause();
        assert_eq!(
            stats,
            HeapStats {
                peak: 150,
                calls: 3,
                bytes: 180
            }
        );
        on_alloc(1 << 20); // paused: the reply is built here, uncounted
        window::resume();
        on_free(30);
        on_alloc(30);
        assert_eq!(window::close(), 30);
        // closed window: nothing moves
        on_alloc(1 << 20);
        let ((), idle) = counted(|| ());
        assert_eq!(
            idle,
            HeapStats {
                peak: 0,
                calls: 0,
                bytes: 0
            }
        );
    }
}
