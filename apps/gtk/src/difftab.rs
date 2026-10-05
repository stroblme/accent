//! A comparison of two texts that are not files, as a tab of its own: a staged change, a commit
//! against its parent. The comparison is `diff.rs`'s; this is the tab around it.

use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::diff::{Compare, Side, pane};
use crate::editor::{self, Flavour};

/// A comparison of two texts that are not files, as a tab of its own: a staged change, a commit
/// against its parent. Both panes are companions, so the tab carries the font provider the
/// editor would otherwise have.
pub struct DiffTab {
    pub page: adw::TabPage,
    key: String,
    compare: Rc<Compare>,
    /// Both panes' views. They take the page's margins from the zoom here, having no editor
    /// beside them for the comparison to copy them from.
    views: [sourceview5::View; 2],
    flavour: Flavour,
    name: String,
    font: RefCell<Option<gtk::CssProvider>>,
}

impl DiffTab {
    /// `old` and `new` are (title, text). `key` is what the tab is keyed by, see
    /// `App::open_diff`; the file name behind it picks the language and the flavour.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        tabs: &adw::TabView,
        key: &str,
        file: &str,
        title: &str,
        flavour: Flavour,
        old: (&str, &str),
        new: (&str, &str),
        font: Option<&str>,
        zoom: f64,
    ) -> Rc<DiffTab> {
        let name = editor::next_view_name();
        let language = match flavour {
            Flavour::Code => editor::language_for(file, new.1),
            _ => None,
        };
        let old = pane(old.0, flavour, old.1, &name, language.as_ref());
        let new = pane(new.0, flavour, new.1, &name, language.as_ref());
        let views = [old.view.clone(), new.view.clone()];
        let compare = Compare::new(old, new, None, false);
        let page = tabs.append(compare.widget());
        page.set_title(title);
        page.set_icon(Some(&gtk::gio::ThemedIcon::new("view-dual-symbolic")));
        let tab = Rc::new(DiffTab {
            page,
            key: key.to_string(),
            compare,
            views,
            flavour,
            name,
            font: RefCell::new(None),
        });
        tab.set_font(font, zoom);
        tab
    }

    pub fn key(&self) -> String {
        self.key.clone()
    }

    /// The font and the page at `zoom`, as `Tab::set_font` sets them for an editor.
    pub fn set_font(&self, font: Option<&str>, zoom: f64) {
        for view in &self.views {
            editor::set_margins(view, zoom);
        }
        editor::install_font(&self.font, self.flavour, font, zoom, &self.name);
        // Heading markers hang in the left margin and are measured in the font, so they are
        // measured again once the font has reached the views, as `Tab::rehang` does.
        let compare = Rc::downgrade(&self.compare);
        glib::idle_add_local_once(move || {
            if let Some(compare) = compare.upgrade() {
                compare.restyle();
            }
        });
    }

    pub fn restyle(&self) {
        self.compare.restyle();
    }

    /// Both texts again, after what they compare has moved.
    pub fn set_texts(&self, old: &str, new: &str) {
        self.compare.set_side(Side::Old, old);
        self.compare.set_side(Side::New, new);
    }

    pub fn comparison(&self) -> &Rc<Compare> {
        &self.compare
    }

    /// What takes the keyboard when the tab's pane does: the right column, what it compares to.
    pub fn key_target(&self) -> gtk::Widget {
        gtk::prelude::Cast::upcast(self.views[1].clone())
    }
}
