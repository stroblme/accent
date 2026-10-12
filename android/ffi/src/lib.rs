//! APK shared-library entry point. Link `accent-api`'s FFI surface as a
//! `cdylib` with UniFFI's exported scaffolding symbols; see `crates/api/src/ffi`.

pub use accent_api::ffi::*;
