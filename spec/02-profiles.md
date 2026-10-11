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

- ONE wall-clock budget covers the scan from the moment its input is
  ready: the clock starts after the image is decoded, validated and
  normalized (alpha flattened, luma computed), and the ladder walks,
  scoring and the envelope share that deadline. Decoding and normalizing
  the image are bounded by the input limits (§ Anti-DoS limits), not by
  the budget.
- Granularity is the ATTEMPT/CELL: an in-flight engine call is not
  interruptible (which is why engine inputs are size-capped — see Limits).
- Override per call: Rust `ScanProfile::Custom(config)` ·
  CLI `--budget-ms` · Node `budgetMs` ·
  WASM `budget_ms` (positional arg #5 of `scan_image`, #5 of `scan_frame`) ·
  Python `budget_ms` kwarg · Kotlin/Swift (UniFFI) `budget_ms`.
- `0` = **unbounded** (WASM: any value that is not above 0, NaN included)
  — NOT a zero-millisecond budget. WASM truncates a fractional budget to
  whole milliseconds, so a value strictly between 0 and 1 IS a
  zero-millisecond budget: the deadline has passed before the first stage,
  which does not run (`trace.budget_exhausted: true`). The other bindings
  take an unsigned integer. An unset `budget_ms` keeps the profile default
  (`full` 4000 · `fast` 800 · `frame` 80 ms): reproducible reports need `0`
  (Rust: `budget_ms: None`).
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
  partial (they never set `trace.budget_exhausted`): re-deriving the grids
  `rqrr` decoded (at most 2 orientations each) or detected without
  decoding (at most 4 per attempt, and once more at the end of the walk),
  the re-read (at most 16 per scan, the margin re-read among them: versions
  1 and 2 only, at most 12 format probes) — 07-pipeline.md property 6 —
  and the confirmation decode of a read the acceptance holds for
  confirmation (at most one per content and four per walk, 07-pipeline.md
  § Acceptance). The end-of-walk pass may run after the deadline (at most
  4 error corrections, no sampling) so that a refusal stays a refusal;
  cancellation is checked before it and before every confirmation decode.
  A judgment whose every planned cell ran also completes its structural,
  UEC and ISO measurements (no decode) without waiting on the deadline.
  When a transparent input's first `auto` walk publishes nothing, the
  opposite background's plane is built — composited at full resolution,
  then downscaled to `max_engine_side` — before the second walk's first
  gate checks the deadline, so that work also runs past an expired one.

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
- Node: `AbortSignal` rejects QUEUED tasks only; a RUNNING scan is not
  interrupted (in-flight cancel is NOT wired — by design at ≤4 s budgets):
  once its image is decoded, the budget stops its planned work at attempt
  granularity, and the work § Budget semantics lets run past the deadline
  still finishes.
- WASM: synchronous on the calling thread — the budget bounds the scan
  once the image is decoded, as for Node; the input limits bound the
  decode.
- Python / Kotlin / Swift: synchronous like WASM — `budget_ms` bounds the
  scan once the image is decoded, the input limits the decode (Python
  releases the GIL for the scan's duration; mobile callers keep the scan
  off the main thread).
