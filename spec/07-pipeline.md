# 07 · The decode pipeline v3 (normative behavior)

```
ImageInput (Encoded | Rgba8 | Luma8)
  → validate Limits
  → alpha flatten (pipeline v2 — 01-report § Alpha): transparent pixels
    composite over the declared background BEFORE luma/RGB extraction;
    opaque inputs take the historical path bit for bit
  → BT.601 luma (+ lazy RGB planes)
  → cap to max_engine_side (engines never see larger; corners rescale back)
  → S1 pyramid   ≤512px downscale (cheapest, often best on artistic)
  → S2 direct    full-resolution luma
  → S3 enhance   otsu · invert · contrast stretch · R/G/B channel planes
  → S4 deep      15 rungs (12 contrast boosts + 3 morphological closes)
                 + size × stretch × {otsu, invert} grid
  → S5 rescue    errors-and-erasures RS over undecoded grids (below)
  → (alpha auto only, zero detections) re-flatten over the OPPOSITE
    background → one more ladder walk within the same budget — both
    walks stay in the trace; `alpha.fallback_used` marks a rescue
  → merge        reconcile reads (property 6) → content interpreted once per
                 detection (01-report § Content)
  → score        contract v5 (04-score.md) — Full/Fast only
  → alpha placement envelope (Full only) — 01-report § Alpha
  → ScanReport
```

Normative properties:

1. **Fixed attempt order** — no RNG anywhere. Same input + config +
   version ⇒ same attempt sequence, bit for bit (per platform; see the
   determinism caveats in 04-score.md).
2. **Stage early-exit** — a stage that finds something still COMPLETES
   (within-stage consensus preserved); the ladder then stops, unless among
   the reads collected so far (refused and withheld reads excluded) there
   is a QR Code read and every QR Code read only signals an ECI it could
   not locate: then the later enabled stages run, within the budget, so
   that a stage can parse the symbol and locate its declarations. A QR Code
   read that a grid the scanner re-derived contains, with a matching
   account, counts as located. Reads of other symbologies (Micro QR, rMQR
   and retail barcodes included) neither stop nor prolong the walk. A stage
   whose reads the acceptance holds (§ Acceptance) does not end the walk
   either; held reads do not count for S5's trigger. S5 runs only when
   S1–S4 published nothing: hits the scanner refused or withheld do not
   count, so `trace.stages[].detections_found` may be non-zero on a walk
   that went on (read `detections`, not the counter).
3. **Budget + cancel checked between attempts** — an engine call is not
   interruptible; inputs are size-capped instead.
4. **Engines are panic-isolated** — a third-party decoder panic increments
   `trace.engine_panics` and the ladder continues.
5. **Photometric polarity travels with geometry** — a grid decoded on an
   INVERTED attempt is sampled/scored in that polarity and reports
   `meta.inverted: true`.
6. **One read set, one content per distinguishable symbol (pipeline v3).**
   Engines report reads, never text. A read carries its payload bytes
   (01-report § Content), its FNC1 position (none · first · second · not
   observable), its ECI evidence, the Kanji and Hanzi segments the scanner
   observed when it parsed the data itself, and the corner points of the
   symbol it read (internal).

   **Parsing what an engine read.** The scanner error-corrects and parses
   streams itself, with its own parser (01-report § Content), from five
   sources:
   - the grid `rqrr` decoded, as sampled and, when that does not
     error-correct, transposed (the orientation `rqrr`'s mirrored retry
     decodes). A parse whose bytes equal `rqrr`'s — or differ only where
     `rqrr` writes the Kanji values 5949–5951 as DF FD–DF FF — decides the
     read's bytes, declarations, FNC1 position and segments, and the
     published stream and geometry come from that orientation. When the
     sampled grid parses malformed or with other bytes, the transposed grid
     is checked too: when exactly one orientation error-corrects, its parse
     decides (a malformed one refuses the read); when both error-correct to
     the same result, that result stands; when both error-correct with
     different results, nothing is decided (the read keeps `rqrr`'s bytes
     with its declarations unobserved, and is never refused for it);
   - grids `rqrr` detected but did not decode (FNC1, Hanzi,
     structured-append and damaged symbols), as sampled or else transposed
     (a grid that parses malformed one way while the other way
     error-corrects to another result decides nothing), re-derived in each
     attempt while a QR Code read the scanner has not parsed is present (at
     most four per attempt), and once more over all collected grids at the
     end of the walk. Such a parse contributes declarations, FNC1 position,
     segments and geometry to the reads of the same bytes it contains, and
     no engine;
   - the codewords of `rxing`'s ZXing-lineage reader, which runs only when
     its zxing-cpp-port reader finds nothing, accepted only when the
     byte-mode segments equal the reader's own;
   - rescue streams (S5);
   - the **re-read** (below).

   **Read checks.** A QR Code read of `rxing`'s zxing-cpp-port reader with
   identifier `]Q1` and a non-empty ASCII text is kept only when its bytes
   are that text (or, behind a byte-order mark, decode to it the way
   `rxing` decodes them); an empty text — `rxing`'s rendering of a segment
   its decoder rejects, such as a Kanji pair 5949–5951 — says nothing of
   the bytes. A `]Q1` read whose bytes mix ASCII with such a pair keeps a
   partial ASCII text and is refused; `rqrr`'s read still publishes that
   content. A DataBar (RSS-14) read whose text is not exactly fourteen
   ASCII digits is refused.

   Error correction accepts any correctable block (2t ≤ the parity
   codewords) on every source except S5 rescue, which keeps the Annex B
   reserve (and, in a block it corrects with erasures, two more parity
   codewords — § S5) because no engine decoded that stream. In each
   attempt: the engines run (`rxing`, then `rqrr`); detected grids are
   re-derived; the refusals they prove are recorded; the re-read runs for
   the reads no parse of the attempt carries and no recorded refusal
   withholds, and records its own refusals; the reads are collected.

   **ECI evidence.** *located*: a parse above found the designators.
   *no-ECI*: a parse found no designator; modifier `]Q0`/`]Q1`/`]Q3`/`]Q5`;
   or no ECI mode (Micro QR). *signalled*: `]Q2`/`]Q4`/`]Q6` and no stream
   of the symbol verified (every rMQR read that signals ECI). *unobserved*:
   non-QR symbologies; a QR read whose identifier does not parse; an `rqrr`
   read whose grid decided nothing and that the re-read could not verify.

   **The re-read.** It runs for a QR Code read of `rxing`'s zxing-cpp-port
   reader (Micro QR and rMQR excluded) whose modifier is `]Q2`–`]Q6`, or
   `]Q0`/`]Q1` with a payload that is not valid UTF-8, and for an `rqrr`
   read whose grid decided nothing — unless a read the scanner parsed, or a
   grid it re-derived, in the same attempt carries the same content (same
   bytes, a matching account) where the read lies (the read lies inside
   it), or a recorded refusal withholds the read (below). At most 16
   re-reads per scan.

   It also runs, for its correction margin only, for a QR Code port read
   at EC level L or M that no other read of its content measured with a
   margin of zero or more (§ Acceptance): candidates are versions 1 and 2
   only (at most 12 format probes and 12 error-correction attempts), once
   per content in a walk, within the 16 re-reads per scan. Such a re-read
   changes nothing else of the read; a re-read that locates a read records
   the margin of the word it read.
   - The scanner samples the attempt image at the four outer symbol
     corners the engine reports. Candidates are versions 1–40 (`rqrr`: its
     measured version) in both orientations, through three sampler passes
     (the engine's binarized image in the polarity it read, the other
     polarity, luma with a local threshold); a version whose module pitch
     would be below one pixel is skipped.
   - The format information must decode, within three bit errors, to the
     read's error-correction level, and from version 7 the version
     information must decode to the candidate version. Blocks error-correct
     within their parity. The parse must reproduce the read's payload bytes
     exactly and agree with its modifier: ECI present if and only if
     `]Q2`/`]Q4`/`]Q6`, and the same FNC1 position.
   - At most 240 format probes and 240 error-correction attempts per
     re-read; the first accepted parse ends it. It is deterministic, has no
     time budget, runs inside the read's attempt under its own panic
     boundary, adds no engine, detection, attempt or trace entry, is never
     run by score cells or probes, and keeps its geometry internal. A
     re-read that cannot verify leaves the engine's evidence.

   **Refusal.** A read whose own grid the scanner error-corrects and parses
   as malformed is refused (01-report § Refused streams); that refusal is
   final. The re-read refuses an `rxing` read when its parse is malformed
   and a parse with that decoder's tolerances (designators up to 2097151,
   FNC1 in both positions) reproduces the payload exactly. A refusal, and a
   detected grid whose re-derivation parses malformed, names what a
   tolerant decoder reports for that stream — payload bytes, whether it
   signals an ECI, FNC1 position — and, for `rqrr`, the bytes `rqrr`
   decoded, and where the refused symbol is (its grid's corner points, or
   the refused read's). Every QR Code read of the scan that the scanner did
   not parse, whose account matches such a name and that lies inside the
   refused symbol — the centre of its corner points inside the box of the
   refused symbol's, wherever the read was made; a read or a refusal
   without corner points never fails this test — is withheld,
   retroactively and for the rest of the scan, unless a read the scanner
   parsed, or a grid it re-derived, carries a matching account; `rqrr`'s
   bytes withhold only `rqrr`'s part of a read. A refused stream is one
   record per pair of names (a stream `rqrr` decoded carries both; the
   re-read and a re-derived grid name the tolerant reading only), whatever
   the attempt or place it was refused in: the record keeps the distinct
   places — a place whose centre lies inside a recorded one is the same
   symbol — at most 8; past that it withholds wherever its names match. A
   walk keeps 16 records that withhold for the whole scan, then 16 more
   that each withhold the reads collected up to the latest attempt that
   refused their stream; past those it fails closed: every QR Code read
   the scanner did not parse, collected up to that attempt, is withheld
   unless a parse carries a matching account. Within these bounds what is
   withheld depends on the refusals and reads, not their order. The
   opposite-background walk starts with the first walk's records, each
   withholding for its whole walk (failing closed throughout when the
   first walk did), and with the parsed contents that protect reads.
   Withheld reads still count in `detections_found`.

   **Reconciliation.** Reads identical in symbology, `raw`, FNC1 position,
   evidence, observed segments and engine reading are kept once: engines
   are united; the earliest attempt, the first corners and the first
   geometry are kept, with the distinct places it was read at — a place
   whose centre lies inside a recorded one is the same symbol — at most 8.
   At most 64 such distinct reads are kept per walk, in attempt order;
   later distinct reads are counted, not reconciled.

   After the ladder completes, withheld reads are dropped; a re-derived
   grid joins as a read only when it contains an engine read of the same
   bytes with a matching account. Reads are grouped by (symbology, `raw`).
   A read *lies inside* a detection when the centre of its corner points
   lies inside the box of the detection's corner points (those of the
   earliest read that formed it); reads or detections without corners
   never fail this test. In each group:
   1. reads with located or no-ECI evidence and an observed FNC1 position
      form one detection per distinct (evidence, FNC1 position);
   2. a signalled read can join only the step-1 detections with a located
      list and its FNC1 position: if exactly one of those contains it, it
      joins that one; if several do, or none does, it joins nothing; if the
      group has no such detection at all, it joins the group's signalled
      detection for its FNC1 position, which the first such read opens;
   3. then reads with unobserved evidence and an observed FNC1 position,
      and after them reads with an unobserved FNC1 position, each against
      the detections formed before: a read joins the one detection with an
      agreeing account that contains it; with several it joins nothing;
      with none, the reads of that kind form one detection per published
      identity, unless a detection with that identity already exists — then
      they join nothing.

   A detection no engine read belongs to is not published. The result
   depends on the set of reads, not their order. Geometry comes from the
   detection's earliest read that carries it; engines are the union of its
   reads (a re-derived grid adds none, so `rescue` means the S5 stage
   only); its Kanji/Hanzi segments are the one distinct non-empty
   observation of its parsed reads (several distinct ones conflict,
   01-report rule 1d; none parsed → not observed). Each detection's content
   is interpreted once: located → `eci` entries; signalled → `eci: []`;
   otherwise no key; FNC1 first/second → `fnc1`. At most 16 detections are
   published: QR family first, then by earliest read.
7. **Probes match content identity, not text.** A score stress cell or an
   alpha envelope/palette probe counts as decoded when one of its reads
   would join the primary detection under property 6's accounts: same
   symbology, `raw` and agreeing FNC1 position, and compatible evidence —
   unobserved with anything; no-ECI with no-ECI; signalled with signalled
   or located; located with an identical list or with signalled. Corners
   are not compared.

## Acceptance

Every read the walk collects is weighed over the whole walk before it may
settle a stage, stop S5 or be published. A held read stays in the walk: it
protects reads it agrees with and keeps re-derivation running.

1. **Correction margin.** A read's margin is that of its own received
   word, when the scanner error-corrected that word itself (the `rqrr`
   grid, the re-read, S5): the least, over its blocks, of
   ⌊(npar − p − f)/2⌋ − e, with p from ISO/IEC 18004 Table 9, f erasures
   and e errors. A content — the reads of its symbology whose accounts
   agree — is refused when at least one of its reads has a known margin
   and every known margin is negative; its reads of unknown margin share
   the refusal. One read with a margin of zero or more admits the content.
   Only QR Code (Model 2) reads ever have a known margin, so this rule
   never refuses a content without one: every Micro QR, rMQR and non-QR
   content, and a QR Code content no parse measured. Errors alone reach a
   negative margin only on v1-L (more than 2 errors), v1-M and v2-L (more
   than 4). A margin of zero or more does not prove the correction right:
   a word miscorrected within the reserve is admitted (when the score
   measures that stream, a block whose errors spent all its parity
   codewords raises `low_correction_margin`, 06-hints.md — which on
   version 1, v2-L and v3-L no errors-only correction within the reserve
   does, 04-score.md § Synthetic UEC). The grid of an `rqrr` read with a
   negative margin is offered to S5 like an undecoded grid.
2. **Independent confirmation.** A read of UPC-E, DataBar, DataBar
   Expanded, or PDF417 at error-correction level 0, 1 or none waits until
   a read of the same symbology and bytes was made on a plane whose
   longest side differs from its own by a factor of 1.25 or more (the
   transforms and colour channels of one plane, and an inverted attempt,
   share its size). Right after the attempt that first reads such a
   content with no confirming read yet, the scanner decodes that symbology
   alone on the attempt's plane downscaled to ⌊3L/4⌋ (L its longest side),
   once per content and at most four times per walk, whatever the budget
   left and never after cancellation; the decode is an attempt of its own,
   its time counted in the stage, its reads in no `detections_found`.
   While the core crate admits rxing below 0.9.3 no DataBar (RSS-14)
   decode of that kind runs, and a DataBar is confirmed only by a read the
   walk makes anyway. Only S1–S3 read symbologies outside the QR family
   (S1 at the 512 px pyramid side, when the plane is larger; S2 and S3 at
   the engine-capped size), so in every profile a DataBar in an image
   whose longest side is under 640 px is never read on two planes 1.25×
   apart, and is refused. A read still waiting when the walk ends is
   refused.

## S5 — the erasure rescue (what `engines: ["rescue"]` means)

Trigger: S1–S4 published no detection (refused, withheld and held reads do
not count), but rqrr READ a grid + its format info on at least one attempt
(`get_raw_data` ok, `decode_to` err). Up to 4 candidates are kept (deduped
by version/ec/mask/polarity/orientation).

Per candidate:

1. **Unmask + de-interleave** the sampled bitstream (ISO 18004 §8.6 —
   exact gather, not the quirc round-slip).
2. **Per-codeword confidence**: worst |sample − threshold| margin over the
   codeword's 8 modules, sampled at module centers through the detection
   homography (center-submodule idea: Halftone QR, Chu et al. 2013).
3. **Erasure marking**: lowest-margin codewords (< 0.30 of half-span)
   become erasures, at most npar − p − 2 per block (p in step 5).
4. **Errors-and-erasures RS** (Forney 1965): erasure locator Γ, modified
   syndromes Ξ = S·Γ (Berlekamp-Massey on the e-shifted Ξ), combined
   locator Ψ = Λ·Γ, Chien roots, b=0 Forney magnitudes.
5. **Hard verification**: corrected blocks must re-check to ZERO syndromes
   AND the bitstream must parse as a structurally valid mode-segment stream
   under 01-report § Content (§ Refused streams). Per block, with t errors
   located, f erasures used and npar parity codewords, the scanner's guard
   is 2t + f + p ≤ npar, where p is the misdecode-protection count of
   ISO/IEC 18004:2015 Table 9 (3 on v1-L, 2 on v1-M and v2-L, 1 on v1-Q,
   v1-H and v3-L, 0 elsewhere); a block corrected with erasures must also
   leave two more parity codewords unspent: 2t + f + p + 2 ≤ npar. These
   two spare codewords are this scanner's choice, not a value taken from
   ISO/IEC 18004. A block damaged past what it can correct still passes
   them about once in 65 536 (2⁻¹⁶ per block): the rescue is
   refusal-biased, not infallible.

Measured yield (v5-H, centered gray disk): engines die past 20% radius;
rescue decodes through 30% (≈2.2× the occlusion area) — the
logo-over-center artistic class.

Cost: rescue work is per-candidate and bounded; it adds nothing to scans
that decode normally.

## Engine roles

| | rxing (zxing-cpp port; ZXing-lineage reader only when the port finds nothing) | rqrr (quirc lineage) | rescue (ours) |
|---|---|---|---|
| Robustness decode | ✓ (TryHarder + AlsoInverted) | ✓ (retries the mirrored grid) | S5 beyond-t recovery (Hanzi and structured-append streams are not rescued) |
| Geometry (corners) | — (points used internally) | ✓ (also grids it detected but could not decode, re-derived by the scanner) | from the candidate |
| version/ec/mask | ec only | ✓ (the orientation that decoded) | ✓ |
| Raw bitstream | — | ✓ (verified orientation; feeds UEC + rescue) | consumes |
| Payload bytes (`raw`) | original: port payload · fallback-reader codewords re-parsed | original: the scanner's parse of rqrr's grid; rqrr's bytes only when it decides nothing | original |
| ECI designators | located by the re-read or a re-derived rqrr grid; fallback reader parsed; else presence only (`]Q2`/`]Q4`/`]Q6`) | parsed from its grid | parsed |
| Kanji / Hanzi segments | observed by the re-read or fallback reader; else the port's reading may confirm a double-byte reading (rule 1e) | Kanji parsed · Hanzi ✗ (grid re-derived instead) | parsed |
| FNC1 symbols | ✓ (`]Q3`/`]Q4` first · `]Q5`/`]Q6` second) | ✗ (rejects mode 0x5/0x9; grid re-derived instead) | ✓ |
