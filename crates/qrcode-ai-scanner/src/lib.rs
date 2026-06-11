//! # QR Code AI Scanner
//!
//! Decoding + scannability validation for artistic, AI-generated, and
//! photo-captured QR codes — the codes that break standard scanners.
//!
//! Contract (see `docs/plans/2026-06-11-v03-rebuild-design.md` in the repo):
//!
//! - **No QR found is `Ok`** with empty detections — `Err` is reserved for real
//!   faults (corrupt image, invalid buffer, cancellation).
//! - **Deterministic**: same bytes + same config + same versions ⇒ the same
//!   report, bit for bit. No RNG anywhere in the pipeline.
//! - **Sync by design**: async belongs to the bindings (napi/wasm), never here.
//! - **Engine-isolated**: third-party decoder panics are caught at the engine
//!   boundary and recorded in the trace; the ladder continues.

#[cfg(not(any(feature = "engine-rxing", feature = "engine-rqrr")))]
compile_error!(
    "qrcode-ai-scanner requires at least one engine feature: `engine-rxing` or `engine-rqrr`"
);

mod engine;
mod error;
mod input;
mod ladder;
mod payload;
mod report;
mod score;
mod transform;

pub use error::{Result, ScanError};
pub use input::{ImageInput, Limits};
pub use ladder::{CancelToken, ScanConfig, ScanProfile, ScoreDepth};
pub use payload::Payload;
pub use report::{
    AxisScore, Charset, DecodedContent, Detection, EcLevel, EngineKind, Grade, Hint, PipelineTrace,
    Point, QrMeta, ScanReport, Score, StageTrace, StressAxis, StructuralReport, Versions,
};

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
        let outcome = ladder::run(&planes, &self.config, cancel)?;
        // Score the primary (first) detection when the profile asks for it.
        let scored = match (outcome.merged.first(), self.config.score_depth) {
            (Some(detection), depth @ (ScoreDepth::Reduced | ScoreDepth::Full)) => {
                Some(score::evaluate(&planes.luma, detection, depth, cancel)?)
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
            let payload = payload::classify(&m.text);
            Detection {
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
                    modules: m.version.map(|v| v * 4 + 17),
                    mirrored: None,
                    inverted: None,
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
