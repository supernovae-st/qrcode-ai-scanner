//! Builder ↔ scanner round-trip gate — the two ends of the product loop.
//!
//! The qrcode-ai.com builder escapes user fields when it CONSTRUCTS a
//! payload (its `utils/qrcode/payload-escape.ts`, unit-pinned there); this
//! suite encodes the SAME adversarial vectors into real QR images and
//! asserts the scanner returns the original field values (WIFI/MECARD,
//! unescaped by the classifier) or the exact written form (sms/mailto —
//! spec/05: values are split structurally but NOT percent-decoded;
//! presentation belongs to the consumer). A break on either side of the
//! convention fails here, not in production.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use qrcode_ai_scanner::{ImageInput, Payload, Scanner};

fn qr_png(content: &str) -> Vec<u8> {
    let code = qrcode::QrCode::with_error_correction_level(content, qrcode::EcLevel::Q).unwrap();
    let img = code
        .render::<image::Luma<u8>>()
        .module_dimensions(6, 6)
        .build();
    let mut buf = Vec::new();
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

fn scan_one(content: &str) -> Payload {
    let report = Scanner::default()
        .scan(ImageInput::encoded(&qr_png(content)))
        .unwrap();
    assert_eq!(report.detections.len(), 1, "payload: {content}");
    report.detections[0].payload.clone()
}

/// The builder's `WIFI:` escaping (`ZXing` convention): `\ ; , : "` each
/// get a backslash. Mirrors `escapeWifiField` in the landing repo.
fn escape_wifi(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        if matches!(c, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[test]
fn wifi_fields_with_every_reserved_char_round_trip() {
    // The builder's own unit vectors: SSID with `;` + password mixing
    // `: , "` — the split-trap class that shipped broken pre-escaping.
    let ssid = "Cafe;Wifi";
    let password = r#"p:a,s"s"#;
    let content = format!(
        "WIFI:T:WPA;S:{};P:{};H:false;;",
        escape_wifi(ssid),
        escape_wifi(password)
    );

    match scan_one(&content) {
        Payload::Wifi {
            ssid: got_ssid,
            security,
            password: got_password,
            hidden,
        } => {
            assert_eq!(got_ssid, ssid);
            assert_eq!(security, "WPA");
            assert_eq!(got_password.as_deref(), Some(password));
            assert!(!hidden);
        }
        other => panic!("expected Wifi, got {other:?}"),
    }
}

#[test]
fn wifi_backslash_in_password_survives_one_round() {
    // `pass\word` must escape to `pass\\word` and come back single-slashed —
    // the double-escape regression class.
    let password = r"pass\word";
    let content = format!("WIFI:T:WPA;S:Net;P:{};;", escape_wifi(password));

    match scan_one(&content) {
        Payload::Wifi {
            ssid, password: p, ..
        } => {
            assert_eq!(ssid, "Net");
            assert_eq!(p.as_deref(), Some(password));
        }
        other => panic!("expected Wifi, got {other:?}"),
    }
}

#[test]
fn mecard_shares_the_wifi_escape_family() {
    // MECARD name carrying `;` and `,` — same `\`-escape convention.
    let name = "Doe;John,Jr";
    let content = format!(
        "MECARD:N:{};TEL:+33612345678;EMAIL:john@qrcode-ai.com;;",
        escape_wifi(name)
    );

    match scan_one(&content) {
        Payload::MeCard {
            name: got,
            tel,
            email,
            ..
        } => {
            assert_eq!(got.as_deref(), Some(name));
            assert_eq!(tel.as_deref(), Some("+33612345678"));
            assert_eq!(email.as_deref(), Some("john@qrcode-ai.com"));
        }
        other => panic!("expected MeCard, got {other:?}"),
    }
}

#[test]
fn sms_body_stays_exactly_as_written() {
    // The builder percent-encodes the body (`buildSmsUri`); spec/05 pins
    // that the scanner does NOT percent-decode — the wire value must be
    // byte-identical to what was written, and decoding is the consumer's.
    let encoded_body = "Hey%20%26%20co%3F%20%3D%2323%20%F0%9F%A6%8B";
    let content = format!("sms:+33612345678?body={encoded_body}");

    match scan_one(&content) {
        Payload::Sms { number, body } => {
            assert_eq!(number, "+33612345678");
            assert_eq!(body.as_deref(), Some(encoded_body));
        }
        other => panic!("expected Sms, got {other:?}"),
    }
}

#[test]
fn vcard_raw_carries_escaped_fields_verbatim() {
    // vCard 3.0 escaping (`\;` `\,` `\n`) is produced by the builder and
    // consumed by phone contact apps; the scanner's contract is raw
    // passthrough — byte-identical, no unescaping, no reflowing.
    let content = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Doe\\;Jr;Jane\\,Ms\r\nFN:Jane Doe\r\nNOTE:line one\\nline two\r\nEND:VCARD";

    match scan_one(content) {
        Payload::VCard { raw } => assert_eq!(raw, content),
        other => panic!("expected VCard, got {other:?}"),
    }
}

#[test]
fn mailto_subject_and_body_stay_as_written() {
    // Same not-percent-decoded contract as sms, on the mailto route.
    let content = "mailto:hello@qrcode-ai.com?subject=Hi%20there&body=Line1%0ALine2%20%26%20done";

    match scan_one(content) {
        Payload::Email { to, subject, body } => {
            assert_eq!(to, "hello@qrcode-ai.com");
            assert_eq!(subject.as_deref(), Some("Hi%20there"));
            assert_eq!(body.as_deref(), Some("Line1%0ALine2%20%26%20done"));
        }
        other => panic!("expected Email, got {other:?}"),
    }
}
