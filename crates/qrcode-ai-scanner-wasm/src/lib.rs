//! Browser bindings — `@supernovae-st/qrcode-ai-scanner-wasm`.
//!
//! Two entry points: `scan_image` (encoded bytes — upload/verify flows) and
//! `scan_frame` (raw RGBA from `ImageData` — live camera, no PNG roundtrip).
//! Both return the full `ScanReport` contract as a JS object (snake_case,
//! `raw` as base64) — identical shape to the server/CLI surfaces.

use qrcode_ai_scanner::{ImageInput, Limits, ScanConfig, ScanProfile, Scanner, StressAxis};
use serde::Serialize as _;
use wasm_bindgen::prelude::*;

fn config_from(
    name: Option<String>,
    budget_ms: Option<f64>,
    score_skip_axes: Option<Vec<String>>,
) -> Result<ScanProfile, JsError> {
    let profile = match name.as_deref() {
        None => ScanProfile::Full,
        Some(name) => ScanProfile::from_name(name).ok_or_else(|| {
            JsError::new(&format!(
                "unknown profile `{name}` — expected full | fast | frame"
            ))
        })?,
    };
    // Integration config: axes excluded from scoring, by wire name
    // (`perspective`, `rotation`, …). A generated-preview host skips the
    // capture-geometry axes — their cells never run (faster verify) and the
    // composite renormalizes engine-side. Unknown names fail LOUD: a typo'd
    // axis silently scoring all six would be the worst kind of drift.
    let skip: Vec<StressAxis> = match &score_skip_axes {
        None => Vec::new(),
        Some(names) => names
            .iter()
            .map(|n| {
                StressAxis::from_name(n).ok_or_else(|| {
                    JsError::new(&format!(
                        "unknown stress axis `{n}` — expected resolution | blur | \
                         contrast | perspective | rotation | lighting"
                    ))
                })
            })
            .collect::<Result<_, _>>()?,
    };
    // wasm runs the scan ON the caller's thread (browser main thread unless
    // the embedder uses a worker) — budget control is how a verify-while-
    // typing UI keeps the worst case bounded without giving up the deep
    // ladder. 0/negative = unbounded.
    if budget_ms.is_none() && skip.is_empty() {
        return Ok(profile);
    }
    let mut config: ScanConfig = profile.config();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "JS number → milliseconds; non-positive means unbounded"
    )]
    if let Some(ms) = budget_ms {
        config.budget_ms = (ms > 0.0).then_some(ms as u64);
    }
    config.score_skip_axes = skip;
    Ok(ScanProfile::Custom(config))
}

fn limits_from(max_dimension: Option<u32>, max_pixels: Option<f64>) -> Limits {
    let mut limits = Limits::default();
    if let Some(dim) = max_dimension {
        limits.max_dimension = dim;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "JS number → pixel count; negative/fractional clamp to 0 is fine"
    )]
    if let Some(px) = max_pixels
        && px > 0.0
    {
        limits.max_pixels = px as u64;
    }
    limits
}

fn run_scan(
    input: ImageInput<'_>,
    profile: ScanProfile,
    max_dimension: Option<u32>,
    max_pixels: Option<f64>,
) -> Result<JsValue, JsError> {
    let scanner = Scanner::builder()
        .profile(profile)
        .limits(limits_from(max_dimension, max_pixels))
        .build();
    let report = scanner
        .scan(input)
        .map_err(|e| JsError::new(&format!("{} [{}]", e, e.code())))?;
    // None → null (not undefined): one contract across wasm/napi/CLI surfaces
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
    report
        .serialize(&serializer)
        .map_err(|e| JsError::new(&e.to_string()))
}

/// Scan encoded image bytes (PNG/JPEG/WebP/GIF). Profile: full | fast |
/// frame. `budget_ms` overrides the profile's wall-clock budget (the scan
/// runs synchronously on the calling thread — bound it in UI contexts).
/// `score_skip_axes` (wire names, e.g. `["perspective","rotation"]`)
/// excludes axes from scoring: their cells never run and the composite
/// renormalizes engine-side — the generated-preview integration config.
#[wasm_bindgen]
pub fn scan_image(
    bytes: &[u8],
    profile: Option<String>,
    max_dimension: Option<u32>,
    max_pixels: Option<f64>,
    budget_ms: Option<f64>,
    score_skip_axes: Option<Vec<String>>,
) -> Result<JsValue, JsError> {
    run_scan(
        ImageInput::encoded(bytes),
        config_from(profile, budget_ms, score_skip_axes)?,
        max_dimension,
        max_pixels,
    )
}

/// Scan a raw RGBA8 frame (`ImageData.data`, width, height). Defaults to the
/// `frame` profile (decode-only, tight budget) — pass another to override.
/// `budget_ms` overrides the profile's wall-clock budget.
#[wasm_bindgen]
pub fn scan_frame(
    data: &[u8],
    width: u32,
    height: u32,
    profile: Option<String>,
    budget_ms: Option<f64>,
) -> Result<JsValue, JsError> {
    let profile = match profile {
        Some(_) => config_from(profile, budget_ms, None)?,
        None => config_from(Some("frame".to_owned()), budget_ms, None)?,
    };
    run_scan(ImageInput::rgba8(data, width, height), profile, None, None)
}

/// Crate version (the `versions.scanner` of every report).
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
