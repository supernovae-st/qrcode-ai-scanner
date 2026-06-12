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
| `raise_error_correction` | `current` (`"l"`/`"m"`/`"q"`) | score < 70 **or** UEC grade d/f, when EC < H | regenerate at a higher EC level |
| `low_correction_margin` | `errors`, `capacity` | UEC margin = 0 — the worst RS block consumed its ENTIRE budget | **distrust signal**: the decode may be a miscorrection; verify content out-of-band / regenerate |

## Consumer patterns

- **Verify-UI badge logic**: any of the first five hints → "improve your
  design" guidance with the specific fix. `low_correction_margin` →
  "decodes, but unreliable — regenerate" (treat as failure, not success).
- **Agent loop** (Nika): feed `hints` straight back into the generation
  prompt; the hints were designed to be actionable without image analysis.
- `hints` is non-empty only when scoring ran (Full/Fast profiles, ≥1
  detection).
