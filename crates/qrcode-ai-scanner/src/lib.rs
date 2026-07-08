//! # QR Code AI Scanner
//!
//! Decoding + scannability validation for artistic, AI-generated, and
//! photo-captured QR codes — the codes that break standard scanners.
//!
//! Contract (the normative wire format lives in the `spec/` directory):
//!
//! - **No QR found is `Ok`** with empty detections — `Err` is reserved for real
//!   faults (corrupt image, invalid buffer, cancellation).
//! - **Deterministic**: same bytes + same config + same versions ⇒ the same
//!   attempt sequence, bit for bit on a given platform/build — no RNG
//!   anywhere. Two caveats, both bounded: a wall-clock budget makes the CUT
//!   point machine-dependent (set `budget_ms: None` for strictly identical
//!   reports), and CROSS-platform the libm transcendentals (warp kernels,
//!   gaussian blur) may differ in the last ulp — a stress cell sitting
//!   exactly on a knee can flip between platforms.
//! - **Sync by design**: async belongs to the bindings (napi/wasm), never here.
//! - **Engine-isolated**: third-party decoder panics are caught at the engine
//!   boundary and recorded in the trace; the ladder continues.

#[cfg(not(any(feature = "engine-rxing", feature = "engine-rqrr")))]
compile_error!(
    "qrcode-ai-scanner requires at least one engine feature: `engine-rxing` or `engine-rqrr`"
);

mod engine;
mod error;
mod gs1;
mod input;
mod ladder;
mod matrix;
mod payload;
mod report;
mod rescue;
mod score;
mod transform;

pub use error::{Result, ScanError};
pub use gs1::Gs1Element;
pub use input::{ImageInput, Limits};
pub use ladder::{CancelToken, ScanConfig, ScanProfile, ScoreDepth};
pub use payload::Payload;
pub use report::{
    AxisScore, Charset, DecodedContent, Detection, EcLevel, EngineKind, Grade, Hint,
    Iso15415Report, IsoGrade, IsoParameter, PipelineTrace, Point, QrMeta, ScanReport, Score,
    StageTrace, StressAxis, StructuralReport, Symbology, UecGrade, UecReport, Versions,
};

/// Fuzz-only entry points — cargo-fuzz builds the whole graph with
/// `--cfg fuzzing`; this surface does not exist in normal builds.
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzzing {
    /// Drive both text classifiers (plain + FNC1) — they parse
    /// attacker-controlled decoded QR content and must never panic.
    pub fn classify_text(text: &str) {
        let _ = crate::payload::classify(text);
        let _ = crate::payload::classify_fnc1(text);
    }

    /// Drive the S5 rescue bitstream parser over attacker-controlled data
    /// codewords. A rescued stream never reaches the engines, so this parser
    /// owns the decode and must never panic (refusal is the correct answer).
    pub fn parse_bitstream(data: &[u8], version: u8) {
        let _ = crate::rescue::fuzz_api::parse_bitstream(data, version);
    }

    /// Drive the errors-and-erasures RS corrector over an adversarial block,
    /// parity count, and erasure set — must never panic (`None` on garbage).
    pub fn correct_block(block: &mut [u8], npar: usize, erasures: &[usize]) {
        let _ = crate::rescue::fuzz_api::correct_block(block, npar, erasures);
    }
}

/// Reusable QR scanner — configure once, scan many. `Send + Sync`, no
/// interior state: share one instance across threads freely.
#[derive(Debug, Clone)]
pub struct Scanner {
    config: ScanConfig,
    limits: Limits,
}

impl Scanner {
    /// Start building a scanner.
    #[must_use]
    pub fn builder() -> ScannerBuilder {
        ScannerBuilder::default()
    }

    /// Scan one input. "No QR found" is `Ok` with empty detections.
    ///
    /// # Errors
    /// Only real faults: invalid/oversized input (`QRS-001..004`) or
    /// cancellation (`QRS-005`) — never "nothing found".
    pub fn scan(&self, input: ImageInput<'_>) -> Result<ScanReport> {
        self.scan_cancellable(input, &CancelToken::new())
    }

    /// Scan with a cooperative cancellation token (checked between decode
    /// attempts — a running engine call is not interruptible).
    ///
    /// # Errors
    /// Same contract as [`Scanner::scan`], plus `QRS-005` once `cancel` fires.
    pub fn scan_cancellable(
        &self,
        input: ImageInput<'_>,
        cancel: &CancelToken,
    ) -> Result<ScanReport> {
        let planes = transform::normalize(&input, &self.limits)?;
        // ONE wall-clock budget for the whole scan — decode ladder AND
        // scoring share it (attempt/cell-granular: an in-flight engine call
        // is not interruptible, which is why engine inputs are size-capped).
        let deadline = self
            .config
            .budget_ms
            .map(|ms| web_time::Instant::now() + std::time::Duration::from_millis(ms));
        let outcome = ladder::run(&planes, &self.config, cancel, deadline)?;
        // Score the primary (first) detection when the profile asks for it.
        // The scoring stage shares the engine panic posture: a panic in the
        // math path degrades to "no score", never a crash (native unwind;
        // on wasm panics trap regardless — documented limitation).
        let scored = match (outcome.merged.first(), self.config.score_depth) {
            (Some(detection), depth @ (ScoreDepth::Reduced | ScoreDepth::Full)) => {
                let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    score::evaluate(
                        &planes.luma,
                        detection,
                        depth,
                        &self.config.score_skip_axes,
                        cancel,
                        deadline,
                    )
                }));
                match attempt {
                    Ok(result) => result?,
                    Err(_) => None,
                }
            }
            _ => None,
        };
        Ok(build_report(outcome, scored))
    }

    /// Scan a batch — the generator best-of-N gate. Parallel under the
    /// `parallel` feature (results identical to sequential by construction).
    /// Per-input results: one `Err` does not fail the batch.
    #[must_use]
    pub fn scan_batch(&self, inputs: &[ImageInput<'_>]) -> Vec<Result<ScanReport>> {
        #[cfg(feature = "parallel")]
        {
            use rayon::prelude::*;
            inputs.par_iter().map(|input| self.scan(*input)).collect()
        }
        #[cfg(not(feature = "parallel"))]
        {
            inputs.iter().map(|input| self.scan(*input)).collect()
        }
    }
}

impl Default for Scanner {
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Builder for [`Scanner`].
#[derive(Debug, Clone, Default)]
pub struct ScannerBuilder {
    profile: ScanProfile,
    limits: Limits,
}

impl ScannerBuilder {
    /// Select a scan profile (default: [`ScanProfile::Full`]).
    #[must_use]
    pub fn profile(mut self, profile: ScanProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Override anti-DoS input limits.
    #[must_use]
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Build the scanner.
    #[must_use]
    pub fn build(self) -> Scanner {
        Scanner {
            config: self.profile.config(),
            limits: self.limits,
        }
    }
}

/// Assemble the public report from ladder output. Text + charset always come
/// from our own resolution over raw bytes (consistent pair; exotic ECIs are a
/// documented limit — `raw` preserves the truth for consumers).
fn build_report(outcome: ladder::LadderOutcome, scored: Option<(Score, Vec<Hint>)>) -> ScanReport {
    let (score, hints) = match scored {
        Some((score, hints)) => (Some(score), hints),
        None => (None, Vec::new()),
    };
    let detections = outcome
        .merged
        .into_iter()
        .map(|m| {
            // payload routing is symbology-aware: FNC1-led carriers (QR
            // ]Q3/]Q4 · DataMatrix ]d2 · GS1-128 ]C1) are element strings;
            // retail 1D symbols ARE a GTIN (AI 01 semantics); everything
            // else goes through the text classifier
            let payload = if m.fnc1 {
                payload::classify_fnc1(&m.text)
            } else if m.symbology.is_retail_gtin() {
                payload::classify_retail_gtin(&m.text, m.symbology)
            } else {
                payload::classify(&m.text)
            };
            Detection {
                symbology: m.symbology,
                content: DecodedContent {
                    text: m.text,
                    raw: m.raw,
                    charset: m.charset,
                },
                payload,
                corners: m.corners,
                meta: QrMeta {
                    version: m.version,
                    ec_level: m.ec,
                    mask: m.mask,
                    modules: m.version.and_then(QrMeta::modules_per_side),
                    inverted: m.corners.is_some().then_some(m.photometric_inverted),
                },
                engines: m.engines,
            }
        })
        .collect();
    ScanReport {
        detections,
        score,
        hints,
        trace: outcome.trace,
        versions: Versions::current(),
    }
}

#[cfg(test)]
mod tests {
    // The public contract is exercised via the re-exported surface; the
    // workspace deny targets `src/`, so tests opt back into unwrap/expect.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{ImageInput, Limits, Scanner};

    #[test]
    fn builder_threads_custom_limits_into_the_scan() {
        // Traps the builder-plumbing survivor (`ScannerBuilder::limits` →
        // Default::default(), shard-0): a builder that discarded the
        // configured Limits would fall back to the 10_000px / 64MP defaults
        // and wave this input straight through. Pin that BOTH limit fields
        // are threaded from the builder into input validation.
        let data = vec![255u8; 64 * 64];

        // (a) dimension cap: 64px per side > 16px limit → QRS-002
        let strict_dim = Scanner::builder()
            .limits(Limits {
                max_dimension: 16,
                ..Limits::default()
            })
            .build();
        let err = strict_dim
            .scan(ImageInput::luma8(&data, 64, 64))
            .expect_err("64px input must exceed the 16px builder limit");
        assert_eq!(err.code(), "QRS-002", "dimension cap threaded from builder");

        // (b) pixel cap: 4096px total > 100px limit → QRS-003
        let strict_pixels = Scanner::builder()
            .limits(Limits {
                max_pixels: 100,
                ..Limits::default()
            })
            .build();
        let err = strict_pixels
            .scan(ImageInput::luma8(&data, 64, 64))
            .expect_err("4096px input must exceed the 100px builder limit");
        assert_eq!(err.code(), "QRS-003", "pixel cap threaded from builder");

        // Control: the SAME input sails through DEFAULT limits — proving the
        // configured cap, not the input, is what rejected it above (so the
        // builder genuinely threaded the value rather than defaulting it away).
        let report = Scanner::default()
            .scan(ImageInput::luma8(&data, 64, 64))
            .expect("default 10_000px / 64MP limits accept a 64px plane");
        assert!(report.detections.is_empty(), "a blank plane carries no QR");
    }
}
