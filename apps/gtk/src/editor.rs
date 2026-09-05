//! One editor tab: a `sourceview5::View` on a note, plus its etag, banner and the debounced
//! work that hangs off a keystroke.
//!
//! Finding and replacing live in `find.rs`, one bar per window: the bar drives the tab's
//! `SearchContext` from the outside, so the same widgets serve every tab and stay on screen while
//! presentation mode has hidden the tab stack.
//!
//! Nothing here knows about the app. What the tab has to say goes out through a `connect_*`
//! callback and what it needs from the vault arrives as a closure, so a tab can be built, moved
//! and closed without `main` reaching inside it.

use crate::{comment, completion, highlight, multicaret, typing};
use accent_core::fs::{self, Etag};
use accent_core::markdown::{Heading, Link};
use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

/// How long a long note waits after the last keystroke before it is re-analysed.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Notes at or below this many characters are re-styled on the keystroke instead, so markup is
/// styled as it is typed the way Apostrophe does it, rather than snapping into place once the
/// typist pauses. Measured cost of a full pass, tag churn included, which dominates the parsing:
/// 0.7 ms at 2 KB, 2.5 ms at 8 KB, 10 ms at 32 KB, 21 ms at 64 KB. This size stays inside a frame
/// and still covers the notes people actually write (median 3.5 KB in the test vault). A longer
/// note keeps the debounce, because a pass that outlasts a frame is felt as input lag.
const INSTANT: i32 = 16 * 1024;
/// DESIGN.md, Motion: save 1 s after the last edit.
const AUTOSAVE: Duration = Duration::from_secs(1);
/// The cursor callback drives the preview's scroll sync; 100 ms is below what the eye follows.
const CURSOR: Duration = Duration::from_millis(100);
/// Monospace by default, so code fences, tables and wikilinks line up. GNOME ships it with the
/// interface fonts, and `Reset` in preferences comes back here.
const DEFAULT_FAMILY: &str = "Adwaita Mono";
/// The page at 100 %: side gutters and the room above and below the text. [`Tab::set_page`]
/// scales them with the zoom along with the column, so zooming keeps the page's proportions
/// instead of squeezing the text into unchanged gutters.
const GUTTER: i32 = 48;
const TOP: i32 = 24;
const BOTTOM: i32 = 96;
/// The narrowest the document column is ever capped at, in 100 % pixels. A percentage of a
/// narrow editor can ask for less than a line worth reading; this leaves 384 px of text between
/// the gutters, which measures ~52 characters in the GNOME document font. It only ever applies
/// below 1600 px of editor, because the preferences row's minimum is 30 %.
const COLUMN_FLOOR: i32 = 480;
/// How muted an unhovered line number is, as an opacity over the view's background. The style
/// scheme already draws the gutter in a grey of its own, so this is a step back from that rather
/// than the whole distance; 0.6 is the alpha `highlight::restyle` gives quotes.
const DIM: f64 = 0.6;

/// A callback the app registered. Stored behind an `Rc` so it can be cloned out of its cell
/// before it runs: a callback is free to reach back into the tab that called it.
type Hook = RefCell<Option<Rc<dyn Fn(&Rc<Tab>)>>>;
type LinkHook = RefCell<Option<Rc<dyn Fn(&Rc<Tab>, &Link)>>>;

/// Why a tab's banner is up. The intent is stored rather than re-derived when the button is
/// pressed, so the button always does what its label says: deriving it from the file system meant
/// a Reload that could arrive as a Save, and a Save that quietly reloaded.
///
/// The first two only ever appear on a tab with unsaved edits: a clean tab is reloaded silently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alert {
    /// Someone else wrote the file while this buffer had edits. The button opens the diff, which
    /// is the only honest one-click answer: neither side can be thrown away unseen.
    Compare,
    /// The file is gone and this buffer is the only copy left. The button writes it back.
    Restore,
    /// Syncthing left a `*.sync-conflict-*` copy of this note beside it. The button opens the
    /// same side-by-side resolver the tree offers, on the copy the vault reports.
    Conflict,
    /// The bytes are not valid UTF-8, so what is on screen is a lossy reading of them. There is
    /// no button: the only safe answer is to leave the file alone, which is what the tab does.
    ReadOnly,
}

impl Alert {
    fn title(self) -> &'static str {
        match self {
            Alert::Compare => "This note changed on disk",
            Alert::Restore => "This note was deleted on disk",
            Alert::Conflict => "A sync conflict copy of this note exists",
            Alert::ReadOnly => "This file is not valid UTF-8 and is shown read-only",
        }
    }

    /// `None` for a banner that only reports, which DESIGN.md allows: a banner is a state that
    /// persists, and not every state has an answer.
    fn button(self) -> Option<&'static str> {
        match self {
            Alert::Compare => Some("Compare"),
            Alert::Restore => Some("Save"),
            Alert::Conflict => Some("Resolve"),
            Alert::ReadOnly => None,
        }
    }
}

/// What kind of text a tab holds.
///
/// Prose and code share every mechanism a tab has — the etag, autosave, find, zoom, the banner —
/// and differ only in how they are shown, so this is a field rather than a second tab type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour {
    /// A markdown note: our own styling spans, wikilink completion, spellcheck, a capped column.
    Note,
    /// Anything else that is text: a GtkSourceView language, monospace, the full width.
    Code,
    /// A CSV, which is code that gets its columns coloured instead of a language.
    Csv,
}

impl Flavour {
    pub fn is_note(self) -> bool {
        self == Flavour::Note
    }
}

/// The preferences a tab is built with. A struct rather than five positional arguments, which is
/// what they were until code tabs needed a sixth.
pub struct Prefs {
    pub spellcheck: bool,
    pub font: Option<String>,
    pub zoom: f64,
    pub column_width: u32,
    pub minimap: bool,
    pub line_numbers: bool,
}

pub struct Tab {
    /// Behind a cell because a rename retargets the tab instead of closing and reopening it.
    rel: RefCell<String>,
    path: RefCell<PathBuf>,
    pub view: sourceview5::View,
    pub buffer: sourceview5::Buffer,
    /// Kept for [`Tab::scroll_lines`] and for the scrollbar the minimap replaces.
    scroller: gtk::ScrolledWindow,
    /// The width cap on the document column, sized by [`Tab::set_clamp`].
    clamp: adw::Clamp,
    /// What the cap is computed from: the document zoom and the column's percentage of the
    /// editor's width. Kept here because the editor is also resized from the outside, and a
    /// resize has to recompute the cap without being told the other two again.
    zoom: Cell<f64>,
    column: Cell<u32>,
    map: sourceview5::Map,
    /// The optional line-number gutter; hidden unless the preference turns it on.
    numbers: sourceview5::GutterRendererText,
    /// What this tab holds, fixed when it opened. Everything markdown-specific — the styling
    /// spans, completion, spellcheck, the column cap, the hanging heading markers — asks this
    /// first, so a source file gets a source editor and a note is unchanged.
    flavour: Flavour,
    /// The file used CRLF line endings. The buffer never sees them and every save puts them back,
    /// so editing one line of a DOS file does not rewrite every line of it.
    crlf: Cell<bool>,
    /// The bytes were not valid UTF-8 and what is shown is a lossy reading of them. The view is
    /// not editable, because writing the buffer back would replace every undecodable byte with a
    /// replacement character.
    lossy: Cell<bool>,
    pub page: adw::TabPage,
    pub banner: adw::Banner,
    pub etag: Cell<Option<Etag>>,
    pub modified: Cell<bool>,
    /// Someone else changed the file under a dirty tab. Autosave stops until the user has
    /// answered the banner, so a conflict is never resolved behind their back.
    pub disk_changed: Cell<bool>,
    /// What the banner is asking for, or `None` while it is hidden.
    alert: Cell<Option<Alert>>,
    context: sourceview5::SearchContext,
    spell: RefCell<Option<libspelling::TextBufferAdapter>>,
    links: RefCell<Vec<Link>>,
    /// The note's headings, for the Outline pane. Produced by the same analysis as the links, so
    /// keeping them costs nothing over throwing them away.
    headings: RefCell<Vec<Heading>>,
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

/// Open `key` in a new tab of `tabs`, with `text` already read from disk.
///
/// The bytes are read by the caller rather than here, because deciding what a file is — text,
/// binary, too large — is what picks the kind of tab in the first place, and by the time we are
/// called that question is settled.
///
/// `key` is vault-relative, or absolute for a file from outside the vault; `root` is the vault's
/// and is only used to build the path and the tooltip, so an absolute key simply ignores it.
/// `completions` are the note and tag lookups behind `[[wikilink]]` and `#tag` completion, and
/// the only way this module ever reaches the vault; a code tab never calls them.
pub fn open(
    root: &Path,
    key: &str,
    text: fs::Text,
    flavour: Flavour,
    tabs: &adw::TabView,
    prefs: &Prefs,
    completions: (
        impl Fn(&str) -> Vec<String> + 'static,
        impl Fn(&str) -> Vec<String> + 'static,
    ),
) -> Rc<Tab> {
    let path = root.join(key);
    let (zoom, column_width) = (prefs.zoom, prefs.column_width);

    // A language for code, none for a note (our own spans do that) and none for a CSV, whose
    // `csv.lang` would colour numbers and strings underneath the column tags and fight them.
    let language = match flavour {
        Flavour::Code => guess_language(&path, &text.text),
        Flavour::Note | Flavour::Csv => None,
    };
    let buffer = sourceview5::Buffer::new(None);
    buffer.set_language(language.as_ref());
    match flavour {
        Flavour::Note => highlight::install_tags(&buffer),
        Flavour::Csv => highlight::install_csv_tags(&buffer),
        Flavour::Code => {}
    }
    buffer.set_text(&text.text);
    // Bracket matching is noise in prose and the point in code.
    buffer.set_highlight_matching_brackets(!flavour.is_note());
    sync_scheme(&buffer);

    // A subclass, so `Shift+Alt+Up`/`Down` can leave extra carets in the buffer. Everything else
    // in this file treats it as the plain view it is.
    let view: sourceview5::View = multicaret::View::new().upcast();
    view.set_buffer(Some(&buffer));
    view.set_monospace(!flavour.is_note());
    view.add_css_class(match flavour {
        Flavour::Note => "accent-doc",
        _ => "accent-code",
    });
    view.set_widget_name(&next_view_name());
    // Prose wraps because a line is a paragraph; code does not, because a line is a line.
    view.set_wrap_mode(match flavour {
        Flavour::Note => gtk::WrapMode::WordChar,
        _ => gtk::WrapMode::None,
    });
    view.set_show_line_numbers(false);
    if !flavour.is_note() {
        view.set_auto_indent(true);
        view.set_indent_on_tab(true);
        view.set_smart_backspace(true);
        view.set_highlight_current_line(true);
        view.set_tab_width(4);
        // Everything but a makefile, where a leading tab is syntax.
        let tabs_are_syntax = language.as_ref().is_some_and(|l| l.id() == "makefile");
        view.set_insert_spaces_instead_of_tabs(!tabs_are_syntax);
    }
    // Not valid UTF-8: what is on screen is lossy, so it must not be written back.
    if text.lossy {
        view.set_editable(false);
    }
    // Apostrophe-like page: generous side gutters, room to breathe at the ends. `set_page`,
    // called from `set_font` below, puts the zoomed values here.
    view.set_pixels_above_lines(2);
    view.set_pixels_below_lines(2);
    let numbers = line_numbers(&view, &buffer);
    // Both are markdown behaviour: wikilink and tag completion, and continuing a list or a fence
    // on Return. In a Python file they would be wrong rather than merely unused.
    if flavour.is_note() {
        let (notes, tags) = completions;
        completion::install(&view, notes, tags);
        typing::install(&view);
    }

    // The clamp caps the line, the view's own margins keep it off the edge, and on a narrow
    // window the clamp simply stops applying. Its maximum is a share of the editor's own width
    // (`Config::column_width`), which `set_clamp` puts here as soon as that width is known.
    let clamp = adw::Clamp::builder().child(&view).build();

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
    let settings = sourceview5::SearchSettings::builder()
        .wrap_around(true)
        .case_sensitive(false)
        .build();
    let context = sourceview5::SearchContext::new(&buffer, Some(&settings));

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&banner);
    column.append(&document);
    let page = tabs.append(&column);
    page.set_title(tab_name(key, flavour));
    // The title is only the file name, so where the note really lives is a hover away.
    page.set_tooltip(&crate::fileops::display_path(root, key));

    let tab = Rc::new(Tab {
        rel: RefCell::new(key.to_string()),
        path: RefCell::new(path),
        view: view.clone(),
        buffer: buffer.clone(),
        scroller: scroller.clone(),
        clamp,
        zoom: Cell::new(zoom),
        column: Cell::new(column_width),
        map: map.clone(),
        numbers,
        flavour,
        crlf: Cell::new(text.crlf),
        lossy: Cell::new(text.lossy),
        page,
        banner: banner.clone(),
        etag: Cell::new(Some(text.etag)),
        modified: Cell::new(false),
        disk_changed: Cell::new(false),
        alert: Cell::new(None),
        context,
        spell: RefCell::new(None),
        links: RefCell::new(Vec::new()),
        headings: RefCell::new(Vec::new()),
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
    tab.set_font(prefs.font.as_deref(), zoom);
    tab.set_spellcheck(prefs.spellcheck);
    tab.set_minimap(prefs.minimap);
    tab.set_line_numbers(prefs.line_numbers);
    if text.lossy {
        tab.show_alert(Alert::ReadOnly);
    }

    // The column is a share of the editor's width, so the cap has to be recomputed whenever that
    // width changes. GTK 4 dropped ::size-allocate, and the scrolled window publishes its
    // viewport width as the horizontal adjustment's page size, which is the same number.
    scroller
        .hadjustment()
        .connect_page_size_notify(glib::clone!(
            #[weak(rename_to = tab)]
            tab,
            move |_| tab.set_clamp()
        ));

    tab.analyse();
    // `view.color()` only resolves the theme foreground once the widget is mapped. A tab added to
    // the visible TabView is mapped by `append` above, so restyle now *and* on every later map
    // (a background tab is only mapped when it is first selected). Code takes its colours from
    // the style scheme, which needs none of this.
    if flavour == Flavour::Csv {
        highlight::restyle_csv(&buffer);
        view.connect_map(glib::clone!(
            #[strong]
            buffer,
            move |_| highlight::restyle_csv(&buffer)
        ));
    }
    if flavour.is_note() {
        highlight::restyle(&buffer, &view);
        view.connect_map(glib::clone!(
            #[strong]
            buffer,
            move |view| {
                highlight::restyle(&buffer, view);
                highlight::hang(&buffer, view);
            }
        ));
    }

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

    tab
}

/// The language for `path`, or `None` when GtkSourceView knows none for it.
///
/// The content type is guessed first and handed over with the name, which is what makes a file
/// with no extension work: gio matches `Makefile` and `Dockerfile` by name and falls back to
/// sniffing the bytes, so a `#!/bin/sh` script with no suffix still lands on `sh`.
fn guess_language(path: &Path, text: &str) -> Option<sourceview5::Language> {
    let (content_type, _) = gio::content_type_guess(Some(path), text.as_bytes());
    sourceview5::LanguageManager::default().guess_language(Some(path), Some(&content_type))
}

/// A tab's label: the file name, with `.md` dropped for a note because every note has it.
fn tab_name(key: &str, flavour: Flavour) -> &str {
    match flavour {
        Flavour::Note => title_of(key),
        _ => crate::doc::file_name(key),
    }
}

/// The clamp's maximum for a column that is `percent` of an editor `available` pixels wide.
///
/// Floored at [`COLUMN_FLOOR`] so a narrow window keeps a readable line, then scaled by the zoom
/// like the rest of the page: the percentage is of the editor at 100 %, and zooming in widens the
/// cap until it exceeds the editor and the column simply fills it, which is what the fixed 800 px
/// cap did too.
fn column_max(available: i32, percent: u32, zoom: f64) -> i32 {
    let wanted = f64::from(available) * f64::from(percent) / 100.0;
    (wanted.max(f64::from(COLUMN_FLOOR)) * zoom).round() as i32
}

// -------------------------------------------------------------------------------- line numbers

/// How many digits the last line's number needs. Every label is padded to this width, so they all
/// measure the same and the gutter cannot change width as the view scrolls.
fn digits(line_count: i32) -> usize {
    line_count.max(1).to_string().len()
}

/// A line-number gutter: every line numbered, dimmed, and lifted to full strength while the
/// pointer is in the gutter.
///
/// Headings are numbered like everything else. Their `#` markers hang in the 48 px page gutter
/// (`highlight::hang`), which is a column away from the numbers, so the two read as two margins
/// rather than as two things in one place.
///
/// Dimmed by the widget's own opacity rather than by a colour, because a gutter renderer has no
/// colour to set: composited over the view's background that is the same thing as the foreground
/// at an alpha, which is how `highlight::restyle` dims everything else. The pointer takes it back
/// to the full strength it was drawn at before, which is the style scheme's own gutter grey.
///
/// A plain `GutterRendererText` rather than a subclass: `query-data` arrives once per visible
/// line and only has to print a number. Same shape as `diff.rs`, which prints source numbers the
/// same way. The renderer is a child of the view, so the per-tab `accent-doc-N` font provider
/// reaches it and the zoom follows.
fn line_numbers(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
) -> sourceview5::GutterRendererText {
    let renderer = sourceview5::GutterRendererText::new();
    renderer.set_xalign(1.0);
    renderer.set_xpad(6);
    renderer.set_visible(false);
    renderer.set_opacity(DIM);

    let width = Rc::new(Cell::new(digits(buffer.line_count())));
    renderer.set_text(&" ".repeat(width.get()));

    renderer.connect_query_data(glib::clone!(
        #[strong]
        width,
        move |renderer, _, line| {
            let width = width.get();
            renderer.set_text(&format!("{:>width$}", line + 1));
        }
    ));
    buffer.connect_changed(glib::clone!(
        #[weak]
        renderer,
        #[strong]
        width,
        move |buffer| {
            let wanted = digits(buffer.line_count());
            if width.replace(wanted) != wanted {
                renderer.set_text(&" ".repeat(wanted));
                renderer.queue_resize();
            }
        }
    ));

    // Disambiguated: `TextViewExt` has a `gutter` of its own.
    let gutter = sourceview5::prelude::ViewExt::gutter(view, gtk::TextWindowType::Left);
    gutter.insert(&renderer, 0);

    // Gutter-wide rather than per line: the pointer anywhere in the column lifts every number at
    // once. GTK picks the renderer itself under the pointer, but the controller goes on its
    // parent, whose `contains-pointer` covers the whole column, padding included.
    let motion = gtk::EventControllerMotion::new();
    motion.connect_enter(glib::clone!(
        #[weak]
        renderer,
        move |_, _, _| renderer.set_opacity(1.0)
    ));
    motion.connect_leave(glib::clone!(
        #[weak]
        renderer,
        move |_| renderer.set_opacity(DIM)
    ));
    gutter.add_controller(motion);
    renderer
}

// ------------------------------------------------------------------------------------- helpers

/// GtkSourceView paints its background from its own style scheme, so unlike every other widget in
/// the window it has to be told about the theme explicitly.
pub fn sync_scheme(buffer: &sourceview5::Buffer) {
    let id = crate::theme::scheme_id(adw::StyleManager::default().is_dark());
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

/// The editor's default font: Adwaita Mono at the size of the GNOME document font, and what
/// `main::install_document_font` puts on every editor until a preference overrides it.
///
/// The family is ours and the size still follows the system. DESIGN.md used to take the document
/// font whole on the grounds that notes are prose, but a vault is prose with code fences, tables
/// and wikilinks in it, and none of those line up in a proportional face.
pub fn default_font() -> String {
    let mut desc =
        pango::FontDescription::from_string(&adw::StyleManager::default().document_font_name());
    desc.set_family(DEFAULT_FAMILY);
    desc.to_str().to_string()
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

    pub fn flavour(&self) -> Flavour {
        self.flavour
    }

    /// The note's headings, most recent analysis, for the Outline pane.
    pub fn headings(&self) -> Vec<Heading> {
        self.headings.borrow().clone()
    }

    /// The buffer in the shape the file should hold it: trailing whitespace off code lines, and
    /// the line endings it arrived with. A note is written exactly as typed — two trailing spaces
    /// are a hard line break in markdown.
    pub fn for_disk(&self) -> String {
        fs::for_disk(&self.text(), self.crlf.get(), !self.flavour.is_note())
    }

    /// How the file is encoded and how its lines end, for the readout in the header.
    pub fn encoding_label(&self) -> String {
        let encoding = match self.lossy.get() {
            true => "Not UTF-8",
            false => "UTF-8",
        };
        let ending = match self.crlf.get() {
            true => "CRLF",
            false => "LF",
        };
        format!("{encoding} · {ending}")
    }

    /// A rename landed: point the tab at the new path without losing the buffer.
    pub fn retarget(&self, root: &Path, new_rel: &str) {
        *self.rel.borrow_mut() = new_rel.to_string();
        *self.path.borrow_mut() = root.join(new_rel);
        self.page.set_title(&self.tab_title());
        self.page
            .set_tooltip(&crate::fileops::display_path(root, new_rel));
    }

    pub fn text(&self) -> String {
        let (s, e) = self.buffer.bounds();
        self.buffer.text(&s, &e, true).to_string()
    }

    /// Replace the buffer with `text` without marking the tab dirty. Callers are either loading
    /// from disk or about to write what they just put in.
    pub fn set_text(&self, text: &str) {
        self.loading.set(true);
        self.buffer.set_text(text);
        self.loading.set(false);
        self.analyse();
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
        let text = match fs::read_text(&self.path())? {
            fs::Read::Text(text) => text,
            // It stopped being text while we had it open. The buffer keeps the last readable
            // version rather than showing the user a screen of replacement characters.
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "not text any more",
                ));
            }
        };
        self.crlf.set(text.crlf);
        self.lossy.set(text.lossy);
        let etag = text.etag;
        self.set_text(&text.text);
        let iter = self
            .buffer
            .iter_at_offset(offset.min(self.buffer.char_count()));
        self.buffer.place_cursor(&iter);
        self.view
            .scroll_to_mark(&self.buffer.get_insert(), 0.0, false, 0.0, 0.5);
        self.mark_clean(etag);
        self.clear_disk_alert();
        Ok(())
    }

    pub fn restyle(&self) {
        // The scheme is what recolours code, and it is also what a note's own tags sit on.
        sync_scheme(&self.buffer);
        match self.flavour {
            Flavour::Note => {
                highlight::restyle(&self.buffer, &self.view);
                highlight::hang(&self.buffer, &self.view);
            }
            // The column hues are rotated from the accent, which the theme can change under us.
            Flavour::Csv => highlight::restyle_csv(&self.buffer),
            Flavour::Code => {}
        }
    }

    /// Raise the banner for `alert`, which decides both what it says and what its button does.
    pub fn show_alert(&self, alert: Alert) {
        self.alert.set(Some(alert));
        self.banner.set_title(alert.title());
        self.banner.set_button_label(alert.button());
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

    /// Take down a banner about the file on disk, and only that. A save or a reload answers
    /// "changed on disk" and "deleted on disk"; it says nothing about a conflict copy sitting
    /// next to the note, whose banner has to survive the first autosave.
    pub fn clear_disk_alert(&self) {
        if matches!(self.alert.get(), Some(Alert::Compare | Alert::Restore)) {
            self.hide_banner();
        }
    }

    /// The user chose to lose this buffer's unsaved edits: it stops counting as dirty, so nothing
    /// downstream tries to save it on the way out.
    pub fn discard(&self) {
        self.modified.set(false);
        self.disk_changed.set(false);
        self.page.set_title(&self.tab_title());
        self.clear_disk_alert();
    }

    /// 1-based, the way an editor counts lines and the preview's `data-line` markers do.
    pub fn cursor_line(&self) -> u32 {
        let line = self.buffer.iter_at_mark(&self.buffer.get_insert()).line();
        line.max(0) as u32 + 1
    }

    fn tab_title(&self) -> String {
        let rel = self.rel();
        let name = tab_name(&rel, self.flavour);
        match self.modified.get() {
            true => format!("• {name}"),
            false => name.to_string(),
        }
    }

    // --- preferences ---------------------------------------------------------------------

    pub fn set_spellcheck(&self, on: bool) {
        // Prose only. A checker over identifiers and keywords is a wall of red squiggles.
        if !self.flavour.is_note() {
            return;
        }
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
        self.set_page(zoom);
        let Some(display) = gdk::Display::default() else {
            return;
        };
        if let Some(old) = self.font.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        // At the default zoom and with no font of its own a tab needs no provider at all: the
        // display-wide document font rule already says exactly the right thing. Zooming has to
        // name a font anyway, because CSS has no way to scale a size it cannot see.
        let name = match self.flavour {
            Flavour::Note => match font.filter(|f| !f.is_empty()) {
                Some(font) => Some(font.to_string()),
                None if zoom != 1.0 => Some(default_font()),
                None => None,
            },
            // Code names its font every time: the display-wide rule installed for prose is the
            // GNOME *document* font, and a source file wants the monospace one instead.
            _ => Some(
                adw::StyleManager::default()
                    .monospace_font_name()
                    .to_string(),
            ),
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
        // Only a note has markers hanging in the gutter to re-measure.
        if self.flavour.is_note() {
            self.rehang();
        }
    }

    /// Scale the page with the text. Zoom used to touch the font alone, so a zoomed-in column
    /// held fewer characters between gutters that stayed 48 px wide, and the heading markers,
    /// which `highlight::hang` measures against the left margin, ran out of gutter to hang in the
    /// way h5 and h6 already do. Scaling the gutters and the clamp together with the font keeps
    /// the page proportional, so zooming reads as moving closer rather than as a narrower column.
    fn set_page(&self, zoom: f64) {
        let scale = |base: i32| (f64::from(base) * zoom).round() as i32;
        self.zoom.set(zoom);
        self.view.set_left_margin(scale(GUTTER));
        self.view.set_right_margin(scale(GUTTER));
        self.view.set_top_margin(scale(TOP));
        self.view.set_bottom_margin(scale(BOTTOM));
        self.set_clamp();
    }

    /// Cap the column at its share of the editor's current width. Called on every resize as well
    /// as on a zoom or a preference change, because the share is of a width nothing reports until
    /// the window has been laid out.
    fn set_clamp(&self) {
        // Code fills the width: a capped column is a prose idea, and an indented block read
        // through a 70-character window is worse than a horizontal scrollbar.
        if !self.flavour.is_note() {
            self.clamp.set_maximum_size(i32::MAX);
            self.clamp.set_tightening_threshold(i32::MAX);
            return;
        }
        let available = self.scroller.hadjustment().page_size().round() as i32;
        let max = column_max(available, self.column.get(), self.zoom.get());
        self.clamp.set_maximum_size(max);
        // The 3:4 the fixed clamp had (600 of 800): under it the child simply takes the width it
        // is given, so a window too narrow for the cap loses no text to the gutters.
        self.clamp.set_tightening_threshold(max * 3 / 4);
    }

    /// The document column as a percentage of the editor's width, from preferences.
    pub fn set_column_width(&self, percent: u32) {
        self.column.set(percent);
        self.set_clamp();
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

    /// Numbers in the left gutter, outside the 48 px page gutter the heading markers hang in.
    pub fn set_line_numbers(&self, on: bool) {
        // The preference is about prose, where a number beside every line is clutter. Code is
        // read by line number — a compiler error names one — so it always has them.
        self.numbers.set_visible(on || !self.flavour.is_note());
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

    /// Comment or uncomment the selected lines with the language's own markers.
    ///
    /// GtkSourceView carries the markers in the language's metadata but does no toggling of its
    /// own, so the text goes out to [`crate::comment`] and comes back as one replacement, inside
    /// a single user action so one Ctrl+Z undoes the whole thing.
    pub fn toggle_comment(&self) {
        let Some(language) = self.buffer.language() else {
            return;
        };
        let had_selection = self.buffer.has_selection();
        let (mut start, mut end) = match self.buffer.selection_bounds() {
            Some(bounds) => bounds,
            None => {
                let at = self.buffer.iter_at_mark(&self.buffer.get_insert());
                (at, at)
            }
        };
        // Whole lines: a marker goes in front of a line, never in front of a word.
        start.set_line_offset(0);
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let text = self.buffer.text(&start, &end, true);
        let toggled = match language.metadata("line-comment-start") {
            Some(marker) => comment::toggle_lines(&text, &marker),
            None => {
                let (Some(open), Some(close)) = (
                    language.metadata("block-comment-start"),
                    language.metadata("block-comment-end"),
                ) else {
                    return;
                };
                comment::toggle_block(&text, &open, &close)
            }
        };
        let anchor = start.offset();
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.insert(&mut start, &toggled);
        self.buffer.end_user_action();
        if had_selection {
            self.buffer
                .select_range(&self.buffer.iter_at_offset(anchor), &start);
        }
    }

    /// Wrap long lines, or stop. Prose starts wrapped and code does not; either can be told
    /// otherwise for as long as the tab is open.
    pub fn toggle_wrap(&self) {
        self.view.set_wrap_mode(match self.view.wrap_mode() {
            gtk::WrapMode::None => gtk::WrapMode::WordChar,
            _ => gtk::WrapMode::None,
        });
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

    // --- find, replace and go to line ------------------------------------------------------
    //
    // The widgets live in `find.rs`, one bar per window. What stays here is what belongs to one
    // buffer: its `SearchContext`, and the caret and scroll moves that follow a match.

    /// The context the window's find bar drives, so it can watch the occurrence count.
    pub fn search_context(&self) -> &sourceview5::SearchContext {
        &self.context
    }

    pub fn set_query(&self, text: &str) {
        self.context.settings().set_search_text(Some(text));
    }

    pub fn set_highlight(&self, on: bool) {
        self.context.set_highlight(on);
    }

    /// A one-line selection, which is what the find bar prefills itself from.
    pub fn selected_query(&self) -> Option<String> {
        let (s, e) = self.buffer.selection_bounds()?;
        let selected = self.buffer.text(&s, &e, false).to_string();
        (!selected.is_empty() && !selected.contains('\n')).then_some(selected)
    }

    /// Move to the next or previous match. `from_current` searches from the start of the current
    /// selection, so growing the query keeps the match the user is looking at.
    pub fn step(&self, forward: bool, from_current: bool) {
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
    }

    pub fn replace_current(&self, with: &str) {
        if let Some((mut s, mut e)) = self.buffer.selection_bounds() {
            // Fails when the selection is not itself a match, which is the "nothing to do" case.
            let _ = self.context.replace(&mut s, &mut e, with);
        }
        self.step(true, false);
    }

    /// sourceview5 0.11 exposes no `replace_all` binding, so this walks the matches. Each pass
    /// resumes after the text just inserted, so a replacement containing the query terminates.
    pub fn replace_all(&self, with: &str) {
        let mut from = self.buffer.start_iter();
        self.buffer.begin_user_action();
        while let Some((mut s, mut e, _)) = self.context.forward(&from) {
            if self.context.replace(&mut s, &mut e, with).is_err() {
                break;
            }
            from = e;
        }
        self.buffer.end_user_action();
    }

    /// "n of m", the way every find bar says it. Blank while GtkSourceView is still counting.
    pub fn matches_label(&self) -> String {
        let count = self.context.occurrences_count();
        let blank = self
            .context
            .settings()
            .search_text()
            .is_none_or(|t| t.is_empty());
        match (blank, count) {
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
        }
    }

    pub fn line_count(&self) -> i32 {
        self.buffer.line_count()
    }

    /// Put the caret on a 1-based line and column, both clamped to what the note has.
    pub fn goto_line(&self, line: i32, column: i32) {
        let iter = self.line_iter(line, column);
        self.buffer.place_cursor(&iter);
        self.view
            .scroll_to_mark(&self.buffer.get_insert(), 0.0, true, 0.0, 0.25);
        self.view.grab_focus();
    }

    /// Scroll a line into view without moving the caret: what the go-to entry previews while the
    /// number is still being typed.
    pub fn show_line(&self, line: i32) {
        let mut iter = self.line_iter(line, 1);
        self.view.scroll_to_iter(&mut iter, 0.0, true, 0.0, 0.25);
    }

    fn line_iter(&self, line: i32, column: i32) -> gtk::TextIter {
        let line = (line - 1).clamp(0, (self.buffer.line_count() - 1).max(0));
        let mut iter = self
            .buffer
            .iter_at_line(line)
            .unwrap_or_else(|| self.buffer.end_iter());
        let mut end = iter;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        iter.set_line_offset((column - 1).clamp(0, end.line_offset()));
        iter
    }

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
        if self.buffer.char_count() <= INSTANT {
            self.reanalyse();
        } else {
            let id = glib::timeout_add_local_once(
                DEBOUNCE,
                glib::clone!(
                    #[weak(rename_to = tab)]
                    self,
                    move || {
                        *tab.debounce.borrow_mut() = None;
                        tab.reanalyse();
                    }
                ),
            );
            *self.debounce.borrow_mut() = Some(id);
        }
        self.schedule_autosave();
    }

    /// Re-read the buffer once and refresh everything derived from it: the styling tags, and the
    /// link table that Ctrl+click and Ctrl+Return follow. The preview listens on `on_edited` and
    /// debounces its own re-render, so calling this per keystroke only re-arms that timer.
    fn reanalyse(self: &Rc<Self>) {
        self.analyse();
        self.emit(&self.on_edited);
    }

    /// Re-derive whatever this tab's text implies. A note gets its styling spans and its link
    /// table; code gets nothing, because the style scheme colours it from the language.
    fn analyse(&self) {
        match self.flavour {
            Flavour::Note => {
                let analysis = highlight::apply(&self.buffer);
                *self.links.borrow_mut() = analysis.links;
                *self.headings.borrow_mut() = analysis.headings;
            }
            Flavour::Csv => highlight::apply_csv(&self.buffer),
            // Code is coloured by its language through the style scheme, with nothing to derive.
            Flavour::Code => {}
        }
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
    fn column_max_is_a_share_of_the_editor_with_a_floor_under_it() {
        // The user's maximised window: 1920 less the sidebar, so the default lands within a
        // couple of dozen pixels of the 800 px the fixed clamp used to give it.
        assert_eq!(column_max(1639, 50, 1.0), 820);
        assert_eq!(column_max(1639, 100, 1.0), 1639, "all of it is allowed");
        assert_eq!(
            column_max(700, 50, 1.0),
            COLUMN_FLOOR,
            "a narrow editor keeps a readable line instead of a sliver"
        );
        assert_eq!(column_max(1639, 50, 2.0), 1639, "the zoom scales the page");
    }

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

    /// The gutter is as wide as the longest number it will ever print, and never zero wide.
    #[test]
    fn gutter_width_follows_the_line_count() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(1), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(1000), 4);
    }

    #[test]
    fn a_duplicated_line_brings_its_own_newline_only_when_it_has_none() {
        assert_eq!(duplicated("note\n"), "note\n");
        assert_eq!(duplicated("last line"), "\nlast line");
        assert_eq!(duplicated(""), "\n");
    }
}
