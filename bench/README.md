# Scanner benchmark harness

`xtask bench …` measures what the correctness oracle does not: outer-call
latency, memory, allocations, throughput and WASM cost of the scanner,
**base** (the published 0.9.0) versus **candidate** (this tree), by the
benchmark protocol `qrscan-oracle/v1.4`. Correctness stays with
`xtask oracle`; the bench only records whether outputs agree.

Everything here is deterministic — no RNG, no resampling. Timing depends on
the host, which is why a timing run is gated on a calm host (below) and a
dry run never yields a verdict.

## Runtimes and modes

| runtime | what runs | base | candidate |
|---|---|---|---|
| `lib` | `Scanner::scan` in-process, in a generated worker | worker built against `qrcode-ai-scanner = "=0.9.0"` | worker built against this tree by path |
| `cli` | one `qrscan` process per scan | the published 0.9.0 CLI crate, built `--locked` | this tree's CLI |
| `node` | the napi package's public `scan()` in Node | the v0.9.0 package files with the addon rebuilt from the v0.9.0 source | this tree's package + addon |
| `wasm` | the wasm package's `scan_image()` in Node | the v0.9.0 package rebuilt from its source by the release recipe | a package built by `scripts/build-wasm.sh` |

Modes: `full` (preset budget), `full-unbounded` (`budget_ms: None` — what
every binding maps a budget of 0 to), `fast`, `frame`.

The frozen scope is what a complete run measures: the oracle's three sets
(zxing, gallery, vendored), every image, in all four runtimes and all four
modes with both variants — sixteen segments — plus the `lib` throughput
pass in `full` at 1, 4 and all logical cores, and the WASM cost (sizes and
cold starts) of both packages. A run that measures less is `partial`.

## Build parity

Every pair — library workers, CLIs, Node addons, WASM packages — is built
by the same toolchain with the same profile and recipe; a gated run refuses
(exit 2) a pair whose compilers, effective profiles, observed builds,
requested features, worker sources or binaryen recipes differ, whose base
and candidate are the same file, or whose Node or WASM driver did not load
— by its path in the package — the file the receipt hashes.

- **Library workers.** `bench prepare` writes two crates **outside the
  workspace**, each its own `[workspace]` root, with a lockfile seeded from
  the workspace `Cargo.lock` and the workspace release profile mirrored
  (`bench/worker/*.in`; a test keeps the copy equal to the workspace
  manifest). Neither the workspace manifests nor its lockfile change. Both
  include `xtask/src/bench/worker.rs` by path and report its sha256 at
  `hello`: a run refuses workers compiled from another `worker.rs` than its
  own, so both sides always come from one `prepare` of the same tree.
- **CLIs.** The published crate carries its own `Cargo.lock` and no
  profile, so `cargo build --release --locked` of the extracted crate gives
  the base; the candidate gets the same default release profile through
  `--config` overrides of the workspace profile.
- **Node addons.** The base addon is rebuilt from the v0.9.0 source — the
  `git archive` of the release commit, extracted outside the workspace by
  `bench prepare-base`, built with its own `Cargo.lock` (`--locked`) and
  release profile on the candidate's toolchain. The published npm addon
  stays a correctness reference.
- **WASM packages.** The base package is rebuilt from the same v0.9.0
  source by its own `scripts/build-wasm.sh` — the release recipe:
  `wasm-pack` with the wasm-bindgen of its lock, `RUSTFLAGS="-C
  target-feature=+simd128"`, then `wasm-opt -O3 --enable-simd
  --all-features` of binaryen `version_130` — on the candidate's toolchain.
  A pair whose embedded compiler, wasm-opt version or flags, RUSTFLAGS,
  module recipe (producers, wasm-bindgen, SIMD) or build environment
  differ is refused; the published npm package stays a correctness
  reference.
- **Build manifests come from the build.** `bench stamp` runs the build
  itself and writes, next to the artifact, `<file>.build.json` (a package
  directory: `build.json`): `rustc -vV`, the compiler commits embedded in
  the binary, the effective release profile with the `--config` overrides
  the command passed, the features it asked for, the lockfile, the source
  with its dirty flag, and what cargo's JSON messages report — how the
  artifact's unit, the scanner and rxing were compiled (opt-level,
  debuginfo, debug assertions, overflow checks, features) — with the
  codegen-affecting environment (`*RUSTFLAGS*`, `CARGO_PROFILE_*`, `RUSTC`
  and its wrappers, as hashes). A WASM stamp records `wasm-opt --version`,
  the script's wasm-opt flags and RUSTFLAGS, and the script's hash. Paths
  in the recorded command are cut to their last component. `bench
  prepare` and `bench prepare-base` carry manifests into the package
  directories they assemble; `bench inspect <path>` prints what a run
  records.
- **Inputs.** The harness pins the frozen inputs: the sha256 of the
  external corpus manifest (`corpus-external.tsv`) and of the vendored
  manifest (`corpus.toml`), and each set's image count (zxing 179, gallery
  161, vendored 41). A gated run refuses (exit 2) inputs off those pins —
  either manifest's sha256, a set's listed count, or a set measured whole
  whose selection is not the frozen set; a dry run records them, and a run
  off its pins never ends `completed`.

## Method

- Per image: one warm-up call per variant, then five timed calls each in
  `A B B A` blocks — A first on even-indexed images, B first on odd ones,
  so a drift along the slots cancels within a class; every sample records
  its slot. Before its first image (and after every respawn) each worker
  scans `fixtures/clean/gen_v2_m.png` once, uncounted.
- **Reference walks.** In a budgeted mode (`full`, `fast`, `frame`) every
  image and variant first gets one uncounted call of the same profile
  without a budget, under the unbounded caps. A timed call is **truncated**
  when its walk — stages, Σ `transforms_tried`, detections — or its
  judgment — the score's composite, contract weights, and per axis the
  cells passed of those planned, a failing cell named — differs from that
  reference. A call that completes the reference walk is complete whatever
  its time: the ladder reaching the budget is an overrun, information only.
  An image's status is its median call's (the middle two for an even
  count).
- Per class — the oracle's groups (zxing suites, gallery subdirectories,
  vendored categories) and its three sets: p50 / p95 / max of the per-image
  medians by nearest rank over the valid pairs (`all_complete` keeps every
  image a variant completed); `trace.total_ms` as a secondary column only.
- Valid pairs only: an image enters a paired test only when both variants
  completed every call — reference walk, warm-up and timed calls — without
  error, with one output over every call, the two variants agreeing.
  Excluded pairs are counted by reason (resource failure, error,
  incomplete, no reference, unstable, disagreement) in every class row and
  the status line.
- **Latency** is judged on the valid pairs whose median calls are complete
  on both sides: per-image log-ratios ln(B/A); the Hodges-Lehmann estimate
  with the exact Wilcoxon signed-rank 95 % interval. A regression is
  declared only when the interval's **lower bound** exceeds ln(1 + δ). The
  p95 verdict uses the same test on the tail — the k = ⌊n/10⌋ pairs with
  the largest per-image geometric mean of A and B, a choice that favours
  neither side; k ≥ 6 is required, so p95 is testable from 60 pairs. Fewer
  than six pairs give no interval: *not testable*, never a pass.
- **Budget.** `budget_bound` labels the valid pairs whose median call is
  truncated on a side — it never suppresses the latency verdict of the
  complete ones. Per class (six valid pairs at least), an exact one-sided
  McNemar test over the valid pairs compares truncation status, candidate
  only against base only: p < 0.05 (decided in exact integers) is a
  **truncation regression**; the same test on lost judgments is a
  **lost-judgment regression**. **Work** — the Hodges-Lehmann interval of
  ln(B/A) of the per-image Σ `transforms_tried` — is judged on the pairs
  where a side was truncated only, a regression when its upper bound is
  below −5 %.
- δ: p50 5 %, p95 10 %, peak memory 10 % (median shift and tail), WASM gzip
  / brotli size 2 %, WASM cold start p50 10 %, work 5 %.
- **Memory** is judged in `full-unbounded` only, where both sides walk the
  same deterministic path; in budgeted modes it is information. `lib`
  counts the heap of the warm-up call through a counting global allocator
  (net peak, allocation calls, bytes still allocated once the report is
  dropped); `cli` reports each process's peak RSS (`wait4`). Any call above
  512 MiB is flagged `over_envelope`. Node and WASM per-image memory is
  *not testable*; their worker peak RSS — never counting an incarnation a
  cap ended — and the WASM linear-memory high-water mark are reported per
  segment, A versus B.
- Overruns are counted per image (its median slower than the preset
  budget, Wilson 95 % over images); call-level counts carry no interval.
- Throughput (`lib`): every selected image once through a queue of image
  indices (bound 2 × threads — the images are preloaded, so in-flight scans
  and memory scale with the thread count, not the queue) on 1, 4 and all
  logical cores, `A B B A` passes, fresh workers per thread count. Images
  per second count completed scans only, over the pass's whole time (failed
  jobs' time included); the B/A ratio of the two medians is unpaired — the
  variants may complete different jobs — and no δ is declared for it.
- WASM: raw, gzip-9 and brotli-11 size of the packed `.wasm` (Node zlib);
  the cold start — compile, instantiate and the first scan of the fixture
  in a fresh Node process (V8 compiles lazily, so the first scan pays the
  code generation) — one warm-up then 12 `A B B A`-paired processes per
  variant. Compile and instantiate alone are information.

Known limits of what a report shows: an engine retry that re-flattens an
alpha image between two walks spends time `total_ms` does not cover (the
walk comparison still sees a truncated retry), and a lighting set that
0.9.0 cuts right after a failing cell, with every remaining cell failing,
keeps its passed count — that one cut is invisible in the judgment.

## Resource caps

Every scanned process and worker runs under caps the harness enforces, dry
runs included: a watchdog samples its memory — and the kernel's high-water
mark, so a spike between two samples is still seen — and its wall time
every 100 ms, and kills its process group past **1024 MiB**
(`--rss-cap-mib`, never higher) or past **max(10 × the preset budget,
60 s)** per call (60 s unbounded and for reference walks; a throughput pass
gets the per-image cap × ⌈images / threads⌉, and its worker a memory cap
of a quarter of the per-call cap per thread, at most three per-call caps —
256 MiB per thread, at most 3072 MiB, at the default). A call that returns
with a kernel peak above the cap, or after its wall cap, is a failure all
the same; so is a worker a cap or a signal ends at `quit`. The child is
reaped with `wait4`, keeping its peak RSS. A failed call is a
`resource_failure` (rss, wall, signal, exit, unsampled — with its peak and
wall time), never a timing, memory, overrun or decode sample; the worker
is respawned and the run continues; the class and the run are `failed`
(exit 1).

`bench run` starts only where the caps hold — 64-bit macOS or Linux, with
process memory readable — and refuses to start anywhere else (exit 2).

The caps outlive the harness:

- every child runs in its own process group; the harness kills groups,
  never a pid alone;
- `bench run` handles SIGTERM, SIGHUP and SIGINT by killing every live
  child's group before it exits (128 + the signal);
- workers — the generated library workers and both Node drivers — exit
  within about 50 ms once their parent changes, even mid-scan;
- one-shot children (CLI scans, WASM tools) carry a kernel CPU-time limit
  at their wall cap and write no core file;
- a measured child whose first memory sample fails is killed at once and
  never used (the gate's system probes still run, under their wall cap:
  macOS `ps` is setuid and hides its memory), and only Mach-O or ELF
  executables run as measured children;
  `--node-bin` is resolved once to node's own path (`node -p
  process.execPath`).

Stop a run with SIGTERM, SIGHUP or SIGINT (`kill <pid>`, Ctrl-C): the
handler kills every child's group first. SIGKILL cannot be handled — the
workers then leave on their own, and a CLI scan already running ends by
itself or once it has used its CPU-time limit. Children never inherit `RUST_BACKTRACE`,
`RUST_LIB_BACKTRACE`, `NAPI_RS_NATIVE_LIBRARY_PATH` or `NODE_OPTIONS`;
their stderr is drained on its own thread and reduced to counts, `QRS-`
codes and `file.rs:line:column` panic locations.

## Host gate

A timing segment starts only when the one-minute load average is at most
0.5 per logical core (6 on 12 cores), no sibling build runs (a cargo /
rustc process in any worktree of this repository, or carrying the `qrscan`
/ `qrcode-ai-scanner` markers — arguments are read to classify, never
printed), the host is on AC power outside Low Power Mode, swap use is at
most 8 GB (8 × 10⁹ bytes), and the run holds the host-wide bench lock
(`/var/tmp/qrscan-bench.lock`, outside the cleaned `/tmp`). A run takes the
lock before anything else — before node, the images, the environment
probes and the hellos — and holds it to the end; the lock file's inode is
re-checked at every reading. Readings fail closed: an unreadable load,
process table, swap or power state, a probe past its cap, or a replaced
lock file closes the gate. The gate is read before every segment, every
reference-pass image, every image, the WASM sizes, every cold start, every
10 s of a throughput pass, and after each; a pass's own load counts against
it only as much as it can have built, T·(1 − e^(−t/60)) for T busy threads
after t seconds. A segment that crosses the gate is discarded whole and
retried (three attempts in all), never adjusted.

A dry run enforces only the host floor: it refuses to start, and stops,
while load1 > 30, swap use > 8 GB or another bench run holds the lock; once
stopped it starts no new phase.

```sh
xtask bench gate                # exit 0 open, 3 closed
xtask bench gate --wait 3600    # poll until open or the deadline; holds the host lock while it waits
```

A gated run starts only in a quiet window the operator declares (no other
build of this repository; other work on the host informed). Build the
harness once, in a slot-checked development-profile step, and run the
binary directly — `cargo run` would compile first, and no cargo process
would show the run to others.

## Commands

```sh
# 0. the harness, once (dev profile; <target> is the cargo target directory)
cargo build -p xtask --locked
X=<target>/debug/xtask

# 1. the candidate Node addon, built and stamped by one step
$X bench stamp --source . --artifact <target>/release/libqrcode_ai_scanner_node.dylib \
  -- cargo build -p qrcode-ai-scanner-node --release --locked

# 2. worker crates (+ the candidate Node package), each worker built and stamped
$X bench prepare --work <dir outside the workspace> \
  --node-addon <target>/release/libqrcode_ai_scanner_node.dylib
$X bench stamp --source <dir>/worker-base --artifact <target>/release/qrscan-bench-worker-base \
  -- cargo build --release --manifest-path <dir>/worker-base/Cargo.toml
$X bench stamp --source <dir>/worker-candidate --artifact <target>/release/qrscan-bench-worker-candidate \
  -- cargo build --release --manifest-path <dir>/worker-candidate/Cargo.toml

# 3. base sources: the v0.9.0 git archive (and the published CLI crate);
#    prepare-base prints the build and stamp of each base artifact
$X bench prepare-base --work <dir> [--cli-crate qrcode-ai-scanner-cli-0.9.0.crate]

# 4. a dry run: gate recorded, not enforced; caps and host floor enforced; exit 3
$X bench run --dry-run --per-group 1 --reps 2 --out <fresh dir> \
  --lib-base … --lib-candidate … --cli-base … --cli-candidate … \
  --node-base … --node-candidate … --wasm-base … --wasm-candidate …

# 5. the gated measurement, in a declared quiet window: waits for the lock and the gate up to the deadline
$X bench run --wait-gate 14400 --out <fresh dir> …
```

Run every build on its own, one cargo process at a time.

Exit codes: 0 completed — every segment, thread count and WASM cost of the
declared frozen scope measured with both variants, nothing regressed, no
set row caught by the exclusion rule · 1 a regression (latency,
truncation, lost judgments, work, memory, size, cold start), or a failure
(resource or protocol) · 2 usage, configuration, an unfit pair, inputs off
their pins, a worker of another protocol, or an `--out` that already
holds something · 3 refused by the host lock, the gate or the host floor,
`partial`, or an exploratory dry run (never a pass). A run is `partial`
when it is a subset of the frozen scope (per-group sampling, fewer sets,
modes or runtimes, a runtime without both variants, no throughput in
`full`), when anything of the declared scope was not measured, or when a
set row has fewer than six valid pairs or more than 10 % of its images
excluded; the excluded counts and any A/B disagreement in a budgeted mode
appear in its status line.

## Receipts

A run directory — always a fresh one; an existing non-empty `--out` is
refused — holds `receipt.json` (method, caps, environment with the
harness binary's sha256, inputs with their pin check, artifact hashes with
their build manifests, parity, gate log, per-class summaries, resource
failures, verdicts — a `not_testable` row, with why, for every metric of
the frozen scope the run did not measure — and the status detail),
`receipt.md` (the short tables), `samples.jsonl` (every completed call —
reference walks, warm-ups, timed calls — with its slot, walk and
truncation facts) and `images.jsonl` (per-image summaries and pair status),
plus `artifacts/` — the copies the run executed, hashed in the receipt.
Every receipt file is created new and its sha256 printed.

Decoded text never leaves memory: workers send it as hex over their pipe
and the harness hashes it on arrival. Receipts carry (symbology, text
sha256, length) and corpus-relative paths only, and failure notes carry
error codes, never messages. A short payload can be guessed back from its
hash, and the executed binaries under `artifacts/` embed build paths, so a
run directory stays local — `bench/out/` and `bench/work/` are ignored; the
throughput image list lives in the system temporary directory and is
deleted after the passes.
