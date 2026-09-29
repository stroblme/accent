//! Comparing: the tab hosts a diff beside its own document, and the read-only companion view the
//! other side of one is rendered in.

use super::{Alert, Flavour, Tab, build, line_numbers, sync_scheme};
use crate::{diff, fold, highlight, wrap};
use adw::prelude::*;
use gtk::glib;
use sourceview5::prelude::*;
use std::rc::Rc;

/// A comparison the tab is hosting: see [`Tab::compare`].
pub(super) struct Comparing {
    compare: Rc<diff::Compare>,
    /// What the content shows in the document's place: the paned, and what the host put under it.
    shown: Vec<gtk::Widget>,
    /// The box the document sits in, on its side of the paned.
    holder: gtk::Box,
    pub(super) label: String,
    /// The banner question this comparison is the answer to, see [`Tab::comparing_answers`].
    pub(super) answers: Option<Alert>,
    /// The folds the comparison opened, as a mark on each header line and the header's text:
    /// see [`Tab::open_folds`].
    shut: Vec<(gtk::TextMark, String)>,
}

/// A read-only view over `text` for a comparison, built the way the editor builds its own so the
/// two sides of a diff render one note alike, and named `name` so the font and zoom CSS written
/// for the editor applies here too.
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

/// An editable note view over `text` that is not a tab: a diagram's label, edited as Markdown
/// with the note's own styling (`diagram/label.rs`).
pub fn overlay_view(text: &str) -> (sourceview5::View, sourceview5::Buffer) {
    let (view, buffer) = build(Flavour::Note, None, text);
    style_companion(Flavour::Note, &buffer, &view);
    (view, buffer)
}

/// The styling a companion's text implies: what [`Tab::analyse`] does for the editor, less the
/// parts that need a tab.
pub fn style_companion(flavour: Flavour, buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    match flavour {
        Flavour::Note => {
            wrap::refence(view, || highlight::apply(buffer));
        }
        Flavour::Csv => highlight::apply_csv(buffer),
        Flavour::Code => {}
    }
}

/// A companion's colours after the theme moved: what [`Tab::restyle`] does, less the gutters a
/// tab has.
pub fn restyle_companion(flavour: Flavour, buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    sync_scheme(buffer);
    match flavour {
        Flavour::Note => {
            highlight::restyle(buffer, view);
            highlight::hang(buffer, view);
        }
        Flavour::Csv => highlight::restyle_csv(buffer),
        Flavour::Code => {}
    }
    wrap::measure(view);
}

impl Tab {
    /// Show `other` (title, text) beside this tab's editor with the diff laid over both. The
    /// editor is the `side` column and is never rewritten: what the user types is the merge.
    /// `hunk_buttons` puts Take / Keep Both on the other pane, `below` goes under the panes for
    /// the host's own buttons, and `label` says in the tab title what is being compared.
    pub fn compare(
        self: &Rc<Self>,
        mine: &str,
        other: (&str, &str),
        side: diff::Side,
        hunk_buttons: bool,
        below: Option<gtk::Widget>,
        label: &str,
    ) -> Rc<diff::Compare> {
        self.leave_compare();
        let shut = self.open_folds();
        // A comparison is a merge of its own, with its own tints and buttons over these lines.
        self.conflicts.set_enabled(false);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some("Stop Comparing"));
        close.connect_clicked(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.leave_compare()
        ));
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = diff::header(mine, Some(close.upcast_ref()));
        holder.append(&header);
        holder.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        self.content.remove(&self.document);
        holder.append(&self.document);
        let editor = diff::Pane {
            root: holder.clone().upcast(),
            header: header.upcast(),
            view: self.view.clone(),
            buffer: self.buffer.clone(),
            scroller: self.scroller.clone(),
            flavour: self.flavour,
            pool: self.overlays.clone(),
        };
        let companion = diff::pane(
            other.0,
            self.flavour,
            other.1,
            &self.view.widget_name(),
            self.buffer.language().as_ref(),
        );
        let (old, new) = match side {
            diff::Side::Old => (editor, companion),
            diff::Side::New => (companion, editor),
        };
        let compare =
            self.with_map_unset(|| diff::Compare::new(old, new, Some(side), hunk_buttons));
        // Every lay moves the hidden runs, and a message at the end of a collapsed line would be
        // drawn on the row that stands for the run: the diagnostics are laid again with them.
        compare.on_laid(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.paint_diagnostics()
        ));
        compare.widget().set_vexpand(true);
        self.content.append(compare.widget());
        let mut shown = vec![compare.widget().clone()];
        if let Some(below) = below {
            let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
            self.content.append(&separator);
            self.content.append(&below);
            shown.extend([separator.upcast(), below]);
        }
        *self.comparing.borrow_mut() = Some(Comparing {
            compare: compare.clone(),
            shown,
            holder,
            label: label.to_string(),
            answers: None,
            shut,
        });
        self.show_chevrons();
        self.set_clamp();
        self.page.set_title(&self.tab_title());
        compare
    }

    pub fn comparison(&self) -> Option<Rc<diff::Compare>> {
        self.comparing.borrow().as_ref().map(|c| c.compare.clone())
    }

    /// The editor alone again. A no-op when nothing is being compared.
    pub fn leave_compare(&self) {
        let Some(comparing) = self.comparing.borrow_mut().take() else {
            return;
        };
        self.with_map_unset(|| comparing.compare.leave());
        self.shut_again(comparing.shut);
        self.show_chevrons();
        // The gap tags went with it, so the messages the collapsed lines were keeping quiet about
        // belong back at the ends of their lines.
        self.paint_diagnostics();
        self.conflicts.set_enabled(true);
        comparing.holder.remove(&self.document);
        for widget in &comparing.shown {
            self.content.remove(widget);
        }
        self.content.append(&self.document);
        self.set_clamp();
        self.page.set_title(&self.tab_title());
        // The banner's button comes back, if the comparison had taken it.
        self.render_banner();
    }

    /// Open every shut fold for a comparison, which hides lines of its own: see [`Tab::shut`].
    /// Each header is kept as a mark, which follows it through the comparison's edits, and as its
    /// text, which says whether it is still the same header when the comparison goes.
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

    /// Shut again what [`Tab::open_folds`] opened, where the header is still there and still opens
    /// a block, to where the block ends now. One the caret is in stays open, as a collapsed run
    /// does.
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

    /// Run `swap`, which may hand the editor's view another vertical adjustment, with the minimap
    /// let go of the view around it. GtkSourceMap follows the adjustment the view had when it was
    /// set, and lets go of whichever the view has when it is unset: across the swap it stood still
    /// while the comparison scrolled, and a tab closed mid-comparison logged `instance … has no
    /// handler with id` for both of its handlers.
    fn with_map_unset<T>(&self, swap: impl FnOnce() -> T) -> T {
        self.map.set_property("view", None::<&sourceview5::View>);
        let out = swap();
        self.map.set_view(&self.view);
        out
    }
}
