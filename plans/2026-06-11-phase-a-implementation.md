# Phase A — Core Rebuild Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan
> task-by-task. TDD per task (superpowers:test-driven-development). Design rationale lives in
> [`2026-06-11-v03-rebuild-design.md`](2026-06-11-v03-rebuild-design.md) — read it first; this
> plan does not repeat the WHY.

**Goal:** Rebuild `qrcode-ai-scanner` core as a Diamond-grade pure-Rust crate: deterministic
multi-engine decode ladder + score contract v3 (stress ramps, survival curves, structural caps,
synthetic UEC) + typed payloads, with corpus-driven verification.

**Architecture:** Single core crate (`crates/qrcode-ai-scanner/`), sync by design, wasm-compatible
from day 1 (rayon feature-gated, `web-time` clock). Engines isolated behind `catch_unwind`.
No-QR-found = `Ok` with empty detections. All pub enums `#[non_exhaustive]`.

**Tech Stack:** Rust edition 2024 (MSRV 1.87) · rxing ≥0.9.1 (QR-only features) · rqrr ≥0.10.1
(raw `decode_to` path) · image 0.25 · thiserror 2 + miette 7 · encoding_rs · web-time 1.1 ·
serde · dev: qrcode, insta, proptest, criterion, pretty_assertions · cargo-nextest runner.

**Conventions (every task):** 0 `unwrap`/`expect` in `src/` (workspace lints deny) · ≤1500
LOC/file · ≤100 LOC/fn · tests may unwrap · commit format `type(scope): lowercase` +
`Co-Authored-By: Nika 🦋 <nika@supernovae.studio>` · run `cargo fmt && cargo clippy --all-targets
&& cargo nextest run` (fallback `cargo test`) before every commit.

---

## Task 1: Workspace reset

**Files:**
- Rewrite: `Cargo.toml` (workspace root)
- Create: `crates/qrcode-ai-scanner/Cargo.toml`, `crates/qrcode-ai-scanner/src/lib.rs` (+ empty
  module files: `input.rs`, `error.rs`, `report.rs`, `payload.rs`, `engine/mod.rs`, `ladder.rs`,
  `transform.rs`, `score/mod.rs`)
- Delete: `crates/qrcode-ai-scanner-core/`, `crates/qrcode-ai-scanner-cli/`,
  `crates/qrcode-ai-scanner-node/`, `scripts/`, `examples/gen_test_qr.rs`, `docs/benchmark-*`,
  `docs/PREPROCESSING_ANALYSIS.md` (git is the archive — NUKE, no shims)
- Keep: `test-images/` (seed fixtures, reorganized in Task 10), `docs/plans/`, `LICENSE`,
  `.github/workflows/` (rewritten in Task 10)

**Step 1 — verify upstream facts (phantom-feature-recheck discipline):**
```bash
rustc --version                                  # expect ≥1.87
curl -s https://raw.githubusercontent.com/rxing-core/rxing/v0.9.1/Cargo.toml | grep -A30 '\[features\]'
cargo info image | head -5 ; cargo info qrcode | head -5 ; cargo info miette | head -5
```
Record EXACT rxing feature names for a QR-only build (expected family: `default-features = false`
+ `image`, `encoding_rs`, decoder/QR features per published list — do NOT guess; paste what the
Cargo.toml says). Verify `qrcode` crate API: can it force version + EC level? (mask forcing not
required — Task 9 applies masks itself).

**Step 2 — root `Cargo.toml`:**
```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.3.0-alpha.0"
edition = "2024"
rust-version = "1.87"
license = "AGPL-3.0-or-later"
repository = "https://github.com/supernovae-st/qrcode-ai-scanner"
authors = ["Thibaut MÉLEN <thibaut@supernovae.studio>", "SuperNovae <contact@supernovae.studio>"]

[workspace.dependencies]
# engines (pin exact features from Step 1 verification)
rxing = { version = "0.9.1", default-features = false, features = [/* verified QR-only set */] }
rqrr = "0.10.1"
# imaging
image = { version = "0.25", default-features = false, features = ["png", "jpeg", "webp", "gif"] }
# text / data
encoding_rs = "0.8"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
# errors
thiserror = "2"
miette = "7"
# time / parallel
web-time = "1.1"
rayon = "1.10"
# dev
qrcode = "0.14"
insta = { version = "1", features = ["json"] }
proptest = "1"
criterion = { version = "0.8", features = ["html_reports"] }
pretty_assertions = "1"

[workspace.lints.rust]
missing_docs = "warn"
[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
pedantic = { level = "warn", priority = -1 }

[profile.release]
lto = "thin"
codegen-units = 1
strip = true
```

**Step 3 — core crate manifest** (`crates/qrcode-ai-scanner/Cargo.toml`): name
`qrcode-ai-scanner`, workspace inheritance, features
`default = ["engine-rxing", "engine-rqrr", "parallel", "serde"]`,
`engine-rxing = ["dep:rxing"]`, `engine-rqrr = ["dep:rqrr"]`, `parallel = ["dep:rayon"]`,
`serde = ["dep:serde", "dep:serde_json"]`, `tracing = []` (reserved), `[lints] workspace = true`.
`lib.rs`: crate docs + `mod` declarations + `compile_error!` if no engine feature.

**Step 4 — verify:** `cargo check -p qrcode-ai-scanner` (all default features) AND
`cargo check -p qrcode-ai-scanner --no-default-features --features engine-rqrr,serde` (wasm-shape
build) both pass. `cargo fmt --check`.

**Step 5 — commit:** `feat(workspace): v0.3 reset — edition 2024 · lints deny unwrap · engines feature-gated`

## Task 2: Error model (`error.rs`)

**Files:** Test+impl in `crates/qrcode-ai-scanner/src/error.rs` (inline `#[cfg(test)]`).

**Step 1 — failing tests** (write first, `cargo nextest run -p qrcode-ai-scanner errors` → RED):
codes unique across variants · wire parity `err.code() == "QRS-002"` and miette
`Diagnostic::code()` renders the same string · `is_transient()` false for all current variants ·
Display messages contain the structured fields.

**Step 2 — implementation:**
```rust
//! Scan errors — QRS-xxx registry. Codes are never reused.
use miette::Diagnostic;
use thiserror::Error;

/// Errors for real faults only — "no QR found" is NOT an error (empty detections).
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
pub enum ScanError {
    /// QRS-001 — bytes are not a decodable image.
    #[error("unsupported or corrupt image: {details}")]
    #[diagnostic(code("QRS-001"), help("supported: PNG, JPEG, WebP, GIF"))]
    InvalidImage { details: String },
    /// QRS-002 — dimensions exceed configured limits.
    #[error("image {width}x{height} exceeds limit {max_dimension}")]
    #[diagnostic(code("QRS-002"), help("downscale the image or raise Limits::max_dimension"))]
    DimensionsExceeded { width: u32, height: u32, max_dimension: u32 },
    /// QRS-003 — width*height overflows or exceeds max_pixels.
    #[error("pixel count for {width}x{height} exceeds limit {max_pixels}")]
    #[diagnostic(code("QRS-003"))]
    PixelOverflow { width: u32, height: u32, max_pixels: u64 },
    /// QRS-004 — raw buffer length does not match dimensions.
    #[error("buffer length {got} does not match expected {expected}")]
    #[diagnostic(code("QRS-004"), help("rgba8 expects w*h*4 bytes; luma8 expects w*h"))]
    BufferMismatch { got: usize, expected: usize },
    /// QRS-005 — cooperative cancellation honoured.
    #[error("scan cancelled")]
    #[diagnostic(code("QRS-005"))]
    Cancelled,
}

impl ScanError {
    /// Stable wire code (`"QRS-001"`). Matches the miette diagnostic code.
    pub fn code(&self) -> &'static str { /* match */ }
    /// Whether retrying may succeed without input change. All current variants: false.
    pub fn is_transient(&self) -> bool { false }
}
pub type Result<T> = std::result::Result<T, ScanError>;
```

**Steps 3-5:** GREEN → fmt/clippy → commit `feat(error): scan error model — qrs codes + miette diagnostics + pinning tests`.

## Task 3: Input layer (`input.rs`)

**Files:** `crates/qrcode-ai-scanner/src/input.rs` (+ exif dep decision).

**API:** `ImageInput<'a>::{encoded(&[u8]), rgba8(&[u8], w, h), luma8(&[u8], w, h)}` ·
`Limits { max_dimension: u32 (default 10_000), max_pixels: u64 (default 64MP) }` ·
`pub(crate) fn decode_to_luma(input, &Limits) -> Result<LumaImage>` where `LumaImage` is the
internal owned `{ data: Vec<u8>, w, h }`.

**TDD cases (write failing first):** encoded garbage → `QRS-001` · oversize dims → `QRS-002`
(both encoded + raw paths) · `w*h` overflow → `QRS-003` · rgba8 wrong len → `QRS-004` (exact
`expected` value asserted) · BT.601 luma known-values (pure red 255,0,0 → 76; white → 255;
black → 0) · luma8 passthrough zero-copy-equivalent · EXIF: a JPEG with orientation 6 decodes
rotated (generate fixture in test via `image` re-encode; verify with crate chosen — check
`kamadak-exif` (published crate name) at task time; if EXIF handling needs >1 small dep or >150
LOC, record an open item and ship orientation 1 only with `trace` note — do not gold-plate).

**Commit:** `feat(input): image input layer — limits · bt601 luma · structured rejections`

## Task 4: Report types (`report.rs`)

**Files:** `crates/qrcode-ai-scanner/src/report.rs`, snapshot tests
`crates/qrcode-ai-scanner/tests/report_schema.rs` (insta).

Types per design §5: `ScanReport { detections, score: Option<Score>, hints, trace, versions }` ·
`Detection { content: DecodedContent, payload: Payload (Task 6 stub = Text), corners:
Option<[Point; 4]>, meta: QrMeta, engine: EngineKind }` · `DecodedContent { text: String, raw:
Vec<u8>, charset: Charset }` · `QrMeta { version: u8, ec_level: EcLevel, mask: Option<u8>,
modules: u8, mirrored: bool, inverted: bool }` · `PipelineTrace { stages: Vec<StageTrace>,
engine_panics: u8, total_ms: f64 }` · `Versions { scanner: &'static str (CARGO_PKG_VERSION),
pipeline: u8 = 1, score_contract: u8 = 3 }` · `Point { x: f32, y: f32 }` · enums `EngineKind`,
`EcLevel`, `Charset`, `Hint` — ALL pub enums `#[non_exhaustive]`, serde `snake_case`.

**TDD:** insta JSON snapshot of a fully-populated `ScanReport` (THE schema contract — review the
snapshot by hand before accepting) + a minimal empty report snapshot + serde roundtrip test.

**Commit:** `feat(report): scan report types — versioned serde schema + insta contract`

## Task 5: Engine layer (`engine/`)

**Files:** `engine/mod.rs` (EngineKind dispatch + catch_unwind + `RawDetection`),
`engine/rxing.rs`, `engine/rqrr.rs`, `engine/charset.rs`.

**Contract:** `pub(crate) fn decode_all(luma: &LumaImage, opts: &EngineOpts) ->
Vec<RawDetection>` per engine; panics caught (`std::panic::catch_unwind(AssertUnwindSafe(..))`)
→ counted, never propagated. `RawDetection { raw: Vec<u8>, text_lossy: String, corners:
Option<[Point;4]>, meta partial, engine }`.

- **rxing:** luma → `detect_multiple_in_luma`-equivalent low-level call WITH
  `DecodeHints { TryHarder: true, AlsoInverted: true, PureBarcode: false }` (verify exact 0.9.1
  hint API at task time — old code at `decoder.rs:75` used `rxing::helpers::detect_multiple_in_luma`
  and STUBBED version/EC extraction; v0.3 must extract real `version`, `ec_level`, points,
  `is_mirrored`/`is_inverted` metadata from `RXingResult`).
- **rqrr:** `PreparedImage::prepare` → grids → `grid.bounds` → corners; `decode_to(&mut Vec<u8>)`
  raw bytes (NOT `decode()` — forced-UTF-8/ECI-discard trap) + `MetaData { version, ecc_level,
  mask }`.
- **charset.rs:** `resolve(raw: &[u8]) -> (String, Charset)`: strict UTF-8 → else Shift-JIS via
  encoding_rs sniff → else windows-1252. Unit tests with fixed byte vectors of each.

**TDD parity tests:** generate with `qrcode` crate (byte-mode UTF-8 "héllo→🦋", numeric, and a
windows-1252 byte payload) → both engines return identical `raw` bytes; corners present from
rqrr; rxing meta populated (version/EC assert exact, not just is_some).

**Commit:** `feat(engine): rxing + rqrr isolated wrappers — raw-bytes parity · charset resolution · panic guards`

## Task 6: Typed payloads (`payload.rs`)

`Payload` enum (`#[non_exhaustive]`, serde tagged `kind`): `Url { url }`, `Wifi { ssid, security,
password, hidden }`, `Email { to, subject, body }` (mailto: + MATMSG:), `Sms { number, body }`,
`Tel { number }`, `Geo { lat, lon }`, `VCard { raw }` (no deep parse — YAGNI), `VEvent { raw }`,
`Text`. `pub(crate) fn classify(text: &str) -> Payload`.

**TDD:** one test per format with real-world strings (incl. `WIFI:T:WPA;S:my ssid;P:p\;ss;H:true;;`
escaped-semicolon case) · proptest: `classify` never panics on arbitrary strings.

**Commit:** `feat(payload): typed payload classification — url wifi email sms tel geo vcard vevent`

## Task 7: Transforms + deterministic ladder (`transform.rs`, `ladder.rs`, `lib.rs` Scanner)

**7a transforms (TDD per-op):** `otsu_threshold`, `invert`, `contrast_stretch`, `channel(R|G|B)`
(needs rgb retained: `decode_to_luma` gains a `keep_rgb` mode for the ladder), `downscale_to(max_side)`
(triangle filter). Pure functions `LumaImage -> LumaImage`, deterministic, tested with tiny
synthetic matrices (assert exact pixel values, not is_empty).

**7b ladder + Scanner:**
- `Budget` (web-time `Instant`, `ms_remaining()`), `CancelToken` (Arc<AtomicBool> +
  `cancelled()`), checked between every engine attempt → `QRS-005`.
- Stage plan per design §6: S0 normalize → S1 pyramid (≤512px attempt first) → S2 direct both
  engines → S3 enhance set (otsu, invert, contrast, channels — in FIXED declared order) → S4
  curated grid (start: {otsu,invert}×{512,800,full}×{contrast on/off} ≈ 12 combos — tune via
  corpus in Task 10, order is DATA `const LADDER: &[Step]`).
- Early-exit on first detection (Full profile completes current stage for consensus count).
- `ScanProfile::{Full, Fast, Frame, Custom(ScanConfig)}` → which stages + budget defaults
  (Full 4000ms · Fast 800ms · Frame 80ms).
- `Scanner::builder().profile().limits().build()` · `scan()` / `scan_cancellable()` ·
  `scan_batch(&[ImageInput]) -> Vec<Result<ScanReport>>` (rayon under `parallel`, sequential
  otherwise — identical results property test).
- Trace: per-stage `StageTrace { stage, transforms_tried, ms, detections_found }`.

**TDD:** determinism (same input scanned twice → byte-identical serde JSON) · cancellation honored
(pre-cancelled token → `QRS-005` fast) · budget respected (Frame profile on a hard image returns
`Ok` empty within ~2× budget) · integration on the 3 legacy `test-images/` (clean decodes at S1/S2
in <200ms; artistic decodes by S4; degraded → `Ok` empty) · `scan_batch` parity sequential==parallel.

**Commits:** `feat(transform): deterministic preprocessing ops` then
`feat(ladder): deterministic decode ladder — pyramid · budget · cancel · scanner api`

## Task 8: Score v3 — ramps + survival + structural (`score/`)

**8a geometry+lighting transforms** (`score/warp.rs`): own 3×3 homography + bilinear sampler
(`warp_perspective(luma, h_matrix)` — REUSED by Task 9 grid sampling; test: identity homography
== input, known 90° rotation matrix maps corners exactly) · `shadow_gradient(strength)`,
`glare_blob(cx, cy, r)`, `exposure(delta)`.

**8b ramps + survival** (`score/stress.rs`, `score/survival.rs`): axes per design §7 — resolution
(downscale steps to px/module floor), blur σ ramp, contrast ramp, perspective tilt 15/30/45° two
axes, rotation {10,25,40}°, lighting set. Each axis = ordered intensities → decode each (Fast
ladder subset) → survival index = first failure → axis score = AUC. Composite = weighted mean
(weights in `const SCORE_WEIGHTS_V3`, documented in SCORING.md).

**8c structural caps + hints** (`score/structural.rs`): finder integrity — sample the 3 finder
regions via detection corners homography, 1:1:3:1:1 run-length tolerance check, returns
0.0-1.0 per corner; quiet zone — sample 4-module border ring uniformity. Composite capped:
`finder < 0.5 ⇒ score ≤ 40`, `quiet_zone violated ⇒ score ≤ 60` (constants in contract).
Hints from failures: axis-fail → `EnlargeModules`/`IncreaseContrast`/…, structural →
`FixFinderPattern{corner}`/`RestoreQuietZone`, `meta.ec_level < H && artistic-class ⇒
RaiseErrorCorrection{current}`.

**TDD:** clean generated QR → score ≥85, grade table coherent · monotonicity: synthetically
blurred copy scores strictly less · finder-damaged copy (paint over one finder in test) → capped
≤40 + `FixFinderPattern` hint present · profile wiring (Frame → `score: None`).

**Commits:** `feat(score): warp + lighting stress transforms` ·
`feat(score): survival-curve scoring v3 — ramps · structural caps · hints`

## Task 9: Synthetic UEC (`score/uec.rs`)

**Step 0 — route check:** does rxing 0.9.1 expose corrected codewords (`get_raw_bytes()` on the
QR path = post-correction codewords)? Check docs.rs + source. Route A (preferred): observed
codewords (grid-sampled) vs rxing corrected codewords. Route B (fallback): re-encode decoded
payload at same version/EC with own RS encoder (GF(256) log/antilog tables + RS remainder,
~150 LOC, exhaustively tested vs `qrcode` crate output on byte-mode vectors).

**Pipeline:** detection corners → homography (Task 8a warp, inverse-sample module centers with
Gaussian 3×3 center weighting) → observed module matrix → remove function patterns + unmask
(8 mask formulas, `mask` from rqrr MetaData) → de-interleave codewords (per-version block tables
`const RS_BLOCKS: [[BlockSpec; 4]; 40]` — transcribe from ISO 18004 table, property-test:
total codewords per version+EC == known capacity constants from `qrcode` crate) → diff vs
corrected codewords per block → `e=0, t=mismatches` → `UEC = 1 − 2t/d` worst block → grade
A ≥0.62 · B ≥0.50 · C ≥0.37 · D ≥0.25 · F.

**TDD:** pristine generated QR (each EC level L/M/Q/H) → UEC = 1.0 grade A · flip k modules
(k < capacity/2) in the image → t == k recovered exactly on aligned grid → UEC decreases by
2k/d · flips beyond capacity → decode fails upstream (test documents the boundary) · grid
misalignment guard: UEC computed only when finder integrity ≥ threshold (else `None` + trace
note — never report garbage margin).

**Commit:** `feat(score): synthetic uec — iso 15415 margin from grid diff (flagship)`

## Task 10: Corpus + xtask + CI + CLI + docs

- `fixtures/` reorg: `clean/` (generated matrix: versions {2,5,10} × EC {L,M,Q,H} via dev
  generator bin) · `artistic/` (legacy image + site dump when Nicolas provides — open item) ·
  `degraded/` (deterministic transforms of clean set, generated INTO the repo once, committed) ·
  `corpus.toml` manifest (path, expected_text, category, source).
- `xtask/` crate: `corpus-report` (runs Scanner on corpus → success rates per category → rewrites
  README managed block `<!-- corpus-report:begin/end -->`) · `baseline` (installs
  `qrcode-ai-scanner-cli@0.2.2` from crates.io, runs on same corpus → `docs/baseline-v02.json`
  — the Phase A exit gate comparator).
- `fuzz/` cargo-fuzz: `fuzz_scan_encoded` (arbitrary bytes → scan never panics), seeded from
  fixtures.
- Benches: `benches/decode.rs` criterion (clean/artistic per profile) + iai-callgrind variant.
- CI `.github/workflows/ci.yml`: fmt → clippy → nextest (ubuntu/macos/windows) → wasm-shape
  check (`--no-default-features --features engine-rqrr,engine-rxing,serde` on
  `--target wasm32-unknown-unknown`) → cargo-deny (licenses+advisories) → corpus-report job
  (artifact). `mutants.yml` weekly (`cargo mutants -p qrcode-ai-scanner --minimum-pass 90`).
- CLI crate `qrcode-ai-scanner-cli` rebuild: bin `qrscan`, JSON default, `--pretty`,
  `--score-only`, `--profile`, exit codes 0/1/2.
- Docs: `ARCHITECTURE.md` (ladder + invariants), `SCORING.md` (contract v3 + weights + caps),
  README rewrite (consumer-first + managed corpus block), delete `SPEC.md`.

**Exit gate (Phase A done):** corpus artistic success ≥ baseline-v02 · all CI green · mutants
≥90% on core · fuzz 10min clean · clippy zero warnings.

**Commits:** one per bullet, `feat(corpus)`, `feat(xtask)`, `feat(fuzz)`, `ci:`, `feat(cli)`, `docs:`.

---

## Execution notes

- Tasks are strictly ordered (each builds on prior types). One commit minimum per task.
- Any upstream-API surprise (rxing hints shape, qrcode crate capabilities): STOP, re-verify
  against the crate source (phantom-feature-recheck), adapt the plan in-place, note in commit body.
- Push to `origin main` at natural checkpoints (end of Task 2, 5, 7, 9, 10).
