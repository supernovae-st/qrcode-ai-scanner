import { createScannerLoader, scannerCapabilities } from '@supernovae-st/qrcode-ai-scanner-wasm/auto';
let load;
self.onmessage = async ({ data }) => {
  let phase = 'initialization';
  try {
    load ??= createScannerLoader({ variant: data.variant ?? 'auto', wasmUrl: data.wasmUrl });
    const scanner = await load();
    phase = 'scan';
    const result = data.bytes
      ? scanner.scan_image(new Uint8Array(data.bytes), 'full', undefined, undefined, data.budgetMs ?? 0,
        data.skipAxes, data.skipChecks)
      : null;
    self.postMessage({ id: data.id, ok: true, result, variant: scanner.variant,
      version: scanner.version(), capabilities: scannerCapabilities() });
  } catch (error) {
    self.postMessage({ id: data.id, ok: false, phase, code: error.code ?? null,
      variant: error.variant ?? null, name: error.name, message: error.message,
      status: error.status ?? null,
      nativeError: error.cause ? { name: error.cause.name, message: error.cause.message } : null });
  }
};
