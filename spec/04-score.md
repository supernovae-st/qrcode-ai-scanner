# 04 · Score contract v5

The score answers ONE question: **how much margin does this code have
before it stops scanning in the real world?** It is VALIDATION, never ISO
verification (no calibrated optics — see the honesty section).

A judgment is whole or absent: a present `score` ran its calibration and
every planned stress cell, so its value never depends on the wall-clock
budget (01-report.md § `score: null`, 02-profiles.md § Budget semantics).

## Composite value (0-100)

Weighted survival across six stress axes; ramps stop at the first failed
cell (the knee). The structural caps and the UEC margin cap then bound the
value (§ Caps). Weights are contract constants:

| Axis | Weight | Ramp (Full depth = 5 cells) |
|---|---|---|
| `resolution` | 22 | downscale to 358 · 256 · 179 · 128 · 90 px |
| `blur` | 18 | gaussian σ 0.5 · 1.0 · 1.5 · 2.0 · 2.5 |
| `contrast` | 15 | multiplicative crush ×0.7 · 0.55 · 0.4 · 0.3 · 0.2 |
| `perspective` | 20 | tilt 10° · 18° · 26° · 34° · 42° |
| `rotation` | 10 | 10° · 20° · 30° · 40° · 50° |
| `lighting` | 15 | defect SET (soft/hard shadow · glare · over/under-exposure) — unordered: no knee-exit; depth still picks the cell subset. The glare blob lands on the DATA region (symbol centre), never a finder: a finder-kill is unsurvivable by design (the pre-0.8 placement capped every perfect render at 4/5), while a data-region glare is absorbed or not by the error-correction budget — measurable and actionable |

Every `score.axes[]` entry carries `failed_at` — the wire label of the first
failed cell (`"blur 2.5"`, `"glare"`, `"128px"`…), `null` when every cell
passed. A lost point always names its cell: surfaces render the reason, not
just the count.

Survival is measured **relative to the symbol's own decode class**
(`CellProbe`): an artistic symbol that only decodes through a deep rung is
probed with that rung — "fragile" is never conflated with "undecodable".

### Skipping axes (integration config)

A host may exclude axes from scoring (`ScanConfig.score_skip_axes` · wasm
`score_skip_axes`, wire names): skipped axes never run — their stress cells
are never built — and the composite **renormalizes over the weights of the
axes that ran** (with all six, the divisor is the same 100 as ever). The
report self-describes: `score.axes` carries only the axes that ran, and
axis-derived hints from skipped axes structurally cannot fire. Skipping ALL
axes yields no `score` at all — an axis-less value would be fiction. The
canonical case: a generated preview has no capture geometry, so builder
integrations skip `perspective` + `rotation`; photo-input surfaces keep the
full six.

### Skipping checks (integration config)

The same seam exists for report SECTIONS (`ScanConfig.score_skip_checks` ·
bindings `score_skip_checks` / `scoreSkipChecks`, section names `uec` |
`iso15415` | `alpha_envelope`): a skipped section is withheld and the wire
carries `null` for it. Skipping checks is surface truth, never score
surgery: `score.value`, `score.grade` and the ISO `overall` **never move**.

- `uec`: the margin is still MEASURED — it caps the value whether or not
  the section is published (§ Caps). Withheld are `score.uec`, the ISO
  `unused_error_correction` parameter (reported `null`, never faked; its
  grade still counts in the ISO `overall`) and the two margin-driven hints:
  `low_correction_margin` and the UEC path of `raise_error_correction`.
  That hint's `value < 70` gate reads the composite BEFORE the margin cap,
  so a skipped margin never raises it either.
- `iso15415`: the parameter sweep never runs.
- `alpha_envelope`: the background sweep never runs and the
  `alpha_background_dependent` hint cannot fire (the flatten itself is
  untouched — 01-report § Alpha).

The canonical case: a host that displays neither the correction margin nor
the ISO parameters (the builder panel) skips both; verifier-style surfaces
keep them.

### `weights_run` — the honesty integer

`score.weights_run` = Σ contract weights of the axes that RAN (100 = the
full six-axis contract · 70 with `perspective`+`rotation` skipped). A
score on skipped axes SAYS how much contract stands behind it: two 85s
with different `weights_run` are different promises, and panels can render
"85 — on 70% of the contract" without arithmetic. Only skipped axes lower
it — never the budget, which makes a judgment absent instead. Absent on
pre-0.9 reports (parse leniently, default 0).

### Presets — the drift-proof postures

`score_preset` (bindings `scorePreset` / `--score-preset`) names the two
canonical integration postures instead of N hand-built skip lists:
`design` = generated previews (skips `perspective`+`rotation`, KEEPS
`lighting` — the glare cell measures the design's own fragility, not the
capture) · `capture` = the full six. Pure sugar over `score_skip_axes`;
passing both rejects loudly (silent precedence would be the worst drift).

### The bisected knee — `axes[].refined_failed_at`

At Full depth, an ordered ramp that dies gains ONE bisection probe
between the knee cell and its lower neighbour (the unstressed value for
a knee at cell 0): the wire carries the tightest TESTED failing
intensity ("blur 2.25" when the midpoint fails · the knee cell's label
when it holds). Informational by contract — the composite never reads
it; the key is ABSENT when no refinement ran (Reduced depth · no knee ·
lighting's unordered set · budget cut). Bisections run after every planned
cell: a budget cut drops only the label, never the score, and sets
`trace.budget_exhausted`. The generator loop finally sees sub-cell
progress.

### Caps (applied after the weighted sum)

| Condition | Cap |
|---|---|
| any finder integrity < 0.5 | value ≤ **40** + `fix_finder_pattern` hint |
| quiet zone violated | value ≤ **60** + `restore_quiet_zone` hint |
| measured UEC margin `m` < 0.5 | value ≤ **round(40 + 120·m)** — continuous: 0.5 → 100 · 0.25 → 70 · 0.0 → 40 |

The margin cap reads the measured margin (§ Synthetic UEC), published or
skipped: a consumed Reed-Solomon budget is invisible to the stress axes.
Where a symbol reserves no misdecode-protection codewords, a decode at the
RS limit caps at 40 and is never a pass; on version 1, v2-L and v3-L an
errors-only decode at the acceptance limit caps at 47 to 91 instead
(§ Synthetic UEC, the Annex B reserve).

### Grade bands

`excellent` ≥80 · `good` 70-79 · `acceptable` 60-69 · `fair` 40-59 ·
`poor` <40.

## Synthetic UEC (`score.uec`)

ISO 15415 *Unused Error Correction* computed from the geometry engine's own
sampled bitstream: zigzag replay → unmask → ISO 18004 §8.6 de-interleave →
per-block RS syndromes → Berlekamp-Massey degree = EXACT corrected-error
count `t`. `margin = 1 − 2t/d` over the worst block, `d` its parity
codewords.

The stream measured is that of the orientation the decode verified — for a
symbol decoded mirrored, the transposed grid, with that orientation's
`ec_level` and `mask` (01-report.md § Detection) — so a clean mirrored code
reads the same margin as its upright twin. A margin is measured only when every block
of that stream error-corrects errors-only (`2t ≤ d`, every error located
inside the block) under the detection's version, EC level and mask:
otherwise there is no `uec` section, no margin cap and no margin hint —
absent, never a guess. A detection whose `engines` include `rescue` is
measured without that check: S5 corrects with erasures, so errors alone
need not correct its stream, and `t` is the errors-only count as read. Its
`worst_block_errors` — and the `errors` of its `low_correction_margin`
hint — is then a distrust marker, typically margin 0
(`examples/rescued-occluded.json` reads 11 of 22), not a count of
corrected errors.

Bands (ISO): `a` ≥0.62 · `b` ≥0.50 · `c` ≥0.37 · `d` ≥0.25 · `f` <0.25.

**The Annex B reserve.** ISO/IEC 15415 and 18004 measure this parameter
against `d − p`, where `p` is the misdecode-protection count of ISO/IEC
18004 Annex B (Table 9): 3 on v1-L, 2 on v1-M and v2-L, 1 on v1-Q, v1-H
and v3-L, 0 on every other version and EC level. The margin does not
subtract `p`, and neither does `iso15415.unused_error_correction`, which
repeats it: where `p` is 0 the two agree, and on those six shapes a block
with errors reads above ISO's value. The acceptance subtracts `p`
(07-pipeline.md § Acceptance), so at its limit for a word corrected
errors-only — ⌊(d − p)/2⌋ errors in the block, where ISO reads 0, grade
`f` — the published numbers are:

| Shape | `d` | `p` | Errors at the limit | `margin` | `grade` | Cap |
|---|---|---|---|---|---|---|
| v1-L | 7 | 3 | 2 | 3/7 ≈ 0.43 | `c` | 91 |
| v1-M, v2-L | 10 | 2 | 4 | 1/5 | `f` | 64 |
| v1-Q | 13 | 1 | 6 | 1/13 ≈ 0.077 | `f` | 49 |
| v3-L | 15 | 1 | 7 | 1/15 ≈ 0.067 | `f` | 48 |
| v1-H | 17 | 1 | 8 | 1/17 ≈ 0.059 | `f` | 47 |

On these shapes neither the cap of 40 nor `low_correction_margin` marks an
errors-only decode at the acceptance limit. A stream that error-corrects
errors-only reaches margin 0 there only on v1-M and v2-L, at 5 errors: a
word past the reserve, published only when another read of its content
keeps a margin of zero or more (the detection measures the stream of its
earliest read that carries one, 07-pipeline.md property 6). On v1-L, v1-Q,
v1-H and v3-L `d` is odd, and `2t ≤ d` leaves a parity codeword unspent.
Otherwise the hint comes there only from a rescued read — never on v1-L,
whose detections carry no stream that counts more than 3 errors (margin
1/7 or more).

A published `margin` of 0 raises `low_correction_margin` (06-hints.md):
treat the content as unverified. On a stream that error-corrects
errors-only it means the worst block's errors spent every parity codeword
(`2t = d`). This is a distrust signal, not proof of a miscorrection; where
`p` is 0, it is the acceptance limit itself. On a rescued read it is the
distrust marker above.

## ISO 15415-informed grade card (`score.iso15415`)

Present whenever symbol geometry was measured. Per-parameter
`{value, grade}` in the official ISO bands; `overall` = the LOWEST
measured parameter (the ISO rule) — an `unused_error_correction` withheld
by `score_skip_checks: ["uec"]` still counts, so a skip never moves it.

| Parameter | Measured as | Bands |
|---|---|---|
| `symbol_contrast` | (p98−p2)/255 over module means | a ≥0.70 · b ≥0.55 · c ≥0.40 · d ≥0.20 |
| `modulation` | robust min (p5) of per-module 2·\|R−GT\|/SC | a ≥0.50 · b ≥0.40 · c ≥0.30 · d ≥0.20 |
| `axial_nonuniformity` | \|X̄−Ȳ\|/mean of axis pitches | a ≤0.06 · b ≤0.08 · c ≤0.10 · d ≤0.12 |
| `fixed_pattern_damage` | worst finder integrity; quiet-zone violation caps at `d` | a ≥0.95 · b ≥0.90 · c ≥0.80 · d ≥0.70 |
| `unused_error_correction` | the UEC margin (above; `p` not subtracted) | ISO UEC bands |

## The honesty line (do not oversell)

A conformant ISO 15415 grade requires calibrated reflectance, controlled
45° illumination at a stated wavelength, a defined synthetic aperture, and
ISO/IEC 15426-2 hardware conformance — properties of a verifier DEVICE.
This scanner's output is **standards-based diagnostics** for process
feedback. Parameters that need the hardware (Grid Nonuniformity,
Reflectance Margin) are reported ABSENT, never faked. GS1 conformance is
different — it is pure syntax and IS fully software-checkable
([05-payloads.md](05-payloads.md)).

## Determinism

Same input + same configuration + same scanner build ⇒ same score, as far
as the decode engines are deterministic. The documented bounds:

1. A wall-clock budget makes a judgment ABSENT, never partial: a judgment
   it interrupts publishes `score: null` with `trace.budget_exhausted:
   true`, and a present score is the unbudgeted one (a knee label aside).
2. Cross-platform libm transcendentals may differ in the last ulp, so a
   cell sitting exactly on a knee can flip between platforms.
3. A known engine exception until it is fixed upstream: rxing's PDF417
   decoder (0.9.1–0.9.3) breaks codeword-confidence ties in randomized
   `HashMap` order, so a PDF417 symbol with a stress cell near a decode
   threshold can score differently between runs (observed 62 ↔ 57 on
   `fixtures/symbology/pdf417.png`), and a marginal PDF417 input can
   publish a different stage trace. QR-family symbols and every other
   symbology are unaffected.

Cancellation is not a bound: a cancelled scan is `QRS-005`.

## Frame sensitivity (characterized, not hidden)

The composite is a property of the FRAME, not of the symbol alone. Two
documented consequences:

- **Dilution** — a small symbol in a large frame scores lower (the stress
  base downscales the whole frame to ≤512px, shrinking the symbol's
  effective module size). Monotone and intended: a QR occupying 20% of a
  poster IS harder to scan from afar.
- **Quiet-ring phase** — near the resolution knee, the perspective cells
  (which warp the whole frame) can flip with small quiet-ring changes:
  on identical styled symbol pixels, perspective reads 5/5 bare vs 2/5
  with a +4px white ring (the 26° cell · axis weight 20 × 3/5 = 12
  composite points; the published pair reads 85 vs 70, each frame's RS
  margin capping too), while pure-black comfortable-margin symbols do not
  move. Sentinel: `quiet_ring_shifts_perspective_cells_near_the_knee`
  (`scan_integration.rs`) over the committed
  `fixtures/degraded/quiet-ring-phase-*.png` pair.

Comparing two renders honestly therefore means comparing at equal frame:
same canvas, same ring. Cross-engine parity batteries that deliver
different canvas sizes for the same content are comparing two frames, not
two symbols.
