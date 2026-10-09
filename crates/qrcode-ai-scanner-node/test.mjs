// Smoke for the BUILT binding (`pnpm build` first). Every assertion is exact
// or relational, and none reads a clock: a judgment the wall-clock budget cuts
// is absent (score: null), so every scan that asserts a score runs unbudgeted
// (budgetMs 0) — under a profile budget, machine load alone would turn the
// score null and this smoke red. The contract markers come from the JSON
// Schema, which every contract change updates (the AGENTS.md cascade).
import { scan, scanSync, version } from "./index.js";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";

const here = (rel) => new URL(rel, import.meta.url);
const pkg = JSON.parse(readFileSync(here("./package.json"), "utf8"));
const markers = JSON.parse(readFileSync(here("../../spec/scan-report.schema.json"), "utf8")).$defs.Versions.properties;
const fixture = (rel) => readFileSync(here(`../../fixtures/${rel}`));
const attempts = (report) => report.trace.stages.reduce((n, stage) => n + stage.transforms_tried, 0);
const untimed = ({ trace, ...rest }) => ({
  ...rest,
  trace: { ...trace, total_ms: 0, stages: trace.stages.map((stage) => ({ ...stage, ms: 0 })) },
});
const V5Q = "https://qrcode-ai.com/c/v5q";
const AXES = ["resolution", "blur", "contrast", "perspective", "rotation", "lighting"];

// one version end to end: the manifest, the binary, the report
assert.equal(version(), pkg.version);

const clean = fixture("clean/gen_v5_q.png");
const report = await scan(clean, { profile: "full", budgetMs: 0 });
assert.deepEqual(report.versions, {
  scanner: pkg.version,
  pipeline: markers.pipeline.const,
  score_contract: markers.score_contract.const,
});
assert.equal(report.detections.length, 1);
const [det] = report.detections;
assert.equal(det.symbology, "qr_code");
assert.equal(det.content.text, V5Q);
assert.equal(Buffer.from(det.content.raw, "base64").toString("latin1"), V5Q);
assert.equal(det.content.charset, "utf8");
assert.equal(det.payload.kind, "url");
assert.equal(det.payload.url, V5Q);
assert.equal(det.meta.version, 5);
assert.equal(det.meta.ec_level, "q");
assert.equal(det.meta.modules, 37);
assert.equal(det.meta.inverted, false);

// the unbudgeted Full judgment of a pristine EC-Q symbol: complete, all six
// axes in wire order, the top UEC and ISO bands
const { score } = report;
assert.equal(score.weights_run, 100);
assert.deepEqual(score.axes.map((a) => a.axis), AXES);
assert.ok(Number.isInteger(score.value) && score.value >= 70 && score.value <= 100, `score ${score.value}`);
assert.equal(score.uec.grade, "a");
assert.equal(score.iso15415.overall, "a");
assert.deepEqual(untimed(scanSync(clean, { profile: "full", budgetMs: 0 })), untimed(report),
  "the sync and async paths return the same report");

const frame = scanSync(clean, { profile: "frame", budgetMs: 0 });
assert.equal(frame.score, null, "the frame profile never scores");
assert.deepEqual(frame.detections.map((d) => d.content.text), [V5Q]);

// invalid input rejects with the QRS tag
await assert.rejects(() => scan(Buffer.from("garbage")), /QRS-001/);

// abort BEFORE start rejects
const controller = new AbortController();
controller.abort();
await assert.rejects(() => scan(clean, { signal: controller.signal }), /Abort/i);

// budgetMs rides its wire position: a 1 ms budget stops the Full ladder after
// fewer attempts than the unbounded run of the same never-decoding image. The
// attempt counts come from the trace; no clock is read.
const degraded = fixture("degraded/FAIL_1491ms_0_584c998c.png");
const cut = scanSync(degraded, { profile: "full", budgetMs: 1 });
const whole = scanSync(degraded, { profile: "full", budgetMs: 0 });
assert.equal(cut.detections.length, 0);
assert.equal(whole.detections.length, 0, "the negative sample never decodes");
assert.ok(attempts(cut) < attempts(whole), `1 ms: ${attempts(cut)} attempts · unbounded: ${attempts(whole)}`);
// 0 = UNBOUNDED, not a 0 ms budget (that would return empty before any attempt)
assert.deepEqual(scanSync(clean, { profile: "fast", budgetMs: 0 }).detections.map((d) => d.content.text), [V5Q]);

// scoreSkipAxes: skipped axes absent from the wire, weights declare the
// partial contract; typos reject loudly
const skipped = scanSync(clean, { profile: "full", budgetMs: 0, scoreSkipAxes: ["perspective", "rotation"] });
assert.deepEqual(skipped.score.axes.map((a) => a.axis), ["resolution", "blur", "contrast", "lighting"]);
assert.equal(skipped.score.weights_run, 70);
assert.throws(() => scanSync(clean, { scoreSkipAxes: ["perspektive"] }), /unknown stress axis/,
  "typo'd axis must reject loudly");

// scoreSkipChecks: the skipped sections are null and nothing else moves —
// a skipped check never changes the composite (spec/04)
const noChecks = scanSync(clean, { profile: "full", budgetMs: 0, scoreSkipChecks: ["uec", "iso15415"] });
assert.equal(noChecks.score.uec, null);
assert.equal(noChecks.score.iso15415, null);
assert.equal(noChecks.score.value, score.value);
assert.deepEqual(noChecks.score.axes, score.axes);
assert.throws(() => scanSync(clean, { scoreSkipChecks: ["margin"] }), /unknown score check/,
  "typo'd check must reject loudly");

// scorePreset: design is sugar over skipping perspective + rotation
const designed = scanSync(clean, { profile: "full", budgetMs: 0, scorePreset: "design" });
assert.deepEqual(designed.score, skipped.score);
assert.equal(report.score.weights_run, 100, "the full contract says 100");
assert.throws(() => scanSync(clean, { scorePreset: "builder" }), /unknown score preset/,
  "typo'd preset must reject loudly");
assert.throws(() => scanSync(clean, { scorePreset: "design", scoreSkipAxes: ["blur"] }), /mutually exclusive/,
  "preset + explicit list must reject loudly");

// alphaBackground: a transparent input carries the alpha block (auto default);
// an opaque input NEVER does; "none" restores the drop-the-channel path
const transparent = readFileSync(here("../../playground/public/samples/transparent.png"));
const flat = scanSync(transparent, { profile: "full", budgetMs: 0 });
assert.deepEqual(flat.detections.map((d) => d.content.text), ["https://qrc-ai.com/76xMa"],
  "the flatten rescues the canvas-export class");
assert.equal(flat.alpha.mode, "auto");
assert.equal(flat.alpha.background, "white", "dark content flattens over white");
assert.equal(flat.alpha.fallback_used, false);
assert.equal(flat.alpha.envelope.placement, "light_only");
assert.deepEqual(flat.alpha.envelope.safe_luma, [[32, 255]]);
assert.deepEqual(flat.alpha.envelope.palette, []);
assert.deepEqual(
  flat.hints.filter((h) => h.hint === "alpha_background_dependent" || h.hint === "add_background_plate"),
  [{ hint: "alpha_background_dependent", placement: "light_only" }, { hint: "add_background_plate", color: "white" }],
  "the remedy hint rides with the diagnosis",
);
const opaqueForced = scanSync(clean, { profile: "full", budgetMs: 0, alphaBackground: "white" });
assert.deepEqual(opaqueForced.detections.map((d) => d.content.text), [V5Q]);
assert.ok(!("alpha" in opaqueForced), "opaque reports omit the key entirely, whatever the mode");
const dropped = scanSync(transparent, { profile: "full", budgetMs: 0, alphaBackground: "none" });
assert.equal(dropped.detections.length, 0, "none = the pre-0.9 exporter-dependent path");
assert.ok(!("alpha" in dropped), "none carries no block");
assert.throws(() => scanSync(clean, { alphaBackground: "transparent" }), /unknown alpha background/,
  "typo'd alpha background must reject loudly");
// alphaPalette: per-color verdicts inside one scan, request order
const themed = scanSync(transparent, { profile: "full", budgetMs: 0, alphaPalette: ["#f3f4f6", "black"] });
assert.deepEqual(
  themed.alpha.envelope.palette.map(({ background, background_luma, decoded }) => ({ background, background_luma, decoded })),
  [
    { background: "#f3f4f6", background_luma: 244, decoded: true },
    { background: "black", background_luma: 0, decoded: false },
  ],
);
assert.throws(() => scanSync(clean, { alphaPalette: ["auto"] }), /unknown palette color/,
  "modes are not colors — reject loudly");
assert.throws(() => scanSync(clean, { alphaPalette: Array.from({ length: 33 }, () => "#ffffff") }),
  /alpha palette too large/, "the anti-DoS cap rejects loudly, never truncates silently");

console.log(`node binding OK — native ${version()}`);
