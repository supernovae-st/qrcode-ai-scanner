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
| Alpha placement envelope (01-report § Alpha) | ✓ | — | — |

Format coverage per stage: S1-S3 decode ALL symbologies; S4 (the
QR-calibrated deep rungs) and S5 restrict to the QR family — paying the
1D/PDF417/DataMatrix detectors on 17 recovery rungs would starve the
budget for nothing.

¹ S5 runs on EVERY profile whenever a rescue candidate was collected, the
ladder came up empty and budget remains — `fast` collects candidates from
S1-S3; under `frame`'s 80 ms budget the stage is usually cut before it
attempts (the budget, not the profile, is the gate — and the cut report
says so, `trace.budget_exhausted: true`).

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
  CLI `--budget-ms` · Node `budgetMs` ·
  WASM `budget_ms` (positional arg #5 of `scan_image`, #5 of `scan_frame`) ·
  Python `budget_ms` kwarg · Kotlin/Swift (UniFFI) `budget_ms`.
- `0` or negative = **unbounded** (NOT a zero-millisecond budget). An unset
  `budget_ms` keeps the profile default (`full` 4000 · `fast` 800 · `frame`
  80 ms): reproducible reports need `0` (Rust: `budget_ms: None`).
- A planned unit the deadline prevents does not run: a ladder attempt or
  stage (including a stage the walk continues to because every QR Code
  read so far only signals an ECI it could not locate — 07-pipeline.md
  property 2), the opposite-background walk, a rescue candidate, a score
  calibration step or stress cell, a knee bisection, an envelope probe.
  `trace.budget_exhausted` is `true` exactly when one did not run, else
  `false`; unbounded scans are always `false`.
  - `false` = **FINAL**: the report equals the unbounded report for the
    same input, configuration and scanner build on the same platform,
    apart from `stages[].ms` and `total_ms` (PDF417: 04-score.md
    § Determinism).
  - `true` = **PARTIAL**: empty `detections` is not a final "no QR"; a
    `null` score with detections is an interrupted judgment — never a
    partial or renormalized composite, and no score hints; knee
    refinements (`refined_failed_at`) and envelope probes may be missing.
    WHERE the cut lands is machine-dependent.
- The deadline makes a judgment ABSENT; a cancelled token is `QRS-005`
  (§ Cancellation), never an absent judgment. Every attempt, cell and
  probe checks cancellation before the deadline.
- The scanner's bounded checks have no budget and never make a report
  partial: re-deriving the grids `rqrr` decoded (at most 2 orientations
  each) or detected without decoding (at most 4 per attempt, and once more
  at the end of the walk), and the re-read (at most 16 per scan) —
  07-pipeline.md property 6. The end-of-walk pass may run after the
  deadline (at most 4 error corrections, no sampling) so that a refusal
  stays a refusal; cancellation is checked before it. A judgment whose
  every planned cell ran also completes its structural, UEC and ISO
  measurements (no decode) without waiting on the deadline.

## Anti-DoS limits (validated BEFORE decode)

| Limit | Default | Surface knobs |
|---|---|---|
| `max_dimension` (px per side) | 10 000 | Rust `Limits` · Node `maxDimension` · WASM arg · Python/UniFFI `max_dimension` |
| `max_pixels` (total) | 64 000 000 | Rust `Limits` · Node `maxPixels` · WASM arg · Python/UniFFI `max_pixels` |
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
- Python / Kotlin / Swift: synchronous like WASM — `budget_ms` is the bound
  (Python releases the GIL for the scan's duration; mobile callers keep the
  scan off the main thread).
