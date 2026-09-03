//! accent-core: vault index, safe file ops, markdown analysis, PDF.
//! Rule: no UI toolkit types in this crate. Everything here must work on Linux and Android.

pub mod fs;
pub mod index;
pub mod markdown;
pub mod walk;
pub mod watch;
#[cfg(feature = "pdf")]
pub mod pdf;
