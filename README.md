<div align="center">

# QR Code AI Scanner

**Decode the undecodable — and know your margin.**

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

## Install — pick your surface

One Rust core, **five published surfaces** — all return the same versioned
[`ScanReport`](spec/) contract (mobile bindings ship next, below).

| Surface | Package | Install | One-liner |
|---|---|---|---|
| 🦀 **Rust lib** | [`qrcode-ai-scanner`](https://crates.io/crates/qrcode-ai-scanner) · [docs.rs](https://docs.rs/qrcode-ai-scanner) | `cargo add qrcode-ai-scanner` | `Scanner::builder().build().scan(input)?` |
| 💻 **CLI** | [`qrcode-ai-scanner-cli`](https://crates.io/crates/qrcode-ai-scanner-cli) | `cargo install qrcode-ai-scanner-cli` | `qrscan image.png --pretty` |
| 📦 **Node** (server) | [`@supernovae-st/qrcode-ai-scanner`](https://www.npmjs.com/package/@supernovae-st/qrcode-ai-scanner) | `npm i @supernovae-st/qrcode-ai-scanner` | `await scan(buffer)` |
| 🌐 **Browser** (WASM, SIMD) | [`@supernovae-st/qrcode-ai-scanner-wasm`](https://www.npmjs.com/package/@supernovae-st/qrcode-ai-scanner-wasm) | `npm i @supernovae-st/qrcode-ai-scanner-wasm` | `await init(); scan_image(bytes, "fast")` |
| 🐍 **Python** | [`qrcode-ai-scanner`](https://pypi.org/project/qrcode-ai-scanner/) | `pip install qrcode-ai-scanner` | `import qrcode_ai_scanner as q; q.scan(png_bytes)` |

Per-surface guides in [`docs/`](docs/) · code examples in the Quick start below.

**📱 Mobile + cross-platform:** Kotlin/Android is **live on JitPack**
(`com.github.supernovae-st:qrcode-ai-scanner:v0.6.0` — first tag build
verified green). Swift/iOS (UniFFI → SwiftPM) and Flutter/Dart
(flutter_rust_bridge → pub.dev) are built and CI-green, returning the same
`ScanReport`; their registries land across v0.6.x — the one-time
first-publish steps (pub.dev account claim, SwiftPM xcframework pin) are in
motion. Consult each binding's README for the current install path:
[`bindings/kotlin/`](bindings/kotlin/), [`bindings/swift/`](bindings/swift/), and
[`bindings/flutter/`](bindings/flutter/).

## Why

AI-generated and artistic QR codes break standard scanners: damaged finder
patterns, low module contrast, art texture over data modules. This library
is built specifically for them — and it doesn't just say *"decoded"*, it
tells you **how much margin** the code has before it stops scanning in the
real world.

Five things no other pure-Rust library ships together:

1. **A deterministic multi-engine decode ladder** tuned on artistic
   corpora (rxing + rqrr, curated preprocessing rungs — no RNG anywhere:
   same input, same result, always).
2. **Score contract v3** — survival ramps across six stress axes
   (resolution · blur · contrast · **perspective** · **rotation** ·
   **lighting** — the documented blind spots of naive scorers), with
   structural caps on finder damage and quiet-zone violations.
3. **Synthetic UEC** — the ISO 15415 *Unused Error Correction* margin,
   computed from the engine's own sampled bitstream via RS syndromes +
   Berlekamp-Massey. The real "how close to failure is this code" number.
4. **An ISO 15415-informed grade card** (`score.iso15415`) — Symbol
   Contrast, Modulation, Axial Nonuniformity, Fixed Pattern Damage and UEC,
   each `{value, grade}` in the ISO bands, with `overall` = lowest
   parameter (the ISO rule). Honest by construction: parameters that NEED
   verifier hardware (Grid Nonuniformity, Reflectance Margin) are reported
   absent, never faked.

5. **An erasure-rescue decode stage** — when both engines give up, the
   scanner re-decodes the sampled bitstream itself with errors-and-erasures
   Reed-Solomon (Forney 1965): low-confidence modules (logo zones, art
   texture) become half-price erasures (`e + 2t ≤ d − p`). Measured: center
   occlusion tolerance grows from 20% to **30% radius** (2.2× the area) on
   v5-H — exactly the artistic logo-over-center class.

Plus machine-actionable **hints** (`raise_error_correction`,
`fix_finder_pattern`, `low_correction_margin`, …) — the feedback loop for
generators and AI agents: *generate → scan → act on hints → regenerate*.

And **GS1 awareness** (Sunrise 2027-ready): FNC1-in-first-position symbols
(`]Q3`/`]Q4`) come back as a parsed `gs1` payload (AI element strings, GTIN,
check-digit + format validation), and GS1 **Digital Link** URIs are
recognized and validated (path order, GTIN-14, qualifier formats) as
`gs1_digital_link` — with a `conformant` verdict and per-criterion `issues`.
Scoring is **ISO-15415-informed** (the UEC margin uses the ISO bands and
exact RS error counts) — see [docs/SCORING.md](docs/SCORING.md) for the
parameter mapping and the honest line between software diagnostics and
certified hardware verification.

## Quick start

```rust
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};

let scanner = Scanner::builder().profile(ScanProfile::Full).build();
let report = scanner.scan(ImageInput::encoded(&image_bytes))?;

match report.detections.first() {
    Some(d) => println!("{} (score {:?})", d.content.text, report.score),
    None => println!("no QR found"), // a valid outcome — not an error
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
# exit codes: 0 found · 1 none · 2 invalid input
```

## Corpus results

Regenerated by `cargo run -p xtask -- corpus-report --write` — never
hand-typed.

<!-- corpus-report:begin -->
| category | pass | total | rate | avg ms |
|---|---|---|---|---|
| artistic | 2 | 2 | 100% | 1103 |
| clean | 13 | 13 | 100% | 7 |
| degraded | 6 | 6 | 100% | 209 |
| frontier | 6 | 6 | held | 1296 |
| symbology | 12 | 12 | 100% | 3 |
<!-- corpus-report:end -->

### Measured accuracy — external corpora (2026-06-11, v0.3.0)

Measured against public ground truth and production data. The corpora are
not vendored (30 MB of third-party/production images) — the measured state
IS: [`corpus-external.tsv`](corpus-external.tsv) pins sha256 + per-image
decode status for every file, generated from a real run, never hand-typed
(see “Reproducing the headline numbers” below).

| corpus | result | note |
|---|---|---|
| zxing blackbox qrcode-1…6 (179 images, ground truth) | **170/179 exact-text match @ 0°** | beats the zxing reference pass thresholds (153) on **all six suites** |
| qrcode-ai.com production templates (15 single-symbol styles) | **15/15 decoded** | includes the blob-pixel style no contrast/threshold transform recovers |
| qrcode-ai.com artistic gallery (full pinned set) | **56/161 decoded** | the blind family is multi-QR collages, extreme 3D perspective and busy marketing scenes — six are vendored as `expect = "fail"` frontier fixtures; `corpus-report --external` re-measures this exact number |
| center-logo occlusion (v5-H, gray disk) | engines die at >20% radius · **rescue decodes through 30%** | errors-and-erasures RS — `engines: ["rescue"]` in the report |

One of the 9 zxing misses is rqrr returning a Reed-Solomon
**miscorrection** ("photography" vs ground-truth "photograph") — which the
synthetic UEC flags at margin 0 and surfaces as the
`low_correction_margin` hint. The decode-rate number above counts it as a
miss; the hint is the mechanism that keeps it from being a *silent* one.
(In the manifest it is the one `wrong` pin — its own status, distinct from
`blind`, so a silent flip in either direction fails the gate.)

#### Reproducing the headline numbers

Place the corpora at the repo root (layout below), then run the gate:

```
corpus-external/
├── zxing-blackbox/qrcode-{1..6}/   # zxing core/src/test/resources/blackbox — images + .txt ground truth
└── qrcode-ai/                      # qrcode-ai.com production gallery exports (private)
```

```sh
cargo run --release -p xtask -- corpus-report --external
```

Verifies presence + sha256 of every manifested file, rescans every image
(budget-free full ladder — machine-independent per the determinism
contract; scoring off), prints the per-suite table, and exits non-zero on
any divergence: a regression AND a fresh decode of a pinned-blind image
both fail, so pins are flipped deliberately by regenerating —
`cargo run --release -p xtask -- gen-external-manifest` (~20 s) — and
committing the diff. When `corpus-external/` is absent (CI checkouts) the
gate skips gracefully but loudly, printing exactly how many pins went
unverified. `scripts/batch-scan.py` and `scripts/zxing-blackbox.py` stay
as exploratory reporters.

## Documentation & spec

> **Pourquoi ce scanner ?** Avant/après vs `qr-scanner-wechat` + vs les
> APIs cloud (local · instantané · catégorisé) :
> [docs/comparison.mdx](docs/comparison.mdx) · agents : [AGENTS.md](AGENTS.md).

- **[`spec/`](spec/)** — the NORMATIVE contract: wire format, errors,
  score, payloads, hints, pipeline + a JSON Schema and golden examples
  **validated in CI** against the real types (they cannot rot).
- **[`docs/`](docs/)** — the Mintlify documentation site (quickstart,
  concepts, per-surface API reference, integration guides — for humans
  AND agents). Renders on Mintlify or reads fine raw.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) · [docs/SCORING.md](docs/SCORING.md)
  — engineering deep dives.

## Workspace

| Crate / package | Status |
|---|---|
| [`qrcode-ai-scanner`](crates/qrcode-ai-scanner) (core lib) | v0.3 — release-ready |
| [`qrcode-ai-scanner-cli`](crates/qrcode-ai-scanner-cli) (`qrscan`) | v0.3 — release-ready |
| [`qrcode-ai-scanner-node`](crates/qrcode-ai-scanner-node) (`@supernovae-st/qrcode-ai-scanner`, napi async) | v0.3 — release-ready |
| [`qrcode-ai-scanner-wasm`](crates/qrcode-ai-scanner-wasm) (`@supernovae-st/qrcode-ai-scanner-wasm`, SIMD128) | v0.3 — release-ready |
| [`qrcode-ai-scanner-py`](crates/qrcode-ai-scanner-py) (`qrcode-ai-scanner` on PyPI, PyO3 abi3) | v0.3 — release-ready |

## Contributing & security

Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md). Found a vulnerability? Please report it
privately per the [Security Policy](SECURITY.md).

## License

**Dual-licensed** — full details in [`LICENSING.md`](LICENSING.md):
- **AGPL-3.0-or-later** (free) for open-source, research, and personal use.
- **Commercial license** for closed-source / proprietary products (mobile apps, SaaS) —
  contact `studio.supernovae@gmail.com`.

© SuperNovae Studio. The [qrcode-ai.com](https://qrcode-ai.com) product consumes this library.
