# Scanner playground

A local, browser playground that exercises **every feature** of
`@supernovae-st/qrcode-ai-scanner-wasm` in a real **Vite** bundler context —
the same bundler Nuxt uses, so it de-risks the landing integration without
touching the consumer repo.

What it tests: `scan_image` (encoded bytes), `scan_frame` (live camera RGBA),
all three profiles, the full `ScanReport` (score · 6 survival axes · UEC margin
· ISO 15415 card · hints · payload classification · GS1 conformance · rescue
engine · symbology · the alpha block with its placement envelope, host-palette
verdicts and carousel), and the Vite `.wasm` resolution gotcha
(`optimizeDeps.exclude`).

## Run

```bash
# 1. the wasm package must be built once (uses the committed pkg/ if present):
#    from the repo root:  ./scripts/build-wasm.sh   (needs wasm-pack + binaryen)
#    — or just rely on the existing crates/qrcode-ai-scanner-wasm/pkg/

# 2. then, here:
cd playground
npm install        # installs Vite + links the local wasm package (file: dep)
npm run dev        # → http://localhost:5173
```

Drop / browse / **paste** (⌘V) a QR or barcode image, pick a profile, read the
report. Sample images live in `public/samples/`. "Live camera" runs `scan_frame`.

## Verify (headless, optional)

With the dev server running, `node visual-check.mjs` drives the page in system
Chrome and asserts **every feature** — all 6 samples (symbology · payload · score
· axes · UEC · ISO card · hints · rescue · GS1 conformance · the transparent
sample's alpha block, placement envelope, carousel tiles and both alpha hints),
profile switching, the Alpha mode toggle, the upload path, the FNC1 control-byte
glyph, and live `scan_frame` on a fake device. Exits non-zero on any regression;
writes screenshots to `/tmp/pg-*.png`.

> The `file:` dependency on the local `pkg/` is DELIBERATE — the playground's
> job is to exercise the UNRELEASED build (rebuild with
> `./scripts/build-wasm.sh`). Consumers integrating the published npm package
> use the same API; only the dependency line differs.
