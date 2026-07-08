//! Spec anti-rot — the golden examples and the JSON Schema cannot drift
//! from the code:
//!
//! 1. every `spec/examples/*.json` must deserialize through the REAL serde
//!    types (a removed/renamed field breaks this),
//! 2. every example must validate against `spec/scan-report.schema.json`
//!    (a schema that lags the wire breaks this),
//! 3. a freshly-scanned report must round-trip serde AND validate too
//!    (a NEW field the schema doesn't know breaks this).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{ImageInput, ScanReport, Scanner};

fn spec_path(rel: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../spec/{rel}"))
}

fn schema() -> jsonschema::Validator {
    let raw = std::fs::read_to_string(spec_path("scan-report.schema.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    jsonschema::validator_for(&doc).expect("spec schema must itself be valid")
}

#[test]
fn golden_examples_deserialize_and_validate() {
    let validator = schema();
    let dir = spec_path("examples");
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap();

        // 1. the real serde types accept it
        let report: ScanReport = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{path:?} no longer matches the serde types: {e}"));

        // 2. the schema accepts it
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|e| format!("{} @ {}", e, e.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{path:?} fails the schema: {errors:#?}");

        // 3. round-trip stability (serialize(deserialize(x)) validates too)
        let round: serde_json::Value = serde_json::to_value(&report).expect("re-serialize golden");
        assert!(
            validator.iter_errors(&round).next().is_none(),
            "{path:?} round-trip drifts from the schema"
        );
        checked += 1;
    }
    assert!(checked >= 5, "expected the golden set, found {checked}");
}

#[test]
fn fresh_scan_validates_against_the_spec_schema() {
    let validator = schema();
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/clean/gen_v5_q.png");
    let bytes = std::fs::read(fixture).unwrap();
    let report = Scanner::default()
        .scan(ImageInput::encoded(&bytes))
        .unwrap();
    let value = serde_json::to_value(&report).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| format!("{} @ {}", e, e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "a live report no longer matches the published schema: {errors:#?}"
    );
}
