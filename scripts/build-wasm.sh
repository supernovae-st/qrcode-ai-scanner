#!/usr/bin/env bash
# Build the browser wasm package: wasm-pack (no bundled wasm-opt — its cached
# binaryen is too old for Rust ≥1.87 wasm features) + manual wasm-opt + size
# report. Run from the repo root. Requires: wasm-pack, binaryen (wasm-opt).
set -euo pipefail

if ! command -v wasm-opt >/dev/null; then
  echo "error: wasm-opt not on PATH — install binaryen (brew install binaryen)" >&2
  echo "hint: a local extract works too: PATH=/private/tmp/binaryen-version_130/bin:\$PATH" >&2
  exit 1
fi
# The version gates, not just the presence: a binaryen older than 130 cannot
# read the wasm features Rust >= 1.87 emits by default.
opt_version=$(wasm-opt --version | sed -nE 's/.*version ([0-9]+).*/\1/p')
if [ -z "$opt_version" ] || [ "$opt_version" -lt 130 ]; then
  echo "error: wasm-opt ${opt_version:-of unknown version} is too old — binaryen >= 130 required" >&2
  exit 1
fi

cd "$(dirname "$0")/../crates/qrcode-ai-scanner-wasm"
echo "toolchain: $(rustc --version) · $(wasm-pack --version) · wasm-opt version ${opt_version}"

# `-- --locked`: wasm-pack hands what follows to cargo build, so the build
# uses the committed Cargo.lock or fails, never a fresh resolution.
RUSTFLAGS="-C target-feature=+simd128" wasm-pack build \
  --target web --release --out-name qrcode-ai-scanner -- --locked

wasm-opt pkg/qrcode-ai-scanner_bg.wasm \
  -O3 --enable-simd --all-features \
  -o pkg/qrcode-ai-scanner_bg.wasm.opt
mv pkg/qrcode-ai-scanner_bg.wasm.opt pkg/qrcode-ai-scanner_bg.wasm

node ../../scripts/patch-wasm-pkg.mjs
(cd ../.. && node scripts/sync-report-types.mjs)

node test.mjs

raw=$(wc -c < pkg/qrcode-ai-scanner_bg.wasm)
gz=$(gzip -9 -c pkg/qrcode-ai-scanner_bg.wasm | wc -c)
echo "wasm size: raw ${raw} bytes · gzip ${gz} bytes"
