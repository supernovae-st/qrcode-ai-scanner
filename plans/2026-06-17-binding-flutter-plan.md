# Flutter/Dart binding — implementation plan

> **Status**: DEV-READY · author hand-off · 2026-06-17
> **Goal**: ship a `flutter_rust_bridge` (FRB) v2 binding of the existing Rust
> core `crates/qrcode-ai-scanner` to **pub.dev**, mirroring the canonical
> binding surface (`scan` / `scanFrame`) that the Python (PyO3) and Node (napi)
> bindings already expose.
> **License**: AGPL-3.0-or-later (see RISK §11.1 — copyleft for app bundlers).

This plan assumes **zero context**. Every command, file path, and version is
spelled out. Read top to bottom; do not skip §0 (preflight) or §2 (versions).

---

## 0 · Preflight — what you must understand first

The Rust core (`crates/qrcode-ai-scanner/src/lib.rs`) is **sync, `Send + Sync`,
no interior state**. Every binding is a thin wrapper that:

1. Parses a profile string → `ScanProfile` via the canonical
   `ScanProfile::from_name(&str) -> Option<ScanProfile>` (NEVER reimplement the
   string→enum map; all surfaces share that one parser).
2. Builds `Scanner::builder().profile(p).build()`.
3. Calls `.scan(ImageInput::encoded(&bytes))` or
   `.scan(ImageInput::rgba8(&bytes, w, h))`.
4. Returns the `ScanReport` (serde-`Serialize`, JSON Schema at
   `spec/scan-report.schema.json`).

The canonical binding surface every binding mirrors (verbatim from the brief):

```
scan(image_bytes, profile="full") -> ScanReport
scan_frame(rgba_bytes, width, height, profile="frame") -> ScanReport
profile ∈ {"full","fast","frame"}
```

> **NOTE on defaults**: Python `scan` defaults `profile="full"`, `scan_frame`
> defaults `profile="frame"`. Mirror that exactly in Dart.

Key facts harvested from the repo (do not re-derive):

| Fact | Value | Source |
|---|---|---|
| Core crate name | `qrcode-ai-scanner` | `crates/qrcode-ai-scanner/Cargo.toml` |
| Core needs `serde` feature for `Serialize` | `features = ["serde"]` | py/node Cargo.toml |
| Core default features | `engine-rxing, engine-rqrr, parallel, serde` | core `Cargo.toml` L15 |
| `parallel` = rayon, **native only** (keep for mobile, it's native) | — | core `Cargo.toml` L19 |
| Workspace edition / MSRV | `edition 2024`, `rust-version 1.87` | root `Cargo.toml` |
| Workspace excludes the py crate (maturin-built) | `exclude = ["crates/qrcode-ai-scanner-py"]` | root `Cargo.toml` |
| Wire-type SSOT (TS) | `bindings/report-types.d.ts` | repo root `bindings/` |
| Report version markers | `pipeline=1`, `score_contract=3` | `report.rs::Versions::current()` |
| Lint policy | `unwrap_used = "deny"`, `expect_used = "deny"` | root `Cargo.toml` |

**`ScanReport` serde shape** (what crosses the FFI boundary): `detections[]`
(each: `symbology`, `content{text,raw(base64),charset}`, `payload`, `corners`,
`meta`, `engines`), `score` (nullable; `null` in `frame` profile), `hints[]`,
`trace`, `versions{scanner,pipeline,score_contract}`. `raw` bytes are **base64
strings** in the JSON wire form. All enums are `snake_case`.

---

## 1 · Directory & crate structure

### 1.1 Decision: a NEW excluded cdylib/staticlib crate + a Flutter **plugin** package

Mirror the Python precedent: **the FFI crate is EXCLUDED from the cargo
workspace** (like `crates/qrcode-ai-scanner-py`). Reasons:

- FRB v2's generated Rust glue + `flutter_rust_bridge` runtime dep would
  otherwise pull into `cargo test/clippy --workspace` (CI core) and break the
  pedantic/zero-unwrap lint gates on machine-generated code.
- The crate-type must flip (`cdylib` for Android, `staticlib` for iOS); FRB/
  cargokit handle this, but it is alien to the workspace's pure-lib posture.
- It needs a different toolchain surface (NDK, Flutter) than `cargo
  --workspace`.

> Excluded crates **cannot** use `version.workspace = true` — spell versions
> out (exactly as the py crate does).

### 1.2 Layout

FRB v2's `integrate --template plugin` produces a Flutter **plugin** package
(publishable to pub.dev) with the Rust crate nested under `rust/`. We adapt it
so the Rust crate lives in the repo's `crates/` tree for consistency, and the
Dart plugin lives at repo root under `bindings/flutter/` (sibling to the
existing `bindings/report-types.d.ts`).

```
scanner/
├── Cargo.toml                         # add crate to `exclude` (see §1.3)
├── bindings/
│   ├── report-types.d.ts              # existing wire SSOT (TS)
│   └── flutter/                        # NEW — the pub.dev package root
│       ├── pubspec.yaml                # package: qrcode_ai_scanner
│       ├── README.md
│       ├── CHANGELOG.md
│       ├── LICENSE                     # AGPL-3.0 (copy repo LICENSE)
│       ├── analysis_options.yaml
│       ├── lib/
│       │   ├── qrcode_ai_scanner.dart  # public API (hand-written facade)
│       │   ├── src/
│       │   │   ├── frb_generated.dart  # FRB-GENERATED (do not edit)
│       │   │   ├── frb_generated.io.dart
│       │   │   ├── frb_generated.web.dart
│       │   │   └── api/
│       │   │       └── scan.dart       # FRB-GENERATED mirror of api/scan.rs
│       │   └── report.dart             # typed ScanReport (see §4 decision)
│       ├── android/                    # cargokit gradle glue (generated)
│       │   ├── build.gradle
│       │   └── src/main/AndroidManifest.xml
│       ├── ios/                        # cargokit podspec (generated)
│       │   └── qrcode_ai_scanner.podspec
│       ├── macos/  linux/  windows/    # desktop cargokit glue (optional, see §9)
│       ├── cargokit/                   # vendored cargokit build engine (generated)
│       └── rust_builder/               # cargokit CMake/gradle entry (generated)
└── crates/
    └── qrcode-ai-scanner-flutter/      # NEW — the FFI Rust crate
        ├── Cargo.toml                  # excluded; cdylib+staticlib
        ├── build.rs                    # `flutter_rust_bridge::frb_build()` shim if needed
        └── src/
            ├── lib.rs                  # `mod api; mod frb_generated;`
            ├── api/
            │   └── scan.rs             # HAND-WRITTEN: scan/scanFrame wrappers
            └── frb_generated.rs        # FRB-GENERATED (do not edit)
```

> **Why a separate `crates/qrcode-ai-scanner-flutter/` instead of nesting Rust
> inside `bindings/flutter/rust/`** (FRB default): keeps all Rust crates under
> `crates/`, matches the py/node/wasm precedent, and lets the FFI crate depend
> on the core via a clean relative path
> (`path = "../qrcode-ai-scanner"`). The `pubspec.yaml` + cargokit must point at
> this path — see §8.2 for the cargokit `manifest_dir` wiring.

### 1.3 Workspace exclusion

In root `Cargo.toml`, extend `exclude`:

```toml
exclude = [
  "crates/qrcode-ai-scanner-py",
  "crates/qrcode-ai-scanner-flutter",   # FRB-generated glue + flutter_rust_bridge runtime dep
]
```

---

## 2 · Exact dependencies & CURRENT versions

Verified mid-2026 via context7 (`/fzyzcjy/flutter_rust_bridge`) and official
registries:

| Component | Version (CURRENT) | Source | Notes |
|---|---|---|---|
| `flutter_rust_bridge` (Dart pkg, pub.dev) | **2.12.0** | pub.dev (latest stable; 2.13.0-beta.1 prerelease exists) | Dart-side runtime |
| `flutter_rust_bridge` (Rust crate, crates.io) | **2.12.0** | must match Dart pkg EXACTLY | Rust-side runtime + macros |
| `flutter_rust_bridge_codegen` (CLI) | **2.12.0** | `cargo install flutter_rust_bridge_codegen` | pin `--version 2.12.0` |
| `cargo-ndk` | **4.1.2** (2025-08-09; MSRV 1.86) | github.com/bbqsrc/cargo-ndk | Android ABI builds |
| Android NDK | r25c+ (use what Flutter's AGP pins; r26/r27 fine) | — | set `ANDROID_NDK_HOME` |
| Flutter SDK | ≥ 3.x stable (Dart ≥ 3.3) | — | FRB v2 requires sound null-safety |
| Rust toolchain | 1.87+ (workspace MSRV) + targets §5 | — | — |

> **HARD RULE**: the `flutter_rust_bridge` Rust crate version, the Dart package
> version, and the `flutter_rust_bridge_codegen` CLI version MUST all be the
> SAME (2.12.0). A mismatch produces "codec version" runtime panics. Pin all
> three.

### 2.1 `crates/qrcode-ai-scanner-flutter/Cargo.toml`

```toml
# Excluded from the workspace (see root Cargo.toml): built by cargokit/FRB, not
# cargo workspace tooling. Versions are spelled out (excluded crates cannot
# inherit `workspace = true`).
[package]
name = "qrcode-ai-scanner-flutter"
version = "0.4.0"            # track the workspace product version manually
edition = "2024"
rust-version = "1.87"
license = "AGPL-3.0-or-later"
repository = "https://github.com/supernovae-st/qrcode-ai-scanner"
description = "Flutter/Dart bindings for qrcode-ai-scanner — QR decoding + scannability scoring for artistic / AI-generated QR codes."
publish = false             # ships to pub.dev as a Dart package, not crates.io

[lib]
crate-type = ["cdylib", "staticlib"]   # cdylib=Android/desktop, staticlib=iOS

[dependencies]
flutter_rust_bridge = "=2.12.0"        # MUST equal the codegen + Dart pkg version
# Core: same feature set as node — serde for the wire contract. `parallel`
# (rayon) is fine on mobile (native targets); it is in the core default set.
scanner-core = { package = "qrcode-ai-scanner", path = "../qrcode-ai-scanner", features = ["serde"] }
serde_json = "1"

# Binding crate: keep the safety lints, drop pure-lib pedantry (FRB macro
# expansions fight the pedantic group — same posture as the node crate).
[lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
```

### 2.2 `bindings/flutter/pubspec.yaml` (dependency block)

```yaml
dependencies:
  flutter:
    sdk: flutter
  flutter_rust_bridge: 2.12.0
```

---

## 3 · FRB codegen steps (commands)

Run from repo root unless noted. **Do this once to scaffold, then re-run
`generate` whenever `api/scan.rs` changes.**

### 3.1 Install the CLI (pinned)

```bash
cargo install flutter_rust_bridge_codegen --version 2.12.0 --locked
flutter_rust_bridge_codegen --version   # expect 2.12.0
```

### 3.2 One-time scaffold (creates the plugin + cargokit + rust_builder glue)

FRB's `integrate` expects to run inside a Flutter package. Create the Flutter
plugin shell first, then integrate:

```bash
# 1. create the publishable plugin package
flutter create --template=plugin_ffi --platforms=android,ios \
  --org studio.supernovae --project-name qrcode_ai_scanner \
  bindings/flutter

# 2. integrate FRB into it (generates frb_generated.*, cargokit, rust_builder)
cd bindings/flutter
flutter_rust_bridge_codegen integrate \
  --rust-crate-name qrcode-ai-scanner-flutter \
  --rust-crate-dir ../../crates/qrcode-ai-scanner-flutter \
  --template plugin
```

> If `integrate` insists on creating its own `rust/` crate, let it scaffold,
> then move/point it: delete the generated `rust/`, set the cargokit
> `manifest_dir` to `../../crates/qrcode-ai-scanner-flutter` (see §8.2), and
> author `api/scan.rs` there. The cleaner alternative if path-rewiring fights
> you: keep the Rust crate where `integrate` puts it
> (`bindings/flutter/rust/`) and accept the deviation from the `crates/`
> convention — **document the choice in the crate header**. Prefer the
> `crates/` layout; fall back only if cargokit path resolution blocks you.

### 3.3 Author the Rust API surface (`crates/qrcode-ai-scanner-flutter/src/api/scan.rs`)

```rust
//! Flutter bindings for `qrcode-ai-scanner` (flutter_rust_bridge v2).
//!
//! Thin wrapper over the Rust core: bytes → scan → the same versioned
//! `ScanReport`, returned to Dart as a JSON string (the serde contract, the
//! cross-surface SSOT in `spec/`). FRB runs these on its Rust worker pool, so
//! Dart `await`s without blocking the UI isolate.

use scanner_core::{ImageInput, ScanProfile, Scanner};

fn parse_profile(profile: &str) -> Result<ScanProfile, String> {
    // Canonical parser — same path as py/node/wasm. Keeps every surface in
    // sync as profiles evolve.
    ScanProfile::from_name(profile)
        .ok_or_else(|| format!("unknown profile {profile:?} (expected 'full', 'fast', or 'frame')"))
}

/// Decode + score an encoded image (PNG · JPEG · WebP · GIF).
/// Returns the `ScanReport` as a JSON string. "No QR found" is a normal
/// result (empty `detections`); `Err` only for invalid input / cancellation.
pub fn scan(image: Vec<u8>, profile: String) -> Result<String, String> {
    let profile = parse_profile(&profile)?;
    let report = Scanner::builder()
        .profile(profile)
        .build()
        .scan(ImageInput::encoded(&image))
        .map_err(|e| format!("{e} [{}]", e.code()))?;
    serde_json::to_string(&report).map_err(|e| format!("serialize: {e}"))
}

/// Decode + score a raw RGBA frame (camera frame). `rgba` = width*height*4 bytes.
pub fn scan_frame(rgba: Vec<u8>, width: u32, height: u32, profile: String) -> Result<String, String> {
    let profile = parse_profile(&profile)?;
    let report = Scanner::builder()
        .profile(profile)
        .build()
        .scan(ImageInput::rgba8(&rgba, width, height))
        .map_err(|e| format!("{e} [{}]", e.code()))?;
    serde_json::to_string(&report).map_err(|e| format!("serialize: {e}"))
}
```

`src/lib.rs`:

```rust
mod api;
mod frb_generated; // FRB-generated, do not edit
```

> **Why return `String` (JSON), not native FRB structs**: see §4. Short
> version — the core's `ScanReport` is a deep, `#[non_exhaustive]`, enum-rich
> tree; round-tripping it through serde_json (exactly as Python does via
> `serde_json::Value`, and Node does via JSON string) guarantees the Dart side
> sees the **schema-conformant** shape, not FRB's struct-mirroring guesses
> (which mishandle `#[serde(tag=...)]` payload enums, `skip_serializing_if`,
> base64 `raw`, and tuple→array). One serde contract, every surface.
> `e.code()` surfaces the `QRS-xxx` code into the message, matching node.

### 3.4 Generate the bridge (re-run after any `api/*.rs` change)

```bash
cd bindings/flutter
flutter_rust_bridge_codegen generate
# regenerates: lib/src/frb_generated*.dart, lib/src/api/scan.dart,
#              crates/qrcode-ai-scanner-flutter/src/frb_generated.rs
```

FRB reads its config from `flutter_rust_bridge.yaml` (written by `integrate`):

```yaml
rust_input: crate::api
rust_root: ../../crates/qrcode-ai-scanner-flutter
dart_output: lib/src
```

---

## 4 · Dart API surface — DECISION & justification

**Decision: ship a thin hand-written facade `lib/qrcode_ai_scanner.dart` that
calls the generated `scan`/`scanFrame` (which return `String`), `jsonDecode`s
to `Map<String, dynamic>`, and ALSO ships typed Dart classes mirroring
`ScanReport` as the PUBLIC return type.** I.e. **JSON over the FFI boundary +
typed Dart on the public surface**.

```dart
// lib/qrcode_ai_scanner.dart  (hand-written public facade)
import 'dart:convert';
import 'src/frb_generated.dart';
import 'src/api/scan.dart' as ffi;
import 'report.dart';

class QrcodeAiScanner {
  static Future<void> init() => RustLib.init(); // call once at app start

  /// Decode + score an encoded image (PNG/JPEG/WebP/GIF).
  static Future<ScanReport> scan(Uint8List imageBytes, {String profile = 'full'}) async {
    final json = await ffi.scan(image: imageBytes, profile: profile);
    return ScanReport.fromJson(jsonDecode(json) as Map<String, dynamic>);
  }

  /// Decode + score a raw RGBA camera frame (width*height*4 bytes).
  static Future<ScanReport> scanFrame(Uint8List rgba, int width, int height,
      {String profile = 'frame'}) async {
    final json = await ffi.scanFrame(rgba: rgba, width: width, height: height, profile: profile);
    return ScanReport.fromJson(jsonDecode(json) as Map<String, dynamic>);
  }
}
```

`lib/report.dart` = hand-written typed classes (`ScanReport`, `Detection`,
`Score`, `AxisScore`, `Payload` sealed class, enums `Grade`, `Symbology`,
`EcLevel`, `Charset`, …) with `fromJson` factories, mirroring
`bindings/report-types.d.ts` field-for-field.

### Why this over the two pure alternatives

| Approach | Pro | Con | Verdict |
|---|---|---|---|
| **FRB native structs** (let FRB mirror `ScanReport`) | zero-copy, fully typed end-to-end | FRB cannot faithfully reproduce serde attrs (`tag` payload enum, base64 `raw`, `skip_serializing_if`, `#[non_exhaustive]`); would DRIFT from `spec/scan-report.schema.json`; couples Dart types to FFI codec | ❌ rejected — breaks the "one serde wire contract" invariant |
| **Raw JSON / `Map<String,dynamic>`** (return the map, no typing) | trivial, always schema-correct, additive-evolution-proof | bad DX (stringly-typed access), no autocomplete, no compile-time safety | partial |
| **JSON wire + typed Dart facade** (CHOSEN) | schema-correct (serde is the SSOT, like py/node); good DX; additive-evolution tolerant (unknown fields ignored in `fromJson`); decoupled from FFI codec | hand-maintain `report.dart` against the schema (mitigated: it's a port of the existing `bindings/report-types.d.ts`) | ✅ |

> **Tradeoff vs the JSON Schema**: `report.dart` is a SECOND hand-written mirror
> of the wire contract (TS already exists). Keep it honest by: (a) a Dart unit
> test that round-trips a golden `ScanReport` JSON fixture through
> `ScanReport.fromJson(...).toJson()` and asserts schema-equality; (b) a note in
> `report.dart` pointing at `spec/scan-report.schema.json` + `bindings/report-types.d.ts`
> as the SSOT. Tolerate unknown enum/field values (the schema says consumers
> MUST) — `fromJson` should default-case unknown enums, never throw. **Future
> improvement (R2)**: extend `scripts/sync-report-types.mjs` to also emit
> `report.dart` from the same SSOT, eliminating the hand-maintenance.

---

## 5 · Android build (cargo-ndk) + iOS build (xcframework/static)

FRB v2 + cargokit compile the Rust **at Flutter app build time** — the plugin
ships **source**, the consuming app's `flutter build` triggers the cargo build
through gradle (Android) / CocoaPods+CMake (iOS/desktop). See §8 for the
build-at-app-time vs precompiled decision.

### 5.1 Rust targets to install

```bash
rustup target add \
  aarch64-linux-android armv7-linux-androideabi x86_64-linux-android \
  aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
```

### 5.2 Android (cargo-ndk 4.1.2)

cargokit invokes cargo-ndk under the hood, but the canonical manual command
(for CI smoke / debugging) is:

```bash
cargo install cargo-ndk --version 4.1.2 --locked
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/<version>   # e.g. 27.0.12077973
cd crates/qrcode-ai-scanner-flutter
cargo ndk \
  -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o ../../bindings/flutter/android/src/main/jniLibs \
  build --release
```

ABI → target mapping (cargo-ndk handles it): `arm64-v8a`→`aarch64-linux-android`,
`armeabi-v7a`→`armv7-linux-androideabi`, `x86_64`→`x86_64-linux-android`. Add
`-t x86` only if you must support 32-bit emulators (usually skip). cargokit's
generated `android/build.gradle` already wires a `cargoBuild*` task that runs
this on `flutter build apk` — you only run it manually for CI verification.

### 5.3 iOS (staticlib → cargokit produces the framework)

iOS requires a **static** library (`crate-type = ["staticlib"]`, already set).
cargokit's generated `ios/qrcode_ai_scanner.podspec` compiles
`aarch64-apple-ios` (device) + `aarch64-apple-ios-sim`/`x86_64-apple-ios`
(simulator) and links the `.a` into the Flutter app at `pod install` /
`flutter build ios` time. **You do not hand-build an xcframework** in the
build-at-app-time model — cargokit's CMake/podspec link the static archive
directly.

Manual verification build (CI smoke, macOS runner only):

```bash
cd crates/qrcode-ai-scanner-flutter
cargo build --release --target aarch64-apple-ios
cargo build --release --target aarch64-apple-ios-sim
```

> Only the **precompiled** distribution path (§8) needs a real
> `.xcframework` (lipo the device + sim `.a` into a `*.xcframework`). For the
> recommended build-at-app-time path, skip it.

---

## 6 · Example app sketch

`bindings/flutter/example/` (Flutter auto-creates it). Minimal `main.dart`:

```dart
import 'dart:typed_data';
import 'package:flutter/material.dart';
import 'package:qrcode_ai_scanner/qrcode_ai_scanner.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await QrcodeAiScanner.init();          // RustLib.init() once
  runApp(const DemoApp());
}

class DemoApp extends StatefulWidget { const DemoApp({super.key}); @override State<DemoApp> createState() => _S(); }

class _S extends State<DemoApp> {
  String _out = 'pick an image';
  Future<void> _scan(Uint8List bytes) async {
    final report = await QrcodeAiScanner.scan(bytes, profile: 'full');
    setState(() => _out = report.detections.isEmpty
        ? 'no QR found'
        : '${report.detections.first.content.text}  '
          'score=${report.score?.grade ?? "n/a"}');
  }
  @override Widget build(BuildContext c) => MaterialApp(
    home: Scaffold(
      appBar: AppBar(title: const Text('qrcode_ai_scanner demo')),
      body: Center(child: Text(_out)),
      // wire an image_picker button → _scan(bytes) in the real example
    ),
  );
}
```

The example doubles as the pub.dev "Example" tab AND the integration-test host
(`example/integration_test/`) — FRB's `integrate` scaffolds an integration test
that loads the real native lib; extend it to assert a known QR fixture decodes.

---

## 7 · CI workflow sketch (`.github/workflows/flutter.yml`)

Mirror `python.yml`'s shape: a `test` job on every push/PR, build jobs gated on
tags, a publish job on `v*` tags. pub.dev publishing uses **OIDC automated
publishing** (no token), exactly like python.yml's PyPI trusted publishing.

```yaml
name: flutter

on:
  push:
    branches: [main]
    tags: ["v*"]
  pull_request:
  workflow_dispatch:

permissions:
  contents: read

defaults:
  run:
    working-directory: bindings/flutter

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: subosito/flutter-action@v2
        with: { channel: stable }
      - run: cargo install flutter_rust_bridge_codegen --version 2.12.0 --locked
      - run: flutter_rust_bridge_codegen generate --no-build-runner
      - run: dart pub get
      - run: dart analyze
      - run: dart test          # report.dart round-trip + fromJson golden tests

  build-android:
    if: startsWith(github.ref, 'refs/tags/') || github.event_name == 'workflow_dispatch'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: aarch64-linux-android,armv7-linux-androideabi,x86_64-linux-android
      - uses: nttld/setup-ndk@v1
        with: { ndk-version: r27 }
      - uses: subosito/flutter-action@v2
        with: { channel: stable }
      - run: cargo install cargo-ndk --version 4.1.2 --locked
      - run: cd example && flutter build apk --debug   # exercises cargokit android path

  build-ios:
    if: startsWith(github.ref, 'refs/tags/') || github.event_name == 'workflow_dispatch'
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: aarch64-apple-ios,aarch64-apple-ios-sim }
      - uses: subosito/flutter-action@v2
        with: { channel: stable }
      - run: cd example && flutter build ios --debug --no-codesign  # exercises cargokit ios path

  publish:
    name: publish to pub.dev
    if: startsWith(github.ref, 'refs/tags/') || github.event_name == 'workflow_dispatch'
    needs: [test, build-android, build-ios]
    runs-on: ubuntu-latest
    environment: pub.dev
    permissions:
      id-token: write   # OIDC — pub.dev automated publishing, no token
    steps:
      - uses: actions/checkout@v4
      - uses: subosito/flutter-action@v2
        with: { channel: stable }
      - run: cargo install flutter_rust_bridge_codegen --version 2.12.0 --locked
      - run: flutter_rust_bridge_codegen generate --no-build-runner
      - run: dart pub get
      - uses: dart-lang/setup-dart@v1   # provides the OIDC publish action
      - run: dart pub publish --force
```

> Set up pub.dev "Automated publishing" on the package admin page first: bind
> the GitHub repo + a tag pattern `v{{version}}`, which authorizes the OIDC
> token. Idempotency note: unlike PyPI/npm, pub.dev **rejects re-publishing an
> existing version** outright — there is no `skip-existing`; ensure the
> `pubspec.yaml` version is bumped before tagging.

---

## 8 · pub.dev publishing — pubspec layout + build model

### 8.1 `bindings/flutter/pubspec.yaml`

```yaml
name: qrcode_ai_scanner
description: >-
  QR decoding + scannability scoring for artistic / AI-generated QR codes —
  the codes that break standard scanners. Native Rust core via flutter_rust_bridge.
version: 0.4.0
repository: https://github.com/supernovae-st/qrcode-ai-scanner
homepage: https://github.com/supernovae-st/qrcode-ai-scanner
topics: [qr, barcode, scanner, computer-vision, ffi]

environment:
  sdk: ">=3.3.0 <4.0.0"
  flutter: ">=3.16.0"

dependencies:
  flutter:
    sdk: flutter
  flutter_rust_bridge: 2.12.0

dev_dependencies:
  flutter_test:
    sdk: flutter
  flutter_lints: ^4.0.0

flutter:
  plugin:
    platforms:
      android:
        ffiPlugin: true
      ios:
        ffiPlugin: true
      # macos/linux/windows: add only if §9 desktop support is shipped
```

`pubspec.yaml` must also NOT exclude the Rust sources from the package (source
ships). Verify `dart pub publish --dry-run` lists `crates/.../src/*.rs`,
`cargokit/`, `rust_builder/`, and `Cargo.toml`. The package size will be larger
than a pure-Dart package — acceptable; it's source, not binaries.

### 8.2 Build model — DECISION: **build-at-app-time via cargokit** (recommend), NOT precompiled binaries

| Model | What ships | Pro | Con | Verdict |
|---|---|---|---|---|
| **cargokit build-at-app-time** (default) | Rust source + cargokit | no per-version binary matrix; works on any consumer arch; tiny pub.dev package; matches FRB's blessed path | consuming app needs Rust toolchain + (Android) NDK; first `flutter build` is slow (compiles the core: rxing+rqrr+image — minutes) | ✅ **recommend** |
| **Precompiled binaries** | prebuilt `.so`/`.xcframework` hosted on a release URL, cargokit downloads | app devs need no Rust; fast app builds | must build + host a binary matrix per release (lipo xcframework, per-ABI `.so`); larger/fragile; extra CI | ⏸️ defer to R2 if app-dev friction reported |

**Recommendation**: ship build-at-app-time for v0.4. Document the toolchain
prerequisite (Rust ≥1.87 + Android NDK) prominently in the README "Install"
section — this is the single biggest adoption friction (see RISK §11). Revisit
precompiled binaries (cargokit's
`BUILD_RUST=false` + `precompiled_binaries` URL + a `build-binaries` CI job that
lipos the iOS `.a` into an `.xcframework` and zips per-ABI `.so`s) only if
real-world app-dev friction is reported.

To wire the cargokit `manifest_dir` at the relocated Rust crate path, edit the
generated `rust_builder/` cmake/gradle and the `cargokit_options.yaml` (or the
podspec/build.gradle `apply_cargokit(... <crate_dir> ...)` call) so it points at
`../../crates/qrcode-ai-scanner-flutter` rather than the default `rust/`.

---

## 9 · Desktop platforms (optional scope)

The core is pure-native Rust and compiles for macOS/Linux/Windows trivially.
cargokit's `integrate` can add `--platforms=macos,linux,windows`. **Recommend
shipping Android+iOS for v0.4** (the mobile camera-scan use case is the point),
and add desktop in a follow-up if demand appears — desktop widens the CI matrix
(3 more runners) for marginal initial value. WASM/web is explicitly out of
scope here (the existing `qrcode-ai-scanner-wasm` npm package covers browser).

---

## 10 · Gotchas

1. **FRB v2 vs v1 are incompatible APIs.** v1 used a hand-written `build.rs`
   calling `lib_flutter_rust_bridge_codegen::frb_codegen` + `RawOpts` (you'll
   see this in old docs/examples — IGNORE it). v2 uses the
   `flutter_rust_bridge_codegen integrate/generate` CLI + a
   `flutter_rust_bridge.yaml` config + `#[frb(...)]` attribute macros. Use ONLY
   v2 patterns. Confirm with `flutter_rust_bridge_codegen --version` → 2.x.
2. **Version triple lock-step.** Dart pkg = Rust crate = codegen CLI = 2.12.0.
   Any drift → "codec version mismatch" panic at runtime. Pin all three; bump
   them together.
3. **`RustLib.init()` MUST be called once** before any `scan` call (the example
   does it in `main`). Forgetting it → "not initialized" runtime error.
4. **Don't edit generated files.** `frb_generated.*` (Dart + Rust) and
   `lib/src/api/scan.dart` are machine-output; regenerate, never patch.
5. **JSON over FFI is intentional** (§4) — do NOT "improve" it to native FRB
   structs; that re-introduces schema drift the py/node bindings already avoid.
6. **`#[non_exhaustive]` enums in the core** (`Symbology`, `ScanReport`, …) mean
   the Dart `fromJson` enum parsers MUST default-case unknown values, never
   throw — the schema mandates additive tolerance.
7. **Binary size**: the core links `rxing` + `rqrr` + `image` (png/jpeg/webp/gif)
   — expect the stripped native lib in the **single-digit MB** range per ABI.
   `[profile.release]` in root Cargo.toml already sets `lto="thin"`,
   `codegen-units=1`, `strip=true` — but that profile lives in the WORKSPACE
   root, and the flutter crate is EXCLUDED. **Add an equivalent
   `[profile.release]` to `crates/qrcode-ai-scanner-flutter/Cargo.toml`** (or a
   cargokit profile override) or the excluded crate builds without LTO/strip
   and bloats. Easy to miss.
8. **Build time**: first app build compiles the whole core per ABI (×3 Android
   + ×2 iOS) — several minutes cold. Cache `~/.cargo` + `target/` in CI
   (`Swatinem/rust-cache@v2`).
9. **NDK discovery**: cargokit/cargo-ndk need `ANDROID_NDK_HOME` (or
   `ANDROID_NDK`). On CI use `nttld/setup-ndk`. Locally, point at the
   Flutter-managed NDK.
10. **`armeabi-v7a` 16KB page size** (Android 15+): cargo-ndk 4.1.2 handles the
    `max-page-size` linker flag; ensure NDK r27+ for clean 16KB-aligned output.
11. **pub.dev has no version overwrite** (unlike npm/PyPI skip-existing) — never
    re-tag a published version; always bump.
12. **`crate-type=["cdylib","staticlib"]`**: building both unconditionally is
    fine; cargokit selects the right one per platform. Do not gate by `cfg` —
    let cargo emit both.

---

## 11 · RISK section

### 11.1 ⚠️ AGPL-3.0 copyleft — OPERATOR DECISION REQUIRED (do not resolve here)

The core and this binding are **AGPL-3.0-or-later**. A Flutter app that bundles
this package links the AGPL Rust core into the shipped binary. Under AGPL's
copyleft (and its network-use clause), **any app distributing or serving this
package may be obligated to release its own source under AGPL-compatible
terms.** This is materially stricter than MIT/Apache bindings app devs usually
expect from pub.dev, and is a likely adoption blocker for closed-source apps.

This is **flagged for the operator to decide**, NOT resolved in this plan.
Options the operator may weigh (no recommendation made here):
- ship as-is (AGPL) and document the obligation loudly on pub.dev;
- offer a **dual-license / commercial exception** for app bundlers;
- relicense the binding (only) under a more permissive license (requires the
  core to permit it — it does not today).
Engineer: do NOT pick one. Surface it; let the operator choose before the
first pub.dev publish.

### 11.2 Other risks

| Risk | Severity | Mitigation |
|---|---|---|
| FRB version drift (Dart/Rust/CLI) | High | pin `=2.12.0` everywhere; CI asserts `--version` |
| Consuming app needs Rust+NDK (build-at-app-time) | High (adoption) | document loudly; offer precompiled path (§8.2 R2) if friction reported |
| `report.dart` drifts from `spec/scan-report.schema.json` | Medium | golden round-trip test (§4); R2 auto-gen from SSOT |
| Excluded crate misses release profile (bloat) | Medium | add `[profile.release]` to the crate (§10.7) |
| FRB v2 breaking change in a future minor | Medium | pin exact version; review changelog before bumping |
| iOS static link / 16KB-page issues | Low-Med | NDK r27+, FRB 2.12 + cargokit current handle these |
| Large pub.dev package (ships source) | Low | acceptable; it's source not binaries; verify `--dry-run` |
| pub.dev no-overwrite footgun | Low | always bump version pre-tag |

---

## 12 · Acceptance checklist (definition of done)

- [ ] `crates/qrcode-ai-scanner-flutter/` builds: `cargo build --release` (host) + Android (3 ABI via cargo-ndk) + iOS (device+sim).
- [ ] Root `Cargo.toml` `exclude` updated; `cargo test --workspace` still green (flutter crate skipped).
- [ ] `flutter_rust_bridge_codegen generate` produces clean Dart, no diff drift.
- [ ] `bindings/flutter` `dart analyze` + `dart test` (incl. `ScanReport.fromJson` golden round-trip) pass.
- [ ] `example/` runs on an Android emulator + iOS sim, decodes a known QR fixture.
- [ ] Public API matches the canonical surface: `scan(bytes, profile:'full')`, `scanFrame(rgba, w, h, profile:'frame')`, profiles {full,fast,frame}, returns typed `ScanReport`.
- [ ] `.github/workflows/flutter.yml` test+build jobs green on PR.
- [ ] `dart pub publish --dry-run` clean (ships Rust source + cargokit).
- [ ] **AGPL §11.1 decision recorded by operator** before first publish.
- [ ] pub.dev automated-publishing (OIDC) configured; tag `v0.4.x` publishes.

---

### Sources (current docs, fetched 2026-06-17)

- flutter_rust_bridge — context7 `/fzyzcjy/flutter_rust_bridge` (v2 codegen, `#[frb]` attrs, cargokit, gradle/cargo-ndk tasks).
- [pub.dev/packages/flutter_rust_bridge](https://pub.dev/packages/flutter_rust_bridge) — latest stable **2.12.0** (2.13.0-beta.1 prerelease).
- [github.com/bbqsrc/cargo-ndk](https://github.com/bbqsrc/cargo-ndk) — **4.1.2** (2025-08-09, MSRV 1.86).
- [cjycode.com/flutter_rust_bridge/manual/integrate/cargokit](https://cjycode.com/flutter_rust_bridge/manual/integrate/cargokit) — build-at-app-time vs precompiled-binaries model.
- Repo: `crates/qrcode-ai-scanner/src/lib.rs`, `crates/qrcode-ai-scanner-py/src/lib.rs`, `crates/qrcode-ai-scanner-node/src/lib.rs`, `.github/workflows/python.yml`, `bindings/report-types.d.ts`, `spec/scan-report.schema.json`.
