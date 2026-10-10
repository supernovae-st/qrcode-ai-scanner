# 01 · ScanReport — the wire contract

One JSON object per scan, identical shape on every surface (Rust serde,
CLI stdout, Node, WASM). Evolution is **additive only**: fields are never
renamed or removed within a major; new fields/variants may appear (parse
leniently). `None`/absent serializes as `null` on ALL surfaces (the wasm
serializer is configured `serialize_missing_as_null` to match serde_json).

```jsonc
{
  "detections": [Detection],   // empty = no QR found (a RESULT, not an error)
  "score":      Score | null,  // null when no judgment was published — § score: null
  "hints":      [Hint],        // machine-actionable, stable order (06-hints.md)
  "alpha":      AlphaReport,   // § Alpha — ABSENT (not null) for opaque inputs
  "trace":      PipelineTrace, // why the scan succeeded or came back empty
  "versions":   Versions       // contract markers (spec/README.md)
}
```

Four keys are deliberately OMITTED instead of serialized as `null`, so that
reports that do not need them keep their exact bytes. Parse them as optional
keys:
- `alpha`: opaque inputs and `alpha_background: "none"` (§ Alpha);
- `score.axes[].refined_failed_at`: no knee refinement ran (04-score.md);
- `content.eci`: no ECI designator observed (§ Content);
- `content.fnc1`: no FNC1 observed, or not reported by the symbology
  (§ Content).

## Detection

```jsonc
{
  "symbology": "qr_code" | "micro_qr_code" | "rectangular_micro_qr_code"
             | "data_matrix" | "aztec" | "pdf417" | "maxi_code"
             | "ean13" | "ean8" | "upc_a" | "upc_e"
             | "code128" | "code39" | "code93" | "codabar" | "itf"
             | "data_bar" | "data_bar_expanded" | "telepen",
  "content": {                   // § Content
    "text":    string,           // raw decoded as charset states
    "raw":     string,           // payload bytes, base64 — QR family: ORIGINAL bytes
    "charset": "utf8" | "shift_jis" | "latin1" | "gb18030" | "eci" | "unknown",
    "eci":     [{ "designator": number, "start": number }],   // OPTIONAL KEY
    "fnc1":    "first" | "second"                             // OPTIONAL KEY
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

- **Detections are the distinct contents the scanner can tell apart**, not
  a count of physical symbols. Every read of every attempt is reconciled
  before publication (07-pipeline.md property 6): reads with the same
  symbology, `raw` and FNC1 position and compatible ECI evidence denote one
  content and become one detection, which keeps the most specific
  evidence.
  - Identical copies of a symbol are one detection; corners come from the
    first read of that detection that carries them.
  - Reads the scanner cannot tell apart (same bytes and FNC1 position,
    declarations not located, nothing in their position to separate them)
    may collapse into one detection. A read that fits more than one
    detection and lies inside none of them, or inside several, joins none.
  - A different `raw`, a different FNC1 position or contradicting ECI
    declarations always stay separate detections, even with identical
    `text`. An EAN-13 and a QR carrying identical digits stay two.
  - No two detections of a report denote the same content (§ Comparing
    contents). At most 16 detections per report.
- **Only the QR family** (`qr_code` · `micro_qr_code` ·
  `rectangular_micro_qr_code`) can carry `meta` geometry, the UEC margin,
  the ISO 15415 card and the `rescue` engine. Other symbologies decode
  content + payload classification (all `meta` fields `null`).
- `content` is the scanner's ONE reading of the symbol's data
  (§ Content); for other charset politics, decode `raw` yourself (under
  `content.eci` when present).
- `engines: ["rescue"]` means BOTH decoders failed and the
  errors-and-erasures stage recovered the stream (07-pipeline.md §S5).
  The content passed an RS syndrome re-check — but treat any accompanying
  `low_correction_margin` hint as the distrust signal it is.
- `corners`/`meta.version`/`ec_level`/`mask` come from `rqrr`: from its
  decode, from a grid it detected but could not decode that the scanner
  error-corrected itself (FNC1, Hanzi and structured-append symbols;
  `engines` may then list `rxing` alone), or from the S5 rescue candidate.
  `rxing` measures no geometry: a symbol only `rxing` read carries `null`
  corners, version and mask, and the EC level its decoder states —
  `ec_level` is the one `meta` field such a read fills. (Micro QR/rMQR
  decode via `rxing` only, so they carry no geometry today.) For a symbol
  decoded mirrored, `ec_level` and `mask` are those of
  the orientation that decoded, and the score measures that orientation
  (04-score.md § UEC). An `rqrr` decode whose grid the scanner could not
  verify (07-pipeline.md property 6) carries no stream, version, EC level
  or mask — and then no structural or ISO section. The scanner also
  samples symbols internally (the re-read, 07-pipeline.md property 6) and
  uses every engine's corner points to keep the reads of different symbols
  apart; pipeline 3 publishes neither.
- **Detections are ordered QR-family first** (stable within each group) —
  the scored "primary" (`detections[0]`) is contract, not detector
  iteration order. On a flyer with a QR + a retail barcode, the QR leads.
- **EXIF orientation is NOT applied**: decode is rotation-robust anyway
  (pinned by tests), but `corners` are in STORED pixel space — a browser
  that displays the JPEG EXIF-rotated will disagree with them.

## Content

`content` is one exact reading of a symbol's data: `raw` is the bytes;
`eci`, when present, lists the ECI designators the symbol declares and
where each takes effect; `fnc1`, when present, says the data is in GS1 mode
(`first`) or AIM application mode (`second`); `text` is `raw` decoded once,
and `charset` says how:

- `utf8` · `shift_jis` · `latin1` · `gb18030`: exactly that decoder over all
  of `raw`. `latin1` is the WHATWG windows-1252 decoder, NOT strict ISO/IEC
  8859-1; `gb18030` is the WHATWG gb18030 decoder, which reads GB 2312 and
  GBK unchanged.
- `eci`: `raw` decoded under the listed designators.
- `unknown`: `text` is not a complete decoding.

`unknown` is the only `charset` value whose `text` is not a complete
decoding, in this version and every later one. Every other value, including
values added later, states that `text` is a complete decoding of `raw` that
the scanner checked byte for byte; it states the encoder's declared
interpretation only when `eci` lists the designators (rule 2), otherwise the
observed Kanji/Hanzi segments, the decoding engine's mode-aware reading or
the legacy reading chose it (rule 1). Lossy detail can only arrive as new
optional fields.

### `raw`

For the QR family (`qr_code` · `micro_qr_code` ·
`rectangular_micro_qr_code`), `raw` is the original payload on every decode
path (`rxing`, `rqrr`, `rescue`), as ISO/IEC 18004 defines the data:

- ECI designators, mode indicators and character counts are excluded;
- numeric and alphanumeric data appear as ASCII, byte data verbatim;
- Kanji data appears as its Shift_JIS byte pairs (§7.4.6) and Hanzi data
  (GB/T 18284, subset 1) as its GB 2312 byte pairs, whatever ECI is in
  force;
- under FNC1 (either position), an alphanumeric `%` is 0x1D (GS) and `%%`
  is a literal `%` (§7.4.8);
- with FNC1 in second position, the application indicator comes first: two
  ASCII digits (`00`–`99`) or one ASCII letter (encoded value − 100);
- structured-append headers are consumed; symbols are not reassembled.

`raw` is never an engine's text re-encoded. When the scanner can
error-correct and parse the very grid an engine decoded, its own parse
decides the bytes, even where that engine's conversion differs (Kanji
values 5949–5951, which one decoder writes as DF FD–DF FF, are published as
9F FD–9F FF). When the grid verifies in both orientations with different
results, the scanner keeps the engine's bytes and treats the declarations
as unobserved. A QR-family read whose original bytes cannot be established
is not published.

Other symbologies carry the bytes of their decoding engine's reading: the
ISO/IEC 8859-1 encoding of the engine's text when every character is at
most U+00FF — exactly the symbol's bytes on the engine's paths without ECI
(Data Matrix ASCII, upper shift and Base256; PDF417 byte compaction; Aztec
binary data) — and otherwise the UTF-8 encoding of that text. Under an ECI
the engine's reading can omit a segment its decoder cannot read; `raw` is
that reading and its label is not downgraded for it. The original-bytes
guarantee covers the QR family only.

### `eci` (optional key)

The ECI designators observed in the symbol, in stream order, duplicates
kept. `designator` is the assignment number as encoded (ISO/IEC 18004
§7.4.2, Table 4: one to three codewords, 0–999999), listed whether or not
the scanner can interpret it. `start` is the byte offset into `raw` where
it takes effect; a designator governs `raw` from its `start` up to the next
entry's `start`, or to the end. Offsets never decrease and never exceed the
length of `raw`. A designator whose span is empty governs no byte.

| Wire | Meaning |
|---|---|
| key absent | No designator observed: the scanner parsed the data and found none; the decoding engine reported none (`]Q0`/`]Q1`/`]Q3`/`]Q5`); the symbology has no ECI mode (Micro QR); or the scanner could not observe it — every non-QR symbology, and the rare QR read neither the scanner nor its engine could check (rule 1u). Absent means "none observed", not "verified none". |
| `[]` | The symbol signals ECI use (AIM `]Q2`/`]Q4`/`]Q6`), but no stream of it could be verified, so its designators could not be located. Every rMQR read that signals ECI is in this case. |
| entries | Designators located by the scanner's own parse of the error-corrected data codewords. |

### Refused streams

A read is refused, and produces no detection, when the scanner
error-corrects the grid it was read from and its parse finds a structure
ISO/IEC 18004 (with GB/T 18284 for Hanzi) does not define:

- an ECI designator whose first codeword is `111xxxxx`, whose value exceeds
  999999, or that is cut short;
- FNC1 in second position after any data, after FNC1 in first position, or
  twice; FNC1 in first position after FNC1 in second position;
- an FNC1-second application indicator outside 0–99, 165–190 and 197–222;
- a numeric group of three digits ≥ 1000 (two digits ≥ 100, one digit
  ≥ 10), an alphanumeric pair value ≥ 2025 or a final single value ≥ 45;
- a Hanzi subset other than GB 2312, an unassigned mode indicator, or a
  character count longer than the data.

A refusal also withholds the other QR reads of the same scan that report
what a more tolerant decoder reports for the refused stream (same bytes,
same ECI signal, same FNC1 position), that were read where the refused
symbol is, and that the scanner could not check itself — unless a read the
scanner did check carries that content too: then they may belong to a
well-formed symbol and are reconciled normally. A read of that content made
elsewhere in the image belongs to another symbol and is never withheld for
it. Reads whose stream the scanner could not error-correct in any way, and
that match no refusal, are not checked.

### `fnc1` (optional key)

| Wire | Meaning |
|---|---|
| key absent | No FNC1 observed, or the symbology does not report it. |
| `first` | FNC1 in first position (QR `]Q3`/`]Q4`, Data Matrix `]d2`/`]d5`, Code 128 `]C1` = GS1-128): `raw` holds GS1 element-string data. QR and Data Matrix transmit 0x1D between fields; GS1-128 as read by the current decoding engine does not. |
| `second` | FNC1 in second position (QR `]Q5`/`]Q6`, Data Matrix `]d3`/`]d6`, Code 128 `]C2`): `raw` starts with the AIM application indicator (two ASCII digits, or one ASCII letter). |

For the QR family the position is observed on every decode path. Other
symbologies report the positions their identifiers above carry; GS1 DataBar
(whose decoded text is the GTIN without its application identifier), Aztec,
PDF417 and the rest leave the key absent.

### `text` and `charset`

When `raw` is empty, `text` is empty and `charset` is `utf8`, whichever rule
applies. Otherwise exactly one rule applies.

1. **`eci` absent — the unmarked reading.** The scanner knows a symbol's
   Kanji segments (ISO/IEC 18004 §7.4.6) and Hanzi segments (GB/T 18284)
   only when it parsed the error-corrected data itself (07-pipeline.md
   property 6).
   - u. QR family, the scanner could not observe whether the symbol
     declares an ECI (property 6, *unobserved*), and `raw` has a byte
     ≥ 0x80: `unknown`; ASCII bytes stay, every other byte becomes one
     U+FFFD.

   Segments observed:
   - a. No Kanji or Hanzi segment — the legacy reading: strict UTF-8 →
     `utf8`; else, when every non-ASCII byte sits alone between ASCII bytes,
     windows-1252 → `latin1`; else an error-free Shift_JIS decode →
     `shift_jis`; else windows-1252 → `latin1`. No decoder sniffs a
     byte-order mark.
   - b. Kanji segments and no Hanzi segment: if `raw` is valid UTF-8,
     `utf8` (some encoders put UTF-8 bytes in Kanji mode); otherwise
     `shift_jis`, `text` the Shift_JIS decoding of all of `raw`, provided
     all three hold: that decoder reads every Kanji segment as well-formed
     pairs; it reads every maximal run of bytes outside the Kanji segments
     on its own; no such run containing a byte ≥ 0x80 is valid UTF-8.
   - c. Hanzi segments and no Kanji segment: `gb18030` (the gb18030
     decoding of all of `raw`), under the conditions of b without its UTF-8
     step.
   - d. Otherwise — Kanji and Hanzi segments together, or a segment or run
     that b and c reject: `unknown`, `eci` absent. Kanji pairs read as
     Shift_JIS and Hanzi pairs as gb18030, an ill-formed pair getting the
     WHATWG replacement; ASCII bytes outside the segments stay; every other
     byte becomes one U+FFFD. When the reads of one detection observed
     different non-empty segments, no pair is read: ASCII bytes stay,
     every other byte becomes one U+FFFD.

   Segments not observed (Micro QR and rMQR always; a QR symbol only one
   engine read and that the scanner could not re-read): if `raw` is valid
   UTF-8, `utf8`;
   - e. otherwise, when the decoding engine's own reading equals the
     Shift_JIS decoding of all of `raw` — or else its gb18030 decoding —
     without error and with at least one character decoded from a byte
     pair, that label and that decoding. The engine read the symbol mode by
     mode; its reading only chooses between the scanner's own complete
     decodings. Where the engine itself guessed Shift_JIS for byte-mode
     data, this reading follows that guess. When the reads of one detection
     carry different engine readings, rule e does not apply;

   otherwise rule a.
2. **`eci` has entries — the declared reading.** Bytes before the first
   designator use the ISO/IEC 18004 default interpretation, ISO/IEC 8859-1
   (ECI 000003), whatever mode encoded them. From each designator to the
   next, bytes decode with that designator's character set from the table
   below. Under an ECI every mode, Kanji and Hanzi included, is byte
   compaction (§7.4.2.1). Decoders are strict: no byte-order-mark handling,
   no normalization.
   - A designator whose span is empty governs no byte: it is listed in
     `eci` but neither decodes nor affects `charset`.
   - If some governed byte does not decode, `charset` is `unknown`: each
     byte governed by 899 (binary data) or by a designator outside the
     table becomes one U+FFFD; an ill-formed sequence in a supported set
     becomes U+FFFD (WHATWG: one per maximal ill-formed UTF-8 subpart; a
     Shift_JIS or gb18030 lead with a non-ASCII bad trail is one U+FFFD, a
     lead with an ASCII trail is U+FFFD followed by that character); every
     other byte keeps its declared reading.
   - Otherwise `charset` is the first of `utf8`, `shift_jis`, `latin1`,
     `gb18030` that meets both: (a) its family contains every
     interpretation that governs at least one byte — each designator whose
     span is not empty, plus the default ISO/IEC 8859-1 when a byte before
     the first designator is not ASCII (families: `utf8` ← 26, 27 ·
     `shift_jis` ← 20 · `latin1` ← 1, 3, 23 and the default · `gb18030` ←
     29, 31, 32); (b) its decoder applied to all of `raw` yields `text`
     exactly. If none qualifies, `eci`.
3. **`eci` is `[]` — an unlocated declaration.** No reading of `raw` is
   verifiable without the designators: any designator could govern any
   byte, ECI 000899 binary data included. So `charset` is `unknown` and
   `text` is one U+FFFD per byte of `raw`. The decoding engine's own
   reading is never published.

### ECI decode table (pipeline v3)

Changing this table is a `versions.pipeline` change.

| Designator | Character set |
|---|---|
| 1, 3 | ISO/IEC 8859-1 (byte b → U+00b) |
| 4–10, 12, 15–18 | ISO/IEC 8859-2 … -8, -10, -13 … -16 (WHATWG: 0x80–0x9F → U+0080–U+009F; undefined positions are invalid) |
| 11, 13 | ISO/IEC 8859-9, 8859-11 (exact in-crate tables: 0x80–0x9F → U+0080–U+009F; undefined positions are invalid) |
| 20 | Shift_JIS (WHATWG) |
| 21, 22, 23, 24 | windows-1250, -1251, -1252, -1256 |
| 25, 33 | UTF-16BE, UTF-16LE |
| 26 | UTF-8 |
| 27 | US-ASCII (bytes ≥ 0x80 are invalid) |
| 28 | Big5 (WHATWG) |
| 29, 31, 32 | GB 2312, GBK, GB 18030 (the WHATWG gb18030 decoder) |
| 30 | EUC-KR (WHATWG) |
| 34, 35 | UTF-32BE, UTF-32LE |
| 899 | binary data — never decoded as text |
| any other (0 and 2 CP437, 14, 19, 170, …) | unsupported — listed in `eci`, never decoded |

The WHATWG Shift_JIS, Big5, gb18030 and EUC-KR decoders accept supersets of
the registered character sets.

### Invariants

Pinned by tests; the JSON Schema enforces the first three.

- `charset` `eci` ⇒ `eci` has at least one entry.
- `eci` is `[]` and `raw` is not empty ⇒ `unknown`, one U+FFFD per byte.
- `text` is empty if and only if `raw` is empty, and then `charset` is
  `utf8`.
- `utf8` · `shift_jis` · `latin1` · `gb18030` ⇒ that decoder over all of
  `raw` yields `text` exactly.
- `eci` ⇒ the declared reading (rule 2) yields `text` exactly, with no
  replacement.
- `unknown` ⇒ each U+FFFD the scanner inserted stands for one byte or one
  maximal ill-formed sequence it could not read; a U+FFFD the data encodes
  is a character. Positions cannot be recovered from `text`.
- Removing a designator whose span is empty, while another remains,
  changes neither `text` nor `charset`.
- No two detections of a report denote the same content.
- Same input, configuration and scanner build ⇒ same content (04-score.md
  § Determinism states the PDF417 exception).

`unknown` appears with `eci` (rules 2 and 3) or without it (rules 1u and
1d). Never infer one from the other.

### Comparing contents

`text` is never an identity: undecodable bytes become U+FFFD, and one
printed symbol can come back with or without a reading. Two detections
denote the same content when all four hold: (1) equal `symbology`; (2)
equal `content.raw` bytes; (3) equal `content.fnc1` (both absent, or the
same value); (4) compatible `content.eci`: both absent, or both present and
either one is `[]` or both lists are equal (same designators, same
`start`s, same order).

No two detections of one report denote the same content. Across reads,
attempts, profiles, budgets, engine versions and scans, one printed symbol
may come back with different specificity: `eci: []` is the same content as
the located list. Under one `versions.pipeline`, two same contents with
equal located `eci` lists carry the same `text` and `charset` (rule 2 is a
function of `raw` and `eci`). Readings of `eci`-absent content also depend
on the segments the scanner observed and on the engine's reading (rule 1),
so they can differ; `eci: []` carries none. Across pipeline versions,
compare identity only. Outside the QR family `raw` is the engine's reading,
so identity there is only as exact as the engine.

- **To deduplicate or cache:** key on (`symbology`, `raw`, `fnc1`, whether
  `eci` is present); within a key, merge `[]` into the located list only
  when exactly one located list exists; the located content wins.
- **To check a symbol you generated:** compare `content.raw` (and `eci`,
  `fnc1`) with what you encoded, never `text`.
- The scanner applies this identity itself: to reconcile reads
  (07-pipeline.md property 6) and to match score stress cells and alpha
  probes (property 7).

### Reading content

- `unknown` is the only lossy value, forever. For every other value,
  including one you do not recognize, show or classify `text` and never
  re-decode `raw` from a label you do not know. For `unknown`, say the
  content has no verified text reading and use `raw` (with `eci`).
- `eci: []` means the symbol declares an ECI the scanner could not locate.
  Decoding `raw` under your own knowledge of the encoder's declaration is
  your claim, not the scanner's.
- Never infer the presence of `eci` from `charset`, or the reverse. When
  `eci` has entries, never re-decode `raw` from `charset` alone: use
  `text`, or decode `raw` under `eci` with the table above.

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
    "detections_found": number   // RAW engine hits, PRE-merge (rxing+rqrr on one symbol = 2),
                                 // before the re-read, refusals of other reads and
                                 // reconciliation — it includes hits the scanner refuses
                                 // or holds for confirmation; a decode refused inside the
                                 // decoder step (its own grid parses malformed) is not a hit
  }],
  "engine_panics":    number,    // third-party decoder panics caught + isolated
  "total_ms":         number,    // wall clock of the ladder walk(s) only
  "budget_exhausted": boolean    // score_contract ≥ 5: true = the budget prevented planned work,
                                 // the report is partial; false = final (02-profiles.md § Budget).
                                 // ABSENT on older reports: unknown, never false
}
```

Stage names are stable identifiers. A stage that found something still
completes (consensus within the stage is preserved); the ladder stops at
stage boundaries, except while every QR Code read so far only signals an
ECI it could not locate (07-pipeline.md property 2). `detections_found`
includes hits the scanner refuses or holds for confirmation, so a stage can
count hits with no published detection — read `detections`, not this
counter, to know what was found.

## Versions

```jsonc
// `scanner` is illustrative — it carries whatever workspace release produced
// the report. `pipeline` and `score_contract` are the normative compatibility
// anchors (bumped only on breaking / semantic change).
{ "scanner": "0.9.0", "pipeline": 3, "score_contract": 5 }
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

## `score: null`

`score` judges `detections[0]`. It is `null` exactly when no judgment was
published:

1. there is no detection;
2. the configuration computes no score: score depth off (the `frame`
   profile), or every stress axis skipped;
3. the wall-clock budget interrupted the judgment, and
   `trace.budget_exhausted` is `true` (score_contract ≥ 5; released
   versions with an earlier contract published a partial composite
   instead);
4. a contained internal fault occurred in the score computation (native
   builds; on wasm the call throws).

A judgment is never partial: a present `score` ran its calibration and
every planned stress cell, so its `value`, `grade`, `weights_run` and
`axes[].passed` / `total` / `failed_at` do not depend on the budget; only
`axes[].refined_failed_at` may be missing (and `trace.budget_exhausted`
then says the budget cut it). Never read `null` as `0`. With detections
and `trace.budget_exhausted: true`, `null` means "not judged": re-scan
with a larger budget or none. It says nothing about the symbol.

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
    "placement": "any" | "light_only" | "dark_only" | "mixed" | "none",
    "palette": [               // HOST-requested colors (`alpha_palette`
                               // config), request order — [] when none;
                               // never distorts probes/safe_luma/placement
      { "background": "#f5f5f4", "background_luma": 245, "decoded": true }
    ]
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
  surgery) — and drives the `alpha_background_dependent` +
  `add_background_plate` hints. A probe counts as decoded when one of its
  reads denotes the same content as the primary detection (07-pipeline.md
  property 7).
- **The palette** (`alpha_palette` config, `white` · `black` · `#rrggbb`)
  lets the host probe ITS theme/brand colors inside the same scan —
  per-color verdicts in request order, evaluated at each color's BT.601
  luma (the sweep is luma-space by construction). Every binding rejects
  more than 32 colors loudly (anti-DoS: each entry costs a composite +
  up to three decode attempts).
- **Exporter invariance**: the RGB stored under fully transparent pixels
  never influences the report.
- **Opaque inputs** (or `"none"`): the historical path bit for bit, no
  `alpha` key.

## Machine-readable mirrors

- JSON Schema: [`scan-report.schema.json`](scan-report.schema.json)
- TypeScript: `bindings/report-types.d.ts` (shipped in both npm packages)
- Golden examples: [`examples/`](examples/) — produced by the real binary,
  CI-validated against both the serde types and the schema.

Validate a report against the schema of the release that produced it: the
tag matching `versions.scanner`. The copy on the default branch describes
the next release, and its `const` markers reject reports of other contract
versions. A report whose `pipeline` or `score_contract` differs from the
consts of the schema at the tag its `versions.scanner` names comes from a
development build: validate it against the schema of the commit that built
it.
