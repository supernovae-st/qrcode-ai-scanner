//! Arbitrary bytes through the FULL scan pipeline: the deep grid (S4) +
//! erasure-RS rescue (S5) path that the `Fast` target (`fuzz_scan_encoded`)
//! never reaches. A tight wall-clock budget keeps each iteration fast while
//! still exercising deep binarization + the rescue decoder; the input is the
//! only variable (no RNG — the pipeline is deterministic by contract).
//! Seed corpus: `cargo fuzz run fuzz_scan_full fixtures/` from the root.
#![no_main]

use libfuzzer_sys::fuzz_target;
use qrcode_ai_scanner::{ImageInput, ScanProfile, Scanner};

fuzz_target!(|data: &[u8]| {
    // Start from Full's config (deep + rescue on, full scoring) and only
    // tighten `budget_ms` so a pathological input can't stall one iteration
    // for seconds. Everything else is Full's real stage set.
    let mut config = ScanProfile::Full.config();
    config.budget_ms = Some(200);
    let scanner = Scanner::builder()
        .profile(ScanProfile::Custom(config))
        .build();
    let _ = scanner.scan(ImageInput::encoded(data));
});
