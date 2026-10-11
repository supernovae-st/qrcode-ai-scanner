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
| `low_correction_margin` | `errors`, `capacity` | the PUBLISHED UEC margin = 0 (04-score.md § Synthetic UEC), not the acceptance margin of 07-pipeline.md: the worst RS block's errors spent all its parity — or, with `rescue` among the engines, its errors-only count reached it | **distrust signal**: the decode may be a miscorrection; verify content out-of-band / regenerate |
| `alpha_background_dependent` | `placement` (`light_only` · `dark_only` · `mixed` · `none`) | a transparent design's placement envelope is narrower than `any` (01-report § Alpha) | pin a background layer in the artwork, or constrain placement to `alpha.envelope.safe_luma` |
| `add_background_plate` | `color` (`white` · `black`) | rides with `alpha_background_dependent` — the canonical remedy | add an opaque plate (rounded rectangle + quiet-zone margin) behind the symbol: robust everywhere, transparent look preserved around it |

## Consumer patterns

- **Verify-UI badge logic**: any of the first five hints → "improve your
  design" guidance with the specific fix. `low_correction_margin` →
  "decodes, but unreliable — regenerate" (treat as failure, not success).
- `low_correction_margin` carries the worst block's `errors` and its
  parity `capacity`; with `rescue` among the engines, `errors` is the
  errors-only count of the stream — a distrust marker, not errors
  corrected. Its absence is no proof on version 1, v2-L and v3-L: an
  errors-only decode at the acceptance limit there reads a UEC margin
  above 0 and raises no hint. On those symbols the hint comes from a
  rescued read (never on v1-L) or, on v1-M and v2-L, from a word past the
  reserve that another read of its content admitted (04-score.md
  § Synthetic UEC).
- **Agent loop** (Nika): feed `hints` straight back into the generation
  prompt; the hints were designed to be actionable without image analysis.
- The score hints (the first seven rows) appear only when a score
  judgment completed: never with `score: null` — the `frame` profile, every
  axis skipped, a judgment the budget interrupted (`trace.budget_exhausted:
  true`) or a contained fault. The two alpha hints ride with the placement
  envelope (Full profile, transparent inputs), which runs whether or not the
  score did, except after a contained fault of the score computation: the
  envelope is then absent, and so are both alpha hints.
- A skipped `uec` check (04-score.md § Skipping checks) withholds the two
  margin-driven hints — `low_correction_margin` and the UEC path of
  `raise_error_correction` — while the measured margin still caps the
  score value.
