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
assert.equal(report.versions.score_contract, 3);

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

console.log(`node binding OK — native ${version()}`);
