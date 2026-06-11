# @supernovae-st/qrcode-ai-scanner-wasm

Browser WASM build (SIMD128) of the
[qrcode-ai-scanner](https://crates.io/crates/qrcode-ai-scanner) engine —
QR decoding + scannability scoring for artistic / AI-generated QR codes,
fully client-side, no server roundtrip.

```js
import init, { scan_image, scan_frame } from "@supernovae-st/qrcode-ai-scanner-wasm";

await init(); // loads the wasm module once

// canvas/upload verification (the generator quality-gate path)
const report = scan_image(pngBytes, "fast");
const ok = report.detections.length > 0;
const score = report.score?.value ?? 0;

// live camera (ImageData — no PNG roundtrip)
const frame = ctx.getImageData(0, 0, w, h);
const live = scan_frame(new Uint8Array(frame.data.buffer), w, h);
```

Same versioned `ScanReport` contract as the native package — detections,
0-100 score with per-axis survival, synthetic UEC margin, actionable hints.

License: AGPL-3.0-or-later · © SuperNovae Studio · part of
[qrcode-ai.com](https://qrcode-ai.com).
