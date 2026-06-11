//! Browser bindings — `@supernovae-st/qrcode-ai-scanner-wasm`.
//!
//! Two entry points: `scan_image` (encoded bytes — upload/verify flows) and
//! `scan_frame` (raw RGBA from `ImageData` — live camera, no PNG roundtrip).
//! Both return the full `ScanReport` contract as a JS object (snake_case,
//! `raw` as base64) — identical shape to the server/CLI surfaces.

use qrcode_ai_scanner::{ImageInput, Limits, ScanProfile, Scanner};
use serde::Serialize as _;
use wasm_bindgen::prelude::*;

fn profile_from(name: Option<String>) -> Result<ScanProfile, JsError> {
    match name.as_deref() {
        None => Ok(ScanProfile::Full),
        Some(name) => ScanProfile::from_name(name).ok_or_else(|| {
            JsError::new(&format!(
                "unknown profile `{name}` — expected full | fast | frame"
            ))
        }),
    }
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
    if let Some(px) = max_pixels {
        limits.max_pixels = px.max(0.0) as u64;
    }
    limits
}

fn run_scan(
    input: ImageInput<'_>,
    profile: Option<String>,
    max_dimension: Option<u32>,
    max_pixels: Option<f64>,
) -> Result<JsValue, JsError> {
    let scanner = Scanner::builder()
        .profile(profile_from(profile)?)
        .limits(limits_from(max_dimension, max_pixels))
        .build();
    let report = scanner
        .scan(input)
        .map_err(|e| JsError::new(&format!("{} ({})", e, e.code())))?;
    // None → null (not undefined): one contract across wasm/napi/CLI surfaces
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
    report
        .serialize(&serializer)
        .map_err(|e| JsError::new(&e.to_string()))
}

/// Scan encoded image bytes (PNG/JPEG/WebP/GIF). Profile: full | fast | frame.
#[wasm_bindgen]
pub fn scan_image(
    bytes: &[u8],
    profile: Option<String>,
    max_dimension: Option<u32>,
    max_pixels: Option<f64>,
) -> Result<JsValue, JsError> {
    run_scan(
        ImageInput::encoded(bytes),
        profile,
        max_dimension,
        max_pixels,
    )
}

/// Scan a raw RGBA8 frame (`ImageData.data`, width, height). Defaults to the
/// `frame` profile (decode-only, tight budget) — pass another to override.
#[wasm_bindgen]
pub fn scan_frame(
    data: &[u8],
    width: u32,
    height: u32,
    profile: Option<String>,
) -> Result<JsValue, JsError> {
    match profile {
        Some(_) => run_scan(ImageInput::rgba8(data, width, height), profile, None, None),
        None => {
            let scanner = Scanner::builder().profile(ScanProfile::Frame).build();
            let report = scanner
                .scan(ImageInput::rgba8(data, width, height))
                .map_err(|e| JsError::new(&format!("{} ({})", e, e.code())))?;
            let serializer = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
            report
                .serialize(&serializer)
                .map_err(|e| JsError::new(&e.to_string()))
        }
    }
}

/// Crate version (the `versions.scanner` of every report).
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
