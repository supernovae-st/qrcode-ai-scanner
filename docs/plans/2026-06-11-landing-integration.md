# Landing integration — replacing `qr-scanner-wechat` on Nicolas's `scanner` branch

> Analysis of `supernovae-studio/qrcode-ai_landing` @ `origin/scanner` (2026-06-11).
> Deep-scan verdict first, exact migration second. PR-only discipline: feature
> branch off `origin/scanner`, PR to `scanner`, never `main`.

## What the branch actually is

`origin/scanner` = 78 commits ahead of main, ~1035 files — **mostly SEO/locale
work** (sitemaps, term DB, generator ordering). The scanner part is a
placeholder: **no `@supernovae-st/qrcode-ai-scanner` anywhere**. Today's
scanning is **browser-side WASM** via `qr-scanner-wechat@0.1.3`, used in
exactly ONE place.

## The single integration point

`components/app/qrcode/Editor/Body.vue`:

- line 214: `import { scan } from "qr-scanner-wechat";`
- lines 1058-1138: `onVerify()` — called (debounced 500ms) after every
  generation (`onGenerateCustom` L650, `onGenerateImage` L678).
- The loop tries **3 hand-tuned filter levels** (HIGH contrast-400/bright-300 →
  MEDIUM 300/200 → LOW 100/100, plus grayscale/blur presets per QR type from
  `types/scan.ts` ScanConfig) and calls `scan(_canvas)` per level; first level
  whose `result.text` is truthy becomes `scanStatus` (`HIGH|MEDIUM|LOW`),
  else `NO`.
- Reads ONE field: `result.text`.

Consumers of the status: `Editor/Preview.vue` (quality meter UI) +
`useTracking.ts` (`landing_qrcode_scan_failed` event with
`scan_status: "no"|"low"|"medium"|"high"`).

Runtime: scanning is client-side only (no Nitro scanner route; server is Node
18 but irrelevant here). The editor chunk is already lazy-loaded — the wasm
joins that chunk's async path naturally.

## Why the filter-level loop dies

The 3-level loop exists to compensate for wechat-scanner's weak preprocessing
— the caller had to try contrast/brightness combos manually. Our ladder does
that internally (otsu · invert · contrast boost · channels · the 12 empirical
boost rungs), and the **score** is a real margin measure (6 stress axes +
structural caps + UEC), not a "which filter level worked" proxy. One call
replaces the whole loop, and the status mapping gets *more* honest.

## The migration (Body.vue)

**package.json**: drop `qr-scanner-wechat`; add
`@supernovae-st/qrcode-ai-scanner-wasm@^0.3.0`. (`uqr` stays — generation is
untouched.)

**Import** (the editor chunk is async — top-level await on init is fine, or
lazy-init on first verify):

```ts
import init, { scan_image } from "@supernovae-st/qrcode-ai-scanner-wasm";
const ready = init(); // vite resolves the .wasm URL via import.meta.url
```

**onVerify()** — the whole filter loop collapses to:

```ts
const onVerify = async () => {
  loading.value = true;
  try {
    await ready;
    // keep the existing canvas resize prep (250-300px per type) — then:
    const blob: Blob = await new Promise((r) => _canvas.toBlob((b) => r(b!), "image/png"));
    const bytes = new Uint8Array(await blob.arrayBuffer());
    const report = scan_image(bytes, "fast"); // scored profile, ~100-300ms budget

    if (!report.detections.length) {
      scanStatus.value = ScanStatus.NO;
    } else {
      const score = report.score?.value ?? 0;
      scanStatus.value =
        score >= 80 ? ScanStatus.HIGH :
        score >= 60 ? ScanStatus.MEDIUM :
        ScanStatus.LOW;
    }
    // optional next iteration: surface report.hints in the editor UX
    // ("raise error correction", "fix finder pattern", …)
  } finally {
    loading.value = false;
  }
};
```

Zero-PNG variant (faster, same result): `ctx.getImageData(...)` →
`scan_frame(new Uint8Array(img.data.buffer), w, h, "fast")`.

**Untouched**: `ScanStatus` enum, `Preview.vue` meter, tracking events,
debounce, generation pipeline. `types/scan.ts` `ScanConfig` filter presets
become dead code → delete with the loop.

**Threshold mapping** (aligns with the published grade bands — SCORING.md):
HIGH = score ≥80 (Excellent) · MEDIUM = 60-79 (Good/Acceptable) · LOW =
decoded <60 · NO = no detection. Tracking semantics preserved; the meter
becomes margin-truthful.

## Gains vs today

| | qr-scanner-wechat | ours |
|---|---|---|
| verify calls per generation | up to 3 (filter loop) | 1 |
| status meaning | "which filter helped" | real margin (axes + UEC) |
| artistic decode | CNN, opaque, no score | corpus-tuned ladder + 0-100 score |
| future UX | — | hints → "improve your QR" guidance |
| determinism | not guaranteed | bit-for-bit |

## Open items

1. Bundle size: wechat wasm ≈1 MB class; ours measured at publish time —
   if the dual-engine build crosses ~500 KB gz, ship the rqrr-only feature
   variant for the editor (decision on real numbers).
2. The `scanner` branch carries 78 commits of SEO work — the swap PR should
   target `scanner` with ONLY the scanner files (package.json, Body.vue,
   types/scan.ts) to keep review surface tiny.
3. Corpus dump: the editor generates art/custom/image QRs — a ~200-sample
   dump from Nicolas feeds the corpus + future calibration pass.
