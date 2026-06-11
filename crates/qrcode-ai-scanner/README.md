# qrcode-ai-scanner

QR decoding + scannability scoring for **artistic, AI-generated, and
photo-captured** QR codes. Deterministic multi-engine ladder (rxing + rqrr),
score contract v3 (six survival-ramp stress axes incl. perspective/rotation/
lighting, finder-integrity + quiet-zone caps), and the **synthetic UEC** —
the ISO 15415 unused-error-correction margin computed from the engine's own
sampled bitstream. Machine-actionable hints close the generate → scan →
regenerate loop.

```rust
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};

let scanner = Scanner::builder().profile(ScanProfile::Full).build();
let report = scanner.scan(ImageInput::encoded(&bytes))?;
// report.detections · report.score (value/grade/axes/uec) · report.hints
```

"No QR found" is `Ok` with empty detections — `Err` is reserved for invalid
input and cancellation (`QRS-001..005`). Camera frames skip the PNG roundtrip
via `ImageInput::rgba8`. Full docs: the
[repository](https://github.com/supernovae-st/qrcode-ai-scanner) —
`docs/ARCHITECTURE.md` + `docs/SCORING.md`.

License: AGPL-3.0-or-later · © SuperNovae Studio.
