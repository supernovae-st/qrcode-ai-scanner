//! Scan report — the versioned cross-boundary contract.
//!
//! The serde JSON shape of [`ScanReport`] is consumed by the node/wasm
//! bindings, the site, and future Nika workflows. It only ever evolves
//! additively; `Versions` carries the contract markers.

use crate::payload::Payload;

/// Base64 wire shape for raw byte payloads — a JSON number-array would
/// bloat the contract (one number per byte).
#[cfg(feature = "serde")]
mod raw_b64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(de)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

/// A 2D point in image pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Point {
    /// X coordinate (pixels, sub-pixel precision).
    pub x: f32,
    /// Y coordinate (pixels, sub-pixel precision).
    pub y: f32,
}

/// The barcode symbology of a detection. QR-family entries
/// (`QrCode` · `MicroQrCode` · `RectangularMicroQrCode`) are the only ones
/// that can carry `QrMeta` geometry, the synthetic UEC, the ISO 15415 card
/// and the rescue stage; every other symbology decodes content + payload
/// classification only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Symbology {
    /// QR Code Model 2 (ISO/IEC 18004).
    QrCode,
    /// Micro QR (ISO/IEC 18004 M1-M4).
    MicroQrCode,
    /// rMQR — Rectangular Micro QR (ISO/IEC 23941).
    RectangularMicroQrCode,
    /// Data Matrix ECC 200 (ISO/IEC 16022).
    DataMatrix,
    /// Aztec (ISO/IEC 24778).
    Aztec,
    /// PDF417 (ISO/IEC 15438).
    Pdf417,
    /// `MaxiCode` (ISO/IEC 16023).
    MaxiCode,
    /// EAN-13 (GTIN-13 retail).
    Ean13,
    /// EAN-8.
    Ean8,
    /// UPC-A (GTIN-12 retail).
    UpcA,
    /// UPC-E.
    UpcE,
    /// Code 128 (GS1-128 when FNC1-led).
    Code128,
    /// Code 39.
    Code39,
    /// Code 93.
    Code93,
    /// Codabar.
    Codabar,
    /// ITF — Interleaved 2 of 5 (ITF-14 when 14 digits).
    Itf,
    /// GS1 `DataBar` (RSS-14).
    DataBar,
    /// GS1 `DataBar` Expanded.
    DataBarExpanded,
    /// Telepen.
    Telepen,
}

impl Symbology {
    /// QR family — the symbologies whose geometry/meta/UEC paths exist.
    #[must_use]
    pub fn is_qr_family(self) -> bool {
        matches!(
            self,
            Self::QrCode | Self::MicroQrCode | Self::RectangularMicroQrCode
        )
    }

    /// Retail GTIN carriers — the symbol's data IS a GTIN (AI 01 semantics).
    #[must_use]
    pub fn is_retail_gtin(self) -> bool {
        matches!(self, Self::Ean13 | Self::Ean8 | Self::UpcA | Self::UpcE)
    }
}

/// Which decode engine produced a detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum EngineKind {
    /// ZXing-family engine (rxing).
    Rxing,
    /// quirc-family engine (rqrr).
    Rqrr,
    /// The S5 erasure-rescue stage — errors-and-erasures RS over a grid
    /// the engines detected but could not decode (Forney 1965; the
    /// logo-occlusion recovery path).
    Rescue,
}

/// QR error-correction level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum EcLevel {
    /// ~7% codeword recovery.
    L,
    /// ~15% codeword recovery.
    M,
    /// ~25% codeword recovery.
    Q,
    /// ~30% codeword recovery.
    H,
}

/// Character set resolved for the decoded bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Charset {
    /// Valid UTF-8 (or ASCII).
    Utf8,
    /// Shift-JIS (kanji-mode or sniffed byte-mode).
    ShiftJis,
    /// Windows-1252 / Latin-1 fallback.
    Latin1,
}

/// Decoded content — both the resolved text and the raw bytes truth.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct DecodedContent {
    /// Text under the resolved charset.
    pub text: String,
    /// Raw decoded bytes (charset-independent truth). Base64 on the wire.
    #[cfg_attr(feature = "serde", serde(with = "raw_b64"))]
    pub raw: Vec<u8>,
    /// Charset used to produce `text`.
    pub charset: Charset,
}

/// QR symbol metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct QrMeta {
    /// Symbol version, 1-40, when an engine measured it (rqrr path).
    pub version: Option<u8>,
    /// Error-correction level when measured.
    pub ec_level: Option<EcLevel>,
    /// Mask pattern 0-7 when measured.
    pub mask: Option<u8>,
    /// Modules per side (`version * 4 + 17`), derived from `version`.
    pub modules: Option<u8>,
    /// Symbol is photometrically INVERTED (light-on-dark) — measured from
    /// the geometry source's decode path. `None` when no geometry (the
    /// rxing-only path handles inverted symbols internally without
    /// reporting which reading won).
    pub inverted: Option<bool>,
}

/// One decoded QR symbol.
///
/// Detections merge across engines and attempts BY DECODED TEXT: two
/// physical symbols carrying the identical payload in one image collapse
/// into one detection (corners from the first geometry source). Distinct
/// payloads always stay distinct.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Detection {
    /// The barcode symbology this detection was read as.
    pub symbology: Symbology,
    /// Decoded content (text + raw bytes + charset).
    pub content: DecodedContent,
    /// Typed payload classification of the text.
    pub payload: Payload,
    /// Symbol corners in image coordinates, clockwise from top-left,
    /// when the engine provides them.
    pub corners: Option<[Point; 4]>,
    /// Symbol metadata.
    pub meta: QrMeta,
    /// Engines that confirmed this payload (consensus surface for scoring).
    pub engines: Vec<EngineKind>,
}

/// Score grade bands — the published interpretation table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Grade {
    /// 80-100 — safe for all devices and conditions.
    Excellent,
    /// 70-79 — production ready.
    Good,
    /// 60-69 — may fail on older phones or poor lighting.
    Acceptable,
    /// 40-59 — consider regenerating.
    Fair,
    /// 0-39 — regenerate.
    Poor,
}

impl Grade {
    /// Band for a 0-100 score value.
    #[must_use]
    pub fn from_value(value: u8) -> Self {
        match value {
            80.. => Self::Excellent,
            70..=79 => Self::Good,
            60..=69 => Self::Acceptable,
            40..=59 => Self::Fair,
            0..=39 => Self::Poor,
        }
    }
}

/// One stress dimension of the score contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum StressAxis {
    /// Downscale ramp — sampling margin (px/module floor).
    Resolution,
    /// Gaussian blur ramp — focus/motion margin.
    Blur,
    /// Global contrast reduction ramp.
    Contrast,
    /// Perspective tilt ramp — grid-estimation margin.
    Perspective,
    /// Non-cardinal rotation ramp.
    Rotation,
    /// Local lighting defects (shadow · glare · exposure) — pass set.
    Lighting,
}

/// Survival result on one stress axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct AxisScore {
    /// The dimension.
    pub axis: StressAxis,
    /// Cells survived. Ramps stop at the first failure (the knee); the
    /// lighting set is unordered — no knee-exit (depth still picks the
    /// cell subset).
    pub passed: u8,
    /// Cells in the ramp at this depth.
    pub total: u8,
}

/// ISO 15415 UEC grade bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum UecGrade {
    /// margin ≥ 0.62
    A,
    /// margin ≥ 0.50
    B,
    /// margin ≥ 0.37
    C,
    /// margin ≥ 0.25
    D,
    /// margin < 0.25
    F,
}

impl UecGrade {
    /// ISO band for a margin value.
    #[must_use]
    pub fn from_margin(margin: f32) -> Self {
        if margin >= 0.62 {
            Self::A
        } else if margin >= 0.50 {
            Self::B
        } else if margin >= 0.37 {
            Self::C
        } else if margin >= 0.25 {
            Self::D
        } else {
            Self::F
        }
    }
}

/// Synthetic Unused Error Correction — the real robustness margin
/// (`1 − 2t/d`, worst RS block). Validation-grade, not ISO verification.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct UecReport {
    /// Remaining correction margin, 0.0-1.0 (1.0 = pristine).
    pub margin: f32,
    /// ISO band for `margin`.
    pub grade: UecGrade,
    /// Errors corrected in the worst RS block.
    pub worst_block_errors: u8,
    /// EC codewords (capacity `d`) of that block.
    pub worst_block_capacity: u8,
}

/// Structural checks — computed when symbol geometry was measured.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct StructuralReport {
    /// Per-finder 1:1:3:1:1 integrity, 0.0-1.0 — [top-left, top-right,
    /// bottom-left]. The #1 documented AI-art failure mode.
    pub finder_integrity: [f32; 3],
    /// Clear border present — probed on a 2-module outer ring (ISO/IEC
    /// 18004 recommends 4 modules; the lenient probe targets what breaks
    /// locators in practice).
    pub quiet_zone_ok: bool,
}

/// ISO 15415 letter grade (4=A … 0=F vocabulary, lowercase on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum IsoGrade {
    /// 4.0
    A,
    /// 3.0
    B,
    /// 2.0
    C,
    /// 1.0
    D,
    /// 0.0
    F,
}

/// One measured ISO 15415 parameter: the raw value + its grade band.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct IsoParameter {
    /// Parameter value in its ISO unit (see each field's doc).
    pub value: f32,
    /// ISO band for `value`.
    pub grade: IsoGrade,
}

/// ISO/IEC 15415-**informed** grade card — the software-measurable subset.
///
/// HONESTY CONTRACT: a conformant 15415 grade requires calibrated optics
/// (45° illumination, stated wavelength, defined aperture — they are IN the
/// grade string) and ISO 15426-2 hardware conformance. This report is
/// standards-based DIAGNOSTICS over an arbitrary image. Grid Nonuniformity
/// and Reflectance Margin are deliberately absent (not measurable from
/// 4-corner geometry / uncalibrated reflectance).
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Iso15415Report {
    /// Symbol Contrast — `(R_high − R_low)/255` over module means
    /// (2nd/98th percentile, noise-robust). A ≥0.70 · B ≥0.55 · C ≥0.40 ·
    /// D ≥0.20.
    pub symbol_contrast: IsoParameter,
    /// Modulation — robust minimum of per-module `2·|R − GT|/SC`
    /// (5th percentile; simplified — no notional-UEC iteration).
    /// A ≥0.50 · B ≥0.40 · C ≥0.30 · D ≥0.20.
    pub modulation: IsoParameter,
    /// Axial Nonuniformity — `|X̄ − Ȳ| / mean` of the two axis pitches from
    /// the detected corners. A ≤0.06 · B ≤0.08 · C ≤0.10 · D ≤0.12.
    /// Caveat: perspective in a photo reads as ANU (ISO assumes flat capture).
    pub axial_nonuniformity: IsoParameter,
    /// Fixed Pattern Damage approximation — worst finder integrity
    /// (A ≥0.95 · B ≥0.90 · C ≥0.80 · D ≥0.70); a quiet-zone violation caps
    /// the grade at D. (No clock-track subtest.)
    pub fixed_pattern_damage: IsoParameter,
    /// Unused Error Correction — the synthetic UEC margin, ISO bands
    /// (A ≥0.62 · B ≥0.50 · C ≥0.37 · D ≥0.25). `None` without a bitstream.
    pub unused_error_correction: Option<IsoParameter>,
    /// Overall = LOWEST measured parameter (the ISO rule), Decode = A
    /// implicit (only decoded symbols are graded).
    pub overall: IsoGrade,
}

/// Scannability score — contract v3: survival ramps + structural caps.
/// Validation, NOT ISO verification (no calibrated optics).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Score {
    /// Composite 0-100 (weighted axis survival, structurally capped).
    pub value: u8,
    /// Interpretation band for `value`.
    pub grade: Grade,
    /// Per-axis survival breakdown.
    pub axes: Vec<AxisScore>,
    /// Structural checks — `None` when no geometry was measured.
    pub structural: Option<StructuralReport>,
    /// Synthetic UEC margin — `None` when the raw stream was unavailable.
    pub uec: Option<UecReport>,
    /// ISO 15415-informed grade card — `None` when no geometry was measured.
    pub iso15415: Option<Iso15415Report>,
}

/// Machine-actionable improvement hint — the generator/agent feedback loop.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "hint", rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Hint {
    /// Regenerate with a higher error-correction level.
    RaiseErrorCorrection {
        /// Level observed in the symbol.
        current: EcLevel,
    },
    /// Module contrast is the limiting factor.
    IncreaseContrast,
    /// Modules are too dense/small for reliable sampling.
    EnlargeModules,
    /// A finder pattern is damaged.
    FixFinderPattern {
        /// Corner index: 0 top-left, 1 top-right, 2 bottom-left.
        corner: u8,
    },
    /// The quiet zone is violated.
    RestoreQuietZone,
    /// Artistic texture overwhelms module structure.
    ReduceArtTexture,
    /// The decode sits AT the Reed-Solomon correction limit (UEC margin 0):
    /// one more error would have been an undetectable miscorrection — and
    /// the decode itself may already BE one. Treat the content as
    /// unverified; confirm out-of-band before acting on it.
    LowCorrectionMargin {
        /// Errors corrected in the worst RS block.
        errors: u8,
        /// EC codeword capacity of that block.
        capacity: u8,
    },
}

/// Per-stage pipeline trace entry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct StageTrace {
    /// Stage name (stable identifiers: `pyramid` · `direct` · `enhance` · `deep`).
    pub stage: String,
    /// Number of transform attempts executed in this stage.
    pub transforms_tried: u32,
    /// Wall-clock milliseconds spent in this stage.
    pub ms: f64,
    /// RAW engine hits in this stage, PRE-merge — both engines decoding
    /// the same symbol counts twice here (the merged symbol count is
    /// `detections.len()` on the report).
    pub detections_found: u32,
}

/// Pipeline execution trace — why a scan succeeded or came back empty.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PipelineTrace {
    /// Stages executed, in order.
    pub stages: Vec<StageTrace>,
    /// Engine panics caught and isolated during this scan.
    pub engine_panics: u8,
    /// Total wall-clock milliseconds.
    pub total_ms: f64,
}

/// Contract version markers carried by every report.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Versions {
    /// Crate version that produced the report.
    pub scanner: String,
    /// Decode pipeline version.
    pub pipeline: u8,
    /// Score contract version.
    pub score_contract: u8,
}

impl QrMeta {
    /// Modules per side for a QR version (`v*4 + 17`), `None` past v40 —
    /// the ONE place this formula lives.
    #[must_use]
    pub fn modules_per_side(version: u8) -> Option<u8> {
        (1..=40).contains(&version).then(|| version * 4 + 17)
    }
}

impl Versions {
    /// Markers for this build.
    #[must_use]
    pub fn current() -> Self {
        Self {
            scanner: env!("CARGO_PKG_VERSION").to_owned(),
            pipeline: 1,
            score_contract: 3,
        }
    }
}

/// The scan result — the versioned cross-boundary contract.
///
/// Empty `detections` means "no QR found", which is a valid outcome,
/// never an error.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ScanReport {
    /// Decoded symbols (empty = nothing found; multi-QR future-proof).
    pub detections: Vec<Detection>,
    /// Scannability score of the PRIMARY detection (the first-discovered
    /// symbol — for single-QR inputs, THE symbol). `None` in the `Frame`
    /// profile. Per-detection scoring for multi-QR scenes is a future
    /// additive extension.
    pub score: Option<Score>,
    /// Machine-actionable improvement hints.
    pub hints: Vec<Hint>,
    /// Pipeline execution trace.
    pub trace: PipelineTrace,
    /// Contract version markers.
    pub versions: Versions,
}

impl ScanReport {
    /// A valid "nothing found" report.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            detections: Vec::new(),
            score: None,
            hints: Vec::new(),
            trace: PipelineTrace::default(),
            versions: Versions::current(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::payload::Payload;

    fn full_report() -> ScanReport {
        ScanReport {
            detections: vec![Detection {
                symbology: Symbology::QrCode,
                content: DecodedContent {
                    text: "https://qrcode-ai.com".into(),
                    raw: b"https://qrcode-ai.com".to_vec(),
                    charset: Charset::Utf8,
                },
                payload: Payload::Text,
                corners: Some([
                    Point { x: 10.0, y: 10.0 },
                    Point { x: 90.0, y: 10.5 },
                    Point { x: 90.5, y: 90.0 },
                    Point { x: 10.0, y: 89.5 },
                ]),
                meta: QrMeta {
                    version: Some(5),
                    ec_level: Some(EcLevel::Q),
                    mask: Some(3),
                    modules: Some(37),
                    inverted: Some(false),
                },
                engines: vec![EngineKind::Rxing, EngineKind::Rqrr],
            }],
            score: Some(Score {
                value: 87,
                grade: Grade::Excellent,
                axes: vec![
                    AxisScore {
                        axis: StressAxis::Resolution,
                        passed: 4,
                        total: 5,
                    },
                    AxisScore {
                        axis: StressAxis::Perspective,
                        passed: 3,
                        total: 5,
                    },
                ],
                structural: Some(StructuralReport {
                    finder_integrity: [1.0, 0.96, 0.88],
                    quiet_zone_ok: true,
                }),
                uec: Some(UecReport {
                    margin: 0.85,
                    grade: UecGrade::A,
                    worst_block_errors: 1,
                    worst_block_capacity: 18,
                }),
                iso15415: Some(Iso15415Report {
                    symbol_contrast: IsoParameter {
                        value: 0.82,
                        grade: IsoGrade::A,
                    },
                    modulation: IsoParameter {
                        value: 0.46,
                        grade: IsoGrade::B,
                    },
                    axial_nonuniformity: IsoParameter {
                        value: 0.03,
                        grade: IsoGrade::A,
                    },
                    fixed_pattern_damage: IsoParameter {
                        value: 0.88,
                        grade: IsoGrade::C,
                    },
                    unused_error_correction: Some(IsoParameter {
                        value: 0.85,
                        grade: IsoGrade::A,
                    }),
                    overall: IsoGrade::C,
                }),
            }),
            hints: vec![
                Hint::RaiseErrorCorrection {
                    current: EcLevel::Q,
                },
                Hint::IncreaseContrast,
                Hint::LowCorrectionMargin {
                    errors: 12,
                    capacity: 24,
                },
            ],
            trace: PipelineTrace {
                stages: vec![StageTrace {
                    stage: "direct".into(),
                    transforms_tried: 1,
                    ms: 12.5,
                    detections_found: 1,
                }],
                engine_panics: 0,
                total_ms: 12.5,
            },
            versions: Versions::current(),
        }
    }

    #[test]
    fn symbology_wire_names_pinned() {
        for (sym, wire) in [
            (Symbology::QrCode, "qr_code"),
            (Symbology::MicroQrCode, "micro_qr_code"),
            (
                Symbology::RectangularMicroQrCode,
                "rectangular_micro_qr_code",
            ),
            (Symbology::DataMatrix, "data_matrix"),
            (Symbology::Aztec, "aztec"),
            (Symbology::Pdf417, "pdf417"),
            (Symbology::MaxiCode, "maxi_code"),
            (Symbology::Ean13, "ean13"),
            (Symbology::Ean8, "ean8"),
            (Symbology::UpcA, "upc_a"),
            (Symbology::UpcE, "upc_e"),
            (Symbology::Code128, "code128"),
            (Symbology::Code39, "code39"),
            (Symbology::Code93, "code93"),
            (Symbology::Codabar, "codabar"),
            (Symbology::Itf, "itf"),
            (Symbology::DataBar, "data_bar"),
            (Symbology::DataBarExpanded, "data_bar_expanded"),
            (Symbology::Telepen, "telepen"),
        ] {
            assert_eq!(
                serde_json::to_value(sym).unwrap(),
                serde_json::Value::String(wire.to_owned())
            );
        }
    }

    #[test]
    fn versions_pin_the_contract() {
        let v = Versions::current();
        assert_eq!(v.scanner, env!("CARGO_PKG_VERSION"));
        assert_eq!(v.pipeline, 1, "pipeline version bump must be deliberate");
        assert_eq!(
            v.score_contract, 3,
            "score contract v3 is the published one"
        );
    }

    #[test]
    fn empty_report_has_no_detections_and_no_score() {
        let report = ScanReport::empty();
        assert!(report.detections.is_empty());
        assert!(report.score.is_none());
        assert!(report.hints.is_empty());
        assert_eq!(report.versions, Versions::current());
    }

    #[test]
    fn is_qr_family_pins_exactly_the_three_qr_symbologies() {
        // report.rs 91:9 — the `matches!` body was replaced by `-> true` AND
        // `-> false` stubs, both survived (nothing called the predicate). Pin
        // both directions: every QR-family symbology is true, every other false.
        for sym in [
            Symbology::QrCode,
            Symbology::MicroQrCode,
            Symbology::RectangularMicroQrCode,
        ] {
            assert!(sym.is_qr_family(), "{sym:?} is QR-family"); // kills `-> false`
        }
        for sym in [
            Symbology::DataMatrix,
            Symbology::Aztec,
            Symbology::Pdf417,
            Symbology::MaxiCode,
            Symbology::Ean13,
            Symbology::Ean8,
            Symbology::UpcA,
            Symbology::UpcE,
            Symbology::Code128,
            Symbology::Code39,
            Symbology::DataBar,
            Symbology::Telepen,
        ] {
            assert!(!sym.is_qr_family(), "{sym:?} is NOT QR-family"); // kills `-> true`
        }
    }

    #[test]
    fn is_retail_gtin_pins_exactly_the_four_retail_symbologies() {
        // report.rs 100:9 — the `matches!` body → `-> true` stub survived.
        // Pin both directions for completeness.
        for sym in [
            Symbology::Ean13,
            Symbology::Ean8,
            Symbology::UpcA,
            Symbology::UpcE,
        ] {
            assert!(sym.is_retail_gtin(), "{sym:?} is a retail GTIN carrier"); // kills `-> false`
        }
        for sym in [
            Symbology::QrCode,
            Symbology::DataMatrix,
            Symbology::Aztec,
            Symbology::Code128,
            Symbology::Itf,
            Symbology::DataBar,
        ] {
            assert!(!sym.is_retail_gtin(), "{sym:?} is NOT a retail GTIN"); // kills `-> true`
        }
    }

    #[test]
    fn grade_bands_are_pinned() {
        let bands = [
            (100, Grade::Excellent),
            (80, Grade::Excellent),
            (79, Grade::Good),
            (70, Grade::Good),
            (69, Grade::Acceptable),
            (60, Grade::Acceptable),
            (59, Grade::Fair),
            (40, Grade::Fair),
            (39, Grade::Poor),
            (0, Grade::Poor),
        ];
        for (value, expected) in bands {
            assert_eq!(Grade::from_value(value), expected, "value {value}");
        }
    }

    #[cfg(feature = "serde")]
    #[test]
    fn schema_snapshot_full() {
        insta::assert_json_snapshot!("scan_report_full", full_report(), {
            ".versions.scanner" => "[crate-version]"
        });
    }

    #[cfg(feature = "serde")]
    #[test]
    fn schema_snapshot_empty() {
        insta::assert_json_snapshot!("scan_report_empty", ScanReport::empty(), {
            ".versions.scanner" => "[crate-version]"
        });
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_roundtrip_preserves_everything() {
        let report = full_report();
        let json = serde_json::to_string(&report).unwrap();
        let back: ScanReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}
