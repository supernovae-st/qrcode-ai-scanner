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
| `0` | ≥1 QR decoded |
| `1` | scan ran clean, no QR found (ALL output modes, incl. `--score-only`) |
| `2` | invalid input/usage (QRS error · unreadable file · `--score-only` under the `frame` profile, which computes no score) |

## Consumer guidance

- Branch on the CODE, never the message text (prose may improve).
- `QRS-002/003` on a server = your limits working; respond 413/422, don't
  retry.
- `QRS-001` covers attacker-controlled bytes by design — it is safe to
  surface "invalid image" verbatim to end users.
