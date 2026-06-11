# QR Code AI Scanner — v0.3 Rebuild Design

> **Status**: validated (Thibaut · 2026-06-11) · research-hardened · supersedes `SPEC.md` (v0.2.2 era)
> **Method**: collaborative brainstorm + 3-agent research sweep (Rust QR engines · ISO 15415 scoring
> standards & AI-QR literature · wasm/napi performance SOTA) — key findings cited in §14.
> **Consumers**: (a) QR Code AI site — in-process, no API roundtrip (Nuxt server via napi · browser
> via wasm) · (b) future Nika vision builtin (`invoke: qr.scan`) · (c) CLI dogfood.

---

## 1. Mission

The reference library for decoding and validating **artistic / AI-generated QR codes** — the codes
that break standard scanners. Decode + scannability scoring with a real error-correction margin,
deterministic, sovereign (pure Rust, zero cloud), shipped to every runtime the product needs:
native Node, browser wasm, CLI, and as a clean crate for Nika workflows.

## 2. Goals / Non-goals

**Goals**
- Decode artistic QR at the best pure-Rust rate achievable in 2026 (multi-engine ladder).
- Score scannability 0-100 with ISO-15415-inspired sub-grades incl. **synthetic UEC** (§7).
- Deterministic: same bytes + same config + same versions ⇒ same report, bit-for-bit.
- Per-frame camera decode ≤35 ms desktop / ≤80 ms mid-mobile @720p (wasm SIMD128).
- Never block a Node event loop (napi AsyncTask on libuv pool + cooperative cancel).
- Diamond-grade code: 0 unwrap/expect in src (structural via lints), `#[non_exhaustive]`,
  ≤1500 LOC/file, ≤100 LOC/fn, TDD, mutants ≥90%, fuzz.

**Non-goals**
- ISO-certified *verification* (needs calibrated optics per ISO 15426-2). We ship **validation**
  and say so explicitly — reflectance-class grades are relative, geometry/UEC grades transfer.
- Symbologies beyond the QR family (v0.3; Micro QR/rMQR arrive free via rxing, flagged).
- QR generation (the site generator owns that; we validate).
- wasm threads (nightly + COOP/COEP — single-thread + SIMD suffices, 2026 consensus).
- Native BarcodeDetector dependency (unreliable matrix in 2026: no Windows/Linux desktop Chrome,
  Safari flag-only, Firefox "defer" — and verdict consistency with the server scorer matters).

## 3. Nomenclature — one base, one suffix per target

| Surface | Name |
|---|---|
| Repo / base | `qrcode-ai-scanner` |
| Core crate (lib) | `qrcode-ai-scanner` (bare name = THE crate · free on crates.io, verified 2026-06-11) |
| Bindings crates | `qrcode-ai-scanner-{node,wasm,cli}` |
| npm (server) | `@supernovae-st/qrcode-ai-scanner` (exists at 0.2.2 · upgraded in place) |
| npm (browser) | `@supernovae-st/qrcode-ai-scanner-wasm` |
| CLI binary | `qrscan` (only tolerated abbreviation) |
| Error codes | `QRS-0xx` (never reused · registry + pinning tests) |
| Score contract | `score_contract_version` integer (v3 ships here) |

Old crates.io names (`-core`, `-cli` @0.2.2) stay published as-is — pre-1.0, no external users,
no deprecation ceremony (NUKE-LEGACY).

## 4. Workspace layout

```
qrcode-ai-scanner/
├── Cargo.toml                      # workspace · edition 2024 · MSRV pinned (≥1.87: wasm
│                                   # bulk-memory/nontrapping default) · [workspace.lints]
├── crates/
│   ├── qrcode-ai-scanner/          # CORE · pure · sync by design · zero file I/O
│   │   └── src/
│   │       ├── lib.rs              # Scanner · ScannerBuilder · prelude-free minimal surface
│   │       ├── input.rs            # ImageInput: Encoded(&[u8]) | Rgba8{buf,w,h} | Luma8{..}
│   │       ├── engine/             # EngineKind enum dispatch · catch_unwind isolation
│   │       │   ├── rxing.rs        #   primary · TryHarder + AlsoInverted · QR-only features
│   │       │   ├── rqrr.rs         #   second family · decode_to RAW path (charset! §6) · bounds
│   │       │   └── zedbar.rs       #   optional `engine-zedbar` (3rd family · verify first)
│   │       ├── ladder.rs           # deterministic decode ladder (§6) · Budget · CancelToken
│   │       ├── transform.rs        # preprocessing ops (Otsu, invert, contrast, channels, …)
│   │       ├── score/              # contract v3 (§7)
│   │       │   ├── stress.rs       #   axes: resolution·blur·contrast·perspective·rotation·lighting
│   │       │   ├── uec.rs          #   synthetic Unused Error Correction (flagship)
│   │       │   ├── structural.rs   #   finder integrity · quiet zone
│   │       │   └── survival.rs     #   ramp-to-failure curves · AUC composite
│   │       ├── payload.rs          # typed payloads: Url|Wifi|VCard|Sms|Email|Geo|Event|Text
│   │       ├── report.rs           # ScanReport · Detection · Score · Hint · PipelineTrace · Versions
│   │       └── error.rs            # ScanError (thiserror 2 + miette) · QRS codes module
│   ├── qrcode-ai-scanner-node/     # napi 3 · AsyncTask(libuv) · AbortSignal · zero-copy Buffer
│   ├── qrcode-ai-scanner-wasm/     # wasm-bindgen ESM · SIMD128-only · panic-free
│   └── qrcode-ai-scanner-cli/      # clap · bin qrscan · exit codes (0 found · 1 none · 2 invalid)
├── fixtures/ + corpus.toml         # ground-truth manifest: path · expected · category · source
├── fuzz/                           # cargo-fuzz: decode entry (seeded from fixtures)
├── benches/                        # criterion (local) + iai-callgrind suite (CI gate, linux)
├── examples/web-camera/            # vite · rVFC → worker → wasm · ROI · corners overlay
├── xtask/                          # corpus-report (README managed block) · size-report
└── docs/ ARCHITECTURE.md · SCORING.md (the v3 contract, human-readable)
```

**Core features**:

```toml
default = ["engine-rxing", "engine-rqrr", "parallel", "serde"]
engine-rxing / engine-rqrr / engine-zedbar   # ≥1 engine required (compile_error! otherwise)
parallel                                      # rayon · native only · OFF in wasm
serde                                         # ScanReport (de)serialization — bindings + Nika
tracing                                       # per-stage spans · zero-dep without the feature

[workspace.lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
```

Engine pins: `rxing ≥0.9.1` with granular QR-only features (0.9.0 feature refactor — lean builds,
smaller wasm), `rqrr ≥0.10.1` (mirrored-QR support since 0.10.0), `zedbar 0.4.x` optional,
`web-time 1.1` for Instant on wasm. `zxing-cpp` is **never a dependency** — dev-only oracle (§11).

## 5. Core API

```rust
let scanner = Scanner::builder()          // Send + Sync · Arc internals · reusable · scratch buffers
    .profile(ScanProfile::Full)           // Full | Fast | Frame | Custom(ScanConfig)
    .limits(Limits::default())            // max_dimension · max_pixels (anti-DoS, configurable)
    .build();

let report = scanner.scan(ImageInput::encoded(&bytes))?;            // PNG/JPEG/WebP/GIF
let report = scanner.scan(ImageInput::rgba8(&frame, w, h))?;        // camera frame · no PNG roundtrip
let report = scanner.scan_cancellable(input, &cancel)?;             // cooperative CancelToken
let report = scanner.scan_frame_hinted(input, Some(prev_region))?;  // ROI tracking (§10)
let best   = scanner.scan_batch(&inputs)?;                          // best-of-N (generator gate) · rayon

pub struct ScanReport {
    pub detections: Vec<Detection>,    // empty = nothing found — NOT an error · multi-QR future-proof
    pub score: Option<Score>,          // None in Frame profile
    pub hints: Vec<Hint>,              // machine-actionable (§7) — the agent/generator feedback loop
    pub trace: PipelineTrace,          // stages tried · transforms · timings · engine_panics
    pub versions: Versions,            // scanner / pipeline / score_contract — cache keys
}

pub struct Detection {
    pub content: DecodedContent,       // text + raw bytes + resolved charset (§6)
    pub payload: Payload,              // typed: Url|Wifi|VCard|Sms|Email|Geo|Event|Text
    pub corners: Option<[Point; 4]>,   // rqrr bounds native; extrapolated from rxing finder centers
    pub meta: QrMeta,                  // version 1-40 · EC level · mask · modules · mirrored/inverted
    pub engine: EngineKind,
}
```

**Semantics that matter**
- *No QR found is `Ok`*, never `Err` — on a camera stream 99% of frames have no code; `Err` is
  reserved for real faults (corrupt image, invalid buffer, cancellation).
- Core is **sync by design** — async is a binding concern (napi AsyncTask / caller executors).
- Engine calls wrapped in `catch_unwind`: a panicking engine = failed attempt recorded in trace,
  the ladder continues (in wasm an uncaught panic is a fatal trap).
- All pub enums `#[non_exhaustive]`. Report serde schema = stable versioned contract (snake_case).

## 6. Decode pipeline — deterministic ladder

Replaces v0.2's 4 tiers + **256 random brute-force combos** (non-reproducible) with a fully
deterministic ladder; order tuned empirically per corpus yield (data-driven, re-derived by xtask):

```
S0 normalize   luma extraction (SIMD on wasm) · EXIF orientation · optional ROI crop
S1 pyramid     downscale-first: ~512px decode attempt BEFORE full-res (artistic codes often
               decode better downscaled; pixels drive cost — proven default in production scanners)
S2 direct      both engines on S1/S0 · rxing TryHarder+AlsoInverted · rqrr (mirrored free)
S3 enhance     deterministic transform set: Otsu · invert · contrast stretch · per-channel (R/G/B)
S4 deep sweep  curated transform grid (replaces RNG brute force) · `parallel` feature uses rayon
   each stage: early-exit on detection · Budget (web-time wall-clock) · CancelToken checkpoints
```

**Charset correctness (cross-engine parity)**: rqrr's `decode()` forces UTF-8 and *discards ECI*
(verified in source 0.10.1) → we route rqrr through `decode_to()` raw bytes and resolve charset
ourselves (UTF-8 → ECI → Shift-JIS/Latin-1 sniff via `encoding_rs`), matching rxing's full
handling. `DecodedContent { text, raw: Vec<u8>, charset }` keeps both truths. Parity tests
(UTF-8/Kanji/Latin-1 fixtures) across engines.

Future (v0.5+ research): tier-5 ML detection/rectification for "hard mode" — qrdet-style ONNX
QR detector through `tract`/`candle` (pure Rust, sovereign), feeding rectified crops to the
ladder. The 2026 sovereign replacement for OpenCV WeChat-QRCode. Trigger-gated, not v0.3.

## 7. Score contract v3 — margin, not verdict

**Axes** (each a deterministic *ramp* — survival-curve, not single-point pass/fail):

| Axis | Ramp | Why (research) |
|---|---|---|
| Resolution | downscale steps → px/module floor | ISO floor ≥5 px/module · sampling margin |
| Blur | gaussian σ ramp | focus/motion |
| Contrast | global reduction ramp | symbol contrast proxy |
| **Perspective** | tilt 15°/30°/45° on two axes | **the documented blind spot** of naive scorers — artistic codes erode grid-estimation margin first (BoofCV v3 · DiffQRCoder angle-SSR) |
| **Rotation** | non-cardinal angles | grid estimation |
| **Lighting** | shadow gradient · glare blob · exposure ± | *local* illumination ≠ global contrast — distinct failure modes (BoofCV categories) |

**Sub-grades (ISO-15415-inspired, named, reported)**
- `decode` — reference decode pass.
- **`uec` — synthetic Unused Error Correction (flagship · no pure-Rust competitor ships it)**:
  re-encode the decoded payload at the *same version/EC/mask* (rqrr `MetaData` provides all
  three), perspective-sample the input at the recovered grid (rqrr `bounds` homography), count
  module disagreements vs capacity → ISO formula `UEC = 1 − (e+2t)/(d−p)`, worst RS block,
  grades A ≥0.62 · B ≥0.50 · C ≥0.37 · D ≥0.25 · F. Gaussian module-center weighting
  (DiffQRCoder SRL). Cross-check path: rxing's internal `errorsCorrected` exists but is
  unpopulated for QR — candidate upstream PR, not a dependency for v0.3.
- `finder_integrity` — 1:1:3:1:1 run-length template check at the three finders (the #1
  documented AI-art killer) — **caps the composite score** when damaged.
- `quiet_zone` — ≥4-module clear border check.
- `consensus` — per-engine survival (a cell only rxing survives = marginal in the real world).

**Composite**: weighted AUC of survival curves + structural caps → 0-100 + grade + the
sub-grade table. Output language: *validation*, never "ISO verified". `Hint` derives from the
failing axis/sub-grade: `RaiseErrorCorrection{current}` · `IncreaseContrast` · `EnlargeModules`
· `FixFinderPattern{corner}` · `RestoreQuietZone` · `ReduceArtTexture` (non_exhaustive).
Enables the generator/agent loop: *generate → scan → act on hints → regenerate*, and the
"why 62?" UX ("survives 30° tilt, dies at 2.5 px/module").

**Calibration (the Modal playbook)**: corpus-driven weights now; a one-time human-device pass
(~100-500 artistic codes × 2-3 real phones, screen + paper) fits axis weights before we claim
real-world correlation. Operator task, Phase B+, with Nicolas.

## 8. Error model

`ScanError` — thiserror 2 + miette `Diagnostic` (`code("QRS-001")` + `help` + doc `url`),
`#[non_exhaustive]`, structured fields, `is_transient()` (default false), `fingerprint()`.
Lean: `QRS-001 InvalidImage` · `QRS-002 DimensionsExceeded` · `QRS-003 PixelOverflow` ·
`QRS-004 BufferMismatch` · `QRS-005 Cancelled`. Pinning tests: codes unique · wire-parity
(`"QRS-001"` Display) · transient classification. Codes never reused.

## 9. Bindings

**node** (`@supernovae-st/qrcode-ai-scanner` · napi 3.9+):
- `#[napi] fn scan(img: Buffer, opts?) -> AsyncTask<ScanTask>` — CPU work in `Task::compute()`
  on the **libuv pool** (canonical for CPU-bound; never blocks Nuxt/Nitro). Owned `Buffer` moved
  into the task = zero pixel copy.
- `AsyncTask::with_optional_signal` (AbortSignal) — aborts queued tasks; a **cooperative
  CancelToken checkpoint inside compute()** covers in-flight scans (AbortSignal alone does not).
- `scanSync` for scripts. Typed errors with `.code = "QRS-xxx"`. Generated TS types.
- Packaging: pnpm template (npm-the-pm explicitly not recommended by napi-rs), `engines.node
  ">= 20"`, CI matrix Node 22 + 24. Targets: linux x64/arm64 × gnu+musl · darwin x64/arm64 ·
  win32 x64 (+ optional win32-arm64) **+ `wasm32-wasip1-threads` fallback package** — the
  universal fallback that also covers edge/serverless runtimes where native addons can't load.
- Electron caveat documented: external buffers copy under the V8 memory cage.

**wasm** (`@supernovae-st/qrcode-ai-scanner-wasm`):
- **SIMD128-only build** (baseline ≈93% global, Safari 16.4+ 2023 — 2026 consensus for camera
  use cases). Rust ≥1.87 defaults cover bulk-memory/nontrapping; only `+simd128` added.
- Profile: `opt-level=3 · lto · codegen-units=1 · panic="abort" · strip` + `wasm-opt -O3`
  (measure vs `z`/`-Oz`; expect <15% size for >20% speed on a decoder). rxing QR-only feature
  build (granular since 0.9.0; full multi-format rxing-wasm reference = 2.28 MB raw — ours must
  be far leaner).
- **Budgets: ≤500 KB gzipped hard cap · 250-350 KB target · ≤35 ms/frame desktop · ≤80 ms
  mid-mobile @720p** (references: zxing-wasm reader 1.04 MiB raw · zbar-wasm ~330 KB).
- API: `scanFrame(data: Uint8ClampedArray, w, h, hint?)` direct from `ImageData.data`.
- `examples/web-camera/`: rVFC frame clock (universal incl. Firefox 132+) → in-flight guard +
  adaptive cadence (10-15 decodes/s cap, back off on misses) → worker → wasm; ROI center-crop
  (≈2/3 min dimension → ~512px) by default with periodic full-frame retry; corners overlay;
  `getCapabilities()`-gated torch/zoom/focusMode. getUserMedia 720p ideal.

**cli** (`qrscan`): JSON by default, `--pretty`, `--score-only`, `--profile`, semantic exit
codes — the CI corpus runner and operator dogfood.

## 10. Performance plan

1. **ROI + downscale-first** (S1 pyramid + Frame center-crop) — the proven biggest constant-factor
   win (production default in qr-scanner: 400×400).
2. **Region tracking across frames** — `scan_frame_hinted(prev_region)`: try last-hit
   neighborhood first, widen on miss. Commercial-SDK differentiator, open-source libs stop at
   static crop.
3. **SIMD128 luma/threshold kernels** + **zero per-frame allocation** (persistent scratch
   buffers in `Scanner`; single buffer in wasm, per-thread native).
4. **Engine lean builds** — rxing granular QR-only features.
5. **Bench discipline**: criterion locally · **iai-callgrind instruction-count gate in CI**
   (timing noise on shared runners swamps real regressions) · wasm corpus timings in-browser
   via `web-time` · `xtask size-report` tracks wasm gzipped size per commit against the cap.

## 11. Quality engineering

- **Corpus** = the backbone: `fixtures/` + `corpus.toml` ground truth (clean generated matrix
  version×EC · artistic from the site generator dump + public sets · degraded = deterministic
  synthetic transforms + real photos · BoofCV v3 set for sanity). `cargo xtask corpus-report`
  regenerates the README success-rate table between managed markers — derived numbers are never
  hand-typed (house law).
- **zxing-cpp 3.0.2 as offline oracle**: dev-only harness benchmarking our decode rate vs the
  C++ state of the art (circular-finder detection, `extra("UEC")`) on the corpus. Tells us when
  a ladder stage underperforms; never a runtime dependency (cmake/C++20 — and no wasm32 path).
  Publishing the artistic-QR engine benchmark = credibility asset (none exists publicly).
- **Tests**: TDD · unit + corpus integration · insta snapshots of ScanReport JSON (schema drift
  gate) · proptest (arbitrary inputs never panic; dimension invariants) · **cargo-mutants ≥90%
  on core** · cargo-fuzz decode target (input-parsing lib ⇒ fuzz mandatory) seeded from fixtures.
- **CI**: fmt · clippy (deny unwrap/expect) · nextest 3 OS · wasm32 build + size gate ·
  cargo-deny (licenses/advisories) · iai-callgrind gate · mutants + fuzz weekly.
- AGPL-3.0 kept (anti-extraction moat; we hold copyright — our proprietary site consumes our
  own crate freely; Nika AGPL→AGPL clean).

## 12. Nika consumption contract

- Core stays pure/sync/deterministic (replay & cache-safe for workflow engines) · `Send + Sync`
  · no global state · no tokio · serde report = the cross-boundary JSON contract (versioned).
- The future builtin (`invoke: qr.scan`, lives in the Nika repo) wraps `Scanner::scan`, maps
  `ScanError` structured variants → Nika registry codes via `NikaErrorCode`, and surfaces
  `hints` so agent loops can *act* (regenerate with `RaiseErrorCorrection`, etc.).
- Diamond rules applied now (lints-deny unwrap · non_exhaustive · caps · mutants · TDD) ⇒ zero
  rework at admission time.

## 13. Delivery phases

| Phase | Scope | Exit |
|---|---|---|
| **A — core** | workspace reset · engines (rxing 0.9 lean + rqrr raw-path + charset parity) · deterministic ladder · score v3 (stress axes + survival + structural + **UEC**) · payloads · errors QRS · corpus + tests/mutants/fuzz · docs | corpus report ≥ v0.2 baseline on artistic · all gates green |
| **B — node** | napi 3 AsyncTask + AbortSignal + batch · TS types · platform pkgs + wasip1 fallback · publish **0.3.0** | Nicolas integrates on his `scanner` branch (quality-gate ≥70 + upload tool) — PR to HIS branch, never his main |
| **C — wasm** | SIMD build + size gate · scanFrame + ROI hint · web-camera example | live camera scan in the landing · budgets met |
| **D — later** | Nika builtin (Nika repo arc) · human-device calibration pass · zedbar promotion · tier-5 ML detector research · rxing errorsCorrected upstream PR | trigger-gated |

Versioning: 0.2.2 → **0.3.0**, breaking clean, no shims. Work directly on `main` (Thibaut-DRI),
atomic commits, `Co-Authored-By: Nika 🦋 <nika@supernovae.studio>`, tags `v0.3.0-alpha.N`.

## 14. Research evidence (2026-06-11 sweep · key facts)

- **rxing 0.9.1** (2026-06-08): granular per-symbology features (0.9.0) · TryHarder ·
  `AlsoInverted` · MicroQR + rMQR decode · zxing-cpp QR detector ported in · result points =
  finder centers (not 4 corners) · `errorsCorrected` field exists but unpopulated for QR ·
  rxing-wasm full build 2.28 MB raw. — github.com/rxing-core/rxing
- **rqrr 0.10.1**: `bounds: [Point; 4]` · `MetaData{version, ecc_level, mask}` ·
  `get_raw_data()` raw uncorrected bitstream (unique — enables synthetic UEC) · ECI
  parsed-then-discarded + forced UTF-8 in `decode()` (source-verified) · mirrored since 0.10.0.
  — github.com/WanzenBug/rqrr
- **zxing-cpp 3.0.2**: `extra("UEC")` margin [0,1] · "improve detection rate for circular finder
  patterns" (3.0.0 — directly artistic-relevant) · C++20/cmake · no wasm32 via Rust binding ·
  zxing-wasm npm reader 1.04 MiB raw. — github.com/zxing-cpp/zxing-cpp · Sec-ant/zxing-wasm
- **zedbar 0.4.1** (2026-05-12): new pure-Rust ZBar port, third detector family, active.
- **ISO 15415 / UEC**: `UEC = 1−(e+2t)/(d−p)` worst-block · grades A≥0.62/B≥0.50/C≥0.37/D≥0.25 ·
  software-only = validation not verification (Cognex/Euresys framing) · perspective/rotation =
  documented gap of naive scorers (BoofCV v3 categories · DiffQRCoder WACV 2025 angle-SSR ·
  Modal QArt evals best-of-8 + human-phone alignment ≈2000 pairs). — ni.com · euresys.com ·
  modal.com/blog/qart-codes-evals · arxiv.org/abs/2409.06355
- **napi-rs 3** (stable 2025-07 · napi 3.9.1): AsyncTask on libuv = canonical CPU-bound path ·
  `with_optional_signal` AbortSignal (queued-only ⇒ cooperative token needed in-flight) ·
  owned Buffer zero-copy · pnpm template · Node 20 EOL 2026-04 ⇒ engines ≥20, test 22/24 ·
  `wasm32-wasip1-threads` fallback package pattern. — napi.rs
- **wasm 2026**: SIMD128 ≈93% global (Safari 16.4+) ⇒ SIMD-only ship · Rust ≥1.87 defaults
  bulk-memory/nontrapping ⇒ only `+simd128` needed · skip threads (nightly + COOP/COEP) ·
  `web-time 1.1` canonical Instant · rVFC universal (Firefox 132+) · qr-scanner ROI default
  400×400 + 25 scans/s cap · iai-callgrind for CI benches. — caniuse · rustc platform docs

## 15. Open items

1. `qrcode` encoder crate: confirm it can force version+EC+**mask** for the UEC re-encode path
   (else: minimal own encoder for the reference matrix — we control scope).
2. Measure wasm size with dual engines (rxing QR-only + rqrr); decide rqrr-only camera variant
   only on real numbers.
3. zedbar: verify corners/inverted/charset behavior before promoting past feature flag.
4. Landing deployment runtime: native napi needs Node (not edge) — confirm with Nicolas;
   wasip1 fallback covers edge if needed.
5. SPEC.md: retire/rewrite as ARCHITECTURE.md + SCORING.md during Phase A (this doc supersedes).
6. Corpus sourcing: artistic dump from the site generator (ask Nicolas for N≈200 across styles).
