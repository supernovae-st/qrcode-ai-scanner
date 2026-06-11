//! Scan errors — the `QRS-xxx` registry. Codes are never reused.
//!
//! `Err` is reserved for real faults. "No QR found" is a valid scan outcome
//! (`ScanReport` with empty detections), never an error.

use miette::Diagnostic;
use thiserror::Error;

/// Errors for real faults only — "no QR found" is NOT an error.
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
pub enum ScanError {
    /// QRS-001 — the bytes are not a decodable image.
    #[error("unsupported or corrupt image: {details}")]
    #[diagnostic(code("QRS-001"), help("supported formats: PNG, JPEG, WebP, GIF"))]
    InvalidImage {
        /// Decoder-reported reason.
        details: String,
    },

    /// QRS-002 — image dimensions exceed the configured limit.
    #[error("image {width}x{height} exceeds limit {max_dimension}")]
    #[diagnostic(
        code("QRS-002"),
        help("downscale the image or raise Limits::max_dimension")
    )]
    DimensionsExceeded {
        /// Image width in pixels.
        width: u32,
        /// Image height in pixels.
        height: u32,
        /// Configured per-side limit.
        max_dimension: u32,
    },

    /// QRS-003 — total pixel count overflows or exceeds the configured limit.
    #[error("pixel count for {width}x{height} exceeds limit {max_pixels}")]
    #[diagnostic(code("QRS-003"), help("raise Limits::max_pixels if intentional"))]
    PixelOverflow {
        /// Image width in pixels.
        width: u32,
        /// Image height in pixels.
        height: u32,
        /// Configured total pixel limit.
        max_pixels: u64,
    },

    /// QRS-004 — a raw buffer length does not match the declared dimensions.
    #[error("buffer length {got} does not match expected {expected}")]
    #[diagnostic(
        code("QRS-004"),
        help("rgba8 expects w*h*4 bytes; luma8 expects w*h bytes")
    )]
    BufferMismatch {
        /// Bytes received.
        got: usize,
        /// Bytes required by the declared dimensions.
        expected: usize,
    },

    /// QRS-005 — cooperative cancellation honoured.
    #[error("scan cancelled")]
    #[diagnostic(code("QRS-005"))]
    Cancelled,
}

impl ScanError {
    /// Stable wire code (`"QRS-001"`). Always equals the miette diagnostic code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidImage { .. } => "QRS-001",
            Self::DimensionsExceeded { .. } => "QRS-002",
            Self::PixelOverflow { .. } => "QRS-003",
            Self::BufferMismatch { .. } => "QRS-004",
            Self::Cancelled => "QRS-005",
        }
    }

    /// Whether retrying without changing the input may succeed.
    /// Every current variant is deterministic: false.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        false
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, ScanError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn all_variants() -> Vec<ScanError> {
        vec![
            ScanError::InvalidImage {
                details: "not png".into(),
            },
            ScanError::DimensionsExceeded {
                width: 12_000,
                height: 9_000,
                max_dimension: 10_000,
            },
            ScanError::PixelOverflow {
                width: 4_000_000,
                height: 4_000_000,
                max_pixels: 64_000_000,
            },
            ScanError::BufferMismatch {
                got: 11,
                expected: 16,
            },
            ScanError::Cancelled,
        ]
    }

    #[test]
    fn codes_are_unique_and_well_formed() {
        let mut codes: Vec<&'static str> = all_variants().iter().map(ScanError::code).collect();
        for code in &codes {
            assert!(
                code.starts_with("QRS-") && code.len() == 7,
                "malformed code: {code}"
            );
        }
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "duplicate QRS codes");
    }

    #[test]
    fn wire_code_matches_miette_diagnostic_code() {
        use miette::Diagnostic;
        for err in all_variants() {
            let diagnostic = err.code().to_string();
            let miette_code = Diagnostic::code(&err)
                .map(|c| c.to_string())
                .unwrap_or_default();
            assert_eq!(diagnostic, miette_code, "wire/miette mismatch for {err}");
        }
    }

    #[test]
    fn expected_wire_codes_pinned() {
        let pinned: Vec<&str> = all_variants().iter().map(ScanError::code).collect();
        assert_eq!(
            pinned,
            vec!["QRS-001", "QRS-002", "QRS-003", "QRS-004", "QRS-005"],
            "wire codes are a published contract — never renumber"
        );
    }

    #[test]
    fn no_variant_is_transient_today() {
        for err in all_variants() {
            assert!(!err.is_transient(), "{err} wrongly marked transient");
        }
    }

    #[test]
    fn display_carries_structured_fields() {
        let err = ScanError::DimensionsExceeded {
            width: 12_000,
            height: 9_000,
            max_dimension: 10_000,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("12000") && msg.contains("9000") && msg.contains("10000"),
            "{msg}"
        );

        let err = ScanError::BufferMismatch {
            got: 11,
            expected: 16,
        };
        let msg = err.to_string();
        assert!(msg.contains("11") && msg.contains("16"), "{msg}");
    }
}
