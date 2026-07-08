# Changelog

All notable changes to this workspace. Every surface versions together — the Rust
crate + CLI, the npm node + wasm packages, the Python wheel, and the
Kotlin/Android · Swift/iOS · Flutter bindings.

## 0.6.0 — 2026-07-08

### Added

- CLI `--budget-ms` — override the profile's wall-clock budget (0 =
  unbounded), same semantics as every other binding surface. The CLI was the
  last surface without the knob.
- QD-2 settled by measurement: `cargo xtask rescue-stress` (2304
  deterministic occlusion scans, no RNG, byte-identical reruns) shows the S5
  erasure rescue is refusal-safe — 82/82 rescue decodes correct, 77% refusal
  rate, zero miscorrections. The harness runs weekly in deep-checks with a
  hard zero-rescue-wrong gate. (`QRS_RESCUE_CELL=<substring>` filters the
  grid to a single cell — the repro tool that pinned the rxing OOM wrap.)
  Separately measured: the base rxing engine
  wrong-decodes ~1% at format-info-adjacent occlusion and carries no
  bitstream, so `low_correction_margin` structurally cannot flag that class
  (tracked as its own decision).

### Fixed

- **The rotation stress axis measured frame cropping, not rotation
  tolerance**: `warp::rotate` spun the image within its own canvas, so a
  full-frame symbol's corners — finder patterns included — left the frame
  from the very first 10° ramp step (a v10's corner radius is ≈0.62·w
  against the 0.5·w half-frame). A flawless symbol read `rotation 1/5`
  and its score carried a phantom −8 penalty for a rotation any phone
  handles; engines are rotation-invariant on clean symbols, so every one
  of those deaths was a probe artifact. The canvas now grows to the
  rotated bounding box (pixel-centre exact; f64 trig with a snap-epsilon
  so cardinal angles stay pure shape swaps), and out-of-source lookups
  paint true white background instead of clamp-streaking the source
  edges — the same fix makes the perspective cells' out-of-trapezoid
  corners honest. Pinned by a red→green test: a clean v10 must survive
  the full rotation ramp 5/5. Score VALUES rise for full-frame symbols
  (the old readings under-reported margin); the contract is unchanged —
  same axes, weights, ramps, caps, bands (`score_contract` stays 3, the
  UPC-E wire-value-bugfix precedent).
- **A crafted image can no longer OOM the host through rxing**: rxing 0.9.1's
  RSS-14 (1D) reader underflows a subtraction on a real occlusion cell, and
  the release-mode wrap drove a Vec that doubled itself toward a 14 GiB
  request — the root cause of the silent weekly-CI runner deaths, and an
  adversarial-image DoS candidate on the multi-format path. A
  `[profile.release.package.rxing]` `overflow-checks = true` override turns
  the wrap into the caught-and-counted panic the engine wrapper already
  records (`report.trace.engine_panics`). Proof: the pathological cell
  completes with zero ≥1 GiB allocations and the 2304-cell QD-2 grid stays
  byte-identical (rescue 82/82, wrong 0).
- `cargo add qrcode-ai-scanner` no longer serves a stale crate: crates.io was
  the forgotten fifth registry — nothing ever published to it and nothing
  gated the drift, so the README's first install surface silently served
  0.3.0 through two releases. Both 0.5.0 crates are live, and
  `crates-publish.yml` owns every future tag (existence-probe gated: re-runs
  resume instead of failing on the already-published half).
- JitPack (Kotlin/Android) v0.5.0 build died exit-127: `/opt` is not writable
  on the JitPack image and the quiet `unzip` swallowed the gradle-provisioning
  failure. Gradle now unpacks into `$HOME` with loud failure at every step,
  provisions its own Android cmdline-tools (the image ships none), and a
  native-count gate fails any AAR built without its native libraries instead
  of shipping a lying artifact. First working JitPack build lands with the
  next tag.

### Testing / CI

- cargo-deny findings in the flutter binding tree cleared: stale-lock updates
  (backtrace → 0.3.76 drops `adler` entirely, futures → 0.3.32 un-yanks) and a
  `[[licenses.clarify]]` pinning allo-isolate's real Apache-2.0 (repo LICENSE;
  metadata fix merged upstream, unreleased).
- Shard-0 harvest closed: builder limits-threading pinned (a builder that
  discards its config now fails QRS-002/003 assertions), engine panic tally
  consolidated into one `run_engine()` helper and pinned deterministically.
  cargo-mutants now runs under nextest (`test_tool` policy) so the wall-clock
  test group serialization applies to mutants baselines too.
- The full-crate mutation campaign closed: all 127 weekly-run survivors are
  now killed or individually proven equivalent — the decode ladder
  (merge/absorb identity, rescue candidate collection, stage gating), the
  warp stress synthesizers (tilt/rotate/shadow/glare geometry, hand-computed
  pixel pins), the rescue bitstream parser + the ISO Annex B protection
  table (pinned against an independent enumeration, never the code's own
  formula), and a nine-file sweep (charset, engine adapters, GF(256) log
  table, sampler, payload gates, report predicates, ISO parameters, axis
  folds, Berlekamp-Massey fold bounds — the last settled by a
  125,536-syndrome differential census). 20 exclusions are pinned in
  `.cargo/mutants.toml`, each with its argument inline — 18 argued
  equivalent, plus the wall-clock-shrink pair argued untestable (a
  machine-speed lower bound is the assertion the determinism doctrine
  forbids). The weekly deep-checks missed count is a true zero baseline,
  enforced across 16 step-timed shards.
- pub.dev publish exchanges OIDC before the Flutter toolchain lands
  (dart-lang/setup-dart pinned first — the v0.5.0 job hung 2 h on
  interactive auth) and is capped at 15 min. The FIRST publish stays a
  manual operator act by pub.dev design: automated publishing arms on an
  existing package.
- The weekly mutants runner deaths are closed as a class: disk exhaustion
  refuted by its own telemetry, the rxing 14 GiB wrap pinned (see Fixed),
  and the residual hole — two parallel test processes, each under the old
  12 GB per-process virtual cap, can together write past a 16 GB box —
  closed by bounding the aggregate instead: 6 GB ulimit ×
  `NEXTEST_TEST_THREADS=2` across 16 step-timed shards (run 28879086942 is
  the evidence trail).
- RUSTSEC-2026-0204 (crossbeam-epoch 0.9.18: invalid pointer dereference in
  the `fmt::Pointer` impl, reached here via rayon/criterion) cleared across
  all five lockfiles — workspace, fuzz, py, uniffi, flutter/rust — by
  updating to 0.9.20 the day the advisory published.
- `cargo xtask rotation-sweep` — decode-under-rotation truth on the zxing
  blackbox corpus, two tables: cardinal 0°/90°/180°/270° via exact
  index-permutation rotations (lossless by construction: a rotated column
  sitting below its 0° sibling is an engine orientation gap, never image
  damage), and arbitrary 15°/30°/45° via bilinear rotation into the grown
  canvas (interpolated — a trend line, never a gate). The external gate,
  the manifest pins and the README headline all measure at 0° only; this
  closes that blind spot as a non-gating dashboard. First cardinal table:
  170/171/170/171 of 179 — flat; the engines have no cardinal orientation
  gap on real photos.
- CLI end-to-end wire-contract tests: the shipped binary's real JSON output
  now validates against `spec/scan-report.schema.json` (jsonschema dev-dep)
  across eight decode branches — direct, upscale, EXIF-rotated, boost-rung
  artistic, morph-rung webp, retail GTIN, FNC1 element string, micro-QR —
  plus the NOT-FOUND report (empty detections is a wire shape too, exactly
  where absent-vs-null serde drift hides). The type-parity gate holds the
  TYPE surfaces identical; this is the first gate on a real binary's real
  OUTPUT, the layer where serde attributes live and where the Dart
  int/double truncation class actually bit. Cross-process determinism is
  pinned the same way: two binary runs must agree byte-for-byte modulo the
  documented wall-clock fields.
- The three stock-budget artistic integration tests now reserve every
  nextest scheduler slot (`threads-required = "num-cpus"`): the wall-clock
  group only serialized them against each other while the other ~300 tests
  starved their 4 s budget from the sibling cores — two different victims
  across three same-day runs, ~4.2–5.2 s under load vs ~1.3 s solo. Proven
  by three consecutive full-suite green runs.

## 0.5.0 — 2026-07-05

Correctness + release-trust hardening (night sweep 2026-07-05). Wire schema
unchanged (score contract v3); two wire VALUES corrected as bugfixes. First
mobile release: pub.dev + SwiftPM xcframework + JitPack all fire on this tag.

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
  and `finder_score`'s region-mean threshold. Second pass killed the
  quiet-zone / strided-window / zigzag-bounds survivors; the 13 remaining
  are individually proven equivalent and pinned in `.cargo/mutants.toml` —
  the weekly missed count is a zero-baseline signal now.
- Internal: the symbology substrate (GF(256) tables, version DB, zigzag
  walk, block deinterleave, grid sampler) moved out of `score::` into a
  dedicated `matrix/` module — the rescue path no longer imports scoring
  code. No API or behavior change (same test count green, wire identical).
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
