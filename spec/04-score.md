# 04 · Score contract v3

The score answers ONE question: **how much margin does this code have
before it stops scanning in the real world?** It is VALIDATION, never ISO
verification (no calibrated optics — see the honesty section).

## Composite value (0-100)

Weighted survival across six stress axes; ramps stop at the first failed
cell (the knee). Weights are contract constants:

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
`iso15415` | `alpha_envelope`): a skipped section is never computed — no
UEC bitstream replay, no ISO parameter sweep, no background sweep — and
the wire carries `null` for it. Absence cascades honestly: without `uec`,
the two UEC-driven hints (`thin-margin` priority of
`raise_error_correction`, and `low_correction_margin`) structurally cannot
fire; a still-present `iso15415` block reports its
`unused_error_correction` parameter as `null`, never a faked value; and
without `alpha_envelope` the `alpha_background_dependent` hint cannot fire
(the flatten itself is untouched — 01-report § Alpha). The composite
`score.value` is axis-based and **does not move** — skipping checks is
surface truth, not score surgery. The canonical case: a host that displays
neither the correction margin nor the ISO parameters (the builder panel)
skips both; verifier-style surfaces keep them.

### `weights_run` — the honesty integer

`score.weights_run` = Σ contract weights of the axes that RAN (100 = the
full six-axis contract · 70 with `perspective`+`rotation` skipped). A
partial score SAYS how much contract stands behind it: two 85s with
different `weights_run` are different promises, and panels can render
"85 — on 70% of the contract" without arithmetic. Absent on pre-0.9
reports (parse leniently, default 0).

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
lighting's unordered set · budget cut). The generator loop finally sees
sub-cell progress.

### Structural caps (applied after the weighted sum)

| Condition | Cap |
|---|---|
| any finder integrity < 0.5 | value ≤ **40** + `fix_finder_pattern` hint |
| quiet zone violated | value ≤ **60** + `restore_quiet_zone` hint |

### Grade bands

`excellent` ≥80 · `good` 70-79 · `acceptable` 60-69 · `fair` 40-59 ·
`poor` <40.

## Synthetic UEC (`score.uec`)

ISO 15415 *Unused Error Correction* computed from the geometry engine's own
sampled bitstream: zigzag replay → unmask → ISO 18004 §8.6 de-interleave →
per-block RS syndromes → Berlekamp-Massey degree = EXACT corrected-error
count `t`. `margin = 1 − 2t/d` over the worst block.

Bands (ISO): `a` ≥0.62 · `b` ≥0.50 · `c` ≥0.37 · `d` ≥0.25 · `f` <0.25.

`margin = 0` means the worst block consumed its ENTIRE correction budget —
the Reed-Solomon miscorrection signature → the `low_correction_margin`
hint fires and the content should be treated as unverified.

## ISO 15415-informed grade card (`score.iso15415`)

Present whenever symbol geometry was measured. Per-parameter
`{value, grade}` in the official ISO bands; `overall` = the LOWEST
parameter (the ISO rule).

| Parameter | Measured as | Bands |
|---|---|---|
| `symbol_contrast` | (p98−p2)/255 over module means | a ≥0.70 · b ≥0.55 · c ≥0.40 · d ≥0.20 |
| `modulation` | robust min (p5) of per-module 2·\|R−GT\|/SC | a ≥0.50 · b ≥0.40 · c ≥0.30 · d ≥0.20 |
| `axial_nonuniformity` | \|X̄−Ȳ\|/mean of axis pitches | a ≤0.06 · b ≤0.08 · c ≤0.10 · d ≤0.12 |
| `fixed_pattern_damage` | worst finder integrity; quiet-zone violation caps at `d` | a ≥0.95 · b ≥0.90 · c ≥0.80 · d ≥0.70 |
| `unused_error_correction` | the UEC margin (above) | ISO UEC bands |

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

Same input + same depth ⇒ same score, always — modulo two documented
bounds: a wall-clock budget can cut cells (machine-dependent), and
cross-platform libm transcendentals may differ in the last ulp (a cell
sitting exactly on a knee can flip between platforms).

## Frame sensitivity (characterized, not hidden)

The composite is a property of the FRAME, not of the symbol alone. Two
documented consequences:

- **Dilution** — a small symbol in a large frame scores lower (the stress
  base downscales the whole frame to ≤512px, shrinking the symbol's
  effective module size). Monotone and intended: a QR occupying 20% of a
  poster IS harder to scan from afar.
- **Quiet-ring phase** — near the resolution knee, the perspective cells
  (which warp the whole frame) can flip with small quiet-ring changes:
  identical styled symbol pixels measured 100 bare vs 88 with a +4px white
  ring (perspective 5/5 → 2/5 at the 26° cell · axis weight 20 × 3/5 = 12
  composite points), while pure-black comfortable-margin symbols do not
  move. Sentinel: `quiet_ring_shifts_perspective_cells_near_the_knee`
  (`scan_integration.rs`) over the committed
  `fixtures/degraded/quiet-ring-phase-*.png` pair.

Comparing two renders honestly therefore means comparing at equal frame:
same canvas, same ring. Cross-engine parity batteries that deliver
different canvas sizes for the same content are comparing two frames, not
two symbols.
