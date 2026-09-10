//! Comparing: the tab hosts a diff beside its own document, and the read-only companion view the
//! other side of one is rendered in.

use super::{Flavour, Tab, build, line_numbers, sync_scheme};
use crate::{diff, highlight};
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
    style_companion(flavour, &buffer);
    (view, buffer)
}

/// The styling a companion's text implies: what [`Tab::analyse`] does for the editor, less the
/// parts that need a tab.
pub fn style_companion(flavour: Flavour, buffer: &sourceview5::Buffer) {
    match flavour {
        Flavour::Note => {
            highlight::apply(buffer);
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
        let compare = diff::Compare::new(old, new, Some(side), hunk_buttons);
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
        });
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
        comparing.compare.leave();
        comparing.holder.remove(&self.document);
        for widget in &comparing.shown {
            self.content.remove(widget);
        }
        self.content.append(&self.document);
        self.set_clamp();
        self.page.set_title(&self.tab_title());
    }
}
