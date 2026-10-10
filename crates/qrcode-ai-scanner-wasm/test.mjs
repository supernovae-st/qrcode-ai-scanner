// Smoke test for the BUILT pkg/ (run scripts/build-wasm.sh first; node ≥18).
// Exercises the full exported surface — scan_image, scan_frame, version and
// every positional argument — through the same JS the browser loads. Every
// assertion is exact or relational and none reads a clock: a judgment the
// wall-clock budget cuts is absent (score: null), so every scan that asserts
// a score runs unbudgeted (budget_ms 0).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { initSync, scan_image, scan_frame, version } from "./pkg/qrcode-ai-scanner.js";

initSync({ module: readFileSync(new URL("./pkg/qrcode-ai-scanner_bg.wasm", import.meta.url)) });

const here = (rel) => new URL(rel, import.meta.url);
const pkg = JSON.parse(readFileSync(here("./pkg/package.json"), "utf8"));
const markers = JSON.parse(readFileSync(here("../../spec/scan-report.schema.json"), "utf8")).$defs.Versions.properties;
const fixture = (rel) => new Uint8Array(readFileSync(here(`../../fixtures/${rel}`)));
const attempts = (report) => report.trace.stages.reduce((n, stage) => n + stage.transforms_tried, 0);
const texts = (report) => report.detections.map((d) => d.content.text);
const AXES = ["resolution", "blur", "contrast", "perspective", "rotation", "lighting"];

// one version end to end: the generated manifest, the module, the report
assert.equal(version(), pkg.version);

// scan_image: clean symbol → exact content + a complete unbudgeted judgment
const cleanBytes = fixture("clean/OK_68ms_100_4e875a2c.png");
const clean = scan_image(cleanBytes, "full", undefined, undefined, 0);
assert.deepEqual(clean.versions, {
  scanner: pkg.version,
  pipeline: markers.pipeline.const,
  score_contract: markers.score_contract.const,
});
assert.equal(clean.detections.length, 1);
const [det] = clean.detections;
assert.equal(det.symbology, "qr_code");
assert.equal(det.content.text, "https://qrc-ai.com/76xMa");
assert.equal(det.content.charset, "utf8");
assert.equal(det.payload.kind, "url");
assert.equal(det.payload.url, "https://qrc-ai.com/76xMa");
assert.equal(det.meta.inverted, false);
assert.equal(clean.score.weights_run, 100);
assert.deepEqual(clean.score.axes.map((a) => a.axis), AXES);
assert.ok(Number.isInteger(clean.score.value) && clean.score.value >= 70 && clean.score.value <= 100,
  `score ${clean.score.value}`);
assert.equal(clean.score.iso15415.overall, "a");

// the blob template class (an editor OUTPUT) decodes in full, not fast —
// the exact reason the landing integration uses the full profile
const monkey = fixture("artistic/blob-style-monkey-logo.webp");
assert.deepEqual(texts(scan_image(monkey, "full", undefined, undefined, 0)), ["https://qrc-ai.com/Uo54C"],
  "full must decode the blob class");
assert.deepEqual(texts(scan_image(monkey, "fast", undefined, undefined, 0)), [], "fast skips deep (documented)");

// budget_ms is positional arg #5: a 1 ms budget stops the Full ladder after
// fewer attempts than the unbounded run of the same never-decoding image.
// The attempt counts come from the trace; no clock is read.
const degraded = fixture("degraded/FAIL_1491ms_0_584c998c.png");
const cut = scan_image(degraded, "full", undefined, undefined, 1);
const whole = scan_image(degraded, "full", undefined, undefined, 0);
assert.deepEqual(texts(cut), []);
assert.deepEqual(texts(whole), [], "the negative sample never decodes");
assert.ok(attempts(cut) < attempts(whole), `1 ms: ${attempts(cut)} attempts · unbounded: ${attempts(whole)}`);
// 0 = UNBOUNDED, not a 0 ms budget (that would return empty before any attempt)
const v2l = fixture("clean/gen_v2_l.png");
assert.deepEqual(texts(scan_image(v2l, "fast", undefined, undefined, 0)), ["qrc.ai/v2l"], "budget 0 means unbounded");

// score_skip_axes (#6) + score_skip_checks (#7): skipped axes absent, skipped
// sections null, and a skipped check never moves the composite (spec/04)
const v2lFull = scan_image(v2l, "full", undefined, undefined, 0);
const skipped = scan_image(v2l, "full", undefined, undefined, 0, ["perspective", "rotation"]);
assert.deepEqual(skipped.score.axes.map((a) => a.axis), ["resolution", "blur", "contrast", "lighting"]);
assert.equal(skipped.score.weights_run, 70);
const noChecks = scan_image(v2l, "full", undefined, undefined, 0, undefined, ["uec", "iso15415"]);
assert.equal(noChecks.score.uec, null, "skipped uec must be null");
assert.equal(noChecks.score.iso15415, null, "skipped iso15415 must be null");
assert.equal(noChecks.score.value, v2lFull.score.value);
assert.deepEqual(noChecks.score.axes, v2lFull.score.axes);
assert.throws(
  () => scan_image(v2l, "full", undefined, undefined, undefined, undefined, ["margin"]),
  /unknown score check/,
  "typo'd check must throw loudly",
);

// scan_frame: raw RGBA path (browser ImageData shape) — default frame profile
const side = 64;
const rgba = new Uint8Array(side * side * 4).fill(255); // white field: valid, nothing to find
const frame = scan_frame(rgba, side, side);
assert.deepEqual(texts(frame), []);
assert.equal(frame.score, null, "frame profile skips scoring");
// explicit profile + budget through scan_frame (#4 · #5)
const framed = scan_frame(rgba, side, side, "fast", 0);
assert.deepEqual(texts(framed), []);
assert.equal(framed.score, null, "no detection, no judgment");

// alpha_background (#8): a transparent input carries the alpha block through
// the serde_wasm_bindgen boundary (the skip_serializing_if key must be ABSENT
// on the JS object, not nulled — the serializer's serialize_missing_as_null
// must not resurrect it on opaque reports)
const transparent = new Uint8Array(readFileSync(here("../../playground/public/samples/transparent.png")));
const flat = scan_image(transparent, "full", undefined, undefined, 0);
assert.deepEqual(texts(flat), ["https://qrc-ai.com/76xMa"], "the flatten rescues the canvas-export class");
assert.equal(flat.alpha.mode, "auto");
assert.equal(flat.alpha.background, "white");
assert.equal(flat.alpha.envelope.placement, "light_only");
assert.deepEqual(flat.alpha.envelope.safe_luma, [[32, 255]]);
assert.deepEqual(
  flat.hints.filter((h) => h.hint === "alpha_background_dependent" || h.hint === "add_background_plate"),
  [{ hint: "alpha_background_dependent", placement: "light_only" }, { hint: "add_background_plate", color: "white" }],
  "the narrow envelope drives the hint and its remedy",
);
assert.ok(!("alpha" in clean), "opaque reports omit the alpha key entirely");
const opaqueForced = scan_image(v2l, "full", undefined, undefined, 0, undefined, undefined, "white");
assert.deepEqual(texts(opaqueForced), ["qrc.ai/v2l"]);
assert.ok(!("alpha" in opaqueForced), "forced mode on an opaque input still omits the key");
const dropped = scan_image(transparent, "full", undefined, undefined, 0, undefined, undefined, "none");
assert.deepEqual(texts(dropped), [], "none = the pre-0.9 exporter-dependent path");
assert.throws(
  () => scan_image(v2l, "full", undefined, undefined, undefined, undefined, undefined, "transparent"),
  /unknown alpha background/,
  "typo'd alpha background must throw loudly",
);
// alpha_palette (#9): per-color verdicts inside one scan, request order
const themed = scan_image(transparent, "full", undefined, undefined, 0, undefined, undefined, undefined, ["#f3f4f6", "black"]);
assert.deepEqual(
  themed.alpha.envelope.palette.map(({ background, background_luma, decoded }) => ({ background, background_luma, decoded })),
  [
    { background: "#f3f4f6", background_luma: 244, decoded: true },
    { background: "black", background_luma: 0, decoded: false },
  ],
);
assert.throws(
  () => scan_image(v2l, "full", undefined, undefined, undefined, undefined, undefined, undefined, ["auto"]),
  /unknown palette color/,
  "modes are not colors — throw loudly",
);

// score_preset (#10): design is sugar over skipping perspective + rotation
const designed = scan_image(v2l, "full", undefined, undefined, 0, undefined, undefined, undefined, undefined, "design");
assert.deepEqual(designed.score, skipped.score);
assert.throws(
  () => scan_image(v2l, "full", undefined, undefined, 0, ["blur"], undefined, undefined, undefined, "design"),
  /mutually exclusive/,
  "preset + explicit list must throw loudly",
);

// invalid bytes → typed throw, not a crash
assert.throws(() => scan_image(new Uint8Array([1, 2, 3])), /QRS-001/);

console.log(`wasm pkg OK — ${version()}`);
