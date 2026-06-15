# Explore the scanner — a guided tour of every surface

Three surfaces share **one engine**: the **CLI** (`qrscan`), the **docs site**
(Mintlify), and the **wasm playground** (Vite — the same bundler the qrcode-ai
landing uses). This is a ~20-minute hands-on tour that shows every feature, with
the **real output you should expect** at each step.

```text
qrcode-ai-scanner
├── crates/        core lib · CLI (qrscan) · node (napi) · wasm (SIMD128)
├── docs/          Mintlify site   → cd docs && mint dev        → http://localhost:3000
├── playground/    Vite wasm app   → cd playground && npm run dev → http://localhost:5173
├── fixtures/      ground-truth images (clean · degraded · symbology · artistic)
└── spec/          the normative ScanReport contract + 12 CI-validated goldens
```

| Surface | Start it | Best for |
|---|---|---|
| **CLI** | `cargo build --release` | the engine, scriptable, the raw report |
| **Playground** ⭐ | `cd playground && npm install && npm run dev` → :5173 | seeing everything visually, fast |
| **Docs** | `cd docs && mint dev` → :3000 | concepts, the API contract, the gallery |

---

## Setup (once)

```bash
cargo build --release                     # builds the workspace incl. qrscan
alias qrscan='./target/release/qrscan'     # convenience for this shell
```

---

## The report in one picture

Everything below is one `ScanReport`. `--pretty` renders it like an instrument:

```text
content   https://qrc-ai.com/76xMa          ← decoded text
symbology QrCode                             ← WHAT KIND of code (19 possible)
payload   Url { url: "…" }                   ← WHAT IT MEANS, classified (13 kinds)
symbol    v2 · 25x25 modules                 ← QR geometry
engines   rxing+rqrr                          ← who decoded (or "rescue")
score     89/100 (Excellent)                 ← the MARGIN, not "did it decode"
  resolution 5/5 · blur 5/5 · contrast 5/5   ← 6 survival axes (the knee per axis)
  perspective 5/5 · rotation 1/5 · lighting 4/5
  uec margin 1.00 (grade A · 0/16 ec)        ← unused Reed-Solomon budget (0 = distrust)
iso15415  overall A                          ← ISO-informed grade card (software, not certified)
hint      …                                  ← machine-actionable fixes (generate→scan→act loop)
```

---

## Stage 1 — your first scan: *decode ≠ scannable*

```bash
qrscan fixtures/clean/OK_68ms_100_4e875a2c.png --pretty
```

👀 **Expect:** `QrCode`, payload `Url`, **score `89/100 (Excellent)`**, every axis
maxed **except `rotation 1/5`**, `uec margin 1.00`, `iso15415 overall A`.

💡 **Why it matters:** it decodes perfectly, yet the score is 89 — because the
rotation axis is weak. The engine tells you *how much margin* the code has before
it fails in the wild. A plain scanner just says "decoded".

🧪 **Try:** `qrscan … -s` (bare score) · `qrscan …` (full JSON) · `--profile frame`
(decode only, no score).

---

## Stage 2 — the two layers: `symbology` ≠ `payload`

**(a) Symbology** — the *physical* code type:
```bash
for f in fixtures/symbology/*; do echo "── $(basename "$f")"; qrscan "$f" --pretty | head -3; done
```
👀 12 codes decode: `Ean13`, `DataMatrix`, `Aztec`, `Pdf417`, `Code128`, `MicroQr`… (of 19 supported).

**(b) Payload** — what the content *means*. Mint one of each with `qrencode`:
```bash
qrencode -o /tmp/wifi.png 'WIFI:T:WPA;S:MyNetwork;P:secret123;;'
qrencode -o /tmp/geo.png  'geo:48.8584,2.2945'
qrencode -o /tmp/btc.png  'bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa?amount=0.5'
qrencode -o /tmp/dl.png   'https://id.gs1.org/01/09506000134352/10/LOT1'
for f in wifi geo btc dl; do echo "── $f"; qrscan "/tmp/$f.png" --pretty | grep '^payload'; done
```
👀 **Expect (verified):**
```
wifi → Wifi { ssid: "MyNetwork", security: "WPA", password: "secret123", hidden: false }
geo  → Geo { lat: 48.8584, lon: 2.2945 }
btc  → Crypto { scheme: "bitcoin", address: "1A1zP1…", amount: "0.5" }
dl   → Gs1DigitalLink { gtin: "09506000134352", conformant: true, … }   ← Sunrise-2027 retail form
```
💡 An `ean13` symbology carries a `gs1` payload; a `qr_code` might carry `url`,
`wifi`, or `crypto`. Two questions, two fields.

---

## Stage 3 — the hard cases: the scannability *spectrum*

```bash
qrscan fixtures/degraded/logo-occluded-rescue.png --pretty      # the RESCUE
qrscan fixtures/artistic/OK_1069ms_85_8b6a54b3.png --pretty      # artistic, decodes but fragile
qrscan fixtures/degraded/FAIL_1491ms_0_584c998c.png --pretty; echo "exit=$?"   # negative → exit 1
```
👀 **The rescue:** both standard engines fail; the S5 errors-and-erasures stage
recovers the exact URL → `engines rescue`, **score `0` (Poor)**, `uec margin 0.00
grade F`, and 4 hints incl. **`LowCorrectionMargin { errors: 11, capacity: 22 }`**.

💡 **Honest by construction:** it decodes the undecodable *and* refuses to pretend
it's healthy — "decodes, but unreliable, regenerate". Decoding and reliability are
two facts, both reported.

🧪 **Try:** shrink a QR and watch the score collapse —
`qrencode -s 1 -o /tmp/tiny.png 'https://x.co' && qrscan /tmp/tiny.png --pretty`

---

## Stage 4 — the playground ⭐ — `http://localhost:5173`

```bash
cd playground && npm install && npm run dev
```
(The wasm comes from the committed `crates/qrcode-ai-scanner-wasm/pkg/` via a
`file:` dep — no npm publish needed to test.)

1. **Click the 5 samples** (clean 89 · artistic 70 · logo-rescue 0 · barcode · gs1)
   → read the instrument: big score, the **6 survival-axis bars**, the **UEC gauge**
   (red at margin 0), the **ISO card**, the **hints**, the typed **payload**.
2. **Drag-drop / browse / paste (⌘V)** your own QR — open `/tmp` in Finder and drop
   `wifi.png`, `btc.png`, `dl.png` to see the wired payload (GS1 `✓/✕ conformant`, issues).
3. **Switch the profile** (full/fast/frame → auto re-scan) · tune the **budget (ms)**.
4. **Live camera** → show a QR on your phone and scan it live (this runs `scan_frame`).
5. Expand **raw ScanReport (JSON)** at the bottom of any report.

💡 The playground runs on **Vite, the same bundler as Nuxt** — what works here proves
the qrcode-ai landing integration works.

🧪 **Regression check** (drives the page in headless Chrome, asserts every feature):
```bash
cd playground && node visual-check.mjs     # screenshots → /tmp/pg-*.png
```

---

## Stage 5 — the docs site — `http://localhost:3000`

```bash
cd docs && mint dev          # needs the Mintlify CLI: npm i -g mint
```
Guided walk of the nav:
- **How it works** → the **Mermaid flow diagram** + 6 use-case recipes (accordion).
- **See it in action** → the 5 QR with their scored reports.
- **Scoring** → *the page that was 404* — now fixed (artistic image + plain-words summaries).
- **GS1** → `conformant: true` (EAN) vs `false` (DataMatrix without FNC1), in images.
- **Key terms** → the 15-term glossary.

🧪 **Try:** `cd docs && mint validate` → **"success build validation passed"** (the
404 bug is gone), and `mint broken-links` → clean.

---

## Stage 6 — under the hood

```bash
cargo test --workspace               # 181 tests: unit + integration + spec goldens
cargo run -p xtask -- corpus-report  # live per-category pass rates (matches the README)
ls spec/examples/                    # 12 golden ScanReport JSONs, CI-validated vs the types
```

---

## Reference

**Symbologies (19)** — `qr_code` · `micro_qr_code` · `rectangular_micro_qr_code` ·
`data_matrix` · `aztec` · `pdf417` · `maxi_code` · `ean13` · `ean8` · `upc_a` ·
`upc_e` · `code128` · `code39` · `code93` · `codabar` · `itf` · `data_bar` ·
`data_bar_expanded` · `telepen`. *(Only the QR family carries geometry, UEC, the ISO card, and rescue.)*

**Payload kinds (13)** — `url` · `gs1` · `gs1_digital_link` · `wifi` · `email` ·
`sms` · `tel` · `geo` · `me_card` · `crypto` · `v_card` · `v_event` · `text` (fallback).

**Score axes (6)** + weights — resolution 22 · perspective 20 · blur 18 · contrast 15
· lighting 15 · rotation 10. **Grade bands** — excellent ≥80 · good 70-79 ·
acceptable 60-69 · fair 40-59 · poor <40.

**Hints (7)** — `fix_finder_pattern` · `restore_quiet_zone` · `increase_contrast` ·
`enlarge_modules` · `reduce_art_texture` · `raise_error_correction` ·
`low_correction_margin` (the distrust signal).

**Exit codes** — `0` decoded · `1` nothing found (valid) · `2` invalid input/usage.

---

## Mint your own test QRs (`qrencode`)

> `brew install qrencode` if needed.

```bash
qrencode -o /tmp/url.png    'https://qrcode-ai.com'
qrencode -o /tmp/tel.png    'tel:+33123456789'
qrencode -o /tmp/sms.png    'SMSTO:+33123456789:hello'
qrencode -o /tmp/email.png  'mailto:hi@example.com?subject=Test'
qrencode -o /tmp/mecard.png 'MECARD:N:Dupont,Jean;TEL:0612345678;EMAIL:jean@ex.com;;'
printf 'BEGIN:VCARD\nVERSION:3.0\nFN:Jean Dupont\nTEL:0612345678\nEND:VCARD' | qrencode -o /tmp/vcard.png
qrencode -l H -o /tmp/ecH.png 'https://x.co'    # force EC level H → check meta.ec_level
```
Scan any with `qrscan /tmp/<name>.png --pretty`, or drop it into the playground.

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| `zsh: command not found: qrscan` | use `./target/release/qrscan` or the alias above |
| docs page 404 / `mint` missing | `npm i -g mint`; run `cd docs && mint dev` |
| playground can't find the wasm | the `file:` dep points at `crates/qrcode-ai-scanner-wasm/pkg/` — rebuild with `./scripts/build-wasm.sh` (needs wasm-pack + binaryen) if it's missing |
| port already in use | docs picks 3000, playground 5173; pass `--port` to either |

---

## What was built (this session)

| Surface | Change |
|---|---|
| **Docs** | fixed the scoring **404**; new gallery / tutorial / glossary; clarity pass; moved `plans/` + `research/` out of the Mintlify tree (validate + broken-links 100% clean) |
| **Playground** | built from scratch (Vite + wasm), instrument UI, `visual-check.mjs` headless check — all features verified |
| **Integration** | PR #11 code verified correct against the real wasm API (no change to Nicolas's repo) |
| **Engine** | re-verified — `cargo test --workspace` 181 pass / 0 fail |

**The release gate (next, when you're convinced):** tag `v0.3.0` → CI builds +
publishes the npm packages → the qrcode-ai landing PR #11 un-drafts. Public and
irreversible, so it waits for an explicit go.
