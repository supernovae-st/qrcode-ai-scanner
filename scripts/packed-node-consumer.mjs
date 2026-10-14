// Install the produced tarballs in an empty directory, then import by package
// name there. Neither the checkout's loader nor the registry can supply code.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [mode, archivesArg, fixtureArg, expectedVersion, suffix] = process.argv.slice(2);
assert.ok(["native", "wasm"].includes(mode) && archivesArg && fixtureArg && expectedVersion,
  "usage: packed-node-consumer.mjs native|wasm ARCHIVES FIXTURE VERSION [NAPI_SUFFIX]");
const archives = resolve(archivesArg);
const digest = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const name = `@supernovae-st/qrcode-ai-scanner${mode === "wasm" ? "-wasm" : ""}`;
const files = readdirSync(archives, { recursive: true }).filter((file) => file.endsWith(".tgz"));
const packages = files.map((file) => {
  const path = join(archives, file);
  // Git's GNU tar treats a Windows drive colon as a remote archive. Feed
  // the same tarball bytes through stdin on every host instead.
  const manifest = JSON.parse(execFileSync("tar", ["-xzOf", "-", "package/package.json"],
    { input: readFileSync(path), encoding: "utf8" }));
  assert.equal(manifest.version, expectedVersion, `${manifest.name}: wrong producer version`);
  return { path, manifest, sha256: digest(path) };
});
const unique = (packageName) => {
  const found = packages.filter(({ manifest }) => manifest.name === packageName);
  assert.equal(found.length, 1, `exactly one tarball for ${packageName}`);
  return found[0];
};
const main = unique(name);
const selected = [main];
if (mode === "native") {
  assert.ok(suffix, "the native runtime row must name its NAPI suffix");
  const platform = unique(`${name}-${suffix}`);
  assert.equal(main.manifest.optionalDependencies[platform.manifest.name], expectedVersion);
  selected.push(platform);
}
const consumer = mkdtempSync(join(tmpdir(), "qrscan-packed-node-"));
writeFileSync(join(consumer, "package.json"), '{"name":"packed-consumer","private":true}\n');
const fixture = join(consumer, "fixture.png");
copyFileSync(resolve(fixtureArg), fixture);
const nodeDir = dirname(realpathSync(process.execPath));
const npmCli = [join(nodeDir, "node_modules/npm/bin/npm-cli.js"),
  join(nodeDir, "../lib/node_modules/npm/bin/npm-cli.js")].find(existsSync);
assert.ok(npmCli, "npm must be the CLI shipped with this Node runtime");
for (const key of ["NODE_PATH", "NAPI_RS_NATIVE_LIBRARY_PATH"]) {
  assert.equal(process.env[key], undefined, `${key} would bypass the clean installation`);
}
execFileSync(process.execPath, [npmCli, "install", "--offline", "--ignore-scripts", "--omit=optional",
  "--no-audit", "--no-fund", "--cache", join(consumer, "empty-npm-cache"),
  ...selected.map(({ path }) => path)], { cwd: consumer, stdio: "inherit", timeout: 180_000 });
const require = createRequire(join(consumer, "probe.cjs"));
const imported = realpathSync(require.resolve(name));
const packageRoot = realpathSync(join(consumer, "node_modules", ...name.split("/")));
assert.ok(relative(packageRoot, imported) && !relative(packageRoot, imported).startsWith(".."), imported);
assert.equal(JSON.parse(readFileSync(join(packageRoot, "package.json"), "utf8")).version, expectedVersion);
const bytes = readFileSync(fixture);
const text = "https://qrcode-ai.com/c/v5q";
const axes = ["resolution", "blur", "contrast", "perspective", "rotation", "lighting"];
const white = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAXklEQVR4nO3PMQ0AMAzAsPInvYLYYVWKESTzjhsd8KsBrQGtAa0BrQGtAa0BrQGtAa0BrQGtAa0BrQGtAa0BrQGtAa0BrQGtAa0BrQGtAa0BrQGtAa0BrQGtAa0BbQHKU9LC7/CP1AAAAABJRU5ErkJggg==", "base64");
let qr, scan, full, loaded;
if (mode === "native") {
  process.env.NAPI_RS_ENFORCE_VERSION_CHECK = "1";
  qr = require(name);
  scan = (input, profile) => qr.scanSync(input, { profile, budgetMs: 0 });
  full = await qr.scan(bytes, { profile: "full", budgetMs: 0 });
  loaded = Object.keys(require.cache).filter((file) => file.endsWith(".node"));
  assert.equal(loaded.length, 1, "exactly one native binary loaded");
  assert.equal(realpathSync(loaded[0]), realpathSync(join(consumer, "node_modules", ...selected[1].manifest.name.split("/"),
    `qrcode-ai-scanner.${suffix}.node`)));
  const untimed = ({ trace, ...rest }) => ({ ...rest,
    trace: { ...trace, total_ms: 0, stages: trace.stages.map((stage) => ({ ...stage, ms: 0 })) } });
  assert.deepEqual(untimed(scan(bytes, "full")), untimed(full));
  await assert.rejects(() => qr.scan(Buffer.from("garbage")), /\[QRS-001\]/);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(() => qr.scan(bytes, { signal: controller.signal }), { name: "AbortError" });
  // A fresh process must reject a stale platform manifest: the positive
  // import's module cache must not turn this into a false negative proof.
  const platformManifest = join(dirname(loaded[0]), "package.json");
  const original = readFileSync(platformManifest);
  try {
    const stale = JSON.parse(original);
    stale.version = "0.0.0-mismatch";
    writeFileSync(platformManifest, JSON.stringify(stale));
    const mismatch = JSON.parse(execFileSync(process.execPath, ["-e", `
      try { require(${JSON.stringify(name)}); process.exit(1); }
      catch (error) {
        const chain = [];
        for (let current = error; current; current = current.cause) chain.push(current.message);
        console.log(JSON.stringify(chain));
      }`], { cwd: consumer, encoding: "utf8", timeout: 30_000 }));
    assert.equal(mismatch[0], "Cannot find native binding. npm has a bug related to optional dependencies (https://github.com/npm/cli/issues/4828). Please try `npm i` again after removing both package-lock.json and node_modules directory.");
    assert.ok(mismatch.includes(`Native binding package version mismatch, expected ${expectedVersion} but got 0.0.0-mismatch. You can reinstall dependencies to fix this issue.`));
  } finally {
    writeFileSync(platformManifest, original);
    assert.deepEqual(readFileSync(platformManifest), original, "restore only the consumer manifest, byte for byte");
  }
} else {
  assert.equal(typeof globalThis.window, "undefined", "this is a Node SSR import");
  qr = await import(pathToFileURL(imported));
  const wasm = join(packageRoot, "qrcode-ai-scanner_bg.wasm");
  qr.initSync({ module: readFileSync(wasm) });
  loaded = [wasm];
  scan = (input, profile) => qr.scan_image(input, profile, undefined, undefined, 0);
  full = scan(bytes, "full");
  assert.throws(() => qr.scan_frame(new Uint8Array(10), 2, 2), /\[QRS-004\]/);
}
assert.equal(qr.version(), expectedVersion);
assert.deepEqual(full.versions, { scanner: expectedVersion, pipeline: 3, score_contract: 5 });
assert.equal(full.trace.budget_exhausted, false);
assert.equal(full.detections.length, 1);
assert.equal(full.detections[0].symbology, "qr_code");
assert.deepEqual(full.detections[0].content, { text, raw: Buffer.from(text).toString("base64"), charset: "utf8" });
assert.equal(full.detections[0].payload.kind, "url");
assert.equal(full.detections[0].payload.url, text);
assert.equal(full.score.value, 100);
assert.equal(full.score.weights_run, 100);
assert.deepEqual(full.score.axes.map(({ axis }) => axis), axes);
assert.equal(full.score.uec.grade, "a");
assert.equal(full.score.iso15415.overall, "a");
const frame = scan(bytes, "frame");
assert.equal(frame.score, null);
assert.deepEqual(frame.detections.map(({ content }) => content.text), [text]);
const empty = scan(white, "full");
assert.deepEqual(empty.detections, []);
assert.equal(empty.score, null);
assert.equal(empty.trace.budget_exhausted, false);
assert.throws(() => scan(Buffer.from("garbage"), "full"), /\[QRS-001\]/);
const receipt = { verdict: "PASS", mode, node: process.version, platform: process.platform, arch: process.arch,
  consumer, imported, version: expectedVersion, full_score: full.score.value, fixture_sha256: digest(fixture),
  installed_tarballs: selected.map(({ path, sha256 }) => ({ path, sha256 })),
  platform_version_mismatch_refused: mode === "native" ? true : null,
  loaded: loaded.map((path) => ({ path, sha256: digest(path) })) };
writeFileSync(join(consumer, "receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`);
console.log(JSON.stringify(receipt));
if (process.env.GITHUB_OUTPUT) writeFileSync(process.env.GITHUB_OUTPUT, `receipt=${join(consumer, "receipt.json")}\n`, { flag: "a" });
