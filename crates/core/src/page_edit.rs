//! PDF page edits and page-number remapping. Kept outside `pdf` so hosts without pdfium can
//! update note links through [`crate::markdown::repage_links`].

use serde::{Deserialize, Serialize};

/// One change to a document's pages.
///
/// Everything a viewer keeps of a page — its renders, its glyphs, its ink history — is filed under
/// the page's number, and [`PageEdit::map`] is how each of them follows the edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageEdit {
    /// `count` pages go in at `at`: a blank one the size of the page before it, or the pages of
    /// another document dropped onto this one.
    Insert { at: usize, count: usize },
    /// The `count` pages from `at` on go.
    Delete { at: usize, count: usize },
    /// The page at `from` is taken out and put back so that it ends up at `to`.
    Move { from: usize, to: usize },
}

impl PageEdit {
    /// One blank page in at `at`.
    pub fn insert(at: usize) -> PageEdit {
        PageEdit::Insert { at, count: 1 }
    }

    /// The page at `at` out.
    pub fn delete(at: usize) -> PageEdit {
        PageEdit::Delete { at, count: 1 }
    }

    /// The deletes that take `pages`, in order and without repeats, out: one for each run of
    /// neighbours, the last run first, so each names its pages as they are numbered when it is
    /// made.
    pub fn deletes(pages: &[usize]) -> Vec<PageEdit> {
        let mut edits: Vec<PageEdit> = Vec::new();
        for &page in pages.iter().rev() {
            match edits.last_mut() {
                Some(PageEdit::Delete { at, count }) if *at == page + 1 => {
                    *at = page;
                    *count += 1;
                }
                _ => edits.push(PageEdit::delete(page)),
            }
        }
        edits
    }

    /// The moves that put `pages`, in order and without repeats, together in their order into gap
    /// `gap`, the one before page `gap`; none where they already are. Those before the gap go
    /// down to it last first, then those after it come up first first, so no move shifts a page
    /// still to come.
    pub fn moves(pages: &[usize], gap: usize) -> Vec<PageEdit> {
        let (before, after) = pages.split_at(pages.partition_point(|&page| page < gap));
        let down = (before.iter().rev().enumerate()).map(|(i, &from)| (from, gap - 1 - i));
        let up = (after.iter().enumerate()).map(|(i, &from)| (from, gap + i));
        (down.chain(up))
            .filter(|(from, to)| from != to)
            .map(|(from, to)| PageEdit::Move { from, to })
            .collect()
    }

    /// Where the page that was at `page` is after the edit; `None` for one deleted.
    pub fn map(self, page: usize) -> Option<usize> {
        match self {
            PageEdit::Insert { at, count } if page >= at => Some(page + count),
            PageEdit::Insert { .. } => Some(page),
            PageEdit::Delete { at, count } if page >= at + count => Some(page - count),
            PageEdit::Delete { at, .. } if page >= at => None,
            PageEdit::Delete { .. } => Some(page),
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
            PageEdit::Insert { at, count } => PageEdit::Delete { at, count },
            PageEdit::Delete { at, count } => PageEdit::Insert { at, count },
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
        assert_eq!(after(PageEdit::insert(2)), [s(0), s(1), s(3), s(4), s(5)]);
        assert_eq!(after(PageEdit::insert(5)), [s(0), s(1), s(2), s(3), s(4)]);
        assert_eq!(after(PageEdit::delete(1)), [s(0), None, s(1), s(2), s(3)]);
        // Another document's three pages dropped in before the second page, and taken out again.
        let three = PageEdit::Insert { at: 1, count: 3 };
        assert_eq!(after(three), [s(0), s(4), s(5), s(6), s(7)]);
        let out = PageEdit::Delete { at: 1, count: 3 };
        assert_eq!(after(out), [s(0), None, None, None, s(1)]);
        let down = PageEdit::Move { from: 1, to: 3 };
        assert_eq!(after(down), [s(0), s(3), s(1), s(2), s(4)]);
        let up = PageEdit::Move { from: 3, to: 0 };
        assert_eq!(after(up), [s(1), s(2), s(3), s(0), s(4)]);
        // The inverse puts every page still there back where it was.
        for edit in [
            PageEdit::insert(2),
            PageEdit::delete(1),
            three,
            out,
            down,
            up,
        ] {
            for page in 0..5 {
                if let Some(at) = edit.map(page) {
                    assert_eq!(edit.inverse().map(at), Some(page), "{edit:?} {page}");
                }
            }
        }
    }

    /// The pages in order once `edits` are made, one after another, to `count` pages.
    fn made(count: usize, edits: &[PageEdit]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..count).collect();
        for edit in edits {
            match *edit {
                PageEdit::Delete { at, count } => {
                    order.drain(at..at + count);
                }
                PageEdit::Move { from, to } => {
                    let page = order.remove(from);
                    order.insert(to, page);
                }
                PageEdit::Insert { .. } => unreachable!("no batch inserts"),
            }
        }
        order
    }

    #[test]
    fn several_pages_go_out_or_move_together() {
        // Runs of neighbours go at once, the last first.
        let deletes = PageEdit::deletes(&[0, 2, 3, 6]);
        assert_eq!(
            deletes,
            [
                PageEdit::delete(6),
                PageEdit::Delete { at: 2, count: 2 },
                PageEdit::delete(0)
            ]
        );
        assert_eq!(made(8, &deletes), [1, 4, 5, 7]);
        // Pages from either side of a gap close up into it, in their order.
        assert_eq!(made(6, &PageEdit::moves(&[1, 3], 5)), [0, 2, 4, 1, 3, 5]);
        assert_eq!(made(6, &PageEdit::moves(&[1, 4], 3)), [0, 2, 1, 4, 3, 5]);
        assert_eq!(made(6, &PageEdit::moves(&[2, 5], 0)), [2, 5, 0, 1, 3, 4]);
        assert_eq!(made(6, &PageEdit::moves(&[0, 3], 6)), [1, 2, 4, 5, 0, 3]);
        // A block dropped into a gap inside or beside it stays.
        for gap in 2..=4 {
            assert!(PageEdit::moves(&[2, 3], gap).is_empty(), "{gap}");
        }
        // One page moves as the strip always moved one.
        assert_eq!(
            PageEdit::moves(&[1], 3),
            [PageEdit::Move { from: 1, to: 2 }]
        );
        assert_eq!(
            PageEdit::moves(&[3], 0),
            [PageEdit::Move { from: 3, to: 0 }]
        );
    }
}
