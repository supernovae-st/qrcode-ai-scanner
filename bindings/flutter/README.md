# qrcode_ai_scanner — Flutter / Dart

QR decoding **+ scannability scoring** for artistic / AI-generated QR codes —
the codes that break standard scanners. The native Rust core of
[`qrcode-ai-scanner`](https://github.com/supernovae-st/qrcode-ai-scanner),
bound to Flutter via [flutter_rust_bridge](https://pub.dev/packages/flutter_rust_bridge).
Returns the same versioned `ScanReport` as the Python / Node / WASM / Kotlin /
Swift bindings (one serde wire contract).

## Install

```yaml
dependencies:
  qrcode_ai_scanner: ^0.3.0
```

> **Toolchain prerequisite.** This package compiles the Rust core at *your* app's
> build time (via cargokit) — the building machine needs the **Rust toolchain
> (≥1.87)** and, for Android, the **NDK (r27+)**. No Rust knowledge required;
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

## License

Dual-licensed **AGPL-3.0-or-later OR commercial** — see the repo-root
[`LICENSING.md`](https://github.com/supernovae-st/qrcode-ai-scanner/blob/main/LICENSING.md).
A closed-source app on the App Store / Play Store links the AGPL Rust core and
therefore needs the **commercial** license: `studio.supernovae@gmail.com`.
