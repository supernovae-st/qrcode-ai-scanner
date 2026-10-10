# @supernovae-st/qrcode-ai-scanner

Async native QR decoding + scannability scoring for **artistic / AI-generated
QR codes** — Rust under the hood, libuv pool execution (the event loop never
blocks, even on a 4s artistic scan).

```js
import { scan } from "@supernovae-st/qrcode-ai-scanner";

const report = await scan(imageBuffer); // profile: full (quality gate)
if (report.detections.length) {
  console.log(report.detections[0].content.text);
  if (report.score) {
    console.log(report.score.value, report.score.grade); // 0-100 + band
    console.log(report.score.uec); // real error-correction margin (ISO 15415)
  } else if (report.trace.budget_exhausted) {
    // not judged within the budget — never a zero: scan again with budgetMs: 0
  }
  console.log(report.hints); // machine-actionable: raise_error_correction…
}
```

- `scan(image, { profile, signal, budgetMs })` — async, `full | fast | frame`,
  AbortSignal cancels queued scans. `budgetMs` unset keeps the profile default
  (full 4000 · fast 800 · frame 80 ms); `0` is unbounded, the reproducible mode.
  A report the budget cut says so: `trace.budget_exhausted === true`.
- `scanSync(image, { profile })` — blocking, scripts only.
- "No QR found" **resolves** with empty `detections`; rejections carry
  `[QRS-xxx]` codes (invalid input only).
- Full TypeScript contract in `index.d.ts` — the same versioned shape as the
  [core crate](https://crates.io/crates/qrcode-ai-scanner), CLI, and wasm package.

Browser? Use
[`@supernovae-st/qrcode-ai-scanner-wasm`](https://www.npmjs.com/package/@supernovae-st/qrcode-ai-scanner-wasm).

License: AGPL-3.0-or-later · © SuperNovae Studio · part of
[qrcode-ai.com](https://qrcode-ai.com).
