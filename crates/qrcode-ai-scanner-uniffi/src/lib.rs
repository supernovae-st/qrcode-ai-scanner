//! Kotlin/Android + Swift/iOS bindings for `qrcode-ai-scanner` (UniFFI).
//!
//! Thin wrapper over the Rust core: bytes → scan → the same versioned
//! `ScanReport`, returned as a JSON **String** (serialized via the core's serde
//! contract, the cross-surface SSOT in `spec/`). Consumers parse the string
//! against `spec/scan-report.schema.json` — identical wire format to the
//! Python / Node / WASM bindings. The scan itself is synchronous (the core is
//! sync by design); callers move it off the main thread on their side.

use scanner_core::{ImageInput, ScanProfile, Scanner};

uniffi::setup_scaffolding!();

/// Errors crossing the FFI. UniFFI requires an enum that implements
/// `std::error::Error`; the core's own QRS-XXX codes ride inside the message.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ScanBindingError {
    /// Unknown profile name (not "full" / "fast" / "frame").
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    /// A real scan fault (invalid/oversized buffer, cancellation).
    #[error("scan failed: {0}")]
    ScanFailed(String),
}

fn parse_profile(profile: &str) -> Result<ScanProfile, ScanBindingError> {
    // Canonical parser — same path as every other binding.
    ScanProfile::from_name(profile).ok_or_else(|| ScanBindingError::UnknownProfile(profile.to_string()))
}

/// Decode + score an encoded image (PNG · JPEG · WebP · GIF).
///
/// Returns the `ScanReport` as a JSON string. "No QR found" is a normal result
/// (empty `detections`); an error is raised only for invalid input / bad
/// profile / cancellation.
#[uniffi::export(default(profile = "full"))]
pub fn scan(image: Vec<u8>, profile: String) -> Result<String, ScanBindingError> {
    let profile = parse_profile(&profile)?;
    let report = Scanner::builder()
        .profile(profile)
        .build()
        .scan(ImageInput::encoded(&image))
        .map_err(|e| ScanBindingError::ScanFailed(e.to_string()))?;
    serde_json::to_string(&report).map_err(|e| ScanBindingError::ScanFailed(e.to_string()))
}

/// Decode + score a raw RGBA frame (e.g. a camera frame), no format roundtrip.
///
/// `rgba` must be `width * height * 4` bytes.
#[uniffi::export(default(profile = "frame"))]
pub fn scan_frame(rgba: Vec<u8>, width: u32, height: u32, profile: String) -> Result<String, ScanBindingError> {
    let profile = parse_profile(&profile)?;
    let report = Scanner::builder()
        .profile(profile)
        .build()
        .scan(ImageInput::rgba8(&rgba, width, height))
        .map_err(|e| ScanBindingError::ScanFailed(e.to_string()))?;
    serde_json::to_string(&report).map_err(|e| ScanBindingError::ScanFailed(e.to_string()))
}
