/// QR Code AI Scanner — QR decoding + scannability scoring for artistic /
/// AI-generated QR codes (the ones that break standard scanners). Native Rust
/// core via flutter_rust_bridge.
///
/// ```dart
/// import 'package:qrcode_ai_scanner/qrcode_ai_scanner.dart';
///
/// await QrcodeAiScanner.init();                 // once, before any scan
/// final report = await QrcodeAiScanner.scan(pngBytes);
/// final text = report.detections.isEmpty
///     ? 'no QR'
///     : report.detections.first.content.text;   // typed, no JSON wrangling
/// ```
library;

import 'dart:typed_data';

import 'report.dart';
import 'src/rust/api/scan.dart' as ffi;
import 'src/rust/frb_generated.dart';

export 'report.dart';
export 'src/rust/frb_generated.dart' show RustLib;

/// The QR scanner. One Rust core, the same versioned [ScanReport] as the
/// Python / Node / WASM / Kotlin / Swift bindings (one serde wire contract).
///
/// Scans run on flutter_rust_bridge's Rust worker pool, so `await` never blocks
/// the UI isolate. Valid `profile`s are `'full'` (default for [scan]), `'fast'`,
/// and `'frame'` (default for [scanFrame]). "No QR found" is a normal result
/// (empty [ScanReport.detections]), not an error; errors carry the `[QRS-xxx]`
/// wire code in the thrown message.
class QrcodeAiScanner {
  QrcodeAiScanner._();

  /// Initialise the native bridge. Call once at app start before any scan.
  static Future<void> init() => RustLib.init();

  /// Decode + score an encoded image (PNG · JPEG · WebP · GIF).
  static Future<ScanReport> scan(
    Uint8List imageBytes, {
    String profile = 'full',
  }) async {
    final json = await ffi.scan(image: imageBytes, profile: profile);
    return ScanReport.parse(json);
  }

  /// Decode + score a raw RGBA camera frame. [rgba] must be `width * height * 4`
  /// bytes (a browser `ImageData`-shaped buffer).
  static Future<ScanReport> scanFrame(
    Uint8List rgba,
    int width,
    int height, {
    String profile = 'frame',
  }) async {
    final json = await ffi.scanFrame(
      rgba: rgba,
      width: width,
      height: height,
      profile: profile,
    );
    return ScanReport.parse(json);
  }
}
