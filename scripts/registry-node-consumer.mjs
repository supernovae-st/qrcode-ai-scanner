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
assert.equal(expectedVersion, "0.10.1");
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
const producer = {
  "source": "af440882cd50728a3340eddf089ea2ce89d72a3f",
  "run": 39416169094,
  "native_zip_sha256": "59f3a6eeb5d30a1f5445ac0919cf3e86c6281fb4140c0be7a7f07be55202970f",
  "wasm_zip_sha256": "9c7b598517a79e7c61cf9c969ed25fb376864073a6f03b4a0c405eb8754601a1"
};
const expected = {
  "@supernovae-st/qrcode-ai-scanner-wasm": {
    "tgz_sha256": "a1997fba9ef1947eb033c92a4ed8104af5eb125944786a0e4cbc843809612bc3",
    "binary_members": {
      "package/compatible/qrcode-ai-scanner_bg.wasm": "d757e21e362c7d05fa4b3b5a00d2c446ab27fd46f2d59c18a0252a4136ed73db",
      "package/qrcode-ai-scanner_bg.wasm": "460060fded2bb0dfb560c9193a039f012ff386d7ea527b94f46b71fa32676d86"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-win32-x64-msvc": {
    "tgz_sha256": "82da331fc3b28c9b689755755db567c60c80ba283db62afa119f20c2a60396ba",
    "binary_members": {
      "package/qrcode-ai-scanner.win32-x64-msvc.node": "d9024876b3eca40bb6ef508f91d586afbe0e904580e132f1238af208a8fd097b"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-darwin-x64": {
    "tgz_sha256": "b20d2d3902bbac4b877a3ab65b0794d1c987a1a3a2efadaa27a3a9316514dd8c",
    "binary_members": {
      "package/qrcode-ai-scanner.darwin-x64.node": "cfc37ff01e795460cddfc792cea1521c6ea4724e2fc855cf32ab13bc3dd51243"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-linux-arm64-gnu": {
    "tgz_sha256": "15814456c0e7f83fb1571227bdc12f2fa408581db48577657cc26707f643bb4f",
    "binary_members": {
      "package/qrcode-ai-scanner.linux-arm64-gnu.node": "0b68bc8e71849d8092a96c2aaa99739ea3164933eb76a3423a471656db1eeed7"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-linux-x64-musl": {
    "tgz_sha256": "2d53c398c4b1f1b0cc8b7e83cb3186afc44c484c4ba5a500c226ccf57ca1031c",
    "binary_members": {
      "package/qrcode-ai-scanner.linux-x64-musl.node": "1755d2c197448a079d94ac43569701ed225af167bd3130367e4ae5bf28738d84"
    }
  },
  "@supernovae-st/qrcode-ai-scanner-linux-x64-gnu": {
    "tgz_sha256": "2652d28dd14a5b0ba39f4178ca9ffd68f979e72a1df128553100fafbfb7453ef",
    "binary_members": {
      "package/qrcode-ai-scanner.linux-x64-gnu.node": "1e6dacaad5de11891c545b7f0938633bdc8dde1267f0148a4f5b2523a5068d36"
    }
  },
  "@supernovae-st/qrcode-ai-scanner": {
    "tgz_sha256": "31ef66deca7112ab73815d83e10397ac47a28fe32ac713c1172db9f898f39ee8",
    "binary_members": {}
  },
  "@supernovae-st/qrcode-ai-scanner-darwin-arm64": {
    "tgz_sha256": "412d0bfd05494183caa34d09fbfcb898634fc2de419b730eae4d41db912c9b59",
    "binary_members": {
      "package/qrcode-ai-scanner.darwin-arm64.node": "32cdaa2d77f4c498e06dedb7f96caa3e9121fb6af2b36dfaf66dc76c97509570"
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
  writeFileSync(join(archives, requestedName.split("/").at(-1) + "-0.10.1.tgz"), data);
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
