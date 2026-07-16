# advisor QD-9 · first real grid run · 2026-07-16

**Judge versions (the law: measured numbers carry their judge)** ·
first run = scanner **0.4.0** (the worktree's pre-rebase base 7f4d258) ·
replication run same day = scanner **0.8.1** (rebased onto main 35e832f) ·
**every summary number byte-identical across both judges** (84/180 · median
margin gain 0.125 · grade gain 0.00 · center suboptimal 14/15 · decode 12/20
vs 15/20): the verdict is judge-version-robust across the 0.4→0.8 arc.

```
| text | M | 12 | quad-tr | 33 | no | — | — | — | — |
| text | M | 12 | quad-bl | 33 | yes | 31 | 0.111 | F | Poor |
| text | M | 12 | quad-br | 33 | no | — | — | — | — |
| text | M | 12 | edge-t | 33 | yes | 89 | 0.000 | F | Excellent |
| text | M | 12 | edge-b | 33 | yes | 89 | 0.000 | F | Excellent |
| text | M | 12 | edge-l | 33 | no | — | — | — | — |
| text | M | 12 | edge-r | 33 | no | — | — | — | — |
| text | M | 20 | center | 33 | no | — | — | — | — |
| text | M | 20 | quad-tl | 33 | no | — | — | — | — |
| text | M | 20 | quad-tr | 33 | no | — | — | — | — |
| text | M | 20 | quad-bl | 33 | no | — | — | — | — |
| text | M | 20 | quad-br | 33 | no | — | — | — | — |
| text | M | 20 | edge-t | 33 | no | — | — | — | — |
| text | M | 20 | edge-b | 33 | no | — | — | — | — |
| text | M | 20 | edge-l | 33 | no | — | — | — | — |
| text | M | 20 | edge-r | 33 | no | — | — | — | — |
| text | H | 12 | center | 45 | yes | 92 | 0.385 | C | Excellent |
| text | H | 12 | quad-tl | 45 | no | — | — | — | — |
| text | H | 12 | quad-tr | 45 | no | — | — | — | — |
| text | H | 12 | quad-bl | 45 | yes | 28 | 0.462 | C | Poor |
| text | H | 12 | quad-br | 45 | yes | 80 | — | — | Excellent |
| text | H | 12 | edge-t | 45 | yes | 92 | 0.462 | C | Excellent |
| text | H | 12 | edge-b | 45 | yes | 92 | 0.385 | C | Excellent |
| text | H | 12 | edge-l | 45 | yes | 92 | 0.385 | C | Excellent |
| text | H | 12 | edge-r | 45 | yes | 92 | 0.462 | C | Excellent |
| text | H | 20 | center | 45 | yes | 89 | 0.000 | F | Excellent |
| text | H | 20 | quad-tl | 45 | no | — | — | — | — |
| text | H | 20 | quad-tr | 45 | no | — | — | — | — |
| text | H | 20 | quad-bl | 45 | no | — | — | — | — |
| text | H | 20 | quad-br | 45 | yes | 77 | — | — | Good |
| text | H | 20 | edge-t | 45 | yes | 92 | 0.154 | F | Excellent |
| text | H | 20 | edge-b | 45 | yes | 89 | 0.154 | F | Excellent |
| text | H | 20 | edge-l | 45 | yes | 92 | 0.154 | F | Excellent |
| text | H | 20 | edge-r | 45 | yes | 89 | 0.154 | F | Excellent |

## per-case center vs best (20 cases)

| payload | ec | cov% | modules | center dec | center margin | best pos | best margin | Δmargin | Δgrade | center optimal |
|---|---|---|---|---|---|---|---|---|---|---|
| short-url | M | 12 | 29 | no | — | edge-t | 0.000 | — | — | no |
| short-url | M | 20 | 29 | no | — | — | — | — | — | no |
| short-url | H | 12 | 33 | yes | 0.250 | quad-bl | 0.500 | +0.250 | +2 | no |
| short-url | H | 20 | 33 | yes | 0.000 | edge-l | 0.125 | +0.125 | +0 | no |
| long-url | M | 12 | 45 | yes | 0.000 | center | 0.000 | +0.000 | +0 | yes |
| long-url | M | 20 | 45 | no | — | — | — | — | — | no |
| long-url | H | 12 | 61 | yes | 0.333 | edge-t | 0.500 | +0.167 | +2 | no |
| long-url | H | 20 | 61 | yes | 0.083 | edge-t | 0.167 | +0.083 | +0 | no |
| wifi | M | 12 | 33 | no | — | quad-bl | 0.111 | — | — | no |
| wifi | M | 20 | 33 | no | — | — | — | — | — | no |
| wifi | H | 12 | 37 | yes | 0.273 | quad-bl | 0.455 | +0.182 | +1 | no |
| wifi | H | 20 | 37 | yes | — | edge-t | 0.091 | — | — | no |
| vcard | M | 12 | 41 | yes | 0.000 | quad-bl | 0.125 | +0.125 | +0 | no |
| vcard | M | 20 | 41 | no | — | — | — | — | — | no |
| vcard | H | 12 | 53 | yes | 0.333 | edge-t | 0.500 | +0.167 | +2 | no |
| vcard | H | 20 | 53 | yes | 0.083 | edge-b | 0.167 | +0.083 | +0 | no |
| text | M | 12 | 33 | no | — | quad-bl | 0.111 | — | — | no |
| text | M | 20 | 33 | no | — | — | — | — | — | no |
| text | H | 12 | 45 | yes | 0.385 | quad-bl | 0.462 | +0.077 | +0 | no |
| text | H | 20 | 45 | yes | 0.000 | edge-t | 0.154 | +0.154 | +0 | no |

## summary

- decoded cells: 84/180 (47%)
- ranking metric: `UEC margin`  ·  grade-step metric: `UEC grade steps`
- median gain best-vs-center (UEC margin, center-decoded cases): 0.125
- median gain best-vs-center (margin units): 0.125
- median gain best-vs-center (UEC grade steps): 0.000
- center is NOT optimal: 14/15 decoding cases (93%)
- decode-loss center vs best: center decodes 12/20 cases (60%) · best position decodes 15/20 cases (75%) · delta +15 pts
- HYPOTHESIS (>1 grade median): median grade gain 0.00 UEC grade steps — margin-aware placement does NOT beat the >1-grade bar
```
