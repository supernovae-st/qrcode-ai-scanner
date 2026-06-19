// swift-tools-version:5.9
//
// Swift / iOS binding for qrcode-ai-scanner, generated from the shared UniFFI
// crate `crates/qrcode-ai-scanner-uniffi`. SwiftPM versions are the repo's git
// tags (vX.Y.Z) — there is no version field to sync (architecture plan §3.1).
//
// Dual-licensed AGPL-3.0-or-later OR commercial — see the repo-root
// LICENSING.md. A native iOS app on the App Store is almost always closed-
// source and needs the commercial license (contact studio.supernovae@gmail.com).
//
// Generated, not committed: Sources/QrcodeAiScanner/*.swift (uniffi-bindgen-swift)
// and the xcframework are produced in CI and git-ignored (architecture §1.3).

import PackageDescription

let package = Package(
    name: "QrcodeAiScanner",
    platforms: [.iOS(.v13)],
    products: [
        .library(name: "QrcodeAiScanner", targets: ["QrcodeAiScanner"]),
    ],
    targets: [
        // The generated Swift bindings (the friendly API surface). Depends on
        // the compiled-Rust binary target below.
        .target(
            name: "QrcodeAiScanner",
            dependencies: ["QrcodeAiScannerFFI"],
            // holds the generated qrcode_ai_scanner_uniffi.swift
            path: "Sources/QrcodeAiScanner"
        ),
        // The compiled Rust, as a binary xcframework.
        //
        // DEV (the path form below): a local-path xcframework built by the
        // release.sh build steps into `build/` (git-ignored). This is the
        // pre-first-release default; it does NOT resolve for a remote consumer.
        // `bindings/swift/release.sh vX.Y.Z` rewrites this to the url+checksum
        // RELEASE form (and mobile.yml Guard A blocks a tag that's still path
        // mode). After the first release the committed form is url+checksum.
        .binaryTarget(
            name: "QrcodeAiScannerFFI",
            path: "build/QrcodeAiScannerFFI.xcframework"
        ),
        // RELEASE form that release.sh writes (url + checksum of the exact asset
        // it publishes to the vX.Y.Z release; Guard B re-verifies it in CI):
        //
        // .binaryTarget(
        //     name: "QrcodeAiScannerFFI",
        //     url: "https://github.com/supernovae-st/qrcode-ai-scanner/releases/download/v<VERSION>/QrcodeAiScannerFFI.xcframework.zip",
        //     checksum: "<CHECKSUM>"
        // ),
    ]
)
