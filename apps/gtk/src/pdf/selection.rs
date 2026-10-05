//! Selecting text on the page: what a drag covers, what it puts on the clipboard, and the links
//! and highlights a click on the page lands on.

use std::collections::HashMap;
use std::rc::Rc;

use accent_core::pdf::{self, LinkTarget, link_with_alias};
use adw::prelude::*;

use super::protocol::Request;
use super::tab::PdfTab;
use super::{PdfView, Span};

impl PdfTab {
    /// Put the selected text on the clipboard. Nothing selected is not an error: Ctrl+C on a
    /// page with no selection simply leaves the clipboard alone.
    pub fn copy_selection(&self) {
        let text = self.selected.borrow().clone();
        if text.is_empty() {
            return;
        }
        self.view.clipboard().set_text(&text);
    }

    /// Copy the selection as a wikilink into this PDF, with the selected text as its alias.
    ///
    /// Pasting that link into a note is what makes it a highlight: the index sees a link into a
    /// page and a selection, and the viewer paints it. There is no separate Highlight action,
    /// because a highlight is a link and the clipboard is how a link gets where it is wanted.
    ///
    /// One link per page, joined by newlines: `page=N&selection=…` names one page, and a drag
    /// that ran across a break is two places in the document.
    pub fn copy_link(&self) {
        // A loose PDF is linked by name: an absolute path in a wikilink resolves nowhere, and
        // the name is what a vault would key it by if the file ever joined one.
        let key = self.key.borrow().clone();
        let rel = match crate::doc::is_loose_key(&key) {
            true => crate::doc::file_name(&key).to_string(),
            false => key,
        };
        let glyphs = self.glyphs.borrow();
        let links: Vec<String> = self
            .ranges
            .borrow()
            .iter()
            .filter_map(|sel| {
                let out = pdf::selection_link(glyphs.get(&sel.page)?, &rel, sel);
                Some(link_with_alias(&out.link, &out.text))
            })
            .collect();
        if links.is_empty() {
            return;
        }
        self.view.clipboard().set_text(&links.join("\n"));
    }

    /// A drag selected the text between two points, which may be on different pages.
    ///
    /// The glyphs are fetched the first time a page is dragged over and kept afterwards, so the
    /// first drag onto a page may land a moment late and every one after it is immediate. A drag
    /// that has crossed a page break wants every page it covers, and is answered as soon as it
    /// has them all.
    pub(super) fn selected_between(self: &Rc<Self>, span: Span) {
        let missing: Vec<usize> = {
            let glyphs = self.glyphs.borrow();
            pages_of(span)
                .filter(|page| !glyphs.contains_key(page))
                .collect()
        };
        if missing.is_empty() {
            self.pending_select.set(None);
            return self.select(span);
        }
        // A drag reports on every motion event, so only pages the drag did not already cover are
        // asked for: nothing drops a `Text` request, so the first ask is always answered.
        let asked = self.pending_select.replace(Some(span));
        for page in missing {
            if asked.is_none_or(|before| !pages_of(before).contains(&page)) {
                self.ask(Request::Text(page));
            }
        }
    }

    /// Mark every glyph between the two ends of the drag and remember the text they spell.
    ///
    /// The two ends are put in document order first, so a drag pulled upwards reads the same way
    /// down as one pulled down. Each page in between contributes all of its glyphs, and the two
    /// at the ends contribute from or up to the glyph nearest the pointer.
    pub(super) fn select(&self, span: Span) {
        let glyphs = self.glyphs.borrow();
        let Some((start, end)) = ends(&glyphs, span) else {
            return;
        };
        let mut text = String::new();
        let mut boxes = Vec::new();
        let mut ranges = Vec::new();
        for page in start.0..=end.0 {
            let Some(page_glyphs) = glyphs.get(&page) else {
                return;
            };
            let lo = match page == start.0 {
                true => start.1,
                false => 0,
            };
            let hi = match page == end.0 {
                true => end.1,
                false => page_glyphs.len().saturating_sub(1),
            };
            let Some(picked) = page_glyphs.get(lo..=hi) else {
                continue;
            };
            // A page break reads as a line break, which is what pasting a passage that runs over
            // one should give.
            if !text.is_empty() {
                text.push('\n');
            }
            text.extend(picked.iter().map(|g| g.ch));
            // The indices as well as the boxes: a link is made of the numbers, and a rectangle
            // on screen cannot be turned back into one.
            ranges.push(pdf::Selection {
                page,
                start: lo,
                end: hi + 1,
            });
            // A glyph with no box of its own — a space between words — would paint as a dot.
            boxes.push((
                page,
                picked
                    .iter()
                    .map(|g| g.rect)
                    .filter(|r| r.width() > 0.0 && r.height() > 0.0)
                    .collect(),
            ));
        }
        *self.selected.borrow_mut() = text;
        *self.ranges.borrow_mut() = ranges;
        self.view.set_selection(boxes);
    }

    /// Drop the selection, on a click that is not a drag.
    pub(super) fn clear_selection(&self) {
        if self.selected.borrow().is_empty() {
            return;
        }
        self.selected.borrow_mut().clear();
        self.ranges.borrow_mut().clear();
        self.view.set_selection(Vec::new());
    }

    /// A click on the page: follow a link if there is one under it.
    /// A click that was not a drag: open the note whose link paints a highlight here.
    ///
    /// After the link handler, which answers on the press — a link inside a highlight is still a
    /// link, and following it is what a click on one has always meant. With Ctrl held it is Go
    /// to Source instead, every LaTeX viewer's SyncTeX click, which does nothing where the window
    /// has disabled it, on a PDF with no SyncTeX file.
    pub(super) fn clicked_highlight(
        self: &Rc<Self>,
        view: &PdfView,
        x: f64,
        y: f64,
        state: gtk::gdk::ModifierType,
    ) {
        if self.link_at(view, x, y).is_some() {
            return;
        }
        if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
            self.pointed.set(view.page_point(x, y));
            let _ = view.activate_action("win.pdf-go-to-source", None);
            return;
        }
        let Some(at) = view.highlight_at(x, y) else {
            return;
        };
        let note = self
            .notes
            .borrow()
            .get(at)
            .map(|l| (l.src_rel_path.clone(), l.byte_start.max(0) as usize));
        if let (Some((rel, byte)), Some(f)) = (note, self.on_note.get()) {
            f(&rel, byte);
        }
    }

    pub(super) fn click(self: &Rc<Self>, view: &PdfView, x: f64, y: f64) {
        self.clear_selection();
        let Some(target) = self.link_at(view, x, y) else {
            return;
        };
        match target {
            LinkTarget::Page { page, top } => {
                self.jumping();
                self.view.goto_page(page, top);
            }
            LinkTarget::Uri(uri) => {
                if let Some(f) = self.on_uri.get() {
                    f(&uri);
                }
            }
        }
    }

    pub(super) fn link_at(&self, view: &PdfView, x: f64, y: f64) -> Option<LinkTarget> {
        let (page, px, py) = view.page_point(x, y)?;
        let links = self.links.borrow();
        links
            .get(&page)?
            .iter()
            .find_map(|link| link.rect.contains((px, py)).then(|| link.target.clone()))
    }
}

/// Every page a drag covers, in document order however the drag was pulled.
pub(super) fn pages_of(span: Span) -> std::ops::RangeInclusive<usize> {
    span.from.0.min(span.to.0)..=span.from.0.max(span.to.0)
}

/// Where a drag starts and ends, each a page and a glyph on it, in document order.
///
/// An end on a page with no text at all — a scan, a full-page figure — has no glyph to anchor
/// to, so it moves inwards to the nearest page of the span that has some: the start to the first
/// glyph of the page after it, the end to the last glyph of the page before it. `None` when no
/// page in the span has text.
fn ends(
    glyphs: &HashMap<usize, Vec<pdf::Glyph>>,
    span: Span,
) -> Option<((usize, usize), (usize, usize))> {
    let (first, last) = match span.from.0 <= span.to.0 {
        true => (span.from, span.to),
        false => (span.to, span.from),
    };
    let at = |(page, point): (usize, (f32, f32))| Some((page, nearest(glyphs.get(&page)?, point)?));
    let text = |page: &usize| glyphs.get(page).is_some_and(|g| !g.is_empty());
    let pages = first.0..=last.0;
    let a = at(first).or_else(|| Some((pages.clone().find(text)?, 0)))?;
    let b = at(last).or_else(|| {
        let page = pages.rev().find(text)?;
        Some((page, glyphs[&page].len() - 1))
    })?;
    Some((a.min(b), a.max(b)))
}

/// The glyph nearest a point on the page, which is the one a drag means to start or end on.
///
/// A hit inside a glyph's own box wins outright; otherwise the closest box by the distance from
/// the point to it, so a drag through the margin still catches the line it is level with.
fn nearest(glyphs: &[pdf::Glyph], (x, y): (f32, f32)) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (i, glyph) in glyphs.iter().enumerate() {
        let r = glyph.rect;
        if r.contains((x, y)) {
            return Some(i);
        }
        // Distance to the box, zero along an axis the point already lies within.
        let dx = (r.left - x).max(0.0).max(x - r.right);
        let dy = (r.top - y).max(0.0).max(y - r.bottom);
        let d = dx * dx + dy * dy;
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.map(|(_, i)| i)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use accent_core::pdf::{Glyph, Rect};

    use super::{Span, ends, link_with_alias};

    /// A page of text, one glyph per character side by side along the top line.
    fn text(chars: &str) -> Vec<Glyph> {
        chars
            .chars()
            .enumerate()
            .map(|(i, ch)| Glyph {
                ch,
                rect: Rect::from_corners((i as f32, 0.0), (i as f32 + 1.0, 1.0)),
                index: i,
            })
            .collect()
    }

    #[test]
    fn an_end_on_a_page_without_text_moves_to_the_nearest_page_with_some() {
        let span = |from: usize, to: usize| Span {
            from: (from, (0.5, 0.5)),
            to: (to, (0.5, 0.5)),
        };
        // A scan at the end: the selection runs to the last glyph of the page before it.
        let pages = HashMap::from([(0, text("ab")), (1, text("cd")), (2, text(""))]);
        assert_eq!(ends(&pages, span(0, 2)), Some(((0, 0), (1, 1))));
        // Pulled upwards it is the same passage.
        assert_eq!(ends(&pages, span(2, 0)), Some(((0, 0), (1, 1))));
        // Only the start has text: from the pointer to the end of that page.
        let pages = HashMap::from([(0, text("ab")), (1, text("")), (2, text(""))]);
        assert_eq!(ends(&pages, span(0, 2)), Some(((0, 0), (0, 1))));
        // A scan at the start: the selection begins at the first glyph of the page after it.
        let pages = HashMap::from([(0, text("")), (1, text("ab"))]);
        assert_eq!(ends(&pages, span(0, 1)), Some(((1, 0), (1, 0))));
        // No text anywhere is nothing to select.
        let pages = HashMap::from([(0, text("")), (1, text(""))]);
        assert_eq!(ends(&pages, span(0, 1)), None);
    }

    #[test]
    fn link_with_alias_strips_what_would_end_the_link() {
        let link = "[[a.pdf#page=1&selection=0,0,0,5]]";
        assert_eq!(
            link_with_alias(link, " a |b]]\n c "),
            "[[a.pdf#page=1&selection=0,0,0,5|a b c]]"
        );
        // Nothing worth quoting is no alias, not an empty one.
        assert_eq!(link_with_alias(link, "  \n "), link);
    }
}
