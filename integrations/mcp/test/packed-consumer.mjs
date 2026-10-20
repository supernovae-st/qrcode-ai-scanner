// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = resolve(here, "..");
const source = resolve(pkg, "../..");
const evidence = process.env.QRSCAN_MCP_EVIDENCE_DIR ? resolve(process.env.QRSCAN_MCP_EVIDENCE_DIR) : mkdtempSync(join(tmpdir(), "qrscan-mcp-packed-"));
mkdirSync(evidence, { recursive: true });
const runtime = dirname(realpathSync(process.execPath));
const npmCli = [join(runtime, "node_modules/npm/bin/npm-cli.js"), join(runtime, "../lib/node_modules/npm/bin/npm-cli.js")].find(existsSync);
assert.ok(npmCli, "use npm shipped with the selected Node runtime");
const npm = (args, cwd) => execFileSync(process.execPath, [npmCli, ...args], { cwd, encoding: "utf8", timeout: 180_000 });
const packed = JSON.parse(npm(["pack", "--json", "--pack-destination", evidence], pkg))[0];
assert.deepEqual(packed.files.map(({ path }) => path).sort(), ["LICENSE", "README.md", "package.json", "scan-report.schema.json", "server.json", "src/cli.mjs", "src/server.mjs"]);
assert.equal(packed.files.find(({ path }) => path === "src/cli.mjs").mode, 0o755);
const archive = join(evidence, packed.filename);
const consumer = mkdtempSync(join(tmpdir(), "qrscan-mcp-install-"));
writeFileSync(join(consumer, "package.json"), '{"name":"qrscan-mcp-archive-consumer","private":true,"type":"module"}\n');
const installLog = npm(["install", "--ignore-scripts", "--no-audit", "--no-fund", "--cache", join(consumer, "empty-npm-cache"), archive, "@modelcontextprotocol/client@2.3.1", "@modelcontextprotocol/sdk@1.32.1"], consumer);
writeFileSync(join(evidence, "installation.log"), installLog);
copyFileSync(join(here, "protocol-consumer.mjs"), join(consumer, "probe.mjs"));
copyFileSync(join(source, "fixtures/clean/gen_v5_q.png"), join(consumer, "clean.png"));
copyFileSync(join(source, "fixtures/eci/eci899-binary.png"), join(consumer, "binary.png"));
const white = readFileSync(join(source, "scripts/packed-node-consumer.mjs"), "utf8").match(/const white = Buffer.from\("([A-Za-z0-9+/=]+)"/);
assert.ok(white, "reuse the established public negative PNG fixture");
writeFileSync(join(consumer, "white.png"), Buffer.from(white[1], "base64"));
const installed = join(consumer, "node_modules/@supernovae-st/qrcode-ai-scanner-mcp");
for (const file of ["LICENSE", "scan-report.schema.json", "src/cli.mjs", "src/server.mjs"]) assert.deepEqual(readFileSync(join(installed, file)), readFileSync(join(pkg, file)), `installed ${file} matches packed source bytes`);
assert.deepEqual(readFileSync(join(installed, "scan-report.schema.json")), readFileSync(join(source, "spec/scan-report.schema.json")));
assert.deepEqual(readFileSync(join(installed, "LICENSE")), readFileSync(join(source, "LICENSE")));
const locked = JSON.parse(readFileSync(join(consumer, "package-lock.json")));
assert.equal(locked.packages["node_modules/@supernovae-st/qrcode-ai-scanner"].version, "0.10.0");
assert.equal(locked.packages["node_modules/@modelcontextprotocol/server"].version, "2.3.1");
assert.equal(locked.packages["node_modules/zod"].version, "4.6.7");
const oversized = spawn(process.execPath, [join(installed, "src/cli.mjs")], { cwd: consumer, env: { PATH: process.env.PATH }, stdio: ["pipe", "pipe", "pipe"] });
let oversizeStderr = "";
let oversizeStdout = "";
oversized.stderr.on("data", (chunk) => { oversizeStderr += chunk.toString(); });
oversized.stdout.on("data", (chunk) => { oversizeStdout += chunk.toString(); });
oversized.stdin.on("error", () => {}); // EPIPE is the expected refused input.
await new Promise((accept, reject) => {
  const deadline = setTimeout(() => { oversized.kill("SIGKILL"); reject(new Error("oversize stdio must close actual IO even when the peer keeps stdin open")); }, 5000);
  oversized.on("error", (error) => { clearTimeout(deadline); reject(error); });
  oversized.once("exit", (code, signal) => { clearTimeout(deadline); try { assert.equal(code, 1); assert.equal(signal, null); accept(); } catch (error) { reject(error); } });
  oversized.stdin.write(Buffer.alloc(12 * 1024 * 1024 + 1, 65));
  // Intentionally do not end stdin: the server must enforce its own cap.
});
assert.equal(oversizeStdout, "", "fatal framing diagnostics stay off protocol stdout");
assert.match(oversizeStderr, /^qrscan-mcp: MCP transport error\n/);
writeFileSync(join(evidence, "oversize-transport-receipt.json"), JSON.stringify({ verdict: "PASS", inputBytes: 12 * 1024 * 1024 + 1, peerKeptStdinOpen: true, serverExit: 1, stdoutBytes: 0, diagnostic: oversizeStderr, activeNativeWorkInterrupted: "NOT CLAIMED" }));
for (const settings of [{ QRSCAN_MCP_MAX_INPUT_BYTES: "0" }, { QRSCAN_MCP_MAX_INPUT_BYTES: "8388609" }, { QRSCAN_MCP_MAX_PIXELS: "1.5" }, { QRSCAN_MCP_MAX_CONCURRENT: "3" }, { NAPI_RS_NATIVE_LIBRARY_PATH: "/private/unsupported.node" }]) {
  const rejected = spawnSync(process.execPath, [join(installed, "src/cli.mjs")], { cwd: consumer, env: { PATH: process.env.PATH, ...settings }, input: "", encoding: "utf8", timeout: 10_000 });
  assert.equal(rejected.status, 1);
  assert.equal(rejected.stdout, "", "startup diagnostics cannot corrupt stdio");
  assert.match(rejected.stderr, /^qrscan-mcp:/);
}
const output = execFileSync(process.execPath, [join(consumer, "probe.mjs")], { cwd: consumer, encoding: "utf8", timeout: 180_000 });
writeFileSync(join(evidence, "protocol-output.json"), output);
copyFileSync(join(consumer, "protocol-receipt.json"), join(evidence, "protocol-receipt.json"));
for (const era of ["modern", "legacy"]) copyFileSync(join(consumer, `${era}-wire.json`), join(evidence, `${era}-wire.json`));
copyFileSync(join(consumer, "package-lock.json"), join(evidence, "consumer-package-lock.json"));
const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const adapterStatus = execFileSync("git", ["status", "--porcelain", "--untracked-files=all", "--", "integrations/mcp"], { cwd: source, encoding: "utf8" }).trim();
const receipt = { verdict: "PASS", archive, archiveSha256: sha256(archive), archiveIntegrity: packed.integrity, files: packed.files, sourceRevision: execFileSync("git", ["rev-parse", "HEAD"], { cwd: source, encoding: "utf8" }).trim(), sourceContainsUncommittedAdapter: adapterStatus.length !== 0, adapterWorktreeStatus: adapterStatus.split("\n").filter(Boolean), consumer, node: process.version, protocolReceiptSha256: sha256(join(evidence, "protocol-receipt.json")), clientAcceptance: "SDK consumers only; two real product clients NOT VERIFIED" };
writeFileSync(join(evidence, "packed-receipt.json"), JSON.stringify(receipt, null, 2));
console.log(JSON.stringify(receipt));
