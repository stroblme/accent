//! Generates the Kotlin bindings from the built library. Runs on the host, never on the phone.
//!
//! `cargo run -p accent-android --features cli --bin uniffi-bindgen -- \
//!     generate --library <path to libaccent_android.so> --language kotlin --out-dir <dir>`

fn main() {
    uniffi::uniffi_bindgen_main()
}
