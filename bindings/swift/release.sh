#!/usr/bin/env bash
# Authoritative iOS / SwiftPM release helper. Run on a mac (needs Xcode + Swift).
#
# SwiftPM binary targets resolve a remote consumer via .binaryTarget(url:checksum:)
# — the committed dev `path:` form is git-ignored and does NOT resolve in a fresh
# clone (see mobile.yml's Guard A). The checksum must match the EXACT asset that
# the release serves. This script is the single place that produces a matching
# pair: it builds the xcframework, computes its checksum, and rewrites
# Package.swift to url+checksum for the given version — from ONE local build.
#
# It mutates only the working tree (Package.swift + build/). It does NOT push,
# tag, or publish — it prints the exact commands for you to run, so the network
# steps stay in your hands.
#
# Usage:  bindings/swift/release.sh v0.4.0      (from anywhere in the repo)
set -euo pipefail

REPO_SLUG="supernovae-st/qrcode-ai-scanner"
LIB="libqrcode_ai_scanner_uniffi"
MP="crates/qrcode-ai-scanner-uniffi/Cargo.toml"
T="crates/qrcode-ai-scanner-uniffi/target"

die() { echo "error: $*" >&2; exit 1; }

[ $# -eq 1 ] || die "usage: $0 vX.Y.Z"
# Normalize to a leading-v tag (mobile.yml triggers on tags: ['v*']).
VER="${1#v}"
TAG="v${VER}"
case "$VER" in
  [0-9]*.[0-9]*.[0-9]*) : ;;
  *) die "version must be X.Y.Z (got '$1')" ;;
esac

command -v xcodebuild >/dev/null || die "xcodebuild not found — run this on a mac with Xcode"
command -v swift      >/dev/null || die "swift not found"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

# Keep the tag honest: it should match the workspace version SSOT.
WS_VER="$(grep -m1 -E '^version' Cargo.toml | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
[ "$VER" = "$WS_VER" ] || echo "warning: tag $TAG != workspace version $WS_VER (bump Cargo.toml + the bindings first?)" >&2

echo "==> building the iOS xcframework for $TAG (device + simulator)"
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios >/dev/null
cargo build --release --manifest-path "$MP" --target aarch64-apple-ios
cargo build --release --manifest-path "$MP" --target aarch64-apple-ios-sim
cargo build --release --manifest-path "$MP" --target x86_64-apple-ios

rm -rf bindings/swift/Sources/QrcodeAiScanner build/swift build/sim build/QrcodeAiScannerFFI.xcframework
mkdir -p bindings/swift/Sources/QrcodeAiScanner build/swift/Modules build/sim

echo "==> generating Swift sources + headers + modulemap"
cargo run --manifest-path "$MP" --bin uniffi-bindgen-swift -- \
  "$T/aarch64-apple-ios/release/$LIB.a" \
  bindings/swift/Sources/QrcodeAiScanner --swift-sources
cargo run --manifest-path "$MP" --bin uniffi-bindgen-swift -- \
  "$T/aarch64-apple-ios/release/$LIB.a" \
  build/swift/Modules --headers --xcframework --modulemap --modulemap-filename module.modulemap

echo "==> lipo simulator slice + assembling the xcframework"
lipo -create \
  "$T/aarch64-apple-ios-sim/release/$LIB.a" \
  "$T/x86_64-apple-ios/release/$LIB.a" \
  -output "build/sim/$LIB.a"
xcodebuild -create-xcframework \
  -library "$T/aarch64-apple-ios/release/$LIB.a" -headers build/swift/Modules \
  -library "build/sim/$LIB.a" -headers build/swift/Modules \
  -output build/QrcodeAiScannerFFI.xcframework

echo "==> zipping + computing the SwiftPM checksum"
( cd build && rm -f QrcodeAiScannerFFI.xcframework.zip \
  && zip -qry QrcodeAiScannerFFI.xcframework.zip QrcodeAiScannerFFI.xcframework )
CHECKSUM="$(swift package compute-checksum build/QrcodeAiScannerFFI.xcframework.zip)"
echo "$CHECKSUM" > build/checksum.txt
URL="https://github.com/${REPO_SLUG}/releases/download/${TAG}/QrcodeAiScannerFFI.xcframework.zip"

echo "==> pinning Package.swift to url+checksum"
PKG="bindings/swift/Package.swift"
if grep -q 'path: "build/QrcodeAiScannerFFI.xcframework"' "$PKG"; then
  # First release (or restored dev mode): convert the path line → url + checksum.
  perl -0pi -e "s{ {12}path: \"build/QrcodeAiScannerFFI\\.xcframework\"\n}{            url: \"$URL\",\n            checksum: \"$CHECKSUM\"\n}" "$PKG"
else
  # Already url-mode (subsequent release): refresh the active url + checksum.
  perl -0pi -e "s{(\n {12})url: \"https://github\\.com/[^\"]*\",}{\${1}url: \"$URL\",}" "$PKG"
  perl -0pi -e "s{(\n {12})checksum: \"[a-f0-9]{64}\"}{\${1}checksum: \"$CHECKSUM\"}" "$PKG"
fi
grep -q "$CHECKSUM" "$PKG" || die "failed to write the checksum into $PKG — inspect it by hand"
grep -q 'path: "build/' "$PKG" && die "$PKG still has a path: target — inspect it by hand"

cat <<EOF

✅ built + pinned. xcframework checksum: $CHECKSUM

Review the diff, then publish (these are the only network steps — run them yourself):

  git add $PKG
  git commit -m "release(swift): $TAG"
  git push origin HEAD
  gh release create $TAG \\
    build/QrcodeAiScannerFFI.xcframework.zip build/checksum.txt \\
    --title $TAG --generate-notes --target "\$(git rev-parse HEAD)"

The tag fires mobile.yml: Guard A sees url-mode ✓, the asset is already attached
(skip), Guard B re-checks the published checksum == the pin ✓. Consumers then use:

  .package(url: "https://github.com/${REPO_SLUG}.git", from: "${VER}")
EOF
