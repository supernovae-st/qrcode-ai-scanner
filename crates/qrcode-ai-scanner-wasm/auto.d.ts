export type ScannerVariant = 'simd' | 'compatible';
export type ScannerInitCode = 'invalid_options' | 'wasm_unavailable' | 'simd_unavailable' | 'wasm_features_unsupported'
  | 'loader_import_failed' | 'asset_network_failed' | 'asset_http_failed'
  | 'asset_read_failed' | 'binary_invalid' | 'wasm_validation_failed' | 'wasm_compile_failed'
  | 'wasm_instantiate_failed';
export type LoadedScanner = typeof import('./qrcode-ai-scanner.js') & { readonly variant: ScannerVariant };
export interface ScannerLoaderOptions { variant?: 'auto' | ScannerVariant; wasmUrl?: string | URL; }
export declare class ScannerInitError extends Error {
  readonly code: ScannerInitCode;
  readonly variant: ScannerVariant | null;
  readonly status?: number;
  readonly cause?: unknown;
  constructor(code: ScannerInitCode, variant: ScannerVariant | null, cause?: unknown, status?: number);
}
export declare function scannerCapabilities(): { wasm: boolean; simd: boolean; baseline: boolean };
export declare function createScannerLoader(options?: ScannerLoaderOptions): () => Promise<LoadedScanner>;
export declare const loadScanner: () => Promise<LoadedScanner>;
