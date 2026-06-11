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
    /// Symbol version, 1-40.
    pub version: u8,
    /// Error-correction level.
    pub ec_level: EcLevel,
    /// Mask pattern 0-7 when the engine reports it.
    pub mask: Option<u8>,
    /// Modules per side (`version * 4 + 17`).
    pub modules: u8,
    /// Symbol was mirrored.
    pub mirrored: bool,
    /// Symbol was light-on-dark.
    pub inverted: bool,
}

/// One decoded QR symbol.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Detection {
    /// Decoded content (text + raw bytes + charset).
    pub content: DecodedContent,
    /// Typed payload classification of the text.
    pub payload: Payload,
    /// Symbol corners in image coordinates, clockwise from top-left,
    /// when the engine provides them.
    pub corners: Option<[Point; 4]>,
    /// Symbol metadata.
    pub meta: QrMeta,
    /// Engine that produced this detection.
    pub engine: EngineKind,
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

/// Scannability score (contract v3 — enriched by the score module, task A8).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Score {
    /// Composite 0-100.
    pub value: u8,
    /// Interpretation band for `value`.
    pub grade: Grade,
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
}

/// Per-stage pipeline trace entry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct StageTrace {
    /// Stage name (stable identifiers: `normalize`, `pyramid`, `direct`, …).
    pub stage: String,
    /// Number of transform attempts executed in this stage.
    pub transforms_tried: u32,
    /// Wall-clock milliseconds spent in this stage.
    pub ms: f64,
    /// Detections found by this stage.
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
    /// Scannability score — `None` in the `Frame` profile.
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
                    version: 5,
                    ec_level: EcLevel::Q,
                    mask: Some(3),
                    modules: 37,
                    mirrored: false,
                    inverted: false,
                },
                engine: EngineKind::Rxing,
            }],
            score: Some(Score {
                value: 87,
                grade: Grade::Excellent,
            }),
            hints: vec![
                Hint::RaiseErrorCorrection {
                    current: EcLevel::Q,
                },
                Hint::IncreaseContrast,
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
        insta::assert_json_snapshot!("scan_report_full", full_report());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn schema_snapshot_empty() {
        insta::assert_json_snapshot!("scan_report_empty", ScanReport::empty());
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
