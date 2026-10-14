# qrcode-ai-scanner — specification

This directory is the **normative contract** of the scanner. If code and
spec disagree, one of them has a bug — file it. Everything here is
versioned by the markers every report carries:

| Marker | Current | Bumped when |
|---|---|---|
| `versions.scanner` | crate semver | every release |
| `versions.pipeline` | `3` | the decode ladder's observable behavior changes (v2: transparent inputs flatten before decode — 01-report § Alpha; v3: exact content — original QR bytes, the scanner's own parses and re-read, declarations and Kanji/Hanzi segments interpreted once, refused malformed streams, reconciliation by content identity, the walk continuing past signalled-only QR reads — 01-report § Content, 07-pipeline properties 2 and 6) |
| `versions.score_contract` | `5` | any score semantic changes (weights, axes, caps, bands; v4: alpha inputs score their flattened image; v5: the UEC margin cap, measured when `uec` is skipped, on the verified orientation's stream; interrupted judgments absent; probes match content identity) |

## Layout

| File | Contract |
|---|---|
| [`01-report.md`](01-report.md) | The `ScanReport` wire format — every field, type, nullability |
| [`02-profiles.md`](02-profiles.md) | Profiles, budgets, anti-DoS limits |
| [`03-errors.md`](03-errors.md) | `QRS-xxx` error catalog + per-surface mapping + exit codes |
| [`04-score.md`](04-score.md) | Score contract v5 — axes, weights, caps, UEC, ISO 15415 card |
| [`05-payloads.md`](05-payloads.md) | The payload kinds incl. the GS1 conformance verdict |
| [`06-hints.md`](06-hints.md) | Hint catalog with exact firing conditions |
| [`07-pipeline.md`](07-pipeline.md) | Ladder S1→S5 normative behavior + determinism contract |
| [`scan-report.schema.json`](scan-report.schema.json) | JSON Schema (draft 2020-12) of the report — machine-validatable |
| [`examples/`](examples/) | Golden reports produced by the real binary — **CI-validated** |

## Examples

Each golden is the CLI's JSON for one committed fixture, scanned unbounded
on the full profile from the repository root:
`qrscan <fixture> --profile full --budget-ms 0 > spec/examples/<golden>`.
Timings (`ms`, `total_ms`) are whatever the producing run measured; every
other field is reproducible. Goldens carry the `versions` of the build that
produced them, so a marker or version bump regenerates the set (a
development build's markers can differ from the last release's — 01-report
§ Machine-readable mirrors). The generated fixtures come from
`cargo run -p xtask -- gen-fixtures`.

| Golden | Fixture | Shows |
|---|---|---|
| [`examples/artistic-blob.json`](examples/artistic-blob.json) | `fixtures/artistic/blob-style-monkey-logo.webp` | an artistic code only the deep stage reads |
| [`examples/clean-url.json`](examples/clean-url.json) | `fixtures/clean/OK_68ms_100_4e875a2c.png` | a clean URL, both engines, every score section |
| [`examples/crypto-bitcoin.json`](examples/crypto-bitcoin.json) | `fixtures/payloads/crypto-bitcoin.png` | the `crypto` payload |
| [`examples/datamatrix.json`](examples/datamatrix.json) | `fixtures/symbology/datamatrix.png` | a Data Matrix symbol |
| [`examples/ean13-retail-gtin.json`](examples/ean13-retail-gtin.json) | `fixtures/symbology/ean13.png` | a retail barcode, the GS1 payload |
| [`examples/eci-iso-8859-2.json`](examples/eci-iso-8859-2.json) | `fixtures/eci/eci4-a1.png` | ECI 4 over byte A1: `charset` `eci`, the located designator |
| [`examples/eci-undecodable.json`](examples/eci-undecodable.json) | `fixtures/eci/eci899-binary.png` | ECI 899 (binary): `charset` `unknown`, the exact bytes |
| [`examples/gs1-digital-link.json`](examples/gs1-digital-link.json) | `fixtures/payloads/gs1-digital-link.png` | the `gs1_digital_link` payload |
| [`examples/gs1-fnc1.json`](examples/gs1-fnc1.json) | `fixtures/clean/gs1-fnc1.png` | FNC1 in first position under ECI 26: `fnc1`, a GS1 element string |
| [`examples/hanzi-gb18030.json`](examples/hanzi-gb18030.json) | `fixtures/eci/hanzi-zhongwen.png` | a Hanzi segment: `charset` `gb18030` |
| [`examples/mecard.json`](examples/mecard.json) | `fixtures/payloads/me_card.png` | the `me_card` payload |
| [`examples/micro-qr.json`](examples/micro-qr.json) | `fixtures/symbology/microqr.png` | a Micro QR symbol |
| [`examples/no-detection.json`](examples/no-detection.json) | `fixtures/degraded/FAIL_1491ms_0_584c998c.png` | no detection: an empty, final report |
| [`examples/rescued-occluded.json`](examples/rescued-occluded.json) | `fixtures/degraded/logo-occluded-rescue.png` | the S5 rescue (`engines: ["rescue"]`) |
| [`examples/sms.json`](examples/sms.json) | `fixtures/payloads/sms.png` | the `sms` payload |
| [`examples/transparent-envelope.json`](examples/transparent-envelope.json) | `playground/public/samples/transparent.png` | the `alpha` block and the placement envelope |
| [`examples/transparent-palette.json`](examples/transparent-palette.json) | `playground/public/samples/transparent.png` | the same scan with `--alpha-palette '#f5f5f4,#1c1917'`: per-color verdicts |
| [`examples/wifi.json`](examples/wifi.json) | `fixtures/payloads/wifi.png` | the `wifi` payload |

## Anti-rot guarantees

The spec cannot silently drift from the code:

1. `crates/qrcode-ai-scanner/tests/spec_golden.rs` deserializes every
   `examples/*.json` through the real serde types AND validates each one
   against `scan-report.schema.json` — a contract change that forgets the
   spec breaks the build. It also holds every golden to the build: a final
   report (`trace.budget_exhausted: false`), this build's `versions`, no two
   detections that denote one content, and a row below naming its fixture.
2. The TypeScript mirror (`bindings/report-types.d.ts`) ships inside both
   npm packages and is synced by `scripts/sync-report-types.mjs`.
3. The wire names (`snake_case` tags, profile names, error codes) are
   pinned by unit tests next to their Rust definitions.

## Reading order for agents

`01-report.md` → `05-payloads.md` → `03-errors.md` covers 90% of consumer
integration questions. The schema + examples are the ground truth a tool
can load directly.
