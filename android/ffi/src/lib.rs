//! The shared library the APK loads.
//!
//! It holds no code: `accent-api`'s `ffi` module is the surface, and this crate exists only to
//! link it as a `cdylib` with the scaffolding's exported symbols. See `crates/api/src/ffi`.

pub use accent_api::ffi::*;
