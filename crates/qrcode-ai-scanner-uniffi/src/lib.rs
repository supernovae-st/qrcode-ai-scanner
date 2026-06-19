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
        .map_err(|e| ScanBindingError::ScanFailed(format!("{} [{}]", e, e.code())))?;
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
        .map_err(|e| ScanBindingError::ScanFailed(format!("{} [{}]", e, e.code())))?;
    serde_json::to_string(&report).map_err(|e| ScanBindingError::ScanFailed(e.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::Value;

    // The clean v5-Q QR fixture the core's spec_golden test also uses. The crate
    // is workspace-EXCLUDED, so CARGO_MANIFEST_DIR is crates/qrcode-ai-scanner-uniffi.
    fn clean_qr() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/clean/gen_v5_q.png"
        ))
        .expect("clean fixture present")
    }

    // Every binding emits the same 5-key ScanReport envelope
    // (spec/scan-report.schema.json). Assert the contract structurally, not by
    // `!is_empty()`.
    fn assert_envelope(json: &str) -> Value {
        let v: Value = serde_json::from_str(json).expect("binding output is valid JSON");
        let obj = v.as_object().expect("report is a JSON object");
        for key in ["detections", "score", "hints", "trace", "versions"] {
            assert!(obj.contains_key(key), "missing required key `{key}` in {json}");
        }
        assert!(v["detections"].is_array(), "detections must be an array");
        v
    }

    #[test]
    fn scan_clean_fixture_decodes_to_envelope_with_text() {
        let v = assert_envelope(&scan(clean_qr(), "full".into()).unwrap());
        let dets = v["detections"].as_array().unwrap();
        assert_eq!(dets.len(), 1, "single-QR fixture → exactly one detection");
        let text = dets[0]["content"]["text"]
            .as_str()
            .expect("decoded text is a string");
        assert!(!text.is_empty(), "a clean QR must decode to non-empty text");
        // full profile scores the primary detection (frame leaves it null).
        assert!(v["score"].is_object(), "full profile must populate score");
    }

    #[test]
    fn all_wire_profiles_parse_and_keep_the_envelope() {
        // The cross-binding profile contract: full · fast · frame are the only
        // valid names, and all three emit the 5-key envelope (frame → score:null,
        // which the schema's anyOf allows).
        for profile in ["full", "fast", "frame"] {
            let out = scan(clean_qr(), profile.into())
                .unwrap_or_else(|e| panic!("profile `{profile}` should scan: {e}"));
            assert_envelope(&out);
        }
    }

    #[test]
    fn unknown_profile_is_a_typed_error_not_a_scan() {
        let err = scan(clean_qr(), "turbo".into()).unwrap_err();
        match err {
            ScanBindingError::UnknownProfile(name) => assert_eq!(name, "turbo"),
            other => panic!("expected UnknownProfile, got {other:?}"),
        }
    }

    #[test]
    fn garbage_bytes_map_to_scanfailed_with_qrs_code() {
        // Not a decodable image → a real fault, surfaced as ScanFailed. The FFI
        // boundary must never unwind a panic across it, AND the QRS-xxx wire code
        // must ride in the message (parity with the node/wasm/flutter bindings —
        // ScanError's Display omits the code, so the binding appends `[QRS-xxx]`).
        let err = scan(vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01], "full".into()).unwrap_err();
        let ScanBindingError::ScanFailed(msg) = err else {
            panic!("expected ScanFailed, got {err:?}");
        };
        assert!(msg.contains("[QRS-001]"), "QRS code must ride in the message: {msg}");
    }

    #[test]
    fn scan_frame_buffer_mismatch_maps_to_scanfailed_never_panic() {
        // 2x2 RGBA needs 16 bytes; pass 4. The core validates the buffer length
        // and returns an error — the binding surfaces it, never panics.
        let err = scan_frame(vec![0u8; 4], 2, 2, "frame".into()).unwrap_err();
        assert!(matches!(err, ScanBindingError::ScanFailed(_)), "got {err:?}");
    }

    #[test]
    fn scan_frame_zero_dimension_maps_to_scanfailed_never_panic() {
        let err = scan_frame(Vec::new(), 0, 0, "frame".into()).unwrap_err();
        assert!(matches!(err, ScanBindingError::ScanFailed(_)), "got {err:?}");
    }

    #[test]
    fn scan_frame_blank_buffer_is_no_qr_not_an_error() {
        // A valid all-white 16x16 RGBA frame decodes to "nothing found" — a
        // normal empty-detections report, NOT an error.
        let out = scan_frame(vec![0xFFu8; 16 * 16 * 4], 16, 16, "frame".into())
            .expect("a valid blank frame is not an error");
        let v = assert_envelope(&out);
        assert!(
            v["detections"].as_array().unwrap().is_empty(),
            "a blank frame finds no QR"
        );
    }
}
