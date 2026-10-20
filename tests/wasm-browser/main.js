import { createScannerLoader, scannerCapabilities } from '@supernovae-st/qrcode-ai-scanner-wasm/auto';
const workers = [];
function startWorker(variant = 'auto', wasmUrl) {
  const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
  workers.push(worker);
  let id = 0;
  const pending = new Map();
  worker.onmessage = ({ data }) => { pending.get(data.id)?.resolve(data); pending.delete(data.id); };
  worker.onerror = () => { for (const item of pending.values()) item.resolve({ ok: false, phase: 'worker', code: 'worker_failed' }); pending.clear(); };
  return { request: (options = {}) => new Promise(resolve => {
    const next = ++id; pending.set(next, { resolve }); worker.postMessage({ ...options, id: next, variant, wasmUrl });
  }), stop: () => worker.terminate() };
}
window.harness = {
  scannerCapabilities, startWorker,
  async mainLoad(variant = 'auto') {
    const load = createScannerLoader({ variant });
    const a = load(), b = load();
    const scanner = await a;
    return { samePromise: a === b, variant: scanner.variant, version: scanner.version(), capabilities: scannerCapabilities() };
  },
  stop() { workers.forEach(worker => worker.terminate()); }
};
