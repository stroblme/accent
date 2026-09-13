//! The uniffi surface Android calls this crate through.
//!
//! Everything Kotlin can reach is here and nowhere else: the façade itself stays plain Rust, and
//! the desktop never compiles a line of this. What the module does is narrow the façade twice
//! over. It leaves out what Android does not have — git, language servers, remote vaults — and
//! it restates the types that cross, because uniffi carries neither `usize`, `Range`, `PathBuf`,
//! tuples nor `char`, and because a text offset means something different on the other side:
//! Rust counts bytes, Kotlin counts UTF-16 units.
//!
//! Calls are synchronous. uniffi 0.32 has no cancellation, so a caller that wants to stop asking
//! stops between calls rather than during one; the pieces are small enough for that to be the
//! right granularity (one tile, one page of glyphs, one search).

mod convert;
mod error;
mod event;
mod fuzzy;
mod markdown;
mod pdf;
mod vault;

pub use convert::*;
pub use error::AccentError;
pub use event::Event;
pub use fuzzy::fuzzy_rank;
pub use markdown::{analyze_utf16, to_html};
pub use pdf::PdfSession;
pub use vault::Vault;
