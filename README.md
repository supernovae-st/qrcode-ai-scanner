<div align="center">

# QR Code AI Scanner

**Decode the undecodable · and know your margin.**

QR decoding + scannability scoring for artistic, AI-generated, and
photo-captured QR codes. Deterministic, sovereign, pure Rust.

[![crates.io](https://img.shields.io/crates/v/qrcode-ai-scanner?style=flat-square&logo=rust&logoColor=white&label=crates.io&color=dea584)](https://crates.io/crates/qrcode-ai-scanner)
[![docs.rs](https://img.shields.io/docsrs/qrcode-ai-scanner?style=flat-square&logo=docsdotrs&logoColor=white&label=docs.rs)](https://docs.rs/qrcode-ai-scanner)
[![npm](https://img.shields.io/npm/v/@supernovae-st/qrcode-ai-scanner?style=flat-square&logo=npm&logoColor=white&label=npm&color=CB3837)](https://www.npmjs.com/package/@supernovae-st/qrcode-ai-scanner)
[![PyPI](https://img.shields.io/pypi/v/qrcode-ai-scanner?style=flat-square&logo=pypi&logoColor=white&label=PyPI&color=3776AB)](https://pypi.org/project/qrcode-ai-scanner/)
[![License](https://img.shields.io/badge/license-AGPL--3.0%20or%20Commercial-blue?style=flat-square)](LICENSING.md)
[![CI](https://img.shields.io/github/actions/workflow/status/supernovae-st/qrcode-ai-scanner/ci.yml?style=flat-square&logo=github&label=CI)](https://github.com/supernovae-st/qrcode-ai-scanner/actions)

**Part of the [QR Code AI](https://qrcode-ai.com) ecosystem**

</div>

---

## Install · pick your surface

One Rust core, **six published surfaces** · the five below plus Kotlin/Android
live on JitPack (mobile paragraph under the table) · all return the same
versioned [`ScanReport`](spec/) contract.

| Surface | Package | Install | One-liner |
|---|---|---|---|
| 🦀 **Rust lib** | [`qrcode-ai-scanner`](https://crates.io/crates/qrcode-ai-scanner) · [docs.rs](https://docs.rs/qrcode-ai-scanner) | `cargo add qrcode-ai-scanner` | `Scanner::builder().build().scan(input)?` |
| 💻 **CLI** | [`qrcode-ai-scanner-cli`](https://crates.io/crates/qrcode-ai-scanner-cli) | `cargo install qrcode-ai-scanner-cli` | `qrscan image.png --pretty` |
| 📦 **Node** (server) | [`@supernovae-st/qrcode-ai-scanner`](https://www.npmjs.com/package/@supernovae-st/qrcode-ai-scanner) | `npm i @supernovae-st/qrcode-ai-scanner` | `await scan(buffer)` |
| 🌐 **Browser** (WASM, automatic SIMD/compatible) | [`@supernovae-st/qrcode-ai-scanner-wasm`](https://www.npmjs.com/package/@supernovae-st/qrcode-ai-scanner-wasm) | `npm i @supernovae-st/qrcode-ai-scanner-wasm` | `const s = await loadScanner(); s.scan_image(bytes, "fast")` |
| 🐍 **Python** | [`qrcode-ai-scanner`](https://pypi.org/project/qrcode-ai-scanner/) | `pip install qrcode-ai-scanner` | `import qrcode_ai_scanner as q; q.scan(png_bytes)` |

Per-surface guides in [`docs/`](docs/) · code examples in the Quick start below.

**📱 Mobile + cross-platform:** Kotlin/Android is **live on JitPack**
(`com.github.supernovae-st:qrcode-ai-scanner:v0.10.1` — use the latest tag).
Swift/iOS (UniFFI → SwiftPM) and Flutter/Dart (flutter_rust_bridge →
pub.dev) are built and CI-green, returning the same `ScanReport`.
Consult each binding's README for the current install path:
[`bindings/kotlin/`](bindings/kotlin/), [`bindings/swift/`](bindings/swift/), and
[`bindings/flutter/`](bindings/flutter/).

## Why

AI-generated and artistic QR codes break standard scanners: damaged finder
patterns, low module contrast, art texture over data modules. This library
is built specifically for them · and it doesn't just say *"decoded"*, it
tells you **how much margin** the code has before it stops scanning in the
real world.

Seven things this library ships together:

1. **A deterministic multi-engine decode ladder** tuned on artistic
   corpora (rxing + rqrr, curated preprocessing rungs · no RNG in the
   scanner: on a given platform, same input, configuration and build,
   same report — unbounded, or whenever the report says the budget cut
   nothing, `trace.budget_exhausted: false`; PDF417 excepted).
2. **Score contract v5** · survival ramps across six stress axes
   (resolution · blur · contrast · **perspective** · **rotation** ·
   **lighting** · the documented blind spots of naive scorers), with
   structural caps on finder damage and quiet-zone violations and the
   error-correction margin cap — a whole judgment or none: a budget never
   ships a partial score.
3. **Synthetic UEC** · the ISO 15415 *Unused Error Correction* margin,
   computed from the stream of the orientation the decoder verified, via
   RS syndromes + Berlekamp-Massey, only when it error-corrects. The real
   "how close to failure is this code" number; below 0.5 it caps the
   score. ISO's misdecode reserve is not subtracted, so on version 1, v2-L
   and v3-L a block with errors reads above ISO's value (spec/04
   § Synthetic UEC).
4. **An ISO 15415-informed grade card** (`score.iso15415`) · Symbol
   Contrast, Modulation, Axial Nonuniformity, Fixed Pattern Damage and UEC,
   each `{value, grade}` in the ISO bands, with `overall` = lowest
   measured parameter (the ISO rule). Honest by construction: parameters
   that NEED verifier hardware (Grid Nonuniformity, Reflectance Margin) are
   reported absent, never faked.

5. **An erasure-rescue decode stage** · when both engines give up, the
   scanner re-decodes the sampled bitstream itself with errors-and-erasures
   Reed-Solomon (Forney 1965): low-confidence modules (logo zones, art
   texture) become half-price erasures (`e + 2t ≤ d − p`). Measured: center
   occlusion tolerance grows from 20% to **30% radius** (2.2× the area) on
   v5-H · exactly the artistic logo-over-center class.

6. **Alpha-aware scanning + the placement envelope** · a transparent PNG
   has no one background, and naive scanners read whatever RGB the export
   tool left under the alpha (the same design decodes or dies depending on
   the exporter). Transparent inputs flatten over a declared background
   (`auto` picks the design's intended placement; `#rrggbb` forces the
   host's real surface), and the report answers the actual product
   question — *over which backgrounds does this design keep decoding* —
   with tested probes (`alpha.envelope`: safe luma bands + a
   `light_only`/`dark_only`/`any` placement verdict, every endpoint a
   measured background, never interpolation).

7. **Exact content** · for the QR family, `content.raw` is the payload's
   original bytes on every decode path, with its ECI and FNC1
   declarations read once; other symbologies carry their decoding
   engine's reading re-encoded (ISO/IEC 8859-1 when every character fits,
   else UTF-8) and report no ECI. `charset: "unknown"` is the only lossy
   reading. Detections are the distinct contents the scanner can tell
   apart, never keyed on text. A read the evidence contradicts is refused:
   a malformed stream, a QR Code content whose every measured correction
   went past the standard's misdecode reserve (an `rqrr` decode of a grid
   the scanner cannot error-correct counts as one, at any version), an
   unconfirmed barcode a reader could invent from noise. Reads the scanner
   cannot measure (Micro QR, rMQR, other QR Code reads no parse measured)
   are published as read, and a correction inside the reserve can still be
   wrong: when the score measures the stream, a block whose errors spent
   all its parity raises `low_correction_margin` — which no errors-only
   correction inside the reserve does on version 1, v2-L and v3-L.

Plus machine-actionable **hints** (`raise_error_correction`,
`fix_finder_pattern`, `low_correction_margin`, …) · the feedback loop for
generators and AI agents: *generate → scan → act on hints → regenerate*.

And **GS1 awareness** (Sunrise 2027-ready): FNC1-in-first-position symbols
(`]Q3`/`]Q4`) come back as a parsed `gs1` payload (AI element strings, GTIN,
check-digit + format validation), and GS1 **Digital Link** URIs are
recognized and validated (path order, GTIN-14, qualifier formats) as
`gs1_digital_link` · with a `conformant` verdict and per-criterion `issues`.
Scoring is **ISO-15415-informed** (the UEC margin uses the ISO bands and
exact RS error counts) · see [docs/SCORING.md](docs/SCORING.md) for the
parameter mapping and the honest line between software diagnostics and
certified hardware verification.

## Quick start

```rust
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};

let scanner = Scanner::builder().profile(ScanProfile::Full).build();
let report = scanner.scan(ImageInput::encoded(&image_bytes))?;

match report.detections.first() {
    Some(d) => println!("{} (score {:?})", d.content.text, report.score),
    None => println!("no QR found"), // a valid outcome · not an error
}
for hint in &report.hints {
    println!("hint: {hint:?}"); // feed these back to your generator
}
```

Profiles: `Full` (quality gate, ~4s budget) · `Fast` (upload tool, ~800ms)
· `Frame` (camera frames, ~80ms, no scoring). Camera frames skip the PNG
roundtrip entirely: `ImageInput::rgba8(&frame, w, h)`.

### CLI

```bash
cargo install qrcode-ai-scanner-cli
qrscan image.png              # full ScanReport JSON
qrscan image.png --pretty     # human summary
qrscan image.png -s           # score only
qrscan image.png -s --budget-ms 0   # unbounded: reproducible, never cut
# exit codes: 0 found · 1 none · 2 invalid input/usage · 3 (-s) not judged within the budget
```

## Corpus results

Regenerated by `cargo run -p xtask -- corpus-report --write` · never
hand-typed. Exact results use only entries with known truth; negative samples
and frontier observations have separate counts. A held frontier is an
unlabelled blind spot, not a correct decode.

Measured on 13 October 2026 with the verified native diagnostic from
`2da72aee98c4cdbee93d789d96196262c37b99c3` (release, no LTO, 8 codegen units,
1 build job; release package overflow checks retained). These corpus observations do
not qualify shipped-package performance; trace timings are exploratory.

<!-- corpus-report:begin -->
| category | images | exact / truth | negative held / negatives | unlabelled decoded / blind / total | frontier held / gained | scan errors | avg ms (ladder-only trace.total_ms, exploratory, not a gate) |
|---|---|---|---|---|---|---|---|
| artistic | 2 | 2 / 2 | 0 / 0 | 0 / 0 / 0 | 0 / 0 | 0 | 637 |
| clean | 13 | 13 / 13 | 0 / 0 | 0 / 0 / 0 | 0 / 0 | 0 | 4 |
| degraded | 7 | 7 / 7 | 0 / 0 | 0 / 0 / 0 | 0 / 0 | 0 | 24 |
| frontier | 7 | 0 / 0 | 0 / 0 | 0 / 7 / 7 | 7 / 0 | 0 | 921 |
| symbology | 12 | 12 / 12 | 0 / 0 | 0 / 0 / 0 | 0 / 0 | 0 | 1 |
<!-- corpus-report:end -->

### External corpus observations

[`corpus-external.tsv`](corpus-external.tsv) pins file hashes and measured
decode states. A state pin is not content truth for an unlabelled image. The
corpora themselves are not vendored: they contain third-party test images and
private production exports. The 13 October diagnostic used the same original
pixels and retained qualified truth amendments; strict complete-output results
and exploratory any-text matches are separate measures.

| corpus | observed result | scope |
|---|---|---|
| ZXing blackbox qrcode-1…6 | **169/179 strict exact**, 9 missed, 1 ambiguous | The nested qrcode-2/16 scene has incomplete truth; it is not an exact success. No blocking wrong/extra output or scan error under the qualified truth/dispositions. |
| ZXing exploratory tripwire | **170/179 any-text matches** | Per-suite 20 · 35 · 42 · 39 · 19 · 15. Historical ZXing 0° pass thresholds total 157; these are context, not a contemporary comparator ranking. |
| Production templates (15 single-symbol styles) | **15/15 decoded observations** | No qualified content truth; part of the unlabelled gallery set. |
| Full artistic gallery | **56/161 decoded**, 105 blind | Detection observations, not exact-content successes. Six gallery images are also retained as vendored frontier fixtures. |

The complete strict run judged 381 images: 179 ZXing, 161 unlabelled gallery
and 41 vendored. All 522 external file hashes and 41 vendored inputs verified.
Vendored truth comprises 34 labelled images, 0 verified negatives and 7
unlabelled frontiers. One caught rqrr geometry panic on qrcode-6/7 is retained
in the report trace; it is not a scan error or hidden as a zero-cost sample.
Separate earlier center-logo experiments demonstrated errors-and-erasures rescue
through 30% radius on v5-H; that synthetic observation is not this corpus result
or a new comparative correctness/performance qualification.

One historical ZXing wrong label was a **truth conflict** on `qrcode-2/13.png`:
the upstream `.txt` reads "photograph", while independently executed external
ZXing-C++ and C quirc readers confirmed "photography" on original-derived
pixels. The [pinned text amendment](spec/corpus-truth-amendments.md#zxing-qrcode-2-13)
preserves the original image and truth hashes, uses the confirmed text in all
corpus instruments and records the historical `known_wrong` disposition as
`superseded` only when those exact inputs validate. Wrong and extra outputs
still block. This is text confirmation, not proof of ECI/FNC1 declarations or
native QR byte length; a zero correction margin alone proves neither text.

For suites 1 through 6, the historical ZXing 0° pass
thresholds 17 · 32 · 38 · 36 · 19 · 15 (`QRCodeBlackBox1…6TestCase`, 157 in all)
provide context for the observations above. Strict complete-output verdicts and
exploratory any-text matches remain separate measures.

#### Reproducing the headline numbers

Place the corpora at the repo root (layout below), then run the gate:

```
corpus-external/
├── zxing-blackbox/qrcode-{1..6}/   # zxing core/src/test/resources/blackbox · images + .txt ground truth
└── qrcode-ai/                      # qrcode-ai.com production gallery exports (private)
```

```sh
cargo run --release -p xtask -- corpus-report --external
```

A downloaded `xtask` can use `QRSCAN_REPO_ROOT=/absolute/source/checkout`
to read that checkout's manifests and vendored fixtures. The directory must
exist and both `corpus.toml` and `corpus-external.tsv` must be readable,
nonempty and valid; an invalid override exits 2. Without the variable, the
root embedded at build time remains the default. The strict oracle records
the selected checkout's Git SHA and tree state; the runner must separately
verify that the downloaded artifact belongs to that source revision.
`QRSCAN_EXTERNAL_CORPUS` (or `oracle --corpus-root`) independently selects
the external images. For example, after verifying the artifact:

```sh
QRSCAN_REPO_ROOT=/absolute/source/checkout /absolute/artifacts/xtask corpus-report
QRSCAN_REPO_ROOT=/absolute/source/checkout /absolute/artifacts/xtask oracle \
  --corpus-root /absolute/corpus-external --json /absolute/receipts/oracle.json
```

Verifies presence + sha256 of every manifested file, rescans every image
(budget-free full ladder, so no machine's speed decides a result · the
determinism contract holds per platform; scoring off), prints the
per-suite table, and exits non-zero on any divergence: a regression AND
a fresh decode of a pinned-blind image both fail, so pins are flipped
deliberately by regenerating —
`cargo run --release -p xtask -- gen-external-manifest` (~20 s) · and
committing the diff. When `corpus-external/` is absent (CI checkouts) the
gate skips gracefully but loudly, printing exactly how many pins went
unverified. `scripts/batch-scan.py` and `scripts/zxing-blackbox.py` stay
as exploratory reporters.

## Documentation & spec

> **Pourquoi ce scanner ?** Avant/après vs `qr-scanner-wechat` + vs les
> APIs cloud (local · instantané · catégorisé) :
> [docs/comparison.mdx](docs/comparison.mdx) · agents : [AGENTS.md](AGENTS.md).

- **[`spec/`](spec/)** · the NORMATIVE contract: wire format, errors,
  score, payloads, hints, pipeline + a JSON Schema and golden examples
  **validated in CI** against the real types (they cannot rot).
- **[`docs/`](docs/)** · the Mintlify documentation site (quickstart,
  concepts, per-surface API reference, integration guides · for humans
  AND agents). Renders on Mintlify or reads fine raw.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · [docs/SCORING.md](docs/SCORING.md)
  · engineering deep dives.

## Workspace

Every surface shares the workspace version (`Cargo.toml`).

| Crate / package | Published to |
|---|---|
| [`qrcode-ai-scanner`](crates/qrcode-ai-scanner) (core lib) | crates.io |
| [`qrcode-ai-scanner-cli`](crates/qrcode-ai-scanner-cli) (`qrscan`) | crates.io |
| [`qrcode-ai-scanner-node`](crates/qrcode-ai-scanner-node) (`@supernovae-st/qrcode-ai-scanner`, napi async) | npm |
| [`qrcode-ai-scanner-wasm`](crates/qrcode-ai-scanner-wasm) (`@supernovae-st/qrcode-ai-scanner-wasm`, SIMD128) | npm |
| [`qrcode-ai-scanner-py`](crates/qrcode-ai-scanner-py) (`qrcode-ai-scanner` on PyPI, PyO3 abi3) | PyPI |

## Contributing & security

Contributions welcome · see [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md). Found a vulnerability? Please report it
privately per the [Security Policy](SECURITY.md).

## License

**Dual-licensed** · full details in [`LICENSING.md`](LICENSING.md):
- **AGPL-3.0-or-later** (free) for open-source, research, and personal use.
- **Commercial license** for closed-source / proprietary products (mobile apps, SaaS) —
  contact `studio.supernovae@gmail.com`.

© SuperNovae Studio. The [qrcode-ai.com](https://qrcode-ai.com) product consumes this library.
