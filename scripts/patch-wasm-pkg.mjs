// wasm-pack's generated package.json needs the publishable name + metadata.
// Run from crates/qrcode-ai-scanner-wasm (CI + local publish path).
import { copyFileSync, readFileSync, writeFileSync } from "node:fs";

const path = "pkg/package.json";
const pkg = JSON.parse(readFileSync(path, "utf8"));

pkg.name = "@supernovae-st/qrcode-ai-scanner-wasm";
pkg.description =
  "QR decoding + scannability scoring for artistic and AI-generated QR codes — browser WASM (SIMD128)";
pkg.license = "AGPL-3.0-or-later";
pkg.repository = {
  type: "git",
  url: "https://github.com/supernovae-st/qrcode-ai-scanner",
};
pkg.keywords = ["qrcode", "qr", "scanner", "wasm", "browser", "artistic-qr", "ai-qr"];
// wasm-pack regenerates package.json each build — re-add the shared types
// file to the publish allowlist or the tarball ships broken TS types
pkg.files = [...new Set([...(pkg.files ?? []), "report-types.d.ts", "LICENSE"])];
// The AGPL text ships in the tarball (0.9.0 shipped none). wasm-pack copies
// LICENSE* from the crate dir when Cargo.toml sets `license`, but it rebuilds
// pkg/ from scratch every time, so the crate's copy is pinned in here.
copyFileSync("LICENSE", "pkg/LICENSE");

writeFileSync(path, `${JSON.stringify(pkg, null, 2)}\n`);
console.log(`patched ${path} → ${pkg.name}@${pkg.version}`);
