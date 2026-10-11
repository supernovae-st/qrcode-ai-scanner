# Score contract v5

> `Versions.score_contract = 5`. The composite, the axes, the caps, and the
> weights below are a published contract: changing any of them bumps the
> version. Output language: **validation**, never ISO *verification* — no
> calibrated optics are involved (geometry-class signals transfer to digital
> images; reflectance-class signals are relative). The normative text is
> [`spec/04-score.md`](../spec/04-score.md).

A judgment is whole or absent. A present `score` ran its calibration and
every planned stress cell, so no wall-clock budget moves it (only a knee
label, `refined_failed_at`, may be missing — and `trace.budget_exhausted`
then says so). A judgment the budget interrupts is `score: null` with
`trace.budget_exhausted: true`: "not judged", never a zero.

## Shape

```jsonc
"score": {
  "value": 87,                  // 0-100 composite
  "grade": "excellent",         // interpretation band
  "axes": [ { "axis": "resolution", "passed": 4, "total": 5 }, ... ],
  "structural": { "finder_integrity": [1.0, 0.96, 0.88], "quiet_zone_ok": true },
  "uec": { "margin": 0.85, "grade": "a",
           "worst_block_errors": 1, "worst_block_capacity": 18 }
}
```

## Survival axes

Each axis is an ordered intensity ramp decoded with a FAST subset relative
to the symbol's own decode class: direct + otsu, plus the one deep rung the
unstressed baseline needed, when it needed one (`CellProbe` — "fragile" is
never conflated with "undecodable"; the score still measures *margin*, not
the deep ladder's recovery power). Ramps stop at the first failure (the
knee). Full depth = 5 cells/axis · Fast profile = 2 · Frame = no scoring.

| Axis | Weight | Ramp |
|---|---|---|
| resolution | 22 | downscale 358 → 256 → 179 → 128 → 90 px |
| blur | 18 | gaussian σ 0.5 → 2.5 |
| contrast | 15 | multiplicative 0.7 → 0.2 |
| perspective | 20 | tilt 10° → 42° |
| rotation | 10 | 10° → 50° |
| lighting | 15 | shadow ×2 · glare blob · exposure ±60 (unordered set) |

Perspective/rotation/lighting exist because they are the documented blind
spot of naive scorers (BoofCV categories · DiffQRCoder WACV 2025): artistic
codes erode the *grid-estimation* margin first.

## Caps

| Check | Cap | Why |
|---|---|---|
| finder integrity < 0.5 (worst of 3) | score ≤ 40 | #1 documented AI-art killer |
| quiet zone violated | score ≤ 60 | breaks locators in the wild |
| measured UEC margin `m` < 0.5 | score ≤ round(40 + 120·m) — 0.5 → 100 · 0.25 → 70 · 0.0 → 40 | a consumed RS budget is invisible to the stress axes; a decode at the RS limit caps at 40, but on v1, v2-L and v3-L an errors-only decode at the acceptance limit caps at 47 to 91 (known limits below) |

Finder integrity = 1:1:3:1:1 pattern match in module space (0.0-1.0 per
corner, local threshold). Quiet zone = 2-module outer ring ≥80% lighter
than the symbol interior mean. Both need geometry (rqrr corners); absent
geometry ⇒ `structural: null`, no cap.

## Synthetic UEC (the margin)

The ISO 15415 *Unused Error Correction* computed from rqrr's own sampled
bitstream: zigzag replay → unmask → ISO 18004 §8.6 de-interleave →
per-block RS syndromes → Berlekamp-Massey degree = exact corrected-error
count `t`. `margin = 1 − 2t/d` (worst block, `d` its parity codewords;
ISO uses `d − p`, see the known limits) · grades A ≥0.62 · B ≥0.50 ·
C ≥0.37 · D ≥0.25 · F below.

The stream is the one of the orientation the decode verified (a mirrored
symbol is measured transposed, with that orientation's EC level and mask),
and a margin is read only when every block of it error-corrects errors-only
under that format. A stream that does not has no `uec`, no cap and no
margin hint — absent, never a guess. A stream the S5 rescue recovered
needed erasures, so it keeps its errors-only count: with `rescue` among
the engines, `worst_block_errors` (and the hint's `errors`) is a distrust
marker, typically margin 0, not a count of corrected errors.

The margin caps the value whether or not `uec` is published: skipping the
`uec` check (`score_skip_checks`) withholds `score.uec`, the ISO
`unused_error_correction` parameter and the two margin hints, while
`value`, `grade` and the ISO `overall` never move.

Known limits (deliberate, documented): the ISO `p` misdecode-protection
codewords are not subtracted. The six shapes that reserve them — v1 at
every EC level, v2-L and v3-L — read above ISO's margin once a block holds
an error: for an errors-only decode at the acceptance limit, where ISO
reads 0 (grade F), the margin reads 0.059 (v1-H) to 0.43 (v1-L) and caps
the score at 47 to 91 instead of 40, and `low_correction_margin` does not
fire (the table is in [`spec/04-score.md`](../spec/04-score.md)
§ Synthetic UEC). Margins reflect the *digital image*, not a print.

## Hints

Machine-actionable, stable order — the generate → scan → act → regenerate
loop: `fix_finder_pattern{corner}` · `restore_quiet_zone` ·
`increase_contrast` (contrast survival ≤40%) · `enlarge_modules`
(resolution survival ≤40%) · `reduce_art_texture` (dies at mildest blur) ·
`raise_error_correction{current}` (the score before the margin cap &lt;70
**or** a published UEC grade D/F, when EC < H) ·
`low_correction_margin{errors,capacity}` (the published UEC margin exactly 0:
the worst block's errors spent all its parity — the miscorrection
signature, caught live on the zxing blackbox corpus where rqrr returned
"photography" for a "photograph" ground truth at 12/24 errors — or, with
`rescue` among the engines, the distrust marker above; never for an
errors-only decode at the acceptance limit of v1, v2-L or v3-L. Consumers
should treat the decoded content as unverified). Score hints come only
with a completed judgment.

## ISO/IEC 15415 mapping — and what we honestly claim

ISO/IEC 15415 grades a 2D symbol on 8 parameters; the overall grade is the
LOWEST one, reported as `grade/aperture/wavelength` (e.g. `2.8/08/660`).
What this scanner measures maps onto that vocabulary as follows:

Since v0.3 the report carries an explicit **`score.iso15415` grade card**
(present whenever symbol geometry was measured): per-parameter
`{value, grade}` in the ISO bands, plus `overall` = the LOWEST measured
parameter — the ISO 15415 overall-grade rule (an `unused_error_correction`
withheld by a skipped `uec` check still counts).

| ISO 15415 parameter | This scanner | Fidelity |
|---|---|---|
| Unused Error Correction | `iso15415.unused_error_correction` (= `score.uec` margin) — same bands (A ≥62% · B ≥50 · C ≥37 · D ≥25) | **equivalent except the Annex B reserve `p`** (RS syndromes, exact `t`; divides by `d`, not ISO's `d − p`, so v1, v2-L and v3-L read higher — known limits above) |
| Symbol Contrast | `iso15415.symbol_contrast` — `(R₉₈−R₂)/255` over module means; A ≥70% · B ≥55 · C ≥40 · D ≥20 | measured on uncalibrated luma (percentiles, not reflectance extremes) |
| Modulation | `iso15415.modulation` — robust min of per-module `2·\|R−GT\|/SC`; A ≥0.50 · B ≥0.40 · C ≥0.30 · D ≥0.20 | simplified (no notional-UEC iteration) |
| Axial Nonuniformity | `iso15415.axial_nonuniformity` — `\|X̄−Ȳ\|/mean` from corners; A ≤0.06 · B ≤0.08 · C ≤0.10 · D ≤0.12 | measured; photo perspective reads as ANU (ISO assumes flat capture) |
| Fixed Pattern Damage | `iso15415.fixed_pattern_damage` — worst finder integrity, quiet-zone violation caps at D | approximation (no clock-track subtest) |
| Decode | `detections` non-empty (grade A implicit — only decoded symbols are graded) | equivalent in spirit (two engines, not THE reference decoder) |
| Grid Nonuniformity | — | **not measurable** from 4-corner geometry — reported absent, never faked |
| Reflectance Margin | — | **not measurable** without calibrated reflectance — reported absent |

**Certification honesty (don't oversell this):** a conformant ISO 15415
grade requires calibrated reflectance, controlled 45° illumination at a
stated wavelength, a defined synthetic aperture, and hardware conformance
per ISO/IEC 15426-2 — properties of a verifier device, impossible for
software grading arbitrary images (the grade string itself encodes
aperture + wavelength). This scanner's output is **ISO-informed
diagnostics** for process feedback — the same positioning Cognex calls
"standards-based grading" — never a certified verification grade. If a
customer needs a 15415 certificate (e.g. retail compliance audits), they
need a hardware verifier; this scanner tells them *what to fix first*.

GS1 conformance is different: it is a **syntax** property (element-string
grammar, AI formats, check digits, Digital Link path order) and IS fully
checkable in software — that's the `payload.conformant` + `issues` verdict
(see `gs1.rs`; criteria cited per issue: GenSpecs §7.2.7, §3.4, §7.8.4,
DL URI Syntax §4.4/§4.5).

## Calibration status

Weights and ramp intensities are engineering constants (unchanged since
v3), corpus-informed but **not yet human-device calibrated**. A calibration
pass (~200 artistic codes × 2-3 phones, screen + paper) would change them,
and with them the score contract version.
