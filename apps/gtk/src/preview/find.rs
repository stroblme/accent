//! Find in the preview, as the editor finds in a buffer (`editor/search.rs`): the query matched by
//! `accent_core::search` over the page's text, the matches painted in the scheme's `search-match`
//! look within a screen of the view and again as it scrolls, the current one selected. A render
//! matches the page again once its text is all in, and where it wrote the selected match's block
//! anew, the match at the mark it left is selected again. A table's cells are labels of their
//! own: each is matched alone, its matches counted at the table's place in the text, painted by
//! the label's attributes and the current one selected in the label.

use super::Page;
use super::table::Table;
use crate::editor;
use accent_core::search::{self, Options, Regex};
use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

type Found = Box<dyn Fn(&str)>;

/// Where a match starts or ends, in the order the page is read: a character of the page's text,
/// or one of a table cell's, at the table's U+FFFC, `cell` counting the page's cells from 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct At {
    offset: i32,
    cell: usize,
    char: i32,
}

impl At {
    fn text(offset: i32) -> At {
        At {
            offset,
            ..At::default()
        }
    }
}

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
    /// Where `re` matches in the page's text and its cells; out of date while `stale`.
    matches: Vec<Range<At>>,
    /// The page's cells in order, and where each one's table stands in the text.
    cells: Vec<(i32, gtk::Label)>,
    /// The match selected in a cell, its characters: a cell's selection is the label's own.
    selected: Option<(gtk::Label, Range<i32>)>,
    /// The cells whose matches are painted.
    painted: Vec<gtk::Label>,
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
        state.stale = false;
        state.matches.clear();
        state.cells.clear();
        let Some(re) = state.re.clone() else {
            return;
        };
        let buffer = &self.page.buffer;
        let (start, end) = buffer.bounds();
        // A slice, which keeps a picture's or a table's U+FFFC: offsets stay the buffer's.
        let text = buffer.slice(&start, &end, true);
        let mut matches: Vec<Range<At>> = (search::char_ranges(&re, &text).into_iter())
            .map(|m| At::text(m.start as i32)..At::text(m.end as i32))
            .collect();
        let objects = text.chars().enumerate().filter(|(_, c)| *c == '\u{fffc}');
        for (offset, _) in objects {
            let offset = offset as i32;
            let Some(anchor) = buffer.iter_at_offset(offset).child_anchor() else {
                continue;
            };
            let tables = anchor
                .widgets()
                .into_iter()
                .filter_map(|w| w.downcast::<Table>().ok());
            for label in tables.flat_map(|t| t.labels()) {
                state.cells.push((offset, label.clone()));
                let cell = state.cells.len();
                let at = |char: usize| At {
                    offset,
                    cell,
                    char: char as i32,
                };
                let found = search::char_ranges(&re, &label.text());
                matches.extend(found.into_iter().map(|m| at(m.start)..at(m.end)));
            }
        }
        matches.sort_by_key(|m| m.start);
        state.matches = matches;
    }

    /// The match the reader is at: the page's selection, else the cell match selected while its
    /// label still holds it.
    fn current(&self) -> Option<Range<At>> {
        if let Some((start, end)) = self.page.buffer.selection_bounds() {
            return Some(At::text(start.offset())..At::text(end.offset()));
        }
        let state = self.state.borrow();
        let (label, chars) = state.selected.as_ref()?;
        if label.selection_bounds() != Some((chars.start, chars.end)) {
            return None;
        }
        let i = state.cells.iter().position(|(_, l)| l == label)?;
        let at = |char| At {
            offset: state.cells[i].0,
            cell: i + 1,
            char,
        };
        Some(at(chars.start)..at(chars.end))
    }

    /// Select the next match by [`editor::next_match`]'s rules, from the selection or else the
    /// top of the view, and bring it into view.
    fn step(&self, forward: bool, from_current: bool) {
        self.rematch();
        let at = self.current().unwrap_or_else(|| {
            let view = &self.page.view;
            let top = At::text(view.line_at_y(view.visible_rect().y()).0.offset());
            top..top
        });
        let found = editor::next_match(&self.state.borrow().matches, at, forward, from_current);
        if let Some(m) = found {
            self.select(&m);
            self.reveal();
        }
    }

    fn select(&self, m: &Range<At>) {
        let buffer = &self.page.buffer;
        let start = buffer.iter_at_offset(m.start.offset);
        let previous = self.state.borrow_mut().selected.take();
        if let Some((label, _)) = previous {
            label.select_region(0, 0);
        }
        match m.start.cell {
            0 => buffer.select_range(&start, &buffer.iter_at_offset(m.end.offset)),
            cell => {
                buffer.place_cursor(&start);
                let label = self.state.borrow().cells[cell - 1].1.clone();
                label.select_region(m.start.char, m.end.char);
                self.state.borrow_mut().selected = Some((label, m.start.char..m.end.char));
            }
        }
        buffer.move_mark(&self.mark, &start);
    }

    /// The page changed: where a render took the selection away with its block, the match at or
    /// after the mark it left is the one selected.
    fn keep(&self) {
        self.rematch();
        if self.current().is_some() {
            return;
        }
        let at = At::text(self.page.buffer.iter_at_mark(&self.mark).offset());
        let found = editor::next_match(&self.state.borrow().matches, at..at, true, true);
        if let Some(m) = found {
            self.select(&m);
        }
    }

    /// Bring the selection into view a third of the way down, unless it is in view: the
    /// adjustment set at once and its upper bound widened, as `Page::jump` does.
    fn reveal(&self) {
        let view = &self.page.view;
        let (Some(adjustment), Some(m)) = (view.vadjustment(), self.current()) else {
            return;
        };
        let mut at = view.iter_location(&self.page.buffer.iter_at_offset(m.start.offset));
        // In a cell, the label's own lines, once its table is laid out.
        let label = m
            .start
            .cell
            .checked_sub(1)
            .map(|i| self.state.borrow().cells[i].1.clone());
        if let Some(bounds) = label.and_then(|l| l.compute_bounds(view)) {
            let (_, y) =
                view.window_to_buffer_coords(gtk::TextWindowType::Widget, 0, bounds.y() as i32);
            at = gtk::gdk::Rectangle::new(at.x(), y, at.width(), bounds.height() as i32);
        }
        let shown = view.visible_rect();
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
        let mut cells: Vec<(usize, Vec<Range<i32>>)> = Vec::new();
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
            let first = state.matches.partition_point(|m| m.end < At::text(from));
            let near = (state.matches[first..].iter())
                .take_while(|m| m.start.offset <= to)
                .filter(|m| !m.is_empty());
            for m in near {
                let chars = m.start.char..m.end.char;
                match (m.start.cell, cells.last_mut()) {
                    (0, _) => wanted.push(m.start.offset..m.end.offset),
                    (cell, Some((last, ranges))) if *last == cell => ranges.push(chars),
                    (cell, _) => cells.push((cell, vec![chars])),
                }
            }
        }
        crate::highlight::sync_tag(source, &self.tag, wanted);
        let mut state = self.state.borrow_mut();
        for label in state.painted.drain(..) {
            label.set_attributes(None);
        }
        for (cell, ranges) in cells {
            let label = state.cells[cell - 1].1.clone();
            label.set_attributes(Some(&painted(&self.tag, &label.text(), &ranges)));
            state.painted.push(label);
        }
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
        let at = self.current();
        let state = self.state.borrow();
        let position = at.and_then(|at| state.matches.iter().position(|m| *m == at));
        let at = position.map_or(0, |i| i as u32 + 1);
        matches_label(at, state.matches.len() as u32)
    }
}

/// Characters `ranges` of a label's `text` in `look`'s colours, as attributes: a label has no tags.
fn painted(look: &gtk::TextTag, text: &str, ranges: &[Range<i32>]) -> pango::AttrList {
    let list = pango::AttrList::new();
    let byte = |c: i32| {
        text.char_indices()
            .nth(c as usize)
            .map_or(text.len(), |(b, _)| b) as u32
    };
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    let background = look.background_rgba().filter(|_| look.is_background_set());
    let foreground = look.foreground_rgba().filter(|_| look.is_foreground_set());
    for range in ranges {
        let add = |attribute: pango::Attribute| {
            let mut attribute = attribute;
            attribute.set_start_index(byte(range.start));
            attribute.set_end_index(byte(range.end));
            list.insert(attribute);
        };
        if let Some(c) = background {
            let (r, g, b) = (channel(c.red()), channel(c.green()), channel(c.blue()));
            add(pango::AttrColor::new_background(r, g, b).into());
            add(pango::AttrInt::new_background_alpha(channel(c.alpha()).max(1)).into());
        }
        if let Some(c) = foreground {
            let (r, g, b) = (channel(c.red()), channel(c.green()), channel(c.blue()));
            add(pango::AttrColor::new_foreground(r, g, b).into());
        }
    }
    list
}

/// Match `at` (1-based, 0 for none selected) of `total`: "3 of 12", "12 matches", "No results".
fn matches_label(at: u32, total: u32) -> String {
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
