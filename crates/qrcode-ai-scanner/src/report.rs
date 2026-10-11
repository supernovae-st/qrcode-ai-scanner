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

/// The wire spelling of the report's `f32` fields: the `f64` nearest to
/// the shortest decimal that reads back as the `f32` — the number the JSON
/// text shows for it. Bindings that convert a report through serde's data
/// model instead of JSON text (a `serde_json::Value` turned into Python
/// objects, a serializer straight to JavaScript values) would otherwise
/// widen the `f32` itself — 0.612 arriving as 0.6119999885559082 — while
/// the JSON says 0.612; with this every serializer sees the decimal the JSON
/// shows. The JSON text keeps its digits (`serde_json` writes the same
/// shortest decimal for both types); between 1e-6 and 1e-5, and from 1e13,
/// it spells them in exponent form as it does for any `f64`. Reading a
/// report back is unchanged.
#[cfg(feature = "serde")]
mod decimal {
    use serde::Serializer;
    use serde::ser::SerializeTuple as _;

    /// The `f64` nearest to `value`'s shortest round-trip decimal, as
    /// `serde_json` spells it: when two decimals of that length read back as
    /// `value` and lie equally close to it, the one with the even last
    /// digit (2015.65625 is 2015.6562, where `Display` writes 2015.6563). A
    /// non-finite value and zero keep their kind.
    pub(super) fn of(value: f32) -> f64 {
        let shortest = value.to_string();
        let digits: String = shortest.chars().filter(char::is_ascii_digit).collect();
        let significant = digits.trim_start_matches('0').trim_end_matches('0').len();
        if !value.is_finite() || significant == 0 {
            return f64::from(value);
        }
        // the decimal of that many digits nearest to `value`, an exact tie
        // rounded to even: the spelling when it reads back as `value`
        let precision = significant - 1;
        let even = format!("{value:.precision$e}");
        let reads_back = even
            .parse::<f32>()
            .is_ok_and(|parsed| parsed.to_bits() == value.to_bits());
        let spelled = if reads_back { even } else { shortest };
        spelled.parse().unwrap_or(f64::from(value))
    }

    /// One `f32` field.
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's serialize_with passes the field by reference"
    )]
    pub(super) fn serialize<S: Serializer>(value: &f32, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_f64(of(*value))
    }

    /// An array of `f32` fields, element by element, in serde's own array
    /// shape (a tuple).
    pub(super) fn serialize_array<S: Serializer, const N: usize>(
        values: &[f32; N],
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        let mut tuple = ser.serialize_tuple(N)?;
        for value in values {
            tuple.serialize_element(&of(*value))?;
        }
        tuple.end()
    }
}

/// A 2D point in image pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Point {
    /// X coordinate (pixels, sub-pixel precision).
    #[cfg_attr(feature = "serde", serde(serialize_with = "decimal::serialize"))]
    pub x: f32,
    /// Y coordinate (pixels, sub-pixel precision).
    #[cfg_attr(feature = "serde", serde(serialize_with = "decimal::serialize"))]
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

/// How [`DecodedContent::text`] relates to [`DecodedContent::raw`] (spec/01
/// § Content). Every value except [`Charset::Unknown`] states that `text` is
/// a complete decoding of `raw` that the scanner checked byte for byte; it
/// states the encoder's declared interpretation only when
/// [`DecodedContent::eci`] lists it. `Unknown` is the only lossy value, now
/// and in every later version: test with [`Charset::is_lossy`]. Outside the
/// QR family `raw` is the engine's reading, which under an ECI can omit a
/// segment the engine could not decode; the label describes `raw` as
/// published and is not downgraded for it (spec/01 § raw).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Charset {
    /// Exactly the strict UTF-8 decoding of `raw` (ASCII included).
    Utf8,
    /// Exactly the WHATWG `Shift_JIS` decoding of `raw`.
    ShiftJis,
    /// Exactly the WHATWG windows-1252 decoding of `raw` (historical name;
    /// not strict ISO/IEC 8859-1).
    Latin1,
    /// Exactly the WHATWG gb18030 decoding of `raw` (GB 2312 and GBK read
    /// unchanged): Hanzi-mode data without a designator, or designators 29,
    /// 31, 32.
    Gb18030,
    /// `raw` decoded under [`DecodedContent::eci`] (bytes before the first
    /// designator as ISO/IEC 8859-1), when no label above states the same
    /// reading.
    Eci,
    /// Not a complete decoding: each U+FFFD stands for one byte or one
    /// maximal ill-formed sequence the scanner could not read; positions
    /// cannot be recovered from `text` — use `raw` (and `eci`).
    Unknown,
    /// Reader side only: a label written by a newer scanner, still a
    /// complete, checked decoding (show `text`, never re-decode `raw` from
    /// it). Never produced by this crate; serializing it is an error
    /// (forward the original JSON). Must stay the last variant.
    #[cfg_attr(feature = "serde", serde(other, skip_serializing))]
    Unrecognized,
}

impl Charset {
    /// `true` only for [`Charset::Unknown`], forever.
    #[must_use]
    pub const fn is_lossy(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// One ECI designator observed in the symbol's data (ISO/IEC 18004 §7.4.2).
/// It governs `raw[start..]` up to the next entry's `start`, or the end; an
/// empty span governs no byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct EciSegment {
    /// ECI assignment number as encoded, 0-999999.
    pub designator: u32,
    /// Byte offset into [`DecodedContent::raw`]; non-decreasing along the
    /// list, at most `raw.len()`.
    pub start: u32,
}

/// FNC1 position (ISO/IEC 18004 §7.4.8; AIM `]Q3`–`]Q6`,
/// `]d2`/`]d3`/`]d5`/`]d6`, `]C1`/`]C2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum Fnc1Position {
    /// GS1 mode: `raw` holds GS1 element-string data (QR and Data Matrix
    /// transmit 0x1D between fields; GS1-128 read by the current engine
    /// does not).
    First,
    /// AIM application mode: `raw` starts with the application indicator.
    Second,
    /// Reader side only, as [`Charset::Unrecognized`].
    #[cfg_attr(feature = "serde", serde(other, skip_serializing))]
    Unrecognized,
}

/// The exact bytes, the declarations observed and the scanner's ONE reading
/// of them (spec/01 § Content). Compare contents by identity (spec/01
/// § Comparing contents), never by `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct DecodedContent {
    /// `raw` decoded as `charset` states; empty if and only if `raw` is
    /// empty.
    pub text: String,
    /// Payload bytes, base64 on the wire. QR family: the ORIGINAL bytes on
    /// every decode path (designators excluded), never an engine's text.
    /// Other symbologies: the ISO/IEC 8859-1 encoding of the engine's text
    /// when every character is ≤ U+00FF (exact on its non-ECI paths), else
    /// its UTF-8; under an ECI the engine's text can omit a segment it could
    /// not decode (spec/01 § raw).
    #[cfg_attr(feature = "serde", serde(with = "raw_b64"))]
    pub raw: Vec<u8>,
    /// How `text` relates to `raw`.
    pub charset: Charset,
    /// `None` (key absent): no designator observed. `Some(vec![])`: ECI use
    /// signalled (`]Q2`/`]Q4`/`]Q6`) but the designators could not be
    /// located.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub eci: Option<Vec<EciSegment>>,
    /// `None` (key absent): no FNC1 observed, or not observable for this
    /// symbology.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub fnc1: Option<Fnc1Position>,
}

/// QR symbol metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct QrMeta {
    /// Symbol version, 1-40, when an engine measured it (rqrr path).
    pub version: Option<u8>,
    /// Error-correction level when measured — or, for a symbol only rxing
    /// read, the level its decoder states (the one field such a read fills).
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

/// One decoded symbol content.
///
/// Detections are the distinct contents the scanner can tell apart
/// (spec/01 § Detection): every read of every attempt is reconciled before
/// publication, and reads with the same symbology, `raw` and FNC1 position
/// and compatible ECI evidence become one detection. Identical copies of a
/// symbol are one detection; a different `raw`, FNC1 position or
/// contradicting declarations always stay separate, even with equal text.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Detection {
    /// The barcode symbology this detection was read as.
    pub symbology: Symbology,
    /// The content: raw bytes, declarations and the one reading of them.
    pub content: DecodedContent,
    /// Typed payload classification of the text.
    pub payload: Payload,
    /// Symbol corners in image coordinates, clockwise from top-left: from
    /// rqrr (its decode, or a grid it detected that the scanner
    /// error-corrected itself — `engines` may then list rxing alone) or the
    /// S5 rescue candidate.
    pub corners: Option<[Point; 4]>,
    /// Symbol metadata, from the same source as `corners`; a symbol only
    /// rxing read carries the EC level its decoder states and nothing else
    /// (spec/01 § Detection).
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

impl StressAxis {
    /// Parse the wire name (the `snake_case` serde spelling — what
    /// `score.axes[].axis` carries). The bindings' string-config seam:
    /// same shape as [`crate::ScanProfile::from_name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "resolution" => Some(Self::Resolution),
            "blur" => Some(Self::Blur),
            "contrast" => Some(Self::Contrast),
            "perspective" => Some(Self::Perspective),
            "rotation" => Some(Self::Rotation),
            "lighting" => Some(Self::Lighting),
            _ => None,
        }
    }
}

/// Survival result on one stress axis.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Wire label of the first failed cell — WHY this axis lost points
    /// ("blur 2.5", "glare", "128px"…). `null` when every cell passed.
    /// The panel's explainability seam: a lost point always names its cell.
    pub failed_at: Option<String>,
    /// The knee, bisected once (Full depth, ordered ramps only): the
    /// TIGHTEST TESTED failing intensity — the midpoint label when the
    /// midpoint probe failed, the knee cell's label when it held. Key
    /// ABSENT when no refinement ran (Reduced depth · no knee · lighting ·
    /// budget cut — the score stands, and `trace.budget_exhausted` says the
    /// budget cut it). Informational: the composite never reads it.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub refined_failed_at: Option<String>,
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
/// Measured on the stream of the orientation the decode verified, only
/// when every block error-corrects errors-only under the detection's
/// version, EC level and mask; a stream the S5 rescue recovered keeps its
/// errors-only count.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct UecReport {
    /// Remaining correction margin, 0.0-1.0 (1.0 = pristine).
    #[cfg_attr(feature = "serde", serde(serialize_with = "decimal::serialize"))]
    pub margin: f32,
    /// ISO band for `margin`.
    pub grade: UecGrade,
    /// Errors the worst block corrected, or a rescued read's errors-only count.
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
    #[cfg_attr(feature = "serde", serde(serialize_with = "decimal::serialize_array"))]
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
    #[cfg_attr(feature = "serde", serde(serialize_with = "decimal::serialize"))]
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
    /// (A ≥0.62 · B ≥0.50 · C ≥0.37 · D ≥0.25). `None` without a measured
    /// margin, or withheld when the `uec` check is skipped (its grade still
    /// counts in `overall`).
    pub unused_error_correction: Option<IsoParameter>,
    /// Overall = LOWEST measured parameter (the ISO rule) — a parameter
    /// withheld by `score_skip_checks` (`uec`) still counts, so a skip never
    /// moves it. Decode = A implicit (only decoded symbols are graded).
    pub overall: IsoGrade,
}

/// Scannability score — survival ramps, structural caps and the UEC margin
/// cap (spec/04-score.md). Validation, NOT ISO verification (no calibrated
/// optics). A present score is a COMPLETE judgment: its calibration and
/// every planned stress cell ran, so no budget moves it — only a knee
/// bisection (`AxisScore::refined_failed_at`) may be missing.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct Score {
    /// Composite 0-100 (weighted axis survival, then the structural caps and
    /// the UEC margin cap).
    pub value: u8,
    /// Σ contract weights of the axes that RAN (100 = the full six-axis
    /// contract · 70 with perspective+rotation skipped). The honesty
    /// integer: a score on skipped axes SAYS how much contract stands
    /// behind it — two 85s with different `weights_run` are different
    /// promises. Only skipped axes lower it, never the budget.
    /// Lenient default 0 when parsing pre-0.9 reports.
    #[cfg_attr(feature = "serde", serde(default))]
    pub weights_run: u8,
    /// Interpretation band for `value`.
    pub grade: Grade,
    /// Per-axis survival breakdown.
    pub axes: Vec<AxisScore>,
    /// Structural checks — `None` when no geometry was measured.
    pub structural: Option<StructuralReport>,
    /// Synthetic UEC margin — `None` when no stream error-corrects
    /// errors-only under the detection's format (then no margin cap and no
    /// margin hint either), or when the `uec` check is skipped (the measured
    /// margin still caps `value`).
    pub uec: Option<UecReport>,
    /// ISO 15415-informed grade card — `None` when no geometry was measured.
    pub iso15415: Option<Iso15415Report>,
}

/// The requested alpha-background handling (config echo on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum AlphaMode {
    /// Adaptive: the content's own luma picked the background.
    Auto,
    /// Forced white.
    White,
    /// Forced black.
    Black,
    /// Forced host background (`alpha.background` carries the hex).
    Custom,
}

/// Placement verdict spelled by the envelope's decoded ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum AlphaPlacement {
    /// Every tested background decoded — place it anywhere.
    Any,
    /// Only light backgrounds decoded (dark-module designs).
    LightOnly,
    /// Only dark backgrounds decoded (light-module designs).
    DarkOnly,
    /// A narrower or split range — read `safe_luma` for the exact bands.
    Mixed,
    /// No tested background decoded.
    None,
}

/// One neutral-background decode probe of the placement envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct AlphaProbe {
    /// Neutral background luma composited under the content.
    pub background_luma: u8,
    /// One of the composite's reads denotes the same content as the
    /// primary detection (spec/07 property 7).
    pub decoded: bool,
}

/// One HOST-requested palette probe (`alpha_palette` config) — "does the
/// design survive on this specific theme/brand color".
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct AlphaPaletteProbe {
    /// The requested color — `"white"` · `"black"` · `"#rrggbb"`.
    pub background: String,
    /// Its BT.601 luma (the sweep is luma-space by construction).
    pub background_luma: u8,
    /// One of the composite's reads denotes the same content as the
    /// primary detection (spec/07 property 7).
    pub decoded: bool,
}

/// The placement envelope — "over which backgrounds does this transparent
/// design keep decoding". Probes are quick decodes in the primary symbol's
/// own decode class (never the full ladder's recovery power): the envelope
/// measures COMFORTABLE placement, the primary verdict measures full
/// margin on its declared background.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct AlphaEnvelope {
    /// Decode probes ordered by background luma — the five fixed rungs
    /// (0 · 64 · 128 · 192 · 255) plus one bisection probe per
    /// decoded/undecoded boundary.
    pub probes: Vec<AlphaProbe>,
    /// Contiguous decoded bands `[lo, hi]` — every endpoint is a TESTED
    /// background, never interpolation.
    pub safe_luma: Vec<[u8; 2]>,
    /// The verdict the bands spell.
    pub placement: AlphaPlacement,
    /// HOST-requested palette verdicts (`alpha_palette` config), in
    /// request order — empty when none were requested. Palette colors
    /// never distort the neutral `probes`/`safe_luma`/`placement`.
    pub palette: Vec<AlphaPaletteProbe>,
}

/// How transparency was handled for this scan. Present ONLY when the input
/// actually carried transparent pixels AND `alpha_background` was not
/// `none` — opaque inputs stay byte-identical to pre-0.9 reports.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct AlphaReport {
    /// Fraction of pixels with alpha < 255, rounded to 3 decimals.
    #[cfg_attr(feature = "serde", serde(serialize_with = "decimal::serialize"))]
    pub coverage: f32,
    /// Alpha-weighted mean BT.601 luma of the visible content — what the
    /// `auto` decision reads (< 128 → white, else black).
    pub content_luma: u8,
    /// The requested handling mode (config echo).
    pub mode: AlphaMode,
    /// Background the verdict was measured over — `"white"` · `"black"` ·
    /// `"#rrggbb"`.
    pub background: String,
    /// The declared flatten found nothing and the opposite background
    /// produced the detections (`auto` only).
    pub fallback_used: bool,
    /// The placement envelope — Full profile only; `None` when skipped
    /// (`score_skip_checks: ["alpha_envelope"]`), cut by the budget
    /// (`trace.budget_exhausted`), when no probe class reads the design, or
    /// after a contained fault of the score computation.
    pub envelope: Option<AlphaEnvelope>,
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
    /// The published UEC margin is 0: the worst RS block's errors spent all
    /// its parity (on a rescued read, its errors-only count reached it), so
    /// the decode may already BE a miscorrection. Treat the content as
    /// unverified; confirm out-of-band before acting on it.
    LowCorrectionMargin {
        /// Errors the worst block corrected, or a rescued read's errors-only count.
        errors: u8,
        /// EC codeword capacity of that block.
        capacity: u8,
    },
    /// The transparent design only survives on part of the background
    /// range (`alpha.envelope` narrower than `any`): pin a background
    /// layer in the artwork, or place it only on surfaces inside
    /// `alpha.envelope.safe_luma`.
    AlphaBackgroundDependent {
        /// The envelope's placement verdict.
        placement: AlphaPlacement,
    },
    /// The canonical REMEDY for a background-dependent transparent design:
    /// add an opaque plate (rounded rectangle + quiet-zone margin) behind
    /// the symbol in the artwork — robust everywhere, transparent look
    /// preserved around it. Fires alongside `alpha_background_dependent`.
    AddBackgroundPlate {
        /// Plate color that maximizes contrast with the design's content.
        color: PlateColor,
    },
}

/// Recommended plate color for [`Hint::AddBackgroundPlate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[non_exhaustive]
pub enum PlateColor {
    /// Dark content — plate white.
    White,
    /// Light content — plate black.
    Black,
}

/// Per-stage pipeline trace entry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct StageTrace {
    /// Stage name (stable identifiers: `pyramid` · `direct` · `enhance` ·
    /// `deep` · `rescue`).
    pub stage: String,
    /// Number of transform attempts executed in this stage.
    pub transforms_tried: u32,
    /// Wall-clock milliseconds spent in this stage.
    pub ms: f64,
    /// Reads this stage's engine adapters delivered — raw hits before
    /// folding, the re-read, vetoes and reconciliation (an rxing + rqrr
    /// double read counts 2); a decode an adapter refuses (an rqrr grid that
    /// parses malformed, an rxing read a read check refuses) is not delivered;
    /// candidate re-derivations, re-reads and confirmation decodes never count.
    /// It includes hits the scanner later refuses or holds: read `detections`.
    pub detections_found: u32,
}

/// Pipeline execution trace — why a scan succeeded or came back empty.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct PipelineTrace {
    /// Stages executed, in order.
    pub stages: Vec<StageTrace>,
    /// Engine panics caught and isolated during this scan.
    pub engine_panics: u8,
    /// Wall-clock milliseconds of the ladder walk(s) only.
    pub total_ms: f64,
    /// `Some(true)`: the wall-clock budget prevented planned work — a
    /// ladder attempt or stage, the opposite-background walk, a rescue
    /// candidate, a score calibration step or stress cell, a knee bisection,
    /// an envelope probe — and the report is PARTIAL. `Some(false)`: every
    /// planned unit ran and the report is final, equal to the unbounded one
    /// (wall clock aside). `None`: unknown, a report older than this field;
    /// this crate always writes `Some`, and an unbounded scan `Some(false)`.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub budget_exhausted: Option<bool>,
}

impl Default for PipelineTrace {
    /// An empty walk that skipped nothing: the trace of a final report.
    fn default() -> Self {
        Self {
            stages: Vec::new(),
            engine_panics: 0,
            total_ms: 0.0,
            budget_exhausted: Some(false),
        }
    }
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
            // pipeline 3: exact content — every read is collected, then
            // reconciled by content identity (spec/01 § Content)
            pipeline: 3,
            // score contract 5: the correction-margin cap on the verified
            // orientation's stream, interrupted judgments absent, probes
            // matched by content identity
            score_contract: 5,
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
    /// Decoded symbols (empty = nothing found — final only when
    /// `trace.budget_exhausted` is `Some(false)`; multi-QR future-proof).
    pub detections: Vec<Detection>,
    /// Scannability score of the PRIMARY detection (`detections[0]` — for
    /// single-QR inputs, THE symbol). `None` exactly when no judgment was
    /// published: no detection; a configuration that computes no score
    /// ([`crate::ScanConfig::computes_score`]: the `Frame` profile, every
    /// axis skipped); a judgment the budget interrupted
    /// (`trace.budget_exhausted` is then `Some(true)`); a contained internal
    /// fault in the score computation. Never partial, never a zero.
    /// Per-detection scoring for multi-QR scenes is a future additive
    /// extension.
    pub score: Option<Score>,
    /// Machine-actionable improvement hints.
    pub hints: Vec<Hint>,
    /// Alpha-transparency handling — absent (not `null`) for opaque
    /// inputs and under `alpha_background: none`, so pre-0.9 consumers
    /// and opaque reports keep their exact bytes.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub alpha: Option<AlphaReport>,
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
            alpha: None,
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

    fn full_alpha() -> AlphaReport {
        AlphaReport {
            coverage: 0.612,
            content_luma: 34,
            mode: AlphaMode::Auto,
            background: "white".into(),
            fallback_used: false,
            envelope: Some(AlphaEnvelope {
                probes: [
                    (0, false),
                    (64, false),
                    (96, false),
                    (128, true),
                    (192, true),
                    (255, true),
                ]
                .into_iter()
                .map(|(background_luma, decoded)| AlphaProbe {
                    background_luma,
                    decoded,
                })
                .collect(),
                safe_luma: vec![[128, 255]],
                placement: AlphaPlacement::LightOnly,
                palette: vec![AlphaPaletteProbe {
                    background: "#1a1a2e".into(),
                    background_luma: 28,
                    decoded: false,
                }],
            }),
        }
    }

    fn full_score() -> Score {
        Score {
            value: 87,
            grade: Grade::Excellent,
            weights_run: 42,
            axes: vec![
                AxisScore {
                    axis: StressAxis::Resolution,
                    passed: 4,
                    total: 5,
                    failed_at: None,
                    refined_failed_at: None,
                },
                AxisScore {
                    axis: StressAxis::Perspective,
                    passed: 3,
                    total: 5,
                    failed_at: Some("34\u{b0}".into()),
                    refined_failed_at: Some("30\u{b0}".into()),
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
        }
    }

    fn full_report() -> ScanReport {
        ScanReport {
            detections: vec![Detection {
                symbology: Symbology::QrCode,
                content: DecodedContent {
                    text: "https://qrcode-ai.com".into(),
                    raw: b"https://qrcode-ai.com".to_vec(),
                    charset: Charset::Utf8,
                    eci: None,
                    fnc1: None,
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
            score: Some(full_score()),
            hints: vec![
                Hint::RaiseErrorCorrection {
                    current: EcLevel::Q,
                },
                Hint::IncreaseContrast,
                Hint::LowCorrectionMargin {
                    errors: 12,
                    capacity: 24,
                },
                Hint::AlphaBackgroundDependent {
                    placement: AlphaPlacement::LightOnly,
                },
                Hint::AddBackgroundPlate {
                    color: PlateColor::White,
                },
            ],
            alpha: Some(full_alpha()),
            trace: PipelineTrace {
                stages: vec![StageTrace {
                    stage: "direct".into(),
                    transforms_tried: 1,
                    ms: 12.5,
                    detections_found: 1,
                }],
                engine_panics: 0,
                total_ms: 12.5,
                budget_exhausted: Some(false),
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

    #[cfg(feature = "serde")]
    #[test]
    fn content_wire_names_pinned() {
        for (charset, wire) in [
            (Charset::Utf8, "utf8"),
            (Charset::ShiftJis, "shift_jis"),
            (Charset::Latin1, "latin1"),
            (Charset::Gb18030, "gb18030"),
            (Charset::Eci, "eci"),
            (Charset::Unknown, "unknown"),
        ] {
            assert_eq!(serde_json::to_value(charset).unwrap(), wire);
            let back: Charset = serde_json::from_value(wire.into()).unwrap();
            assert_eq!(back, charset);
            assert_eq!(charset.is_lossy(), charset == Charset::Unknown, "{wire}");
        }
        // a newer scanner's label reads as a complete decoding, and is never
        // written back
        let future: Charset = serde_json::from_str("\"future_x\"").unwrap();
        assert_eq!(future, Charset::Unrecognized);
        assert!(!future.is_lossy());
        assert!(serde_json::to_string(&Charset::Unrecognized).is_err());
        for (position, wire) in [
            (Fnc1Position::First, "first"),
            (Fnc1Position::Second, "second"),
        ] {
            assert_eq!(serde_json::to_value(position).unwrap(), wire);
        }
        let fifth: Fnc1Position = serde_json::from_str("\"fifth\"").unwrap();
        assert_eq!(fifth, Fnc1Position::Unrecognized);
        assert!(serde_json::to_string(&Fnc1Position::Unrecognized).is_err());
        assert_eq!(
            serde_json::to_value(EciSegment {
                designator: 4,
                start: 0
            })
            .unwrap(),
            serde_json::json!({"designator": 4, "start": 0})
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn content_optional_keys_are_absent_or_present() {
        let content = |eci, fnc1| DecodedContent {
            text: "hi".into(),
            raw: b"hi".to_vec(),
            charset: Charset::Utf8,
            eci,
            fnc1,
        };
        assert_eq!(
            serde_json::to_value(content(None, None)).unwrap(),
            serde_json::json!({"text": "hi", "raw": "aGk=", "charset": "utf8"})
        );
        assert_eq!(
            serde_json::to_value(content(Some(Vec::new()), Some(Fnc1Position::First))).unwrap(),
            serde_json::json!({
                "text": "hi", "raw": "aGk=", "charset": "utf8", "eci": [], "fnc1": "first"
            })
        );
        // reports without the keys read with both absent
        let old: DecodedContent =
            serde_json::from_str(r#"{"text":"hi","raw":"aGk=","charset":"utf8"}"#).unwrap();
        assert_eq!(old, content(None, None));
        let unknown: DecodedContent =
            serde_json::from_str(r#"{"text":"日本中文","raw":"k/qWe9bQzsQ=","charset":"unknown"}"#)
                .unwrap();
        assert_eq!((unknown.charset, unknown.eci), (Charset::Unknown, None));
        assert_eq!(
            unknown.raw,
            [0x93, 0xFA, 0x96, 0x7B, 0xD6, 0xD0, 0xCE, 0xC4]
        );
    }

    #[test]
    fn versions_pin_the_contract() {
        let v = Versions::current();
        assert_eq!(v.scanner, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            v.pipeline, 3,
            "pipeline v3 = collect-then-reconcile reads and exact content; \
             a bump must be deliberate"
        );
        assert_eq!(
            v.score_contract, 5,
            "score contract v5 = the margin cap on the verified stream, interrupted \
             judgments absent, probes matched by content identity"
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

    /// Every report this crate writes says whether the budget cut planned
    /// work; a report without the key predates it, and reads as unknown —
    /// never as "final".
    #[cfg(feature = "serde")]
    #[test]
    fn the_budget_flag_is_written_always_and_read_absent_as_unknown() {
        let old: PipelineTrace =
            serde_json::from_str(r#"{"stages":[],"engine_panics":0,"total_ms":0.0}"#).unwrap();
        assert_eq!(old.budget_exhausted, None, "absent reads as unknown");
        assert_eq!(
            serde_json::to_value(&old).unwrap(),
            serde_json::json!({"stages": [], "engine_panics": 0, "total_ms": 0.0}),
            "an unknown flag is re-written absent, never as false"
        );
        let cut: PipelineTrace = serde_json::from_str(
            r#"{"stages":[],"engine_panics":0,"total_ms":0.0,"budget_exhausted":true}"#,
        )
        .unwrap();
        assert_eq!(cut.budget_exhausted, Some(true));
        assert_eq!(
            serde_json::to_value(&cut).unwrap()["budget_exhausted"],
            true
        );
        assert_eq!(PipelineTrace::default().budget_exhausted, Some(false));
        let empty = serde_json::to_value(ScanReport::empty()).unwrap();
        assert_eq!(empty["trace"]["budget_exhausted"], false, "{empty}");
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

    #[test]
    fn stress_axes_parse_from_their_wire_names_only() {
        // the bindings' string-config seam: every axis from the name the
        // report writes for it, and nothing from any other spelling
        for (name, axis) in [
            ("resolution", StressAxis::Resolution),
            ("blur", StressAxis::Blur),
            ("contrast", StressAxis::Contrast),
            ("perspective", StressAxis::Perspective),
            ("rotation", StressAxis::Rotation),
            ("lighting", StressAxis::Lighting),
        ] {
            assert_eq!(StressAxis::from_name(name), Some(axis), "{name}");
            #[cfg(feature = "serde")]
            assert_eq!(serde_json::to_string(&axis).unwrap(), format!("\"{name}\""));
        }
        for name in ["", "Blur", "BLUR", "exposure", "tilt"] {
            assert_eq!(StressAxis::from_name(name), None, "{name}");
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

    /// Bindings that convert through serde's data model — pythonize over a
    /// `serde_json::Value`, serde-wasm-bindgen — receive what the JSON text
    /// shows: an `f32` field arrives as the decimal `serde_json` prints for
    /// it, never as the `f32` itself widened (0.612 as 0.6119999885559082).
    #[cfg(feature = "serde")]
    #[test]
    fn float_fields_reach_serde_as_the_decimals_the_json_shows() {
        let sum = 0.1_f32 + 0.2_f32; // f32 arithmetic: 0.3
        let third = 1.0_f32 / 3.0; // 0.33333334
        let point = Point { x: 0.612, y: third };
        let value = serde_json::to_value(point).unwrap();
        assert_eq!(
            (value["x"].as_f64(), value["y"].as_f64()),
            (Some(0.612), Some(0.333_333_34))
        );
        let uec = UecReport {
            margin: sum,
            grade: UecGrade::A,
            worst_block_errors: 0,
            worst_block_capacity: 26,
        };
        assert_eq!(
            serde_json::to_value(uec).unwrap()["margin"].as_f64(),
            Some(0.3)
        );
        let structural = StructuralReport {
            finder_integrity: [0.612, third, sum],
            quiet_zone_ok: true,
        };
        assert_eq!(
            serde_json::to_value(structural).unwrap()["finder_integrity"],
            serde_json::json!([0.612, 0.333_333_34, 0.3])
        );
        let parameter = IsoParameter {
            value: 0.612,
            grade: IsoGrade::A,
        };
        assert_eq!(
            serde_json::to_value(parameter).unwrap()["value"].as_f64(),
            Some(0.612)
        );
        assert_eq!(
            serde_json::to_value(full_alpha()).unwrap()["coverage"].as_f64(),
            Some(0.612)
        );
        // the JSON text is the f32's own shortest decimal, as it always was
        assert_eq!(
            serde_json::to_string(&point).unwrap(),
            r#"{"x":0.612,"y":0.33333334}"#
        );
    }

    /// The decimal spelling never moves a number, and moves no JSON byte
    /// where the report's values live: every finite `f32` of a sweep over
    /// the whole bit range reads back as the same number, and from 1e-5 to
    /// 1e13 (and zero) as the same text.
    #[cfg(feature = "serde")]
    #[test]
    fn the_decimal_spelling_keeps_the_json_text() {
        let mut same_text = 0;
        for bits in (0..=u32::MAX).step_by(65_521) {
            let value = f32::from_bits(bits);
            if !value.is_finite() {
                continue;
            }
            let widened = serde_json::to_string(&value).unwrap();
            let decimal = serde_json::to_string(&decimal::of(value)).unwrap();
            assert_eq!(
                widened.parse::<f64>().unwrap().to_bits(),
                decimal.parse::<f64>().unwrap().to_bits(),
                "{value:e}: {widened} vs {decimal}"
            );
            // ±0: every bit below the sign is clear
            let zero = bits.trailing_zeros() >= 31;
            if zero || (1e-5..1e13).contains(&value.abs()) {
                assert_eq!(widened, decimal, "{value:e}");
                same_text += 1;
            }
        }
        assert!(same_text > 10_000, "{same_text}");
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
