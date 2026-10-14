// SPDX-License-Identifier: AGPL-3.0-or-later
// This file is copied outside the checkout, beside only installed dependencies.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join, relative } from "node:path";
import { Client as ModernClient, InMemoryTransport } from "@modelcontextprotocol/client";
import { pathToFileURL } from "node:url";
import { StdioClientTransport as ModernTransport } from "@modelcontextprotocol/client/stdio";
import { Client as LegacyClient } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport as LegacyTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { scan, version } from "@supernovae-st/qrcode-ai-scanner";

const root = process.cwd();
const require = createRequire(import.meta.url);
const packageRoot = realpathSync(join(root, "node_modules/@supernovae-st/qrcode-ai-scanner-mcp"));
const entry = join(packageRoot, "src/cli.mjs");
const fixture = readFileSync(join(root, "clean.png"));
const binaryFixture = readFileSync(join(root, "binary.png"));
const white = readFileSync(join(root, "white.png"));
const payload = "https://qrcode-ai.com/c/v5q";
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const untimed = ({ trace, ...report }) => ({ ...report, trace: { ...trace, total_ms: 0, stages: trace.stages.map((stage) => ({ ...stage, ms: 0 })) } });
assert.equal(version(), "0.10.0");
assert.ok(!relative(root, require.resolve("@supernovae-st/qrcode-ai-scanner")).startsWith(".."));
for (const key of ["NODE_PATH", "NAPI_RS_NATIVE_LIBRARY_PATH"]) assert.equal(process.env[key], undefined, `${key} bypasses the installed consumer`);

async function connect(era, settings = {}) {
  const modern = era === "modern";
  const transport = new (modern ? ModernTransport : LegacyTransport)({ command: process.execPath, args: [entry], cwd: root, stderr: "pipe", env: { PATH: process.env.PATH, ...settings } });
  const client = new (modern ? ModernClient : LegacyClient)({ name: "qrscan-packed-consumer", version: "1.0.0" }, modern ? { versionNegotiation: { mode: { pin: "2026-07-28" } } } : {});
  let stderr = "";
  const wire = [];
  transport.stderr.on("data", (chunk) => { stderr += chunk.toString(); });
  const send = transport.send.bind(transport);
  transport.send = async (message) => { wire.push({ direction: "sent", message }); return send(message); };
  await client.connect(transport);
  const onmessage = transport.onmessage;
  transport.onmessage = (message, extra) => { wire.push({ direction: "received", message }); return onmessage(message, extra); };
  assert.deepEqual(client.getServerVersion(), { name: "io.github.supernovae-st/qrcode-ai-scanner", version: "0.1.0", websiteUrl: "https://qrcode-ai.com" });
  if (modern) assert.equal(client.getNegotiatedProtocolVersion(), "2026-07-28");
  else assert.equal(wire[0].message.method, "initialize");
  const pid = transport.pid;
  return { client, wire, async close() { await client.close(); assert.equal(stderr, "", stderr); if (pid) assert.throws(() => process.kill(pid, 0), { code: "ESRCH" }, "server exited after stdio close"); } };
}

function reportFrom(result) {
  assert.equal(result.isError, false, JSON.stringify(result));
  assert.deepEqual(JSON.parse(result.content.find(({ type }) => type === "text").text), result.structuredContent);
  return result.structuredContent;
}
function errorFrom(result, code) {
  assert.equal(result.isError, true);
  assert.equal(result.structuredContent, undefined);
  const error = JSON.parse(result.content.find(({ type }) => type === "text").text).error;
  assert.equal(error.code, code);
  return error;
}
const receipts = [];
for (const era of ["modern", "legacy"]) {
  const session = await connect(era);
  try {
    const tools = await session.client.listTools();
    assert.deepEqual(tools.tools.map(({ name }) => name), ["scan_image"]);
    assert.deepEqual(tools.tools[0].inputSchema.required, ["imageBase64"]);
    assert.equal(tools.tools[0].inputSchema.additionalProperties, false);
    assert.deepEqual(tools.tools[0].annotations, { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false });
    const resources = await session.client.listResources();
    assert.deepEqual(resources.resources.map(({ uri }) => uri).sort(), ["qrscan://limits", "qrscan://scan-report-schema"]);
    const schema = await session.client.readResource({ uri: "qrscan://scan-report-schema" });
    assert.equal(schema.contents[0].text, readFileSync(join(packageRoot, "scan-report.schema.json"), "utf8"));
    const limits = JSON.parse((await session.client.readResource({ uri: "qrscan://limits" })).contents[0].text);
    assert.equal(limits.inputBytes, 8 * 1024 * 1024);
    assert.equal(limits.dimension, 4096);
    assert.equal(limits.pixels, 16_000_000);
    const call = (image, options = {}) => session.client.callTool({ name: "scan_image", arguments: { imageBase64: image.toString("base64"), ...options } });
    const full = reportFrom(await call(fixture, { profile: "full", budgetMs: 0 }));
    assert.deepEqual(full.versions, { scanner: "0.10.0", pipeline: 3, score_contract: 5 });
    assert.equal(full.trace.budget_exhausted, false);
    assert.equal(full.detections.length, 1);
    assert.deepEqual(full.detections[0].content, { text: payload, raw: Buffer.from(payload).toString("base64"), charset: "utf8" });
    assert.equal(full.score.value, 100);
    assert.equal(full.score.weights_run, 100);
    assert.deepEqual(full.score.axes.map(({ axis }) => axis), ["resolution", "blur", "contrast", "perspective", "rotation", "lighting"]);
    assert.equal(full.score.uec.grade, "a");
    assert.equal(full.score.iso15415.overall, "a");
    assert.equal(Object.hasOwn(full, "alpha"), false);
    assert.deepEqual(untimed(full), untimed(await scan(fixture, { profile: "full", budgetMs: 0, maxDimension: 4096, maxPixels: 16_000_000 })), "adapter preserves every non-timing report field");
    const frame = reportFrom(await call(fixture, { profile: "frame", budgetMs: 0 }));
    assert.equal(frame.score, null);
    assert.equal(frame.detections[0].content.raw, Buffer.from(payload).toString("base64"));
    assert.equal(reportFrom(await call(fixture)).score, null, "default frame profile is implemented");
    const fast = reportFrom(await call(fixture, { profile: "fast", budgetMs: 0 }));
    assert.equal(fast.detections[0].content.text, payload);
    const binary = reportFrom(await call(binaryFixture, { profile: "frame", budgetMs: 0 }));
    assert.equal(binary.detections.length, 1);
    assert.deepEqual(binary.detections[0].content, { text: "����", raw: "AP+Afw==", charset: "unknown", eci: [{ designator: 899, start: 0 }] });
    assert.deepEqual([...Buffer.from(binary.detections[0].content.raw, "base64")], [0, 255, 128, 127]);
    const empty = reportFrom(await call(white, { profile: "full", budgetMs: 0 }));
    assert.deepEqual(empty.detections, []);
    assert.equal(empty.score, null);
    assert.equal(empty.trace.budget_exhausted, false);
    errorFrom(await call(Buffer.from("garbage")), "QRS-001");
    for (const encoded of ["%%invalid", "Zg==\n", "Zh==", "data:image/png;base64,AAAA"]) errorFrom(await session.client.callTool({ name: "scan_image", arguments: { imageBase64: encoded } }), "QRSMCP_BASE64");
    for (const options of [{ path: "/private/secret" }, { url: "https://example.com/image.png" }, { scorePreset: "design" }, { profile: "typo" }, { budgetMs: -1 }, { budgetMs: 60_001 }]) {
      const result = await call(fixture, options);
      assert.equal(result.isError, true, `unsupported input must reject: ${JSON.stringify(options)}`);
    }
    const wireFile = join(root, `${era}-wire.json`);
    writeFileSync(wireFile, JSON.stringify(session.wire, null, 2));
    receipts.push({ era, sdk: era === "modern" ? "@modelcontextprotocol/client@2.3.1" : "@modelcontextprotocol/sdk@1.32.1", protocol: era === "modern" ? "2026-07-28" : session.wire[0].message.params.protocolVersion, fullScore: 100, frameScore: null, binaryRaw: "AP+Afw==", emptySuccess: true, wireSha256: sha256(readFileSync(wireFile)) });
  } finally { await session.close(); }
}

for (const [settings, image, code] of [
  [{ QRSCAN_MCP_MAX_INPUT_BYTES: "8" }, fixture, "QRSMCP_INPUT_LIMIT"],
  [{ QRSCAN_MCP_MAX_DIMENSION: "16" }, white, "QRS-002"],
  [{ QRSCAN_MCP_MAX_PIXELS: "100" }, white, "QRS-003"]
]) {
  const session = await connect("modern", settings);
  try { errorFrom(await session.client.callTool({ name: "scan_image", arguments: { imageBase64: image.toString("base64") } }), code); }
  finally { await session.close(); }
}

const byteBoundary = await connect("modern", { QRSCAN_MCP_MAX_INPUT_BYTES: String(white.length) });
try {
  const call = (bytes) => byteBoundary.client.callTool({ name: "scan_image", arguments: { imageBase64: bytes.toString("base64") } });
  assert.deepEqual(reportFrom(await call(white)).detections, [], "exact input-byte ceiling is accepted");
  errorFrom(await call(Buffer.concat([white, Buffer.of(0)])), "QRSMCP_INPUT_LIMIT");
} finally { await byteBoundary.close(); }

const limited = await connect("modern", { QRSCAN_MCP_MAX_CONCURRENT: "1" });
try {
  const args = { imageBase64: fixture.toString("base64"), profile: "full", budgetMs: 0 };
  const pending = limited.client.callTool({ name: "scan_image", arguments: args });
  errorFrom(await limited.client.callTool({ name: "scan_image", arguments: args }), "QRSMCP_BUSY");
  assert.equal(reportFrom(await pending).score.value, 100);
  assert.equal(reportFrom(await limited.client.callTool({ name: "scan_image", arguments: { ...args, profile: "frame" } })).score, null, "capacity is released after completion");
} finally { await limited.close(); }

// Distinct factory instances must not multiply the process-wide native cap.
const { createScannerServer, limitsFromEnvironment } = await import(pathToFileURL(join(packageRoot, "src/server.mjs")));
const factorySessions = [];
try {
  for (const name of ["factory-one", "factory-two"]) {
    const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
    const server = createScannerServer(limitsFromEnvironment({ QRSCAN_MCP_MAX_CONCURRENT: "1" }));
    const client = new ModernClient({ name, version: "1.0.0" });
    await server.connect(serverTransport);
    await client.connect(clientTransport);
    factorySessions.push({ client, server });
  }
  const args = { imageBase64: fixture.toString("base64"), profile: "full", budgetMs: 0 };
  const first = factorySessions[0].client.callTool({ name: "scan_image", arguments: args });
  errorFrom(await factorySessions[1].client.callTool({ name: "scan_image", arguments: args }), "QRSMCP_BUSY");
  assert.equal(reportFrom(await first).score.value, 100);
  assert.equal(reportFrom(await factorySessions[1].client.callTool({ name: "scan_image", arguments: { ...args, profile: "frame" } })).score, null);
} finally { for (const { client, server } of factorySessions) { await client.close(); await server.close(); } }

const loaded = Object.keys(require.cache).filter((file) => file.endsWith(".node"));
assert.equal(loaded.length, 1);
assert.ok(!relative(root, loaded[0]).startsWith(".."));
const receipt = { verdict: "PASS", adapter: "0.1.0", scanner: version(), node: process.version, os: process.platform, arch: process.arch, installedEntry: entry, fixtureSha256: sha256(fixture), loadedNative: { path: loaded[0], sha256: sha256(readFileSync(loaded[0])) }, sessions: receipts, inputByteCap: true, dimensionCap: true, pixelCap: true, concurrencyCap: true, concurrencyAcrossFactories: true, realProductClients: "NOT VERIFIED" };
writeFileSync(join(root, "protocol-receipt.json"), JSON.stringify(receipt, null, 2));
console.log(JSON.stringify(receipt));
