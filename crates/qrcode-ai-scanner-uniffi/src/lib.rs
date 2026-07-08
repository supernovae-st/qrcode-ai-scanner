//! Kotlin/Android + Swift/iOS bindings for `qrcode-ai-scanner` (UniFFI).
//!
//! Thin wrapper over the Rust core: bytes → scan → the same versioned
//! `ScanReport`, returned as a JSON **String** (serialized via the core's serde
//! contract, the cross-surface SSOT in `spec/`). Consumers parse the string
//! against `spec/scan-report.schema.json` — identical wire format to the
//! Python / Node / WASM bindings. The scan itself is synchronous (the core is
//! sync by design); callers move it off the main thread on their side.

use scanner_core::{ImageInput, Limits, ScanProfile, Scanner, StressAxis};

uniffi::setup_scaffolding!();

/// Errors crossing the FFI. UniFFI requires an enum that implements
/// `std::error::Error`; the core's own QRS-XXX codes ride inside the message.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ScanBindingError {
    /// Unknown profile name (not "full" / "fast" / "frame").
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    /// Unknown stress-axis wire name in `score_skip_axes`.
    #[error(
        "unknown stress axis: {0} — expected resolution | blur | contrast | \
         perspective | rotation | lighting"
    )]
    UnknownStressAxis(String),
    /// A real scan fault (invalid/oversized buffer, cancellation).
    #[error("scan failed: {0}")]
    ScanFailed(String),
}

fn parse_profile(profile: &str) -> Result<ScanProfile, ScanBindingError> {
    // Canonical parser — same path as every other binding.
    ScanProfile::from_name(profile)
        .ok_or_else(|| ScanBindingError::UnknownProfile(profile.to_string()))
}

fn profile_from(
    profile: &str,
    budget_ms: Option<u64>,
    score_skip_axes: Option<Vec<String>>,
) -> Result<ScanProfile, ScanBindingError> {
    let profile = parse_profile(profile)?;
    // score_skip_axes: axes excluded from scoring, by wire name — the
    // generated-preview integration (spec/04 § skipping axes): their cells
    // never run and the composite renormalizes engine-side. Unknown names
    // fail LOUD — a typo silently scoring all six would be silent drift.
    let skip: Vec<StressAxis> = match &score_skip_axes {
        None => Vec::new(),
        Some(names) => names
            .iter()
            .map(|n| {
                StressAxis::from_name(n)
                    .ok_or_else(|| ScanBindingError::UnknownStressAxis(n.clone()))
            })
            .collect::<Result<_, _>>()?,
    };
    // budget_ms overrides the preset's wall-clock budget (0 = unbounded, NOT a
    // zero-millisecond budget — spec/02) — mobile embedders bound tail latency
    // on their scan thread without giving up the deep ladder.
    if budget_ms.is_none() && skip.is_empty() {
        return Ok(profile);
    }
    let mut config = profile.config();
    if let Some(ms) = budget_ms {
        config.budget_ms = (ms > 0).then_some(ms);
    }
    config.score_skip_axes = skip;
    Ok(ScanProfile::Custom(config))
}

/// Optional input caps from the caller. Mobile embedders SHOULD lower them
/// (the library default — 10000 px / 64 MP — is a desktop/CLI posture);
/// violations reject BEFORE any pixel work (QRS-002 / QRS-003).
fn limits_from(max_dimension: Option<u32>, max_pixels: Option<u64>) -> Limits {
    let mut limits = Limits::default();
    if let Some(dim) = max_dimension {
        limits.max_dimension = dim;
    }
    if let Some(px) = max_pixels {
        limits.max_pixels = px;
    }
    limits
}

/// Decode + score an encoded image (PNG · JPEG · WebP · GIF).
///
/// Returns the `ScanReport` as a JSON string. "No QR found" is a normal result
/// (empty `detections`); an error is raised only for invalid input / bad
/// profile / cancellation.
///
/// `max_dimension` / `max_pixels` cap the input before any pixel work
/// (defaults 10000 px / 64 MP — lower them on server/mobile hardening paths).
/// `budget_ms` overrides the profile's wall-clock budget (0 = unbounded); the
/// scan is synchronous, so a UI embedder bounds its scan thread with it.
#[uniffi::export(default(profile = "full", max_dimension = None, max_pixels = None, budget_ms = None, score_skip_axes = None))]
pub fn scan(
    image: Vec<u8>,
    profile: String,
    max_dimension: Option<u32>,
    max_pixels: Option<u64>,
    budget_ms: Option<u64>,
    score_skip_axes: Option<Vec<String>>,
) -> Result<String, ScanBindingError> {
    let report = Scanner::builder()
        .profile(profile_from(&profile, budget_ms, score_skip_axes)?)
        .limits(limits_from(max_dimension, max_pixels))
        .build()
        .scan(ImageInput::encoded(&image))
        .map_err(|e| ScanBindingError::ScanFailed(format!("{} [{}]", e, e.code())))?;
    serde_json::to_string(&report).map_err(|e| ScanBindingError::ScanFailed(e.to_string()))
}

/// Decode + score a raw RGBA frame (e.g. a camera frame), no format roundtrip.
///
/// `rgba` must be `width * height * 4` bytes. `max_dimension` / `max_pixels`
/// cap attacker-controlled dimensions before any pixel work; `budget_ms`
/// overrides the profile's wall-clock budget (0 = unbounded) — the camera
/// loop's per-frame bound.
#[uniffi::export(default(profile = "frame", max_dimension = None, max_pixels = None, budget_ms = None, score_skip_axes = None))]
#[expect(
    clippy::too_many_arguments,
    reason = "the FFI signature IS the cross-binding contract (dims + profile + caps + budget + axis skip)"
)]
pub fn scan_frame(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    profile: String,
    max_dimension: Option<u32>,
    max_pixels: Option<u64>,
    budget_ms: Option<u64>,
    score_skip_axes: Option<Vec<String>>,
) -> Result<String, ScanBindingError> {
    let report = Scanner::builder()
        .profile(profile_from(&profile, budget_ms, score_skip_axes)?)
        .limits(limits_from(max_dimension, max_pixels))
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
            assert!(
                obj.contains_key(key),
                "missing required key `{key}` in {json}"
            );
        }
        assert!(v["detections"].is_array(), "detections must be an array");
        v
    }

    #[test]
    fn scan_clean_fixture_decodes_to_envelope_with_text() {
        let v = assert_envelope(&scan(clean_qr(), "full".into(), None, None, None, None).unwrap());
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
            let out = scan(clean_qr(), profile.into(), None, None, None, None)
                .unwrap_or_else(|e| panic!("profile `{profile}` should scan: {e}"));
            assert_envelope(&out);
        }
    }

    #[test]
    fn unknown_profile_is_a_typed_error_not_a_scan() {
        let err = scan(clean_qr(), "turbo".into(), None, None, None, None).unwrap_err();
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
        let err = scan(
            vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01],
            "full".into(),
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        let ScanBindingError::ScanFailed(msg) = err else {
            panic!("expected ScanFailed, got {err:?}");
        };
        assert!(
            msg.contains("[QRS-001]"),
            "QRS code must ride in the message: {msg}"
        );
    }

    #[test]
    fn scan_frame_buffer_mismatch_maps_to_scanfailed_never_panic() {
        // 2x2 RGBA needs 16 bytes; pass 4. The core validates the buffer length
        // and returns an error — the binding surfaces it, never panics.
        let err = scan_frame(vec![0u8; 4], 2, 2, "frame".into(), None, None, None, None).unwrap_err();
        assert!(
            matches!(err, ScanBindingError::ScanFailed(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn scan_frame_zero_dimension_maps_to_scanfailed_never_panic() {
        let err = scan_frame(Vec::new(), 0, 0, "frame".into(), None, None, None, None).unwrap_err();
        assert!(
            matches!(err, ScanBindingError::ScanFailed(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn scan_frame_blank_buffer_is_no_qr_not_an_error() {
        // A valid all-white 16x16 RGBA frame decodes to "nothing found" — a
        // normal empty-detections report, NOT an error.
        let out = scan_frame(
            vec![0xFFu8; 16 * 16 * 4],
            16,
            16,
            "frame".into(),
            None,
            None,
            None,
            None,
        )
        .expect("a valid blank frame is not an error");
        let v = assert_envelope(&out);
        assert!(
            v["detections"].as_array().unwrap().is_empty(),
            "a blank frame finds no QR"
        );
    }

    #[test]
    fn tight_dimension_cap_rejects_with_qrs_002() {
        // The clean fixture is far larger than 16 px — the cap must reject
        // BEFORE any pixel work, with the dimension-limit wire code.
        let err = scan(clean_qr(), "full".into(), Some(16), None, None, None).unwrap_err();
        let ScanBindingError::ScanFailed(msg) = err else {
            panic!("expected ScanFailed, got {err:?}");
        };
        assert!(
            msg.contains("[QRS-002]"),
            "dimension-cap code must ride in the message: {msg}"
        );
    }

    #[test]
    fn tight_pixel_cap_rejects_with_qrs_003() {
        let err = scan(clean_qr(), "full".into(), None, Some(64), None, None).unwrap_err();
        let ScanBindingError::ScanFailed(msg) = err else {
            panic!("expected ScanFailed, got {err:?}");
        };
        assert!(
            msg.contains("[QRS-003]"),
            "pixel-cap code must ride in the message: {msg}"
        );
    }

    #[test]
    fn budget_zero_is_unbounded_and_still_decodes() {
        // 0 = unbounded (NOT a zero-millisecond budget) — the cross-binding
        // convention from spec/02. Deterministic: no wall-clock dependence.
        let v = assert_envelope(&scan(clean_qr(), "full".into(), None, None, Some(0), None).unwrap());
        assert_eq!(v["detections"].as_array().unwrap().len(), 1);
        assert!(
            v["score"].is_object(),
            "full profile must still score unbounded"
        );
    }

    #[test]
    fn generous_budget_keeps_the_full_contract() {
        // A 10-minute budget cannot cut a clean-fixture scan: asserts the
        // budget PLUMBING (the Custom-profile path) without wall-clock flake.
        let json = scan(clean_qr(), "full".into(), None, None, Some(600_000), None).unwrap();
        let v = assert_envelope(&json);
        assert_eq!(v["detections"].as_array().unwrap().len(), 1);
        assert!(v["score"].is_object());
    }

    #[test]
    fn frame_caps_apply_to_raw_frames_too() {
        // Raw-plane inputs carry attacker-controlled dimensions — the caps
        // must bind on scan_frame exactly as on scan.
        let rgba = vec![0xFFu8; 64 * 64 * 4];
        let err = scan_frame(rgba, 64, 64, "frame".into(), Some(16), None, None, None).unwrap_err();
        let ScanBindingError::ScanFailed(msg) = err else {
            panic!("expected ScanFailed, got {err:?}");
        };
        assert!(msg.contains("[QRS-002]"), "got: {msg}");
    }

    /// score_skip_axes parity: skipped axes are absent from the wire and an
    /// unknown name is a LOUD typed error (never a silent six-axis score).
    #[test]
    fn score_skip_axes_thread_through_and_reject_typos() {
        let v = assert_envelope(
            &scan(
                clean_qr(),
                "full".into(),
                None,
                None,
                None,
                Some(vec!["perspective".into(), "rotation".into()]),
            )
            .unwrap(),
        );
        let axes = v["score"]["axes"].as_array().unwrap();
        assert_eq!(axes.len(), 4, "six axes minus the two skipped: {axes:?}");
        assert!(
            axes.iter().all(|a| {
                let name = a["axis"].as_str().unwrap();
                name != "perspective" && name != "rotation"
            }),
            "skipped axes absent from the wire: {axes:?}"
        );
        let err = scan(
            clean_qr(),
            "full".into(),
            None,
            None,
            None,
            Some(vec!["perspektive".into()]),
        )
        .unwrap_err();
        assert!(
            matches!(err, ScanBindingError::UnknownStressAxis(ref n) if n == "perspektive"),
            "typo must be the typed loud error, got {err:?}"
        );
    }
}
