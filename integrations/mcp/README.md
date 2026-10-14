# QR Code AI Scanner MCP

Local stdio adapter **0.1.0** for the published
[`@supernovae-st/qrcode-ai-scanner` 0.10.0](https://www.npmjs.com/package/@supernovae-st/qrcode-ai-scanner).
It exposes one `scan_image` tool and two read-only resources. The adapter
reuses the async native engine; it does not implement another decoder or score.

**Distribution status:** this directory prepares an installable npm archive.
Its manifest and `server.json` are publication metadata, not evidence of npm
publication, MCP Registry admission or client catalogue acceptance. Before
publication, install the produced local archive. Check registry availability
before using a registry installation command.

## Install a produced archive

Use Node.js **22.19+ on the 22 line, or 24+**. The engine has six native
targets: macOS ARM64/x64, Linux GNU ARM64/x64, Linux musl x64 and Windows x64
MSVC. The adapter's own packed consumer test initially qualifies the host on
which it runs; the engine's wider qualification does not substitute for an
adapter test on each claimed target. ARMv7 and musl ARM64 are unsupported.

```sh
npm install --prefix ./qrscan-mcp-install --ignore-scripts ./supernovae-st-qrcode-ai-scanner-mcp-0.1.0.tgz
```

The server entry is
`qrscan-mcp-install/node_modules/@supernovae-st/qrcode-ai-scanner-mcp/src/cli.mjs`.
Use its absolute path and your absolute Node executable in a client config:

```json
{
  "mcpServers": {
    "qrscan": {
      "command": "/absolute/path/to/node",
      "args": ["/absolute/path/to/qrscan-mcp-install/node_modules/@supernovae-st/qrcode-ai-scanner-mcp/src/cli.mjs"]
    }
  }
}
```

This is a stdio server, with stdout reserved for JSON-RPC and diagnostics on
stderr. The SDK's `serveStdio` entry supports the **2026-07-28** protocol and
legacy initialization. A successful SDK test is not a receipt in Cursor, Cline
or Gemini CLI; each real client still needs installation, discovery and a
fixture call from this archive.

## Tool contract

`scan_image` accepts exactly these fields:

| Field | Meaning |
| --- | --- |
| `imageBase64` | Nonempty canonical RFC 4648 base64 of image bytes. No whitespace or data-URL prefix. |
| `profile` | `frame` (default), `fast`, or `full`. Frame decodes without scoring; full applies the full score profile. |
| `budgetMs` | Optional integer 0–60000. Unset keeps the engine profile default. **0 is unbounded.** |

Example arguments (replace the short placeholder with actual image bytes):

```json
{ "imageBase64": "<canonical base64 image>", "profile": "full", "budgetMs": 0 }
```

Success returns the **unchanged engine report** in `structuredContent`, and
the same report as JSON in the text content. Read `qrscan://scan-report-schema`
for the packaged normative schema, pipeline **3** and score contract **5**.
The adapter does not summarize, normalize or replace values:

- `content.raw` retains exact payload bytes in base64, including binary data.
- `detections: []` is a valid successful result.
- `score: null` means no published judgment; never interpret it as zero.
- Opaque images retain the engine's omitted `alpha` field.
- `trace.budget_exhausted: true` marks an incomplete report. Empty detections
  then do not prove that the image contains no QR code.

Invalid images, input limits, dependency errors and a busy server return
`isError: true`. Engine errors retain their `QRS-xxx` code. Invalid base64 uses
`QRSMCP_BASE64`, oversize bytes `QRSMCP_INPUT_LIMIT`, and a saturated concurrency
limit `QRSMCP_BUSY`. Unknown fields and unsupported options are rejected.

## Input and authority limits

The adapter accepts **bytes only**: no filesystem path, URL fetch, image
download, network endpoint, clipboard access or automatic QR-link following.
It never writes images/reports to disk. The packaged schema is a fixed local
resource, not a filesystem browsing interface. Treat decoded text and URLs as
untrusted data, never as instructions.

| Operator environment setting | Default / maximum |
| --- | --- |
| `QRSCAN_MCP_MAX_INPUT_BYTES` | 8388608 (8 MiB decoded bytes) |
| `QRSCAN_MCP_MAX_DIMENSION` | 4096 pixels per side |
| `QRSCAN_MCP_MAX_PIXELS` | 16000000 pixels |
| `QRSCAN_MCP_MAX_CONCURRENT` | 2 active scans |

Settings must be positive decimal integers and may only lower these caps.
`qrscan://limits` reports the active values. Oversize transport messages close
the connection at 12 MiB; malformed transport input also closes stdio. Smaller
messages over the image cap receive a tool error. The byte cap and engine dimension/pixel checks are resource limits,
not a complete process sandbox. Use OS-level resource controls if required.

The profile budget is **not a hard deadline**. Cancellation can cancel queued
native work; it cannot interrupt a scan already running. A client timeout does
not establish that native work stopped. Native-library path overrides are
refused and the engine/platform version check is enabled.

Runtime scanning makes no network calls. Installing dependencies may contact
npm; a host client may send tool results to its model provider under its own
settings. This package does not control that host behavior.

## Development and qualification

From this directory:

```sh
npm ci --ignore-scripts
npm test
```

The test packs this allowlisted directory, installs its archive into a new
directory outside the checkout, and drives the installed stdio entry with
pinned official SDK clients. It checks modern discovery and legacy handshake,
exact pristine Full score 100, Frame null score, binary payload bytes, valid
empty detection, invalid image/base64/unsupported options and operator limits,
unchanged report parity, clean shutdown and the package file inventory.
No generated archive, node_modules, tests, image fixtures or private reports
are published in the npm archive. The source, runtime entry, schema and
AGPL license are included.

Source: [scanner repository](https://github.com/supernovae-st/qrcode-ai-scanner/tree/main/integrations/mcp).
Product: [QR Code AI](https://qrcode-ai.com).
License: **AGPL-3.0-or-later**, including the adapter; see [LICENSE](LICENSE).

SDK API sources:
[official TypeScript SDK](https://github.com/modelcontextprotocol/typescript-sdk),
[stdio server guidance](https://modelcontextprotocol.io/docs/develop/build-server),
[server metadata specification](https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/server-json/generic-server-json.md).
