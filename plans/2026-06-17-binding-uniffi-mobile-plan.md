# Kotlin/Android + Swift/iOS bindings via one shared UniFFI crate

> **Status**: DEV-READY plan · authored 2026-06-17
> **Scope**: add `crates/qrcode-ai-scanner-uniffi` (one crate) → generate Kotlin
> *and* Swift bindings → package an Android AAR (JitPack first) + an iOS
> SwiftPM xcframework. Mirrors the existing PyO3 / napi / wasm thin-wrapper
> pattern (JSON-string output, canonical SSOT in `spec/`).
> **Verified against current docs (mid-2026)**: `uniffi 0.31.1`,
> `cargo-ndk 4.1.2` (both confirmed via crates.io on 2026-06-17).

An engineer with zero context can execute this top-to-bottom. Every command,
file path, and version is spelled out.

---

## 0. Design decision (the one that shapes everything)

### How does `ScanReport` cross the FFI? → **Option A: JSON String** ✅

UniFFI cannot serialize a serde struct automatically. Two options:

| | A — JSON String (CHOSEN) | B — UniFFI records |
|---|---|---|
| Wrapper returns | `String` (`serde_json::to_string(&report)`) | `#[derive(uniffi::Record)]` mirror of the whole `ScanReport` tree |
| Kotlin/Swift consumer | parses the string (kotlinx.serialization / `JSONDecoder`) against `spec/scan-report.schema.json` | gets a typed object for free |
| Boilerplate | ~zero | must hand-mirror **every** struct/enum in `report.rs` (`Detection`, `Score`, `AxisScore`, `PipelineTrace`, `StageTrace`, `Iso15415Report`, `UecReport`, `Payload`, `Gs1Element`, …) AND keep them in lockstep forever |
| Drift risk | the wire format is already the SSOT — same bytes as Python/Node/WASM | high — every core struct change must be re-mirrored in this crate |
| Schema validation | already exists (`spec/scan-report.schema.json`) | duplicated in the type system |

**Recommendation: ship Option A for v0.1.** It is identical to what the
PyO3 binding does conceptually (`serde_json::to_value` → `pythonize`) and what
the Node/WASM bindings emit (JSON). It keeps **one** SSOT (the serde contract +
`spec/`), zero per-struct mirroring, and unblocks both platforms immediately.

**Tradeoff (state it in the README):** the consumer gets a `String`, not a
typed object — they decode it themselves. That's acceptable: every other
binding already returns JSON, and `ScanReport` is a deep, frequently-evolving
tree (mirroring it as records would be a maintenance tax with no SSOT win).
**Migration path** noted in §8: a future v0.2 *can* add typed records
incrementally (e.g. just the top-level `detections`/`score` summary) without
breaking the JSON method.

---

## 1. The shared crate `crates/qrcode-ai-scanner-uniffi`

ONE crate produces BOTH Kotlin and Swift. It is a thin wrapper over
`scanner-core`, exactly like `qrcode-ai-scanner-py`.

### 1.1 Workspace membership — EXCLUDE it (like the py crate)

Root `Cargo.toml` currently has:

```toml
exclude = ["crates/qrcode-ai-scanner-py"]
```

Add the uniffi crate to `exclude` too. Reason: it is a `cdylib`/`staticlib`
built by `cargo-ndk` / `xcodebuild`-driven flows, not by
`cargo test/clippy --workspace` (CI core). Excluding keeps the core `ci`
workflow clean and lets the crate pin its own versions (excluded crates
**cannot** use `workspace = true` inheritance — versions are spelled out, same
note as the py `Cargo.toml`).

```toml
# root Cargo.toml
exclude = ["crates/qrcode-ai-scanner-py", "crates/qrcode-ai-scanner-uniffi"]
```

### 1.2 `crates/qrcode-ai-scanner-uniffi/Cargo.toml`

```toml
# Excluded from the workspace (see root Cargo.toml): built via cargo-ndk
# (Android .so) and cargo + xcodebuild (iOS xcframework), not by cargo
# workspace tooling. Versions are spelled out because excluded crates cannot
# inherit `workspace = true`.
[package]
name = "qrcode-ai-scanner-uniffi"
version = "0.3.0"
edition = "2024"
rust-version = "1.87"
license = "AGPL-3.0-or-later"
repository = "https://github.com/supernovae-st/qrcode-ai-scanner"
description = "Kotlin/Android + Swift/iOS bindings for qrcode-ai-scanner via UniFFI."
publish = false

[lib]
# cdylib  → Android .so (loaded by JNA at runtime)
# staticlib → iOS .a (linked into the xcframework)
crate-type = ["cdylib", "staticlib"]
name = "qrcode_ai_scanner_uniffi"

[dependencies]
uniffi = { version = "0.31", features = ["cli"] }
serde_json = "1"
# alias to dodge the name clash with this crate's lib name, same as the py crate.
scanner-core = { package = "qrcode-ai-scanner", path = "../qrcode-ai-scanner", features = ["serde"] }

[build-dependencies]
uniffi = { version = "0.31", features = ["build"] }

# The standalone uniffi-bindgen binary (library-mode generation) lives in this
# crate so CI can `cargo run --bin uniffi-bindgen` without a separate install.
[[bin]]
name = "uniffi-bindgen"
path = "uniffi-bindgen.rs"
```

> **Why `crate-type = ["cdylib", "staticlib"]`:** Android loads a `cdylib`
> (`.so`) via JNA at runtime; iOS links a `staticlib` (`.a`) into the
> xcframework. One crate, two artifact shapes from the same source.

### 1.3 `crates/qrcode-ai-scanner-uniffi/uniffi-bindgen.rs`

The current (0.31) idiom: ship the bindgen as a tiny binary in the crate so
CI/devs run `cargo run --bin uniffi-bindgen -- …` with no global install and
no version skew between the lib's `uniffi` dep and the generator.

```rust
fn main() {
    uniffi::uniffi_bindgen_main()
}
```

### 1.4 `crates/qrcode-ai-scanner-uniffi/src/lib.rs` (the wrapper — udl-less, proc-macro)

UDL-less / proc-macro-only API. `uniffi::setup_scaffolding!()` replaces the
old `build.rs` + `.udl` route. (Do **not** also call
`uniffi::include_scaffolding!` — pick one; we use scaffolding.)

```rust
//! Kotlin/Android + Swift/iOS bindings for `qrcode-ai-scanner` (UniFFI).
//!
//! Thin wrapper over the Rust core: bytes → scan → the same versioned
//! `ScanReport`, returned as a JSON **String** (serialized via the core's serde
//! contract, the cross-surface SSOT in `spec/`). Consumers parse the string
//! against `spec/scan-report.schema.json` — identical wire format to the
//! Python / Node / WASM bindings. The scan itself is synchronous (the core is
//! sync by design); callers move it off the main thread on their side.

use scanner_core::{ImageInput, ScanProfile, Scanner};

uniffi::setup_scaffolding!();

/// Errors crossing the FFI. UniFFI requires the error type be an enum that
/// implements `std::error::Error`; we collapse the core's faults into a single
/// message-bearing variant (the core's own error codes are inside the string).
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ScanBindingError {
    /// Unknown profile name (not "full" / "fast" / "frame").
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    /// A real scan fault (invalid/oversized buffer, cancellation) — carries the
    /// core's QRS-XXX message.
    #[error("scan failed: {0}")]
    ScanFailed(String),
}

fn parse_profile(profile: &str) -> Result<ScanProfile, ScanBindingError> {
    // Canonical parser, same path as every other binding.
    ScanProfile::from_name(profile)
        .ok_or_else(|| ScanBindingError::UnknownProfile(profile.to_string()))
}

/// Decode + score an encoded image (PNG · JPEG · WebP · GIF).
///
/// Returns the `ScanReport` as a JSON string. "No QR found" is a normal result
/// (empty `detections`); an error is raised only for invalid input / bad
/// profile / cancellation.
#[uniffi::export(default(profile = "full"))]
pub fn scan(image: Vec<u8>, profile: String) -> Result<String, ScanBindingError> {
    let profile = parse_profile(&profile)?;
    let report = Scanner::builder()
        .profile(profile)
        .build()
        .scan(ImageInput::encoded(&image))
        .map_err(|e| ScanBindingError::ScanFailed(e.to_string()))?;
    serde_json::to_string(&report).map_err(|e| ScanBindingError::ScanFailed(e.to_string()))
}

/// Decode + score a raw RGBA frame (e.g. a camera frame), no format roundtrip.
///
/// `rgba` must be `width * height * 4` bytes.
#[uniffi::export(default(profile = "frame"))]
pub fn scan_frame(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    profile: String,
) -> Result<String, ScanBindingError> {
    let profile = parse_profile(&profile)?;
    let report = Scanner::builder()
        .profile(profile)
        .build()
        .scan(ImageInput::rgba8(&rgba, width, height))
        .map_err(|e| ScanBindingError::ScanFailed(e.to_string()))?;
    serde_json::to_string(&report).map_err(|e| ScanBindingError::ScanFailed(e.to_string()))
}
```

**Type mapping notes (from current UniFFI built-in types doc):**
- `Vec<u8>` → UniFFI `bytes` → Kotlin **`ByteArray`**, Swift **`Data`**. (Use
  owned `Vec<u8>`, not `&[u8]`: UniFFI takes the buffer by value across FFI.)
- `String` → Kotlin `String`, Swift `String`.
- `u32` → Kotlin `UInt`, Swift `UInt32`.
- `Result<String, ScanBindingError>` → throws a typed exception on the foreign
  side (`ScanBindingException` in Kotlin, `ScanBindingError` enum in Swift).
- `#[uniffi::export(default(profile = "full"))]` gives the binding the same
  default-argument ergonomics the py binding has (`profile = "full"`).

**Panic posture:** the core already catches third-party decoder + scoring
panics internally (see `lib.rs` `catch_unwind`). UniFFI also converts an
unexpected Rust panic that *does* escape into a foreign exception (it does not
unwind across the FFI). No extra work needed.

**No async for v0.1.** The core is sync by design; `scan` blocks for the
wall-clock budget. Do NOT add `async fn` here (UniFFI async needs a runtime +
foreign executor wiring — see Gotchas §7). Document "call off the main thread"
in both READMEs.

---

## 2. Exact deps + current versions (confirmed 2026-06-17)

| Tool / dep | Version | Where |
|---|---|---|
| `uniffi` (lib + build + cli) | **0.31.1** (pin `"0.31"`) | crate `Cargo.toml` |
| `cargo-ndk` | **4.1.2** | Android build host (`cargo install cargo-ndk`) |
| Android NDK | r26+ (r27 LTS preferred) | via Android SDK / CI action |
| JNA | **5.13.0** `@aar` (≥5.12.0 required by UniFFI) | Android `build.gradle` |
| Kotlin | 1.9+ / 2.0 | Android module |
| Swift tools | 5.9+ (`Package.swift` `swift-tools-version:5.9`) | iOS package |
| Xcode | 15+ (xcframework + visionOS-safe slices) | macOS CI runner |

Rust toolchain stays `1.87` (matches the workspace `rust-version`).

---

## 3. `uniffi-bindgen` invocation — Kotlin AND Swift (library mode)

**Library mode** (`--library`) is the modern path: point the generator at the
compiled artifact, not a `.udl`. It is required when reading metadata baked in
by `setup_scaffolding!`.

### 3.1 Kotlin

```bash
# build the cdylib first (host build is fine for metadata extraction)
cargo build --release -p qrcode-ai-scanner-uniffi

cargo run -p qrcode-ai-scanner-uniffi --bin uniffi-bindgen -- \
  generate \
  --library target/release/libqrcode_ai_scanner_uniffi.so \
  --language kotlin \
  --out-dir build/kotlin
# → build/kotlin/<namespace>/qrcode_ai_scanner_uniffi.kt
```

> On macOS hosts the host artifact is `.dylib`; the *generated Kotlin is
> identical regardless of which host artifact you point at* — bindgen only
> reads metadata. The per-ABI `.so` (§4) is what ships, generated separately by
> cargo-ndk.

### 3.2 Swift — use the dedicated `uniffi-bindgen-swift` tool

UniFFI 0.31 ships a separate `uniffi-bindgen-swift` for the modern
xcframework-friendly flow (it emits the `.swift`, the `FFI.h`, AND an
xcframework-compatible modulemap). Add it as a second bin OR
`cargo install uniffi-bindgen-swift`. Recommended: add a `[[bin]]` mirroring
`uniffi-bindgen.rs`:

```rust
// crates/qrcode-ai-scanner-uniffi/uniffi-bindgen-swift.rs
fn main() {
    uniffi::uniffi_bindgen_swift_main()
}
```

```toml
# add to Cargo.toml
[[bin]]
name = "uniffi-bindgen-swift"
path = "uniffi-bindgen-swift.rs"
```

```bash
# point at the iOS staticlib (any slice works for source/header generation)
cargo run -p qrcode-ai-scanner-uniffi --bin uniffi-bindgen-swift -- \
  target/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a \
  build/swift/Sources \
  --swift-sources

cargo run -p qrcode-ai-scanner-uniffi --bin uniffi-bindgen-swift -- \
  target/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a \
  build/swift/Modules \
  --headers --xcframework --modulemap --modulemap-filename module.modulemap
# → build/swift/Sources/qrcode_ai_scanner_uniffi.swift
# → build/swift/Modules/qrcode_ai_scanner_uniffiFFI.h + module.modulemap
```

---

## 4. Kotlin / Android packaging

### 4.1 Build the `.so` per ABI with cargo-ndk

```bash
cargo install cargo-ndk    # 4.1.2
rustup target add \
  aarch64-linux-android armv7-linux-androideabi \
  x86_64-linux-android i686-linux-android

# -o = jniLibs root; cargo-ndk lays out the ABI subdirs automatically.
cargo ndk \
  -t arm64-v8a \
  -t armeabi-v7a \
  -t x86_64 \
  -t x86 \
  -o android/qrcodeaiscanner/src/main/jniLibs \
  build --release -p qrcode-ai-scanner-uniffi
```

Produces:
```
android/qrcodeaiscanner/src/main/jniLibs/
  arm64-v8a/libqrcode_ai_scanner_uniffi.so
  armeabi-v7a/libqrcode_ai_scanner_uniffi.so
  x86_64/libqrcode_ai_scanner_uniffi.so
  x86/libqrcode_ai_scanner_uniffi.so
```

> **ABI recommendation:** ship `arm64-v8a` + `x86_64` as mandatory (real
> devices + emulators). `armeabi-v7a` only if you must support pre-2017
> 32-bit devices; `x86` is essentially dead — make it optional behind a CI
> flag to cut artifact size/time.

### 4.2 Android library module (AAR)

Layout under `android/`:
```
android/
  settings.gradle.kts
  build.gradle.kts
  qrcodeaiscanner/
    build.gradle.kts
    src/main/
      AndroidManifest.xml          # <manifest package="studio.supernovae.qrcodeaiscanner"/>
      jniLibs/<abi>/*.so           # from cargo-ndk (§4.1)
      kotlin/.../qrcode_ai_scanner_uniffi.kt   # from bindgen (§3.1)
```

`qrcodeaiscanner/build.gradle.kts` (essentials):
```kotlin
plugins {
    id("com.android.library")
    kotlin("android")
    `maven-publish`
}
android {
    namespace = "studio.supernovae.qrcodeaiscanner"
    compileSdk = 34
    defaultConfig { minSdk = 21 }   // JNA @aar supports 21+
    // jniLibs are picked up automatically from src/main/jniLibs
}
dependencies {
    // JNA is the runtime bridge UniFFI Kotlin uses to call the .so.
    implementation("net.java.dev.jna:jna:5.13.0@aar")  // ≥5.12.0 required
}
```

Build the AAR: `./gradlew :qrcodeaiscanner:assembleRelease` →
`qrcodeaiscanner/build/outputs/aar/qrcodeaiscanner-release.aar`.

> **JNA `@aar` is non-negotiable on Android.** The `@aar` classifier pulls
> JNA's own native `.so`s (libjnidispatch) per ABI. A plain `jna:5.13.0` (the
> desktop jar) will compile but **crash at runtime** with
> `UnsatisfiedLinkError` on device. This is the #1 UniFFI-Android footgun.

### 4.3 Publish target — **JitPack first**, Maven Central later ✅

| | JitPack (RECOMMENDED v0.1) | Maven Central (Sonatype Central Portal) |
|---|---|---|
| Setup | add `jitpack.yml`, tag a release — JitPack builds from the GitHub tag on demand | register namespace `studio.supernovae` / `io.github.supernovae-st`, **GPG-sign every artifact**, upload bundle to Central Portal |
| Signing | none | mandatory PGP signing (key management, CI secrets) |
| Consumer adds | `maven { url 'https://jitpack.io' }` + `com.github.supernovae-st:qrcode-ai-scanner:vX.Y.Z` | nothing (default repo) — `studio.supernovae:qrcodeaiscanner:X.Y.Z` |
| Native `.so` in artifact? | yes if AAR is built by JitPack (needs NDK in `jitpack.yml`) — **tricky**; simpler to attach a prebuilt AAR as a GitHub release asset and have JitPack serve it, OR publish AAR straight to JitPack | yes (we upload the AAR we built in CI) |
| Friction | low — zero signing, no account approval wait | high — Sonatype namespace verification + GPG + bundle upload ceremony |

**Recommendation:** **JitPack for v0.1** to ship fast with zero signing
friction. Because JitPack building an NDK-cross-compiled AAR is awkward, the
pragmatic v0.1 is: **CI builds the AAR (it has the NDK + cargo-ndk), then
`maven-publish` pushes it to a GitHub Packages or a JitPack-served release.**
Graduate to **Maven Central in v0.2** once the API is stable and the GPG
signing pipeline is set up (it's a one-time cost worth paying for the
zero-config consumer experience). Flag the signing setup as a separate task.

---

## 5. Swift / iOS packaging (xcframework + SwiftPM)

### 5.1 Build static libs for device + simulator

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios

# device (arm64)
cargo build --release -p qrcode-ai-scanner-uniffi --target aarch64-apple-ios
# simulator arm64 (Apple silicon Macs)
cargo build --release -p qrcode-ai-scanner-uniffi --target aarch64-apple-ios-sim
# simulator x86_64 (Intel Macs / older CI)
cargo build --release -p qrcode-ai-scanner-uniffi --target x86_64-apple-ios
```

The **simulator** slice must be a fat lib (arm64-sim + x86_64-sim) via `lipo`;
device stays a single arm64 lib. xcframework cannot contain two slices for the
same platform, so sim arches are `lipo`-merged into one:

```bash
mkdir -p build/sim
lipo -create \
  target/aarch64-apple-ios-sim/release/libqrcode_ai_scanner_uniffi.a \
  target/x86_64-apple-ios/release/libqrcode_ai_scanner_uniffi.a \
  -output build/sim/libqrcode_ai_scanner_uniffi.a
```

### 5.2 Assemble the xcframework

Generate Swift sources + headers + modulemap (§3.2), then:

```bash
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a \
  -headers build/swift/Modules \
  -library build/sim/libqrcode_ai_scanner_uniffi.a \
  -headers build/swift/Modules \
  -output build/QrcodeAiScannerFFI.xcframework
```

> The `module.modulemap` (renamed/placed in the `-headers` dir, alongside
> `qrcode_ai_scanner_uniffiFFI.h`) is what makes the generated `.swift` able to
> `import QrcodeAiScannerFFI`. Use the `--xcframework` modulemap flavour from
> bindgen (§3.2) — the plain modulemap is NOT xcframework-compatible.

### 5.3 `Package.swift` (SwiftPM)

Repo root or a dedicated `swift/` dir:
```swift
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "QrcodeAiScanner",
    platforms: [.iOS(.v13)],
    products: [
        .library(name: "QrcodeAiScanner", targets: ["QrcodeAiScanner"]),
    ],
    targets: [
        // The generated Swift bindings (the friendly API).
        .target(
            name: "QrcodeAiScanner",
            dependencies: ["QrcodeAiScannerFFI"],
            path: "swift/Sources/QrcodeAiScanner"   // holds qrcode_ai_scanner_uniffi.swift
        ),
        // The compiled Rust, as a binary xcframework.
        .binaryTarget(
            name: "QrcodeAiScannerFFI",
            // v0.1: local path during dev; release: url+checksum to a GH release asset
            path: "build/QrcodeAiScannerFFI.xcframework"
        ),
    ]
)
```

> **Release packaging:** zip the xcframework, attach to a GitHub release, and
> switch `.binaryTarget` to
> `url: "...QrcodeAiScannerFFI.xcframework.zip", checksum: "<swift package compute-checksum>"`.
> SwiftPM consumers then just add the repo URL — no build step on their side.

---

## 6. CI workflow sketches

New file `.github/workflows/mobile.yml`, modelled on the existing `python.yml`
/ `npm-publish.yml` (tag-triggered + `workflow_dispatch`). Two jobs.

### 6.1 Android job

```yaml
name: mobile
on:
  push:
    tags: ["v*"]
  workflow_dispatch:

jobs:
  android:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: aarch64-linux-android,armv7-linux-androideabi,x86_64-linux-android,i686-linux-android
      - uses: nttld/setup-ndk@v1
        with: { ndk-version: r27 }
      - run: cargo install cargo-ndk --version 4.1.2 --locked
      - name: build .so per ABI
        run: |
          cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -t x86 \
            -o android/qrcodeaiscanner/src/main/jniLibs \
            build --release -p qrcode-ai-scanner-uniffi
      - name: generate Kotlin bindings
        run: |
          cargo build --release -p qrcode-ai-scanner-uniffi
          cargo run -p qrcode-ai-scanner-uniffi --bin uniffi-bindgen -- \
            generate --library target/release/libqrcode_ai_scanner_uniffi.so \
            --language kotlin \
            --out-dir android/qrcodeaiscanner/src/main/kotlin
      - uses: actions/setup-java@v4
        with: { distribution: temurin, java-version: '17' }
      - name: assemble AAR
        run: ./gradlew -p android :qrcodeaiscanner:assembleRelease
      - uses: actions/upload-artifact@v4
        with:
          name: android-aar
          path: android/qrcodeaiscanner/build/outputs/aar/*.aar
      # v0.2: maven-publish to Central (needs GPG secrets in an env-gated release job)
```

### 6.2 iOS job

```yaml
  ios:
    runs-on: macos-15        # Xcode 16, Apple silicon
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: aarch64-apple-ios,aarch64-apple-ios-sim,x86_64-apple-ios
      - name: build static libs
        run: |
          cargo build --release -p qrcode-ai-scanner-uniffi --target aarch64-apple-ios
          cargo build --release -p qrcode-ai-scanner-uniffi --target aarch64-apple-ios-sim
          cargo build --release -p qrcode-ai-scanner-uniffi --target x86_64-apple-ios
      - name: generate Swift + headers + xcframework modulemap
        run: |
          mkdir -p swift/Sources/QrcodeAiScanner build/swift/Modules
          cargo run -p qrcode-ai-scanner-uniffi --bin uniffi-bindgen-swift -- \
            target/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a \
            swift/Sources/QrcodeAiScanner --swift-sources
          cargo run -p qrcode-ai-scanner-uniffi --bin uniffi-bindgen-swift -- \
            target/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a \
            build/swift/Modules --headers --xcframework --modulemap --modulemap-filename module.modulemap
      - name: lipo sim + create xcframework
        run: |
          mkdir -p build/sim
          lipo -create \
            target/aarch64-apple-ios-sim/release/libqrcode_ai_scanner_uniffi.a \
            target/x86_64-apple-ios/release/libqrcode_ai_scanner_uniffi.a \
            -output build/sim/libqrcode_ai_scanner_uniffi.a
          xcodebuild -create-xcframework \
            -library target/aarch64-apple-ios/release/libqrcode_ai_scanner_uniffi.a -headers build/swift/Modules \
            -library build/sim/libqrcode_ai_scanner_uniffi.a -headers build/swift/Modules \
            -output build/QrcodeAiScannerFFI.xcframework
      - name: zip + checksum
        run: |
          (cd build && zip -ry QrcodeAiScannerFFI.xcframework.zip QrcodeAiScannerFFI.xcframework)
          swift package compute-checksum build/QrcodeAiScannerFFI.xcframework.zip > build/checksum.txt
      - uses: actions/upload-artifact@v4
        with:
          name: ios-xcframework
          path: |
            build/QrcodeAiScannerFFI.xcframework.zip
            build/checksum.txt
```

A `test` job (host build + a smoke `cargo run --bin uniffi-bindgen -- generate`
on PRs) mirrors `python.yml`'s `test` job and should gate PRs.

---

## 7. Gotchas

1. **JNA `@aar`, not the jar (Android).** Plain `net.java.dev.jna:jna:5.13.0`
   compiles but throws `UnsatisfiedLinkError` at runtime — it lacks the native
   `libjnidispatch.so` per ABI. Always the `@aar` classifier. (#1 footgun.)

2. **Version skew between generated bindings and the `.so`/`.a`.** The Kotlin
   `.kt` / Swift `.swift` carry a UniFFI contract checksum that is verified
   against the loaded native lib at startup; a mismatch panics immediately.
   **Mandate:** generate bindings and build the native lib **in the same CI run
   from the same commit** (never check generated bindings into git and rebuild
   the lib separately). The `[[bin]] uniffi-bindgen` living in the crate (same
   `uniffi` dep) is the structural guard against generator-vs-lib skew.

3. **UniFFI async needs a foreign executor** (`uniffi::export async fn` + a
   registered runtime + the foreign side's async executor). We deliberately
   stay **sync** for v0.1 (the core is sync). If a future version wants
   `async`, it's a real wiring task, not a flag — scope it separately.

4. **`Vec<u8>` is copied across the FFI** (no zero-copy). A 4K RGBA frame is
   ~33 MB — the bytes are copied into the Rust side. Fine for v0.1 (matches the
   py binding's `.to_vec()`), but note it for high-fps camera loops; a future
   optimisation could pass a direct `ByteBuffer`/pointer (out of scope).

5. **iOS xcframework: one slice per platform.** Device = arm64 only; simulator
   = `lipo`-merged arm64-sim + x86_64-sim. Putting two libs for the same
   platform-variant in `-create-xcframework` errors out.

6. **Bitcode is dead** (Xcode 14+) — do not pass bitcode flags; modern
   xcframeworks don't need it.

7. **Maven Central signing.** Every artifact (AAR, sources jar, POM) must be
   GPG-signed and the public key on a keyserver. This is why v0.1 = JitPack.
   The signing key → CI secret → `signing {}` Gradle block is a separate task.

8. **16 KB page size (Android 15+).** NDK r26+ links `.so`s 16 KB-aligned by
   default; ensure the NDK in CI is r26+ (we pin r27) so the lib loads on
   Pixel 8+ / Android 15 devices.

9. **Default-argument support** (`#[uniffi::export(default(...))]`) requires
   uniffi ≥0.28; we're on 0.31 so it's available — gives Kotlin/Swift the same
   `profile="full"` ergonomics as Python.

---

## 8. RISK section

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | **AGPL-3.0 copyleft for apps bundling the lib** — see §9, FLAGGED FOR OPERATOR, not resolved here | — | High (legal) | Operator decision — §9 |
| R2 | Binding/lib checksum skew ships a broken artifact | Med | High (runtime crash) | Generate + build in one CI run from one commit (§7.2); in-crate `uniffi-bindgen` bin |
| R3 | JNA jar-vs-aar mistake → runtime `UnsatisfiedLinkError` | Med | High | `@aar` classifier mandated (§4.2); add an instrumented (device/emulator) smoke test in CI before publish |
| R4 | Maven Central signing pipeline stalls the release | Med | Med | Ship v0.1 on JitPack (zero signing); treat Central as a v0.2 task |
| R5 | UniFFI 0.31 → 0.32+ breaking changes (the project moves fast) | Med | Med | Pin `"0.31"`; `cargo install --locked`; gate upgrades behind the binding test job |
| R6 | iOS sim/device slice or modulemap misconfig → "no such module" | Med | Med | Use `--xcframework` modulemap flavour (§3.2); CI builds a tiny SwiftPM consumer that imports + calls `scan` |
| R7 | `ScanReport` schema evolves; JSON consumers break silently | Low | Med | JSON is the SSOT (`spec/scan-report.schema.json`); add a CI assertion that the bindings' JSON validates against the schema (same fixture the py tests use) |
| R8 | Artifact size (4 ABIs + xcframework) bloats the lib | Low | Low | Drop `x86` (and optionally `armeabi-v7a`) per §4.1; `strip = true` is already in `[profile.release]` |
| R9 | `edition = "2024"` / `rust-version 1.87` vs NDK toolchain mismatch | Low | Med | CI pins toolchain + NDK r27; matches the workspace pin |

**Migration note (Option A → typed records, v0.2):** if consumers want typed
access, add `#[derive(uniffi::Record)]` mirrors *incrementally* (e.g. a slim
`ScanSummary { detection_count, score }`) exposed by a *new* method, leaving
`scan`/`scan_frame` JSON methods intact. No breaking change, no big-bang mirror
of the whole `report.rs` tree.

---

## 9. ⚠️ OPERATOR FLAG — AGPL-3.0-or-later copyleft (DO NOT RESOLVE)

This repo is **AGPL-3.0-or-later**. The Kotlin/Android (Maven/JitPack) and
Swift/iOS (SwiftPM) artifacts produced here are **derived works of AGPL code**.
Any third-party mobile app that **links/bundles** this binding inherits AGPL
obligations:

- The app, as a whole, would be subject to **AGPL-3.0** copyleft — distributing
  the app (App Store / Play Store) plausibly triggers the obligation to offer
  the **complete corresponding source** of the app under AGPL-compatible terms.
- AGPL's **§13 network clause** extends this to apps that interact with users
  over a network using the covered code.
- This is a hard blocker for **closed-source / proprietary** mobile apps and a
  serious consideration even for many open-source ones (license compatibility).
- Common resolutions exist (NOT decided here): a **commercial / dual license**
  for the binding, an exception clause, or a more permissive license for the
  binding crate specifically. **This is an operator/legal decision.**

**Action: flag to the operator before publishing any mobile artifact.** Do not
pick a resolution in this plan.

---

## 10. Execution checklist (ordered)

1. `crates/qrcode-ai-scanner-uniffi/` — `Cargo.toml`, `src/lib.rs`,
   `uniffi-bindgen.rs`, `uniffi-bindgen-swift.rs` (§1).
2. Add the crate to root `Cargo.toml` `exclude` (§1.1).
3. `cargo build --release -p qrcode-ai-scanner-uniffi` (host) + smoke-gen
   Kotlin (§3.1) → confirm the wrapper compiles & metadata reads.
4. Android: cargo-ndk per-ABI `.so` (§4.1) + AAR module (§4.2).
5. iOS: per-target static libs + xcframework + `Package.swift` (§5).
6. `.github/workflows/mobile.yml` (§6).
7. v0.1 publish: JitPack (Android) + GH-release xcframework zip (iOS) (§4.3/§5.3).
8. **Before any publish: surface the AGPL flag (§9) to the operator.**
9. v0.2 backlog: Maven Central + GPG signing; optional typed records; async if
   ever needed.
