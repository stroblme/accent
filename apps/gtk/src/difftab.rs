//! Host comparisons of non-file texts, such as staged changes and commits, in their own tabs.
//! `diff.rs` provides the comparison widget.

use gtk::glib;
use sourceview5::prelude::*;
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use crate::diff::{self, Compare, Side, pane};
use crate::editor::{self, Finder, Flavour};

/// Tab for a comparison of non-file texts. It carries the font provider that an editor
/// would otherwise supply to both panes.
pub struct DiffTab {
    pub page: adw::TabPage,
    key: String,
    compare: Rc<Compare>,
    /// Both panes' views. They take the page's margins from the zoom here, having no editor
    /// beside them for the comparison to copy them from.
    views: [sourceview5::View; 2],
    /// What the find bar searches in each pane, the old one first.
    finders: [Rc<Finder>; 2],
    /// The right column's title row, which Show Changes goes in.
    header: gtk::Box,
    /// The comparison, or the right text alone once Show Changes is let go.
    stack: gtk::Stack,
    /// The page's child, which shows the loading page until the comparison is in
    /// ([`DiffTab::show`]).
    holder: gtk::Stack,
    /// The file name behind the comparison, which picks its language.
    file: String,
    /// Show Changes, once [`DiffTab::offer_alone`] has put it in: down while the comparison is
    /// shown.
    changes: OnceCell<gtk::ToggleButton>,
    /// The right text alone, built the first time Show Changes is let go.
    alone: OnceCell<Alone>,
    flavour: Flavour,
    name: String,
    font: RefCell<Option<gtk::CssProvider>>,
    /// The zoom, Column Width and Line Numbers the text alone is shown with, as a text tab is.
    zoom: Cell<f64>,
    column: Cell<u32>,
    numbers: Cell<bool>,
}

/// The right text alone, as its own tab would show it: one read-only column, capped at the
/// Column Width and numbered as Line Numbers says.
struct Alone {
    root: gtk::Box,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    numbers: sourceview5::GutterRenderer,
    clamp: adw::ClampScrollable,
    /// Reports the width the column is capped from (`editor::clamped`).
    width: gtk::DrawingArea,
    finder: Rc<Finder>,
    /// Show Changes in this column's title row, the comparison's own being out of sight.
    changes: gtk::ToggleButton,
    /// The line the right column had at its top, which this one is scrolled to.
    top: gtk::TextMark,
}

impl DiffTab {
    /// `old` and `new` are (title, text), shown once [`DiffTab::show`] says so. `key` is what the
    /// tab is keyed by, see `App::open_diff`; the file name behind it picks the language and the
    /// flavour.
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
        let holder = crate::loading::holder();
        let page = tabs.append(&holder);
        page.set_title(title);
        page.set_icon(Some(&gtk::gio::ThemedIcon::new("view-dual-symbolic")));
        Self::build(page, holder, key, file, flavour, old, new, font, zoom)
    }

    /// The tab built again from `old` and `new` in its page, as a comparison of them opens: one
    /// put up before its texts were read (`App::hold_diff`).
    pub fn rebuild(
        &self,
        old: (&str, &str),
        new: (&str, &str),
        font: Option<&str>,
        zoom: f64,
    ) -> Rc<DiffTab> {
        let (page, holder) = (self.page.clone(), self.holder.clone());
        Self::build(
            page,
            holder,
            &self.key,
            &self.file,
            self.flavour,
            old,
            new,
            font,
            zoom,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        page: adw::TabPage,
        holder: gtk::Stack,
        key: &str,
        file: &str,
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
        let finders = [old.finder.clone(), new.finder.clone()];
        let header = new.header.clone();
        let compare = Compare::new(old, new, None, false);
        let stack = gtk::Stack::new();
        stack.add_child(compare.widget());
        crate::loading::put(&holder, &stack);
        let tab = Rc::new(DiffTab {
            page,
            key: key.to_string(),
            compare,
            views,
            finders,
            header,
            stack,
            holder,
            file: file.to_string(),
            changes: OnceCell::new(),
            alone: OnceCell::new(),
            flavour,
            name,
            font: RefCell::new(None),
            zoom: Cell::new(zoom),
            column: Cell::default(),
            numbers: Cell::default(),
        });
        tab.set_font(font, zoom);
        tab
    }

    pub fn key(&self) -> String {
        self.key.clone()
    }

    /// Put Show Changes at the end of the right column's title row, down: letting it go shows the
    /// right text alone, as its own tab would show it. For a text that never changes, a file as a
    /// commit left it: the text alone is not read again with the comparison.
    pub fn offer_alone(self: &Rc<Self>) {
        if self.changes.get().is_some() {
            return;
        }
        let changes = changes_toggle();
        self.header.append(&changes);
        let weak = Rc::downgrade(self);
        changes.connect_toggled(move |changes| {
            if let Some(tab) = weak.upgrade() {
                tab.show_changes(changes.is_active());
            }
        });
        let _ = self.changes.set(changes);
    }

    /// Whether Show Changes is down, where the tab offers it.
    pub fn changes_shown(&self) -> Option<bool> {
        self.changes.get().map(|changes| changes.is_active())
    }

    /// Show Changes from the palette: the toggle pressed, or let go.
    pub fn toggle_changes(&self) {
        if let Some(changes) = self.changes.get() {
            changes.set_active(!changes.is_active());
        }
    }

    /// The comparison, or the right text alone from where the right column is. The keyboard goes
    /// along with it if it was in the tab, the column it was in being out of sight now.
    fn show_changes(self: &Rc<Self>, on: bool) {
        let focused = self
            .stack
            .state_flags()
            .contains(gtk::StateFlags::FOCUS_WITHIN);
        if on {
            self.stack.set_visible_child(self.compare.widget());
        } else {
            let alone = self.alone();
            let top = alone.follow(&self.views[1]);
            self.stack.set_visible_child(&alone.root);
            alone.scroll_to(top);
        }
        if focused {
            self.key_target().grab_focus();
        }
    }

    /// The right text alone, built on first asking: a commit's tab opens as the comparison, and
    /// a large file would be styled twice for nothing.
    fn alone(self: &Rc<Self>) -> &Alone {
        if let Some(alone) = self.alone.get() {
            return alone;
        }
        let right = &self.views[1];
        let shown = right.buffer();
        let text = shown.text(&shown.start_iter(), &shown.end_iter(), true);
        let language = shown
            .downcast_ref::<sourceview5::Buffer>()
            .and_then(|b| b.language());
        let (view, buffer, numbers) =
            editor::companion(self.flavour, &text, &self.name, language.as_ref());
        numbers.set_visible(self.numbers.get());
        editor::set_margins(&view, self.zoom.get());
        view.set_tab_width(right.tab_width());
        view.set_wrap_mode(right.wrap_mode());
        // The markup's tints are mixed from the theme's foreground, which a view has only once it
        // is mapped, and a heading's markers hang in the font it has then: styled again on every
        // showing, as the comparison styles its columns.
        let flavour = self.flavour;
        view.connect_map(move |view| {
            if let Some(buffer) = view.buffer().downcast_ref::<sourceview5::Buffer>() {
                editor::restyle_companion(flavour, buffer, view);
            }
        });
        let editor::Clamped {
            clamp,
            width,
            overlay,
            ..
        } = editor::clamped(&view);
        let weak = Rc::downgrade(self);
        width.connect_resize(move |_, _, _| {
            if let Some(tab) = weak.upgrade() {
                tab.cap();
            }
        });
        let changes = changes_toggle();
        if let Some(shown) = self.changes.get() {
            shown
                .bind_property("active", &changes, "active")
                .bidirectional()
                .sync_create()
                .build();
        }
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&diff::header(
            &self.page.title(),
            Some(changes.upcast_ref()),
        ));
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&overlay);
        self.stack.add_child(&root);
        let top = buffer.create_mark(None, &buffer.start_iter(), true);
        let finder = Finder::new(&view, &buffer);
        let alone = self.alone.get_or_init(|| Alone {
            root,
            view,
            buffer,
            numbers,
            clamp,
            width,
            finder,
            changes,
            top,
        });
        self.cap();
        alone
    }

    /// The text alone capped as a text tab caps its column, at its width now.
    fn cap(&self) {
        if let Some(alone) = self.alone.get() {
            let max = editor::column_max(alone.width.width(), self.column.get(), self.zoom.get());
            editor::cap_column(&alone.clamp, max);
        }
    }

    /// Every view the tab has: both panes, and the text alone once it has been shown.
    fn all_views(&self) -> impl Iterator<Item = &sourceview5::View> {
        self.views.iter().chain(self.alone.get().map(|a| &a.view))
    }

    /// The text alone, while it is what the tab shows.
    fn shown_alone(&self) -> Option<&Alone> {
        let shown = self.stack.visible_child();
        self.alone
            .get()
            .filter(|a| shown.as_ref() == Some(a.root.upcast_ref()))
    }

    /// The font and the page at `zoom`, as `Tab::set_font` sets them for an editor.
    pub fn set_font(&self, font: Option<&str>, zoom: f64) {
        self.zoom.set(zoom);
        for view in self.all_views() {
            editor::set_margins(view, zoom);
        }
        editor::install_font(&self.font, self.flavour, font, zoom, &self.name);
        for view in self.all_views() {
            editor::refont(view);
        }
        self.cap();
        // Heading markers hang in the left margin and are measured in the font, so they are
        // measured again once the font has reached the views, as `Tab::rehang` does.
        let compare = Rc::downgrade(&self.compare);
        let alone = self
            .alone
            .get()
            .map(|a| (a.buffer.clone(), a.view.downgrade()));
        let flavour = self.flavour;
        glib::idle_add_local_once(move || {
            if let Some(compare) = compare.upgrade() {
                compare.restyle();
            }
            if let Some((buffer, view)) = alone
                && let Some(view) = view.upgrade()
            {
                editor::rehang_companion(flavour, &buffer, &view);
            }
        });
    }

    pub fn restyle(&self) {
        self.compare.restyle();
        if let Some(alone) = self.alone.get() {
            editor::restyle_companion(self.flavour, &alone.buffer, &alone.view);
        }
    }

    /// The Indent Width preference: how many columns a tab takes in both panes, as in a text tab.
    pub fn set_indent_width(&self, columns: u32) {
        for view in self.all_views() {
            view.set_tab_width(columns);
        }
    }

    /// The Column Width and Line Numbers preferences, which the text alone takes as a text tab
    /// does.
    pub fn set_page(&self, column: u32, numbers: bool) {
        self.column.set(column);
        self.numbers.set(numbers);
        if let Some(alone) = self.alone.get() {
            alone.numbers.set_visible(numbers);
        }
        self.cap();
    }

    /// Alt+Z: both panes unwrapped, or wrapped again, together.
    pub fn toggle_wrap(&self) {
        let mode = editor::toggled_wrap(self.views[1].wrap_mode());
        for view in self.all_views() {
            view.set_wrap_mode(mode);
        }
    }

    /// Both texts again, after what they compare has moved.
    pub fn set_texts(&self, old: &str, new: &str) {
        self.compare.set_side(Side::Old, old);
        self.compare.set_side(Side::New, new);
    }

    /// The comparison, in place of the loading page.
    pub fn show(&self) {
        crate::loading::show(&self.holder);
    }

    /// Whether the comparison is shown ([`DiffTab::show`]).
    pub fn filled(&self) -> bool {
        crate::loading::shown(&self.holder)
    }

    pub fn comparison(&self) -> &Rc<Compare> {
        &self.compare
    }

    /// The buttons that fade with the chrome: the comparison's, and Show Changes over the text
    /// alone.
    pub fn controls(&self) -> Vec<gtk::Widget> {
        let mut controls = self.compare.controls();
        controls.extend(self.alone.get().map(|a| a.changes.clone().upcast()));
        controls
    }

    /// Both panes' views, the old one first.
    #[cfg(feature = "bench")]
    pub fn views(&self) -> &[sourceview5::View; 2] {
        &self.views
    }

    /// Show Changes, where the tab offers it.
    #[cfg(feature = "bench")]
    pub fn changes_toggle(&self) -> Option<&gtk::ToggleButton> {
        self.changes.get()
    }

    /// What takes the keyboard when the tab's pane does: the right column, what it compares to,
    /// or the text alone while it is shown.
    pub fn key_target(&self) -> gtk::Widget {
        self.view().clone().upcast()
    }

    /// What the find bar searches: the text alone while it is shown, else the pane `focused` is,
    /// or the right one, as [`Self::key_target`].
    pub fn finder(&self, focused: Option<&sourceview5::View>) -> Rc<Finder> {
        if let Some(alone) = self.shown_alone() {
            return alone.finder.clone();
        }
        let at = self.views.iter().position(|view| Some(view) == focused);
        self.finders[at.unwrap_or(1)].clone()
    }

    /// The pane the editing chords act on from the palette: the right one, as [`Self::key_target`].
    pub fn view(&self) -> &sourceview5::View {
        match self.shown_alone() {
            Some(alone) => &alone.view,
            None => &self.views[1],
        }
    }
}

impl Drop for DiffTab {
    /// Release the font provider, as a text tab does: a Git row's tab is built twice, once as it
    /// is put up and again once its texts are read.
    fn drop(&mut self) {
        if let (Some(display), Some(provider)) =
            (gtk::gdk::Display::default(), self.font.borrow_mut().take())
        {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
    }
}

impl Alone {
    /// Put the caret on the line `right`, the right column, has it on, and say which line `right`
    /// has at the top of its view. Both hold the same text, line for line.
    fn follow(&self, right: &sourceview5::View) -> i32 {
        let shown = right.buffer();
        let caret = shown.iter_at_mark(&shown.get_insert()).line();
        if let Some(at) = self.buffer.iter_at_line(caret) {
            self.buffer.place_cursor(&at);
        }
        right.line_at_y(right.visible_rect().y()).0.line()
    }

    /// Scroll `line` to the top of the view, once GTK has measured the view: one just built
    /// would stop short of it ([`editor::when_measured`]).
    fn scroll_to(&self, line: i32) {
        let Some(at) = self.buffer.iter_at_line(line) else {
            return;
        };
        self.buffer.move_mark(&self.top, &at);
        let top = self.top.clone();
        editor::when_measured(&self.view, false, move |view| {
            view.scroll_to_mark(&top, 0.0, true, 0.0, 0.0)
        });
    }
}

/// Show Changes, as it opens: down, with the comparison shown. A click leaves the keyboard where
/// it was.
fn changes_toggle() -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .icon_name("view-dual-symbolic")
        .tooltip_text("Show Changes")
        .active(true)
        .css_classes(["flat", "chrome-fade"])
        .focus_on_click(false)
        .build()
}
