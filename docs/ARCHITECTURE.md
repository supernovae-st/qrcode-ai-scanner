# Architecture

> Core invariants and the decode pipeline. The score contract lives in
> [SCORING.md](SCORING.md); design rationale + research evidence in
> [plans/2026-06-11-v03-rebuild-design.md](plans/2026-06-11-v03-rebuild-design.md).

## Workspace

| Crate | Role |
|---|---|
| `qrcode-ai-scanner` | the core library — pure, sync, deterministic |
| `qrcode-ai-scanner-cli` | `qrscan` binary (JSON default · exit codes 0/1/2) |
| `qrcode-ai-scanner-node` | napi bindings (phase B) |
| `qrcode-ai-scanner-wasm` | browser wasm (phase C) |
| `xtask` | repo automation: gen-fixtures · corpus-report · baseline |

## Core invariants

1. **No QR found is `Ok`** with empty `detections` — `Err` only for real
   faults (`QRS-001..004` invalid input, `QRS-005` cancellation).
2. **Deterministic**: same bytes + same config + same versions ⇒ the same
   report bit-for-bit (trace wall-clock fields excepted). No RNG anywhere.
3. **Sync by design**: async belongs to the bindings.
4. **Engine-isolated**: third-party decoder panics are caught at the engine
   boundary (`catch_unwind`), counted in the trace, never propagated.
5. **Raw bytes are the truth**: charset (UTF-8 → Shift-JIS → Windows-1252)
   is resolved ONCE from raw payload bytes; `DecodedContent.raw` always
   carries the unmodified bytes.

## Decode pipeline

```
ImageInput (Encoded | Rgba8 | Luma8)
  → normalize        validate Limits · BT.601 luma · keep RGB planes lazily
  → ladder           deterministic stage sequence · budget + cancel between attempts
       S1 pyramid    ≤512px downscale attempt first (cheapest, often best)
       S2 direct     full-resolution luma
       S3 enhance    otsu · invert · contrast stretch · R/G/B channels
       S4 deep       12 boost rungs (v0.2 empirical known-good: resize ×
                     multiplicative contrast × light blur) + binarization grid
  → merge            cross-engine, keyed by raw payload bytes → engines consensus
  → score            contract v3 (see SCORING.md) — Full/Fast profiles only
  → ScanReport       versioned serde contract (snake_case · raw as base64)
```

Engines: rxing (ZXing lineage — TryHarder + AlsoInverted, QR-only feature
build) and rqrr (quirc lineage — geometry: corners, version, EC, mask, and
the raw sampled bitstream that feeds the synthetic UEC).

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
