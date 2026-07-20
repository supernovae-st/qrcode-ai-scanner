// Smoke test for the BUILT pkg/ (run scripts/build-wasm.sh first; node ≥18).
// Exercises the full exported surface — scan_image, scan_frame, version,
// budget override — through the same JS the browser loads.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { initSync, scan_image, scan_frame, version } from "./pkg/qrcode-ai-scanner.js";

initSync({ module: readFileSync(new URL("./pkg/qrcode-ai-scanner_bg.wasm", import.meta.url)) });
assert.match(version(), /^\d+\.\d+\.\d+/);

const fixture = (rel) => readFileSync(new URL(`../../fixtures/${rel}`, import.meta.url));

// scan_image: clean symbol → decode + score + grade card + measured polarity
const clean = scan_image(new Uint8Array(fixture("clean/OK_68ms_100_4e875a2c.png")), "full");
assert.equal(clean.detections.length, 1);
assert.equal(clean.detections[0].payload.kind, "url");
assert.equal(clean.detections[0].symbology, "qr_code");
assert.ok(clean.score.value >= 70, `score ${clean.score.value}`);
assert.equal(clean.score.iso15415.overall, "a");
assert.equal(clean.detections[0].meta.inverted, false);

// the blob template class (an editor OUTPUT) decodes in full, not fast —
// the exact reason the landing integration uses the full profile
const monkey = new Uint8Array(fixture("artistic/blob-style-monkey-logo.webp"));
assert.equal(scan_image(monkey, "full").detections.length, 1, "full must decode the blob class");
assert.equal(scan_image(monkey, "fast").detections.length, 0, "fast skips deep (documented)");

// budget_ms positional contract: arg #5 on scan_image
const degraded = new Uint8Array(fixture("degraded/FAIL_1491ms_0_584c998c.png"));
const t0 = performance.now();
const cut = scan_image(degraded, "full", undefined, undefined, 1);
assert.equal(cut.detections.length, 0);
assert.ok(performance.now() - t0 < 1500, "1ms budget must cut early");
// 0 = UNBOUNDED, not a 0ms budget — a clean symbol must still decode
assert.equal(
  scan_image(new Uint8Array(fixture("clean/gen_v2_l.png")), "fast", undefined, undefined, 0)
    .detections.length,
  1,
  "budget 0 means unbounded",
);

// score_skip_axes (#6) + score_skip_checks (#7) positional contract: skipped
// axes absent, skipped sections null, composite still scores, typos throw
const cleanBytes = new Uint8Array(fixture("clean/gen_v2_l.png"));
const skipped = scan_image(cleanBytes, "full", undefined, undefined, 0, ["perspective", "rotation"], ["uec", "iso15415"]);
assert.equal(skipped.score.axes.length, 4, "six axes minus the two skipped");
assert.equal(skipped.score.uec, null, "skipped uec must be null");
assert.equal(skipped.score.iso15415, null, "skipped iso15415 must be null");
assert.ok(Number.isInteger(skipped.score.value), "composite still scores");
assert.throws(
  () => scan_image(cleanBytes, "full", undefined, undefined, undefined, undefined, ["margin"]),
  /unknown score check/,
  "typo'd check must throw loudly",
);

// scan_frame: raw RGBA path (browser ImageData shape) — default frame profile
const { side, rgba } = (() => {
  // decode the clean PNG via scan_image's own engine? No — build a trivial
  // synthetic frame instead: white field = no detection, exercises the path.
  const s = 64;
  return { side: s, rgba: new Uint8Array(s * s * 4).fill(255) };
})();
const frame = scan_frame(rgba, side, side);
assert.equal(frame.detections.length, 0);
assert.equal(frame.score, null, "frame profile skips scoring");
// explicit profile + budget through scan_frame
const framed = scan_frame(rgba, side, side, "fast", 200);
assert.equal(framed.detections.length, 0);

// alpha_background (#8) positional contract: a transparent input carries the
// alpha block through the serde_wasm_bindgen boundary (the skip_serializing_if
// key must be ABSENT on the JS object, not nulled — the serializer's
// serialize_missing_as_null(true) must not resurrect it on opaque reports)
const transparent = new Uint8Array(
  readFileSync(new URL("../../playground/public/samples/transparent.png", import.meta.url)),
);
const flat = scan_image(transparent, "full", undefined, undefined, 0);
assert.equal(flat.detections.length, 1, "the flatten rescues the canvas-export class");
assert.equal(flat.alpha.background, "white");
assert.equal(flat.alpha.envelope.placement, "light_only");
assert.ok(
  flat.hints.some((h) => h.hint === "alpha_background_dependent"),
  "narrow envelope drives the hint",
);
assert.ok(!("alpha" in clean), "opaque reports omit the alpha key entirely");
const opaqueForced = scan_image(cleanBytes, "full", undefined, undefined, 0, undefined, undefined, "white");
assert.ok(!("alpha" in opaqueForced), "forced mode on an opaque input still omits the key");
const dropped = scan_image(transparent, "full", undefined, undefined, 0, undefined, undefined, "none");
assert.equal(dropped.detections.length, 0, "none = the pre-0.9 exporter-dependent path");
assert.throws(
  () => scan_image(cleanBytes, "full", undefined, undefined, undefined, undefined, undefined, "transparent"),
  /unknown alpha background/,
  "typo'd alpha background must throw loudly",
);
// alpha_palette (#9): per-color verdicts inside one scan
const themed = scan_image(transparent, "full", undefined, undefined, 0, undefined, undefined, undefined, ["#f3f4f6", "black"]);
assert.equal(themed.alpha.envelope.palette.length, 2);
assert.equal(themed.alpha.envelope.palette[0].decoded, true);
assert.equal(themed.alpha.envelope.palette[1].decoded, false);
assert.throws(
  () => scan_image(cleanBytes, "full", undefined, undefined, undefined, undefined, undefined, undefined, ["auto"]),
  /unknown palette color/,
  "modes are not colors — throw loudly",
);

// invalid bytes → typed throw, not a crash
assert.throws(() => scan_image(new Uint8Array([1, 2, 3])), /QRS-001/);

console.log(`wasm pkg OK — ${version()}`);
