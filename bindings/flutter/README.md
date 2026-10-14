# qrcode_ai_scanner — Flutter / Dart

QR decoding **+ scannability scoring** for artistic / AI-generated QR codes —
the codes that break standard scanners. The native Rust core of
[`qrcode-ai-scanner`](https://github.com/supernovae-st/qrcode-ai-scanner),
bound to Flutter via [flutter_rust_bridge](https://pub.dev/packages/flutter_rust_bridge).
Returns the same versioned `ScanReport` as the Python / Node / WASM / Kotlin /
Swift bindings (one serde wire contract).

## Install

```bash
flutter pub add qrcode_ai_scanner
```

> **Toolchain prerequisite.** This package compiles the Rust core at *your* app's
> build time (via cargokit) — the building machine needs the **Rust toolchain
> (≥1.88)** and, for Android, the **NDK (r27+)**. No Rust knowledge required;
> it's a one-time `rustup` install. (A precompiled-binary distribution is a
> future option if this friction is reported.)

## Usage

```dart
import 'package:qrcode_ai_scanner/qrcode_ai_scanner.dart';

await QrcodeAiScanner.init();                       // once, before any scan

// Decode + score an encoded image (PNG/JPEG/WebP/GIF):
final report = await QrcodeAiScanner.scan(pngBytes); // profile: 'full' (default)
if (report.detections.isEmpty) {
  print('no QR found');                              // a result, not an error
} else {
  final d = report.detections.first;
  print('${d.content.text}  •  grade ${report.score?.grade.name}');
}

// Decode a raw RGBA camera frame (width*height*4 bytes), no scoring:
final frame = await QrcodeAiScanner.scanFrame(rgba, w, h); // profile: 'frame'
```

Scans run on flutter_rust_bridge's Rust worker pool, so `await` never blocks the
UI isolate. Profiles: `'full'` (full ladder + score), `'fast'` (reduced),
`'frame'` (per-frame camera decode, no scoring). Errors throw with the
`[QRS-xxx]` wire code in the message.

The return type [`ScanReport`](lib/report.dart) is fully typed (detections, a
typed `Payload` sealed class, `Score` with ISO 15415 / UEC grade cards, hints,
trace, versions). Unknown enum values and payload kinds decode to `unknown` /
catch-all variants — never a throw (the contract is additively forward-compatible).
`Charset` and `Fnc1Position` are the exceptions: a newer wire value decodes to
`unrecognized`. `Charset.unrecognized` is a complete reading, while
`Charset.unknown` is the wire's lossy value — test `content.charset.isLossy`
and then use `content.raw`, the payload's bytes.
`content.eci` and `content.fnc1` are null when none was observed or the
symbology does not report it — not proof that the symbol declares none.

## Packaging from a candidate checkout

The development binding uses the workspace core by relative path. A pub.dev
package instead bundles the **same candidate's sources**, from Cargo's normalized
package archive, under `rust/vendor/qrcode-ai-scanner`. No published registry
version is substituted. From a clean, committed checkout, with Python 3.11+:

```bash
set -o pipefail
python3 scripts/stage-flutter-package.py --output /tmp/scanner-flutter-validation/bindings/flutter \
  --archive /tmp/scanner-flutter.tar.gz --receipt /tmp/scanner-flutter.json
git archive HEAD spec/examples | tar -x -C /tmp/scanner-flutter-validation
cd /tmp/scanner-flutter-validation/bindings/flutter
cargo check --locked --manifest-path rust/Cargo.toml --lib
flutter pub get
flutter analyze
test -f ../../spec/examples/clean-url.json
flutter test
```

Run the inspector from the candidate checkout, pointing it at that same root:

```bash
python3 scripts/package-inspect.py flutter --package-root /tmp/scanner-flutter-validation/bindings/flutter \
  --strict --release-set --record /tmp/scanner-flutter-inspected.sha256
```

The output directory must not exist. The staging command runs
`cargo package --locked --no-verify` (packing only); `--core-archive FILE`
accepts an already packed candidate and `--cargo-wrapper PATH` supports isolated
Cargo runners. It verifies the revision, source bytes, versions and LICENSE,
rejects archive links and escaping paths, and preserves the binding lockfile.
Only the staged binding manifest's core path and bundle metadata change.
The metadata includes a deterministic core content digest: cargokit hashes the
binding manifest but does not recursively hash path dependency sources, so a
core-only change must update that manifest. A staged `.pubignore` preserves the
core's Cargo provenance file in pub's published file set.

CI creates one staged archive, then uses it outside the checkout for the host
checks, Android/iOS example builds and the guarded publishing job. Host contract
tests receive the candidate's canonical JSON goldens outside the package, at
their expected `../../spec/examples` path; no adjacent Rust checkout is copied.
Without these fixtures, seven golden tests skip rather than qualify the contract.
The fixtures do not change the staged archive or pub's file set. The inspector
requires pub's parsed file tree and completed validation, checks the bundled
sources recursively, and records the published files' hashes for a final
same-byte check. Validation warnings remain visible; validation errors and an
unparsed listing fail. A missing Flutter/Dart SDK is **UNAVAILABLE**, not a pass.
Portable source inspection alone does not establish pub.dev readiness: the
real validator and native consumer builds must also pass, and the first upload
and trusted-publisher setup remain operator steps in `RELEASING.md`.

## License

Dual-licensed **AGPL-3.0-or-later OR commercial** — see the repo-root
[`LICENSING.md`](https://github.com/supernovae-st/qrcode-ai-scanner/blob/main/LICENSING.md).
A closed-source app on the App Store / Play Store links the AGPL Rust core and
therefore needs the **commercial** license: `studio.supernovae@gmail.com`.
