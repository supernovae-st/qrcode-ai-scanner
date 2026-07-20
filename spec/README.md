# qrcode-ai-scanner — specification

This directory is the **normative contract** of the scanner. If code and
spec disagree, one of them has a bug — file it. Everything here is
versioned by the markers every report carries:

| Marker | Current | Bumped when |
|---|---|---|
| `versions.scanner` | crate semver | every release |
| `versions.pipeline` | `2` | the decode ladder's observable behavior changes (v2: transparent inputs flatten before decode — 01-report § Alpha) |
| `versions.score_contract` | `4` | any score semantic changes (weights, axes, caps, bands; v4: alpha inputs score their flattened image) |

## Layout

| File | Contract |
|---|---|
| [`01-report.md`](01-report.md) | The `ScanReport` wire format — every field, type, nullability |
| [`02-profiles.md`](02-profiles.md) | Profiles, budgets, anti-DoS limits |
| [`03-errors.md`](03-errors.md) | `QRS-xxx` error catalog + per-surface mapping + exit codes |
| [`04-score.md`](04-score.md) | Score contract v3 — axes, weights, caps, UEC, ISO 15415 card |
| [`05-payloads.md`](05-payloads.md) | The payload kinds incl. the GS1 conformance verdict |
| [`06-hints.md`](06-hints.md) | Hint catalog with exact firing conditions |
| [`07-pipeline.md`](07-pipeline.md) | Ladder S1→S5 normative behavior + determinism contract |
| [`scan-report.schema.json`](scan-report.schema.json) | JSON Schema (draft 2020-12) of the report — machine-validatable |
| [`examples/`](examples/) | Golden reports produced by the real binary — **CI-validated** |

## Anti-rot guarantees

The spec cannot silently drift from the code:

1. `crates/qrcode-ai-scanner/tests/spec_golden.rs` deserializes every
   `examples/*.json` through the real serde types AND validates each one
   against `scan-report.schema.json` — a contract change that forgets the
   spec breaks the build.
2. The TypeScript mirror (`bindings/report-types.d.ts`) ships inside both
   npm packages and is synced by `scripts/sync-report-types.mjs`.
3. The wire names (`snake_case` tags, profile names, error codes) are
   pinned by unit tests next to their Rust definitions.

## Reading order for agents

`01-report.md` → `05-payloads.md` → `03-errors.md` covers 90% of consumer
integration questions. The schema + examples are the ground truth a tool
can load directly.
