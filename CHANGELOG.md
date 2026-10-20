# Changelog

All notable changes to this workspace. Every surface versions together — the Rust
crate + CLI, the npm node + wasm packages, the Python wheel, and the
Kotlin/Android · Swift/iOS · Flutter bindings.

## 0.10.1 — 2026-10-20

### Fixed

- The browser package includes a non-SIMD variant alongside the accelerated
  build. The additive `/auto` entry detects capabilities in its caller's realm,
  including workers, before selecting and downloading one binary. Initialization
  diagnostics remain separate from scan execution errors and valid empty reports.
  Existing root imports remain available; consumers opt in to automatic loading.
  Decoding, report and score contracts are unchanged.

## 0.10.0 — 2026-10-13

### Fixed

- Python source distributions carry the exact type stub and `py.typed` beside
  the relocated `pyproject.toml`, so wheels built from the sdist retain typing.
  Archive inspection rejects missing or changed project-root typing files.
- Swift xcframework archives carry `LICENSE` and `LICENSING.md` inside the
  bundle. The release helper refuses version drift; Swift and JitPack Cargo
  builds use the committed binding lockfile.
- The CLI's own packaged manifest retains rxing overflow checks for standalone
  `cargo install` builds; archive inspection rejects a missing or false guard.

### Changed

- **Exact QR content** (`versions.pipeline` 2 → 3) — a QR-family
  `content.raw` is now the original payload on every decode path: where
  the scanner can error-correct the grid a decoder read, its own parse
  decides the bytes. Declared ECIs decide `content.text`; unmarked Kanji
  and Hanzi segments read Shift_JIS or GB 18030 when that set also reads
  the surrounding bytes, UTF-8 when the whole payload is UTF-8, and
  `unknown` otherwise; Micro QR and rMQR Kanji read Shift_JIS when the
  decoder's own reading is exactly that decoding. Detections are the
  distinct contents the scanner can tell apart: every read is reconciled
  at the end of the walk by its bytes, FNC1 position, ECI declarations
  and where it lies — never by `text`. The ladder keeps going while every
  QR code found only signals an ECI it could not locate (also beside a
  retail barcode).
- **`versions.score_contract` 4 → 5** (one bump, with the margin cap
  below) — the UEC cap is measured even when `uec` is skipped, and on the
  orientation the decoder read (a clean mirrored code scores like an
  upright one), never from a stream that does not error-correct;
  `score_skip_checks` never moves `score.value` or the ISO overall. A
  judgment the wall-clock budget interrupted is absent (`score: null`,
  with `trace.budget_exhausted: true`) where 0.9 published a partial
  composite. Stress cells and alpha probes match content identity, so ECI
  and non-UTF-8 symbols score their cells. The alpha placement envelope's
  calibration honours the scan deadline: an envelope the budget cut is
  absent with `trace.budget_exhausted: true`, no longer conflated with a
  design no background decodes (0.9 ran that calibration unbounded).
- **CLI** — `--score-only` exits 3 with empty stdout when the budget
  interrupted the judgment of a decoded QR (0.9 printed a deflated value,
  exit 0); every-axis-skipped and internal-fault cases name their cause
  (exit 2). `--pretty` says why a score is missing and when the budget
  cut the scan (`budget … exhausted — not final` where no score line
  says it: the frame profile, or every stress axis skipped), prints
  `charset` and `fnc1` lines (a content with no verified text ends its
  charset line with `(raw: in the JSON report, run without --pretty)`),
  and shows controls and spoofing characters as `\u{…}`, a backslash as
  `\\` and a U+FFFD that is part of the text as `\u{fffd}`.
- **`trace.stages[].detections_found` still counts raw decoder hits** —
  hits the scanner later refuses or holds for confirmation included; a
  decode refused inside the decoder step is not a hit — an rqrr decode
  whose own grid parses malformed, an rxing read a read check refuses (a
  DataBar text that is not fourteen digits, a `]Q1` read whose bytes are
  not its text) or whose original bytes cannot be established. The reads
  of a confirmation decode are not counted either.
- **QR content without ECI** — unmarked 0x80–0x9F and binary payloads
  become one windows-1252 detection; reads only rxing made publish their
  original bytes (Micro QR `[e9]`: `[c3 a9]` utf8 → `[e9]` `é` latin1;
  Micro QR and rMQR 山田: `[e5 b1 b1 e7 94 b0]` utf8 → `[8e 52 93 63]`
  `山田` shift_jis); no byte-order-mark sniffing; Kanji with ASCII trail
  bytes (山田, テスト) read shift_jis, Hanzi gb18030, UTF-8 runs beside
  such segments `unknown`; the Kanji values 5949–5951 keep `9f fd`–`9f
  ff`, read `unknown`, one detection. FNC1, Hanzi and structured-append
  symbols one decoder cannot read gain geometry and score sections
  (`engines: ["rxing"]`); FNC1 symbols gain `content.fnc1` (GS1 Data
  Matrix including `]d5`, GS1-128); Data Matrix `]d3` / `]d6` and Code 128
  `]C2` data is AIM application data; same-text symbols with different
  bytes, declarations or FNC1 stay separate; a QR read whose declarations
  could not be observed (rare) shows non-ASCII bytes as U+FFFD
  (`unknown`). Short unmarked Latin-1 payloads (for example `© 2026`,
  `¡Hola!`), read as two detections in 0.9 through rxing's charset guess,
  are now one detection.
- **Other symbologies carry the bytes of their decoding engine's
  reading** — Data Matrix, PDF417 and Aztec `content.raw` is that
  reading's ISO/IEC 8859-1 encoding when every character fits, else its
  UTF-8: UTF-8 text such as `Café crème brûlée` or
  `https://…/日本語テスト` reads correctly and whole, mixed encodations
  keep all their data, and distinct Data Matrix symbols no longer merge.
- **ECI symbols** — one detection with the declared reading; unsupported,
  binary or undecodable declarations and unlocatable ECIs (`eci: []`)
  read `unknown`; mirrored and damaged-but-correctable symbols are
  located.
- **A melting RS margin now caps the composite** (part of
  `score_contract` 4 → 5) — a growing center logo used up the correction
  budget while every stress axis kept passing, so the score stayed flat
  until decoding failed without warning: at EC level H the
  `raise_error_correction` hint can never fire, and
  `low_correction_margin` only speaks at margin zero. The measured UEC
  margin now caps the value on a continuous slope below the healthy
  half-budget: 0.5 → 100 (no cap) · 0.25 (ISO D) → 70 · 0.0 (ISO F) → 40,
  the finder-damage floor, so a decode at the RS limit scores at most 40
  where the symbol reserves no misdecode-protection codewords. On
  version 1, v2-L and v3-L, which reserve them, the margin does not
  subtract them: an errors-only decode at the acceptance limit reads above
  0 and caps at 47 to 91 (spec/04 § Synthetic UEC). The cap applies only
  where UEC measures (decodes that carry the raw stream, the rqrr path);
  rxing-path artistic decodes report `uec: null` and keep their prior
  scores — read a null margin as unmeasured, never as healthy.
- **Releases publish from a version tag only, through trusted publishing,
  and upload exactly what they inspected** — crates.io and npm now
  publish through GitHub OIDC trusted publishing (no workflow reads a
  long-lived registry token; npm packages carry provenance attestations,
  as the PyPI wheels already did); every publishing job runs only for a
  `v*` tag, in its own GitHub environment, after checking that the tag
  names the version and that this changelog dates it. Each job reads
  back, in the job itself, the archives it uploads: the npm tarballs go
  up as packed, with no lifecycle script run; PyPI receives exactly one
  wheel per platform and the sdist; each crate upload is compared with
  its inspected archive byte for byte. Publishing jobs run only actions
  pinned by commit. A run from any other ref builds and inspects every
  archive and stops, and the npm main package goes up only once every
  platform package it pins is live. A live version on crates.io or npm,
  and every file PyPI already serves at the version, must match the
  inspected archive (PyPI may serve no other file there), every npm
  upload first checks that its operand is the very tarball inspected in
  the job, a tarball npm would read differently fails, and manifests and
  Python sources are bound to the checkout.
- **Source builds of the Python package need maturin 1.9.3 or newer** —
  the build requirement is now `maturin>=1.9.3,<2.0`, the first maturin
  that puts PEP 639 license files into source distributions.

### Added

- `content.charset` values `gb18030`, `eci` and `unknown` (`unknown` is
  the only lossy value, now and later); the optional `content.eci`
  (`[{designator, start}]`: located designators, `[]` when signalled but
  not located) and `content.fnc1` (`"first"` | `"second"`); Rust
  `EciSegment`, `Fnc1Position`, `Charset::is_lossy`, reader-only
  `Unrecognized` variants and `ScanConfig::computes_score`; Dart
  `Charset.gb18030` / `eci` / `unrecognized`, `isLossy`, `EciSegment`,
  `Fnc1Position`; `trace.budget_exhausted` (`false`: the report is final;
  absent on older reports: unknown).

### Fixed

- **Inputs larger than the engine cap keep their rescue decodes** — the
  S5 erasure rescue kept its candidate geometry in original input
  coordinates but sampled module confidence on the engine-capped plane,
  so for any input whose longest side exceeds `max_engine_side` (2048 px
  in every built-in profile) it read the wrong pixels, and a symbol only
  the rescue can recover was lost. Candidate geometry is now mapped into
  the capped sampling plane before sampling, and the reported `corners`
  stay in original input space. Transparent inputs under automatic
  background selection now follow the documented stage order: a
  successful rescue over the automatically chosen background settles the
  scan, where the opposite-background retry used to run instead (and
  report `fallback_used: true` when an engine read the other placement).
- **The Node loader enforces its own version, and loads without its
  manifest** — with `NAPI_RS_ENFORCE_VERSION_CHECK` set (and not `0`),
  the 0.9.0 loader compared every installed platform package against
  `0.8.1`, a literal napi-rs wrote when the loader was generated, and
  refused its own 0.9.0 binaries. The loader now compares against the
  package's own `package.json`, read only once the check is on: an
  installed platform package must carry this package's version, and a
  loader vendored or bundled without its `package.json` still loads
  with the check off (the default).
- **Every distributed archive carries the AGPL text** — none of the
  0.9.0 archives held a LICENSE. Both crates, the npm main, platform and
  wasm packages, the wheels and the sdist now carry the root text byte
  for byte (Python through PEP 639 `license-files`, with a
  `License-File` entry), and CI reads every archive back before
  anything publishes. The iOS xcframework carries license materials inside
  its bundle; the JitPack AAR remains a named exception.
- **rxing overflow-checks in the Python, Kotlin/Swift and Flutter
  builds** — cargo reads `[profile]` from the build root only, so the
  wheels and sdist builds, the UniFFI library and the Flutter crate
  compiled rxing without the workspace's `overflow-checks`, the guard
  that turns the crafted-image arithmetic underflow documented in 0.6.0
  into a caught panic counted in `trace.engine_panics` instead of a
  runaway allocation. Each of those build roots now restates it.
  The packaged CLI now restates it for `cargo install` as well.
- **Malformed streams are refused, whichever decoder reads them** —
  designators above 999999 or written `111xxxxx` (mirrored symbols
  included), FNC1 in both positions, out-of-range numeric or
  alphanumeric groups; a tolerant reading of the same symbol is withheld.
  The rescue reads FNC1 `%%` as `%`; the FNC1-second indicator is kept
  and never read as GS1; no more split ECI or 0x80–0x9F detections; no
  guessed charset for an unsupported ECI. The Python stub types `score`
  as `dict | None` (`NotRequired` from `typing_extensions`), and the docs
  say that reproducible scans need `budget_ms=0` / `--budget-ms 0`.
- **A plain port read whose bytes are not its text is refused** — rxing's
  zxing-cpp port reader could report a QR Code read under identifier
  `]Q1` with ASCII text whose raw bytes were another reader's shape
  (codewords, not the text). Such a read is now refused unless its bytes
  are its text, or decode to it behind a byte-order mark the way rxing
  decodes them. An empty text claims nothing of the bytes (it is how
  rxing renders a segment its decoder rejects, such as a Kanji pair
  5949–5951) and keeps the read.
- **A DataBar text is exactly fourteen digits** — a DataBar (RSS-14) read
  whose text is anything else is refused.
- **The misdecode-protection reserve of v2-L is two codewords** —
  ISO/IEC 18004:2015 Table 9 reserves two on version 2 at level L; the
  scanner reserved one.
- **The S5 rescue keeps two parity codewords unspent in every block it
  erases** — with f erasures a block keeps only npar − f checks, and a
  correction that spent them all could land on another valid codeword: a
  constructed v2-M symbol was rescued onto a different payload. A block
  corrected with erasures must now keep two parity codewords beyond the
  reserve (a wrong correction passes about once in 65 536), and the
  rescue marks at most npar − p − 2 erasures per block. Trade-off:
  rescues that spent the last parity codewords are refused too (the
  rescue-stress sweep loses 4 correct reads of 2304 in each of its
  uncapped and capped passes, 13 in its dual pass).
- **A QR Code content whose every measured correction went past the
  Annex B reserve is not published** — the scanner now measures a read's
  correction margin on its own received word wherever it error-corrects
  that word itself (the worst block's ⌊(npar − p − f)/2⌋ − e). A content
  whose every measured margin is negative is refused, with its reads of
  unknown margin: both engines sample the same damaged modules, so two
  engines agreeing on such a word are no independent evidence. A margin
  is negative in two cases: errors alone took the word past the reserve,
  which only happens on versions 1 and 2 at low EC levels, or rqrr
  decoded a grid that neither orientation error-corrects for the
  scanner's bounded decoder, which counts as negative at any version and
  EC level. One read keeping a margin of zero or more confirms the
  content. Port reads at EC level L or M are re-read on versions 1 and 2
  to measure their margin. Reads with no measured margin — every Micro
  QR and rMQR read, and any other QR Code read no parse measured — are
  published as before, and a margin of zero or more does not prove a
  correction right. Trade-off: a v1/v2 symbol so damaged
  that every read of it corrects past the reserve now reads as no
  detection (it was published, with a misdecode risk the standard's
  reserve exists to bound), and so, at any version, does a symbol whose
  only known margin is that of an rqrr decode of a grid that does not
  verify. On the ZXing blackbox QR sets this
  costs one exact read of 179: `qrcode-4/41.png`, a v2-L symbol rxing
  read with five corrections (margin −1); its `corpus-external.tsv`
  expectation moves from `match` to `blind`.
- **A content measured only on a re-derived grid meets the same margin
  rule** — a QR content whose only measured correction margin came from
  re-deriving a grid rqrr detects but cannot decode (FNC1 in either
  position, Hanzi, structured append) is now refused when that margin is
  negative, as the acceptance rules require: such an FNC1 symbol
  corrected one error past the ISO/IEC 18004 Annex B reserve (v1-M with
  five errors, v1-L with three) was published, possibly as another valid
  GTIN. A port read the re-derived grid carries is still re-read at its
  own corners for its margin (versions 1 and 2, EC level L or M), and
  one read with a margin of zero or more admits the content.
- **Barcodes the readers invent from noise need a second, independent
  read** — UPC-E, DataBar, DataBar Expanded and PDF417 at EC level 0, 1
  or none are published only once a read of the same symbology and bytes
  comes from a plane whose longest side differs by a factor of 1.25 or
  more. Right after the attempt that first reads one, the scanner
  decodes that symbology alone on the attempt's plane at three quarters
  of its size (at most one decode per content and four per scan walk).
  While the core crate admits rxing below 0.9.3, whose RSS-14 reader can
  underflow, no DataBar decode of that kind runs: a DataBar is confirmed
  only by a read the walk makes anyway. EAN-13, EAN-8, UPC-A, Code 128,
  Code 39, Code 93, Codabar, ITF and Telepen keep settling on their first
  read. In the rescue-stress sweep the 46 UPC-E and DataBar outputs rxing
  read out of QR modules are gone (wrong outputs 0 in every pass).
  Trade-off: a DataBar the walk reads at one size only is no longer
  published. Only the first three stages read symbologies outside the QR
  family, at the 512 px pyramid and at full size, so in every profile a
  DataBar in an image whose longest side is under 640 px is refused,
  until the core crate requires rxing 0.9.3 or later and its
  confirmation decode can run.
- **rMQR symbols are scored by probes that read rMQR** — the score cells
  and placement-envelope probes of a Rectangular Micro QR (rMQR) symbol
  decode in a pass restricted to rMQR, and that pass ran no reader
  (rxing 0.9.1's multi-use dispatcher has no rMQR arm), so from 0.9.0 on
  a clean rMQR scored 0 on every axis. The pass now runs the zxing-cpp
  port reader for rMQR alone.
- **Decoder panics caught inside score stress cells and
  placement-envelope probes are counted** in `trace.engine_panics`; how
  a cell is judged does not change.
- **After a contained internal fault of the score computation, the
  placement envelope is not computed** — it is reported absent, like any
  envelope that was not computed, so the fault does not read as a budget
  cut: `qrscan --score-only` exits 2 for it, not 3, unless the budget had
  already cut part of that judgment.
- **The Python and WASM bindings publish every `f32` report field as the
  decimal the JSON report prints** (`alpha.coverage` 0.612, not
  0.6119999885559082) — the core serializes those fields as the `f64`
  nearest to their shortest decimal. The JSON text keeps its digits; a
  value between 1e-6 and 1e-5, or from 1e13, is now spelled in exponent
  form (the same number).

### Upgrade notes

- TypeScript: exhaustive `Charset` switches gain three values. Dart: the
  new values shift `Charset.index` (`unknown` 3 → 5), and implementers
  need the new getters; the Flutter `scan` / `scanFrame` facade still
  runs at the profile's default budget in 0.10.0, so read
  `trace.budget_exhausted` there. Python: test `charset == 'unknown'`.
  Rust 0.9.x cannot read reports that carry the new values. Validate a
  report with the schema of the release tag its `versions.scanner`
  names. Pin `qrcode-ai-scanner~=0.10.0` and `cargo install
  qrcode-ai-scanner-cli --locked --version 0.10.0`. That `--locked`
  install builds rxing 0.9.1, the version the lockfile pins, with the
  `overflow-checks` guard declared in the packaged CLI root (see Fixed).
  Projects that gate on the score should re-measure their
  thresholds before upgrading.

### CI

- **Flutter builds consume one portable candidate** — the staging tool packs
  the current core once, validates its normalized manifest and sources, and
  vendors it inside the pub package. Host, Android, iOS and publishing jobs
  consume the same hashed archive outside the repository. The committed
  binding lockfile is preserved, and a core-content digest invalidates
  cargokit's cache when only the bundled core changes.
- **Warp boundary tests kill eight recorded mutation debts** — exact pixels
  cover the half-pixel edges and outside fill without changing production
  code. A focused before/after run establishes the eight kills from its raw
  artifacts; its failed collector was repaired separately. This focused
  evidence does not replace the complete deep gate.
- **The release tooling pins its napi CLI exactly** — the v0.9.0 npm
  publishing jobs failed partway because the unpinned napi CLI range
  (`^3.7.1`) resolved to 3.7.3, which rejects `--use-napi-cross` on macOS
  and Windows hosts and on musl targets. The flag is now set per build
  target (Linux gnu only), musl targets build through Zig and
  cargo-zigbuild (`-x`, the napi-rs template's recipe), and
  `@napi-rs/cli` is pinned at `3.7.3`.
- **npm publishing hands `npm publish` absolute tarball paths** — npm
  reads a relative `dir/file.tgz` as a GitHub repository (the wasm
  operand `npm-wasm/<file>.tgz` names `github.com/npm-wasm/…`), so both
  publishing jobs spell every operand as an absolute path, the same in
  the in-job record. An `operand-proof` job, which both need, parses
  every operand with the publishers' npm before any token exists, and
  after each upload the registry's integrity must match the inspected
  tarball.
- **No build cache is restored on a tag in the jobs whose archives are
  published** — the musl build installs Zig 0.14.1 from ziglang.org
  against its published sha256, with an empty global cache, rather than
  through `mlugg/setup-zig`, which restores the compiler from the
  Actions cache on a tag too, where a cache hit skips its signature
  check. `check-publish-guards.py` refuses a cache-capable step on tag
  runs in every job whose artifacts a publishing job downloads.
- **RELEASING.md names what older tags can still publish** — the tags
  v0.5.0 to v0.9.0 can still publish to PyPI through OIDC on a dispatch
  (the owner's option: required reviewers on the `release` environment),
  and three wheel build images are pulled by a mutable tag (an accepted
  residual; maturin-action takes a digest through its `container`
  input).
- **New: `xtask oracle`, a correctness oracle that judges contents, not
  texts** (its report says `qrscan-oracle/v2`) — a `corpus.toml` entry
  may carry its bytes and declarations (`raw_hex`, `eci`, `fnc1`; 31
  entries do, each checked against an independent reader). Such a truth
  is met by content identity: equal text over other bytes is another
  content, and a detection whose charset is `unknown`, or whose ECI was
  signalled but not located, meets it as `unread` — a rate of its own,
  never exact and never blocking. Units meet detections through a best
  matching, and the report states the rule of each truth kind. The tree
  state it records leaves out the files the run writes, and a corpus root
  it cannot walk is named as such, not by its absolute path.
- **New: `xtask bench`, a benchmark harness** (`bench/README.md`; its
  report says `qrscan-bench/v5`) — it compares the published 0.9.0 with
  this tree on latency, memory, allocations, throughput and WASM cost.
  Work is each side's share of its own reference walk, and an erroring
  reference walk counts as an error; a gated pair whose sides are not the
  declared base and candidate is refused; stopping or suspending the
  harness stops its workers, and a call that spans a stop is repeated,
  never counted; the host gate fails closed on its probes, its lock and
  its run directory. Its records name every run-time input it read
  (driver scripts, the priming fixture, every vendored image), alpha
  images, the budget flag and pairs cut at the same point, and a dry run
  pre-screens the share of A/B disagreements. The library worker records
  the candidate's `trace.budget_exhausted`; the driver scripts and the
  fixture run from copies under the run's `artifacts/`, and every image
  is hashed again after each segment, a change failing the run; a build
  stamp refuses a build outside its `--source`, and a run refuses a Node
  or WASM base whose stamp lacks that record; a cut knee bisection is a
  stated known limit; the report names its method by that label alone.
- **`corpus-report --external` and `scripts/zxing-blackbox.py` print
  ZXing's own 0° thresholds as their reference** — 157, where two
  outdated values gave 153. No verdict changes.
- **Two 0.9.x gates no longer hide failures** — the cargo-deny scan of
  the binding trees the workspace excludes (Python, UniFFI, Flutter)
  turned every failure into a warning, and under cargo-deny 0.20 even the
  usage error of its retired `check --config` form, so those dependency
  graphs went unchecked while the step stayed green. It now runs the
  pinned cargo-deny 0.20.2 in its current form, and a tool or usage error
  fails the step; findings stay report-only, per tree, in the job
  summary. The weekly mutation shards ran under `continue-on-error`,
  which read shards exiting with missed or timed-out mutants as a green
  run; each shard now records cargo-mutants' exit code, and the verdict
  job of the next entry judges them.
- **The weekly mutation suite runs in 85 shards, judged against a
  committed dispositions file** — 16 shards no longer finished within the
  job's time limit; the shard count comes from measured per-mutant costs.
  The shards are dealt round-robin: slice shards put survivor-dense code
  on a few shards (the slowest of 85 took 79.2 min of an 85-min limit,
  and the last one received no mutant). The weekly verdict fails on a
  baseline failure, an unfinished shard, or a missed or timed-out mutant
  that `.cargo/mutants-dispositions.toml` does not list as equivalent, as
  a structural timeout or as test debt. It accepts a shard cargo-mutants
  gave no mutant, and lists an equivalent that `.cargo/mutants.toml` also
  excludes as excluded and documented instead of stale. The fuzzing
  bridge's operator mutants are excluded like its replaced functions
  (nine of them had reached the verdict), no mutant is generated in the
  rqrr-only stub of `Binarized::black`, which an all-features build never
  compiles, and the two wall-clock exclusions follow their conversions to
  their current lines.
- **Twenty mutation-debt entries are killed and one bench test no longer
  races its shell** — unit tests pin the score-check names, the input
  limits at their exact values, the widest bit read, the GS1
  element-string truncation and Digital Link qualifiers, the
  stream-length check and when raw RGBA flattens; two GS1 separator
  survivors are argued equivalent, while two allocation mutants remain
  resource-behavior test debt
  (`.cargo/mutants-dispositions.toml`: 33 equivalent · 9
  timeout-structural · 21 debt); and the orphaned-worker test releases
  its shell only once the worker has taken its parent.

## 0.9.0 — 2026-07-20

### Changed

- **Transparent inputs flatten before decode** (`pipeline` 1→2,
  `score_contract` 3→4) — the engine used to read the RGB *stored under*
  the transparency (alpha was dropped at the input seam), which made the
  verdict exporter-dependent: the same visual design scored 100
  (white-under-alpha exports), 74 (canvas-style black-under-alpha) or
  "no detection" (dark modules over canvas transparency) depending on
  which tool wrote the PNG. Since this release the input composites over
  a DECLARED background before luma conversion and RGB extraction —
  every stage (ladder · score · UEC · ISO) sees the flattened image, and
  the stored under-alpha RGB can never influence the report
  (exporter-invariance is pinned by test). `auto` (the default) measures
  the design on its intended placement: dark content over white, light
  content over black, with an opposite-background retry on a
  zero-detection scan (`alpha.fallback_used`) so a mis-called mean can
  never produce a false "no detection". Opaque inputs are untouched —
  their reports stay byte-identical (the `alpha` key is omitted
  entirely, not `null`). Spec: 01-report § Alpha · 07-pipeline.

### Added

- `ScanConfig.alpha_background` on every surface, per the
  `score_skip_axes` precedent — core (`AlphaBackground` +
  `from_name`), wasm (`scan_image(..., alpha_background)`), Node
  (`alphaBackground` option, typed
  `"auto" | "white" | "black" | "none" |` hex), CLI
  (`--alpha-background`), Python (`alpha_background=` kwarg, stub
  updated), UniFFI/Kotlin/Swift (`alpha_background` arg with a typed
  `UnknownAlphaBackground` error). `#rrggbb` forces the host's real
  placement surface — the truest verdict when the embedder knows where
  the code will sit; `none` restores the drop-the-channel behavior as
  an escape hatch. Unknown values fail loud on every surface. (The
  Flutter facade parses the new block; its option arg lands with the
  facade's next options pass — it exposes neither budget nor skips
  today.)
- **The placement envelope** (`alpha.envelope`, Full profile) — the
  transparent-design question is not "does it scan" but "over which
  backgrounds does it keep scanning": five fixed neutral rungs
  (luma 0 · 64 · 128 · 192 · 255) plus one bisection probe per verdict
  boundary, each a quick decode in the primary symbol's own decode
  class. The report carries the probes (every endpoint a TESTED
  background, never interpolation), the contiguous `safe_luma` bands,
  and a `placement` verdict (`any` · `light_only` · `dark_only` ·
  `mixed` · `none`). Informational by contract: it never moves
  `score.value`. Skippable through the standard seam
  (`score_skip_checks: ["alpha_envelope"]` — `ScoreCheck` gains the
  variant on every surface).
- `Hint::AlphaBackgroundDependent { placement }` — fires when the
  envelope is narrower than `any`: pin a background layer in the
  artwork, or constrain placement to the safe bands. The generator/UI
  loop's alpha feedback.
- `report.alpha` block: `coverage` · `content_luma` · `mode` ·
  `background` (`"white"` / `"black"` / `"#rrggbb"`) · `fallback_used`
  · `envelope`. Schema, TS types, Dart mirror and the type-parity gate
  updated in lockstep; `qrscan --pretty` renders the flatten verdict
  and the envelope strip.
- `alpha_palette` on every option surface (`alphaPalette` /
  `--alpha-palette`) — the host's theme/brand colors probed by the
  envelope inside the SAME scan: per-color verdicts land in
  `alpha.envelope.palette` (request order, each at its BT.601 luma),
  instead of one full scan per candidate background. Palette colors
  never distort the neutral probes/bands/placement. Unknown colors fail
  loud (`white` · `black` · `#rrggbb` only — the flatten keywords are
  modes, not colors).
- `Hint::AddBackgroundPlate { color }` — the canonical REMEDY riding
  with the `alpha_background_dependent` diagnosis: add an opaque plate
  (rounded rectangle + quiet-zone margin) behind the symbol; `color`
  (`white` for dark content, `black` for light) maximizes contrast.
  Maps 1:1 to an editor's one-click fix.

- `score.weights_run` — the honesty integer: Σ contract weights of the
  axes that ran (100 = the full six-axis contract). A partial score now
  SAYS how much contract stands behind it; pre-0.9 reports parse
  leniently (default 0). `qrscan --pretty` renders "· 70% of contract"
  when partial.
- `score_preset` on every option surface (`scorePreset` /
  `--score-preset`): the two canonical postures by NAME — `design`
  (generated previews: skips perspective+rotation, keeps lighting) ·
  `capture` (all six). Sugar over `score_skip_axes`, mutually exclusive
  with an explicit list (rejects loudly on every surface) — N host
  integrations can no longer drift apart on hand-built lists.
- `axes[].refined_failed_at` — the knee, bisected once at Full depth:
  the tightest TESTED failing intensity ("blur 2.25", "435px", "63%").
  One extra probe per kneed ramp; the composite never reads it; absent
  when no refinement ran. The generate→scan→regenerate loop sees
  sub-cell progress instead of 2-4 point cliffs.

## 0.8.1 — 2026-07-10

### Fixed

- **The npm native package ships its loader** — `native.js`/`native.d.ts`
  are napi-generated and were gitignored; the publish job checks out fresh
  and never regenerates them, and npm silently drops missing `files`
  entries, so every fresh `npm i @supernovae-st/qrcode-ai-scanner` since
  0.5.0 failed at require time (`Cannot find module './native.js'`) — the
  per-platform binaries were fine, the 300-byte loader was the hole. Caught
  by the first true fresh-consumer smoke. The generated pair is now
  committed (stable: it derives from binaryName+targets, not code) and
  `npm pack` proves the tarball. The wasm package was never affected.

## 0.8.0 — 2026-07-10

### Changed

- **A perfect render now reaches 100** — the glare stress cell moves onto
  the DATA region (symbol centre). Its old placement (0.3w, 0.3h) saturated
  the top-left finder: a structural kill no design survives, silently
  capping every pristine render at 4/5 lighting (the universal 95/97
  ceiling — the axis measured the instrument, not the symbol; same class as
  the 0.6.0 rotation-cropping fix). Centred, the error-correction budget
  decides: absorbable on healthy symbols, lethal where a finder sits or the
  margin is thin. Pinned by real product fixtures: the builder's default
  render scores exactly 100; a pale styled template keeps real, explained
  losses.

### Added

- `AxisScore.failed_at` (wire, all surfaces) — the label of the first
  failed stress cell ("blur 2.5", "glare", "128px"…), `null` when every
  cell passed. A lost point always names its cell; panels render the
  reason, not just the count. Schema + TS + Dart types updated in lockstep
  (type-parity gate green). `AxisScore` loses `Copy` in the Rust API
  (String field) — the 0.x-breaking bump, per the 0.7.0 `ScanConfig`
  precedent.

## 0.7.2 — 2026-07-10

### Added

- `ScanConfig.score_skip_checks` on every surface — the section-level
  sibling of `score_skip_axes`, same seam, same posture: Node
  (`scoreSkipChecks` option, typed `ScoreCheck = "uec" | "iso15415"`),
  wasm (`score_skip_checks` positional arg on `scan_image`), CLI
  (`--score-skip-checks uec,iso15415`), Python (`score_skip_checks=`
  kwarg on `scan`/`scan_frame`, stub updated), UniFFI/Kotlin/Swift
  (`score_skip_checks` arg with a typed `UnknownScoreCheck` error). A
  skipped section is never computed (no UEC bitstream replay · no ISO
  parameter sweep), the wire carries `null`, and the UEC-driven hints
  (`low_correction_margin`, the thin-margin priority of
  `raise_error_correction`) structurally cannot fire. The composite
  `score.value` is axis-based and does not move — surface truth, never
  score surgery (spec/04 § skipping checks). Canonical host: the builder
  panel displays neither the margin nor the ISO parameters — it skips
  both and the report stops carrying what the product never shows.
  Unknown names fail loud on every surface. Parity pinned per surface:
  core unit (sections null · hints silent · value byte-identical ·
  UEC-skip nulls the ISO `unused_error_correction` parameter), uniffi
  unit, CLI e2e, node test.mjs, pytest.

## 0.7.1 — 2026-07-08

### Added

- `score_skip_axes` reaches every binding surface — same name, same
  semantics, per the `budget_ms` precedent: Node (`scoreSkipAxes` option
  on `scan`/`scanSync`, typed), CLI (`--score-skip-axes
  perspective,rotation`), Python (`score_skip_axes=` kwarg on
  `scan`/`scan_frame`, stub updated), UniFFI/Kotlin/Swift
  (`score_skip_axes` arg with a typed `UnknownStressAxis` error). v0.7.0
  had shipped it core + wasm only. Everywhere: unknown axis names fail
  loud (CLI exit 2, ValueError, typed FFI error) — a typo silently
  scoring all six axes would be the worst kind of drift. Parity pinned
  per surface: uniffi unit tests, CLI e2e, node test.mjs, pytest.

## 0.7.0 — 2026-07-08

### Added

- `ScanConfig.score_skip_axes` (+ wasm `scan_image(...,
  score_skip_axes)`) — integration config excluding stress axes from
  scoring. Skipped axes never run (their cells are never built — the
  builder's verify loop drops the 10 perspective/rotation cells), the
  composite renormalizes over the weights of the axes that ran, the report
  self-describes (`score.axes` carries only what ran), and axis-derived
  hints from skipped axes structurally cannot fire. Skipping all six yields
  no score at all (an axis-less value would be fiction). The canonical
  case: a generated preview has no capture geometry — builders skip
  `perspective` + `rotation`; photo-input surfaces keep the full six.
  `StressAxis::from_name` joins the string-config seam. Rust API note:
  `ScanConfig`/`ScanProfile` are `Clone`, no longer `Copy` (the skip list
  is a collection); both were already `#[non_exhaustive]`.

## 0.6.0 — 2026-07-08

### Added

- CLI `--budget-ms` — override the profile's wall-clock budget (0 =
  unbounded), same semantics as every other binding surface. The CLI was the
  last surface without the knob.
- QD-2 settled by measurement: `cargo xtask rescue-stress` (2304
  deterministic occlusion scans, no RNG, byte-identical reruns) shows the S5
  erasure rescue is refusal-safe — 82/82 rescue decodes correct, 77% refusal
  rate, zero miscorrections. The harness runs weekly in deep-checks with a
  hard zero-rescue-wrong gate. (`QRS_RESCUE_CELL=<substring>` filters the
  grid to a single cell — the repro tool that pinned the rxing OOM wrap.)
  Separately measured: the base rxing engine
  wrong-decodes ~1% at format-info-adjacent occlusion and carries no
  bitstream, so `low_correction_margin` structurally cannot flag that class
  (tracked as its own decision).

### Fixed

- **The rotation stress axis measured frame cropping, not rotation
  tolerance**: `warp::rotate` spun the image within its own canvas, so a
  full-frame symbol's corners — finder patterns included — left the frame
  from the very first 10° ramp step (a v10's corner radius is ≈0.62·w
  against the 0.5·w half-frame). A flawless symbol read `rotation 1/5`
  and its score carried a phantom −8 penalty for a rotation any phone
  handles; engines are rotation-invariant on clean symbols, so every one
  of those deaths was a probe artifact. The canvas now grows to the
  rotated bounding box (pixel-centre exact; f64 trig with a snap-epsilon
  so cardinal angles stay pure shape swaps), and out-of-source lookups
  paint true white background instead of clamp-streaking the source
  edges — the same fix makes the perspective cells' out-of-trapezoid
  corners honest. Pinned by a red→green test: a clean v10 must survive
  the full rotation ramp 5/5. Score VALUES rise for full-frame symbols
  (the old readings under-reported margin); the contract is unchanged —
  same axes, weights, ramps, caps, bands (`score_contract` stays 3, the
  UPC-E wire-value-bugfix precedent).
- **A crafted image can no longer OOM the host through rxing**: rxing 0.9.1's
  RSS-14 (1D) reader underflows a subtraction on a real occlusion cell, and
  the release-mode wrap drove a Vec that doubled itself toward a 14 GiB
  request — the root cause of the silent weekly-CI runner deaths, and an
  adversarial-image DoS candidate on the multi-format path. A
  `[profile.release.package.rxing]` `overflow-checks = true` override turns
  the wrap into the caught-and-counted panic the engine wrapper already
  records (`report.trace.engine_panics`). Proof: the pathological cell
  completes with zero ≥1 GiB allocations and the 2304-cell QD-2 grid stays
  byte-identical (rescue 82/82, wrong 0).
- `cargo add qrcode-ai-scanner` no longer serves a stale crate: crates.io was
  the forgotten fifth registry — nothing ever published to it and nothing
  gated the drift, so the README's first install surface silently served
  0.3.0 through two releases. Both 0.5.0 crates are live, and
  `crates-publish.yml` owns every future tag (existence-probe gated: re-runs
  resume instead of failing on the already-published half).
- JitPack (Kotlin/Android) v0.5.0 build died exit-127: `/opt` is not writable
  on the JitPack image and the quiet `unzip` swallowed the gradle-provisioning
  failure. Gradle now unpacks into `$HOME` with loud failure at every step,
  provisions its own Android cmdline-tools (the image ships none), and a
  native-count gate fails any AAR built without its native libraries instead
  of shipping a lying artifact. First working JitPack build lands with the
  next tag.

### Testing / CI

- cargo-deny findings in the flutter binding tree cleared: stale-lock updates
  (backtrace → 0.3.76 drops `adler` entirely, futures → 0.3.32 un-yanks) and a
  `[[licenses.clarify]]` pinning allo-isolate's real Apache-2.0 (repo LICENSE;
  metadata fix merged upstream, unreleased).
- Shard-0 harvest closed: builder limits-threading pinned (a builder that
  discards its config now fails QRS-002/003 assertions), engine panic tally
  consolidated into one `run_engine()` helper and pinned deterministically.
  cargo-mutants now runs under nextest (`test_tool` policy) so the wall-clock
  test group serialization applies to mutants baselines too.
- The full-crate mutation campaign closed: all 127 weekly-run survivors are
  now killed or individually proven equivalent — the decode ladder
  (merge/absorb identity, rescue candidate collection, stage gating), the
  warp stress synthesizers (tilt/rotate/shadow/glare geometry, hand-computed
  pixel pins), the rescue bitstream parser + the ISO Annex B protection
  table (pinned against an independent enumeration, never the code's own
  formula), and a nine-file sweep (charset, engine adapters, GF(256) log
  table, sampler, payload gates, report predicates, ISO parameters, axis
  folds, Berlekamp-Massey fold bounds — the last settled by a
  125,536-syndrome differential census). 20 exclusions are pinned in
  `.cargo/mutants.toml`, each with its argument inline — 18 argued
  equivalent, plus the wall-clock-shrink pair argued untestable (a
  machine-speed lower bound is the assertion the determinism doctrine
  forbids). The weekly deep-checks missed count is a true zero baseline,
  enforced across 16 step-timed shards.
- pub.dev publish exchanges OIDC before the Flutter toolchain lands
  (dart-lang/setup-dart pinned first — the v0.5.0 job hung 2 h on
  interactive auth) and is capped at 15 min. The FIRST publish stays a
  manual operator act by pub.dev design: automated publishing arms on an
  existing package.
- The weekly mutants runner deaths are closed as a class: disk exhaustion
  refuted by its own telemetry, the rxing 14 GiB wrap pinned (see Fixed),
  and the residual hole — two parallel test processes, each under the old
  12 GB per-process virtual cap, can together write past a 16 GB box —
  closed by bounding the aggregate instead: 6 GB ulimit ×
  `NEXTEST_TEST_THREADS=2` across 16 step-timed shards (run 28879086942 is
  the evidence trail).
- RUSTSEC-2026-0204 (crossbeam-epoch 0.9.18: invalid pointer dereference in
  the `fmt::Pointer` impl, reached here via rayon/criterion) cleared across
  all five lockfiles — workspace, fuzz, py, uniffi, flutter/rust — by
  updating to 0.9.20 the day the advisory published.
- `cargo xtask rotation-sweep` — decode-under-rotation truth on the zxing
  blackbox corpus, two tables: cardinal 0°/90°/180°/270° via exact
  index-permutation rotations (lossless by construction: a rotated column
  sitting below its 0° sibling is an engine orientation gap, never image
  damage), and arbitrary 15°/30°/45° via bilinear rotation into the grown
  canvas (interpolated — a trend line, never a gate). The external gate,
  the manifest pins and the README headline all measure at 0° only; this
  closes that blind spot as a non-gating dashboard. First cardinal table:
  170/171/170/171 of 179 — flat; the engines have no cardinal orientation
  gap on real photos.
- CLI end-to-end wire-contract tests: the shipped binary's real JSON output
  now validates against `spec/scan-report.schema.json` (jsonschema dev-dep)
  across eight decode branches — direct, upscale, EXIF-rotated, boost-rung
  artistic, morph-rung webp, retail GTIN, FNC1 element string, micro-QR —
  plus the NOT-FOUND report (empty detections is a wire shape too, exactly
  where absent-vs-null serde drift hides). The type-parity gate holds the
  TYPE surfaces identical; this is the first gate on a real binary's real
  OUTPUT, the layer where serde attributes live and where the Dart
  int/double truncation class actually bit. Cross-process determinism is
  pinned the same way: two binary runs must agree byte-for-byte modulo the
  documented wall-clock fields.
- The three stock-budget artistic integration tests now reserve every
  nextest scheduler slot (`threads-required = "num-cpus"`): the wall-clock
  group only serialized them against each other while the other ~300 tests
  starved their 4 s budget from the sibling cores — two different victims
  across three same-day runs, ~4.2–5.2 s under load vs ~1.3 s solo. Proven
  by three consecutive full-suite green runs.

## 0.5.0 — 2026-07-05

Correctness + release-trust hardening (night sweep 2026-07-05). Wire schema
unchanged (score contract v3); two wire VALUES corrected as bugfixes. First
mobile release: pub.dev + SwiftPM xcframework + JitPack all fire on this tag.

### Added

- DoS-hardening + latency knobs across the mobile/server bindings: UniFFI
  (Kotlin/Swift) `scan` / `scan_frame` gain `max_dimension` / `max_pixels`
  input caps and a `budget_ms` wall-clock override; Python gains `budget_ms`
  (its input caps landed earlier this cycle). Same names, same semantics as
  Node/WASM (`0` = unbounded — spec/02). All arguments default, so existing
  call-sites are untouched.

### Fixed

- **UPC-E `gtin`/`conformant` were wrong**: the check digit was computed over
  the compressed 8-digit form and the GTIN-14 padded from it. Now expands
  UPC-E → UPC-A first (GS1 rule), then check-digits and pads from the
  expansion.
- GS1 evidence sniffing no longer counts the *stripped leading* GS of a
  transmission artifact as element-string evidence.
- Flutter mirror: `UecReport.margin` and `StructuralReport.finderIntegrity`
  are `double` now — the previous `int` truncation read every real margin
  (0.85, 0.62…) as 0. Lands before the first pub.dev publish.

### Testing / CI

- Rescue path fuzzed at last: `fuzz_scan_full` (reaches S4 deep + S5 rescue)
  and `fuzz_rescue_bitstream` (adversarial bitstream → errors-and-erasures
  correction via a `cfg(fuzzing)` bridge). Weekly fuzz job repaired (missing
  gitignored corpus dir) and now runs all four targets.
- Corpus gains `expect = "fail"` frontier entries: 6 vendored blind-spot
  fixtures (multi-symbol collages · extreme 3D perspective · busy scenes);
  an expected-fail that starts passing fails the run loudly — capability
  gains are detected, never silent.
- Golden-value pins for `otsu_threshold` / morphology / `downscale_to`
  (the surviving-mutant cluster) + direct rqrr/rxing adapter tests + the
  UEC 0.25/0.24 grade edge.
- Mutant-harvest sweep to zero on the hot math: Berlekamp-Massey locator
  degree pinned exact on injected errors (0–3 plus a 90-pattern sweep across
  `npar` 4..=20), `windowed_extremum` clamp/identity edges, Otsu's
  negative-variance init and argmax weighting, `has_color` source-kind flag,
  and `finder_score`'s region-mean threshold. Second pass killed the
  quiet-zone / strided-window / zigzag-bounds survivors; the 13 remaining
  are individually proven equivalent and pinned in `.cargo/mutants.toml` —
  the weekly missed count is a zero-baseline signal now.
- Internal: the symbology substrate (GF(256) tables, version DB, zigzag
  walk, block deinterleave, grid sampler) moved out of `score::` into a
  dedicated `matrix/` module — the rescue path no longer imports scoring
  code. No API or behavior change (same test count green, wire identical).
- Threaded determinism pinned: one shared `Scanner` scanned from 4 OS
  threads under contention produces byte-identical reports vs a sequential
  baseline (wall-clock trace fields zeroed — the one documented
  nondeterminism). The three stock-budget artistic integration tests now run
  in a serialized nextest group, so a loaded machine no longer flakes them.
- External corpora pinned without vendoring: `corpus-external.tsv` commits
  sha256 + measured decode status for all 522 files (zxing blackbox 170/179
  @ 0°, gallery 17/27); `corpus-report --external` re-hashes + re-scans
  budget-free and exits red on regression AND capability gained. CI gains
  `pipefail` — a red exit piped through `tee` used to be swallowed.
- Type-parity gate v2: 8 enums · 15 contract structs · payload kinds ·
  hint tags, held identical across Rust ↔ TypeScript ↔ JSON-Schema ↔ Dart
  (the Dart mirror was previously ungated).
- Release trust: the version gate now covers the node package.json + flutter
  pubspec + tag==workspace (a forgotten bump used to become a silent
  non-release); the published wasm crate is now cargo-checked on every PR;
  cargo-deny enforces bans+sources on the workspace and scans the three
  excluded binding lockfiles (report-only).
- New MSRV CI job — whose first run proved the declared 1.87 floor had
  never compiled (let-chains stabilized in 1.88). **MSRV is now honestly
  1.88** across all manifests + the JitPack pin. Cargo.lock is committed
  (the workspace ships binaries and claims determinism; a floating dep
  graph contradicted both) and the workspace uses resolver 3, so re-locks
  are MSRV-aware by construction.

### Docs

- Prose truth-sync with the implementation: the erasure-rescue re-check
  rejects *inconsistent* corrections (a codeword-valued miscorrection is
  the UEC margin-0 class, flagged downstream); stress-cell probe set
  documented as direct + otsu + the baseline's deep rung (`CellProbe`);
  quiet-zone probe documented as the 2-module ring it is; the lighting
  set's no-knee-exit semantics clarified everywhere.

## 0.4.0

New binding surfaces + cross-binding consistency. The core decoding/scoring
contract is unchanged from 0.3.0 (same `ScanReport`, score contract v3).

### Bindings

- **Kotlin/Android + Swift/iOS** (UniFFI → JitPack / SwiftPM) — the same
  `ScanReport` as a JSON string. Mobile CI builds the AAR + xcframework.
- **Flutter/Dart** (flutter_rust_bridge → pub.dev) — typed `ScanReport` facade
  (sealed `Payload`/`Hint`, ISO 15415 / UEC cards), tolerant of unknown values.
- **Consistency:** every binding (py · node · wasm · uniffi · flutter) now
  surfaces the `[QRS-xxx]` wire code in errors (py + uniffi previously dropped it).
- Python: optional `max_dimension` / `max_pixels` caps on `scan` / `scan_frame`.

### Core

- UEC worst-block now tracks the lowest-margin block (not the most-errors block);
  all-clean symbols report a real capacity. Morph filter rewritten to an
  output-identical monotonic-deque pass.
- Dual-licensed: AGPL-3.0-or-later OR commercial (see `LICENSING.md`).

## 0.3.0 — 2026-06-12

Full rebuild ("Diamond-grade"): deterministic architecture, scoring contract
v3, GS1 awareness, ISO-informed grading, hardened bindings. Supersedes the
0.2.x exploration line.

### Decoding

- Deterministic decode ladder (S1 pyramid → S2 direct → S3 enhance → S4 deep
  → S5 rescue) replaces the v0.2 RNG brute force — same input, same result,
  always.
- **Every mainstream symbology** (19): the QR family (QR · Micro QR · rMQR)
  plus Data Matrix, Aztec, PDF417, MaxiCode, EAN-13/8, UPC-A/E,
  Code 128/39/93, Codabar, ITF, GS1 DataBar (+Expanded), Telepen. Every
  detection carries a required `symbology`; merge identity is
  (symbology, text); only the QR family carries geometry/UEC/ISO/rescue.
- **S5 erasure rescue** (Forney 1965 errors-and-erasures RS): grids the
  engines read but could not decode are re-decoded with low-confidence
  codewords marked as half-price erasures (`e + 2t ≤ d − p`). Measured:
  center-logo occlusion tolerance 20% → 30% radius on v5-H.
- Dual engines: rxing (ZXing lineage, robustness) + rqrr (quirc lineage —
  geometry, format metadata, raw bitstream). Cross-engine merge keyed by
  resolved text (kanji-safe), bitstream + format metadata adopted as a unit.
- 15 deep rungs: 12 empirical contrast boosts + 3 morphological closes — the
  blob/dot pixel template class no threshold/contrast transform recovers.
- Charset resolution: UTF-8 → isolated-byte Latin-1 short-circuit →
  Shift-JIS → windows-1252 (fixes "Québec" → "Qu饕ec" garbling; kanji runs
  keep the SJIS reading).
- Photometric polarity threaded end-to-end: light-on-dark symbols carry
  `meta.inverted` (measured) and their structural/ISO checks sample the
  correct polarity.
- Anti-DoS: dimension/pixel/engine-side caps, whole-scan wall-clock budget,
  decompression-bomb guard, 16-detection cap, engine panic isolation.

### Scoring (contract v3)

- Six survival-ramp stress axes (resolution · blur · contrast · perspective ·
  rotation · lighting) with structural caps (finder integrity, quiet zone).
- Synthetic UEC: ISO 15415 Unused Error Correction from the engine's own
  sampled bitstream via RS syndromes + Berlekamp-Massey — exact corrected-
  error counts, ISO grade bands. Flags real Reed-Solomon miscorrections
  (`low_correction_margin` hint at margin 0).
- ISO/IEC 15415-informed grade card (`score.iso15415`): Symbol Contrast,
  Modulation, Axial Nonuniformity, Fixed Pattern Damage, UEC — each
  `{value, grade}`, overall = lowest (the ISO rule). Parameters needing
  verifier hardware are reported absent, never faked.
- Machine-actionable hints: `raise_error_correction` · `increase_contrast` ·
  `enlarge_modules` · `fix_finder_pattern` · `restore_quiet_zone` ·
  `reduce_art_texture` · `low_correction_margin`.

### Payloads

- Typed classification: url · wifi · email · sms · tel · geo · me_card ·
  crypto (BIP-21/ERC-681) · v_card · v_event · text.
- GS1: FNC1-first symbols (`]Q3`/`]Q4`) parse as `gs1` element strings
  (GenSpecs subset: predefined lengths, check digits, CSET 82, dates) and
  GS1 Digital Link URIs (Syntax 1.6, Sunrise-2027 retail form) as
  `gs1_digital_link` — both with a `conformant` verdict and per-criterion
  `issues`. GS1 syntax in a plain QR (missing FNC1) is sniffed and flagged.

### Bindings & tooling

- Node (`@supernovae-st/qrcode-ai-scanner`): napi 3 AsyncTask on the libuv
  pool, AbortSignal, `maxDimension`/`maxPixels`/`budgetMs` options, full
  TypeScript types.
- Browser (`@supernovae-st/qrcode-ai-scanner-wasm`): `scan_image` +
  `scan_frame`, SIMD128, ~797 KB gz (all symbologies), typed `ScanReport` return, `budget_ms`
  override for UI-thread bounds.
- One canonical TS contract (`report-types.d.ts`) shared by both packages.
- CLI `qrscan`: JSON by default, `--pretty` (terminal-injection-sanitized),
  `--score-only`, exit codes 0/1/2.

### Spec & docs

- `spec/`: the normative contract (report wire format, profiles, QRS error
  catalog, score v3, payloads, hints, pipeline) + a JSON Schema and golden
  examples produced by the real binary — all CI-validated against the serde
  types (`tests/spec_golden.rs`) and cross-surface type parity
  (`scripts/check-type-parity.py`).
- `docs/`: Mintlify documentation site (quickstart, concepts, per-surface
  API reference, integration guides for the qrcode-ai.com editor and for
  agents).

### Measured accuracy (reproduce: `scripts/zxing-blackbox.py` · `scripts/batch-scan.py`)

- zxing blackbox qrcode-1…6: 170/179 exact-text match @ 0°.
- qrcode-ai.com production templates: 15/15 decoded.

## 0.2.2 — 2026-06-10

Last release of the exploration line (sync napi binding, RNG-based retry
scanner). Superseded by 0.3.0.
