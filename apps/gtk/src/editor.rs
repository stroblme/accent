//! One editor tab: a `sourceview5::View` on a note, plus its etag, banner, find bar and the
//! debounced work that hangs off a keystroke.
//!
//! Nothing here knows about the app. What the tab has to say goes out through a `connect_*`
//! callback and what it needs from the vault arrives as a closure, so a tab can be built, moved
//! and closed without `main` reaching inside it.

use crate::{completion, highlight, multicaret};
use accent_core::fs::{self, Etag};
use accent_core::markdown::{self, Link};
use adw::prelude::*;
use gtk::{gdk, glib, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// Re-analysing on every keystroke would be wasteful; ~150 ms after the last one is invisible.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// DESIGN.md, Motion: save 1 s after the last edit.
const AUTOSAVE: Duration = Duration::from_secs(1);
/// The cursor callback drives the preview's scroll sync; 100 ms is below what the eye follows.
const CURSOR: Duration = Duration::from_millis(100);

/// A callback the app registered. Stored behind an `Rc` so it can be cloned out of its cell
/// before it runs: a callback is free to reach back into the tab that called it.
type Hook = RefCell<Option<Rc<dyn Fn(&Rc<Tab>)>>>;
type LinkHook = RefCell<Option<Rc<dyn Fn(&Rc<Tab>, &Link)>>>;

/// Why a tab's banner is up. The intent is stored rather than re-derived when the button is
/// pressed, so the button always does what its label says: deriving it from the file system meant
/// a Reload that could arrive as a Save, and a Save that quietly reloaded.
///
/// Both states only ever appear on a tab with unsaved edits: a clean tab is reloaded silently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alert {
    /// Someone else wrote the file while this buffer had edits. The button opens the diff, which
    /// is the only honest one-click answer: neither side can be thrown away unseen.
    Compare,
    /// The file is gone and this buffer is the only copy left. The button writes it back.
    Restore,
}

impl Alert {
    fn title(self) -> &'static str {
        match self {
            Alert::Compare => "This note changed on disk",
            Alert::Restore => "This note was deleted on disk",
        }
    }

    fn button(self) -> &'static str {
        match self {
            Alert::Compare => "Compare",
            Alert::Restore => "Save",
        }
    }
}

pub struct Tab {
    /// Behind a cell because a rename retargets the tab instead of closing and reopening it.
    rel: RefCell<String>,
    path: RefCell<PathBuf>,
    pub view: sourceview5::View,
    pub buffer: sourceview5::Buffer,
    /// Kept for [`Tab::scroll_lines`] and for the scrollbar the minimap replaces.
    scroller: gtk::ScrolledWindow,
    map: sourceview5::Map,
    pub page: adw::TabPage,
    pub banner: adw::Banner,
    pub search: gtk::SearchBar,
    pub etag: Cell<Option<Etag>>,
    pub modified: Cell<bool>,
    /// Someone else changed the file under a dirty tab. Autosave stops until the user has
    /// answered the banner, so a conflict is never resolved behind their back.
    pub disk_changed: Cell<bool>,
    /// What the banner is asking for, or `None` while it is hidden.
    alert: Cell<Option<Alert>>,
    context: sourceview5::SearchContext,
    find_entry: gtk::SearchEntry,
    replace_entry: gtk::Entry,
    replace_row: gtk::Box,
    matches: gtk::Label,
    spell: RefCell<Option<libspelling::TextBufferAdapter>>,
    links: RefCell<Vec<Link>>,
    font: RefCell<Option<gtk::CssProvider>>,
    /// Set while we replace the buffer text ourselves, so `changed` does not mark it dirty.
    loading: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
    autosave: RefCell<Option<glib::SourceId>>,
    cursor: RefCell<Option<glib::SourceId>>,
    on_autosave: Hook,
    on_edited: Hook,
    on_banner: Hook,
    on_cursor: Hook,
    on_follow: LinkHook,
}

/// Open `rel` from `root` in a new tab of `tabs`.
///
/// `notes` and `tags` feed the `[[wikilink]]` and `#tag` completions; they are the only way this
/// module ever reaches the vault. `spellcheck`, `font` and `zoom` are the current preferences.
#[allow(clippy::too_many_arguments)]
pub fn open(
    root: &Path,
    rel: &str,
    tabs: &adw::TabView,
    notes: impl Fn(&str) -> Vec<String> + 'static,
    tags: impl Fn(&str) -> Vec<String> + 'static,
    spellcheck: bool,
    font: Option<&str>,
    zoom: f64,
) -> std::io::Result<Rc<Tab>> {
    let path = root.join(rel);
    let (text, etag) = fs::read_note(&path)?;

    let buffer = sourceview5::Buffer::new(None);
    highlight::install_tags(&buffer);
    buffer.set_text(&text);
    buffer.set_highlight_matching_brackets(false);
    sync_scheme(&buffer);

    // A subclass, so `Shift+Alt+Up`/`Down` can leave extra carets in the buffer. Everything else
    // in this file treats it as the plain view it is.
    let view: sourceview5::View = multicaret::View::new().upcast();
    view.set_buffer(Some(&buffer));
    view.set_monospace(false);
    view.add_css_class("accent-doc");
    view.set_widget_name(&next_view_name());
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_show_line_numbers(false);
    // Apostrophe-like page: generous side gutters, room to breathe at the ends.
    view.set_left_margin(48);
    view.set_right_margin(48);
    view.set_top_margin(24);
    view.set_bottom_margin(96);
    view.set_pixels_above_lines(2);
    view.set_pixels_below_lines(2);
    completion::install(&view, notes, tags);

    // The clamp caps the line, the view's own margins keep it off the edge, and on a narrow
    // window the clamp simply stops applying. 800 leaves 704 px of text, which measures ~96
    // characters in the GNOME document font at its default size: wider than the 60 to 72
    // DESIGN.md asks for, and requested that way because 580 read as a narrow column here.
    let clamp = adw::Clamp::builder()
        .maximum_size(800)
        .tightening_threshold(600)
        .child(&view)
        .build();

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&clamp)
        .build();

    // The minimap is off unless the preference says otherwise; `set_minimap` decides that, so a
    // tab that is built before the config is read still starts in a defined state.
    let map = sourceview5::Map::new();
    map.set_view(&view);
    map.set_vexpand(true);
    map.set_visible(false);
    let document = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    document.append(&scroller);
    document.append(&map);

    let banner = adw::Banner::new("");
    let bar = find_bar();
    let settings = sourceview5::SearchSettings::builder()
        .wrap_around(true)
        .case_sensitive(false)
        .build();
    let context = sourceview5::SearchContext::new(&buffer, Some(&settings));

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&banner);
    column.append(&bar.search);
    column.append(&document);
    let page = tabs.append(&column);
    page.set_title(title_of(rel));

    let tab = Rc::new(Tab {
        rel: RefCell::new(rel.to_string()),
        path: RefCell::new(path),
        view: view.clone(),
        buffer: buffer.clone(),
        scroller: scroller.clone(),
        map: map.clone(),
        page,
        banner: banner.clone(),
        search: bar.search.clone(),
        etag: Cell::new(Some(etag)),
        modified: Cell::new(false),
        disk_changed: Cell::new(false),
        alert: Cell::new(None),
        context: context.clone(),
        find_entry: bar.find.clone(),
        replace_entry: bar.replace.clone(),
        replace_row: bar.replace_row,
        matches: bar.matches,
        spell: RefCell::new(None),
        links: RefCell::new(markdown::analyze(&text).links),
        font: RefCell::new(None),
        loading: Cell::new(false),
        debounce: RefCell::new(None),
        autosave: RefCell::new(None),
        cursor: RefCell::new(None),
        on_autosave: RefCell::new(None),
        on_edited: RefCell::new(None),
        on_banner: RefCell::new(None),
        on_cursor: RefCell::new(None),
        on_follow: RefCell::new(None),
    });
    tab.set_font(font, zoom);
    tab.set_spellcheck(spellcheck);

    highlight::apply(&buffer);
    // `view.color()` only resolves the theme foreground once the widget is mapped. A tab added to
    // the visible TabView is mapped by `append` above, so restyle now *and* on every later map
    // (a background tab is only mapped when it is first selected).
    highlight::restyle(&buffer, &view);
    view.connect_map(glib::clone!(
        #[strong]
        buffer,
        move |view| {
            highlight::restyle(&buffer, view);
            highlight::hang(&buffer, view);
        }
    ));

    // Weak throughout: the buffer, the controllers and the timeouts all live inside the tab, so a
    // strong capture here would be the cycle that kept every closed tab alive.
    buffer.connect_changed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.on_changed()
    ));
    buffer.connect_cursor_position_notify(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.on_cursor_moved()
    ));
    banner.connect_button_clicked(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.emit(&tab.on_banner)
    ));

    // Leaving the view is the other autosave trigger: switching tabs or windows mid-sentence
    // should not be the one edit that is lost.
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.autosave_now()
    ));
    view.add_controller(focus);

    let click = gtk::GestureClick::new();
    click.connect_pressed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |gesture, _, x, y| {
            if !gesture
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                return;
            }
            if let Some(link) = tab.link_at(x, y) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                tab.follow(&link);
            }
        }
    ));
    view.add_controller(click);

    // The pointer only changes when the answer changes: setting a cursor on every motion event
    // would be a GDK call per pixel of travel.
    let hot = Rc::new(Cell::new(false));
    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        #[strong]
        hot,
        move |controller, x, y| {
            let over = controller
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
                && tab.link_at(x, y).is_some();
            if hot.replace(over) != over {
                tab.view
                    .set_cursor_from_name(Some(if over { "pointer" } else { "text" }));
            }
        }
    ));
    view.add_controller(motion);

    wire_find(
        &tab,
        &bar.find,
        &bar.replace,
        &bar.next,
        &bar.previous,
        &bar.replace_one,
        &bar.replace_all,
    );
    context.connect_occurrences_count_notify(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.update_matches()
    ));

    Ok(tab)
}

// ------------------------------------------------------------------------------------ find bar

/// The widgets of the find bar, handed back so `open` can both store and wire them.
struct FindBar {
    search: gtk::SearchBar,
    find: gtk::SearchEntry,
    replace: gtk::Entry,
    replace_row: gtk::Box,
    matches: gtk::Label,
    next: gtk::Button,
    previous: gtk::Button,
    replace_one: gtk::Button,
    replace_all: gtk::Button,
}

fn find_bar() -> FindBar {
    let find = gtk::SearchEntry::builder()
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
    top.append(&find);
    top.append(&matches);
    top.append(&previous);
    top.append(&next);

    let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    replace_row.append(&replace);
    replace_row.append(&replace_one);
    replace_row.append(&replace_all);
    replace_row.set_visible(false);

    let rows = gtk::Box::new(gtk::Orientation::Vertical, 12);
    rows.append(&top);
    rows.append(&replace_row);

    let search = gtk::SearchBar::builder().show_close_button(true).build();
    search.set_child(Some(&rows));
    search.connect_entry(&find);

    FindBar {
        search,
        find,
        replace,
        replace_row,
        matches,
        next,
        previous,
        replace_one,
        replace_all,
    }
}

#[allow(clippy::too_many_arguments)]
fn wire_find(
    tab: &Rc<Tab>,
    find: &gtk::SearchEntry,
    replace: &gtk::Entry,
    next: &gtk::Button,
    previous: &gtk::Button,
    replace_one: &gtk::Button,
    replace_all: &gtk::Button,
) {
    find.connect_search_changed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |entry| {
            tab.context.settings().set_search_text(Some(&entry.text()));
            // From the current match, not past it: typing must not walk through the document.
            tab.step(true, true);
        }
    ));
    find.connect_activate(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.step(true, false)
    ));
    next.connect_clicked(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.step(true, false)
    ));
    previous.connect_clicked(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.step(false, false)
    ));
    replace.connect_activate(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.replace_current()
    ));
    replace_one.connect_clicked(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.replace_current()
    ));
    replace_all.connect_clicked(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.replace_all()
    ));

    // Escape leaves the bar and puts the caret back where the user was typing.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key != gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            tab.close_find();
            glib::Propagation::Stop
        }
    ));
    tab.search.add_controller(keys);
    tab.search.connect_search_mode_enabled_notify(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |bar| {
            if !bar.is_search_mode() {
                tab.context.set_highlight(false);
            }
        }
    ));
}

// ------------------------------------------------------------------------------------- helpers

/// GtkSourceView paints its background from its own style scheme, so unlike every other widget in
/// the window it has to be told about dark mode explicitly.
fn sync_scheme(buffer: &sourceview5::Buffer) {
    let id = if adw::StyleManager::default().is_dark() {
        "Adwaita-dark"
    } else {
        "Adwaita"
    };
    let scheme = sourceview5::StyleSchemeManager::default().scheme(id);
    buffer.set_style_scheme(scheme.as_ref());
}

fn title_of(rel: &str) -> &str {
    rel.rsplit('/')
        .next()
        .map(|n| n.strip_suffix(".md").unwrap_or(n))
        .unwrap_or(rel)
}

/// A per-view CSS name, so the font override can be one provider per tab.
///
/// ponytail: `#name` is the only per-widget CSS hook GTK 4 still offers — `StyleContext` and its
/// `add_provider` are deprecated since 4.10 — and the font is a global preference, so one
/// display-wide provider would do. Swap for that if the provider count ever matters.
fn next_view_name() -> String {
    thread_local! {
        static NEXT: Cell<u32> = const { Cell::new(0) };
    }
    NEXT.with(|n| {
        n.set(n.get() + 1);
        format!("accent-doc-{}", n.get())
    })
}

/// The GNOME document font, which is what `main::install_document_font` puts on every editor.
fn system_font() -> String {
    adw::StyleManager::default()
        .document_font_name()
        .to_string()
}

/// The text a duplicated line is inserted as. A line that already ends in a newline can be
/// repeated as it stands; the last line of a file has none, so the copy brings its own.
fn duplicated(line: &str) -> String {
    match line.ends_with('\n') {
        true => line.to_string(),
        false => format!("\n{line}"),
    }
}

/// Family and point size of a font description, with GNOME's defaults where it is silent, scaled
/// by `zoom`. Rounded to two decimals so stepping the zoom does not write `12.100000000000001pt`.
fn font_css(name: &str, selector: &str, zoom: f64) -> String {
    let desc = pango::FontDescription::from_string(name);
    let family = desc
        .family()
        .map(|f| f.to_string())
        .unwrap_or_else(|| "Cantarell".to_string());
    let size = match desc.size() as f64 / pango::SCALE as f64 {
        pt if pt > 0.0 => pt,
        _ => 11.0,
    };
    let size = (size * zoom * 100.0).round() / 100.0;
    format!("{selector} {{ font-family: \"{family}\"; font-size: {size}pt; }}")
}

// ---------------------------------------------------------------------------------------- tab

impl Tab {
    pub fn rel(&self) -> String {
        self.rel.borrow().clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    /// A rename landed: point the tab at the new path without losing the buffer.
    pub fn retarget(&self, root: &Path, new_rel: &str) {
        *self.rel.borrow_mut() = new_rel.to_string();
        *self.path.borrow_mut() = root.join(new_rel);
        self.page.set_title(&self.tab_title());
    }

    pub fn text(&self) -> String {
        let (s, e) = self.buffer.bounds();
        self.buffer.text(&s, &e, true).to_string()
    }

    /// Replace the buffer with `text` without marking the tab dirty.
    fn set_text(&self, text: &str) {
        self.loading.set(true);
        self.buffer.set_text(text);
        self.loading.set(false);
        *self.links.borrow_mut() = markdown::analyze(text).links;
        highlight::apply(&self.buffer);
    }

    pub fn mark_clean(&self, etag: Etag) {
        self.etag.set(Some(etag));
        self.modified.set(false);
        self.disk_changed.set(false);
        self.page.set_title(&self.tab_title());
    }

    /// Silent reload for a clean tab: the file changed on disk and there is nothing to lose.
    pub fn reload_keep_cursor(&self) -> std::io::Result<()> {
        let offset = self.buffer.iter_at_mark(&self.buffer.get_insert()).offset();
        let (text, etag) = fs::read_note(&self.path())?;
        self.set_text(&text);
        let iter = self
            .buffer
            .iter_at_offset(offset.min(self.buffer.char_count()));
        self.buffer.place_cursor(&iter);
        self.view
            .scroll_to_mark(&self.buffer.get_insert(), 0.0, false, 0.0, 0.5);
        self.mark_clean(etag);
        self.hide_banner();
        Ok(())
    }

    pub fn restyle(&self) {
        sync_scheme(&self.buffer);
        highlight::restyle(&self.buffer, &self.view);
        highlight::hang(&self.buffer, &self.view);
    }

    /// Raise the banner for `alert`, which decides both what it says and what its button does.
    pub fn show_alert(&self, alert: Alert) {
        self.alert.set(Some(alert));
        self.banner.set_title(alert.title());
        self.banner.set_button_label(Some(alert.button()));
        self.banner.set_revealed(true);
    }

    /// What the visible banner is asking for, for the handler of its button.
    pub fn alert(&self) -> Option<Alert> {
        self.alert.get()
    }

    pub fn hide_banner(&self) {
        self.alert.set(None);
        self.banner.set_revealed(false);
    }

    /// The user chose to lose this buffer's unsaved edits: it stops counting as dirty, so nothing
    /// downstream tries to save it on the way out.
    pub fn discard(&self) {
        self.modified.set(false);
        self.disk_changed.set(false);
        self.page.set_title(&self.tab_title());
        self.hide_banner();
    }

    /// 1-based, the way an editor counts lines and the preview's `data-line` markers do.
    pub fn cursor_line(&self) -> u32 {
        let line = self.buffer.iter_at_mark(&self.buffer.get_insert()).line();
        line.max(0) as u32 + 1
    }

    fn tab_title(&self) -> String {
        let rel = self.rel();
        let name = title_of(&rel);
        match self.modified.get() {
            true => format!("• {name}"),
            false => name.to_string(),
        }
    }

    // --- preferences ---------------------------------------------------------------------

    pub fn set_spellcheck(&self, on: bool) {
        // Cloned out first: the adapter is created inside the `else`, which borrows mutably.
        let existing = self.spell.borrow().clone();
        let adapter = match existing {
            Some(adapter) => adapter,
            None if !on => return,
            None => {
                let adapter = libspelling::TextBufferAdapter::new(
                    &self.buffer,
                    &libspelling::Checker::default(),
                );
                // The adapter *is* the action group its own menu items resolve through.
                self.view.insert_action_group("spelling", Some(&adapter));
                self.view.set_extra_menu(Some(&adapter.menu_model()));
                *self.spell.borrow_mut() = Some(adapter.clone());
                adapter
            }
        };
        adapter.set_enabled(on);
    }

    /// `font` of `None` follows the GNOME document font that `main` installs for every editor;
    /// `zoom` scales whichever of the two applies, and only this tab's document.
    pub fn set_font(self: &Rc<Self>, font: Option<&str>, zoom: f64) {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        if let Some(old) = self.font.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        // At the default zoom and with no font of its own a tab needs no provider at all: the
        // display-wide document font rule already says exactly the right thing. Zooming has to
        // name a font anyway, because CSS has no way to scale a size it cannot see.
        let name = match font.filter(|f| !f.is_empty()) {
            Some(font) => Some(font.to_string()),
            None if zoom != 1.0 => Some(system_font()),
            None => None,
        };
        if let Some(name) = name {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(&font_css(
                &name,
                &format!("#{}", self.view.widget_name()),
                zoom,
            ));
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            *self.font.borrow_mut() = Some(provider);
        }
        self.rehang();
    }

    /// Re-measure the hanging heading markers from the next idle. A CSS font change only reaches
    /// the view's pango context once the frame clock has validated the style, and gtk4-rs 0.11
    /// exposes no `css_changed` vfunc to hang this off.
    pub fn rehang(self: &Rc<Self>) {
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || highlight::hang(&tab.buffer, &tab.view)
        ));
    }

    /// The minimap stands in for the scrollbar rather than sitting next to it, which is what
    /// VS Code's code map does and what keeps the document column from losing width twice.
    pub fn set_minimap(&self, on: bool) {
        self.map.set_visible(on);
        let vertical = match on {
            true => gtk::PolicyType::External,
            false => gtk::PolicyType::Automatic,
        };
        self.scroller
            .set_policy(gtk::PolicyType::Automatic, vertical);
    }

    // --- line operations -----------------------------------------------------------------

    /// The caret's line, from its start to the start of the next one, so the trailing newline is
    /// part of it except on a last line that has none.
    fn line_bounds(&self) -> (gtk::TextIter, gtk::TextIter) {
        let mut start = self.buffer.iter_at_mark(&self.buffer.get_insert());
        start.set_line_offset(0);
        let mut end = start;
        // On the last line this lands on the end of the buffer and reports failure, which is
        // exactly where the line ends, so the answer is the same either way.
        end.forward_line();
        (start, end)
    }

    pub fn duplicate_line(&self) {
        let (start, mut end) = self.line_bounds();
        let line = self.buffer.text(&start, &end, true);
        self.buffer.begin_user_action();
        self.buffer.insert(&mut end, &duplicated(&line));
        self.buffer.end_user_action();
    }

    pub fn delete_line(&self) {
        let (mut start, mut end) = self.line_bounds();
        // A last line with no newline of its own takes the one separating it from the line
        // above, or deleting it would leave the blank line it used to sit on.
        if !self.buffer.text(&start, &end, true).ends_with('\n') {
            start.backward_char();
        }
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.end_user_action();
    }

    /// Scroll the viewport by `n` lines, leaving the caret where it is. The adjustment's own
    /// `step_increment` is a tenth of a page in GtkTextView rather than a line, so the height
    /// comes from the first visible line instead.
    pub fn scroll_lines(&self, n: i32) {
        let visible = self.view.visible_rect();
        let height = self
            .view
            .iter_at_location(visible.x(), visible.y())
            .map(|iter| self.view.iter_location(&iter).height())
            .filter(|height| *height > 0);
        let Some(height) = height else { return };
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value(adjustment.value() + f64::from(n * height));
    }

    /// VS Code's Add Cursor Above / Below. Multi-caret lives on the view subclass; the tab keeps
    /// the plain `sourceview5::View` type so nothing else has to know about it.
    pub fn add_caret(&self, below: bool) {
        if let Some(view) = self.view.downcast_ref::<multicaret::View>() {
            view.add_caret(below);
        }
    }

    // --- links ---------------------------------------------------------------------------

    pub fn link_at_cursor(&self) -> Option<Link> {
        let iter = self.buffer.iter_at_mark(&self.buffer.get_insert());
        self.link_at_iter(&iter)
    }

    /// The link under a pointer position in the view's own coordinates.
    pub fn link_at(&self, x: f64, y: f64) -> Option<Link> {
        let (bx, by) =
            self.view
                .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let iter = self.view.iter_at_location(bx, by)?;
        self.link_at_iter(&iter)
    }

    /// Link ranges are byte offsets into the note, `TextIter`s count characters, so the text up
    /// to the iter is what translates between them.
    fn link_at_iter(&self, iter: &gtk::TextIter) -> Option<Link> {
        let start = self.buffer.start_iter();
        let byte = self.buffer.text(&start, iter, true).len();
        self.links
            .borrow()
            .iter()
            .find(|link| link.range.contains(&byte))
            .cloned()
    }

    // --- find and replace ----------------------------------------------------------------

    /// Reveal the find bar, prefilled from the selection when there is one worth searching for.
    pub fn find(&self, replace: bool) {
        if let Some((s, e)) = self.buffer.selection_bounds() {
            let selected = self.buffer.text(&s, &e, false);
            if !selected.is_empty() && !selected.contains('\n') {
                self.find_entry.set_text(&selected);
            }
        }
        self.replace_row.set_visible(replace);
        self.context.set_highlight(true);
        self.search.set_search_mode(true);
        self.find_entry.grab_focus();
        self.find_entry.select_region(0, -1);
    }

    pub fn find_next(&self) {
        self.step(true, false);
    }

    pub fn find_previous(&self) {
        self.step(false, false);
    }

    fn close_find(&self) {
        self.search.set_search_mode(false);
        self.context.set_highlight(false);
        self.view.grab_focus();
    }

    /// Move to the next or previous match. `from_current` searches from the start of the current
    /// selection, so growing the query keeps the match the user is looking at.
    fn step(&self, forward: bool, from_current: bool) {
        let insert = self.buffer.iter_at_mark(&self.buffer.get_insert());
        let (start, end) = self.buffer.selection_bounds().unwrap_or((insert, insert));
        let found = match (forward, from_current) {
            (true, true) => self.context.forward(&start),
            (true, false) => self.context.forward(&end),
            (false, _) => self.context.backward(&start),
        };
        if let Some((s, e, _)) = found {
            self.buffer.select_range(&s, &e);
            self.view
                .scroll_to_mark(&self.buffer.get_insert(), 0.1, false, 0.0, 0.5);
        }
        self.update_matches();
    }

    fn replace_current(&self) {
        if let Some((mut s, mut e)) = self.buffer.selection_bounds() {
            // Fails when the selection is not itself a match, which is the "nothing to do" case.
            let _ = self
                .context
                .replace(&mut s, &mut e, &self.replace_entry.text());
        }
        self.step(true, false);
    }

    /// sourceview5 0.11 exposes no `replace_all` binding, so this walks the matches. Each pass
    /// resumes after the text just inserted, so a replacement containing the query terminates.
    fn replace_all(&self) {
        let with = self.replace_entry.text();
        let mut from = self.buffer.start_iter();
        self.buffer.begin_user_action();
        while let Some((mut s, mut e, _)) = self.context.forward(&from) {
            if self.context.replace(&mut s, &mut e, &with).is_err() {
                break;
            }
            from = e;
        }
        self.buffer.end_user_action();
        self.update_matches();
    }

    /// "n of m", the way every find bar says it. Blank while GtkSourceView is still counting.
    fn update_matches(&self) {
        let count = self.context.occurrences_count();
        let blank = self
            .context
            .settings()
            .search_text()
            .is_none_or(|t| t.is_empty());
        let label = match (blank, count) {
            (true, _) | (_, ..0) => String::new(),
            (_, 0) => "No results".to_string(),
            _ => match self
                .buffer
                .selection_bounds()
                .map(|(s, e)| self.context.occurrence_position(&s, &e))
            {
                Some(position) if position > 0 => format!("{position} of {count}"),
                _ => format!("{count} matches"),
            },
        };
        self.matches.set_text(&label);
    }

    // --- callbacks -----------------------------------------------------------------------

    /// Called 1 s after the last edit and when focus leaves the view. Never fires while
    /// `disk_changed` is set.
    pub fn connect_autosave(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.on_autosave.borrow_mut() = Some(Rc::new(f));
    }

    /// Called after the re-highlight debounce, for the preview.
    pub fn connect_edited(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.on_edited.borrow_mut() = Some(Rc::new(f));
    }

    /// Called when the banner's button is pressed.
    pub fn connect_banner(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.on_banner.borrow_mut() = Some(Rc::new(f));
    }

    /// Called for a Ctrl+click or a Ctrl+Return on a link.
    pub fn connect_follow(self: &Rc<Self>, f: impl Fn(&Rc<Tab>, &Link) + 'static) {
        *self.on_follow.borrow_mut() = Some(Rc::new(f));
    }

    /// Called at most every 100 ms while the caret moves.
    pub fn connect_cursor(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        *self.on_cursor.borrow_mut() = Some(Rc::new(f));
    }

    /// Cloned out of its cell before it runs: a callback may reach back into this tab, and a
    /// live borrow here would be a panic waiting for the first re-entrant call.
    fn emit(self: &Rc<Self>, hook: &Hook) {
        let f = hook.borrow().clone();
        if let Some(f) = f {
            f(self);
        }
    }

    fn follow(self: &Rc<Self>, link: &Link) {
        let f = self.on_follow.borrow().clone();
        if let Some(f) = f {
            f(self, link);
        }
    }

    // --- edits ---------------------------------------------------------------------------

    fn on_changed(self: &Rc<Self>) {
        if self.loading.get() {
            return;
        }
        if !self.modified.replace(true) {
            self.page.set_title(&self.tab_title());
        }
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            DEBOUNCE,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || {
                    *tab.debounce.borrow_mut() = None;
                    // ponytail: the note is parsed twice per debounce, once here for the links
                    // that Ctrl+click and Ctrl+Return follow and once inside `highlight::apply`
                    // for the spans. Have `apply` return the `Analysis` when `highlight.rs` is
                    // next touched, and this call goes away.
                    *tab.links.borrow_mut() = markdown::analyze(&tab.text()).links;
                    highlight::apply(&tab.buffer);
                    tab.emit(&tab.on_edited);
                }
            ),
        );
        *self.debounce.borrow_mut() = Some(id);
        self.schedule_autosave();
    }

    fn schedule_autosave(self: &Rc<Self>) {
        if let Some(id) = self.autosave.borrow_mut().take() {
            id.remove();
        }
        if self.disk_changed.get() {
            return;
        }
        let id = glib::timeout_add_local_once(
            AUTOSAVE,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || {
                    *tab.autosave.borrow_mut() = None;
                    tab.autosave_now();
                }
            ),
        );
        *self.autosave.borrow_mut() = Some(id);
    }

    /// Save now, unless the file changed underneath us: the user is looking at a banner asking
    /// what to do about it, and writing over the answer they have not given yet is not an option.
    fn autosave_now(self: &Rc<Self>) {
        if let Some(id) = self.autosave.borrow_mut().take() {
            id.remove();
        }
        if self.modified.get() && !self.disk_changed.get() {
            self.emit(&self.on_autosave);
        }
    }

    fn on_cursor_moved(self: &Rc<Self>) {
        if self.cursor.borrow().is_some() {
            return;
        }
        let id = glib::timeout_add_local_once(
            CURSOR,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || {
                    *tab.cursor.borrow_mut() = None;
                    tab.emit(&tab.on_cursor);
                }
            ),
        );
        *self.cursor.borrow_mut() = Some(id);
    }
}

impl Drop for Tab {
    /// A closed tab takes its pending timeouts and its font provider with it.
    fn drop(&mut self) {
        for pending in [&self.debounce, &self.autosave, &self.cursor] {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
        }
        if let (Some(display), Some(provider)) =
            (gdk::Display::default(), self.font.borrow_mut().take())
        {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_css_scales_the_point_size_by_the_zoom() {
        let css = font_css("Cantarell 11", "#doc", 1.0);
        assert!(css.contains("font-family: \"Cantarell\""), "{css}");
        assert!(css.contains("font-size: 11pt"), "{css}");
        assert!(
            font_css("Cantarell 11", "#doc", 1.5).contains("font-size: 16.5pt"),
            "a zoom multiplies the size"
        );
        assert!(
            font_css("Cantarell 11", "#doc", 1.1).contains("font-size: 12.1pt"),
            "and is rounded, not written out in full binary"
        );
    }

    /// A description with no size of its own falls back to GNOME's 11 pt, zoom included.
    #[test]
    fn font_css_fills_in_a_missing_size() {
        assert!(font_css("Cantarell", "#doc", 2.0).contains("font-size: 22pt"));
    }

    #[test]
    fn a_duplicated_line_brings_its_own_newline_only_when_it_has_none() {
        assert_eq!(duplicated("note\n"), "note\n");
        assert_eq!(duplicated("last line"), "\nlast line");
        assert_eq!(duplicated(""), "\n");
    }
}
