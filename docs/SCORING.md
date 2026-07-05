# Score contract v3

> `Versions.score_contract = 3`. The composite, the axes, the caps, and the
> weights below are a published contract: changing any of them bumps the
> version. Output language: **validation**, never ISO *verification* — no
> calibrated optics are involved (geometry-class signals transfer to digital
> images; reflectance-class signals are relative).

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

## Structural caps

| Check | Cap | Why |
|---|---|---|
| finder integrity < 0.5 (worst of 3) | score ≤ 40 | #1 documented AI-art killer |
| quiet zone violated | score ≤ 60 | breaks locators in the wild |

Finder integrity = 1:1:3:1:1 pattern match in module space (0.0-1.0 per
corner, local threshold). Quiet zone = 2-module outer ring ≥80% lighter
than the symbol interior mean. Both need geometry (rqrr corners); absent
geometry ⇒ `structural: null`, no cap.

## Synthetic UEC (the margin)

The ISO 15415 *Unused Error Correction* computed from rqrr's own sampled
bitstream: zigzag replay → unmask → ISO 18004 §8.6 de-interleave →
per-block RS syndromes → Berlekamp-Massey degree = exact corrected-error
count `t`. `margin = 1 − 2t/d` (worst block) · grades A ≥0.62 · B ≥0.50 ·
C ≥0.37 · D ≥0.25 · F below.

Known limits (deliberate, documented): the ISO `p` misdecode-protection
codewords of the very low versions are not subtracted (v1-v2 margins read
marginally optimistic); margins reflect the *digital image*, not a print.

## Hints

Machine-actionable, stable order — the generate → scan → act → regenerate
loop: `fix_finder_pattern{corner}` · `restore_quiet_zone` ·
`increase_contrast` (contrast survival ≤40%) · `enlarge_modules`
(resolution survival ≤40%) · `reduce_art_texture` (dies at mildest blur) ·
`raise_error_correction{current}` (score &lt;70 **or** UEC grade D/F, when
EC < H) · `low_correction_margin{errors,capacity}` (UEC margin exactly 0:
the worst block consumed its entire correction budget — the miscorrection
signature; caught live on the zxing blackbox corpus where rqrr returned
"photography" for a "photograph" ground truth at 12/24 errors. Consumers
should treat the decoded content as unverified).

## ISO/IEC 15415 mapping — and what we honestly claim

ISO/IEC 15415 grades a 2D symbol on 8 parameters; the overall grade is the
LOWEST one, reported as `grade/aperture/wavelength` (e.g. `2.8/08/660`).
What this scanner measures maps onto that vocabulary as follows:

Since v0.3 the report carries an explicit **`score.iso15415` grade card**
(present whenever symbol geometry was measured): per-parameter
`{value, grade}` in the ISO bands, plus `overall` = the LOWEST parameter —
the ISO 15415 overall-grade rule.

| ISO 15415 parameter | This scanner | Fidelity |
|---|---|---|
| Unused Error Correction | `iso15415.unused_error_correction` (= `score.uec` margin) — same bands (A ≥62% · B ≥50 · C ≥37 · D ≥25) | **algorithmically equivalent** (RS syndromes, exact `t`) |
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

Weights and ramp intensities are v3 engineering constants, corpus-informed
but **not yet human-device calibrated**. The calibration pass (Modal
playbook: ~200 artistic codes × 2-3 phones, screen + paper) is phase D;
expect a `score_contract: 4` when it lands.
