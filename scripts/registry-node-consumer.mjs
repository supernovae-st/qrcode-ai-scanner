// Reuse the owning packed-consumer assertions with an actual exact-version
// registry install. Bind downloaded and loaded bytes to the tag producer.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const [mode, fixtureArg, expectedVersion, suffix] = process.argv.slice(2);
assert.ok(["native", "wasm"].includes(mode) && fixtureArg && expectedVersion,
  "usage: registry-node-consumer.mjs native|wasm FIXTURE VERSION [NAPI_SUFFIX]");
assert.equal(expectedVersion, "0.10.0");
assert.equal(Number(process.version.split(".")[0].slice(1)), Number(process.env.NODE_LINE));
assert.ok([22, 24].includes(Number(process.env.NODE_LINE)));
if (mode === "native") {
  assert.equal(process.platform, suffix.startsWith("darwin") ? "darwin" : suffix.startsWith("win32") ? "win32" : "linux");
  assert.equal(process.arch, suffix.includes("arm64") ? "arm64" : "x64");
}
const proof = resolve(process.env.REGISTRY_PROOF_DIR);
mkdirSync(proof, { recursive: true });
const archives = join(proof, "registry-tarballs");
mkdirSync(archives);
const producer = { source: "2d3b577582979f0dc603b5cb0d96d1be02b459f5", run: 38514060480,
  native_zip_sha256: "c67bd0cde8f180bad9c3133e4b3cf1e5819a3e081f8d4e860229a83cf2277aff",
  wasm_zip_sha256: "dad4842d2ebc8764981412d12dc95a96e58adc7cfb23227fe6c8ad9e54c8a8d4" };
const expected = {
  "@supernovae-st/qrcode-ai-scanner-darwin-arm64": {
    "tgz_sha256": "f8c012932bd9ae618c04da0c38ae89ae446dd2fec04109fd7c8b288ba57404ab",
    "binary_members": {
      "package/qrcode-ai-scanner.darwin-arm64.node": "a9c3c8f692e5388a96c1cb81adc997bd28aac9a69f144af382221b2cb865b75a"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-darwin-x64": {
    "tgz_sha256": "a527e05de92b122971e2af2a062e463573f6cd07129429944bbf47a88d043bed",
    "binary_members": {
      "package/qrcode-ai-scanner.darwin-x64.node": "cb9ccf86eb330912bfae6db5be02d50ee99a26894ded1e7f98111d4b82863993"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-linux-arm64-gnu": {
    "tgz_sha256": "ea96094420f2099ed5886dc4ae02809f6ac5dfe584a24db14fcce35d9ada5e9f",
    "binary_members": {
      "package/qrcode-ai-scanner.linux-arm64-gnu.node": "88fa796ed82a2199472b8104fee585d8cd00315e153fac1de45fd064bd6daf77"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-linux-x64-gnu": {
    "tgz_sha256": "99f20c9229c7ee81c60688fdd20da24a7251ebfe4f3b3feb7b71e6eafc9d4bfb",
    "binary_members": {
      "package/qrcode-ai-scanner.linux-x64-gnu.node": "2742c405d4254368519ba770877d89ba788ec27c298c32f966edb5a503586272"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-linux-x64-musl": {
    "tgz_sha256": "b3d9e940f9d7e538b802cc63f7c0faff890d741f8d876d6c2a28a40c7fe749e5",
    "binary_members": {
      "package/qrcode-ai-scanner.linux-x64-musl.node": "fe94d712d3939c2f544ebe8bc146ab016b39433db62942becb2c7927041aadb4"
    }
  },
  "@supernovae-st/qrcode-ai-scanner": {
    "tgz_sha256": "7e06fb0af9114d40e202429f74e90d81a408e62b70a6b3d781473737d248d465",
    "binary_members": {}
  },
  "@supernovae-st/qrcode-ai-scanner-win32-x64-msvc": {
    "tgz_sha256": "a9ccd7ee0b017669a3207c100cf36ac23ab0fb216d59600f999b9723057a3b78",
    "binary_members": {
      "package/qrcode-ai-scanner.win32-x64-msvc.node": "7b4040ff78c96299da65c876edb14c7860a4ad667ea506c0c6796738d57a645c"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-wasm": {
    "tgz_sha256": "ba7ba5eda7961bf987c0062b720892101e83be46c9ef08c5314922ba82c7f567",
    "binary_members": {
      "package/qrcode-ai-scanner_bg.wasm": "e8718ee68e75e737e0fb3c65bcf3d2ca7d2aa5bead7e9c6cb203c0c7360d2cc5"
    }
  }
};
const registry = new Map();
async function get(url) {
  assert.equal(new URL(url).protocol, "https:");
  assert.equal(new URL(url).hostname, "registry.npmjs.org");
  const response = await fetch(url, { signal: AbortSignal.timeout(60_000) });
  assert.equal(response.status, 200, `${url}: registry request must succeed`);
  assert.equal(new URL(response.url).hostname, "registry.npmjs.org");
  const data = Buffer.from(await response.arrayBuffer());
  assert.ok(data.length <= 64 * 1024 ** 2, "bounded registry response");
  return data;
}
const base = "@supernovae-st/qrcode-ai-scanner";
const nativeSuffixes = ["darwin-arm64", "darwin-x64", "linux-arm64-gnu", "linux-x64-gnu", "linux-x64-musl", "win32-x64-msvc"];
if (mode === "native") assert.ok(nativeSuffixes.includes(suffix), "qualified native runtime target");
// npm locks every optional platform even when --omit=optional installs only
// the explicitly requested runtime platform. Verify both closures separately.
const requested = mode === "wasm" ? [base + "-wasm"] : [base, ...nativeSuffixes.map((target) => base + "-" + target)];
for (const requestedName of requested) {
  assert.ok(expected[requestedName], "the runtime row must be in the qualified producer matrix");
  const metadata = JSON.parse(await get(`https://registry.npmjs.org/${encodeURIComponent(requestedName)}/${expectedVersion}`));
  assert.equal(metadata.name, requestedName);
  assert.equal(metadata.version, expectedVersion);
  const data = await get(metadata.dist.tarball);
  assert.equal(createHash("sha256").update(data).digest("hex"), expected[requestedName].tgz_sha256,
    "registry tarball must match the exact tag's official producer bytes");
  assert.equal(metadata.dist.integrity, "sha512-" + createHash("sha512").update(data).digest("base64"));
  assert.equal(metadata.dist.shasum, createHash("sha1").update(data).digest("hex"));
  writeFileSync(join(archives, requestedName.split("/").at(-1) + "-0.10.0.tgz"), data);
  writeFileSync(join(proof, requestedName.split("/").at(-1) + "-registry.json"), JSON.stringify(metadata, null, 2) + "\n");
  registry.set(requestedName, metadata);
}
if (mode === "native") {
  assert.deepEqual(registry.get(base).optionalDependencies,
    Object.fromEntries(nativeSuffixes.map((target) => [base + "-" + target, expectedVersion])));
}
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
const userConfig = join(consumer, "empty-user.npmrc");
const globalConfig = join(consumer, "empty-global.npmrc");
writeFileSync(userConfig, "");
writeFileSync(globalConfig, "");
execFileSync(process.execPath, [npmCli, "install", "--ignore-scripts", "--omit=optional",
  "--no-audit", "--no-fund", "--registry", "https://registry.npmjs.org/",
  "--userconfig", userConfig, "--globalconfig", globalConfig,
  "--cache", join(consumer, "empty-npm-cache"),
  ...selected.map(({ manifest }) => `${manifest.name}@${expectedVersion}`)],
  { cwd: consumer, stdio: "inherit", timeout: 180_000 });
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
// The unchanged owning assertions above exercise the module actually installed.
// Now bind that installation's resolution and loaded bytes to the public closure.
const lockPath = join(consumer, "package-lock.json");
const lock = JSON.parse(readFileSync(lockPath, "utf8"));
const lockRecords = Object.entries(lock.packages).filter(([path]) => path);
assert.equal(lockRecords.length, requested.length);
assert.deepEqual(lockRecords.map(([path]) => path.replace(/^node_modules\//, "")).sort(), [...requested].sort());
for (const [path, entry] of lockRecords) {
  const packageName = path.replace(/^node_modules\//, "");
  const metadata = registry.get(packageName);
  assert.ok(metadata, "no unexpected locked package or source fallback");
  assert.equal(entry.version, expectedVersion);
  assert.equal(entry.resolved, metadata.dist.tarball);
  assert.equal(entry.integrity, metadata.dist.integrity);
}
const installedNames = readdirSync(join(consumer, "node_modules", "@supernovae-st"))
  .map((packageName) => "@supernovae-st/" + packageName).sort();
const selectedNames = selected.map(({ manifest }) => manifest.name).sort();
assert.deepEqual(installedNames, selectedNames, "only the selected runtime packages are installed");
const excludedOptionalNames = requested.filter((packageName) => !selectedNames.includes(packageName));
for (const packageName of excludedOptionalNames) {
  assert.equal(existsSync(join(consumer, "node_modules", ...packageName.split("/"))), false,
    "the five unselected optional platforms must remain absent");
}
assert.equal(readFileSync(userConfig, "utf8"), "");
assert.equal(readFileSync(globalConfig, "utf8"), "");
for (const file of loaded) {
  const publicName = mode === "native" ? selected[1].manifest.name : name;
  assert.equal(digest(file), Object.values(expected[publicName].binary_members)[0],
    "the loaded binary must match its exact producer and public registry archive");
}
writeFileSync(join(proof, "package-lock.json"), readFileSync(lockPath));
const receipt = { verdict: "PASS", installation: "fresh public registry exact-version install", mode, node: process.version, platform: process.platform, arch: process.arch,
  consumer, imported, version: expectedVersion, full_score: full.score.value, fixture_sha256: digest(fixture),
  harness_sha: process.env.GITHUB_SHA, diagnostic_run: process.env.GITHUB_RUN_ID,
  producer, suffix: suffix ?? null, npm: execFileSync(process.execPath, [npmCli, "--version"], { encoding: "utf8" }).trim(),
  lock_sha256: digest(lockPath), registry_lock: lockRecords.map(([path, entry]) => ({ path, version: entry.version,
    resolved: entry.resolved, integrity: entry.integrity })),
  installed_packages: installedNames, excluded_optional_packages: excludedOptionalNames,
  registry_artifacts: packages.map(({ path, sha256, manifest }) => ({ path, sha256, name: manifest.name,
    dist_integrity: registry.get(manifest.name).dist.integrity, producer_binary_members: expected[manifest.name].binary_members })),
  platform_version_mismatch_refused: mode === "native" ? true : null,
  loaded: loaded.map((path) => ({ path, sha256: digest(path) })) };
assert.equal(digest(fixture), "1dc399cad19623a145b20251cf1c0da42ea4f54fd79871a4dd796c9554822dd6");
writeFileSync(join(proof, "receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`);
const filesInProof = readdirSync(proof, { recursive: true }).filter((file) => !file.endsWith("sha256.json"));
const hashes = Object.fromEntries(filesInProof.filter((file) => !["registry-tarballs"].includes(file)).map((file) => {
  const path = join(proof, file);
  return [file.replaceAll("\\", "/"), { sha256: digest(path), bytes: readFileSync(path).length }];
}));
writeFileSync(join(proof, "sha256.json"), JSON.stringify(hashes, null, 2) + "\n");
console.log(JSON.stringify(receipt));
if (process.env.GITHUB_OUTPUT) writeFileSync(process.env.GITHUB_OUTPUT, `receipt=${join(proof, "receipt.json")}\n`, { flag: "a" });
