//! Android UniFFI surface, excluded from desktop builds. Omit git, language
//! servers and remote vaults; replace types UniFFI cannot carry (`usize`,
//! `Range`, `PathBuf`, tuples and `char`). Convert Rust byte offsets to
//! Kotlin UTF-16 units.
//! Calls are synchronous and UniFFI 0.32 cannot cancel them. Callers stop
//! between small operations such as tiles, glyph pages or searches.

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
pub use fuzzy::fuzzy_rank;
pub use markdown::{analyze_utf16, pdf_anchor, to_html};
pub use pdf::{PdfSession, link_with_alias};
pub use recolour::{is_animated, looks_like_document, recolour_image, recolour_svg, upright_image};
pub use vault::Vault;
