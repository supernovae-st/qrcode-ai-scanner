// Canonical TypeScript contract for the ScanReport wire format — the ONE
// source both npm packages ship (node re-exports it; the wasm patch script
// copies it into pkg/). The Rust side of this contract is pinned by the
// insta schema snapshots in crates/qrcode-ai-scanner/src/report.rs.

export interface Point { x: number; y: number }
/**
 * Barcode symbology. The QR family (qr_code · micro_qr_code ·
 * rectangular_micro_qr_code) is the only one that can carry QrMeta
 * geometry, the UEC margin, the ISO 15415 card and the rescue stage.
 */
export type Symbology =
  | "qr_code" | "micro_qr_code" | "rectangular_micro_qr_code"
  | "data_matrix" | "aztec" | "pdf417" | "maxi_code"
  | "ean13" | "ean8" | "upc_a" | "upc_e"
  | "code128" | "code39" | "code93" | "codabar" | "itf"
  | "data_bar" | "data_bar_expanded" | "telepen";
/**
 * "rescue" = the S5 errors-and-erasures stage: a grid the engines detected
 * but could not decode, recovered by treating low-confidence (logo/texture)
 * codewords as half-price erasures (e + 2t ≤ d − p).
 */
export type EngineKind = "rxing" | "rqrr" | "rescue";
export type EcLevel = "l" | "m" | "q" | "h";
export type Charset = "utf8" | "shift_jis" | "latin1";
export type Grade = "excellent" | "good" | "acceptable" | "fair" | "poor";
export type UecGrade = "a" | "b" | "c" | "d" | "f";
export type StressAxis = "resolution" | "blur" | "contrast" | "perspective" | "rotation" | "lighting";

/** One GS1 `(AI, value)` element string. */
export interface Gs1Element { ai: string; value: string }

export type Payload =
  | { kind: "url"; url: string }
  | { kind: "wifi"; ssid: string; security: string; password: string | null; hidden: boolean }
  | { kind: "email"; to: string; subject: string | null; body: string | null }
  | { kind: "sms"; number: string; body: string | null }
  | { kind: "tel"; number: string }
  | { kind: "geo"; lat: number; lon: number }
  /**
   * GS1 element string (FNC1-in-first-position QR, symbology ]Q3/]Q4).
   * `conformant` = validator-clean against the GenSpecs subset; every
   * issue string names its violated criterion.
   */
  | { kind: "gs1"; elements: Gs1Element[]; gtin: string | null; conformant: boolean; issues: string[] }
  /**
   * GS1 Digital Link URI (DL URI Syntax 1.6 — the Sunrise-2027 retail
   * form). Still an openable URL.
   */
  | { kind: "gs1_digital_link"; url: string; elements: Gs1Element[]; gtin: string | null; conformant: boolean; issues: string[] }
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
  /**
   * Symbol is photometrically inverted (light-on-dark), measured from the
   * geometry source's decode path; null when no geometry was measured.
   */
  inverted: boolean | null;
}

export interface Detection {
  /** The symbology this detection was read as. */
  symbology: Symbology;
  content: DecodedContent;
  payload: Payload;
  corners: [Point, Point, Point, Point] | null;
  meta: QrMeta;
  engines: EngineKind[];
}

export interface AxisScore {
  axis: StressAxis;
  passed: number;
  total: number;
  /** Wire label of the first failed cell ("blur 2.5", "glare", "128px"…) — null when every cell passed. */
  failed_at: string | null;
}
export interface StructuralReport { finder_integrity: [number, number, number]; quiet_zone_ok: boolean }
export interface UecReport {
  margin: number;
  grade: UecGrade;
  worst_block_errors: number;
  worst_block_capacity: number;
}

/** ISO 15415 letter grade (4=A … 0=F). */
export type IsoGrade = "a" | "b" | "c" | "d" | "f";
/** One measured ISO 15415 parameter: raw value + grade band. */
export interface IsoParameter { value: number; grade: IsoGrade }

/**
 * ISO/IEC 15415-INFORMED grade card — the software-measurable subset.
 * Standards-based diagnostics, NOT a certified verifier grade (that
 * requires calibrated optics + ISO 15426-2 hardware conformance).
 * Grid Nonuniformity and Reflectance Margin are deliberately absent.
 */
export interface Iso15415Report {
  /** (R_high − R_low)/255 over module means. A ≥0.70 · B ≥0.55 · C ≥0.40 · D ≥0.20. */
  symbol_contrast: IsoParameter;
  /** Robust min of per-module 2·|R−GT|/SC. A ≥0.50 · B ≥0.40 · C ≥0.30 · D ≥0.20. */
  modulation: IsoParameter;
  /** |X̄−Ȳ|/mean of axis pitches. A ≤0.06 · B ≤0.08 · C ≤0.10 · D ≤0.12. */
  axial_nonuniformity: IsoParameter;
  /** Worst finder integrity; quiet-zone violation caps at D. */
  fixed_pattern_damage: IsoParameter;
  /** Synthetic UEC margin in ISO bands — null without a bitstream. */
  unused_error_correction: IsoParameter | null;
  /** The ISO rule: lowest measured parameter. */
  overall: IsoGrade;
}

export interface Score {
  value: number;
  grade: Grade;
  axes: AxisScore[];
  structural: StructuralReport | null;
  /** Synthetic ISO 15415 unused-error-correction margin. */
  uec: UecReport | null;
  /** ISO 15415-informed grade card — null when no geometry was measured. */
  iso15415: Iso15415Report | null;
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
  | { hint: "low_correction_margin"; errors: number; capacity: number }
  /**
   * The transparent design only survives on part of the background range
   * (`alpha.envelope` narrower than "any"): pin a background layer in the
   * artwork, or constrain placement to `alpha.envelope.safe_luma`.
   */
  | { hint: "alpha_background_dependent"; placement: AlphaPlacement };

/** The requested alpha-background handling (config echo). */
export type AlphaMode = "auto" | "white" | "black" | "custom";
/** Placement verdict spelled by the envelope's decoded bands. */
export type AlphaPlacement = "any" | "light_only" | "dark_only" | "mixed" | "none";

/** One neutral-background decode probe of the placement envelope. */
export interface AlphaProbe {
  /** Neutral background luma composited under the content (0-255). */
  background_luma: number;
  /** The composite still decoded to the primary symbol's text. */
  decoded: boolean;
}

/**
 * The placement envelope — "over which backgrounds does this transparent
 * design keep decoding". Probes are quick decodes in the primary symbol's
 * own decode class; every reported endpoint is a TESTED background, never
 * interpolation.
 */
export interface AlphaEnvelope {
  /** Probes ordered by background luma (5 fixed rungs + bisection refinements). */
  probes: AlphaProbe[];
  /** Contiguous decoded bands [lo, hi]. */
  safe_luma: [number, number][];
  placement: AlphaPlacement;
}

/**
 * How transparency was handled. Present ONLY when the input carried
 * transparent pixels AND `alphaBackground` was not "none" — opaque inputs
 * keep their exact pre-0.9 report bytes.
 */
export interface AlphaReport {
  /** Fraction of pixels with alpha < 255, rounded to 3 decimals. */
  coverage: number;
  /** Alpha-weighted mean BT.601 luma of the visible content. */
  content_luma: number;
  mode: AlphaMode;
  /** Background the verdict was measured over — "white" · "black" · "#rrggbb". */
  background: string;
  /** The declared flatten found nothing; the opposite background rescued the scan (auto only). */
  fallback_used: boolean;
  /** Full profile only; null when skipped, budget-exhausted, or nothing decoded. */
  envelope: AlphaEnvelope | null;
}

export interface StageTrace { stage: string; transforms_tried: number; ms: number; detections_found: number }
export interface PipelineTrace { stages: StageTrace[]; engine_panics: number; total_ms: number }
export interface Versions { scanner: string; pipeline: number; score_contract: number }

export interface ScanReport {
  /** Empty = no QR found (a valid outcome, not an error). */
  detections: Detection[];
  /** null in the frame profile. */
  score: Score | null;
  hints: Hint[];
  /** ABSENT (not null) for opaque inputs and under alphaBackground "none". */
  alpha?: AlphaReport;
  trace: PipelineTrace;
  versions: Versions;
}
