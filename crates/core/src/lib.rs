//! accent-core: vault index, safe file ops, markdown and org analysis, PDF.
//! Rule: no UI toolkit types in this crate. Everything here must work on Linux and Android.

pub mod animation;
pub mod attachment;
pub mod bib;
pub mod code;
pub mod config;
pub mod conflict;
pub mod csv;
pub mod diff;
pub mod dimensions;
mod error;
pub mod fs;
pub mod fuzzy;
pub mod git;
pub mod index;
pub mod malloc;
pub mod markdown;
pub mod note;
pub mod notebook;
pub mod org;
pub mod orientation;
pub mod page_edit;
pub mod path;
#[cfg(feature = "pdf")]
pub mod pdf;
pub mod pdf_text;
pub mod query;
pub mod recolour;
pub mod search;
pub mod shrink;
pub mod synctex;
pub mod template;
pub mod walk;
pub mod watch;

pub use error::{Error, Result};
