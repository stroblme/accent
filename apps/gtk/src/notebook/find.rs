//! Find over a notebook's cells, for the pane's find bar, which addresses a notebook as it does a
//! PDF (`find::PreviewOp`): the query is matched by `accent_core::search` as a buffer's is, a cell
//! at a time, so `^` is the start of a cell and no match runs on into the next. The matches are
//! counted through the cells as one text; the current one is the selected cell's selection; they
//! are painted only in the cells within a screen of the view. A search hit lands here too.

use std::ops::Range;
use std::rc::Rc;

use accent_core::notebook::{CellId, Edit};
use accent_core::search::{self, Options, Regex};

use super::NotebookTab;
use crate::*;

/// The tag a cell's buffer paints the matches with, made the first time it paints one.
const TAG: &str = "notebook-find";

/// The find bar's query over the cells, and where it matches.
#[derive(Default)]
pub(super) struct Find {
    query: String,
    options: Options,
    /// The query compiled, or `None` while it is empty or does not compile.
    re: Option<Regex>,
    invalid: bool,
    /// Whether the matches are painted: while the bar is up over the notebook.
    highlight: bool,
    /// Where the query matches, in characters counted through the cells' sources as one text, a
    /// line break after each; out of date while `stale`.
    matches: Vec<Range<i32>>,
    /// Where each cell's source starts in that count.
    starts: Vec<i32>,
    stale: bool,
    /// A repaint is waiting for the main loop, so a run of edits or scroll steps paints once.
    queued: bool,
    /// The cells whose matches are painted.
    painted: Vec<CellId>,
    /// Whether scrolling repaints, which it does from the first find on.
    scrolling: bool,
}

impl NotebookTab {
    /// Search the cells for `text` under `options`, painting the matches and selecting the first
    /// from the caret; asked again for the same while painted, only the readout is said again.
    pub fn find(self: &Rc<Self>, text: &str, options: Options) {
        let asked = {
            let find = self.find.borrow();
            find.query != text || find.options != options || !find.highlight
        };
        if asked {
            self.set_query(text, options);
            self.find.borrow_mut().highlight = true;
            self.hook_scroll();
            // From the current match, not past it: typing must not walk through the notebook.
            self.step(true, true);
        }
        self.paint();
        self.on_matches.emit(self);
    }

    /// Take `text` under `options` as the query, matched when next asked.
    fn set_query(&self, text: &str, options: Options) {
        let compiled = (!text.is_empty()).then(|| search::pattern(text, options));
        let mut find = self.find.borrow_mut();
        find.invalid = matches!(compiled, Some(Err(_)));
        find.re = compiled.and_then(Result::ok);
        find.query = text.to_string();
        find.options = options;
        find.stale = true;
    }

    /// The match after the selection, or the one before it, round past either end.
    pub fn step_match(self: &Rc<Self>, forward: bool) {
        self.step(forward, false);
        self.on_matches.emit(self);
    }

    /// Select the next match by [`editor::next_match`]'s rules: from the selection's start with
    /// `from_current`, otherwise past it.
    fn step(self: &Rc<Self>, forward: bool, from_current: bool) {
        self.rematch();
        let found = {
            let find = self.find.borrow();
            let at = self.selection_at(&find.starts);
            editor::next_match(&find.matches, at, forward, from_current).map(|m| {
                let cell = find.starts.partition_point(|s| *s <= m.start) - 1;
                let start = find.starts[cell];
                (cell, m.start - start..m.end - start)
            })
        };
        if let Some((cell, chars)) = found {
            self.show_match(cell, chars);
        }
    }

    /// Select characters `chars` of the `cell`th cell and bring them into view, the keyboard left
    /// where it is, in the find bar.
    fn show_match(self: &Rc<Self>, cell: usize, chars: Range<i32>) {
        let Some(id) = self.views.borrow().get(cell).map(|v| v.id) else {
            return;
        };
        self.select(id);
        let Some((_, editor)) = self.editor(id) else {
            return;
        };
        let buffer = &editor.buffer;
        buffer.select_range(
            &buffer.iter_at_offset(chars.start),
            &buffer.iter_at_offset(chars.end),
        );
        self.show(id, None);
    }

    /// The selected cell's selection, or its caret, in the count the matches are in; the
    /// notebook's start while no cell is selected.
    fn selection_at(&self, starts: &[i32]) -> Range<i32> {
        let Some(id) = self.selected.get() else {
            return 0..0;
        };
        let base = self.position_of(id).and_then(|at| starts.get(at).copied());
        let base = base.unwrap_or(0);
        let Some(editor) = self.view_of(Some(id)).and_then(|v| v.editor().cloned()) else {
            return base..base;
        };
        let buffer = &editor.buffer;
        let caret = buffer.iter_at_mark(&buffer.get_insert());
        let (start, end) = buffer.selection_bounds().unwrap_or((caret, caret));
        base + start.offset()..base + end.offset()
    }

    /// Match the query again if the cells changed since, over the model brought up to date, so
    /// no cell's editor has to be built for it.
    fn rematch(&self) {
        if !self.find.borrow().stale {
            return;
        }
        self.sync_model();
        let model = self.model.borrow();
        let find = &mut *self.find.borrow_mut();
        let (mut matches, mut starts, mut at) = (Vec::new(), Vec::new(), 0);
        for cell in model.cells() {
            starts.push(at);
            if let Some(re) = &find.re {
                let found = search::char_ranges(re, cell.source()).into_iter();
                matches.extend(found.map(|m| at + m.start as i32..at + m.end as i32));
            }
            at += cell.source().chars().count() as i32 + 1;
        }
        (find.matches, find.starts, find.stale) = (matches, starts, false);
    }

    /// Paint the matches in the cells within a screen of the view, and take the paint off those
    /// that had it: a tag on each of a hundred thousand matches is seconds of work.
    fn paint(&self) {
        let painted = std::mem::take(&mut self.find.borrow_mut().painted);
        for editor in painted
            .into_iter()
            .filter_map(|id| self.view_of(Some(id))?.editor().cloned())
        {
            if let Some(tag) = editor.buffer.tag_table().lookup(TAG) {
                let (start, end) = editor.buffer.bounds();
                editor.buffer.remove_tag(&tag, &start, &end);
            }
        }
        if !self.find.borrow().highlight {
            return;
        }
        self.rematch();
        let mut painted = Vec::new();
        for view in self.near() {
            let (Some(editor), Some(cell)) = (view.editor(), self.position_of(view.id)) else {
                continue;
            };
            let mine: Vec<Range<i32>> = {
                let find = self.find.borrow();
                let start = find.starts[cell];
                let end = find.starts.get(cell + 1).copied().unwrap_or(i32::MAX);
                let first = find.matches.partition_point(|m| m.start < start);
                find.matches[first..]
                    .iter()
                    .take_while(|m| m.start < end)
                    .filter(|m| !m.is_empty())
                    .map(|m| m.start - start..m.end - start)
                    .collect()
            };
            if mine.is_empty() {
                continue;
            }
            let buffer = &editor.buffer;
            let tag = find_tag(buffer);
            for m in mine {
                buffer.apply_tag(
                    &tag,
                    &buffer.iter_at_offset(m.start),
                    &buffer.iter_at_offset(m.end),
                );
            }
            painted.push(view.id);
        }
        self.find.borrow_mut().painted = painted;
    }

    /// Paint again once the main loop is idle, and say the readout again.
    fn queue_paint(self: &Rc<Self>) {
        {
            let mut find = self.find.borrow_mut();
            if !find.highlight || std::mem::replace(&mut find.queued, true) {
                return;
            }
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
            if let Some(tab) = weak.upgrade() {
                tab.find.borrow_mut().queued = false;
                tab.paint();
                tab.on_matches.emit(&tab);
            }
            glib::ControlFlow::Break
        });
    }

    /// Paint again as the view scrolls, from the first find on.
    fn hook_scroll(self: &Rc<Self>) {
        if std::mem::replace(&mut self.find.borrow_mut().scrolling, true) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.scroller.vadjustment().connect_value_changed(move |_| {
            if let Some(tab) = weak.upgrade() {
                tab.queue_paint();
            }
        });
    }

    /// The cells changed: matched again when next asked, and at once while painted.
    pub(super) fn find_changed(self: &Rc<Self>) {
        self.find.borrow_mut().stale = true;
        self.queue_paint();
    }

    /// The bar went away: nothing painted, the query kept for F3.
    pub fn find_clear(&self) {
        self.find.borrow_mut().highlight = false;
        self.paint();
    }

    /// The readout: "3 of 12", "12 matches", "No results", "Invalid pattern", or nothing for no
    /// query.
    pub fn matches_label(&self) -> String {
        {
            let find = self.find.borrow();
            if find.query.is_empty() {
                return String::new();
            }
            if find.invalid {
                return "Invalid pattern".to_string();
            }
        }
        self.rematch();
        let find = self.find.borrow();
        let at = self.selection_at(&find.starts);
        match (
            find.matches.len(),
            find.matches.iter().position(|m| *m == at),
        ) {
            (0, _) => "No results".to_string(),
            (count, Some(i)) => format!("{} of {count}", i + 1),
            (count, None) => format!("{count} matches"),
        }
    }

    pub fn find_invalid(&self) -> bool {
        self.find.borrow().invalid
    }

    /// The selected cell's selection, when it is one line: the find bar's query on opening.
    pub fn selected_query(&self) -> Option<String> {
        let editor = self.view_of(self.selected.get())?.editor()?.clone();
        let (start, end) = editor.buffer.selection_bounds()?;
        let text = editor.buffer.text(&start, &end, false).to_string();
        (!text.is_empty() && !text.contains('\n')).then_some(text)
    }

    /// Whether the cells can be rewritten: not while presented.
    pub fn rewritable(&self) -> bool {
        !self.presenting.get()
    }

    /// Replace the selected match and go on to the next, one Undo step; or `all` of them, in every
    /// cell, one step through the model. Neither while presented.
    pub fn replace(self: &Rc<Self>, with: &str, all: bool) {
        let (re, options) = {
            let find = self.find.borrow();
            (find.re.clone(), find.options)
        };
        let Some(re) = re.filter(|_| self.rewritable()) else {
            return;
        };
        if all {
            self.operate(|nb| {
                let edits = nb
                    .cells()
                    .iter()
                    .filter(|c| re.is_match(c.source()))
                    .map(|c| Edit::Source {
                        id: c.id(),
                        text: substitute(&re, c.source(), with, options),
                    })
                    .collect();
                Some(nb.apply_all(edits))
            });
        } else {
            self.replace_selected(&re, with, options);
            self.step(true, false);
        }
        self.on_matches.emit(self);
    }

    /// Replace the selected cell's selection where it is a match, as one user action.
    fn replace_selected(self: &Rc<Self>, re: &Regex, with: &str, options: Options) {
        let Some((_, editor)) = self.selected.get().and_then(|id| self.editor(id)) else {
            return;
        };
        let buffer = &editor.buffer;
        let Some((mut start, mut end)) = buffer.selection_bounds() else {
            return;
        };
        let at = start.offset() as usize..end.offset() as usize;
        let found = search::replacements(re, &editor.text(), with, options)
            .into_iter()
            .find(|(m, _)| *m == at);
        if let Some((_, text)) = found {
            buffer.begin_user_action();
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &text);
            buffer.end_user_action();
        }
    }

    /// Go to Cell: the `n`th cell from 0, the last past the end.
    pub fn goto_cell(self: &Rc<Self>, n: usize) {
        let id = {
            let views = self.views.borrow();
            views.get(n).or(views.last()).map(|v| v.id)
        };
        if let Some(id) = id {
            self.goto(id, 0);
        }
    }

    /// A search hit on bytes `bytes` of what the index holds of the notebook (`Notebook::place`):
    /// its cell selected with the match, the keyboard in it, and the match the query F3 steps
    /// through, nothing painted, as a note's hit is (`Tab::goto_range`).
    pub fn reveal_hit(self: &Rc<Self>, bytes: Range<usize>) {
        self.sync_model();
        let found = {
            let model = self.model.borrow();
            model.place(bytes.start).map(|(cell, at)| {
                let cell = &model.cells()[cell];
                let source = cell.source();
                let chars = |at: usize| source[..source.floor_char_boundary(at)].chars().count();
                let chars = chars(at) as i32..chars(at + bytes.len()) as i32;
                (cell.id(), chars)
            })
        };
        let Some((id, chars)) = found else {
            return;
        };
        self.focus_cell(id);
        let Some((_, editor)) = self.editor(id) else {
            return;
        };
        let buffer = &editor.buffer;
        let (start, end) = (
            buffer.iter_at_offset(chars.start),
            buffer.iter_at_offset(chars.end),
        );
        buffer.select_range(&start, &end);
        self.show(id, Some(0.3));
        let options = self.find.borrow().options;
        self.set_query(&buffer.text(&start, &end, false), options);
        self.find_clear();
    }
}

/// What Replace writes over `text`'s matches: `with` as written, or, under Regular Expression,
/// with `$1` naming a group, as Replace All in Files writes it.
fn substitute(re: &Regex, text: &str, with: &str, options: Options) -> String {
    match options.regex {
        true => re.replace_all(text, with).into_owned(),
        false => re.replace_all(text, search::NoExpand(with)).into_owned(),
    }
}

/// `buffer`'s match tag, above every other tag as the editor's own find tag is, in the colours
/// the style scheme gives a match now.
fn find_tag(buffer: &sourceview5::Buffer) -> gtk::TextTag {
    let table = buffer.tag_table();
    let tag = table.lookup(TAG).unwrap_or_else(|| {
        let tag = gtk::TextTag::new(Some(TAG));
        table.add(&tag);
        tag
    });
    tag.set_priority(table.size() - 1);
    editor::match_style(buffer, &tag);
    tag
}

impl App {
    /// The notebook `pane` shows, which its find bar searches as it does a PDF.
    pub(crate) fn notebook_of(&self, pane: &Pane) -> Option<Rc<NotebookTab>> {
        self.doc_of(pane)?.notebook().cloned()
    }

    /// The find bar of the pane `tab` is in front in says its matches, and the Outline follows its
    /// caret while it is the tab in front.
    pub(super) fn wire_notebook_places(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        let weak = Rc::downgrade(self);
        tab.on_matches.set(Rc::new(move |tab: &Rc<NotebookTab>| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let Some(pane) = app.pane_of(&tab.page) else {
                return;
            };
            if app.notebook_of(&pane).is_some_and(|n| Rc::ptr_eq(&n, tab)) {
                pane.find.set_matches_text(&tab.matches_label());
                pane.find.set_invalid(tab.find_invalid());
            }
        }));
        let weak = Rc::downgrade(self);
        tab.on_caret.set(Rc::new(move |tab: &Rc<NotebookTab>| {
            if let Some(app) = weak.upgrade()
                && app.active_notebook().is_some_and(|n| Rc::ptr_eq(&n, tab))
            {
                app.follow_outline();
            }
        }));
    }
}

/// What `ACCENT_BENCH_NOTEBOOK` reads of a find.
#[cfg(feature = "bench")]
impl NotebookTab {
    /// How many matches the query has, the selected cell's place, and what is selected in it.
    pub fn bench_found(&self) -> (usize, Option<usize>, String) {
        self.rematch();
        let count = self.find.borrow().matches.len();
        let selected = self.view_of(self.selected.get()).and_then(|v| {
            let editor = v.editor()?;
            let (start, end) = editor.buffer.selection_bounds()?;
            Some(editor.buffer.text(&start, &end, false).to_string())
        });
        (count, self.selected_at(), selected.unwrap_or_default())
    }

    /// Whether the selected cell's caret is inside the view.
    pub fn bench_caret_shown(&self) -> bool {
        let editor = self
            .view_of(self.selected.get())
            .and_then(|v| v.editor().cloned());
        let adjustment = self.scroller.vadjustment();
        let (value, page) = (adjustment.value(), adjustment.page_size());
        editor
            .and_then(|editor| self.caret_at(&editor))
            .is_some_and(|(top, bottom)| top >= value && bottom <= value + page)
    }
}
