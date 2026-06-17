# qrcode-ai-scanner (Python)

QR decoding + scannability scoring for **artistic / AI-generated / photo-captured** QR
codes — the Rust [`qrcode-ai-scanner`](https://crates.io/crates/qrcode-ai-scanner) engine,
via PyO3. Same versioned `ScanReport` contract as the Rust, Node and WASM surfaces.

```python
import qrcode_ai_scanner as qr

with open("image.png", "rb") as f:
    report = qr.scan(f.read(), profile="full")   # "full" | "fast" | "frame"

if report["detections"]:
    d = report["detections"][0]
    print(d["content"]["text"])
    print(report["score"]["value"], report["score"]["grade"])  # 0-100 + ISO band
    print(report["hints"])                                      # machine-actionable
```

- `scan(image: bytes, profile="full") -> dict` — decode encoded bytes (PNG/JPEG/WebP/GIF).
- `scan_frame(rgba: bytes, width: int, height: int, profile="frame") -> dict` — raw RGBA
  frame, no image-format roundtrip.
- "No QR found" returns a `dict` with empty `detections`; `ValueError` is raised only for
  invalid input.

The returned `dict` is the same `ScanReport` documented in the
[spec](https://github.com/supernovae-st/qrcode-ai-scanner/tree/main/spec).

License: AGPL-3.0-or-later · © SuperNovae Studio.
