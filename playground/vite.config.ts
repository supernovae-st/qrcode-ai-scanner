import { defineConfig } from 'vite';

// The wasm package ships a `--target web` build whose loader resolves the
// `.wasm` via `new URL(..., import.meta.url)`. esbuild's dep pre-bundling
// rewrites that URL and breaks resolution — so EXCLUDE it (the exact gotcha
// Nuxt/Vite consumers hit; documented in GUIDE-NICOLAS + docs/guides/nuxt-verify).
export default defineConfig({
  optimizeDeps: {
    exclude: ['@supernovae-st/qrcode-ai-scanner-wasm'],
  },
  server: {
    // allow serving the wasm asset from the linked package (outside playground/)
    fs: { allow: ['..'] },
  },
});
