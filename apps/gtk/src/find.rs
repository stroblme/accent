//! A pane's find, replace and go-to-line bar.
//!
//! One bar per pane rather than one per tab or one per window: a pane owns the document it is
//! showing, so a split searches two notes at once, each with its own query, its own mode and its
//! own open state. It is a revealed row between the pane's tab bar and its document, which pushes
//! the document down rather than covering it.
//!
//! Because it outlives any one tab, the bar drives the tab from the outside — `Tab` keeps its
//! query, its matches and the caret moves, this file keeps the widgets — and it re-targets
//! whenever the pane's tab in front changes. Re-targeting leaves the old tab's marks alone: the query and
//! the highlight belong to the tab, so a note opened from a search hit is still marked when the
//! reader comes back to it. While presenting there is no buffer to search, so the same widgets
//! address the rendered preview through [`PreviewOp`] instead; that indirection is also what keeps
//! this file from having to know what an `App` is.

use crate::editor::Tab;
use crate::recall::{self, QUERIES, REPLACEMENTS};
use accent_core::search::Options;
use adw::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

/// Match Case, Match Whole Word and Use Regular Expression, in the order the find bar and the
/// Search pane both show them: text rather than icons, since Adwaita has no glyph for any of the
/// three, and VS Code's `Aa`, `Word` and `.*` are what a user arriving from there already reads.
pub const TOGGLES: [(&str, &str); 3] = [
    ("Aa", "Match Case"),
    ("Word", "Match Whole Word"),
    (".*", "Use Regular Expression"),
];

/// Match Whole Word's tooltip over the rendered preview, where WebKit's find can only hold a match
/// to the start of a word (`AT_WORD_STARTS`), so `foo` finds `food` there.
const WORD_STARTS: &str = "Match Whole Word (word starts in the preview)";

/// A flat text toggle, the look every query toggle has.
pub fn toggle((label, tooltip): (&str, &str)) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::builder()
        .label(label)
        .tooltip_text(tooltip)
        .build();
    button.add_css_class("flat");
    button
}

/// What three toggles made from [`TOGGLES`] say, in that order.
pub fn options([case, word, regex]: [&gtk::ToggleButton; 3]) -> Options {
    Options {
        case: case.is_active(),
        word: word.is_active(),
        regex: regex.is_active(),
    }
}

/// What the bar wants done to the rendered preview while presentation mode is on.
pub enum PreviewOp {
    /// A query, and the toggles: Match Case and Match Whole Word, Regular Expression being a
    /// buffer's alone.
    Find(String, Options),
    Next,
    Previous,
    Clear,
    /// A line or a page to show. `commit` is the Return that ends the entry rather than the live
    /// preview under a half-typed number, which matters to a PDF: only a committed jump is worth
    /// remembering, or Back walks the digits back one at a time.
    Line {
        line: u32,
        commit: bool,
    },
}

/// What the bar needs from the window. Three closures, so `find.rs` never names `App`; all three
/// answer for the bar's own pane, not for whichever pane has the keyboard.
pub struct Wiring {
    /// Whether something other than this pane's editor — the rendered preview, or a PDF — is what
    /// the user is looking at, so find and go-to are addressed there instead.
    pub presenting: Box<dyn Fn() -> bool>,
    pub preview: Box<dyn Fn(PreviewOp)>,
    /// How many pages the thing being looked at has, when it is counted in pages rather than
    /// lines. `None` for an editor, which counts lines.
    pub pages: Box<dyn Fn() -> Option<usize>>,
    /// Record where the reader is in this pane, so Back returns to where the search started.
    pub mark: Box<dyn Fn()>,
}

/// Which row the bar shows, and whether the replace controls come with it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Find,
    Replace,
    Goto,
}

/// A tab the bar is pointed at, with the handler watching its line count, which goes when the bar
/// is pointed elsewhere, or they accumulate one per tab switch.
type Watch = (Rc<Tab>, glib::SignalHandlerId);

pub struct Bar {
    bar: gtk::SearchBar,
    rows: gtk::Stack,
    query: gtk::SearchEntry,
    /// This bar's own [`TOGGLES`]. The bar's rather than the tab's or the Search pane's: each bar
    /// keeps what it was set to.
    toggles: [gtk::ToggleButton; 3],
    replace: gtk::Entry,
    replace_row: gtk::Box,
    matches: gtk::Label,
    line: gtk::Entry,
    lines: gtk::Label,
    /// The tab being searched, and what is watching it.
    target: RefCell<Option<Watch>>,
    /// What the bar last wrote into the query box on the tab's behalf, waiting for the delayed
    /// `search-changed` that follows it. A sidebar jump sets the tab's query and deliberately
    /// leaves it unpainted; running it here as a search of the reader's own would light every
    /// match and step the caret off the place the jump just revealed.
    synced: RefCell<Option<String>>,
    /// Set while the bar itself moves the caret, so the resulting count notification does not
    /// walk the label back to the value it had before the jump.
    busy: Cell<bool>,
    /// Whether this run of the bar has already recorded where it started. The first search, step
    /// or committed go-to marks the place and the rest of the walk does not, so Back returns to
    /// where the search began rather than to the previous hit. Reset by opening and by closing,
    /// the second because `F3` works with the bar shut.
    marked: Cell<bool>,
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
        // The Search pane's toggles, in the same `.linked` group.
        let toggles = TOGGLES.map(toggle);
        let toggle_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        toggle_group.add_css_class("linked");
        for button in &toggles {
            toggle_group.append(button);
        }
        let matches = gtk::Label::builder().css_classes(["dim-label"]).build();
        // Up and down, not back and forward: the matches are places in a document that scrolls
        // vertically, which is the axis every other find bar names here (DESIGN.md, Iconography).
        let previous = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Find Previous")
            .build();
        let next = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .tooltip_text("Find Next")
            .build();

        let replace = gtk::Entry::builder()
            .placeholder_text("Replace")
            .hexpand(true)
            .build();
        recall::attach(&query, &QUERIES);
        recall::attach(&replace, &REPLACEMENTS);
        let replace_one = gtk::Button::with_label("Replace");
        let replace_all = gtk::Button::with_label("Replace All");

        // 6 px inside a control group, 12 px between the two rows (DESIGN.md, Spacing).
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        top.append(&query);
        top.append(&toggle_group);
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
        // On the note's own background, as the header above it is: Adwaita paints a search bar in
        // the header bar colour, which is the note's in light but a lighter band in dark.
        bar.add_css_class("accent-flat");

        let this = Rc::new(Bar {
            bar,
            rows,
            query: query.clone(),
            toggles: toggles.clone(),
            replace: replace.clone(),
            replace_row,
            matches,
            line: line.clone(),
            lines,
            target: RefCell::new(None),
            synced: RefCell::new(None),
            busy: Cell::new(false),
            marked: Cell::new(false),
            wiring: OnceCell::new(),
        });

        query.connect_search_changed(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |entry| {
                let text = entry.text();
                // Put there by the tab rather than typed into: see [`Bar::synced`].
                let mine = {
                    let mut synced = bar.synced.borrow_mut();
                    match synced.as_deref() {
                        Some(written) if written == text.as_str() => {
                            *synced = None;
                            true
                        }
                        Some(_) if text.is_empty() => true,
                        _ => {
                            *synced = None;
                            false
                        }
                    }
                };
                if !mine {
                    bar.search(&text);
                }
            }
        ));
        query.connect_activate(glib::clone!(
            #[weak(rename_to = bar)]
            this,
            move |_| bar.step(true)
        ));
        // A toggle changes what the query means, so the query is asked again.
        for button in &toggles {
            button.connect_toggled(glib::clone!(
                #[weak(rename_to = bar)]
                this,
                move |_| bar.search(&bar.query.text())
            ));
        }
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
        // because `GtkSearchEntry` binds Escape to `stop-search` and would eat it first — so
        // Escape always closes the bar rather than first clearing the query, which is what the
        // key is for here. The window carries the other half, for an Escape pressed with the
        // focus back in the document (`main.rs`).
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
                    // A query the bar is closed on was used, as one stepped through is.
                    if bar.rows.visible_child_name().as_deref() == Some("find") {
                        recall::remember(&QUERIES, &bar.query.text());
                    }
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

    /// Point the bar at the tab that just came to the front of its pane.
    ///
    /// The old tab keeps its query and its marks: they are the tab's, not the bar's, so a note
    /// opened from a search hit is still marked after a switch away and back. Only the handler
    /// watching its length goes, or they accumulate one per tab switch.
    pub fn retarget(self: &Rc<Self>, tab: Option<Rc<Tab>>) {
        if let Some((old, lines)) = self.target.borrow_mut().take() {
            old.buffer.disconnect(lines);
        }
        self.sync_toggles();
        let Some(tab) = tab else {
            return self.refresh_count();
        };
        let lines = tab.buffer.connect_changed(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |_| bar.refresh_count()
        ));
        // Set anew on every retarget rather than taken back on the way out: a tab dragged into
        // another pane is that pane's bar's from then on, whichever bar lets go of it last.
        tab.connect_found(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            #[weak]
            tab,
            move |query| bar.found(&tab, query)
        ));
        if self.showing("find") {
            tab.set_find_options(self.options());
            tab.set_query(&self.query.text());
            tab.set_highlight(true);
        }
        *self.target.borrow_mut() = Some((tab, lines));
        self.refresh_matches();
        self.refresh_count();
    }

    /// `tab` matched its query again, and the readout follows. The query is the tab's, so it can
    /// change without the bar being touched — a sidebar jump hands the tab the text it landed on
    /// — and where it did (`query`) the box follows it too, or the bar would say one thing while
    /// `F3` stepped through another. Only then: text typed into the box waits out its
    /// `search-changed` delay before the tab has it, and an edit or a scroll matched meanwhile
    /// would put the old query back over it. Nothing is written when the box already says it,
    /// which is what keeps the bar's own searches from coming back round to it. A tab this bar is
    /// no longer pointed at is none of its business.
    fn found(&self, tab: &Rc<Tab>, query: bool) {
        if !self.tab().is_some_and(|mine| Rc::ptr_eq(&mine, tab)) {
            return;
        }
        let text = tab.find_query();
        if query && self.query.text() != text {
            *self.synced.borrow_mut() = Some(text.clone());
            self.query.set_text(&text);
        }
        self.refresh_matches();
    }

    /// What this bar's toggles say.
    fn options(&self) -> Options {
        let [case, word, regex] = &self.toggles;
        options([case, word, regex])
    }

    /// Over the rendered preview and a PDF, Match Case and Match Whole Word follow as WebKit's
    /// find and pdfium's do — whole words being word starts in the preview, which the tooltip
    /// says — and Regular Expression is shown off-limits rather than quietly ignored, neither
    /// having one, keeping what it was set to for the next note.
    fn sync_toggles(&self) {
        let presenting = self.presenting();
        let [_, word, regex] = &self.toggles;
        regex.set_sensitive(!presenting);
        // A PDF is counted in pages; the preview, like a buffer, is not.
        let preview = presenting && self.wiring.get().is_some_and(|w| (w.pages)().is_none());
        word.set_tooltip_text(Some(match preview {
            true => WORD_STARTS,
            false => TOGGLES[1].1,
        }));
        if presenting {
            self.query.remove_css_class("error");
        }
    }

    /// Reveal the bar in `mode`, prefilled from the selection when there is one worth searching.
    /// Replace over such a selection starts in the replacement box, the query being given already.
    pub fn open(self: &Rc<Self>, mode: Mode) {
        self.marked.set(false);
        match mode {
            Mode::Goto => {
                self.rows.set_visible_child_name("goto");
                self.bar.set_search_mode(true);
                self.refresh_count();
                self.line.grab_focus();
                self.line.select_region(0, -1);
            }
            _ => {
                let selected = self.tab().and_then(|tab| tab.selected_query());
                if let Some(selected) = &selected {
                    self.query.set_text(selected);
                }
                // Nothing behind a presented preview or PDF can be rewritten, so the row is
                // not offered there: it used to appear and quietly do nothing.
                let replacing = mode == Mode::Replace && !self.presenting();
                self.replace_row.set_visible(replacing);
                self.sync_toggles();
                self.rows.set_visible_child_name("find");
                self.bar.set_search_mode(true);
                self.search(&self.query.text());
                let entry: &gtk::Editable = match replacing && selected.is_some() {
                    true => self.replace.upcast_ref(),
                    false => self.query.upcast_ref(),
                };
                entry.grab_focus();
                entry.select_region(0, -1);
            }
        }
    }

    /// Where the search started, recorded once per run of the bar, which is why every mover
    /// calls it and only the first of them does anything.
    fn mark_once(&self) {
        if self.marked.replace(true) {
            return;
        }
        if let Some(wiring) = self.wiring.get() {
            (wiring.mark)();
        }
    }

    /// Next or previous match. Works with the bar closed too, which is what F3 is for.
    pub fn step(self: &Rc<Self>, forward: bool) {
        self.mark_once();
        recall::remember(&QUERIES, &self.query.text());
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
        self.show_matches(&tab);
    }

    /// The readout, said in the caller's own words: the PDF reader and the preview both count
    /// their own matches and say "3 of 12", which the bar has no way to work out for them.
    pub fn set_matches_text(&self, text: &str) {
        self.matches.set_text(text);
    }

    /// Put the bar away and give the document the keyboard back. Public because Escape reaches it
    /// from outside the bar too.
    pub fn close(&self) {
        self.marked.set(false);
        self.bar.set_search_mode(false);
        // The editor is behind the preview while one is presented, and focusing a hidden view is
        // giving the keyboard to nothing the reader can see.
        if let (false, Some(tab)) = (self.presenting(), self.tab()) {
            tab.view.grab_focus();
        }
    }

    // --- internals -------------------------------------------------------------------------

    fn tab(&self) -> Option<Rc<Tab>> {
        self.target.borrow().as_ref().map(|(tab, ..)| tab.clone())
    }

    fn presenting(&self) -> bool {
        self.wiring.get().is_some_and(|w| (w.presenting)())
    }

    /// Whether the bar is up on the `"find"` or the `"goto"` row.
    fn showing(&self, row: &str) -> bool {
        self.is_open() && self.rows.visible_child_name().as_deref() == Some(row)
    }

    fn to_preview(&self, op: PreviewOp) {
        if let Some(wiring) = self.wiring.get() {
            (wiring.preview)(op);
        }
    }

    fn search(self: &Rc<Self>, text: &str) {
        self.mark_once();
        self.sync_toggles();
        if self.presenting() {
            return self.to_preview(PreviewOp::Find(text.to_string(), self.options()));
        }
        let Some(tab) = self.tab() else { return };
        // What the two boxes have selected, to put back below. The query box says `search-changed`
        // after a delay, so this runs a moment *after* Ctrl+F or Ctrl+H over an open bar has
        // selected the whole of one of them — and the step below moves the document's caret, which
        // takes that selection with it. The select-all used to flash and go, leaving the reader
        // appending to the old query rather than typing over it.
        let boxes: [&gtk::Editable; 2] = [self.query.upcast_ref(), self.replace.upcast_ref()];
        let selected = boxes.map(|entry| entry.selection_bounds());
        self.busy.set(true);
        tab.set_find_options(self.options());
        tab.set_query(text);
        tab.set_highlight(true);
        // From the current match, not past it: typing must not walk through the document.
        tab.step(true, true);
        self.busy.set(false);
        for (entry, bounds) in boxes.into_iter().zip(selected) {
            if let Some((start, end)) = bounds {
                entry.select_region(start, end);
            }
        }
        self.show_matches(&tab);
    }

    fn replace(self: &Rc<Self>, all: bool) {
        // A rendered preview and a PDF are read-only: Ctrl+H reaches neither, and the row that
        // asks for one is hidden while either is presented.
        if self.presenting() {
            return;
        }
        let Some(tab) = self.tab() else { return };
        let with = self.replace.text();
        recall::remember(&QUERIES, &self.query.text());
        recall::remember(&REPLACEMENTS, &with);
        self.busy.set(true);
        match all {
            true => tab.replace_all(&with),
            false => tab.replace_current(&with),
        }
        self.busy.set(false);
        self.show_matches(&tab);
    }

    fn refresh_matches(&self) {
        if self.busy.get() {
            return;
        }
        match self.tab() {
            Some(tab) => self.show_matches(&tab),
            None => self.matches.set_text(""),
        }
    }

    /// The readout for `tab`, and the query box marked as the Search pane marks its own when the
    /// query does not compile. Not while presenting, where the query is searched as written.
    fn show_matches(&self, tab: &Tab) {
        self.matches.set_text(&tab.matches_label());
        match tab.find_invalid() && !self.presenting() {
            true => self.query.add_css_class("error"),
            false => self.query.remove_css_class("error"),
        }
    }

    /// The go-to row's "of N lines", read again whenever it can have changed while the row is up:
    /// on opening it, on every edit, when another tab comes to the front, and when a PDF finishes
    /// opening.
    pub fn refresh_count(&self) {
        if !self.showing("goto") {
            return;
        }
        // A PDF is counted in pages and everything else in lines, and the row says so in both
        // places: a "Line[:column]" prompt over a page count is nonsense.
        let (label, placeholder) = match self.wiring.get().and_then(|w| (w.pages)()) {
            Some(pages) => (format!("of {pages} pages"), "Page"),
            None => {
                let count = self.tab().map(|tab| tab.line_count()).unwrap_or(0);
                (format!("of {count} lines"), "Line[:column]")
            }
        };
        self.lines.set_text(&label);
        self.line.set_placeholder_text(Some(placeholder));
    }

    fn preview_line(&self, text: &str) {
        let Some((line, _)) = goto_target(text) else {
            // The box emptied, or says something that is not a line: nothing is being pointed at,
            // so nothing stays painted.
            if let Some(tab) = self.tab() {
                tab.clear_reveal();
            }
            return;
        };
        match self.presenting() {
            true => self.to_preview(PreviewOp::Line {
                line: line.max(1) as u32,
                commit: false,
            }),
            false => {
                if let Some(tab) = self.tab() {
                    tab.show_line(line);
                    // The same paint a search hit leaves, and the same one this line's Return
                    // will leave: scrolling a line into view says nothing about which of the ones
                    // on screen it is. `show_line` has opened whatever folded it first.
                    tab.reveal_line(line);
                }
            }
        }
    }

    fn jump(&self, text: &str) {
        let Some((line, column)) = goto_target(text) else {
            return;
        };
        self.mark_once();
        match self.presenting() {
            true => self.to_preview(PreviewOp::Line {
                line: line.max(1) as u32,
                commit: true,
            }),
            false => {
                if let Some(tab) = self.tab() {
                    tab.goto_line(line, column);
                    // The whole line, because the line is the place that was asked for: a column
                    // narrows where the caret lands, not what the reader was looking for.
                    tab.reveal_line(line);
                }
            }
        }
    }

    /// What the query box says. Only `ACCENT_BENCH_REVEAL` reads it.
    #[cfg(feature = "bench")]
    pub fn query_text(&self) -> String {
        self.query.text().to_string()
    }

    /// The query box itself, to type into and to read what is selected in. Only
    /// `ACCENT_BENCH_FIND` touches it.
    #[cfg(feature = "bench")]
    pub fn query_box(&self) -> &gtk::SearchEntry {
        &self.query
    }

    /// The three toggles, for `ACCENT_BENCH_FIND=options` to set.
    #[cfg(feature = "bench")]
    pub fn toggles(&self) -> &[gtk::ToggleButton; 3] {
        &self.toggles
    }

    /// The readout and whether the query box is marked invalid, for the same drill.
    #[cfg(feature = "bench")]
    pub fn readout(&self) -> (String, bool) {
        (
            self.matches.text().to_string(),
            self.query.has_css_class("error"),
        )
    }

    /// Replace All, as its button presses it, for `ACCENT_BENCH_FIND=options`.
    #[cfg(feature = "bench")]
    pub fn press_replace_all(self: &Rc<Self>) {
        self.replace(true);
    }

    /// The replacement box, for the same reason as [`Bar::query_box`].
    #[cfg(feature = "bench")]
    pub fn replace_box(&self) -> &gtk::Entry {
        &self.replace
    }

    /// The go-to box, so a drill can type a line number into it. Only `ACCENT_BENCH_REVEAL`
    /// touches it.
    #[cfg(feature = "bench")]
    pub fn line_box(&self) -> &gtk::Entry {
        &self.line
    }

    /// The bar went away: drop the match highlight on both possible targets.
    fn clear(&self) {
        if let Some(tab) = self.tab() {
            tab.set_highlight(false);
        }
        self.to_preview(PreviewOp::Clear);
        self.matches.set_text("");
        self.query.remove_css_class("error");
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
