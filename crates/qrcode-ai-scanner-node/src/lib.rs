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
use qrcode_ai_scanner::{ImageInput, Limits, ScanProfile, Scanner, ScoreCheck, StressAxis};

/// Server deployments SHOULD lower the input caps (the library default —
/// 10000px / 64MP — is a desktop/CLI posture): pass `maxDimension` /
/// `maxPixels` per call.
fn limits_from(max_dimension: Option<u32>, max_pixels: Option<i64>) -> Limits {
    let mut limits = Limits::default();
    if let Some(dim) = max_dimension {
        limits.max_dimension = dim;
    }
    if let Some(px) = max_pixels
        && let Ok(px) = u64::try_from(px)
    {
        limits.max_pixels = px;
    }
    limits
}

fn profile_from(
    name: Option<&str>,
    budget_ms: Option<u32>,
    score_skip_axes: Option<Vec<String>>,
    score_skip_checks: Option<Vec<String>>,
) -> Result<ScanProfile> {
    let profile = match name {
        None => ScanProfile::Full,
        Some(name) => ScanProfile::from_name(name).ok_or_else(|| {
            Error::from_reason(format!(
                "unknown profile `{name}` — expected full | fast | frame"
            ))
        })?,
    };
    // scoreSkipAxes: axes excluded from scoring, by wire name — the
    // generated-preview integration (their stress cells never run, the
    // composite renormalizes engine-side, the report self-describes).
    // Unknown names fail LOUD: a typo'd axis silently scoring all six
    // would be the worst kind of drift.
    let skip: Vec<StressAxis> = match &score_skip_axes {
        None => Vec::new(),
        Some(names) => names
            .iter()
            .map(|n| {
                StressAxis::from_name(n).ok_or_else(|| {
                    Error::from_reason(format!(
                        "unknown stress axis `{n}` — expected resolution | blur | \
                         contrast | perspective | rotation | lighting"
                    ))
                })
            })
            .collect::<Result<_>>()?,
    };
    // scoreSkipChecks: report SECTIONS excluded at the source (`uec` /
    // `iso15415`) — never computed, wire carries null, UEC-driven hints
    // silent, composite untouched. Same loud-typo posture as the axes.
    let checks: Vec<ScoreCheck> = match &score_skip_checks {
        None => Vec::new(),
        Some(names) => names
            .iter()
            .map(|n| {
                ScoreCheck::from_name(n).ok_or_else(|| {
                    Error::from_reason(format!(
                        "unknown score check `{n}` — expected uec | iso15415"
                    ))
                })
            })
            .collect::<Result<_>>()?,
    };
    // budgetMs overrides the preset's wall-clock budget (0 = unbounded) —
    // server embedders bound tail latency without giving up the deep ladder.
    if budget_ms.is_none() && skip.is_empty() && checks.is_empty() {
        return Ok(profile);
    }
    let mut config = profile.config();
    if let Some(ms) = budget_ms {
        config.budget_ms = (ms > 0).then_some(u64::from(ms));
    }
    config.score_skip_axes = skip;
    config.score_skip_checks = checks;
    Ok(ScanProfile::Custom(config))
}

fn to_json(report: &qrcode_ai_scanner::ScanReport) -> Result<String> {
    serde_json::to_string(report).map_err(|e| Error::from_reason(format!("serialize: {e}")))
}

fn scan_error(e: &qrcode_ai_scanner::ScanError) -> Error {
    // QRS-xxx lands in the message — consumers match `[QRS-...]`
    Error::new(Status::GenericFailure, format!("{} [{}]", e, e.code()))
}

/// One scan job on the libuv pool. The `AbortSignal` cancels QUEUED tasks
/// only (napi semantics); a RUNNING scan is bounded by the whole-scan
/// profile/budget_ms wall clock — in-flight cancellation is NOT wired
/// (wire `scan_cancellable` if sub-budget cancel ever matters).
pub struct ScanTask {
    bytes: Vec<u8>,
    profile: ScanProfile,
    limits: Limits,
}

#[napi]
impl Task for ScanTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> Result<Self::Output> {
        let scanner = Scanner::builder()
            .profile(self.profile.clone())
            .limits(self.limits)
            .build();
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
// allow, not expect: the napi macro re-emits the fn, breaking #[expect]
// fulfillment tracking — the lint fires on the generated item.
#[allow(
    clippy::too_many_arguments,
    reason = "the napi signature IS the cross-binding contract (profile + signal + caps + budget + skips)"
)]
#[napi(js_name = "scanJson")]
pub fn scan(
    image: Buffer,
    profile: Option<String>,
    signal: Option<AbortSignal>,
    max_dimension: Option<u32>,
    max_pixels: Option<i64>,
    budget_ms: Option<u32>,
    score_skip_axes: Option<Vec<String>>,
    score_skip_checks: Option<Vec<String>>,
) -> Result<AsyncTask<ScanTask>> {
    let task = ScanTask {
        bytes: image.to_vec(),
        profile: profile_from(
            profile.as_deref(),
            budget_ms,
            score_skip_axes,
            score_skip_checks,
        )?,
        limits: limits_from(max_dimension, max_pixels),
    };
    Ok(AsyncTask::with_optional_signal(task, signal))
}

/// Synchronous scan returning the report JSON — scripts only.
#[napi(js_name = "scanSyncJson")]
pub fn scan_sync(
    image: Buffer,
    profile: Option<String>,
    max_dimension: Option<u32>,
    max_pixels: Option<i64>,
    budget_ms: Option<u32>,
    score_skip_axes: Option<Vec<String>>,
    score_skip_checks: Option<Vec<String>>,
) -> Result<String> {
    let scanner = Scanner::builder()
        .profile(profile_from(
            profile.as_deref(),
            budget_ms,
            score_skip_axes,
            score_skip_checks,
        )?)
        .limits(limits_from(max_dimension, max_pixels))
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
