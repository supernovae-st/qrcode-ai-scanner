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
  "trace":      PipelineTrace, // why the scan succeeded or came back empty
  "versions":   Versions       // contract markers (spec/README.md)
}
```

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
  them — expect `null`s, never fabricated values.

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
{ "scanner": "0.3.0", "pipeline": 1, "score_contract": 3 }
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

## Machine-readable mirrors

- JSON Schema: [`scan-report.schema.json`](scan-report.schema.json)
- TypeScript: `bindings/report-types.d.ts` (shipped in both npm packages)
- Golden examples: [`examples/`](examples/) — produced by the real binary,
  CI-validated against both the serde types and the schema.
