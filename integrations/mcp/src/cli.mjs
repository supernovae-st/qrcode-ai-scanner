#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-or-later
import { serveStdio, StdioServerTransport } from "@modelcontextprotocol/server/stdio";
import { createScannerServer, limitsFromEnvironment } from "./server.mjs";

try {
  if (process.argv.length !== 2) throw new Error("No command-line arguments are supported; configure limits through QRSCAN_MCP_* environment variables");
  if (process.env.NAPI_RS_NATIVE_LIBRARY_PATH) throw new Error("NAPI_RS_NATIVE_LIBRARY_PATH overrides are unsupported; use the pinned registry native package");
  process.env.NAPI_RS_ENFORCE_VERSION_CHECK = "1";
  const limits = limitsFromEnvironment();
  const transport = new StdioServerTransport(process.stdin, process.stdout, { maxBufferSize: 12 * 1024 * 1024 });
  const handle = serveStdio(() => createScannerServer(limits), { legacy: "serve", transport, maxSubscriptions: 4, onerror: () => process.stderr.write("qrscan-mcp: MCP transport error\n") });
  // The SDK closes its transport abstraction without destroying Node's pipe.
  // End the actual IO too, so a peer holding stdin open cannot keep it alive.
  const sdkClose = transport.onclose;
  transport.onclose = () => { sdkClose?.(); process.stdin.destroy(); process.stdout.end(); };
  const sdkError = transport.onerror;
  transport.onerror = (error) => {
    process.exitCode = 1;
    sdkError?.(error);
    handle.close().catch(() => { process.stdin.destroy(); process.stdout.end(); });
  };
  for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => { handle.close().catch(() => { process.exitCode = 1; }); });
} catch (error) {
  process.stderr.write(`qrscan-mcp: ${error.message}\n`);
  process.exitCode = 1;
}
