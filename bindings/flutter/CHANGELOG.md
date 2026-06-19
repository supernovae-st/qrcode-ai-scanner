## 0.3.0

* Initial Flutter/Dart binding (flutter_rust_bridge v2) for `qrcode-ai-scanner`.
* `QrcodeAiScanner.scan(bytes, profile:)` + `scanFrame(rgba, w, h, profile:)` —
  the same canonical surface as the Python / Node / WASM / Kotlin / Swift bindings.
* Fully typed `ScanReport` (typed `Payload`/`Hint` sealed classes, ISO 15415 /
  UEC grade cards) that tolerates unknown enum values and payload kinds.
* Android (cargokit/NDK) + iOS (cargokit/static lib) build-at-app-time.
