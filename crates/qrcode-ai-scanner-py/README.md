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
    score = report["score"]                 # None = not judged, never 0
    if score is not None:
        print(score["value"], score["grade"])  # 0-100 + ISO band
    elif report["trace"]["budget_exhausted"]:
        print("not judged within the budget: scan again with budget_ms=0")
    print(report["hints"])                     # machine-actionable
```

- `scan(image: bytes, profile="full", max_dimension=None, max_pixels=None, budget_ms=None)
  -> dict` — decode encoded bytes (PNG/JPEG/WebP/GIF).
- `scan_frame(rgba: bytes, width: int, height: int, profile="frame", max_dimension=None,
  max_pixels=None, budget_ms=None) -> dict` — raw RGBA frame, no image-format roundtrip.
- Optional kwargs: `max_dimension` / `max_pixels` cap inputs BEFORE any pixel work
  (defaults 10000 px / 64 MP — lower them for untrusted uploads); `budget_ms` bounds the
  scan wall-clock once the image is decoded, the caps bound the decode (unset = the
  profile default: full 4000 · fast 800 · frame 80 ms; `0` = unbounded, the strictly
  reproducible mode). A report the budget cut says so:
  `report["trace"]["budget_exhausted"]` is `True`, and a `None` score beside detections
  then means "not judged".
- "No QR found" returns a `dict` with empty `detections`; `ValueError` is raised only for
  invalid input.
- `content["raw"]` is the payload's bytes (base64; QR family: the original bytes, other
  symbologies: the decoding engine's reading); `content["text"]` reads them as
  `content["charset"]` says. Test `charset == "unknown"` — the only lossy value: no
  verified reading, use `raw`. The optional `eci` and `fnc1` keys carry the ECI
  designators and the FNC1 position; compare contents by `symbology`, `raw`, `fnc1` and
  `eci`, never by `text`.

The returned `dict` is the same `ScanReport` documented in the
[spec](https://github.com/supernovae-st/qrcode-ai-scanner/tree/main/spec).

License: AGPL-3.0-or-later · © SuperNovae Studio.
