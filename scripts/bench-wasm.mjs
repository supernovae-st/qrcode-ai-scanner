// WASM side of `xtask bench`: the wasm package run in Node.
//
//   node scripts/bench-wasm.mjs serve <package-dir>
//     A persistent worker for the package's `scan_image()` (the same line
//     protocol as bench-node.mjs). The module is compiled and instantiated
//     once at start; `hello` refuses a `scan_image` whose leading
//     parameters are not (bytes, profile, max_dimension, max_pixels,
//     budget_ms) — the budget is passed by position. The clock covers the
//     public call (bytes copied into linear memory, scan, report
//     conversion) and nothing else. `quit` answers `bye <linear memory
//     bytes>`: wasm memory only grows, so that is the process's heap
//     high-water mark.
//   node scripts/bench-wasm.mjs cold <package-dir> <fixture>
//     One cold start, in a fresh process per sample (V8 caches compiled
//     modules within an isolate, and compiles lazily): `ok <compile ns>
//     <instantiate ns> <first scan ns> <memory bytes> <n> (<symbology>
//     <text hex>)×n` — `new WebAssembly.Module(bytes)`, the glue's
//     `initSync({ module })`, then the first scan of the fixture (full,
//     unbounded), where the code generation lazy compilation deferred is
//     paid.
//   node scripts/bench-wasm.mjs sizes <package-dir>
//     `ok <raw> <gzip-9> <brotli-11> <glue bytes>` of the packed .wasm,
//     compressed with Node zlib (gzip level 9, brotli quality 11).
//
// <package-dir> holds the glue named by package.json `main` (default
// `qrcode-ai-scanner.js`) and its `_bg.wasm` — the published npm layout.
// Every command leaves within about 50 ms once its parent changes, even
// inside a synchronous scan (bench-node.mjs exitWithParent).
import { existsSync, readFileSync, statSync } from "node:fs";
import { relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";
import { PROTOCOL, budgetMs, exitWithParent, failReply, serve, timed } from "./bench-node.mjs";

/** The leading `scan_image` parameters the worker passes by position. */
const SIGNATURE = ["bytes", "profile", "max_dimension", "max_pixels", "budget_ms"];

function locate(dir) {
  const manifest = resolve(dir, "package.json");
  const main = existsSync(manifest)
    ? (JSON.parse(readFileSync(manifest, "utf8")).main ?? "qrcode-ai-scanner.js")
    : "qrcode-ai-scanner.js";
  const glue = resolve(dir, main);
  return { glue, wasm: glue.replace(/\.js$/, "_bg.wasm") };
}

async function load(dir) {
  const { glue, wasm } = locate(dir);
  const bytes = readFileSync(wasm);
  return { api: await import(pathToFileURL(glue).href), bytes, wasm };
}

/** `null` when `scan_image` takes the expected leading parameters. */
function signatureProblem(api) {
  if (typeof api.scan_image !== "function") return "no scan_image export";
  const text = Function.prototype.toString.call(api.scan_image);
  const params = text
    .slice(text.indexOf("(") + 1, text.indexOf(")"))
    .split(",")
    .map((p) => p.trim());
  return SIGNATURE.every((name, i) => params[i] === name)
    ? null
    : `scan_image takes (${params.slice(0, SIGNATURE.length).join(", ")}), expected (${SIGNATURE.join(", ")})`;
}

async function serveWasm(dir) {
  const { api, bytes, wasm } = await load(dir);
  const problem = signatureProblem(api);
  const exports = api.initSync({ module: new WebAssembly.Module(bytes) });
  await serve({
    hello: () =>
      problem
        ? failReply(problem)
        : `hello\t${PROTOCOL}\t${api.version()}\t-\t-\twasm-node\t-\t${relative(resolve(dir), wasm)}`,
    scan: async (profile, budget, _memory, path) => {
      const ms = budgetMs(budget);
      const input = readFileSync(path);
      return timed(() => api.scan_image(input, profile, undefined, undefined, ms));
    },
    farewell: () => `bye\t${exports.memory.buffer.byteLength}`,
  });
}

async function cold(dir, fixture) {
  const { api, bytes } = await load(dir);
  const problem = signatureProblem(api);
  if (problem) throw new Error(problem);
  const input = readFileSync(fixture);
  const started = process.hrtime.bigint();
  const module = new WebAssembly.Module(bytes);
  const compiled = process.hrtime.bigint();
  const exports = api.initSync({ module });
  const ready = process.hrtime.bigint();
  const report = api.scan_image(input, "full", undefined, undefined, 0);
  const scanned = process.hrtime.bigint();
  const parts = [
    "ok",
    compiled - started,
    ready - compiled,
    scanned - ready,
    exports.memory.buffer.byteLength,
    report.detections.length,
  ];
  for (const detection of report.detections) {
    parts.push(detection.symbology, Buffer.from(detection.content.text, "utf8").toString("hex"));
  }
  console.log(parts.join("\t"));
}

function sizes(dir) {
  const { glue, wasm } = locate(dir);
  const bytes = readFileSync(wasm);
  const gzip = gzipSync(bytes, { level: 9 }).length;
  const brotli = brotliCompressSync(bytes, {
    params: { [constants.BROTLI_PARAM_QUALITY]: 11 },
  }).length;
  console.log(`ok\t${bytes.length}\t${gzip}\t${brotli}\t${statSync(glue).size}`);
}

const [command, dir, ...rest] = process.argv.slice(2);
const commands = { serve: [serveWasm, 0], cold: [cold, 1], sizes: [sizes, 0] };
if (!commands[command] || !dir || rest.length !== commands[command][1]) {
  console.error("usage: bench-wasm.mjs serve|sizes <package-dir> | cold <package-dir> <fixture>");
  process.exit(2);
}
// every command runs under the harness: it leaves when its parent does
exitWithParent();
try {
  await commands[command][0](dir, ...rest);
} catch (error) {
  console.error(`bench-wasm: ${error?.message ?? error}`);
  process.exit(2);
}
