//! The window's find, replace and go-to-line bar.
//!
//! One bar per window rather than one per tab, and it sits in the editor column's *content*
//! rather than among its top bars. Both follow from presentation mode, which unreveals the top
//! bars and hides the whole tab stack: a bar living inside a tab was on screen right up to the
//! moment `F5` made it useful.
//!
//! Because it outlives any one tab, the bar drives the tab from the outside — `Tab` keeps its
//! `SearchContext` and the caret moves, this file keeps the widgets — and it re-targets on every
//! tab switch. While presenting there is no buffer to search, so the same widgets address the
//! rendered preview through [`PreviewOp`] instead; that indirection is also what keeps this file
//! from having to know what an `App` is.

use crate::editor::Tab;
use adw::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

/// What the bar wants done to the rendered preview while presentation mode is on.
pub enum PreviewOp {
    Find(String),
    Next,
    Previous,
    Clear,
    Line(u32),
}

/// What the bar needs from the window. Two closures, so `find.rs` never names `App`.
pub struct Wiring {
    /// Whether something other than the editor — the rendered preview, or a PDF — is what the
    /// user is looking at, so find and go-to are addressed there instead.
    pub presenting: Box<dyn Fn() -> bool>,
    pub preview: Box<dyn Fn(PreviewOp)>,
    /// How many pages the thing being looked at has, when it is counted in pages rather than
    /// lines. `None` for an editor, which counts lines.
    pub pages: Box<dyn Fn() -> Option<usize>>,
}

/// Which row the bar shows, and whether the replace controls come with it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Find,
    Replace,
    Goto,
}

pub struct Bar {
    bar: gtk::SearchBar,
    rows: gtk::Stack,
    query: gtk::SearchEntry,
    replace: gtk::Entry,
    replace_row: gtk::Box,
    matches: gtk::Label,
    line: gtk::Entry,
    lines: gtk::Label,
    /// The tab being searched, with the handler watching its match count.
    target: RefCell<Option<(Rc<Tab>, glib::SignalHandlerId)>>,
    /// Set while the bar itself moves the caret, so the resulting count notification does not
    /// walk the label back to the value it had before the jump.
    busy: Cell<bool>,
    wiring: OnceCell<Wiring>,
}

/// The 1-based line and column a `line[:column]` entry names, or `None` for anything else.
/// A missing or unparsable column is the start of the line.
pub fn goto_target(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
    let (line, column) = match text.split_once(':') {
        Some((l, c)) => (l, c.trim().parse().unwrap_or(1)),
        None => (text, 1),
    };
    Some((line.trim().parse().ok()?, column))
}

impl Bar {
    pub fn new() -> Rc<Bar> {
        let query = gtk::SearchEntry::builder()
            .placeholder_text("Find")
            .hexpand(true)
            .build();
        let matches = gtk::Label::builder().css_classes(["dim-label"]).build();
        let previous = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Find Previous")
            .build();
        let next = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text("Find Next")
            .build();

        let replace = gtk::Entry::builder()
            .placeholder_text("Replace")
            .hexpand(true)
            .build();
        let replace_one = gtk::Button::with_label("Replace");
        let replace_all = gtk::Button::with_label("Replace All");

        // 6 px inside a control group, 12 px between the two rows (DESIGN.md, Spacing).
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&query);
        top.append(&matches);
        top.append(&previous);
        top.append(&next);

        let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        replace_row.append(&replace);
        replace_row.append(&replace_one);
        replace_row.append(&replace_all);
        replace_row.set_visible(false);

        let find_rows = gtk::Box::new(gtk::Orientation::Vertical, 12);
        find_rows.append(&top);
        find_rows.append(&replace_row);

        let line = gtk::Entry::builder()
            .placeholder_text("Line[:column]")
            .input_purpose(gtk::InputPurpose::FreeForm)
            .hexpand(true)
            .build();
        let lines = gtk::Label::builder().css_classes(["dim-label"]).build();
        let goto_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        goto_row.append(&line);
        goto_row.append(&lines);

        // A stack rather than two bars: the two rows are the same control in two modes, and one
        // `GtkSearchBar` means one Escape, one close button and one reveal animation.
        let rows = gtk::Stack::new();
        rows.add_named(&find_rows, Some("find"));
        rows.add_named(&goto_row, Some("goto"));

        // No `connect_entry`: it would hand the reveal's focus to the find entry even when the
        // bar came up in go-to-line mode. Focus is taken explicitly in `open` instead.
        let bar = gtk::SearchBar::builder().show_close_button(true).build();
        bar.set_child(Some(&rows));

        let this = Rc::new(Bar {
            bar,
            rows,
            query: query.clone(),
            replace: replace.clone(),
            replace_row,
            matches,
            line: line.clone(),
            lines,
            target: RefCell::new(None),
            busy: Cell::new(false),
            wiring: OnceCell::new(),
        });

        query.connect_search_changed(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |entry| bar.search(&entry.text())
        ));
        query.connect_activate(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |_| bar.step(true)
        ));
        next.connect_clicked(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |_| bar.step(true)
        ));
        previous.connect_clicked(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |_| bar.step(false)
        ));
        for button in [&replace_one, &replace_all] {
            let all = button == &replace_all;
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = bar)]
                this,
                move |_| bar.replace(all)
            ));
        }
        replace.connect_activate(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |_| bar.replace(false)
        ));

        line.connect_changed(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |entry| bar.preview_line(&entry.text())
        ));
        line.connect_activate(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |entry| {
                bar.jump(&entry.text());
                bar.close();
            }
        ));

        // Escape leaves the bar and puts the caret back where the user was typing. Capture phase,
        // because `GtkSearchEntry` binds Escape to `stop-search` and would eat it first.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| match key {
                gdk::Key::Escape => {
                    bar.close();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        ));
        this.bar.add_controller(keys);
        this.bar.connect_search_mode_enabled_notify(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |widget| {
                if !widget.is_search_mode() {
                    bar.clear();
                }
            }
        ));
        this
    }

    pub fn widget(&self) -> &gtk::SearchBar {
        &self.bar
    }

    /// Whether the bar is up, which is one of the things that suspends the chrome fade.
    pub fn is_open(&self) -> bool {
        self.bar.is_search_mode()
    }

    pub fn wire(&self, wiring: Wiring) {
        let _ = self.wiring.set(wiring);
    }

    /// Point the bar at the tab that just became active, dropping the highlight on the old one.
    pub fn retarget(self: &Rc<Self>, tab: Option<Rc<Tab>>) {
        if let Some((old, handler)) = self.target.borrow_mut().take() {
            old.set_highlight(false);
            old.search_context().disconnect(handler);
        }
        let Some(tab) = tab else { return };
        let handler = tab
            .search_context()
            .connect_occurrences_count_notify(glib::clone!(
                #[weak(rename_to = bar)]
                self,
                move |_| bar.refresh_matches()
            ));
        if self.is_open() && self.rows.visible_child_name().as_deref() == Some("find") {
            tab.set_query(&self.query.text());
            tab.set_highlight(true);
        }
        *self.target.borrow_mut() = Some((tab, handler));
        self.refresh_matches();
    }

    /// Reveal the bar in `mode`, prefilled from the selection when there is one worth searching.
    pub fn open(self: &Rc<Self>, mode: Mode) {
        match mode {
            Mode::Goto => {
                // A PDF is counted in pages and everything else in lines, and the row says so
                // in both places: a "Line[:column]" prompt over a page count is nonsense.
                let (label, placeholder) = match self.wiring.get().and_then(|w| (w.pages)()) {
                    Some(pages) => (format!("of {pages} pages"), "Page"),
                    None => {
                        let count = self.tab().map(|tab| tab.line_count()).unwrap_or(0);
                        (format!("of {count} lines"), "Line[:column]")
                    }
                };
                self.lines.set_text(&label);
                self.line.set_placeholder_text(Some(placeholder));
                self.rows.set_visible_child_name("goto");
                self.bar.set_search_mode(true);
                self.line.grab_focus();
                self.line.select_region(0, -1);
            }
            _ => {
                if let Some(selected) = self.tab().and_then(|tab| tab.selected_query()) {
                    self.query.set_text(&selected);
                }
                self.replace_row.set_visible(mode == Mode::Replace);
                self.rows.set_visible_child_name("find");
                self.bar.set_search_mode(true);
                self.search(&self.query.text());
                self.query.grab_focus();
                self.query.select_region(0, -1);
            }
        }
    }

    /// Next or previous match. Works with the bar closed too, which is what F3 is for.
    pub fn step(self: &Rc<Self>, forward: bool) {
        if self.presenting() {
            return self.to_preview(match forward {
                true => PreviewOp::Next,
                false => PreviewOp::Previous,
            });
        }
        let Some(tab) = self.tab() else { return };
        self.busy.set(true);
        tab.step(forward, false);
        self.busy.set(false);
        self.matches.set_text(&tab.matches_label());
    }

    /// The number of matches in the preview, reported by WebKit after a search.
    pub fn set_matches(&self, count: u32) {
        self.set_matches_text(&match count {
            0 => "No results".to_string(),
            n => format!("{n} matches"),
        });
    }

    /// The same readout, said in the caller's own words. The PDF reader counts its matches
    /// itself and can say "3 of 12", which a plain count cannot.
    pub fn set_matches_text(&self, text: &str) {
        self.matches.set_text(text);
    }

    // --- internals -------------------------------------------------------------------------

    fn tab(&self) -> Option<Rc<Tab>> {
        self.target.borrow().as_ref().map(|(tab, _)| tab.clone())
    }

    fn presenting(&self) -> bool {
        self.wiring.get().is_some_and(|w| (w.presenting)())
    }

    fn to_preview(&self, op: PreviewOp) {
        if let Some(wiring) = self.wiring.get() {
            (wiring.preview)(op);
        }
    }

    fn search(self: &Rc<Self>, text: &str) {
        if self.presenting() {
            return self.to_preview(PreviewOp::Find(text.to_string()));
        }
        let Some(tab) = self.tab() else { return };
        tab.set_query(text);
        tab.set_highlight(true);
        // From the current match, not past it: typing must not walk through the document.
        self.busy.set(true);
        tab.step(true, true);
        self.busy.set(false);
        self.matches.set_text(&tab.matches_label());
    }

    fn replace(self: &Rc<Self>, all: bool) {
        let Some(tab) = self.tab() else { return };
        let with = self.replace.text();
        self.busy.set(true);
        match all {
            true => tab.replace_all(&with),
            false => tab.replace_current(&with),
        }
        self.busy.set(false);
        self.matches.set_text(&tab.matches_label());
    }

    fn refresh_matches(&self) {
        if self.busy.get() {
            return;
        }
        let label = self
            .tab()
            .map(|tab| tab.matches_label())
            .unwrap_or_default();
        self.matches.set_text(&label);
    }

    fn preview_line(&self, text: &str) {
        let Some((line, _)) = goto_target(text) else {
            return;
        };
        match self.presenting() {
            true => self.to_preview(PreviewOp::Line(line.max(1) as u32)),
            false => {
                if let Some(tab) = self.tab() {
                    tab.show_line(line);
                }
            }
        }
    }

    fn jump(&self, text: &str) {
        let Some((line, column)) = goto_target(text) else {
            return;
        };
        match self.presenting() {
            true => self.to_preview(PreviewOp::Line(line.max(1) as u32)),
            false => {
                if let Some(tab) = self.tab() {
                    tab.goto_line(line, column);
                }
            }
        }
    }

    fn close(&self) {
        self.bar.set_search_mode(false);
        if let Some(tab) = self.tab() {
            tab.view.grab_focus();
        }
    }

    /// The bar went away: drop the match highlight on both possible targets.
    fn clear(&self) {
        if let Some(tab) = self.tab() {
            tab.set_highlight(false);
        }
        self.to_preview(PreviewOp::Clear);
        self.matches.set_text("");
    }
}

#[cfg(test)]
mod tests {
    use super::goto_target;

    #[test]
    fn a_goto_entry_reads_line_and_optional_column() {
        assert_eq!(goto_target("42"), Some((42, 1)));
        assert_eq!(goto_target(" 42 "), Some((42, 1)));
        assert_eq!(goto_target("42:7"), Some((42, 7)));
        assert_eq!(
            goto_target("42:"),
            Some((42, 1)),
            "a bare colon is column 1"
        );
        assert_eq!(goto_target(""), None);
        assert_eq!(goto_target("x"), None);
    }
}
