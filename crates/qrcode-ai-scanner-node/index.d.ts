// Report wire types live in ONE canonical file shared with the wasm
// package — see bindings/report-types.d.ts at the repo root (copied here
// as ./report-types.d.ts at pack time by scripts/sync-report-types.mjs).
export * from "./report-types";
import type { ScanReport, StressAxis } from "./report-types";

/** A skippable report section (`score.uec` / `score.iso15415` / `alpha.envelope`) — config-only, never serialized. */
export type ScoreCheck = "uec" | "iso15415" | "alpha_envelope";

/**
 * Background flattened under transparent pixels (config-only). `auto`
 * resolves the maximum-contrast background from the design's own content
 * and retries the opposite on a zero-detection scan; `#rrggbb` is the
 * host's real placement surface; `none` restores the pre-0.9 drop-the-
 * channel behavior (exporter-dependent verdicts — escape hatch only).
 */
export type AlphaBackgroundOption = "auto" | "white" | "black" | "none" | `#${string}`;

/** Scan profiles: quality gate · upload tool · camera frame. */
export type ScanProfile = "full" | "fast" | "frame";

export interface ScanOptions {
  /** Default: "full". */
  profile?: ScanProfile;
  /** Cancels a QUEUED task; a RUNNING scan completes within its profile/budgetMs bound. */
  signal?: AbortSignal;
  /**
   * Max accepted width/height in px (default 10000). Server deployments
   * handling untrusted uploads SHOULD lower this (e.g. 4096).
   */
  maxDimension?: number;
  /** Max accepted total pixels (default 64_000_000; servers: e.g. 16_000_000). */
  maxPixels?: number;
  /**
   * Override the profile's wall-clock budget in ms (0 = unbounded) —
   * bound tail latency without giving up the deep ladder.
   */
  budgetMs?: number;
  /**
   * Stress axes excluded from scoring, engine-side: their cells never run,
   * the composite renormalizes, `score.axes` omits them, their hints can't
   * fire. Generated-preview hosts pass `["perspective", "rotation"]`;
   * photo surfaces omit this. Unknown names reject loudly.
   */
  scoreSkipAxes?: StressAxis[];
  /**
   * Report SECTIONS excluded at the source: never computed, the wire
   * carries `null`, and the UEC-driven hints never fire. The composite
   * value does not move — this is surface truth, not score surgery. Hosts
   * that display neither the correction margin nor the ISO parameters pass
   * `["uec", "iso15415"]`. Unknown names reject loudly.
   */
  scoreSkipChecks?: ScoreCheck[];
  /**
   * Background flattened under transparent pixels (default "auto").
   * Opaque inputs are untouched whatever this is set to; transparent
   * inputs report the resolved background + placement envelope in
   * `report.alpha`. Unknown values reject loudly.
   */
  alphaBackground?: AlphaBackgroundOption;
}

/** Async scan on the libuv pool — never blocks the event loop. */
export function scan(image: Buffer | Uint8Array, options?: ScanOptions): Promise<ScanReport>;
/** Blocking scan — scripts only. */
export function scanSync(image: Buffer | Uint8Array, options?: Omit<ScanOptions, "signal">): ScanReport;
/** Native crate version. */
export function version(): string;
