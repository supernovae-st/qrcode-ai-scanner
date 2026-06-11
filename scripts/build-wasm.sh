#!/usr/bin/env bash
# Build the browser wasm package: wasm-pack (no bundled wasm-opt — its cached
# binaryen is too old for Rust ≥1.87 wasm features) + manual wasm-opt + size
# report. Run from the repo root. Requires: wasm-pack, binaryen (wasm-opt).
set -euo pipefail

cd "$(dirname "$0")/../crates/qrcode-ai-scanner-wasm"

RUSTFLAGS="-C target-feature=+simd128" wasm-pack build \
  --target web --release --out-name qrcode-ai-scanner

wasm-opt pkg/qrcode-ai-scanner_bg.wasm \
  -O3 --enable-simd --all-features \
  -o pkg/qrcode-ai-scanner_bg.wasm.opt
mv pkg/qrcode-ai-scanner_bg.wasm.opt pkg/qrcode-ai-scanner_bg.wasm

node ../../scripts/patch-wasm-pkg.mjs
(cd ../.. && node scripts/sync-report-types.mjs)

raw=$(wc -c < pkg/qrcode-ai-scanner_bg.wasm)
gz=$(gzip -9 -c pkg/qrcode-ai-scanner_bg.wasm | wc -c)
echo "wasm size: raw ${raw} bytes · gzip ${gz} bytes"
