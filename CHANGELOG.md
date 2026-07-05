# Changelog

All notable changes to this workspace. Every surface versions together — the Rust
crate + CLI, the npm node + wasm packages, the Python wheel, and the
Kotlin/Android · Swift/iOS · Flutter bindings.

## Unreleased

Correctness + release-trust hardening (night sweep 2026-07-05). Wire schema
unchanged (score contract v3); two wire VALUES corrected as bugfixes.

### Added

- DoS-hardening + latency knobs across the mobile/server bindings: UniFFI
  (Kotlin/Swift) `scan` / `scan_frame` gain `max_dimension` / `max_pixels`
  input caps and a `budget_ms` wall-clock override; Python gains `budget_ms`
  (its input caps landed earlier this cycle). Same names, same semantics as
  Node/WASM (`0` = unbounded — spec/02). All arguments default, so existing
  call-sites are untouched.

### Fixed

- **UPC-E `gtin`/`conformant` were wrong**: the check digit was computed over
  the compressed 8-digit form and the GTIN-14 padded from it. Now expands
  UPC-E → UPC-A first (GS1 rule), then check-digits and pads from the
  expansion.
- GS1 evidence sniffing no longer counts the *stripped leading* GS of a
  transmission artifact as element-string evidence.
- Flutter mirror: `UecReport.margin` and `StructuralReport.finderIntegrity`
  are `double` now — the previous `int` truncation read every real margin
  (0.85, 0.62…) as 0. Lands before the first pub.dev publish.

### Testing / CI

- Rescue path fuzzed at last: `fuzz_scan_full` (reaches S4 deep + S5 rescue)
  and `fuzz_rescue_bitstream` (adversarial bitstream → errors-and-erasures
  correction via a `cfg(fuzzing)` bridge). Weekly fuzz job repaired (missing
  gitignored corpus dir) and now runs all four targets.
- Corpus gains `expect = "fail"` frontier entries: 6 vendored blind-spot
  fixtures (multi-symbol collages · extreme 3D perspective · busy scenes);
  an expected-fail that starts passing fails the run loudly — capability
  gains are detected, never silent.
- Golden-value pins for `otsu_threshold` / morphology / `downscale_to`
  (the surviving-mutant cluster) + direct rqrr/rxing adapter tests + the
  UEC 0.25/0.24 grade edge.
- Mutant-harvest sweep to zero on the hot math: Berlekamp-Massey locator
  degree pinned exact on injected errors (0–3 plus a 90-pattern sweep across
  `npar` 4..=20), `windowed_extremum` clamp/identity edges, Otsu's
  negative-variance init and argmax weighting, `has_color` source-kind flag,
  and `finder_score`'s region-mean threshold.
- Threaded determinism pinned: one shared `Scanner` scanned from 4 OS
  threads under contention produces byte-identical reports vs a sequential
  baseline (wall-clock trace fields zeroed — the one documented
  nondeterminism). The three stock-budget artistic integration tests now run
  in a serialized nextest group, so a loaded machine no longer flakes them.
- External corpora pinned without vendoring: `corpus-external.tsv` commits
  sha256 + measured decode status for all 522 files (zxing blackbox 170/179
  @ 0°, gallery 17/27); `corpus-report --external` re-hashes + re-scans
  budget-free and exits red on regression AND capability gained. CI gains
  `pipefail` — a red exit piped through `tee` used to be swallowed.
- Type-parity gate v2: 8 enums · 15 contract structs · payload kinds ·
  hint tags, held identical across Rust ↔ TypeScript ↔ JSON-Schema ↔ Dart
  (the Dart mirror was previously ungated).
- Release trust: the version gate now covers the node package.json + flutter
  pubspec + tag==workspace (a forgotten bump used to become a silent
  non-release); the published wasm crate is now cargo-checked on every PR;
  cargo-deny enforces bans+sources on the workspace and scans the three
  excluded binding lockfiles (report-only).
- New MSRV CI job — whose first run proved the declared 1.87 floor had
  never compiled (let-chains stabilized in 1.88). **MSRV is now honestly
  1.88** across all manifests + the JitPack pin. Cargo.lock is committed
  (the workspace ships binaries and claims determinism; a floating dep
  graph contradicted both) and the workspace uses resolver 3, so re-locks
  are MSRV-aware by construction.

### Docs

- Prose truth-sync with the implementation: the erasure-rescue re-check
  rejects *inconsistent* corrections (a codeword-valued miscorrection is
  the UEC margin-0 class, flagged downstream); stress-cell probe set
  documented as direct + otsu + the baseline's deep rung (`CellProbe`);
  quiet-zone probe documented as the 2-module ring it is; the lighting
  set's no-knee-exit semantics clarified everywhere.

## 0.4.0

New binding surfaces + cross-binding consistency. The core decoding/scoring
contract is unchanged from 0.3.0 (same `ScanReport`, score contract v3).

### Bindings

- **Kotlin/Android + Swift/iOS** (UniFFI → JitPack / SwiftPM) — the same
  `ScanReport` as a JSON string. Mobile CI builds the AAR + xcframework.
- **Flutter/Dart** (flutter_rust_bridge → pub.dev) — typed `ScanReport` facade
  (sealed `Payload`/`Hint`, ISO 15415 / UEC cards), tolerant of unknown values.
- **Consistency:** every binding (py · node · wasm · uniffi · flutter) now
  surfaces the `[QRS-xxx]` wire code in errors (py + uniffi previously dropped it).
- Python: optional `max_dimension` / `max_pixels` caps on `scan` / `scan_frame`.

### Core

- UEC worst-block now tracks the lowest-margin block (not the most-errors block);
  all-clean symbols report a real capacity. Morph filter rewritten to an
  output-identical monotonic-deque pass.
- Dual-licensed: AGPL-3.0-or-later OR commercial (see `LICENSING.md`).

## 0.3.0 — 2026-06-12

Full rebuild ("Diamond-grade"): deterministic architecture, scoring contract
v3, GS1 awareness, ISO-informed grading, hardened bindings. Supersedes the
0.2.x exploration line.

### Decoding

- Deterministic decode ladder (S1 pyramid → S2 direct → S3 enhance → S4 deep
  → S5 rescue) replaces the v0.2 RNG brute force — same input, same result,
  always.
- **Every mainstream symbology** (19): the QR family (QR · Micro QR · rMQR)
  plus Data Matrix, Aztec, PDF417, MaxiCode, EAN-13/8, UPC-A/E,
  Code 128/39/93, Codabar, ITF, GS1 DataBar (+Expanded), Telepen. Every
  detection carries a required `symbology`; merge identity is
  (symbology, text); only the QR family carries geometry/UEC/ISO/rescue.
- **S5 erasure rescue** (Forney 1965 errors-and-erasures RS): grids the
  engines read but could not decode are re-decoded with low-confidence
  codewords marked as half-price erasures (`e + 2t ≤ d − p`). Measured:
  center-logo occlusion tolerance 20% → 30% radius on v5-H.
- Dual engines: rxing (ZXing lineage, robustness) + rqrr (quirc lineage —
  geometry, format metadata, raw bitstream). Cross-engine merge keyed by
  resolved text (kanji-safe), bitstream + format metadata adopted as a unit.
- 15 deep rungs: 12 empirical contrast boosts + 3 morphological closes — the
  blob/dot pixel template class no threshold/contrast transform recovers.
- Charset resolution: UTF-8 → isolated-byte Latin-1 short-circuit →
  Shift-JIS → windows-1252 (fixes "Québec" → "Qu饕ec" garbling; kanji runs
  keep the SJIS reading).
- Photometric polarity threaded end-to-end: light-on-dark symbols carry
  `meta.inverted` (measured) and their structural/ISO checks sample the
  correct polarity.
- Anti-DoS: dimension/pixel/engine-side caps, whole-scan wall-clock budget,
  decompression-bomb guard, 16-detection cap, engine panic isolation.

### Scoring (contract v3)

- Six survival-ramp stress axes (resolution · blur · contrast · perspective ·
  rotation · lighting) with structural caps (finder integrity, quiet zone).
- Synthetic UEC: ISO 15415 Unused Error Correction from the engine's own
  sampled bitstream via RS syndromes + Berlekamp-Massey — exact corrected-
  error counts, ISO grade bands. Flags real Reed-Solomon miscorrections
  (`low_correction_margin` hint at margin 0).
- ISO/IEC 15415-informed grade card (`score.iso15415`): Symbol Contrast,
  Modulation, Axial Nonuniformity, Fixed Pattern Damage, UEC — each
  `{value, grade}`, overall = lowest (the ISO rule). Parameters needing
  verifier hardware are reported absent, never faked.
- Machine-actionable hints: `raise_error_correction` · `increase_contrast` ·
  `enlarge_modules` · `fix_finder_pattern` · `restore_quiet_zone` ·
  `reduce_art_texture` · `low_correction_margin`.

### Payloads

- Typed classification: url · wifi · email · sms · tel · geo · me_card ·
  crypto (BIP-21/ERC-681) · v_card · v_event · text.
- GS1: FNC1-first symbols (`]Q3`/`]Q4`) parse as `gs1` element strings
  (GenSpecs subset: predefined lengths, check digits, CSET 82, dates) and
  GS1 Digital Link URIs (Syntax 1.6, Sunrise-2027 retail form) as
  `gs1_digital_link` — both with a `conformant` verdict and per-criterion
  `issues`. GS1 syntax in a plain QR (missing FNC1) is sniffed and flagged.

### Bindings & tooling

- Node (`@supernovae-st/qrcode-ai-scanner`): napi 3 AsyncTask on the libuv
  pool, AbortSignal, `maxDimension`/`maxPixels`/`budgetMs` options, full
  TypeScript types.
- Browser (`@supernovae-st/qrcode-ai-scanner-wasm`): `scan_image` +
  `scan_frame`, SIMD128, ~797 KB gz (all symbologies), typed `ScanReport` return, `budget_ms`
  override for UI-thread bounds.
- One canonical TS contract (`report-types.d.ts`) shared by both packages.
- CLI `qrscan`: JSON by default, `--pretty` (terminal-injection-sanitized),
  `--score-only`, exit codes 0/1/2.

### Spec & docs

- `spec/`: the normative contract (report wire format, profiles, QRS error
  catalog, score v3, payloads, hints, pipeline) + a JSON Schema and golden
  examples produced by the real binary — all CI-validated against the serde
  types (`tests/spec_golden.rs`) and cross-surface type parity
  (`scripts/check-type-parity.py`).
- `docs/`: Mintlify documentation site (quickstart, concepts, per-surface
  API reference, integration guides for the qrcode-ai.com editor and for
  agents).

### Measured accuracy (reproduce: `scripts/zxing-blackbox.py` · `scripts/batch-scan.py`)

- zxing blackbox qrcode-1…6: 170/179 exact-text match @ 0° — beats the zxing
  reference thresholds on all six suites.
- qrcode-ai.com production templates: 15/15 decoded.

## 0.2.2 — 2026-06-10

Last release of the exploration line (sync napi binding, RNG-based retry
scanner). Superseded by 0.3.0.
