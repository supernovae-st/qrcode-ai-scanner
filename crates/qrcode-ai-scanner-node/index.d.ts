// Report wire types live in ONE canonical file shared with the wasm
// package — see bindings/report-types.d.ts at the repo root (copied here
// as ./report-types.d.ts at pack time by scripts/sync-report-types.mjs).
export * from "./report-types";
import type { ScanReport } from "./report-types";

/** Scan profiles: quality gate · upload tool · camera frame. */
export type ScanProfile = "full" | "fast" | "frame";

export interface ScanOptions {
  /** Default: "full". */
  profile?: ScanProfile;
  /** Cancels a QUEUED task; a running scan stops at the next decode attempt. */
  signal?: AbortSignal;
  /**
   * Max accepted width/height in px (default 10000). Server deployments
   * handling untrusted uploads SHOULD lower this (e.g. 4096).
   */
  maxDimension?: number;
  /** Max accepted total pixels (default 64_000_000; servers: e.g. 16_000_000). */
  maxPixels?: number;
}

/** Async scan on the libuv pool — never blocks the event loop. */
export function scan(image: Buffer | Uint8Array, options?: ScanOptions): Promise<ScanReport>;
/** Blocking scan — scripts only. */
export function scanSync(image: Buffer | Uint8Array, options?: Omit<ScanOptions, "signal">): ScanReport;
/** Native crate version. */
export function version(): string;
