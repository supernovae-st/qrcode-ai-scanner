//! Arbitrary bytes through the full scan pipeline: must never panic.
//! Seed corpus: `cargo fuzz run fuzz_scan_encoded fixtures/` from the root.
#![no_main]

use libfuzzer_sys::fuzz_target;
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};

fuzz_target!(|data: &[u8]| {
    let scanner = Scanner::builder().profile(ScanProfile::Fast).build();
    let _ = scanner.scan(ImageInput::encoded(data));
});
