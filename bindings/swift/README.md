# qrcode-ai-scanner — Swift / iOS binding

SwiftPM package for `qrcode-ai-scanner`, generated from the shared UniFFI crate
`crates/qrcode-ai-scanner-uniffi`. SwiftPM versions are the repo's git tags
(`vX.Y.Z`) — no manifest version field to sync.

> **Generated, not committed.** `Sources/QrcodeAiScanner/*.swift`
> (uniffi-bindgen-swift) and `build/QrcodeAiScannerFFI.xcframework` (CI) are
> git-ignored — git is not the artifact store (architecture plan §1.3). A fresh
> checkout will NOT resolve in Xcode until CI has produced the xcframework (or
> you build it locally, below), or the package is switched to the release
> `url + checksum` binaryTarget form.

## License

Dual-licensed **AGPL-3.0-or-later OR commercial** — see the repo-root
`LICENSING.md`. A native iOS app shipped on the App Store is almost always
closed-source and therefore needs the **commercial** license. Contact
`studio.supernovae@gmail.com`.

## Install (SwiftPM, on a tagged release)

```swift
// Package.swift
dependencies: [
    .package(url: "https://github.com/supernovae-st/qrcode-ai-scanner", from: "0.3.0"),
],
// target deps:
.product(name: "QrcodeAiScanner", package: "qrcode-ai-scanner"),
```

On a release the `.binaryTarget` resolves to the GitHub-release xcframework zip
via `url` + `checksum` (CI fills them — see `Package.swift`), so consumers need
no build step.

## Usage

`scan(...)` throws `ScanBindingError` and returns the `ScanReport` as a **JSON
`String`** (the cross-surface wire SSOT). Decode with `JSONDecoder` against
`spec/scan-report.schema.json`. Run it off the main thread — the scan is sync.

```swift
import QrcodeAiScanner

let json = try scan(image: imageData, profile: "full")  // empty detections = no QR (not an error)
```

## Build the xcframework locally

```bash
# from repo root — the uniffi crate is EXCLUDED from the workspace, so always
# address it with --manifest-path and read its crate-local target/ dir (NOT -p,
# NOT the root target/). This mirrors the `ios` job in .github/workflows/mobile.yml.
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
MP=crates/qrcode-ai-scanner-uniffi/Cargo.toml
T=crates/qrcode-ai-scanner-uniffi/target
cargo build --release --manifest-path "$MP" --target aarch64-apple-ios
cargo build --release --manifest-path "$MP" --target aarch64-apple-ios-sim
cargo build --release --manifest-path "$MP" --target x86_64-apple-ios

mkdir -p bindings/swift/Sources/QrcodeAiScanner build/swift/Modules
cargo run --manifest-path "$MP" --bin uniffi-bindgen-swift -- \
  "$T/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a" \
  bindings/swift/Sources/QrcodeAiScanner --swift-sources
cargo run --manifest-path "$MP" --bin uniffi-bindgen-swift -- \
  "$T/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a" \
  build/swift/Modules --headers --xcframework --modulemap --modulemap-filename module.modulemap

mkdir -p build/sim
lipo -create \
  "$T/aarch64-apple-ios-sim/release/libqrcode_ai_scanner_uniffi.a" \
  "$T/x86_64-apple-ios/release/libqrcode_ai_scanner_uniffi.a" \
  -output build/sim/libqrcode_ai_scanner_uniffi.a
xcodebuild -create-xcframework \
  -library "$T/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a" -headers build/swift/Modules \
  -library build/sim/libqrcode_ai_scanner_uniffi.a -headers build/swift/Modules \
  -output bindings/swift/build/QrcodeAiScannerFFI.xcframework
```
