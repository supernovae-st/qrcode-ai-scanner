# 07 · The decode pipeline (normative behavior)

```
ImageInput (Encoded | Rgba8 | Luma8)
  → validate Limits
  → alpha flatten (pipeline v2 — 01-report § Alpha): transparent pixels
    composite over the declared background BEFORE luma/RGB extraction;
    opaque inputs take the historical path bit for bit
  → BT.601 luma (+ lazy RGB planes)
  → cap to max_engine_side (engines never see larger; corners rescale back)
  → S1 pyramid   ≤512px downscale (cheapest, often best on artistic)
  → S2 direct    full-resolution luma
  → S3 enhance   otsu · invert · contrast stretch · R/G/B channel planes
  → S4 deep      15 rungs (12 contrast boosts + 3 morphological closes)
                 + size × stretch × {otsu, invert} grid
  → S5 rescue    errors-and-erasures RS over undecoded grids (below)
  → (alpha auto only, zero detections) re-flatten over the OPPOSITE
    background → one more ladder walk within the same budget — both
    walks stay in the trace; `alpha.fallback_used` marks a rescue
  → merge        text-keyed, cross-engine consensus
  → score        contract v4 (04-score.md) — Full/Fast only
  → alpha placement envelope (Full only) — 01-report § Alpha
  → ScanReport
```

Normative properties:

1. **Fixed attempt order** — no RNG anywhere. Same input + config +
   version ⇒ same attempt sequence, bit for bit (per platform; see the
   determinism caveats in 04-score.md).
2. **Stage early-exit** — a stage that finds something still COMPLETES
   (within-stage consensus preserved); the ladder then stops. S5 runs only
   when S1-S4 found nothing.
3. **Budget + cancel checked between attempts** — an engine call is not
   interruptible; inputs are size-capped instead.
4. **Engines are panic-isolated** — a third-party decoder panic increments
   `trace.engine_panics` and the ladder continues.
5. **Photometric polarity travels with geometry** — a grid decoded on an
   INVERTED attempt is sampled/scored in that polarity and reports
   `meta.inverted: true`.

## S5 — the erasure rescue (what `engines: ["rescue"]` means)

Trigger: both engines failed on every attempt, but rqrr READ a grid + its
format info on at least one attempt (`get_raw_data` ok, `decode_to` err).
Up to 4 candidates are kept (deduped by version/ec/mask/polarity).

Per candidate:

1. **Unmask + de-interleave** the sampled bitstream (ISO 18004 §8.6 —
   exact gather, not the quirc round-slip).
2. **Per-codeword confidence**: worst |sample − threshold| margin over the
   codeword's 8 modules, sampled at module centers through the detection
   homography (center-submodule idea: Halftone QR, Chu et al. 2013).
3. **Erasure marking**: lowest-margin codewords (< 0.30 of half-span)
   become erasures, budget ≤ npar − p − 1 per block.
4. **Errors-and-erasures RS** (Forney 1965): erasure locator Γ, modified
   syndromes Ξ = S·Γ (Berlekamp-Massey on the e-shifted Ξ), combined
   locator Ψ = Λ·Γ, Chien roots, b=0 Forney magnitudes. Capacity law
   `e + 2t ≤ d − p` (ISO 18004 Annex B `p` protection respected).
5. **Hard verification**: corrected blocks must re-check to ZERO syndromes
   AND the bitstream must parse as a structurally valid mode-segment
   stream — refusal-biased: no surviving guess ever reaches the report.

Measured yield (v5-H, centered gray disk): engines die past 20% radius;
rescue decodes through 30% (≈2.2× the occlusion area) — the
logo-over-center artistic class.

Cost: rescue work is per-candidate and bounded; it adds nothing to scans
that decode normally.

## Engine roles

| | rxing (ZXing lineage) | rqrr (quirc lineage) | rescue (ours) |
|---|---|---|---|
| Robustness decode | ✓ (TryHarder + AlsoInverted) | ✓ | beyond-t recovery |
| Geometry (corners) | — | ✓ | from candidate |
| version/ec/mask | ec only | ✓ | ✓ |
| Raw bitstream | — | ✓ (feeds UEC + rescue) | consumes |
| FNC1/GS1 symbols | ✓ (`]Q3`/`]Q4`) | ✗ (rejects mode 0x5/0x9) | ✓ (parses FNC1 segments) |
| Kanji bytes | re-encodes UTF-8 | original SJIS | original SJIS |
