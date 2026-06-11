// Smoke: async scan + abort semantics + report shape. Run via `pnpm test`.
import { scan, scanSync, version } from "./index.js";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";

const clean = readFileSync(new URL("../../fixtures/clean/gen_v5_q.png", import.meta.url));

const report = await scan(clean);
assert.equal(report.detections.length, 1);
assert.equal(report.detections[0].content.text, "https://qrcode-ai.com/c/v5q");
assert.equal(report.detections[0].payload.kind, "url");
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

console.log(`node binding OK — native ${version()}`);
