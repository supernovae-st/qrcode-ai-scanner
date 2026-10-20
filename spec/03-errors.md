# 03 · Errors — the QRS catalog

**"No QR found" is NOT an error** — it is `Ok` with empty `detections`.
Errors are reserved for real faults. Every error carries a stable wire
code; the cross-surface match contract is the regex `QRS-\d{3}`
(delimiters unified to `[QRS-xxx]` on every surface).

## Catalog

| Code | Name | Meaning | Typical trigger |
|---|---|---|---|
| `QRS-001` | InvalidImage | bytes are not a decodable image | corrupt upload, truncated file, unsupported format |
| `QRS-002` | DimensionLimit | width/height exceeds `max_dimension` | oversized upload |
| `QRS-003` | PixelLimit | total pixels exceed `max_pixels` | decompression bomb, huge photo |
| `QRS-004` | BufferMismatch | raw buffer length ≠ expected for (w,h,format) | binding misuse (`rgba8`/`luma8` paths) |
| `QRS-005` | Cancelled | the `CancelToken` fired | caller-initiated cancellation |

## Per-surface mapping

| Surface | Shape | How to match |
|---|---|---|
| Rust | `Err(ScanError)` — `.code()` returns `"QRS-xxx"`, `thiserror` + `miette` diagnostics | `match err.code() { "QRS-003" => … }` |
| CLI stderr | `error: <message> [QRS-xxx]` + exit code 2 | match the code, not the prose |
| Node | rejected Promise, `Error.message` ends with `[QRS-xxx]`; a pre-aborted signal rejects with an `AbortError` DOMException | `e.message.match(/QRS-\d{3}/)` |
| WASM | thrown `JsError`, message ends with `[QRS-xxx]` | same match |

## CLI exit codes (the headline contract)

| Exit | Meaning |
|---|---|
| `0` | At least one symbol decoded, of any symbology. With `--score-only`, its score value is on stdout; a printed value is a complete judgment and does not depend on the budget. |
| `1` | No symbol decoded, in ALL output modes including `--score-only`. Final only when the scan was not cut: `trace.budget_exhausted` is `false` in the JSON report. `--score-only` and `--pretty` say when the budget ran out first. |
| `2` | Invalid input, usage or fault: a QRS error · an unreadable file · `--score-only` with a configuration that computes no score (the `frame` profile, or every stress axis skipped) · `--score-only` when a contained internal fault left the score absent. |
| `3` | `--score-only` only: a symbol was decoded, but the wall-clock budget ran out before its score was judged (`score: null`, `trace.budget_exhausted: true`). Nothing is printed on stdout. Retry with a larger `--budget-ms`; `0` (unbounded) never yields 3. |

In JSON and `--pretty` modes, a completed scan exits 0 or 1; a missing
score is read from the report. Branch on the CODE, never on the message
text.

## Consumer guidance

Browser initialization happens before a scan and has a separate additive JS
loader error contract (`ScannerInitError` from the npm `/auto` entry). Its feature,
network, asset, validation, compilation and instantiation codes are documented in
`docs/api/wasm.mdx` and the WASM package README. They are not new Rust `ScanError`
variants or `QRS` codes. Preserve initialization/worker/scan phases when relaying
errors. A completed empty report remains successful engine execution.

- Branch on the CODE, never the message text (prose may improve).
- `QRS-002/003` on a server = your limits working; respond 413/422, don't
  retry.
- `QRS-001` covers attacker-controlled bytes by design — it is safe to
  surface "invalid image" verbatim to end users.
