//! One change to a PDF's pages, and where it takes every page number.
//!
//! Outside `pdf` because what names a page by its number is not only the viewer: the notes do
//! too (`[[paper.pdf#page=3]]`), and a host with no pdfium rewrites them
//! ([`crate::markdown::repage_links`]).

use serde::{Deserialize, Serialize};

/// One change to a document's pages.
///
/// Everything a viewer keeps of a page — its renders, its glyphs, its ink history — is filed under
/// the page's number, and [`PageEdit::map`] is how each of them follows the edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageEdit {
    /// A blank page goes in at this index, the size of the page before it.
    Insert(usize),
    /// The page at this index goes.
    Delete(usize),
    /// The page at `from` is taken out and put back so that it ends up at `to`.
    Move { from: usize, to: usize },
}

impl PageEdit {
    /// Where the page that was at `page` is after the edit; `None` for the one deleted.
    pub fn map(self, page: usize) -> Option<usize> {
        match self {
            PageEdit::Insert(at) => Some(page + usize::from(page >= at)),
            PageEdit::Delete(at) if page == at => None,
            PageEdit::Delete(at) => Some(page - usize::from(page > at)),
            PageEdit::Move { from, to } if page == from => Some(to),
            // Taking `from` out moves every page after it up one; putting it back at `to` moves
            // every page from there on down one.
            PageEdit::Move { from, to } => {
                let out = page - usize::from(page > from);
                Some(out + usize::from(out >= to))
            }
        }
    }

    /// The edit that takes this one back: a page put in comes out, a page taken out goes back
    /// where it was, a moved page moves back.
    pub fn inverse(self) -> PageEdit {
        match self {
            PageEdit::Insert(at) => PageEdit::Delete(at),
            PageEdit::Delete(at) => PageEdit::Insert(at),
            PageEdit::Move { from, to } => PageEdit::Move { from: to, to: from },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PageEdit;

    #[test]
    fn a_page_edit_says_where_every_page_went() {
        // Five pages, and where each of them is after the edit.
        let after = |edit: PageEdit| (0..5).map(|p| edit.map(p)).collect::<Vec<_>>();
        let s = Some;
        assert_eq!(after(PageEdit::Insert(2)), [s(0), s(1), s(3), s(4), s(5)]);
        assert_eq!(after(PageEdit::Insert(5)), [s(0), s(1), s(2), s(3), s(4)]);
        assert_eq!(after(PageEdit::Delete(1)), [s(0), None, s(1), s(2), s(3)]);
        let down = PageEdit::Move { from: 1, to: 3 };
        assert_eq!(after(down), [s(0), s(3), s(1), s(2), s(4)]);
        let up = PageEdit::Move { from: 3, to: 0 };
        assert_eq!(after(up), [s(1), s(2), s(3), s(0), s(4)]);
        // The inverse puts every page still there back where it was.
        for edit in [PageEdit::Insert(2), PageEdit::Delete(1), down, up] {
            for page in 0..5 {
                if let Some(at) = edit.map(page) {
                    assert_eq!(edit.inverse().map(at), Some(page), "{edit:?} {page}");
                }
            }
        }
    }
}
