//! Node bindings — `@supernovae-st/qrcode-ai-scanner`.
//!
//! CPU-bound scans run as `AsyncTask` on the libuv pool: the event loop
//! never blocks, even on a 2-4s artistic-QR Full scan. `AbortSignal`
//! cancels queued tasks; once running, cancellation is cooperative
//! (checked between decode attempts).
//!
//! Reports cross the boundary as JSON strings (the exact serde contract,
//! snake_case, `raw` as base64) — the index.js wrapper parses them. One
//! source of truth; a JSON.parse is microseconds against a 5-4000ms scan.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};

fn profile_from(name: Option<&str>) -> Result<ScanProfile> {
    match name {
        None | Some("full") => Ok(ScanProfile::Full),
        Some("fast") => Ok(ScanProfile::Fast),
        Some("frame") => Ok(ScanProfile::Frame),
        Some(other) => Err(Error::from_reason(format!(
            "unknown profile `{other}` — expected full | fast | frame"
        ))),
    }
}

fn to_json(report: &qrcode_ai_scanner::ScanReport) -> Result<String> {
    serde_json::to_string(report).map_err(|e| Error::from_reason(format!("serialize: {e}")))
}

fn scan_error(e: &qrcode_ai_scanner::ScanError) -> Error {
    // QRS-xxx lands in the message — consumers match `[QRS-...]`
    Error::new(Status::GenericFailure, format!("{} [{}]", e, e.code()))
}

/// One scan job on the libuv pool. The `AbortSignal` cancels QUEUED tasks
/// (napi semantics); a RUNNING scan is bounded by the profile budget
/// (≤4s Full · ≤800ms Fast · ≤80ms Frame), so in-flight cancellation
/// is not wired — budgets make it moot.
pub struct ScanTask {
    bytes: Vec<u8>,
    profile: ScanProfile,
}

#[napi]
impl Task for ScanTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        let scanner = Scanner::builder().profile(self.profile).build();
        let report = scanner
            .scan(ImageInput::encoded(&self.bytes))
            .map_err(|e| scan_error(&e))?;
        to_json(&report)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// Scan encoded image bytes asynchronously (libuv pool — never blocks the
/// event loop). Returns the ScanReport as a JSON string (index.js parses).
/// `profile`: full | fast | frame (default full). The promise rejects with
/// `[QRS-xxx]`-tagged errors; "no QR found" RESOLVES with empty detections.
#[napi(js_name = "scanJson")]
pub fn scan(
    image: Buffer,
    profile: Option<String>,
    signal: Option<AbortSignal>,
) -> Result<AsyncTask<ScanTask>> {
    let task = ScanTask {
        bytes: image.to_vec(),
        profile: profile_from(profile.as_deref())?,
    };
    Ok(AsyncTask::with_optional_signal(task, signal))
}

/// Synchronous scan returning the report JSON — scripts only.
#[napi(js_name = "scanSyncJson")]
pub fn scan_sync(image: Buffer, profile: Option<String>) -> Result<String> {
    let scanner = Scanner::builder()
        .profile(profile_from(profile.as_deref())?)
        .build();
    let report = scanner
        .scan(ImageInput::encoded(&image))
        .map_err(|e| scan_error(&e))?;
    to_json(&report)
}

/// Native crate version.
#[napi]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
