//! Browser bindings — `@supernovae-st/qrcode-ai-scanner-wasm`.
//!
//! Two entry points: `scan_image` (encoded bytes — upload/verify flows) and
//! `scan_frame` (raw RGBA from `ImageData` — live camera, no PNG roundtrip).
//! Both return the full `ScanReport` contract as a JS object (snake_case,
//! `raw` as base64) — identical shape to the server/CLI surfaces.

use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};
use wasm_bindgen::prelude::*;

fn profile_from(name: Option<String>) -> Result<ScanProfile, JsError> {
    match name.as_deref() {
        None | Some("full") => Ok(ScanProfile::Full),
        Some("fast") => Ok(ScanProfile::Fast),
        Some("frame") => Ok(ScanProfile::Frame),
        Some(other) => Err(JsError::new(&format!(
            "unknown profile `{other}` — expected full | fast | frame"
        ))),
    }
}

fn run_scan(input: ImageInput<'_>, profile: Option<String>) -> Result<JsValue, JsError> {
    let scanner = Scanner::builder().profile(profile_from(profile)?).build();
    let report = scanner
        .scan(input)
        .map_err(|e| JsError::new(&format!("{} ({})", e, e.code())))?;
    serde_wasm_bindgen::to_value(&report).map_err(|e| JsError::new(&e.to_string()))
}

/// Scan encoded image bytes (PNG/JPEG/WebP/GIF). Profile: full | fast | frame.
#[wasm_bindgen]
pub fn scan_image(bytes: &[u8], profile: Option<String>) -> Result<JsValue, JsError> {
    run_scan(ImageInput::encoded(bytes), profile)
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
    let profile = profile.or_else(|| Some("frame".to_owned()));
    run_scan(ImageInput::rgba8(data, width, height), profile)
}

/// Crate version (the `versions.scanner` of every report).
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
