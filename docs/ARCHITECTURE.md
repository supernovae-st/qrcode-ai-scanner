# Architecture

> Core invariants and the decode pipeline. The score contract lives in
> [SCORING.md](SCORING.md).

## Workspace

| Crate | Role |
|---|---|
| `qrcode-ai-scanner` | the core library — pure, sync, deterministic |
| `qrcode-ai-scanner-cli` | `qrscan` binary (JSON default · exit codes 0/1/2/3) |
| `qrcode-ai-scanner-node` | napi bindings (phase B) |
| `qrcode-ai-scanner-wasm` | browser wasm (phase C) |
| `xtask` | repo automation: gen-fixtures · corpus-report · baseline |

Workspace-EXCLUDED (foreign toolchains build them; `cargo … --workspace`
skips them by design — see the root `Cargo.toml` comment):

| Crate / tree | Role |
|---|---|
| `qrcode-ai-scanner-py` | PyO3/maturin wheel (PyPI) |
| `qrcode-ai-scanner-uniffi` | UniFFI wrapper behind Kotlin (JitPack) + Swift (SwiftPM) |
| `bindings/flutter/rust` | flutter_rust_bridge crate inside the pub.dev package |

## Core invariants

1. **No QR found is `Ok`** with empty `detections` — `Err` only for real
   faults (`QRS-001..004` invalid input, `QRS-005` cancellation).
2. **Deterministic**: same bytes + same config + same versions ⇒ the same
   report bit-for-bit (trace wall-clock fields excepted) for an unbounded
   scan, or whenever `trace.budget_exhausted` is `false`; a budget cut is
   flagged, never partial. No RNG anywhere (rxing's PDF417 decoder breaks
   its ties in randomized order: the one dependency exception).
3. **Sync by design**: async belongs to the bindings.
4. **Engine-isolated**: third-party decoder panics are caught at the engine
   boundary (`catch_unwind`), counted in the trace, never propagated.
5. **Raw bytes are the truth**: for the QR family `DecodedContent.raw`
   carries the original payload bytes (designators excluded, never an
   engine's text); other symbologies carry their decoding engine's reading
   re-encoded (spec/01 § raw). What a QR symbol declares — ECI designators
   and where each takes effect, the FNC1 position, Kanji/Hanzi segments —
   is observed by the scanner's own parser over error-corrected codewords
   wherever it can parse the stream, and the text is decoded ONCE
   from the bytes (spec/01 § Content): located designators decide it, else
   the observed segments, the decoding engine's mode-aware reading or the
   legacy order (UTF-8 → `Shift_JIS` → Windows-1252). `charset` says how;
   `unknown` marks a text that is not a complete decoding.
6. **Distinct contents, not merged text**: every read of a walk is collected
   and reconciled once at the end — identical reads fold; reads of one
   content join by their account and where they lie (spec/07 property 6).
   Text is never an identity.
7. **Refuse what the evidence contradicts**: a malformed stream is
   refused whichever engine reads it; a QR Code content whose every
   measured correction margin is negative (past the Annex B reserve, or an
   `rqrr` decode of a grid the scanner cannot error-correct, at any
   version) and an invention-prone barcode read no independent read
   confirmed are not published (spec/07 § Acceptance). Reads with no
   measured margin — every Micro QR and rMQR read, any other QR Code read
   no parse measured — are published as read, and a correction inside the
   reserve can still be wrong (`low_correction_margin` flags a measured
   block whose errors spent all its parity — which no errors-only
   correction inside the reserve does on version 1, v2-L and v3-L, spec/04
   § Synthetic UEC).

## Decode pipeline

```
ImageInput (Encoded | Rgba8 | Luma8)
  → normalize        validate Limits · BT.601 luma · keep RGB planes lazily
  → ladder           deterministic stage sequence · budget + cancel between attempts
       S1 pyramid    ≤512px downscale attempt first (cheapest, often best)
       S2 direct     full-resolution luma
       S3 enhance    otsu · invert · contrast stretch · R/G/B channels
       S4 deep       15 deep rungs: 12 boosts (v0.2 empirical known-good:
                     resize × multiplicative contrast × light blur) + 3
                     morphological closes (blob/dot pixel styles — gaps
                     between sub-module blobs read as noise until closed)
                     + binarization grid
       S5 rescue     engines failed but a grid was read: errors-and-erasures
                     RS (Forney 1965) with confidence-marked erasures —
                     e + 2t ≤ d − p, twice the budget where the art lives,
                     two parity codewords left unspent per erased block
                     per attempt: engines → re-derived grids (candidate
                     parses) → the re-read at rxing's corner points →
                     refusals → the read set → confirmation decodes for
                     held reads
  → acceptance       correction margins and independent confirmations,
                     weighed over the whole walk (spec/07 § Acceptance)
  → reconcile        the walk's read set → the distinct contents the scanner
                     can tell apart; each content read once → engines consensus
  → score            contract v5 (see SCORING.md) — Full/Fast profiles only
  → ScanReport       versioned serde contract (snake_case · raw as base64)
```

Engines: rxing (ZXing lineage — TryHarder + AlsoInverted, QR-only feature
build; its result points frame the scanner's re-read, `engine::locate`) and
rqrr (quirc lineage — geometry: corners, version, EC, mask, and the raw
sampled bitstream of the orientation that verified, which feeds the
synthetic UEC). Neither decides text: the scanner error-corrects and
parses the streams itself (`rescue::bitstream`, the one segment parser).

## Error model

`ScanError` — thiserror 2 + miette diagnostics, stable wire codes:

| Code | Meaning |
|---|---|
| QRS-001 | unsupported or corrupt image |
| QRS-002 | dimensions exceed `Limits::max_dimension` |
| QRS-003 | pixel count exceeds `Limits::max_pixels` |
| QRS-004 | raw buffer length mismatch |
| QRS-005 | cooperative cancellation |

Codes are never reused or renumbered (pinned by tests).

## Quality gates

- 0 `unwrap`/`expect` in `src/` (workspace lints deny — structural).
- All pub enums and contract structs `#[non_exhaustive]`.
- insta snapshots pin the serde schema; pinning tests pin error codes,
  grade bands, profile presets, and the UEC version table.
- `cargo xtask corpus-report` — derived numbers are never hand-typed.
- CI: fmt · clippy (`-D warnings`) · nextest 3 OS · wasm32 shape check ·
  cargo-deny · corpus artifact. Weekly: cargo-mutants + 15min fuzz.
