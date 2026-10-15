//! Android UniFFI surface, excluded from desktop builds. It exposes local vault operations and
//! converts unsupported Rust types and byte offsets to Kotlin-compatible records and UTF-16
//! units. Calls are synchronous and cannot be interrupted by UniFFI 0.32; callers cancel between
//! operations such as tiles, glyph pages, or searches.

mod convert;
mod error;
mod event;
mod fuzzy;
mod markdown;
mod pdf;
mod recolour;
mod vault;

pub use convert::*;
pub use error::AccentError;
pub use event::Event;
pub use fuzzy::{Ranker, fuzzy_rank};
pub use markdown::{analyze_utf16, pdf_anchor, to_html};
pub use pdf::{PdfSession, link_with_alias};
pub use recolour::{is_animated, looks_like_document, recolour_image, recolour_svg, upright_image};
pub use vault::Vault;
