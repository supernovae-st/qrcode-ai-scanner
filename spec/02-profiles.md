# 02 · Profiles, budgets, limits

## The three profiles (wire names: `full` · `fast` · `frame`)

| | `full` (default) | `fast` | `frame` |
|---|---|---|---|
| Use case | generator quality gate | upload tool | live camera frames |
| Budget (default) | 4000 ms | 800 ms | 80 ms |
| S1 pyramid (≤512px) | ✓ | ✓ | ✓ |
| S2 direct (full res) | ✓ | ✓ | ✓ |
| S3 enhance (otsu·invert·stretch·RGB) | ✓ | ✓ | — |
| S4 deep (15 rungs + grid) | ✓ | — | — |
| S5 rescue (errors-and-erasures) | ✓ | ✓¹ | ✓¹ |
| Scoring depth | Full (5 cells/axis) | Reduced (2 cells/axis) | Off (`score: null`) |

Format coverage per stage: S1-S3 decode ALL symbologies; S4 (the
QR-calibrated deep rungs) and S5 restrict to the QR family — paying the
1D/PDF417/DataMatrix detectors on 17 recovery rungs would starve the
budget for nothing.

¹ S5 runs on EVERY profile whenever a rescue candidate was collected, the
ladder came up empty and budget remains — `fast` collects candidates from
S1-S3; under `frame`'s 80 ms budget the stage is usually cut before it
attempts (the budget, not the profile, is the gate).

**Profile choice rule of thumb:** the blob/dot pixel template class (a
qrcode-ai.com generator OUTPUT) only decodes in S4 — a verify flow for
generator output must use `full` (bound it with `budget_ms`, below). `fast`
reading "NO" on valid product output is the documented trap.

## Budget semantics

- ONE wall-clock budget covers the WHOLE scan: ladder + scoring share the
  same deadline.
- Granularity is the ATTEMPT/CELL: an in-flight engine call is not
  interruptible (which is why engine inputs are size-capped — see Limits).
- Override per call: Rust `ScanProfile::Custom(config)` ·
  CLI `--profile` only (no budget flag yet) · Node `budgetMs` ·
  WASM `budget_ms` (positional arg #5 of `scan_image`, #5 of `scan_frame`).
- `0` or negative = **unbounded** (NOT a zero-millisecond budget).
- With a budget set, WHERE the run cuts is machine-dependent — set
  `budget_ms: None`/unbounded for strictly reproducible reports. The cut is
  best-effort and observable after the fact: the per-stage `trace` records
  how far the ladder got (`transforms_tried` per stage). A future
  work-counted budget (bounding ATTEMPTS instead of milliseconds, fully
  deterministic) would be additive; this contract does not foreclose it.

## Anti-DoS limits (validated BEFORE decode)

| Limit | Default | Surface knobs |
|---|---|---|
| `max_dimension` (px per side) | 10 000 | Rust `Limits` · Node `maxDimension` · WASM arg |
| `max_pixels` (total) | 64 000 000 | Rust `Limits` · Node `maxPixels` · WASM arg |
| `max_engine_side` (internal) | 2 048 | Rust `ScanConfig` only — engines NEVER see larger |
| decoder allocation cap | `max_pixels × 8` bytes | automatic (decompression-bomb guard) |
| detections per report | 16 | fixed |
| rescue candidates per scan | 4 | fixed |

Server guidance: lower `maxDimension`/`maxPixels` for untrusted uploads
(e.g. 4096 / 16M). Violations are `QRS-002`/`QRS-003` errors — see
[03-errors.md](03-errors.md).

## Cancellation

- Rust: `scan_cancellable(&CancelToken)` — checked between attempts/cells.
- Node: `AbortSignal` rejects QUEUED tasks only; a RUNNING scan completes
  within its budget (in-flight cancel is NOT wired — by design at ≤4 s
  budgets).
- WASM: synchronous on the calling thread — budget IS the bound.
