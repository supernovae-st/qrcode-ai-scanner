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

# The two variants use the same features, source and lock. Disable SIMD
# explicitly for the compatible build, including dependencies; do not let an
# ambient RUSTFLAGS or an optimizer's all-features setting reintroduce it.
# wasm-pack skips its old bundled optimizer; validation happens after Binaryen.
RUSTFLAGS="-C target-feature=+simd128,-relaxed-simd" wasm-pack build \
  --target web --release --out-name qrcode-ai-scanner -- --locked

wasm-opt pkg/qrcode-ai-scanner_bg.wasm \
  -O3 --mvp-features --enable-mutable-globals --enable-nontrapping-float-to-int \
  --enable-bulk-memory --enable-sign-ext --enable-reference-types --enable-multivalue --enable-simd \
  -o pkg/qrcode-ai-scanner_bg.wasm.opt
mv pkg/qrcode-ai-scanner_bg.wasm.opt pkg/qrcode-ai-scanner_bg.wasm

RUSTFLAGS="-C target-feature=-simd128,-relaxed-simd" wasm-pack build \
  --target web --release --out-dir pkg/compatible --out-name qrcode-ai-scanner -- --locked
wasm-opt pkg/compatible/qrcode-ai-scanner_bg.wasm \
  -O3 --mvp-features --enable-mutable-globals --enable-nontrapping-float-to-int \
  --enable-bulk-memory --enable-sign-ext --enable-reference-types --enable-multivalue --disable-simd \
  -o pkg/compatible/qrcode-ai-scanner_bg.wasm.opt
mv pkg/compatible/qrcode-ai-scanner_bg.wasm.opt pkg/compatible/qrcode-ai-scanner_bg.wasm
# This validation rejects SIMD instructions/types, even if a dependency or
# optimizer accidentally emitted them. The browser acceptance also disables
# SIMD in the actual runtime rather than mocking the detector.
wasm-opt pkg/compatible/qrcode-ai-scanner_bg.wasm \
  --mvp-features --enable-mutable-globals --enable-nontrapping-float-to-int \
  --enable-bulk-memory --enable-sign-ext --enable-reference-types --enable-multivalue --disable-simd \
  -o /dev/null

node ../../scripts/patch-wasm-pkg.mjs
(cd ../.. && node scripts/sync-report-types.mjs)

node test.mjs
WASM_VARIANT=compatible node test.mjs

raw=$(wc -c <pkg/qrcode-ai-scanner_bg.wasm)
gz=$(gzip -9 -c pkg/qrcode-ai-scanner_bg.wasm | wc -c)
echo "wasm size: raw ${raw} bytes · gzip ${gz} bytes"
