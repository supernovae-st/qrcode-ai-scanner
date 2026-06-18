# qrcode-ai-scanner — Kotlin / Android binding

The Android library module (AAR) for `qrcode-ai-scanner`, generated from the
shared UniFFI crate `crates/qrcode-ai-scanner-uniffi` (the engine; this is the
Gradle shell). One crate produces both this Kotlin binding and the Swift one in
`../swift/`.

> **Generated, not committed.** `src/main/kotlin/` (UniFFI `.kt`) and
> `src/main/jniLibs/<abi>/*.so` (cargo-ndk) are produced in CI / JitPack and are
> git-ignored — git is not the artifact store (architecture plan §1.3).

## License

Dual-licensed **AGPL-3.0-or-later OR commercial** — see the repo-root
`LICENSING.md`. A native mobile app shipped on the App Store / Play Store is
almost always closed-source and therefore needs the **commercial** license.
Contact `studio.supernovae@gmail.com`.

## Install (JitPack)

```kotlin
// settings.gradle.kts (or root build.gradle.kts repositories)
repositories {
    maven { url = uri("https://jitpack.io") }
}

// app/build.gradle.kts
dependencies {
    implementation("com.github.supernovae-st:qrcode-ai-scanner:vX.Y.Z")
}
```

## Usage

`scan(...)` returns the `ScanReport` as a **JSON String** (the cross-surface
wire SSOT — same bytes as the Python / Node / WASM bindings). Decode it with
`kotlinx.serialization` against `spec/scan-report.schema.json`. Run it off the
main thread — the scan is synchronous.

UniFFI 0.31 emits the API into the package `uniffi.qrcode_ai_scanner_uniffi`
(derived from the crate's `[lib] name`), while the `.so`/AAR ships under the
module `namespace` `studio.supernovae.qrcodeaiscanner`:

```kotlin
import uniffi.qrcode_ai_scanner_uniffi.scan

val json: String = scan(imageBytes, "full")   // "No QR found" = empty detections, not an error
```

## Build locally

```bash
# from repo root — the uniffi crate is EXCLUDED from the workspace, so always
# address it with --manifest-path and read its crate-local target/ dir (NOT -p,
# NOT the root target/ — that's the gotcha that fails for excluded crates).
cargo ndk -t arm64-v8a -t x86_64 \
  -o bindings/kotlin/qrcodeaiscanner/src/main/jniLibs \
  build --release --manifest-path crates/qrcode-ai-scanner-uniffi/Cargo.toml
cargo build --release --manifest-path crates/qrcode-ai-scanner-uniffi/Cargo.toml
cargo run --manifest-path crates/qrcode-ai-scanner-uniffi/Cargo.toml --bin uniffi-bindgen -- \
  generate --library crates/qrcode-ai-scanner-uniffi/target/release/libqrcode_ai_scanner_uniffi.so \
  --language kotlin --out-dir bindings/kotlin/qrcodeaiscanner/src/main/kotlin

cd bindings/kotlin && gradle :qrcodeaiscanner:assembleRelease   # or ./gradlew if you ran `gradle wrapper`
# → qrcodeaiscanner/build/outputs/aar/qrcodeaiscanner-release.aar
```

## The Gradle wrapper JAR (optional · local-dev convenience only)

Neither CI nor JitPack needs a committed Gradle wrapper jar — both provision
Gradle 8.9 themselves:

- **CI** (`.github/workflows/mobile.yml`) uses `gradle/actions/setup-gradle` and
  invokes `gradle` directly.
- **JitPack** (`jitpack.yml`) downloads `gradle-8.9-bin.zip` in `before_install`
  and runs `/opt/gradle-8.9/bin/gradle` — so a tagged release builds with **no
  committed wrapper jar**.

The committed `gradle-wrapper.properties` pins Gradle 8.9. If you want a
self-bootstrapping `./gradlew` for local dev, generate it once:

```bash
cd bindings/kotlin
gradle wrapper --gradle-version 8.9   # creates gradlew, gradlew.bat, gradle-wrapper.jar
git add gradlew gradlew.bat gradle/wrapper/gradle-wrapper.jar
```
