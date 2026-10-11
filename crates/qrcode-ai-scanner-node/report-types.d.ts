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
/**
 * How content.text relates to content.raw (spec/01 § Content). utf8 · shift_jis · latin1 · gb18030 (latin1 = WHATWG
 * windows-1252, gb18030 = WHATWG gb18030): text is EXACTLY raw decoded with that decoder. eci: raw decoded under
 * content.eci. unknown: NOT a complete decoding (each U+FFFD = one unread byte or maximal ill-formed sequence; use raw).
 * "unknown" is the ONLY lossy value, now and later: any other value, even one newer than these types, is a complete
 * decoding the scanner checked (the declared interpretation only when content.eci lists it). Test === "unknown".
 */
export type Charset = "utf8" | "shift_jis" | "latin1" | "gb18030" | "eci" | "unknown";
/** FNC1 position: first = GS1 mode; second = AIM application mode (raw starts with the application indicator). */
export type Fnc1Position = "first" | "second";
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

/** One ECI designator observed in the symbol data (ISO/IEC 18004 §7.4.2). */
export interface EciSegment {
  /** ECI assignment number as encoded, 0–999999. */
  designator: number;
  /** Byte offset into raw where it takes effect (until the next entry's start, or the end). */
  start: number;
}

/** One exact reading of a symbol's data. Compare by identity (symbology, raw, fnc1, compatible eci), never text. */
export interface DecodedContent {
  /** raw decoded as charset states; empty if and only if raw is empty. */
  text: string;
  /**
   * Payload bytes, base64. QR family: the original bytes; other symbologies: the bytes of the engine's reading
   * (under an ECI it can omit a segment the engine could not decode).
   */
  raw: string;
  charset: Charset;
  /** ECI designators in symbol order. ABSENT: none observed; []: ECI use signalled but not located. */
  eci?: EciSegment[];
  /** FNC1 position; absent when none was observed or the symbology does not report it. */
  fnc1?: Fnc1Position;
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
  /** One reading of the symbol's data; detections are the distinct contents the scanner can tell apart. */
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
  /**
   * The knee bisected once (Full depth, ordered ramps): the tightest
   * TESTED failing intensity ("blur 2.25"…). ABSENT when no refinement
   * ran — a budget cut included (the score stands; trace.budget_exhausted
   * is then true). Informational — the composite never reads it.
   */
  refined_failed_at?: string;
}
export interface StructuralReport { finder_integrity: [number, number, number]; quiet_zone_ok: boolean }
/**
 * Synthetic ISO 15415 unused-error-correction margin (1 − 2t/d, worst RS
 * block), measured on the stream of the orientation the decode verified,
 * only when every block error-corrects errors-only under the detection's
 * version, EC level and mask (an S5 rescue stream keeps its own count).
 * ISO measures against d − p: the Annex B reserve p of v1, v2-L and v3-L is
 * not subtracted, so there a block with errors reads above ISO's margin
 * (spec/04-score.md § Synthetic UEC).
 */
export interface UecReport {
  margin: number;
  grade: UecGrade;
  /**
   * Errors the worst RS block corrected — or, for a detection whose engines
   * include "rescue", the errors-only count of its stream: a distrust
   * marker (typically margin 0), not errors corrected.
   */
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
  /**
   * Synthetic UEC margin in ISO bands — null without a measured margin, or
   * withheld when the uec check is skipped (its grade still counts in overall).
   */
  unused_error_correction: IsoParameter | null;
  /** The ISO rule: lowest MEASURED parameter — a withheld one counts too, so a skip never moves it. */
  overall: IsoGrade;
}

/** A complete judgment: its calibration and every planned stress cell ran, so no budget moves it. */
export interface Score {
  /** Composite 0-100: weighted survival, then the structural caps and the UEC margin cap. */
  value: number;
  grade: Grade;
  /**
   * Σ contract weights of the axes that RAN (100 = the full six-axis
   * contract · 70 with perspective+rotation skipped). The honesty
   * integer: two 85s with different weights_run are different promises.
   * Only skipped axes lower it, never the budget. Absent on pre-0.9 reports.
   */
  weights_run: number;
  axes: AxisScore[];
  structural: StructuralReport | null;
  /**
   * Synthetic ISO 15415 unused-error-correction margin — null when no stream
   * error-corrects errors-only, or when the uec check is skipped (the
   * measured margin still caps value).
   */
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
   * The published UEC margin is 0: the worst RS block's errors spent all its
   * parity — the classic miscorrection signature — or, with "rescue" among
   * the engines, its errors-only count reached it (a distrust marker, not
   * errors corrected). Treat the decoded content as unverified. Never raised
   * by an errors-only decode at the acceptance limit of v1, v2-L or v3-L
   * (spec/04-score.md § Synthetic UEC).
   */
  | { hint: "low_correction_margin"; errors: number; capacity: number }
  /**
   * The transparent design only survives on part of the background range
   * (`alpha.envelope` narrower than "any"): pin a background layer in the
   * artwork, or constrain placement to `alpha.envelope.safe_luma`.
   */
  | { hint: "alpha_background_dependent"; placement: AlphaPlacement }
  /**
   * The canonical remedy: add an opaque plate (rounded rectangle +
   * quiet-zone margin) behind the symbol — robust everywhere, transparent
   * look preserved around it. Fires alongside alpha_background_dependent.
   */
  | { hint: "add_background_plate"; color: "white" | "black" };

/** The requested alpha-background handling (config echo). */
export type AlphaMode = "auto" | "white" | "black" | "custom";
/** Placement verdict spelled by the envelope's decoded bands. */
export type AlphaPlacement = "any" | "light_only" | "dark_only" | "mixed" | "none";

/** One neutral-background decode probe of the placement envelope. */
export interface AlphaProbe {
  /** Neutral background luma composited under the content (0-255). */
  background_luma: number;
  /** One of the composite's reads denotes the same content as the primary. */
  decoded: boolean;
}

/** One HOST-requested palette probe (`alphaPalette` option). */
export interface AlphaPaletteProbe {
  /** The requested color — "white" · "black" · "#rrggbb". */
  background: string;
  /** Its BT.601 luma (the sweep is luma-space by construction). */
  background_luma: number;
  /** One of the composite's reads denotes the same content as the primary. */
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
  /** HOST palette verdicts, request order — [] when none requested. */
  palette: AlphaPaletteProbe[];
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
  /** Full profile only; null when skipped, budget-exhausted, nothing decoded, or after a contained fault of the score computation. */
  envelope: AlphaEnvelope | null;
}

export interface StageTrace { stage: string; transforms_tried: number; ms: number; detections_found: number }
export interface PipelineTrace {
  stages: StageTrace[];
  engine_panics: number;
  /** Wall-clock milliseconds of the ladder walk(s) only. */
  total_ms: number;
  /** true: the budget cut planned work (partial). false: final. Absent before score_contract 5 (unknown); test === false. */
  budget_exhausted?: boolean;
}
export interface Versions { scanner: string; pipeline: number; score_contract: number }

export interface ScanReport {
  /** Empty = no QR found (a valid outcome, not an error) — final only when trace.budget_exhausted === false. */
  detections: Detection[];
  /**
   * Judgment of detections[0]. null when no judgment was published: no detection; a configuration that computes
   * no score (frame, every axis skipped); a judgment the budget interrupted (trace.budget_exhausted === true: retry
   * with a larger or no budget); a contained fault (native; wasm throws). Never partial; never read null as 0.
   */
  score: Score | null;
  hints: Hint[];
  /** ABSENT (not null) for opaque inputs and under alphaBackground "none". */
  alpha?: AlphaReport;
  trace: PipelineTrace;
  versions: Versions;
}
