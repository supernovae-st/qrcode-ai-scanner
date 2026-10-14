//! One independently confirmed text correction over immutable upstream files.
//! This is corpus provenance, never a scanner exception or a configurable
//! proof registry. See `spec/corpus-truth-amendments.md` for evidence limits.

use serde::Serialize;

use crate::external::sha256_bytes;

/// A qualified external observation; hashes identify archived evidence,
/// not a claim that an artifact was reproducibly built from source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct Witness {
    reader: &'static str,
    lineage: &'static str,
    artifact_sha256: &'static str,
    receipt_sha256: &'static str,
}

/// Public, text-only provenance for the single accepted amendment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct Amendment {
    pub(crate) id: &'static str,
    pub(crate) image_path: &'static str,
    pub(crate) image_sha256: &'static str,
    pub(crate) original_truth_sha256: &'static str,
    pub(crate) effective_text_sha256: &'static str,
    source: &'static str,
    confirmation_sha256: &'static str,
    reporting_correction_sha256: &'static str,
    witnesses: [Witness; 2],
}

pub(crate) const Q13: Amendment = Amendment {
    id: "zxing-qrcode-2-13-text-2026-10-13",
    image_path: "zxing-blackbox/qrcode-2/13.png",
    image_sha256: "a8d498d6d2d6a3e27e24fd75ed023cd5b8b6b42cb4996674e88e555d68855ec1",
    original_truth_sha256: "c19d2dcb13469aae263a41cd73add68c45e086223a04c41368128c47143c0e5a",
    effective_text_sha256: "30a2a450189a5b357a360265102c4c99baa4c84a2559b6a9b16d9a0d5bf80a07",
    source: "spec/corpus-truth-amendments.md#zxing-qrcode-2-13",
    confirmation_sha256: "a5386ff61e9be111183067f9ae5c301dc3ad229a9721b39d86dc5aa29a9c3680",
    reporting_correction_sha256: "d4ce9b94e0ebe97110e8ebb970f75f373335a94a69aad75223c1d99000125e81",
    witnesses: [
        Witness {
            reader: "ZXing-C++ 3.1.1",
            lineage: "zxing",
            artifact_sha256: "1f0f117c6bc35092f9e1cd42d6d6fb0ed7f0fcf52faebecf996713a8e6179f6b",
            receipt_sha256: "1b2b757849106c24b2c76c364eabe1eafced4bc39e781f834769ef18bb8e8635",
        },
        Witness {
            reader: "quirc-wasm-emcc 1.0.26 (external C/WASM)",
            lineage: "quirc",
            artifact_sha256: "5711597fef34ffe13f87c3252fe43d578447b465bbe60941e010d8651aa1422f",
            receipt_sha256: "657851092cc4182601bac8dfdf643d8dc4b1073d0f3396d317f47d8e1832e375",
        },
    ],
};

const Q13_TEXT: &str = "The 2005 USGS aerial photography of the Washington Monument is censored.";

/// Text consumed by every corpus judge, plus provenance only when validated.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Truth {
    pub(crate) text: String,
    pub(crate) amendment: Option<&'static Amendment>,
}

/// Resolve original verified inputs. A stale q13 pin is an error even when
/// the scanner is blind; no output or decoder name participates in this rule.
pub(crate) fn resolve(
    image_path: &str,
    image_sha256: &str,
    original_truth_sha256: &str,
    original_bytes: &[u8],
) -> Result<Truth, String> {
    if image_path != Q13.image_path {
        return Ok(Truth {
            text: crate::external::decode_truth(original_bytes.to_vec()),
            amendment: None,
        });
    }
    if image_sha256 != Q13.image_sha256 {
        return Err(String::from("q13 truth amendment: image sha256 mismatch"));
    }
    if original_truth_sha256 != Q13.original_truth_sha256
        || sha256_bytes(original_bytes) != Q13.original_truth_sha256
    {
        return Err(String::from(
            "q13 truth amendment: original truth sha256 mismatch",
        ));
    }
    if sha256_bytes(Q13_TEXT.as_bytes()) != Q13.effective_text_sha256 {
        return Err(String::from(
            "q13 truth amendment: effective text sha256 mismatch",
        ));
    }
    Ok(Truth {
        text: Q13_TEXT.to_owned(),
        amendment: Some(&Q13),
    })
}

/// Fingerprint the accepted provenance separately from unchanged file pins.
pub(crate) fn fingerprint() -> String {
    sha256_bytes(&serde_json::to_vec(&Q13).expect("amendment serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str =
        "The 2005 USGS aerial photograph of the Washington Monument is censored.";

    #[test]
    fn one_pinned_text_changes_without_trimming_or_claiming_structural_truth() {
        let resolved = resolve(
            Q13.image_path,
            Q13.image_sha256,
            &sha256_bytes(ORIGINAL.as_bytes()),
            ORIGINAL.as_bytes(),
        )
        .expect("accepted source");
        assert_eq!(ORIGINAL.len(), 71);
        assert_eq!(
            resolved.text.as_bytes(),
            b"The 2005 USGS aerial photography of the Washington Monument is censored."
        );
        assert_eq!(resolved.text.len(), 72);
        let provenance =
            serde_json::to_value(resolved.amendment.expect("provenance")).expect("JSON");
        assert_eq!(
            provenance["original_truth_sha256"],
            sha256_bytes(ORIGINAL.as_bytes())
        );
        assert_eq!(
            provenance["effective_text_sha256"],
            sha256_bytes(resolved.text.as_bytes())
        );
        assert_eq!(
            provenance["source"],
            "spec/corpus-truth-amendments.md#zxing-qrcode-2-13"
        );
        for structural in ["raw_hex", "eci", "fnc1"] {
            assert!(provenance.get(structural).is_none());
        }
        assert_eq!(
            resolve(
                "zxing-blackbox/qrcode-2/16.png",
                "other image",
                "other truth",
                b"text\n"
            ),
            Ok(Truth {
                text: "text\n".to_owned(),
                amendment: None
            })
        );
    }

    #[test]
    fn stale_image_or_original_truth_cannot_activate_the_amendment() {
        assert_eq!(
            resolve(
                Q13.image_path,
                "different",
                Q13.original_truth_sha256,
                ORIGINAL.as_bytes()
            ),
            Err(String::from("q13 truth amendment: image sha256 mismatch"))
        );
        for (digest, text) in [
            ("different", ORIGINAL.as_bytes().to_vec()),
            (
                Q13.original_truth_sha256,
                format!("{ORIGINAL}\n").into_bytes(),
            ),
            (Q13.original_truth_sha256, Q13_TEXT.as_bytes().to_vec()),
        ] {
            assert_eq!(
                resolve(Q13.image_path, Q13.image_sha256, digest, &text),
                Err(String::from(
                    "q13 truth amendment: original truth sha256 mismatch"
                ))
            );
        }
    }
}
