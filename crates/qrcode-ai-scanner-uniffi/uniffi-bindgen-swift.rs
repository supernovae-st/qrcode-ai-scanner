// Standalone UniFFI Swift bindgen (library mode): generates the Swift sources,
// the FFI header, and an xcframework-compatible modulemap from the built
// staticlib. Run via `cargo run --bin uniffi-bindgen-swift -- ...` so the
// generator version always matches this crate's `uniffi` dep (no version skew
// between the generated `.swift` and the compiled `.a`).
fn main() {
    // NB: the entry point is `uniffi_bindgen_swift` (no `_main` suffix) in
    // uniffi 0.31.x — the mobile plan §3.2 sketch wrote `_main`, which does not
    // exist in this version. Corrected here (compile-verified against 0.31.2).
    uniffi::uniffi_bindgen_swift()
}
