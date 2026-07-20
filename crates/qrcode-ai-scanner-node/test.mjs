// Smoke: async scan + abort semantics + report shape. Run via `pnpm test`.
import { scan, scanSync, version } from "./index.js";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";

const clean = readFileSync(new URL("../../fixtures/clean/gen_v5_q.png", import.meta.url));

const report = await scan(clean);
assert.equal(report.detections.length, 1);
assert.equal(report.detections[0].content.text, "https://qrcode-ai.com/c/v5q");
assert.equal(report.detections[0].payload.kind, "url");
assert.equal(report.detections[0].symbology, "qr_code");
assert.ok(report.score.value >= 70, `score ${report.score.value}`);
assert.equal(report.score.uec.grade, "a");
assert.equal(report.versions.score_contract, 4);

const sync = scanSync(clean, { profile: "frame" });
assert.equal(sync.score, null, "frame profile skips scoring");
assert.equal(sync.detections.length, 1);

// invalid input rejects with the QRS tag
await assert.rejects(() => scan(Buffer.from("garbage")), /QRS-001/);

// abort BEFORE start rejects
const controller = new AbortController();
controller.abort();
await assert.rejects(() => scan(clean, { signal: controller.signal }), /Abort/i);

// budgetMs override: a degraded image under a 1ms budget returns FAST and
// empty (the full ladder would otherwise grind it) — pins the wire position
const degraded = readFileSync(new URL("../../fixtures/degraded/FAIL_1491ms_0_584c998c.png", import.meta.url));
const t0 = performance.now();
const tight = scanSync(degraded, { profile: "full", budgetMs: 1 });
const elapsed = performance.now() - t0;
assert.equal(tight.detections.length, 0);
assert.ok(elapsed < 1500, `1ms budget must cut early, took ${elapsed}ms`);
// 0 = UNBOUNDED, not a 0ms budget — a clean symbol must still decode
const unbounded = scanSync(clean, { profile: "fast", budgetMs: 0 });
assert.equal(unbounded.detections.length, 1, "budgetMs 0 means unbounded");

// scoreSkipAxes: skipped axes absent from the wire; typos reject loudly
const skipped = scanSync(clean, { profile: "full", budgetMs: 0, scoreSkipAxes: ["perspective", "rotation"] });
assert.equal(skipped.score.axes.length, 4, "six axes minus the two skipped");
assert.ok(
  skipped.score.axes.every((a) => a.axis !== "perspective" && a.axis !== "rotation"),
  "skipped axes absent from the wire",
);
assert.throws(
  () => scanSync(clean, { scoreSkipAxes: ["perspektive"] }),
  /unknown stress axis/,
  "typo'd axis must reject loudly",
);

// scoreSkipChecks: skipped sections null on the wire; composite untouched; typos reject
const noChecks = scanSync(clean, { profile: "full", budgetMs: 0, scoreSkipChecks: ["uec", "iso15415"] });
assert.equal(noChecks.score.uec, null, "skipped uec must be null");
assert.equal(noChecks.score.iso15415, null, "skipped iso15415 must be null");
assert.ok(Number.isInteger(noChecks.score.value), "composite still scores");
assert.throws(
  () => scanSync(clean, { scoreSkipChecks: ["margin"] }),
  /unknown score check/,
  "typo'd check must reject loudly",
);

// alphaBackground: a transparent input carries the alpha block (auto default);
// an opaque input NEVER does; "none" restores the drop-the-channel path; typos reject
const transparent = readFileSync(new URL("../../playground/public/samples/transparent.png", import.meta.url));
const flat = scanSync(transparent, { profile: "full", budgetMs: 0 });
assert.equal(flat.detections.length, 1, "the flatten rescues the canvas-export class");
assert.equal(flat.alpha.background, "white", "dark content flattens over white");
assert.equal(flat.alpha.envelope.placement, "light_only");
// an opaque input never carries the block — even under a FORCED mode
const opaqueForced = scanSync(clean, { profile: "full", budgetMs: 0, alphaBackground: "white" });
assert.equal(opaqueForced.detections.length, 1);
assert.ok(!("alpha" in opaqueForced), "opaque reports omit the key entirely, whatever the mode");
const dropped = scanSync(transparent, { profile: "full", budgetMs: 0, alphaBackground: "none" });
assert.equal(dropped.detections.length, 0, "none = the pre-0.9 exporter-dependent path");
assert.ok(!("alpha" in dropped), "none carries no block");
assert.throws(
  () => scanSync(clean, { alphaBackground: "transparent" }),
  /unknown alpha background/,
  "typo'd alpha background must reject loudly",
);
// alphaPalette: per-color verdicts inside one scan, request order
const themed = scanSync(transparent, { profile: "full", budgetMs: 0, alphaPalette: ["#f3f4f6", "black"] });
assert.equal(themed.alpha.envelope.palette.length, 2);
assert.equal(themed.alpha.envelope.palette[0].background, "#f3f4f6");
assert.equal(themed.alpha.envelope.palette[0].decoded, true, "light theme carries the dark design");
assert.equal(themed.alpha.envelope.palette[1].decoded, false, "black-on-black cannot decode");
assert.ok(
  themed.hints.some((h) => h.hint === "add_background_plate" && h.color === "white"),
  "the remedy hint rides with the diagnosis",
);
assert.throws(
  () => scanSync(clean, { alphaPalette: ["auto"] }),
  /unknown palette color/,
  "modes are not colors — reject loudly",
);
assert.throws(
  () => scanSync(clean, { alphaPalette: Array.from({ length: 33 }, () => "#ffffff") }),
  /alpha palette too large/,
  "the anti-DoS cap rejects loudly, never truncates silently",
);

console.log(`node binding OK — native ${version()}`);
