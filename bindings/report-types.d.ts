// Canonical TypeScript contract for the ScanReport wire format — the ONE
// source both npm packages ship (node re-exports it; the wasm patch script
// copies it into pkg/). The Rust side of this contract is pinned by the
// insta schema snapshots in crates/qrcode-ai-scanner/src/report.rs.

export interface Point { x: number; y: number }
export type EngineKind = "rxing" | "rqrr";
export type EcLevel = "l" | "m" | "q" | "h";
export type Charset = "utf8" | "shift_jis" | "latin1";
export type Grade = "excellent" | "good" | "acceptable" | "fair" | "poor";
export type UecGrade = "a" | "b" | "c" | "d" | "f";
export type StressAxis = "resolution" | "blur" | "contrast" | "perspective" | "rotation" | "lighting";

export type Payload =
  | { kind: "url"; url: string }
  | { kind: "wifi"; ssid: string; security: string; password: string | null; hidden: boolean }
  | { kind: "email"; to: string; subject: string | null; body: string | null }
  | { kind: "sms"; number: string; body: string | null }
  | { kind: "tel"; number: string }
  | { kind: "geo"; lat: number; lon: number }
  | { kind: "me_card"; name: string | null; tel: string | null; email: string | null; url: string | null }
  | { kind: "crypto"; scheme: string; address: string; amount: string | null }
  | { kind: "v_card"; raw: string }
  | { kind: "v_event"; raw: string }
  | { kind: "text" };

export interface DecodedContent {
  text: string;
  /** Original payload bytes, base64. */
  raw: string;
  charset: Charset;
}

export interface QrMeta {
  version: number | null;
  ec_level: EcLevel | null;
  mask: number | null;
  modules: number | null;
  mirrored: boolean | null;
  inverted: boolean | null;
}

export interface Detection {
  content: DecodedContent;
  payload: Payload;
  corners: [Point, Point, Point, Point] | null;
  meta: QrMeta;
  engines: EngineKind[];
}

export interface AxisScore { axis: StressAxis; passed: number; total: number }
export interface StructuralReport { finder_integrity: [number, number, number]; quiet_zone_ok: boolean }
export interface UecReport {
  margin: number;
  grade: UecGrade;
  worst_block_errors: number;
  worst_block_capacity: number;
}

export interface Score {
  value: number;
  grade: Grade;
  axes: AxisScore[];
  structural: StructuralReport | null;
  /** Synthetic ISO 15415 unused-error-correction margin. */
  uec: UecReport | null;
}

export type Hint =
  | { hint: "raise_error_correction"; current: EcLevel }
  | { hint: "increase_contrast" }
  | { hint: "enlarge_modules" }
  | { hint: "fix_finder_pattern"; corner: number }
  | { hint: "restore_quiet_zone" }
  | { hint: "reduce_art_texture" }
  /**
   * The decode consumed the worst RS block's ENTIRE correction budget
   * (UEC margin 0) — the classic miscorrection signature. Treat the
   * decoded content as unverified.
   */
  | { hint: "low_correction_margin"; errors: number; capacity: number };

export interface StageTrace { stage: string; transforms_tried: number; ms: number; detections_found: number }
export interface PipelineTrace { stages: StageTrace[]; engine_panics: number; total_ms: number }
export interface Versions { scanner: string; pipeline: number; score_contract: number }

export interface ScanReport {
  /** Empty = no QR found (a valid outcome, not an error). */
  detections: Detection[];
  /** null in the frame profile. */
  score: Score | null;
  hints: Hint[];
  trace: PipelineTrace;
  versions: Versions;
}
