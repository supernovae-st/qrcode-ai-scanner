# 01 · ScanReport — the wire contract

One JSON object per scan, identical shape on every surface (Rust serde,
CLI stdout, Node, WASM). Evolution is **additive only**: fields are never
renamed or removed within a major; new fields/variants may appear (parse
leniently). `None`/absent serializes as `null` on ALL surfaces (the wasm
serializer is configured `serialize_missing_as_null` to match serde_json).

```jsonc
{
  "detections": [Detection],   // empty = no QR found (a RESULT, not an error)
  "score":      Score | null,  // null in the frame profile / no detection
  "hints":      [Hint],        // machine-actionable, stable order (06-hints.md)
  "alpha":      AlphaReport,   // § Alpha — ABSENT (not null) for opaque inputs
  "trace":      PipelineTrace, // why the scan succeeded or came back empty
  "versions":   Versions       // contract markers (spec/README.md)
}
```

`alpha` is the ONE deliberate exception to the `None`-serializes-as-`null`
rule: the key is OMITTED entirely for opaque inputs (and under
`alpha_background: "none"`), so every pre-alpha report keeps its exact
bytes. Parse it as an optional key.

## Detection

```jsonc
{
  "symbology": "qr_code" | "micro_qr_code" | "rectangular_micro_qr_code"
             | "data_matrix" | "aztec" | "pdf417" | "maxi_code"
             | "ean13" | "ean8" | "upc_a" | "upc_e"
             | "code128" | "code39" | "code93" | "codabar" | "itf"
             | "data_bar" | "data_bar_expanded" | "telepen",
  "content": {
    "text":    string,   // decoded text under the resolved charset
    "raw":     string,   // ORIGINAL payload bytes, base64 — the truth
    "charset": "utf8" | "shift_jis" | "latin1"
  },
  "payload":  Payload,           // typed classification — 05-payloads.md
  "corners":  [Point,Point,Point,Point] | null, // clockwise from top-left,
                                 // ORIGINAL-image pixel coords, sub-pixel
  "meta": {
    "version":  1..40 | null,    // measured (geometry path), never guessed
    "ec_level": "l"|"m"|"q"|"h" | null,
    "mask":     0..7 | null,
    "modules":  number | null,   // version*4+17, derived
    "inverted": boolean | null   // light-on-dark, measured; null = no geometry
  },
  "engines": ["rxing"|"rqrr"|"rescue", ...]  // consensus surface, ≥1 entry
}
```

Semantics that bite:

- **Merging is by (symbology, decoded text)** — two physical symbols of
  the SAME symbology with the same payload collapse into one detection;
  an EAN-13 and a QR carrying identical digits stay two. Max 16
  detections per report (anti-amplification).
- **Only the QR family** (`qr_code` · `micro_qr_code` ·
  `rectangular_micro_qr_code`) can carry `meta` geometry, the UEC margin,
  the ISO 15415 card and the `rescue` engine. Other symbologies decode
  content + payload classification (all `meta` fields `null`).
- `content.text` + `content.charset` form a consistent pair resolved by
  the scanner ONCE over `raw`. If your consumer needs different charset
  politics, decode `raw` yourself.
- `engines: ["rescue"]` means BOTH decoders failed and the
  errors-and-erasures stage recovered the stream (07-pipeline.md §S5).
  The content passed an RS syndrome re-check — but treat any accompanying
  `low_correction_margin` hint as the distrust signal it is.
- `corners`/`meta.version`/`ec_level`/`mask` come from the geometry engine
  (rqrr) or the rescue candidate; the rxing-only path measures none of
  them — expect `null`s, never fabricated values. (Micro QR/rMQR decode
  via rxing only, so they carry no geometry today.)
- **Detections are ordered QR-family first** (stable within each group) —
  the scored "primary" (`detections[0]`) is contract, not detector
  iteration order. On a flyer with a QR + a retail barcode, the QR leads.
- **EXIF orientation is NOT applied**: decode is rotation-robust anyway
  (pinned by tests), but `corners` are in STORED pixel space — a browser
  that displays the JPEG EXIF-rotated will disagree with them.

## Point

`{ "x": number, "y": number }` — pixels in the ORIGINAL input image
(detections found on internal downscales are rescaled back).

## PipelineTrace

```jsonc
{
  "stages": [{
    "stage":            "pyramid"|"direct"|"enhance"|"deep"|"rescue",
    "transforms_tried": number,  // attempts executed in this stage
    "ms":               number,  // wall-clock (NOT deterministic)
    "detections_found": number   // RAW engine hits, PRE-merge (an rxing+rqrr
                                 // double-decode of one symbol counts 2)
  }],
  "engine_panics": number,       // third-party decoder panics caught + isolated
  "total_ms":      number
}
```

Stage names are stable identifiers. A stage that found something still
completes (consensus within the stage is preserved); the LADDER stops at
stage boundaries.

## Versions

```jsonc
// `scanner` is illustrative — it carries whatever workspace release produced
// the report. `pipeline` and `score_contract` are the normative compatibility
// anchors (bumped only on breaking / semantic change).
{ "scanner": "0.9.0", "pipeline": 2, "score_contract": 4 }
```

## Score — see [04-score.md](04-score.md) for full semantics

```jsonc
{
  "value":      0..100,
  "grade":      "excellent"|"good"|"acceptable"|"fair"|"poor",
  "axes":       [{ "axis": StressAxis, "passed": n, "total": n }],
  "structural": { "finder_integrity": [f,f,f], "quiet_zone_ok": bool } | null,
  "uec":        { "margin": f, "grade": "a".."f",
                  "worst_block_errors": n, "worst_block_capacity": n } | null,
  "iso15415":   Iso15415Report | null
}
```

`score` evaluates the PRIMARY detection (the first-discovered symbol).

## Alpha — transparent-input handling

Present ONLY when the input carried at least one pixel with alpha < 255
AND `alpha_background` was not `"none"`. A transparent asset has no one
background — before v0.9 the engine read the RGB *stored under* the
transparency, an exporter-dependent verdict (canvas exports store black
under full transparency, editors often white: the same visual design
scored 100, 74 or nothing depending on the exporting tool). Since
pipeline v2 the input is composited over a DECLARED background before
luma conversion and RGB extraction; every downstream stage (ladder ·
score · UEC · ISO) sees the flattened image.

```jsonc
{
  "coverage":     0.612,    // fraction of pixels with alpha < 255 (3 decimals)
  "content_luma": 34,       // alpha-weighted mean BT.601 luma of the content
  "mode":         "auto" | "white" | "black" | "custom",   // config echo
  "background":   "white" | "black" | "#rrggbb",  // what the verdict used
  "fallback_used": false,   // auto only: the opposite background rescued
                            // a zero-detection scan (both walks in trace)
  "envelope": {             // Full profile only; null when skipped
                            // (score_skip_checks: ["alpha_envelope"]),
                            // budget-exhausted, or nothing decoded
    "probes": [             // ordered by luma: 5 fixed rungs (0·64·128·192·255)
      { "background_luma": 0,   "decoded": false },   // + one bisection probe
      { "background_luma": 255, "decoded": true }     //   per verdict boundary
    ],
    "safe_luma": [[160, 255]], // contiguous decoded bands — every endpoint
                               // is a TESTED background, never interpolation
    "placement": "any" | "light_only" | "dark_only" | "mixed" | "none"
  }
}
```

Semantics, normative:

- **`auto`** (the default): content with alpha-weighted mean luma < 128
  flattens over white, else over black — the design is measured on its
  intended placement. A zero-detection scan re-flattens over the opposite
  background and walks the ladder once more within the same budget
  (`fallback_used: true` when that walk produced the detections). Forced
  modes (`white` · `black` · `#rrggbb`) never retry: the host's declared
  surface is the truth being measured.
- **The envelope** answers "over which backgrounds does this design keep
  decoding": quick decode probes (the primary symbol's own decode class,
  never the full ladder's recovery power) on neutral backgrounds. It is
  informational — it never moves `score.value` (surface truth, not score
  surgery) — and drives the `alpha_background_dependent` hint.
- **Exporter invariance**: the RGB stored under fully transparent pixels
  never influences the report.
- **Opaque inputs** (or `"none"`): the historical path bit for bit, no
  `alpha` key.

## Machine-readable mirrors

- JSON Schema: [`scan-report.schema.json`](scan-report.schema.json)
- TypeScript: `bindings/report-types.d.ts` (shipped in both npm packages)
- Golden examples: [`examples/`](examples/) — produced by the real binary,
  CI-validated against both the serde types and the schema.
