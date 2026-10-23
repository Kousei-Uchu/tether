// Generates the Swift (or Python, for tests) bindings from the compiled library:
//   cargo run -p tether-ffi --features bindgen --bin uniffi-bindgen -- generate --library <lib> --language swift --out-dir <dir>
fn main() {
    uniffi::uniffi_bindgen_main()
}
