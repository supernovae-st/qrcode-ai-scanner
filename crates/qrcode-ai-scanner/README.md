# qrcode-ai-scanner

QR decoding + scannability scoring for **artistic, AI-generated, and
photo-captured** QR codes. Deterministic multi-engine ladder (rxing + rqrr),
score contract v5 (six survival-ramp stress axes incl. perspective/rotation/
lighting, finder-integrity + quiet-zone caps), and the **synthetic UEC** —
the ISO 15415 unused-error-correction margin computed from the verified
stream the decoder read. Exact content: `content.raw` is the payload's
original bytes, `charset` says how `text` reads them (`unknown`, the only
lossy value, means use `raw`), and detections are distinct contents, never
keyed on text. Machine-actionable hints close the generate → scan →
regenerate loop.

```rust
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};

let scanner = Scanner::builder().profile(ScanProfile::Full).build();
let report = scanner.scan(ImageInput::encoded(&bytes))?;
// report.detections · report.score (value/grade/axes/uec) · report.hints
```

"No QR found" is `Ok` with empty detections — `Err` is reserved for invalid
input and cancellation (`QRS-001..005`). A report is final when
`report.trace.budget_exhausted == Some(false)`; set `budget_ms: None` for the
unbounded, reproducible scan. Camera frames skip the PNG roundtrip
via `ImageInput::rgba8`. Full docs: the
[repository](https://github.com/supernovae-st/qrcode-ai-scanner) —
`docs/ARCHITECTURE.md` + `docs/SCORING.md`.

License: AGPL-3.0-or-later · © SuperNovae Studio.
