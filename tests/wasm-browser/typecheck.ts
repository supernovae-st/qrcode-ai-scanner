import type { ScanReport } from '@supernovae-st/qrcode-ai-scanner-wasm';
import { loadScanner, createScannerLoader, ScannerInitError, type ScannerVariant } from '@supernovae-st/qrcode-ai-scanner-wasm/auto';
const scanner = await loadScanner();
const variant: ScannerVariant = scanner.variant;
const report: ScanReport = scanner.scan_image(new Uint8Array(), 'full');
const compatible = createScannerLoader({ variant: 'compatible', wasmUrl: new URL('/scanner.wasm', 'https://example.invalid') });
const error = new ScannerInitError('asset_http_failed', variant, undefined, 404);
void [report, compatible, error.code, error.status];
