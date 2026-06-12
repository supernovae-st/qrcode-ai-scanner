# Changelog

All notable changes to this workspace. The four artifacts version together
(`qrcode-ai-scanner` · `qrcode-ai-scanner-cli` · `@supernovae-st/qrcode-ai-scanner`
· `@supernovae-st/qrcode-ai-scanner-wasm`).

## 0.3.0 — 2026-06-12

Full rebuild ("Diamond-grade"): deterministic architecture, scoring contract
v3, GS1 awareness, ISO-informed grading, hardened bindings. Supersedes the
0.2.x exploration line.

### Decoding

- Deterministic decode ladder (S1 pyramid → S2 direct → S3 enhance → S4 deep)
  replaces the v0.2 RNG brute force — same input, same result, always.
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
  `scan_frame`, SIMD128, ~607 KB gz, typed `ScanReport` return, `budget_ms`
  override for UI-thread bounds.
- One canonical TS contract (`report-types.d.ts`) shared by both packages.
- CLI `qrscan`: JSON by default, `--pretty` (terminal-injection-sanitized),
  `--score-only`, exit codes 0/1/2.

### Measured accuracy (reproduce: `scripts/zxing-blackbox.py` · `scripts/batch-scan.py`)

- zxing blackbox qrcode-1…6: 170/179 exact-text match @ 0° — beats the zxing
  reference thresholds on all six suites.
- qrcode-ai.com production templates: 15/15 decoded.

## 0.2.2 — 2026-06-10

Last release of the exploration line (sync napi binding, RNG-based retry
scanner). Superseded by 0.3.0.
