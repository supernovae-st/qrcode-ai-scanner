// Node (napi) side of `xtask bench`, plus the helpers bench-wasm.mjs shares.
//
//   node scripts/bench-node.mjs serve <package-dir>
//     A persistent worker for the package's public `scan()` (index.js →
//     native addon), speaking the line protocol of xtask/src/bench/worker.rs
//     on stdin/stdout: `hello`, `scan <profile> <budget> <memory> <path>`,
//     `quit`. `hello` names the addon the package actually loaded and
//     refuses one from outside the package directory (an installed
//     platform package, or a library override). The clock covers the
//     awaited public call (task dispatch, scan, JSON.parse) and nothing
//     else: the file is read before it, the reply built after it, and a
//     report of an unexpected shape is a protocol `fail`, never a timed
//     error.
//   node scripts/bench-node.mjs unpack <archive.tgz> <dest-dir> [--strip <prefix>] [--only <name>]
//     Extract regular files of an npm tarball (or a .crate) — zlib + ustar,
//     no dependency; refuses any entry that would land outside <dest-dir>.
//
// Decoded text leaves this process only as hex on the protocol pipe; the
// orchestrator hashes it on arrival. Failure reasons carry error codes,
// never messages (a message can carry a path). Nothing is written but
// unpacked files.
//
// Every harness-run command leaves within about 50 ms once its parent
// changes (exitWithParent), even inside a synchronous scan: a harness
// killed outright never leaves it running.
import { createRequire } from "node:module";
import { mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, resolve, sep } from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";
import { Worker } from "node:worker_threads";
import { gunzipSync } from "node:zlib";

/** The worker protocol (xtask/src/bench/worker.rs `PROTOCOL`). */
export const PROTOCOL = "qrscan-bench-worker/3";

/** The parent the harness tells its children to keep (worker.rs `PARENT_ENV`). */
const PARENT_ENV = "QRSCAN_BENCH_PARENT";

/**
 * Leave at once when the parent is not the one to keep (`PARENT_ENV`, else
 * the parent at start): checked now, then every 50 ms on a worker thread, so
 * a scan blocking the main thread cannot hold it up. SIGKILL to itself ends
 * the whole process from any thread.
 */
export function exitWithParent() {
  const keep = Number.parseInt(process.env[PARENT_ENV] ?? "", 10) || process.ppid;
  if (process.ppid !== keep) process.kill(process.pid, "SIGKILL");
  const watcher = new Worker(
    `const { workerData } = require("node:worker_threads");
     setInterval(() => {
       if (process.ppid !== workerData) process.kill(process.pid, "SIGKILL");
     }, 50);`,
    { eval: true, workerData: keep },
  );
  watcher.unref();
}

/** A failure of the harness's own protocol: its message is safe to send. */
export class ProtocolError extends Error {}

/** One protocol field: no tab or newline may leak into a reply. */
const field = (value) => String(value).replace(/[\t\r\n]/g, " ");

/**
 * The judgment signature of a report's `score` — the string worker.rs
 * `judgment()` builds in-process: `v<composite>w<weights run>` then, per
 * axis, `,<passed>/<total>` with `x` when a failing cell is named; `-`
 * without a score. Throws a ProtocolError on a score of another shape.
 */
export function judgment(score) {
  if (score == null) return "-";
  if (typeof score.value !== "number" || !Array.isArray(score.axes)) {
    throw new ProtocolError("unexpected score shape");
  }
  let out = `v${score.value}w${score.weights_run ?? 0}`;
  for (const axis of score.axes) {
    if (typeof axis?.passed !== "number" || typeof axis?.total !== "number") {
      throw new ProtocolError("unexpected axis shape");
    }
    out += `,${axis.passed}/${axis.total}${axis.failed_at == null ? "" : "x"}`;
  }
  return out;
}

/**
 * `ok` reply for a ScanReport-shaped object: timing, trace, the walk
 * (stages, Σ transforms_tried, the judgment signature — compared with the
 * image's unbudgeted reference walk by the orchestrator), detections as
 * hex. Heap fields are in-process only. Throws a ProtocolError on any
 * other shape.
 */
export function okReply(ns, report) {
  const stages = report?.trace?.stages;
  if (!Array.isArray(stages) || !Array.isArray(report?.detections)) {
    throw new ProtocolError("unexpected report shape");
  }
  let transforms = 0;
  for (const stage of stages) {
    if (typeof stage?.transforms_tried !== "number") {
      throw new ProtocolError("unexpected stage shape");
    }
    transforms += stage.transforms_tried;
  }
  const parts = [
    "ok",
    ns,
    report.trace.total_ms,
    report.trace.engine_panics,
    "-",
    "-",
    "-",
    "-",
    stages.length,
    transforms,
    judgment(report.score),
    report.detections.length,
  ];
  for (const detection of report.detections) {
    if (typeof detection?.symbology !== "string" || typeof detection?.content?.text !== "string") {
      throw new ProtocolError("unexpected detection shape");
    }
    parts.push(detection.symbology, Buffer.from(detection.content.text, "utf8").toString("hex"));
  }
  return parts.join("\t");
}

/** `error` reply: the call failed; keep its `QRS-xxx` code, never its message. */
export function errorReply(ns, error) {
  const code = /\[(QRS-\d+)\]/.exec(String(error?.message ?? error))?.[1] ?? "exception";
  return `error\t${ns}\t${code}`;
}

export const failReply = (reason) => `fail\t${field(reason)}`;

/** A failure reason that cannot carry a path or decoded text. */
export const reasonOf = (error) =>
  error instanceof ProtocolError ? error.message : String(error?.code ?? error?.name ?? "error");

/** The budget field → the bindings' `budgetMs` (0 = unbounded, as every binding maps it). */
export function budgetMs(budget) {
  if (budget === "default") return undefined;
  if (budget === "unbounded") return 0;
  throw new ProtocolError(`unknown budget ${field(budget)}`);
}

/**
 * Time one call of the public API: the clock covers `call` only; the reply
 * is built after it, outside the measured region.
 */
export async function timed(call) {
  let report;
  let thrown;
  let threw = false;
  const started = process.hrtime.bigint();
  try {
    report = await call();
  } catch (error) {
    threw = true;
    thrown = error;
  }
  const ns = process.hrtime.bigint() - started;
  return threw ? errorReply(ns, thrown) : okReply(ns, report);
}

/** Read request lines until `quit`; `scan(profile, budget, memory, path)` returns a reply. */
export async function serve({ hello, scan, farewell }) {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  for await (const line of lines) {
    const [verb, ...fields] = line.split("\t");
    if (verb === "quit") {
      process.stdout.write(`${farewell()}\n`, () => process.exit(0));
      return;
    }
    let reply;
    try {
      if (verb === "hello") {
        reply = hello();
      } else if (verb === "scan" && fields.length === 4) {
        reply = await scan(...fields);
      } else {
        reply = failReply("unknown request");
      }
    } catch (error) {
      reply = failReply(reasonOf(error));
    }
    process.stdout.write(`${reply}\n`);
  }
}

/**
 * The one native addon the process loaded, if it lives in `dir`: its path
 * inside the package, which the harness compares with the file it hashed.
 */
function loadedAddon(require, dir) {
  const root = realpathSync(dir) + sep;
  const loaded = Object.keys(require.cache).filter((key) => key.endsWith(".node"));
  if (loaded.length !== 1) return { error: `${loaded.length} native addons loaded` };
  if (!loaded[0].startsWith(root)) return { error: "the addon loaded from outside the package" };
  return { name: loaded[0].slice(root.length) };
}

async function serveNode(dir) {
  const require = createRequire(import.meta.url);
  const pkg = require(resolve(dir, "index.js"));
  const addon = loadedAddon(require, dir);
  await serve({
    hello: () =>
      addon.error
        ? failReply(addon.error)
        : `hello\t${PROTOCOL}\t${pkg.version()}\t-\t-\tnode-napi\t-\t${addon.name}`,
    scan: async (profile, budget, _memory, path) => {
      const options = { profile };
      const ms = budgetMs(budget);
      if (ms !== undefined) options.budgetMs = ms;
      const bytes = readFileSync(path);
      return timed(() => pkg.scan(bytes, options));
    },
    farewell: () => "bye",
  });
}

/** Extract regular files from a gzipped tar (ustar, GNU long names, pax paths). */
export function unpack(archive, dest, { strip = "", only = null } = {}) {
  const tar = gunzipSync(readFileSync(archive));
  const root = resolve(dest);
  const text = (header, start, length) => {
    const raw = header.subarray(start, start + length);
    const end = raw.indexOf(0);
    return raw.subarray(0, end === -1 ? length : end).toString("utf8");
  };
  let offset = 0;
  let longName = null;
  let written = 0;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const size = parseInt(text(header, 124, 12).trim() || "0", 8);
    const type = String.fromCharCode(header[156] || 0x30);
    const prefix = text(header, 257, 6).startsWith("ustar") ? text(header, 345, 155) : "";
    let name = longName ?? (prefix ? `${prefix}/${text(header, 0, 100)}` : text(header, 0, 100));
    longName = null;
    const body = tar.subarray(offset + 512, offset + 512 + size);
    offset += 512 + Math.ceil(size / 512) * 512;
    if (type === "L") {
      longName = body.toString("utf8").replace(/\0+$/, "");
      continue;
    }
    if (type === "x") {
      longName = /\d+ path=([^\n]*)\n/.exec(body.toString("utf8"))?.[1] ?? null;
      continue;
    }
    if (type !== "0") continue;
    if (!name.startsWith(strip)) continue;
    name = name.slice(strip.length);
    if (only !== null && name !== only) continue;
    const target = resolve(root, name);
    if (!target.startsWith(root + sep)) throw new Error(`entry outside the destination: ${name}`);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, body);
    written += 1;
  }
  return written;
}

async function main(args) {
  const [command, ...rest] = args;
  if (command === "serve" && rest.length === 1) {
    exitWithParent();
    return serveNode(rest[0]);
  }
  if (command === "unpack" && rest.length >= 2) {
    const [archive, dest, ...flags] = rest;
    const options = {};
    for (let i = 0; i < flags.length; i += 2) {
      if (flags[i] === "--strip") options.strip = flags[i + 1];
      else if (flags[i] === "--only") options.only = flags[i + 1];
      else throw new Error(`unknown flag ${flags[i]}`);
    }
    console.log(`unpacked ${unpack(archive, dest, options)} files into ${dest}`);
    return undefined;
  }
  throw new Error(
    "usage: bench-node.mjs serve <package-dir> | unpack <archive> <dest> [--strip <prefix>] [--only <name>]",
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  // stderr reaches the operator's terminal or the harness's drain, which
  // keeps no text
  main(process.argv.slice(2)).catch((error) => {
    console.error(`bench-node: ${error?.message ?? error}`);
    process.exit(2);
  });
}
