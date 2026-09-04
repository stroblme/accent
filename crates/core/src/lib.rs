//! accent-core: vault index, safe file ops, markdown analysis, PDF.
//! Rule: no UI toolkit types in this crate. Everything here must work on Linux and Android.

pub mod config;
pub mod diff;
pub mod fs;
pub mod index;
pub mod markdown;
#[cfg(feature = "pdf")]
pub mod pdf;
pub mod search;
pub mod template;
pub mod walk;
pub mod watch;
