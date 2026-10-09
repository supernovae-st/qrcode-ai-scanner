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

/// A validator for one `$defs` entry of the spec schema.
fn definition(name: &str) -> jsonschema::Validator {
    let raw = std::fs::read_to_string(spec_path("scan-report.schema.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let wrapper = serde_json::json!({
        "$schema": doc["$schema"],
        "$defs": doc["$defs"],
        "$ref": format!("#/$defs/{name}"),
    });
    jsonschema::validator_for(&wrapper).expect("a definition of the spec schema")
}

/// Content test objects (each normative wire value once) and the schema's
/// negatives.
const CONTENT_OBJECTS: &str = r#"{"valid":[
{"text":"\u0104","raw":"oQ==","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"\u041f\u0440\u0438\u0432\u0435\u0442","raw":"v+DY0tXi","charset":"eci","eci":[{"designator":7,"start":0}]},
{"text":"\u0104\u20ac","raw":"oeKCrA==","charset":"eci","eci":[{"designator":4,"start":0},{"designator":26,"start":1}]},
{"text":"12345\u0104","raw":"MTIzNDWh","charset":"eci","eci":[{"designator":4,"start":5}]},
{"text":"5\u0080","raw":"NYA=","charset":"eci","eci":[{"designator":3,"start":0}]},
{"text":"\ufffd","raw":"oQ==","charset":"unknown","eci":[{"designator":999,"start":0}]},
{"text":"\ufffd","raw":"oQ==","charset":"unknown","eci":[{"designator":26,"start":0}]},
{"text":"\u00e9\ufffd\ufffd\ufffd\ufffd\u0242\ufffd\ufffd\ufffd","raw":"w6mCsYLxgsmCv4LN","charset":"unknown","eci":[{"designator":26,"start":0}]},
{"text":"\u3042","raw":"44GC","charset":"utf8","eci":[{"designator":26,"start":0}]},
{"text":"\ufffd\ufffd\ufffd\ufffd","raw":"AP+Afw==","charset":"unknown","eci":[{"designator":899,"start":0}]},
{"text":"\ufffd\ufffd\ufffd","raw":"YWJj","charset":"unknown","eci":[{"designator":999999,"start":0}]},
{"text":"\u0104","raw":"xIQ=","charset":"utf8","eci":[{"designator":26,"start":0}]},
{"text":"\u65e5\u672c","raw":"k/qWew==","charset":"shift_jis","eci":[{"designator":20,"start":0}]},
{"text":"\u00a1","raw":"oQ==","charset":"latin1","eci":[{"designator":3,"start":0}]},
{"text":"\u00a1","raw":"oQ==","charset":"latin1"},
{"text":"\u00e9","raw":"w6k=","charset":"utf8"},
{"text":"\u00e9","raw":"6Q==","charset":"latin1"},
{"text":"https://qrc-ai.com/K1Ng2","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL0sxTmcy","charset":"utf8","eci":[{"designator":26,"start":0}]},
{"text":"https://qrc-ai.com/caf\u00e9","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL2NhZsOp","charset":"utf8","eci":[{"designator":26,"start":0}]},
{"text":"37abc","raw":"MzdhYmM=","charset":"utf8","fnc1":"second"},
{"text":"\ufeffA","raw":"/v8AQQ==","charset":"eci","eci":[{"designator":25,"start":0}]},
{"text":"\ufeffA","raw":"77u/QQ==","charset":"utf8","eci":[{"designator":26,"start":0}]},
{"text":"\ufeffA","raw":"//5BAA==","charset":"eci","eci":[{"designator":33,"start":0}]},
{"text":"\ufffd","raw":"n/0=","charset":"unknown"},
{"text":"\u009f\u00fd","raw":"n/0=","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"\ufffd\ufffd","raw":"n/0=","charset":"unknown","eci":[{"designator":26,"start":0}]},
{"text":"\u0104\u009f\u00fd","raw":"oZ/9","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"\ufffd","raw":"n/4=","charset":"unknown"},
{"text":"\u009f\u0163","raw":"n/4=","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"\ufffd\ufffd","raw":"n/4=","charset":"unknown","eci":[{"designator":26,"start":0}]},
{"text":"\u0104\u009f\u0163","raw":"oZ/+","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"\ufffd","raw":"n/8=","charset":"unknown"},
{"text":"\u009f\u02d9","raw":"n/8=","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"\ufffd\ufffd","raw":"n/8=","charset":"unknown","eci":[{"designator":26,"start":0}]},
{"text":"\u0104\u009f\u02d9","raw":"oZ//","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"hello","raw":"aGVsbG8=","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"ok","raw":"b2s=","charset":"utf8","eci":[{"designator":27,"start":0}]},
{"text":"AB","raw":"AEEAQg==","charset":"eci","eci":[{"designator":25,"start":0}]},
{"text":"\ufffd\ufffd\ufffd\ufffd\ufffd","raw":"aGVsbG8=","charset":"unknown","eci":[{"designator":170,"start":0}]},
{"text":"\u2018\u2019\u00a3\u20ac\u20af","raw":"oaKjpKU=","charset":"eci","eci":[{"designator":9,"start":0}]},
{"text":"caf\u00e9\ufffd","raw":"Y2Fmw6n/","charset":"unknown","eci":[{"designator":26,"start":0}]},
{"text":"\ufffdA","raw":"8J+YQQ==","charset":"unknown","eci":[{"designator":26,"start":0}]},
{"text":"\u0104\ufffd\ufffd","raw":"oUFC","charset":"unknown","eci":[{"designator":4,"start":0},{"designator":999,"start":1}]},
{"text":"\u00e9\u00e9","raw":"6cOp","charset":"eci","eci":[{"designator":26,"start":1}]},
{"text":"\u00e9\u00e9","raw":"6ek=","charset":"latin1","eci":[{"designator":3,"start":1}]},
{"text":"\u0093\u00fa\u672c","raw":"k/qWew==","charset":"eci","eci":[{"designator":20,"start":2}]},
{"text":"\u8305","raw":"w6k=","charset":"gb18030","eci":[{"designator":31,"start":0}]},
{"text":"\u4e2d\u6587","raw":"1tDOxA==","charset":"gb18030","eci":[{"designator":29,"start":0}]},
{"text":"\u4e2d\u6587\ud840\udc00","raw":"1tDOxJUygjY=","charset":"gb18030","eci":[{"designator":32,"start":0}]},
{"text":"\u00e9\u4e2d\u6587","raw":"6dbQzsQ=","charset":"eci","eci":[{"designator":29,"start":1}]},
{"text":"","raw":"","charset":"utf8","eci":[{"designator":999,"start":0}]},
{"text":"caf\u00e9","raw":"Y2Fmw6k=","charset":"utf8","eci":[{"designator":26,"start":0},{"designator":999,"start":5}]},
{"text":"caf\u00e9","raw":"Y2Fmw6k=","charset":"utf8","eci":[{"designator":26,"start":0},{"designator":4,"start":5}]},
{"text":"ok","raw":"b2s=","charset":"utf8","eci":[{"designator":999,"start":0},{"designator":26,"start":0}]},
{"text":"ok","raw":"b2s=","charset":"utf8","eci":[{"designator":899,"start":0},{"designator":26,"start":0}]},
{"text":"\u0080","raw":"gA==","charset":"eci","eci":[{"designator":999,"start":1}]},
{"text":"12345\u00e9","raw":"MTIzNDXDqQ==","charset":"utf8","eci":[{"designator":26,"start":5}]},
{"text":"","raw":"","charset":"utf8"},
{"text":"","raw":"","charset":"utf8","eci":[]},
{"text":"\ufffd","raw":"oQ==","charset":"unknown","eci":[]},
{"text":"\ufffd\ufffd\ufffd\ufffd","raw":"k/qWew==","charset":"unknown","eci":[]},
{"text":"\ufffd\ufffd","raw":"w6k=","charset":"unknown","eci":[]},
{"text":"\ufffd\ufffd\ufffd\ufffd\ufffd","raw":"Y2Fmw6k=","charset":"unknown","eci":[]},
{"text":"\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd","raw":"MDEwNDAxMjM0NTY3ODkwMQ==","charset":"unknown","eci":[],"fnc1":"first"},
{"text":"\ufffd","raw":"oQ==","charset":"unknown"},
{"text":"Ab\ufffd","raw":"QWLp","charset":"unknown"},
{"text":"caf\ufffd\ufffd","raw":"Y2Fmw6k=","charset":"unknown"},
{"text":"abc","raw":"YWJj","charset":"utf8"},
{"text":"\u0104Za\u017c\u00f3\u0142\u0107 g\u0119\u015bl\u0105","raw":"oVphv/Oz5iBn6rZssQ==","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"\u65e5\u672c\u306e\u6587\u5b57\u30b3\u30fc\u30c9","raw":"k/qWe4LMlbaOmoNSgVuDaA==","charset":"shift_jis","eci":[{"designator":20,"start":0}]},
{"text":"caf\u00e9 \u2615 cr\u00e8me","raw":"Y2Fmw6kg4piVIGNyw6htZQ==","charset":"utf8","eci":[{"designator":26,"start":0}]},
{"text":"\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd","raw":"QUIgdW5zdXBwb3J0ZWQ=","charset":"unknown","eci":[{"designator":999,"start":0}]},
{"text":"12345\u041f\u0440\u0438\u0432\u0435\u0442 \u043c\u0438\u0440","raw":"MTIzNDW/4NjS1eIg3Njg","charset":"eci","eci":[{"designator":7,"start":5}]},
{"text":"\u0104\u0105\u20ac ok","raw":"obHigqwgb2s=","charset":"eci","eci":[{"designator":4,"start":0},{"designator":26,"start":2}]},
{"text":"5\u20ac","raw":"NYA=","charset":"latin1"},
{"text":"\u0000\u00ff\u20ac\u007f","raw":"AP+Afw==","charset":"latin1"},
{"text":"h\u00e9llo \u2192 \ud83e\udd8b","raw":"aMOpbGxvIOKGkiDwn6aL","charset":"utf8"},
{"text":"Qu\u00e9bec","raw":"UXXpYmVj","charset":"latin1"},
{"text":"\u3053\u3093\u306b\u3061\u306f","raw":"grGC8YLJgr+CzQ==","charset":"shift_jis"},
{"text":"\u00a9 2026","raw":"qSAyMDI2","charset":"latin1"},
{"text":"\u00a1Hola!","raw":"oUhvbGEh","charset":"latin1"},
{"text":"\uff71\uff72\uff73","raw":"sbKz","charset":"shift_jis"},
{"text":"\u00ff\u00feA\u0000","raw":"//5BAA==","charset":"latin1"},
{"text":"\u5c71\u7530","raw":"jlKTYw==","charset":"shift_jis"},
{"text":"\u5c71\u672c","raw":"jlKWew==","charset":"shift_jis"},
{"text":"\u65e5\u672c","raw":"k/qWew==","charset":"shift_jis"},
{"text":"\u30c6\u30b9\u30c8","raw":"g2WDWINn","charset":"shift_jis"},
{"text":"\u65e5\u672c\u8a9e","raw":"k/qWe4zq","charset":"shift_jis"},
{"text":"https://qrc-ai.com/\u65e5\u672c","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL5P6lns=","charset":"shift_jis"},
{"text":"12345abc","raw":"MTIzNDVhYmM=","charset":"utf8"},
{"text":"abc\u3053\u3093\u306b\u3061\u306f","raw":"YWJjgrGC8YLJgr+CzQ==","charset":"shift_jis"},
{"text":"MECARD:N:\u5c71\u7530;;","raw":"TUVDQVJEOk46jlKTYzs7","charset":"shift_jis"},
{"text":"\u5c71\u7530\u30c6\u30b9\u30c8\u672c","raw":"jlKTY4Nlg1iDZ5Z7","charset":"shift_jis"},
{"text":"\uff71\uff72\uff73\u65e5\u672c","raw":"sbKzk/qWew==","charset":"shift_jis"},
{"text":"A\uff71\u65e5\u672c","raw":"QbGT+pZ7","charset":"shift_jis"},
{"text":"Qu\u9955ec \u65e5\u672c","raw":"UXXpYmVjIJP6lns=","charset":"shift_jis"},
{"text":"Qu\ufffd\ufffdbec \u65e5\u672c","raw":"UXXDqWJlYyCT+pZ7","charset":"unknown"},
{"text":"\u5c71\u7530\ufffd","raw":"jlKTY/8=","charset":"unknown"},
{"text":"\ufffd\u65e5\u672c","raw":"gZP6lns=","charset":"unknown"},
{"text":"\u3042\u308a\u304c\u3068\u3046\u3054\u3056\u3044\u307e\u3059","raw":"44GC44KK44GM44Go44GG44GU44GW44GE44G+44GZ","charset":"utf8"},
{"text":"\u304a\u77e5\u3089\u305b","raw":"44GK55+l44KJ44Gb","charset":"utf8"},
{"text":"\u3042\u3044","raw":"44GC44GE","charset":"utf8"},
{"text":"\u4e2d\u6587","raw":"1tDOxA==","charset":"gb18030"},
{"text":"\u4f60\u597d","raw":"xOO6ww==","charset":"gb18030"},
{"text":"\u4e8c\u7ef4\u7801","raw":"tv7OrMLr","charset":"gb18030"},
{"text":"ABC \u4e2d\u6587","raw":"QUJDINbQzsQ=","charset":"gb18030"},
{"text":"\u5803\u4e2d\u6587","raw":"iNLW0M7E","charset":"gb18030"},
{"text":"\ufffd\ufffd\u4e2d\u6587","raw":"w6nW0M7E","charset":"unknown"},
{"text":"Qu\u958eec \u4e2d\u6587","raw":"UXXpYmVjINbQzsQ=","charset":"gb18030"},
{"text":"\u65e5\u672c\u4e2d\u6587","raw":"k/qWe9bQzsQ=","charset":"unknown"},
{"text":"\u017dR\u201cc","raw":"jlKTYw==","charset":"latin1"},
{"text":"\ufffd\ufffd\ufffd\ufffd","raw":"4KHgoQ==","charset":"unknown"},
{"text":"\ufffd\ufffd\ufffd\ufffd","raw":"sKGwoQ==","charset":"unknown"},
{"text":"0109506000134352","raw":"MDEwOTUwNjAwMDEzNDM1Mg==","charset":"utf8","eci":[{"designator":26,"start":0}],"fnc1":"first"},
{"text":"10ABC\u001d2112","raw":"MTBBQkMdMjExMg==","charset":"utf8","fnc1":"first"},
{"text":"10AB%C","raw":"MTBBQiVD","charset":"utf8","fnc1":"first"},
{"text":"Aabc","raw":"QWFiYw==","charset":"utf8","fnc1":"second"},
{"text":"\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd\ufffd","raw":"MDEwNDAxMjM0NTY3ODkwMQ==","charset":"unknown","eci":[{"designator":999,"start":0}],"fnc1":"first"},
{"text":"0109506000134352","raw":"MDEwOTUwNjAwMDEzNDM1Mg==","charset":"utf8"},
{"text":"0109506000134352","raw":"MDEwOTUwNjAwMDEzNDM1Mg==","charset":"utf8","fnc1":"first"},
{"text":"37abc","raw":"MzdhYmM=","charset":"utf8"},
{"text":"https://qrc-ai.com/\u65e5\u672c\u8a9e\u30c6\u30b9\u30c8","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL+aXpeacrOiqnuODhuOCueODiA==","charset":"utf8"},
{"text":"Caf\u00e9 cr\u00e8me br\u00fbl\u00e9e","raw":"Q2Fmw6kgY3LDqG1lIGJyw7tsw6ll","charset":"utf8"},
{"text":"https://qrc-ai.com/caf\u00e9","raw":"aHR0cHM6Ly9xcmMtYWkuY29tL2NhZsOp","charset":"utf8"},
{"text":"https://a.example/\u65e5\u672c\u8a9e\u30c6\u30b9\u30c8","raw":"aHR0cHM6Ly9hLmV4YW1wbGUv5pel5pys6Kqe44OG44K544OI","charset":"utf8"},
{"text":"https://b.example/\u65e5\u672c\u8a9e\u30c6\u30b9\u30c8","raw":"aHR0cHM6Ly9iLmV4YW1wbGUv5pel5pys6Kqe44OG44K544OI","charset":"utf8"},
{"text":"\u65e5\u672c","raw":"5pel5pys","charset":"utf8"},
{"text":"09506000134352","raw":"MDk1MDYwMDAxMzQzNTI=","charset":"utf8"},
{"text":"\u00e9","raw":"w6k=","charset":"utf8","eci":[{"designator":26,"start":0}]},
{"text":"\u0102\u0160","raw":"w6k=","charset":"eci","eci":[{"designator":4,"start":0}]},
{"text":"A\ufffdB","raw":"Qe+/vUI=","charset":"utf8"},
{"text":"A\ufffdB","raw":"QYQxpDdC","charset":"gb18030","eci":[{"designator":32,"start":0}]},
{"text":"a\\b","raw":"YVxi","charset":"utf8"},
{"text":"abc","raw":"YWJj","charset":"utf8","fnc1":"first"},
{"text":"hello","raw":"aGVsbG8=","charset":"utf8"},
{"text":"hi","raw":"aGk=","charset":"utf8"}
],"invalid":[
{"why":"eci [] labelled utf8","content":{"text":"\ufffd","raw":"oQ==","charset":"utf8","eci":[]}},
{"why":"empty raw labelled unknown","content":{"text":"","raw":"","charset":"unknown"}},
{"why":"empty raw labelled eci","content":{"text":"","raw":"","charset":"eci","eci":[{"designator":4,"start":0}]}},
{"why":"non-empty raw with empty text","content":{"text":"","raw":"oQ==","charset":"utf8"}},
{"why":"fnc1 fifth","content":{"text":"abc","raw":"YWJj","charset":"utf8","fnc1":"fifth"}},
{"why":"charset eci without eci","content":{"text":"\u0104","raw":"oQ==","charset":"eci"}},
{"why":"charset eci with eci []","content":{"text":"\u0104","raw":"oQ==","charset":"eci","eci":[]}},
{"why":"gb18030 with eci []","content":{"text":"\u8305","raw":"w6k=","charset":"gb18030","eci":[]}},
{"why":"charset gb2312","content":{"text":"\u4e2d\u6587","raw":"1tDOxA==","charset":"gb2312"}},
{"why":"designator 1000000","content":{"text":"\ufffd","raw":"oQ==","charset":"unknown","eci":[{"designator":1000000,"start":0}]}}
]}"#;

#[test]
fn content_contract_objects_validate_and_its_negatives_do_not() {
    // Every content test object is a valid `DecodedContent` (the six
    // charset labels, `eci` absent / `[]` / entries, `fnc1`); the schema's
    // own invariants reject the negatives (spec/01 § Content · Invariants).
    let validator = definition("DecodedContent");
    let objects: serde_json::Value = serde_json::from_str(CONTENT_OBJECTS).unwrap();
    let valid = objects["valid"].as_array().unwrap();
    assert!(valid.len() >= 100, "the matrix objects");
    for content in valid {
        let errors: Vec<String> = validator
            .iter_errors(content)
            .map(|e| format!("{} @ {}", e, e.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{content}: {errors:#?}");
        // the real types read it back to the same object
        let back: qrcode_ai_scanner::DecodedContent =
            serde_json::from_value(content.clone()).unwrap_or_else(|e| panic!("{content}: {e}"));
        assert_eq!(&serde_json::to_value(&back).unwrap(), content);
    }
    for negative in objects["invalid"].as_array().unwrap() {
        assert!(
            !validator.is_valid(&negative["content"]),
            "{}: {}",
            negative["why"],
            negative["content"]
        );
    }
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
