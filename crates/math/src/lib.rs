// Derived from formulary 0.3.0 `src/lib.rs` (MIT OR Apache-2.0, Copyright (c) 2026 Jeffrey T.
// Peckham (Ophymx)), modified for accent; see crates/math/NOTICE.
//! Native math layout: the MathML that pulldown-latex writes (`accent_core::markdown::mathml`),
//! laid out with the OpenType MATH table of a font the system HarfBuzz reads, into a display
//! list of glyphs and rules a renderer draws in that same font.
//!
//! ```
//! let font = accent_math::MathFont::from_static(accent_math::STIX_TWO_MATH)?;
//! let root = accent_math::parse("<math><mi>x</mi><mo>+</mo><mn>2</mn></math>")?;
//! let laid = accent_math::layout(&root, &font, 16.0);
//! assert!(laid.width > 0.0 && root.warnings.is_empty());
//! # Ok::<(), accent_math::Error>(())
//! ```

mod ast;
mod font;
mod hb;
mod layout;
mod mathvariant;
mod opdict;
mod parse;

pub use ast::{Color, MathRoot};
pub use font::{MathFont, PathOp};
pub use layout::{Item, Layout, layout};
pub use parse::parse;

/// STIX Two Math, the font a formula falls back to where the system has no math font
/// (`fonts/README.md`).
pub const STIX_TWO_MATH: &[u8] = include_bytes!("../fonts/STIXTwoMath-Regular.otf");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("malformed MathML: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("expected a <math> element, found <{0}>")]
    NotMath(String),
    #[error("the font has no OpenType MATH table")]
    NoMathTable,
}
