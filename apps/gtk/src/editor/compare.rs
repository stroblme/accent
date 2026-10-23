//! Tab-hosted comparisons and merges, with read-only companion columns.

use super::{Alert, Flavour, Tab, build, line_numbers, refont, sync_scheme};
use crate::{diff, fold, highlight, wrap};
use adw::prelude::*;
use gtk::glib;
use sourceview5::prelude::*;
use std::rc::Rc;

/// A comparison ([`Tab::compare`]) or merge ([`Tab::merge`]) hosted around
/// the editor as one column.
#[derive(Clone)]
pub enum Hosted {
    Compare(Rc<diff::Compare>),
    Merge(Rc<diff::Merge>),
}

impl Hosted {
    pub fn widget(&self) -> &gtk::Widget {
        match self {
            Hosted::Compare(c) => c.widget(),
            Hosted::Merge(m) => m.widget(),
        }
    }

    /// Lay the columns over the texts again: on every edit of the editor's.
    pub fn refresh(&self) {
        match self {
            Hosted::Compare(c) => c.refresh(),
            Hosted::Merge(m) => m.refresh(),
        }
    }

    pub fn restyle(&self) {
        match self {
            Hosted::Compare(c) => c.restyle(),
            Hosted::Merge(m) => m.restyle(),
        }
    }

    pub fn follow_editor(&self, refont: bool) {
        match self {
            Hosted::Compare(c) => c.follow_editor(refont),
            Hosted::Merge(m) => m.follow_editor(refont),
        }
    }

    /// Open the run hiding `offset` in the editor's column: `true` when one was.
    pub fn open_hiding(&self, offset: i32) -> bool {
        match self {
            Hosted::Compare(c) => c.open_hiding(offset),
            Hosted::Merge(m) => m.open_hiding(offset),
        }
    }

    fn on_laid(&self, f: impl Fn() + 'static) {
        match self {
            Hosted::Compare(c) => c.on_laid(f),
            Hosted::Merge(m) => m.on_laid(f),
        }
    }

    fn leave(&self) {
        match self {
            Hosted::Compare(c) => c.leave(),
            Hosted::Merge(m) => m.leave(),
        }
    }
}

/// What the tab is hosting, and how to put the document back: see [`Tab::host`].
pub(super) struct Comparing {
    hosted: Hosted,
    /// Paned comparison content and the host's controls below it.
    shown: Vec<gtk::Widget>,
    /// The box the document sits in, on its side of the paned.
    holder: gtk::Box,
    pub(super) label: String,
    /// The banner question this comparison is the answer to, see [`Tab::comparing_answers`].
    pub(super) answers: Option<Alert>,
    /// Opened fold headers as text and edit-tracking marks ([`Tab::open_folds`]).
    shut: Vec<(gtk::TextMark, String)>,
}

/// Build a read-only companion matching the editor's rendering. Use `name`
/// to share its font and zoom CSS, keeping both diff columns consistent.
pub fn companion(
    flavour: Flavour,
    text: &str,
    name: &str,
    language: Option<&sourceview5::Language>,
) -> (sourceview5::View, sourceview5::Buffer) {
    let (view, buffer) = build(flavour, language.cloned(), text);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_widget_name(name);
    refont(&view);
    // A comparison is about lines, so the numbers are always on here.
    line_numbers(&view, &buffer).set_visible(true);
    style_companion(flavour, &buffer, &view);
    (view, buffer)
}

/// The text of the line `at` starts, without its newline.
fn line_text(at: &gtk::TextIter) -> String {
    let mut end = *at;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    at.text(&end).to_string()
}

/// Build an editable Markdown view for diagram labels without a tab
/// (`diagram/label.rs`).
pub fn overlay_view(text: &str) -> (sourceview5::View, sourceview5::Buffer) {
    let (view, buffer) = build(Flavour::Note, None, text);
    style_companion(Flavour::Note, &buffer, &view);
    (view, buffer)
}

/// Apply companion styling like [`Tab::analyse`], without tab-specific work.
pub fn style_companion(flavour: Flavour, buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    match flavour {
        Flavour::Note => {
            wrap::refence(view, || highlight::apply(buffer));
        }
        Flavour::Csv(delim) => highlight::apply_csv(buffer, delim),
        Flavour::Code => {}
    }
}

/// Update companion colours after theme changes, without tab gutters
/// ([`Tab::restyle`]).
pub fn restyle_companion(flavour: Flavour, buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    sync_scheme(buffer);
    match flavour {
        Flavour::Note => highlight::restyle(buffer, view),
        Flavour::Csv(_) => highlight::restyle_csv(buffer),
        Flavour::Code => {}
    }
    rehang_companion(flavour, buffer, view);
}

/// Remeasure companion markers and wrap indent in its own font and margin
/// ([`Tab::rehang`]).
pub fn rehang_companion(flavour: Flavour, buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    if flavour.is_note() {
        highlight::hang(buffer, view);
    }
    wrap::measure(view);
}

impl Tab {
    /// Show `other` (title, text) beside the editor at `side`. Preserve editor
    /// text so user edits form the merge. `hunk_buttons` adds Take/Keep Both
    /// to the peer; `below` holds host controls and `label` names the comparison.
    pub fn compare(
        self: &Rc<Self>,
        mine: &str,
        other: (&str, &str),
        side: diff::Side,
        hunk_buttons: bool,
        below: Option<gtk::Widget>,
        label: &str,
    ) -> Rc<diff::Compare> {
        let hosted = self.host(mine, "Stop Comparing", below, label, |tab, editor| {
            // A comparison is a merge of its own, with its own tints and buttons over these lines.
            tab.conflicts.set_enabled(false);
            let companion = diff::pane(
                other.0,
                tab.flavour,
                other.1,
                &tab.view.widget_name(),
                tab.buffer.language().as_ref(),
            );
            let (old, new) = match side {
                diff::Side::Old => (editor, companion),
                diff::Side::New => (companion, editor),
            };
            Hosted::Compare(diff::Compare::new(old, new, Some(side), hunk_buttons))
        });
        match hosted {
            Hosted::Compare(compare) => compare,
            Hosted::Merge(_) => unreachable!("made a comparison"),
        }
    }

    /// Show git `stages` (base, current, incoming) around the editor, with
    /// current on the left and incoming on the right.
    pub fn merge(self: &Rc<Self>, stages: [String; 3]) -> Rc<diff::Merge> {
        // The sides as the markers name them, `HEAD` and the branch, which a rebase swaps.
        let text = self.text();
        let labels = accent_core::conflict::blocks(&text).first().map(|block| {
            let markers = block.markers();
            let label = |i: usize| accent_core::conflict::label(&text, markers[i].clone());
            (label(0).to_string(), label(markers.len() - 1).to_string())
        });
        let title = |what: &str, label: Option<&String>| match label.filter(|l| !l.is_empty()) {
            Some(label) => format!("{what} ({label})"),
            None => what.to_string(),
        };
        let titles = [
            "Base".to_string(),
            title("Current", labels.as_ref().map(|l| &l.0)),
            title("Incoming", labels.as_ref().map(|l| &l.1)),
        ];
        let name = crate::doc::file_name(&self.rel()).to_string();
        let hosted = self.host(&name, "Stop Merging", None, "Merge", |tab, editor| {
            // The blocks keep their tints; their buttons go beside them, in every column.
            tab.conflicts.set_band(false);
            let side = |stage: usize| {
                diff::pane(
                    &titles[stage],
                    tab.flavour,
                    &stages[stage],
                    &tab.view.widget_name(),
                    tab.buffer.language().as_ref(),
                )
            };
            let (left, right) = (side(diff::merge::CURRENT), side(diff::merge::INCOMING));
            let header = editor.header.clone();
            let merge =
                diff::Merge::new([left, editor, right], stages, titles, tab.conflicts.clone());
            // "(Result)" after the name, which gives way to it in a narrow column.
            if let Some(name) = header.first_child() {
                let result = gtk::Label::builder()
                    .label("(Result)")
                    .xalign(0.0)
                    .hexpand(true)
                    .css_classes(["heading", "dim-label"])
                    .build();
                name.set_hexpand(false);
                header.insert_child_after(&result, Some(&name));
            }
            Hosted::Merge(merge)
        });
        match hosted {
            Hosted::Merge(merge) => merge,
            Hosted::Compare(_) => unreachable!("made a merge"),
        }
    }

    /// Wrap the editor with `make`, a `mine` title and leave button (`stop`
    /// tooltip). Place `below` under columns and `label` in the tab title.
    fn host(
        self: &Rc<Self>,
        mine: &str,
        stop: &str,
        below: Option<gtk::Widget>,
        label: &str,
        make: impl FnOnce(&Rc<Self>, diff::Pane) -> Hosted,
    ) -> Hosted {
        self.leave_compare();
        let shut = self.open_folds();
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some(stop));
        close.connect_clicked(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.leave_compare()
        ));
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = diff::header(mine, Some(close.upcast_ref()));
        holder.append(&header);
        holder.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        // GTK 4.22 gives the overlay scrollbar a fade handler on the scroller's adjustment at
        // every realize and takes it off only while realized, from the adjustment it is on then.
        // Out of the window, the scroller may be put on the other column's adjustment: its own
        // would keep the handler, to run it on a scrollbar GTK has let go of once the comparison
        // hands that adjustment back. Overlay scrolling off takes the handler now, while it can.
        let overlay = self.scroller.is_overlay_scrolling();
        self.scroller.set_overlay_scrolling(false);
        self.content.remove(&self.document);
        holder.append(&self.document);
        let editor = diff::Pane {
            root: holder.clone().upcast(),
            header,
            view: self.view.clone(),
            buffer: self.buffer.clone(),
            scroller: self.scroller.clone(),
            flavour: self.flavour,
            pool: self.overlays.clone(),
        };
        let hosted = make(self, editor);
        self.scroller.set_overlay_scrolling(overlay);
        // Every lay moves the hidden runs, and a message at the end of a collapsed line would be
        // drawn on the row that stands for the run: the diagnostics are laid again with them.
        hosted.on_laid(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.paint_diagnostics()
        ));
        hosted.widget().set_vexpand(true);
        self.content.append(hosted.widget());
        let mut shown = vec![hosted.widget().clone()];
        if let Some(below) = below {
            let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
            self.content.append(&separator);
            self.content.append(&below);
            shown.extend([separator.upcast(), below]);
        }
        *self.comparing.borrow_mut() = Some(Comparing {
            hosted: hosted.clone(),
            shown,
            holder,
            label: label.to_string(),
            answers: None,
            shut,
        });
        self.show_chevrons();
        self.update_sticky();
        self.set_clamp();
        self.page.set_title(&self.tab_title());
        hosted
    }

    /// The comparison the tab hosts, if it hosts one.
    pub fn comparison(&self) -> Option<Rc<diff::Compare>> {
        match self.hosted()? {
            Hosted::Compare(compare) => Some(compare),
            Hosted::Merge(_) => None,
        }
    }

    /// The merge the tab hosts, if it hosts one.
    pub fn merging(&self) -> Option<Rc<diff::Merge>> {
        match self.hosted()? {
            Hosted::Merge(merge) => Some(merge),
            Hosted::Compare(_) => None,
        }
    }

    /// The comparison or the merge the tab hosts.
    pub fn hosted(&self) -> Option<Hosted> {
        self.comparing.borrow().as_ref().map(|c| c.hosted.clone())
    }

    /// The editor alone again, from a comparison or a merge. A no-op when neither is up.
    pub fn leave_compare(&self) {
        let Some(comparing) = self.comparing.borrow_mut().take() else {
            return;
        };
        comparing.hosted.leave();
        self.shut_again(comparing.shut);
        self.show_chevrons();
        // The gap tags went with it, so the messages the collapsed lines were keeping quiet about
        // belong back at the ends of their lines.
        self.paint_diagnostics();
        match comparing.hosted {
            Hosted::Compare(_) => self.conflicts.set_enabled(true),
            Hosted::Merge(_) => self.conflicts.set_band(true),
        }
        comparing.holder.remove(&self.document);
        for widget in &comparing.shown {
            self.content.remove(widget);
        }
        self.content.append(&self.document);
        self.update_sticky();
        self.set_clamp();
        self.page.set_title(&self.tab_title());
        // The banner's button comes back, if the comparison had taken it.
        self.render_banner();
    }

    /// Open folds for comparison, retaining each header's text and a mark
    /// tracking edits so folds can be restored afterwards ([`Tab::shut`]).
    fn open_folds(&self) -> Vec<(gtk::TextMark, String)> {
        let buffer = self.text_buffer();
        let shut = self
            .folds
            .borrow()
            .iter()
            .filter(|f| fold::is_folded(buffer, f.start_line as i32))
            .filter_map(|f| buffer.iter_at_line(f.start_line as i32))
            .map(|at| (buffer.create_mark(None, &at, true), line_text(&at)))
            .collect();
        fold::unfold_all(buffer);
        shut
    }

    /// Restore folds opened by [`Tab::open_folds`] if their headers still
    /// exist and define blocks, using current ends. Keep caret-containing
    /// blocks open, as with collapsed runs.
    fn shut_again(&self, shut: Vec<(gtk::TextMark, String)>) {
        let buffer = self.text_buffer();
        let caret = self.caret_line() as u32;
        for (mark, text) in shut {
            let at = buffer.iter_at_mark(&mark);
            buffer.delete_mark(&mark);
            let line = at.line() as u32;
            let found = self
                .folds
                .borrow()
                .iter()
                .find(|f| f.start_line == line && !(line < caret && caret <= f.end_line))
                .copied();
            if let Some(f) = found.filter(|_| at.starts_line() && line_text(&at) == text) {
                self.shut(f);
            }
        }
    }
}
