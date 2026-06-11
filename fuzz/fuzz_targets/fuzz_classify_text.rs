//! Payload classification fuzz — decoded QR text is attacker-controlled.
//! Found-by-construction class: UTF-8 boundary panics in the GS1 element
//! string parser (byte-index slicing on multi-byte content).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        qrcode_ai_scanner::fuzzing::classify_text(text);
    }
});
