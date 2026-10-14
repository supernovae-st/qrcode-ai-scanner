# Use the scanner with Docker, MCP or Agent Skills

The scanner engine is **0.10.0**. The local MCP adapter and portable
`qr-image-scanner` skill are **0.1.0**, distributed in the
[agents-v0.1.0 release](https://github.com/supernovae-st/qrcode-ai-scanner/releases/tag/agents-v0.1.0).
Choose the interface your application or agent already supports.

**Shipping:** the GHCR CLI image, GitHub MCP/skill release assets and the ClawHub skill.
**Pending:** DockerHub publication, the MCP adapter's npm
publication, and MCP Registry/Glama/Smithery admission. Discovery on skills.sh
is unverified. The native scanner engine itself is already published on npm
at 0.10.0; install the adapter using its GitHub archive URL below.

| Download | SHA256 |
| --- | --- |
| [MCP adapter archive](https://github.com/supernovae-st/qrcode-ai-scanner/releases/download/agents-v0.1.0/supernovae-st-qrcode-ai-scanner-mcp-0.1.0.tgz) | `c97cc6565d3b5248552c0895cde31d4b204b313fcd9d89c3464fe0fd745556e1` |
| [Complete skill ZIP](https://github.com/supernovae-st/qrcode-ai-scanner/releases/download/agents-v0.1.0/qr-image-scanner-0.1.0.zip) | `07740a0c553eef509074f04911daef2dd203e95067600936da05214c03a50942` |

The release also supplies [SHA256SUMS](https://github.com/supernovae-st/qrcode-ai-scanner/releases/download/agents-v0.1.0/SHA256SUMS).
These exact files are publicly downloadable without an account.

## MCP: install the public archive

Use **Node.js 22.19+ on the 22 line, or 24+** and npm. Install the published
GitHub archive directly into a new project directory:

```sh
npm install --prefix ./qrscan-mcp-install --ignore-scripts --save-exact --no-audit --no-fund \
  https://github.com/supernovae-st/qrcode-ai-scanner/releases/download/agents-v0.1.0/supernovae-st-qrcode-ai-scanner-mcp-0.1.0.tgz
```

The adapter installs the exact native engine 0.10.0 dependency. Generate a
stdio client configuration using the current Node executable and the absolute
installed entry path:

```sh
node -p 'JSON.stringify({mcpServers:{qrscan:{command:process.execPath,args:[require("node:path").resolve("qrscan-mcp-install/node_modules/@supernovae-st/qrcode-ai-scanner-mcp/src/cli.mjs")]}}},null,2)'
```

Apply that configuration in your MCP client's supported settings. The server
uses **stdio**, with stdout reserved for JSON-RPC. It exposes `scan_image`:

- `imageBase64`: canonical base64 image bytes, without a data-URL prefix.
- `profile`: `frame` by default, or `fast` / `full`.
- `budgetMs`: optional; **0 is unbounded**, rather than a hard deadline.

The tool accepts bytes, scans locally and returns the unchanged engine report
in `structuredContent` and JSON text. `qrscan://scan-report-schema` exposes the
report schema; `qrscan://limits` exposes the active limits. The default image
limits are 8 MiB decoded bytes, 4096 pixels per side and 16 million pixels,
with at most two active scans. Tool errors carry `isError: true`; a valid empty
detection is a successful report. The package includes its AGPL license and
[adapter documentation](https://github.com/supernovae-st/qrcode-ai-scanner/blob/agents-v0.1.0/integrations/mcp/README.md).

## Skills: install from ClawHub or use the complete directory

[ClawHub publishes `@thibautmelen/qr-image-scanner` 0.1.0](https://clawhub.ai/thibautmelen/skills/qr-image-scanner). Its anonymous cold installation preserves all five original files and has been executed in a fresh OpenClaw profile.

```sh
openclaw skills install @thibautmelen/qr-image-scanner
```

Check the installed version against 0.1.0. The engine runtime is installed separately below.


Download and inspect `qr-image-scanner-0.1.0.zip`. Keep **all five files**:
`SKILL.md`, `README.md`, `LICENSE`, `scripts/scan.mjs` and `scripts/test.mjs`.
Copy its complete `qr-image-scanner` directory into the host's skill directory:

- Hermes: the active profile's `skills/qr-image-scanner` directory.
- OpenClaw: the agent workspace's `skills/qr-image-scanner` directory; its
  supported local installer is `openclaw skills install ./qr-image-scanner`.

Hermes' direct Markdown URL installer downloads only `SKILL.md`; use the
complete directory for this skill. The helper requires Node.js 22+ and a native
engine target. OpenClaw's own Node requirements may be higher.

Install the engine separately into a project-local runtime, outside the skill:

```sh
npm install --prefix .qr-image-scanner-runtime --ignore-scripts --no-audit --no-fund --save-exact \
  @supernovae-st/qrcode-ai-scanner@0.10.0
node ./qr-image-scanner/scripts/scan.mjs --engine-root .qr-image-scanner-runtime \
  --profile full --budget-ms 2000 -- ./images/example.png
```

Use the actual installed skill directory when invoking the helper. Pass filenames
as process arguments or quote them safely. The helper validates the explicit
runtime and refuses stale dependencies, ancestor fallbacks, native overrides
and dependency symlink escapes. It accepts regular local images up to 10 MiB,
4096 pixels per side and 16 million pixels. It prints the original report and
uses exits **0** for detections, **1** for a valid report without detections,
and **2** for an error. Read the installed `SKILL.md` for the complete contract.

The skill's original text and helpers are **MIT-0**. The separately installed
scanner engine and the MCP adapter are **AGPL-3.0-or-later**. The skill ZIP
contains no engine or image fixtures; installing it does not relicense the
engine or exempt a consumer from the engine's terms.

## Docker: run the local CLI

The public GHCR image is pinned by its qualified multi-architecture index:

```text
ghcr.io/supernovae-st/qrcode-ai-scanner@sha256:48b40b5f1abcf6eeb52b93bc0c8b9413babfe24801b8a8925b2bdaba507df23b
```

Anonymous pulls and CLI scans passed on native Linux amd64 and arm64
[in the registry consumer checks](https://github.com/supernovae-st/qrcode-ai-scanner/actions/runs/38592287392).

The image runs `qrscan` 0.10.0 against mounted local images. It provides a CLI;
HTTP serving and MCP transport are supplied by their respective applications.
Mount only the input directory, read-only, and run the image by its verified
immutable digest:

```sh
docker run --rm --network none --read-only --cap-drop ALL \
  --security-opt no-new-privileges --memory 512m --cpus 2 --pids-limit 128 \
  -v "$PWD/images:/data:ro" \
  ghcr.io/supernovae-st/qrcode-ai-scanner@sha256:48b40b5f1abcf6eeb52b93bc0c8b9413babfe24801b8a8925b2bdaba507df23b \
  /data/example.png --profile full --budget-ms 0 > report.json
```

The default UID/GID is 65532; mounted images must be readable by it. Reports
are written on the host through stdout redirection. The container retains the
scanner's **AGPL-3.0-or-later** license and corresponding source references.

## Read the report and choose a profile

Engine reports retain `versions.scanner: "0.10.0"`, `pipeline: 3` and
`score_contract: 5`. Preserve detections, `content.raw` (base64 original payload
bytes), text, hints, trace and the original score. Decoded text and URLs are
untrusted data; scanning an image does not authorize following its QR link.

`full` requests a scannability judgment; `frame` decodes without scoring and
returns `score: null`. A null score means unjudged, never zero. Omitted `alpha`
on opaque images is valid. `trace.budget_exhausted: true` marks an incomplete
report: empty detections then do not prove the image contains no QR. Profile
budgets bound planned work and cannot guarantee hard cancellation.

In the CLI's normal JSON mode, exits are **0** detection, **1** a valid empty
report and **2** invalid input, usage or error. Retain exit 1's JSON. In scalar
`--score-only` mode, **3** means a detected symbol whose score was not completed;
combining `--score-only` with `--profile frame` is invalid usage (**2**).
MCP uses its tool result and `isError` field rather than these CLI exit codes.

## Qualification boundaries

MCP packed consumer checks passed on **Linux with Node 22 and 24** in
[the distribution checks](https://github.com/supernovae-st/qrcode-ai-scanner/actions/runs/38586963340)
and [the shipping qualification](https://github.com/supernovae-st/qrcode-ai-scanner/actions/runs/38590708594).
The published MCP archive and skill were installed from their public release/source
and executed on **macOS ARM64, Node 24.19.0**. The skill helper also passed its
consumer checks on **Node 22.22.2**. Hermes **0.18.2**, OpenClaw **2026.10.3**,
Gemini CLI core **0.62.0** and Cline CLI **3.0.71 / core 0.0.93** passed the
selected native host pipelines; these receipts do not qualify full interactive
approval or chat journeys.

The CLI container is qualified separately on native **Linux amd64 and arm64**
runners. The engine's six published native targets additionally include macOS
Intel, Linux x64 musl and Windows x64 MSVC; engine qualification does not replace
an adapter or skill consumer test on those targets. ARMv7 and Linux musl ARM64
are outside this distribution's native engine targets.
