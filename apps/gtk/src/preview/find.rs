//! Find in the preview, as the editor finds in a buffer (`editor/search.rs`): the query matched by
//! `accent_core::search` over the page's text, the matches painted in the scheme's `search-match`
//! look within a screen of the view and again as it scrolls, the current one selected. A render
//! matches the page again once its text is all in, and where it wrote the selected match's block
//! anew, the match at the mark it left is selected again. A table's cells are labels of their own,
//! which this does not search.

use super::native::Page;
use crate::editor;
use accent_core::search::{self, Options, Regex};
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

type Found = Box<dyn Fn(&str)>;

pub(super) struct Find(Rc<Inner>);

struct Inner {
    page: Rc<Page>,
    /// The scheme's `search-match` look, over the matches near the view.
    tag: gtk::TextTag,
    /// Where the selected match starts. Left gravity: a render that writes its block anew leaves
    /// it at the block's start.
    mark: gtk::TextMark,
    state: RefCell<State>,
    /// Told the readout, "3 of 12", after each search or step.
    found: RefCell<Option<Found>>,
}

#[derive(Default)]
struct State {
    query: String,
    options: Options,
    /// The query compiled, or `None` while it is empty or does not compile.
    re: Option<Regex>,
    invalid: bool,
    /// Where `re` matches in the page's text, in characters; out of date while `stale`.
    matches: Vec<Range<i32>>,
    stale: bool,
    /// Whether the matches are painted: while the bar is up over the preview.
    highlight: bool,
    /// A repaint is waiting for the main loop, so a run of inserts or scroll steps paints once.
    queued: bool,
    /// Whether the page's changes and the view's scroll are watched, from the first find on.
    hooked: bool,
}

impl Find {
    pub fn new(page: &Rc<Page>) -> Find {
        let buffer = &page.buffer;
        let tag = gtk::TextTag::new(Some("find"));
        buffer.tag_table().add(&tag);
        Find(Rc::new(Inner {
            page: page.clone(),
            tag,
            mark: buffer.create_mark(None, &buffer.start_iter(), true),
            state: RefCell::default(),
            found: RefCell::default(),
        }))
    }

    /// Search for `text` under `options`, painting the matches and selecting the first from the
    /// selection, or from the top of the view; asked again for the same while painted, only the
    /// readout is said again.
    pub fn find(&self, text: &str, options: Options) {
        let find = &self.0;
        let asked = {
            let state = find.state.borrow();
            state.query != text || state.options != options || !state.highlight
        };
        if asked {
            find.set_query(text, options);
            find.state.borrow_mut().highlight = true;
            find.hook();
            // From the current match, not past it: typing must not walk through the page.
            find.step(true, true);
        }
        find.paint();
        find.say();
    }

    pub fn next(&self) {
        self.0.step(true, false);
        self.0.say();
    }

    pub fn previous(&self) {
        self.0.step(false, false);
        self.0.say();
    }

    /// The bar went away: nothing painted and nothing said, the query kept for F3.
    pub fn clear(&self) {
        self.0.state.borrow_mut().highlight = false;
        self.0.paint();
        if let Some(found) = self.0.found.borrow().as_ref() {
            found("");
        }
    }

    pub fn connect_found(&self, f: impl Fn(&str) + 'static) {
        *self.0.found.borrow_mut() = Some(Box::new(f));
    }
}

impl Inner {
    fn set_query(&self, text: &str, options: Options) {
        let compiled = (!text.is_empty()).then(|| search::pattern(text, options));
        let mut state = self.state.borrow_mut();
        state.invalid = matches!(compiled, Some(Err(_)));
        state.re = compiled.and_then(Result::ok);
        state.query = text.to_string();
        state.options = options;
        state.stale = true;
    }

    /// Repaint as the view scrolls, and match again once the page changed.
    fn hook(self: &Rc<Self>) {
        if std::mem::replace(&mut self.state.borrow_mut().hooked, true) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.page.buffer.connect_changed(move |_| {
            if let Some(find) = weak.upgrade() {
                find.state.borrow_mut().stale = true;
                // Below the render's batches and GTK's measuring: once the text is all in.
                find.queue(glib::Priority::LOW);
            }
        });
        let weak = Rc::downgrade(self);
        if let Some(adjustment) = self.page.view.vadjustment() {
            adjustment.connect_value_changed(move |_| {
                if let Some(find) = weak.upgrade() {
                    find.queue(glib::Priority::HIGH_IDLE);
                }
            });
        }
    }

    /// Paint again from an idle at `priority` and say the readout again; after a change, first
    /// select the match a render took the selection from.
    fn queue(self: &Rc<Self>, priority: glib::Priority) {
        {
            let mut state = self.state.borrow_mut();
            if !state.highlight || std::mem::replace(&mut state.queued, true) {
                return;
            }
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_full(priority, move || {
            if let Some(find) = weak.upgrade() {
                let changed = {
                    let mut state = find.state.borrow_mut();
                    state.queued = false;
                    state.stale
                };
                if changed {
                    find.keep();
                }
                find.paint();
                find.say();
            }
            glib::ControlFlow::Break
        });
    }

    /// Match the query again if the page changed since.
    fn rematch(&self) {
        let mut state = self.state.borrow_mut();
        if !state.stale {
            return;
        }
        state.matches = match &state.re {
            Some(re) => {
                let (start, end) = self.page.buffer.bounds();
                // A slice, which keeps a picture's or a table's U+FFFC: offsets stay the buffer's.
                let text = self.page.buffer.slice(&start, &end, true);
                (search::char_ranges(re, &text).into_iter())
                    .map(|m| m.start as i32..m.end as i32)
                    .collect()
            }
            None => Vec::new(),
        };
        state.stale = false;
    }

    /// Select the next match by [`editor::next_match`]'s rules, from the selection or else the
    /// top of the view, and bring it into view.
    fn step(&self, forward: bool, from_current: bool) {
        self.rematch();
        let buffer = &self.page.buffer;
        let at = match buffer.selection_bounds() {
            Some((start, end)) => start.offset()..end.offset(),
            None => {
                let view = &self.page.view;
                let top = view.line_at_y(view.visible_rect().y()).0.offset();
                top..top
            }
        };
        let found = editor::next_match(&self.state.borrow().matches, at, forward, from_current);
        if let Some(m) = found {
            self.select(m);
            self.reveal();
        }
    }

    fn select(&self, m: Range<i32>) {
        let buffer = &self.page.buffer;
        let start = buffer.iter_at_offset(m.start);
        buffer.select_range(&start, &buffer.iter_at_offset(m.end));
        buffer.move_mark(&self.mark, &start);
    }

    /// The page changed: where a render took the selection away with its block, the match at or
    /// after the mark it left is the one selected.
    fn keep(&self) {
        if self.page.buffer.has_selection() {
            return;
        }
        self.rematch();
        let at = self.page.buffer.iter_at_mark(&self.mark).offset();
        let found = editor::next_match(&self.state.borrow().matches, at..at, true, true);
        if let Some(m) = found {
            self.select(m);
        }
    }

    /// Bring the selection into view a third of the way down, unless it is in view: the
    /// adjustment set at once and its upper bound widened, as `Page::jump` does.
    fn reveal(&self) {
        let view = &self.page.view;
        let (Some(adjustment), Some((start, _))) =
            (view.vadjustment(), self.page.buffer.selection_bounds())
        else {
            return;
        };
        let (shown, at) = (view.visible_rect(), view.iter_location(&start));
        let inside = at.y() >= shown.y() && at.y() + at.height() <= shown.y() + shown.height();
        if view.height() == 0 || inside {
            return;
        }
        let below = f64::from(at.y() - shown.y() - shown.height() / 3);
        let wanted = (adjustment.value() + below).max(adjustment.lower());
        adjustment.configure(
            wanted,
            adjustment.lower(),
            adjustment.upper().max(wanted + adjustment.page_size()),
            adjustment.step_increment(),
            adjustment.page_increment(),
            adjustment.page_size(),
        );
    }

    /// Paint the matches within a screen of the view, or none while the bar is away: a tag on
    /// each of a hundred thousand matches is seconds of work.
    fn paint(&self) {
        let page = &self.page;
        let Some(source) = page.buffer.downcast_ref::<sourceview5::Buffer>() else {
            return;
        };
        let mut wanted = Vec::new();
        if self.state.borrow().highlight {
            editor::match_style(source, &self.tag);
            // Over every tag, code's included, as the editor's own find tag is.
            self.tag.set_priority(source.tag_table().size() - 1);
            self.rematch();
            let shown = page.view.visible_rect();
            let from = page.view.line_at_y(shown.y() - shown.height()).0.offset();
            let mut to = page.view.line_at_y(shown.y() + 2 * shown.height()).0;
            to.forward_line();
            let to = to.offset();
            let state = self.state.borrow();
            let first = state.matches.partition_point(|m| m.end < from);
            wanted.extend(
                (state.matches[first..].iter())
                    .take_while(|m| m.start <= to)
                    .filter(|m| !m.is_empty())
                    .cloned(),
            );
        }
        crate::highlight::sync_tag(source, &self.tag, wanted);
    }

    fn say(&self) {
        let label = self.label();
        if let Some(found) = self.found.borrow().as_ref() {
            found(&label);
        }
    }

    /// The readout: which match the selection is, of how many.
    fn label(&self) -> String {
        {
            let state = self.state.borrow();
            if state.query.is_empty() {
                return String::new();
            }
            if state.invalid {
                return "Invalid pattern".to_string();
            }
        }
        self.rematch();
        let state = self.state.borrow();
        let at = self.page.buffer.selection_bounds();
        let at = at.map(|(start, end)| start.offset()..end.offset());
        let position = at.and_then(|at| state.matches.iter().position(|m| *m == at));
        let at = position.map_or(0, |i| i as u32 + 1);
        matches_label(at, state.matches.len() as u32)
    }
}

/// Match `at` (1-based, 0 for none selected) of `total`: "3 of 12", "12 matches", "No results".
pub(crate) fn matches_label(at: u32, total: u32) -> String {
    match (at, total) {
        (_, 0) => "No results".to_string(),
        (0, n) => format!("{n} matches"),
        (at, n) => format!("{at} of {n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_label_says_where_the_reader_is() {
        assert_eq!(matches_label(0, 0), "No results");
        assert_eq!(matches_label(1, 1), "1 of 1");
        assert_eq!(matches_label(3, 12), "3 of 12");
        // Nothing selected, or a selection that is no match.
        assert_eq!(matches_label(0, 12), "12 matches");
    }
}
