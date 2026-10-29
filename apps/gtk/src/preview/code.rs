//! A code block's text, set plain by [`super::text`]. Stage 2 P7 colours it as the editor does
//! (`codeblocks.rs`) from an idle after the text is in, and again on a restyle.

use super::native::Page;
use super::text::Writer;
use std::ops::Range;

/// The code block in `lang` the writer just put at `range` (offsets from its block's start).
pub(super) fn fence(_w: &mut Writer, _lang: &str, _range: Range<i32>) {}

/// The theme changed: colour the code again.
pub(super) fn restyle(_page: &Page) {}
