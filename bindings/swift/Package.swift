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
        // DEV (default below): a local-path xcframework built by CI / the build
        // commands into `build/` (git-ignored). Uncomment for local iteration.
        .binaryTarget(
            name: "QrcodeAiScannerFFI",
            path: "build/QrcodeAiScannerFFI.xcframework"
        ),
        // RELEASE: switch to a url + checksum pointing at the GitHub release
        // asset. CI zips the xcframework, attaches it to the `vX.Y.Z` release,
        // and computes the checksum (`swift package compute-checksum`). On
        // release, replace the `.binaryTarget` above with this form (CI fills
        // the <VERSION> and <CHECKSUM> placeholders):
        //
        // .binaryTarget(
        //     name: "QrcodeAiScannerFFI",
        //     url: "https://github.com/supernovae-st/qrcode-ai-scanner/releases/download/v<VERSION>/QrcodeAiScannerFFI.xcframework.zip",
        //     checksum: "<CHECKSUM>"
        // ),
    ]
)
