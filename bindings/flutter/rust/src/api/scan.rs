//! Flutter bindings for `qrcode-ai-scanner` (flutter_rust_bridge v2).
//!
//! Thin wrapper over the Rust core: bytes → scan → the same versioned
//! `ScanReport`, returned to Dart as a JSON **string** (the serde contract, the
//! cross-surface SSOT in `spec/`). FRB runs these on its Rust worker pool, so
//! Dart `await`s them without blocking the UI isolate. The `QRS-xxx` wire code
//! rides in error messages (`[QRS-001]`) — parity with the node/wasm/py/uniffi
//! bindings (the core's `Display` omits it; `.code()` is the stable surface).

use scanner_core::{ImageInput, ScanProfile, Scanner};

fn parse_profile(profile: &str) -> Result<ScanProfile, String> {
    // Canonical parser — the ONE string→enum map every binding reuses.
    ScanProfile::from_name(profile)
        .ok_or_else(|| format!("unknown profile {profile:?} (expected 'full', 'fast', or 'frame')"))
}

/// Decode + score an encoded image (PNG · JPEG · WebP · GIF).
///
/// Returns the `ScanReport` as a JSON string. "No QR found" is a normal result
/// (empty `detections`); `Err` only for invalid input / a bad profile name.
pub fn scan(image: Vec<u8>, profile: String) -> Result<String, String> {
    let profile = parse_profile(&profile)?;
    let report = Scanner::builder()
        .profile(profile)
        .build()
        .scan(ImageInput::encoded(&image))
        .map_err(|e| format!("{e} [{}]", e.code()))?;
    serde_json::to_string(&report).map_err(|e| format!("serialize: {e}"))
}

/// Decode + score a raw RGBA frame (e.g. a camera frame), no format roundtrip.
///
/// `rgba` must be `width * height * 4` bytes.
pub fn scan_frame(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    profile: String,
) -> Result<String, String> {
    let profile = parse_profile(&profile)?;
    let report = Scanner::builder()
        .profile(profile)
        .build()
        .scan(ImageInput::rgba8(&rgba, width, height))
        .map_err(|e| format!("{e} [{}]", e.code()))?;
    serde_json::to_string(&report).map_err(|e| format!("serialize: {e}"))
}

/// Initialise the bridge (default panic handler + logging). The Dart facade
/// calls this once via `RustLib.init()` before the first scan.
#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}
