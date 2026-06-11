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

mod error;
mod input;
mod payload;
mod report;

pub use error::{Result, ScanError};
pub use input::{ImageInput, Limits};
pub use payload::Payload;
pub use report::{
    Charset, DecodedContent, Detection, EcLevel, EngineKind, Grade, Hint, PipelineTrace, Point,
    QrMeta, ScanReport, Score, StageTrace, Versions,
};
