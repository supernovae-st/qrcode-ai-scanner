// Standalone UniFFI bindgen (library mode): generates the Kotlin + Swift
// bindings from the built lib. Run via `cargo run --bin uniffi-bindgen -- ...`
// so the generator version always matches this crate's `uniffi` dep.
fn main() {
    uniffi::uniffi_bindgen_main()
}
