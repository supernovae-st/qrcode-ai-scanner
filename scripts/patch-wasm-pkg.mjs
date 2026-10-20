// wasm-pack's generated package.json needs the publishable name + metadata.
// Run from crates/qrcode-ai-scanner-wasm (CI + local publish path).
import { copyFileSync, readFileSync, writeFileSync, unlinkSync } from "node:fs";

const path = "pkg/package.json";
const pkg = JSON.parse(readFileSync(path, "utf8"));

pkg.name = "@supernovae-st/qrcode-ai-scanner-wasm";
pkg.description =
  "QR decoding + scannability scoring — browser WASM with automatic SIMD and compatible loading";
pkg.license = "AGPL-3.0-or-later";
pkg.repository = {
  type: "git",
  url: "https://github.com/supernovae-st/qrcode-ai-scanner",
};
pkg.keywords = ["qrcode", "qr", "scanner", "wasm", "browser", "artistic-qr", "ai-qr"];
// wasm-pack regenerates package.json each build — re-add the shared types
// file to the publish allowlist or the tarball ships broken TS types
pkg.files = ["qrcode-ai-scanner.js", "qrcode-ai-scanner.d.ts", "qrcode-ai-scanner_bg.wasm",
  "compatible/qrcode-ai-scanner.js", "compatible/qrcode-ai-scanner.d.ts", "compatible/qrcode-ai-scanner_bg.wasm",
  "compatible/report-types.d.ts", "auto.js", "auto.d.ts", "report-types.d.ts", "LICENSE"];
pkg.exports = {
  ".": { types: "./qrcode-ai-scanner.d.ts", import: "./qrcode-ai-scanner.js", default: "./qrcode-ai-scanner.js" },
  "./auto": { types: "./auto.d.ts", import: "./auto.js", default: "./auto.js" },
  "./qrcode-ai-scanner_bg.wasm": "./qrcode-ai-scanner_bg.wasm",
  "./compatible/qrcode-ai-scanner_bg.wasm": "./compatible/qrcode-ai-scanner_bg.wasm",
  "./package.json": "./package.json",
  "./*": "./*",
};
for (const file of ["auto.js", "auto.d.ts"]) copyFileSync(file, `pkg/${file}`);
// The nested wasm-pack manifest would create a second package boundary and
// README/license copies. The root archive owns both variants and its license.
for (const file of ["package.json", "README.md", "LICENSE"]) unlinkSync(`pkg/compatible/${file}`);
// The AGPL text ships in the tarball (0.9.0 shipped none). wasm-pack copies
// LICENSE* from the crate dir when Cargo.toml sets `license`, but it rebuilds
// pkg/ from scratch every time, so the crate's copy is pinned in here.
copyFileSync("LICENSE", "pkg/LICENSE");

writeFileSync(path, `${JSON.stringify(pkg, null, 2)}\n`);
console.log(`patched ${path} → ${pkg.name}@${pkg.version}`);
