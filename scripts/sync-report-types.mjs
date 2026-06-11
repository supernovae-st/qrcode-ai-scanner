// Copy the canonical report-types contract into both npm packages and
// retype the wasm scan functions from `any` to `ScanReport`. Run from the
// repo root (local + CI, before `npm pack`/`npm publish` of either pkg).
import { copyFileSync, readFileSync, writeFileSync, existsSync } from "node:fs";

const CANON = "bindings/report-types.d.ts";

// node package: plain copy next to index.d.ts (which re-exports it)
copyFileSync(CANON, "crates/qrcode-ai-scanner-node/report-types.d.ts");
console.log("synced → crates/qrcode-ai-scanner-node/report-types.d.ts");

// wasm package: copy into pkg/ + retype the two scan functions
const wasmDts = "crates/qrcode-ai-scanner-wasm/pkg/qrcode-ai-scanner.d.ts";
if (existsSync(wasmDts)) {
  copyFileSync(CANON, "crates/qrcode-ai-scanner-wasm/pkg/report-types.d.ts");
  let dts = readFileSync(wasmDts, "utf8");
  if (!dts.includes("./report-types")) {
    dts = dts.replace(
      "/* eslint-disable */",
      '/* eslint-disable */\nexport * from "./report-types";\nimport type { ScanReport } from "./report-types";'
    );
  }
  dts = dts
    .replace(
      /export function scan_frame\(([^)]*)\): any;/,
      "export function scan_frame($1): ScanReport;"
    )
    .replace(
      /export function scan_image\(([^)]*)\): any;/,
      "export function scan_image($1): ScanReport;"
    );
  writeFileSync(wasmDts, dts);
  console.log(`synced + retyped → ${wasmDts}`);
}
