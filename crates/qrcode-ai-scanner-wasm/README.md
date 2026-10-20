# @supernovae-st/qrcode-ai-scanner-wasm

Browser WASM builds (SIMD128 and compatible) of the
[qrcode-ai-scanner](https://crates.io/crates/qrcode-ai-scanner) engine —
QR decoding + scannability scoring for artistic / AI-generated QR codes,
fully client-side, no server roundtrip.

## Automatic loading, including module workers

```js
import { loadScanner } from "@supernovae-st/qrcode-ai-scanner-wasm/auto";

const scanner = await loadScanner();
const report = scanner.scan_image(pngBytes, "full");
const loadedVariant = scanner.variant; // "simd" | "compatible"
const scannerVersion = scanner.version();
```

Run this same code inside a module Worker for UI responsiveness. Capability
detection runs in that worker's realm before the scanner is compiled. It uses
small real WASM validation probes, never user-agent, browser-version or CPU
guesses. SIMD unavailable to this runtime selects the compatible binary. This
does not identify whether SIMD was disabled by policy, the runtime, or its host.

Vite resolves both literal asset URLs at build time, but loading fetches only
the selected binary. No remote service is involved. Importing `/auto` is safe
during SSR; calling it requires browser/worker WebAssembly and fetch support.
The existing root import below remains the explicit accelerated API, including
its original default initialization and `initSync` behavior. Automatic selection
is additive: both worker and main-thread fallback consumers must adopt `/auto`.

Concurrent or repeated `loadScanner()` calls share one promise and instance.
An initialization rejection also stays cached. There are no automatic retries,
reloads, or blind fallbacks after a network/compiler error. For an explicit
user-initiated retry, create one fresh loader:

```js
import { createScannerLoader } from "@supernovae-st/qrcode-ai-scanner-wasm/auto";
const retry = createScannerLoader();
const scanner = await retry();
```

The factory also accepts `{variant: "compatible"}` for controlled qualification
or a deliberate host choice, and an optional `wasmUrl` for an asset pipeline
that cannot process package URLs. Keep that override on the same exact package
version and variant. Standard Vite/module-worker use needs no URL override.

`ScannerInitError` carries a stable `code`, selected `variant`, optional HTTP
`status`, and original `cause`. Codes distinguish `wasm_unavailable`,
`wasm_features_unsupported`, `simd_unavailable` (an explicitly requested SIMD
variant), `loader_import_failed`, `asset_network_failed`, `asset_http_failed`,
`asset_read_failed`, `binary_invalid`, `wasm_validation_failed`, `wasm_compile_failed`,
`wasm_instantiate_failed` and `invalid_options`. Preserve the native cause for
diagnosis; do not label every compile failure as SIMD incompatibility. These
are JS loading diagnostics, separate from the unchanged Rust/report/QRS error
contract. Serialize stable fields explicitly when crossing a worker boundary.
`binary_invalid` means the bytes lack a valid WASM magic/version header.
`wasm_validation_failed` retains ambiguity between malformed code and unexpected
requirements in a wrong/stale asset. Inspect its native cause and the actual
failing bytes rather than guessing SIMD or CPU incompatibility.

The compatible build is **not an MVP-only WASM binary**. Automatic loading uses
a conservative supported baseline: mutable globals, bulk memory, sign extension,
nontrapping float-to-int, multivalue and reference types. The probe tests these
together; it is not a measurement of the optimized binary's minimum requirements:

```wat
(module
  (memory 1) (table 1 funcref)
  (global (export "g") (mut i32) (i32.const 0))
  (func (result i32 i32)
    i32.const 0 i32.extend8_s drop
    f64.const 0 i32.trunc_sat_f64_s drop
    i32.const 0 i32.const 0 i32.const 0 memory.copy
    ref.null func drop
    i32.const 0 i32.const 0))
```

Builds use the same Cargo features, lockfile and report types. The owning build
script restricts optimization to that feature profile, adding SIMD only for the
accelerated variant, then validates the compatible artifact with SIMD forbidden.
Inspect each actual artifact before attributing a failure to a feature. For the
qualified 0.10.1 builds, individually omitting features from Binaryen validation
identifies bulk memory, sign extension and nontrapping float-to-int as required;
the accelerated binary additionally requires SIMD. Mutable globals, multivalue
and reference types can each be omitted from that validation profile without
invalidating these exact binaries. The broader loader baseline remains a declared
support policy, not evidence that an unknown failing asset uses all six features.
Compatibility claims require actual browser evidence; this path alone does not
qualify historical browser floors, real Safari, mobile devices, arbitrary CSP
rules, or a consumer's deployed binary.

## Existing accelerated entry

```js
import init, { scan_image, scan_frame } from "@supernovae-st/qrcode-ai-scanner-wasm";

await init(); // loads the wasm module once

// canvas/upload verification (the generator quality-gate path)
const report = scan_image(pngBytes, "fast");
const ok = report.detections.length > 0;
// null = no judgment, never a zero: with trace.budget_exhausted the budget
// cut it (argument #5, budget_ms: 0 = unbounded, scans without a cut; the
// clock starts once the image is decoded)
const score = report.score?.value ?? null;
const judged = report.score !== null || !report.trace.budget_exhausted;

// live camera (ImageData — no PNG roundtrip)
const frame = ctx.getImageData(0, 0, w, h);
const live = scan_frame(new Uint8Array(frame.data.buffer), w, h);
```

Same versioned `ScanReport` contract as the native package — detections,
0-100 score with per-axis survival, synthetic UEC margin, actionable hints.

License: AGPL-3.0-or-later · © SuperNovae Studio · part of
[qrcode-ai.com](https://qrcode-ai.com).
