# 06 · Hints — the feedback loop

`report.hints` is the machine-actionable bridge of the
*generate → scan → act → regenerate* loop. Stable order; internally
tagged (`"hint"` discriminator). Parse leniently — new hints may appear.

| `hint` | Fields | Fires when | Action for the generator/UI |
|---|---|---|---|
| `fix_finder_pattern` | `corner` (0=TL · 1=TR · 2=BL) | a finder's 1:1:3:1:1 integrity < 0.5 (also caps score ≤40) | clear art off that corner |
| `restore_quiet_zone` | — | the ≥2-module border isn't clean (caps score ≤60) | add margin around the symbol |
| `increase_contrast` | — | contrast-axis survival ≤ 40% | darken modules / lighten background |
| `enlarge_modules` | — | resolution-axis survival ≤ 40% | bigger render or lower QR version |
| `reduce_art_texture` | — | blur-axis survival = 0 (dies at the mildest blur) | lighten texture over the data zone |
| `raise_error_correction` | `current` (`"l"`/`"m"`/`"q"`) | the score value before the UEC margin cap < 70 **or** a PUBLISHED UEC grade d/f, when EC < H | regenerate at a higher EC level |
| `low_correction_margin` | `errors`, `capacity` | the PUBLISHED UEC margin = 0 — the worst RS block consumed its ENTIRE budget | **distrust signal**: the decode may be a miscorrection; verify content out-of-band / regenerate |
| `alpha_background_dependent` | `placement` (`light_only` · `dark_only` · `mixed` · `none`) | a transparent design's placement envelope is narrower than `any` (01-report § Alpha) | pin a background layer in the artwork, or constrain placement to `alpha.envelope.safe_luma` |
| `add_background_plate` | `color` (`white` · `black`) | rides with `alpha_background_dependent` — the canonical remedy | add an opaque plate (rounded rectangle + quiet-zone margin) behind the symbol: robust everywhere, transparent look preserved around it |

## Consumer patterns

- **Verify-UI badge logic**: any of the first five hints → "improve your
  design" guidance with the specific fix. `low_correction_margin` →
  "decodes, but unreliable — regenerate" (treat as failure, not success).
- **Agent loop** (Nika): feed `hints` straight back into the generation
  prompt; the hints were designed to be actionable without image analysis.
- The score hints (the first seven rows) appear only when a score
  judgment completed: never with `score: null` — the `frame` profile, every
  axis skipped, a judgment the budget interrupted (`trace.budget_exhausted:
  true`) or a contained fault. The two alpha hints ride with the placement
  envelope (Full profile, transparent inputs), which runs whether or not the
  score did.
- A skipped `uec` check (04-score.md § Skipping checks) withholds the two
  margin-driven hints — `low_correction_margin` and the UEC path of
  `raise_error_correction` — while the measured margin still caps the
  score value.
