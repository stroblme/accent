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

use crate::{comment, diagnostics, fold, highlight, lang, multicaret, typing};
use accent_api::{Diagnostic, Fold, Pos};
use accent_core::fs::{self, Etag};
use accent_core::markdown::Link;
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;
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

/// Which of the standing questions the one banner shows.
///
/// A tab has one `AdwBanner` and can have more than one thing to say about its file, so they
/// queue instead of overwriting each other: a conflict copy appearing used to wipe the "changed
/// on disk" question, and resolving that copy then took the bar down with the wiped question
/// still standing — a dirty tab that would never autosave again, with nothing on screen to say
/// why. The order is what each one can cost: the two that mean this buffer holds the only copy
/// of something come first, the conflict copy beside the note next (it blocks nothing), and the
/// read-only report last, because it asks nothing at all.
///
/// Queued rather than merged: `AdwBanner` has exactly one button, and two questions on one line
/// have no honest single label.
fn banner_alert(standing: &[Alert]) -> Option<Alert> {
    [
        Alert::Restore,
        Alert::Compare,
        Alert::Conflict,
        Alert::ReadOnly,
    ]
    .into_iter()
    .find(|a| standing.contains(a))
}

/// Whether a save may write the file under a tab.
///
/// The one rule every save path shares. A buffer whose file moved underneath it holds the only
/// copy of its edits *and* the answer to a question the banner is still asking, so nothing
/// writes until that answer is given — which is the rule VS Code follows for the same reason.
/// A clean buffer has nothing to lose and is let through: that is how the "deleted on disk"
/// banner writes the note back.
///
/// The etag gate in `fs::write_note` is still the last word; this is what keeps a save from
/// being attempted at all once the tab already knows the answer.
pub fn may_save(modified: bool, disk_changed: bool) -> bool {
    !(modified && disk_changed)
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
    /// The committed text this buffer is drawn against, and the bars in the gutter that say how
    /// it differs. `None` when the file is in no repository, or is not tracked in one.
    head: RefCell<Option<String>>,
    marks: crate::marks::Renderer,
    /// Kept for [`Tab::scroll_lines`] and for the scrollbar the minimap replaces.
    scroller: gtk::ScrolledWindow,
    /// The width cap on the document column, sized by [`Tab::set_clamp`]. Scrollable, so the
    /// view below it is the scrolled window's own scrollable child rather than a viewport's.
    clamp: adw::ClampScrollable,
    /// What the cap is computed from: the document zoom and the column's percentage of the
    /// editor's width. Kept here because the editor is also resized from the outside, and a
    /// resize has to recompute the cap without being told the other two again.
    zoom: Cell<f64>,
    column: Cell<u32>,
    map: sourceview5::Map,
    /// The optional line-number gutter; hidden unless the preference turns it on.
    numbers: sourceview5::GutterRendererText,
    /// The sticky block title over the top of the view, and the bar it sits on. Hidden until
    /// something is scrolled out of sight above the first visible line.
    sticky: gtk::Label,
    sticky_bar: gtk::Box,
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
    /// Every question standing about this file. The banner shows one of them ([`banner_alert`]);
    /// the rest wait rather than being overwritten.
    alerts: RefCell<Vec<Alert>>,
    context: sourceview5::SearchContext,
    spell: RefCell<Option<libspelling::TextBufferAdapter>>,
    links: RefCell<Vec<Link>>,
    /// What the language server last said about this file, and the provider that shows the loud
    /// half of it at the ends of the lines. Kept because the gutter tooltip and the status bar
    /// both read it back after the paint.
    diagnostics: RefCell<Vec<Diagnostic>>,
    annotations: sourceview5::AnnotationProvider,
    /// The blocks the server says can be hidden, and the chevrons beside their headers. What is
    /// hidden right now lives in the buffer's own tag, not here.
    folds: RefCell<Vec<Fold>>,
    fold_renderer: fold::Renderer,
    font: RefCell<Option<gtk::CssProvider>>,
    /// A watch on the file itself, for a tab no vault watcher covers. `None` for everything
    /// inside a vault, which the worker already reports on.
    monitor: RefCell<Option<gio::FileMonitor>>,
    /// Set while we replace the buffer text ourselves, so `changed` does not mark it dirty.
    loading: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
    autosave: RefCell<Option<glib::SourceId>>,
    cursor: RefCell<Option<glib::SourceId>>,
    on_autosave: Hook,
    on_edited: Hook,
    on_banner: Hook,
    on_cursor: Hook,
    on_follow: Hook,
    /// This tab's document on the vault's language layer: what it can answer, what it last
    /// answered, and the refresh that is still in flight. Empty for a tab outside every vault.
    pub lang: lang::State,
}

/// Open `key` in a new tab of `tabs`, with `text` already read from disk.
///
/// The bytes are read by the caller rather than here, because deciding what a file is — text,
/// binary, too large — is what picks the kind of tab in the first place, and by the time we are
/// called that question is settled.
///
/// `key` is vault-relative, or absolute for a file from outside the vault; `root` is the vault's
/// and is only used to build the path and the tooltip, so an absolute key simply ignores it.
pub fn open(
    root: &Path,
    key: &str,
    text: fs::Text,
    flavour: Flavour,
    tabs: &adw::TabView,
    prefs: &Prefs,
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
    // Every flavour: a note gets diagnostics too (a dangling wikilink is one), and folds its
    // sections as a source file folds its functions.
    diagnostics::install_tags(&buffer);
    fold::install_tag(&buffer);
    buffer.set_text(&text.text);
    // `set_text` leaves the insert mark where the text ended, so a note opened without one — every
    // note but a search hit or a template's `{{cursor}}` — had its caret on the last line while the
    // view sat at the top. Invisible in the editor, but the preview follows the caret, so a note
    // opened in Split view opened at its end.
    buffer.place_cursor(&buffer.start_iter());
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
    // Everything wraps. A note wraps because a line is a paragraph, and code wraps because the
    // alternative is a document that scrolls sideways: the far end of a long line is then off the
    // screen and out of the way of the eye, which is worse than a folded line. `Alt+Z` unwraps
    // the odd generated file where the columns really do mean something.
    view.set_wrap_mode(gtk::WrapMode::WordChar);
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
    // Between the numbers and the text: a change bar belongs next to the line it is about.
    let marks = crate::marks::Renderer::new();
    marks.set_visible(false);
    sourceview5::prelude::ViewExt::gutter(&view, gtk::TextWindowType::Left).insert(&marks, 1);
    // The diagnostic gutter and the messages at the ends of the lines. A note's own diagnostics
    // are hints, which draw neither, so it keeps a clean margin.
    view.set_show_line_marks(!flavour.is_note());
    let annotations = sourceview5::AnnotationProvider::new();
    view.annotations().add_provider(&annotations);
    // Outside the change bars, next to the text: a chevron is about the block it opens.
    let folds = crate::fold::Renderer::new();
    sourceview5::prelude::ViewExt::gutter(&view, gtk::TextWindowType::Left).insert(&folds, 2);
    // Whole-line cut and copy, whatever the tab holds: an editor where Ctrl+X on no selection
    // does nothing is one that makes the user select the line first.
    line_clipboard(&view);
    // Markdown behaviour: continuing a list or a fence on Return, and closing a bracket as it is
    // typed. In a Python file both would be wrong rather than merely unused. Completion is not
    // here: every flavour has it now, and `lang::attach` installs it once there is a vault to ask.
    if flavour.is_note() {
        typing::install(&view);
    }

    // The clamp caps the line, the view's own margins keep it off the edge, and on a narrow
    // window the clamp simply stops applying. Its maximum is a share of the editor's own width
    // (`Config::column_width`), which `set_clamp` puts here as soon as that width is known.
    //
    // `AdwClampScrollable` and not `AdwClamp`, because only the scrollable one lets the view
    // through to the scrolled window: with a plain clamp GTK inserts a `GtkViewport`, the view's
    // adjustments are then throwaway ones nothing reads, and every `scroll_to_mark` — GTK's own
    // caret following included — writes to a dead adjustment while the viewport scrolls to the
    // focused widget instead (`GtkViewport:scroll-to-focus`, on by default), which is what put a
    // scrolled note back at the top on any focus change.
    let clamp = adw::ClampScrollable::builder().child(&view).build();

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&clamp)
        .build();

    // How the column learns the editor's width. The view being the scrollable child, the
    // horizontal adjustment now reports the *column's* width rather than the scroller's, so
    // feeding it back into `set_clamp` would collapse the column to its floor and then go quiet.
    // GTK 4 has no signal for "my width changed" — `::size-allocate` is gone and `GtkWidget` has
    // no width property — and `GtkDrawingArea::resize` is the one public signal that fires on
    // every allocation, so a zero-sized one laid over the scroller is what reports it. It draws
    // nothing, takes no input and is invisible to assistive technology.
    let width = gtk::DrawingArea::builder()
        .can_target(false)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    overlay.add_overlay(&width);

    // The sticky block title, pinned over the top of the view. The label carries the document
    // font by name and by class, so it follows both the display-wide rule and this tab's own
    // zoom; the box under it paints the view's own background, which is what the scrolled text
    // has to disappear behind.
    let sticky = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .single_line_mode(true)
        .margin_top(2)
        .margin_bottom(2)
        .margin_end(GUTTER)
        .build();
    sticky.add_css_class("accent-doc");
    sticky.set_widget_name(&view.widget_name());
    let sticky_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Start)
        .can_target(false)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    sticky_bar.add_css_class("view");
    sticky_bar.append(&sticky);
    // A rule under it, or the pinned line reads as a line of the note that the one below it has
    // been scrolled halfway behind.
    sticky_bar.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    overlay.add_overlay(&sticky_bar);

    // The minimap is off unless the preference says otherwise; `set_minimap` decides that, so a
    // tab that is built before the config is read still starts in a defined state.
    let map = sourceview5::Map::new();
    map.set_view(&view);
    map.set_vexpand(true);
    map.set_visible(false);
    let document = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    document.append(&overlay);
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
        sticky: sticky.clone(),
        sticky_bar: sticky_bar.clone(),
        head: RefCell::new(None),
        marks: marks.clone(),
        flavour,
        crlf: Cell::new(text.crlf),
        lossy: Cell::new(text.lossy),
        page,
        banner: banner.clone(),
        etag: Cell::new(Some(text.etag)),
        modified: Cell::new(false),
        disk_changed: Cell::new(false),
        alerts: RefCell::new(Vec::new()),
        context,
        spell: RefCell::new(None),
        links: RefCell::new(Vec::new()),
        diagnostics: RefCell::new(Vec::new()),
        annotations,
        folds: RefCell::new(Vec::new()),
        fold_renderer: folds.clone(),
        font: RefCell::new(None),
        monitor: RefCell::new(None),
        loading: Cell::new(false),
        debounce: RefCell::new(None),
        autosave: RefCell::new(None),
        cursor: RefCell::new(None),
        on_autosave: RefCell::new(None),
        on_edited: RefCell::new(None),
        on_banner: RefCell::new(None),
        on_cursor: RefCell::new(None),
        on_follow: RefCell::new(None),
        lang: lang::State::default(),
    });
    tab.set_font(prefs.font.as_deref(), zoom);
    tab.set_spellcheck(prefs.spellcheck);
    tab.set_minimap(prefs.minimap);
    tab.set_line_numbers(prefs.line_numbers);
    if text.lossy {
        tab.show_alert(Alert::ReadOnly);
    }

    // The two gutter icons, and what they say when the pointer rests on one. Installed here
    // rather than above because the tooltip reads the tab's own diagnostics back.
    for (category, icon) in [
        (diagnostics::MARK_ERROR, "dialog-error-symbolic"),
        (diagnostics::MARK_WARNING, "dialog-warning-symbolic"),
    ] {
        let attributes = sourceview5::MarkAttributes::builder()
            .icon_name(icon)
            .build();
        attributes.connect_query_tooltip_text(glib::clone!(
            #[weak(rename_to = tab)]
            tab,
            #[upgrade_or_default]
            move |_, mark| {
                let line = tab.buffer.iter_at_mark(mark).line().max(0) as u32;
                diagnostics::messages_on(&tab.diagnostics.borrow(), line)
            }
        ));
        // Above the git change bars, which have no icon and nothing to say.
        view.set_mark_attributes(category, &attributes, 2);
    }

    // The column is a share of the editor's width, so the cap has to be recomputed whenever that
    // width changes: a window resize, a paned drag, the sidebar, a split, the minimap. One hook
    // covers all of them, because the overlaid area is allocated the scroller's own width.
    width.connect_resize(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_, _, _| {
            tab.set_clamp();
            tab.update_sticky();
        }
    ));

    // What the top of the view is inside changes on every scroll, and the widget the title has
    // to line up with moves with the clamp, so the bar is recomputed rather than positioned once.
    scroller.vadjustment().connect_value_changed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.update_sticky()
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
    // The change bars need the same resolved foreground, whatever the flavour: a source file in a
    // repository gets them exactly as a note does.
    marks.restyle(&view);
    view.connect_map(glib::clone!(
        #[strong]
        marks,
        move |view| marks.restyle(view)
    ));
    // Same resolved foreground, same reason: the underlines and the chevrons are mixed with it.
    diagnostics::restyle(&buffer, &view);
    folds.restyle(&view);
    view.connect_map(glib::clone!(
        #[strong]
        buffer,
        #[strong]
        folds,
        move |view| {
            diagnostics::restyle(&buffer, view);
            folds.restyle(view);
        }
    ));
    folds.connect_toggle(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |line| tab.toggle_fold(line)
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
            // The caret goes where the pointer is first: Go to Definition asks about the caret,
            // and a Ctrl+click means "this one", not "wherever I last typed".
            let (bx, by) =
                tab.view
                    .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            if let Some(iter) = tab.view.iter_at_location(bx, by) {
                tab.buffer.place_cursor(&iter);
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
            tab.emit(&tab.on_follow);
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
        move |renderer, lines, line| {
            // A line hidden inside a fold still reaches here and is laid out with no height, so
            // its number would be painted on top of the header's. Nothing is the right number.
            // The signal hands the lines over as a plain `GObject`, hence the cast.
            if let Some(lines) = lines.downcast_ref::<sourceview5::GutterLines>() {
                let mode = sourceview5::GutterRendererAlignmentMode::Cell;
                if lines.line_yrange(line, mode).1 <= 0 {
                    renderer.set_text("");
                    return;
                }
            }
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

/// The spaces and tabs a line opens with, which is what a line inserted below it copies.
fn leading_indent(line: &str) -> &str {
    let end = line
        .find(|c: char| c != ' ' && c != '\t')
        .unwrap_or(line.len());
    &line[..end]
}

/// The text a duplicated line is inserted as. A line that already ends in a newline can be
/// repeated as it stands; the last line of a file has none, so the copy brings its own.
fn duplicated(line: &str) -> String {
    match line.ends_with('\n') {
        true => line.to_string(),
        false => format!("\n{line}"),
    }
}

/// Which opener a sticky block title shows for the line at the top of the view: the innermost of
/// the two candidates, and none at all where the only one is the top line itself, which the
/// reader can already see.
fn sticky_opener(top: i32, heading: Option<i32>, fence: Option<i32>) -> Option<i32> {
    [heading, fence]
        .into_iter()
        .flatten()
        .filter(|line| *line < top)
        .max()
}

/// A line as the clipboard should carry it: with the newline back that a last line does not have
/// of its own, so pasting it opens a line rather than splicing into the one under the caret.
fn paste_ready(line: &str) -> String {
    match line.ends_with('\n') {
        true => line.to_string(),
        false => format!("{line}\n"),
    }
}

/// The caret's line, from its start to the start of the next one, so the trailing newline is part
/// of it except on a last line that has none.
fn caret_line(buffer: &gtk::TextBuffer) -> (gtk::TextIter, gtk::TextIter) {
    let mut start = buffer.iter_at_mark(&buffer.get_insert());
    start.set_line_offset(0);
    let mut end = start;
    // On the last line this lands on the end of the buffer and reports failure, which is
    // exactly where the line ends, so the answer is the same either way.
    end.forward_line();
    (start, end)
}

/// VS Code's whole-line cut and copy: with nothing selected, `Ctrl+X` and `Ctrl+C` take the
/// caret's whole line, its newline with it, so a later paste puts a line back instead of a
/// fragment.
///
/// No key handling, and no accelerator either — DESIGN.md's never-bind list keeps `Ctrl+X`/`C`
/// for the widget. Both chords emit these two signals, and `gtk_text_buffer_cut_clipboard` and
/// `..._copy_clipboard` do nothing at all without a selection, so each handler runs before an
/// inherited one that is then a no-op and does the work itself. Selecting the line and letting
/// the default handler have it instead would work for the cut and leave the copy selected.
///
/// After a cut the caret is where the deletion left it, at the start of the following line;
/// VS Code lands on the same line but keeps the column.
fn line_clipboard(view: &sourceview5::View) {
    view.connect_copy_clipboard(|view| {
        let buffer = view.buffer();
        if buffer.has_selection() {
            return;
        }
        let (start, end) = caret_line(&buffer);
        view.clipboard()
            .set_text(&paste_ready(&buffer.text(&start, &end, true)));
    });
    view.connect_cut_clipboard(|view| {
        let buffer = view.buffer();
        if buffer.has_selection() || !view.is_editable() {
            return;
        }
        let (mut start, mut end) = caret_line(&buffer);
        let line = buffer.text(&start, &end, true);
        view.clipboard().set_text(&paste_ready(&line));
        // A last line with no newline of its own takes the one above it, or the cut leaves the
        // blank line it used to sit on. `delete_line` does the same.
        if !line.ends_with('\n') {
            start.backward_char();
        }
        buffer.begin_user_action();
        buffer.delete(&mut start, &mut end);
        buffer.end_user_action();
    });
}

/// Family and point size of a font description, with our own defaults where it is silent, scaled
/// by `zoom`. Rounded to two decimals so stepping the zoom does not write `12.100000000000001pt`.
///
/// The one place either is decided. `main::install_document_font` writes the display-wide rule
/// through this too, at zoom 1.0: the extraction used to be written out a second time there with a
/// different fallback family, so a description with no family of its own would have produced two
/// different faces.
pub(crate) fn font_css(name: &str, selector: &str, zoom: f64) -> String {
    let desc = pango::FontDescription::from_string(name);
    let family = desc
        .family()
        .map(|f| f.to_string())
        .unwrap_or_else(|| DEFAULT_FAMILY.to_string());
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

    /// Point the gutter's change bars at the committed text, or take them away with `None`. The
    /// caller is the Git pane, which is the only thing in the window that has asked git anything.
    pub fn set_head(&self, head: Option<String>) {
        let showing = head.is_some();
        *self.head.borrow_mut() = head;
        self.marks.set_visible(showing);
        match showing {
            true => self.update_marks(),
            false => self.marks.set_marks(Vec::new()),
        }
    }

    /// The GtkSourceView language this tab was given, by its display name ("Rust", "Makefile").
    /// `None` for a file no language claimed, which the status bar calls plain text.
    pub fn language(&self) -> Option<String> {
        self.buffer.language().map(|l| l.name().to_string())
    }

    /// Watch the file behind this tab and call `f` when someone else writes it.
    ///
    /// Only for a tab outside every vault: inside one, the vault's own watcher reports the change
    /// and knows which writes were ours, which a bare file monitor cannot.
    pub fn watch_file(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
        let file = gio::File::for_path(self.path());
        let Ok(monitor) = file.monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        monitor.connect_changed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, _, _, event| {
                // `ChangesDoneHint` is the settled write; `Created` is the rename an atomic save
                // lands as, ours included, which the etag check then makes a no-op.
                if matches!(
                    event,
                    gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created
                ) {
                    f(&tab);
                }
            }
        ));
        *self.monitor.borrow_mut() = Some(monitor);
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
    pub fn retarget(self: &Rc<Self>, root: &Path, new_rel: &str) {
        let old_rel = self.rel();
        *self.rel.borrow_mut() = new_rel.to_string();
        *self.path.borrow_mut() = root.join(new_rel);
        self.page.set_title(&self.tab_title());
        self.page
            .set_tooltip(&crate::fileops::display_path(root, new_rel));
        // A language server keys its documents by URI and knows nothing of the move, so the old
        // path is closed and the new one opened.
        lang::retarget(self, &old_rel);
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
        // Where the page is, kept alongside the caret: the scroll position, and the line at the
        // top of the view with where that line sits in the buffer, so the same text goes back
        // under the same edge however far the reload moves it. Replacing the buffer empties it,
        // which drops the view to line one, and a scroll to the caret from there parks it
        // against whichever edge is nearer instead of putting the page back.
        let (top_iter, _) = self.view.line_at_y(self.view.visible_rect().y());
        let (top_line, top_y) = (top_iter.line(), self.view.line_yrange(&top_iter).0);
        let scrolled = self.view.vadjustment().map_or(0.0, |v| v.value());
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
        // One idle later, because the buffer has only just been replaced: a position measured
        // against lines the view has not laid out yet lands short of the line it was given.
        let (view, buffer) = (self.view.clone(), self.buffer.clone());
        glib::idle_add_local_once(move || {
            let iter = buffer
                .iter_at_line(top_line.min(buffer.line_count() - 1))
                .unwrap_or_else(|| buffer.end_iter());
            let moved = view.line_yrange(&iter).0 - top_y;
            if let Some(vadjustment) = view.vadjustment() {
                vadjustment.set_value(scrolled + f64::from(moved));
            }
        });
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
        self.marks.restyle(&self.view);
        diagnostics::restyle(&self.buffer, &self.view);
        self.fold_renderer.restyle(&self.view);
    }

    /// What the language server last said about this file. Replaces the previous answer whole,
    /// which is what a publish is; an empty list clears the tab.
    pub fn set_diagnostics(&self, items: Vec<Diagnostic>) {
        diagnostics::render(&self.buffer, &self.annotations, &items);
        *self.diagnostics.borrow_mut() = items;
    }

    /// What is painted now, for the status bar and the hover.
    pub fn diagnostics(&self) -> std::cell::Ref<'_, Vec<Diagnostic>> {
        self.diagnostics.borrow()
    }

    /// The buffer as a plain `GtkTextBuffer`, which is what `fold` works in: nothing it does
    /// needs GtkSourceView.
    fn text_buffer(&self) -> &gtk::TextBuffer {
        self.buffer.upcast_ref()
    }

    /// What the server says can be folded. Whatever is hidden stays hidden if its header survived
    /// the re-analysis, at wherever the line has moved to.
    pub fn set_folds(&self, folds: Vec<Fold>) {
        fold::resync(self.text_buffer(), &self.folds.borrow(), &folds);
        self.fold_renderer
            .set_starts(folds.iter().map(|f| f.start_line as i32).collect());
        *self.folds.borrow_mut() = folds;
    }

    /// Open or shut the block whose header is `line`. What the gutter chevron does.
    pub fn toggle_fold(&self, line: i32) {
        let known = self
            .folds
            .borrow()
            .iter()
            .any(|f| f.start_line as i32 == line);
        if !known {
            return;
        }
        match fold::is_folded(self.text_buffer(), line) {
            true => fold::unfold(self.text_buffer(), line),
            false => self.fold_line(line),
        }
        self.fold_renderer.queue_draw();
    }

    fn fold_line(&self, line: i32) {
        let found = self
            .folds
            .borrow()
            .iter()
            .find(|f| f.start_line as i32 == line)
            .copied();
        if let Some(f) = found {
            fold::fold(self.text_buffer(), f);
        }
    }

    fn caret_line(&self) -> i32 {
        self.buffer
            .iter_at_mark(&self.buffer.get_insert())
            .line()
            .max(0)
    }

    /// Fold the innermost block the caret is in.
    pub fn fold_at_caret(&self) {
        let found = fold::containing(&self.folds.borrow(), self.caret_line() as u32).copied();
        if let Some(f) = found {
            fold::fold(self.text_buffer(), f);
            self.fold_renderer.queue_draw();
        }
    }

    /// Open the block the caret is on, whether the caret is on its header or inside it.
    pub fn unfold_at_caret(&self) {
        let found = fold::containing(&self.folds.borrow(), self.caret_line() as u32).copied();
        if let Some(f) = found {
            fold::unfold(self.text_buffer(), f.start_line as i32);
            self.fold_renderer.queue_draw();
        }
    }

    pub fn fold_all(&self) {
        // Outermost first, so a nested block is already inside a hidden run and the caret only
        // has to be moved out once.
        let mut folds = self.folds.borrow().clone();
        folds.sort_by_key(|f| f.start_line);
        for f in folds {
            fold::fold(self.text_buffer(), f);
        }
        self.fold_renderer.queue_draw();
    }

    pub fn unfold_all(&self) {
        fold::unfold_all(self.text_buffer());
        self.fold_renderer.queue_draw();
    }

    /// Raise `alert`, which decides both what the banner says and what its button does. It goes
    /// on the queue: whichever standing question matters most is the one on screen.
    pub fn show_alert(&self, alert: Alert) {
        let mut standing = self.alerts.borrow_mut();
        if !standing.contains(&alert) {
            standing.push(alert);
        }
        drop(standing);
        self.render_banner();
    }

    /// Take one question down, leaving whatever else is standing. The banner comes back with the
    /// next one rather than going away.
    pub fn clear_alert(&self, alert: Alert) {
        self.alerts.borrow_mut().retain(|a| *a != alert);
        self.render_banner();
    }

    /// What the visible banner is asking for, for the handler of its button.
    pub fn alert(&self) -> Option<Alert> {
        banner_alert(&self.alerts.borrow())
    }

    pub fn hide_banner(&self) {
        self.alerts.borrow_mut().clear();
        self.render_banner();
    }

    fn render_banner(&self) {
        match self.alert() {
            Some(alert) => {
                self.banner.set_title(alert.title());
                self.banner.set_button_label(alert.button());
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
    }

    /// Take down the questions about the file on disk, and only those. A save or a reload answers
    /// "changed on disk" and "deleted on disk"; it says nothing about a conflict copy sitting
    /// next to the note, whose banner has to survive the first autosave.
    pub fn clear_disk_alert(&self) {
        self.alerts
            .borrow_mut()
            .retain(|a| !matches!(a, Alert::Compare | Alert::Restore));
        self.render_banner();
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
        // The scroller's own width, not the horizontal adjustment's page size: with the view as
        // the scrollable child that page size *is* the clamped column, so it would feed back.
        let available = self.scroller.width();
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

    /// Wrap long lines, or stop. Every tab starts wrapped and can be told otherwise for as long
    /// as it is open, which is the escape hatch for a file whose columns are the point.
    pub fn toggle_wrap(&self) {
        self.view.set_wrap_mode(match self.view.wrap_mode() {
            gtk::WrapMode::None => gtk::WrapMode::WordChar,
            _ => gtk::WrapMode::None,
        });
    }

    // --- line operations -----------------------------------------------------------------

    /// The caret's whole line; see [`caret_line`], which the clipboard handlers share.
    fn line_bounds(&self) -> (gtk::TextIter, gtk::TextIter) {
        caret_line(self.buffer.upcast_ref())
    }

    /// VS Code's Insert Line Below: open a line under the caret's and put the caret on it, at the
    /// same indent, so a list item or an indented block carries on where it was. That is the idiom
    /// `typing.rs` already uses on Return; continuing the marker itself is Return's job, not this
    /// one's, because this is also how a line is opened *out* of a list.
    pub fn newline_below(&self) {
        let (start, end) = self.line_bounds();
        let line = self.buffer.text(&start, &end, true);
        let indent = leading_indent(&line).to_string();
        // Insert before the line's own newline, or at the end of the buffer on a last line that
        // has none. One user action, so one Ctrl+Z takes the whole line back.
        let mut at = end;
        if line.ends_with('\n') {
            at.backward_char();
        }
        self.buffer.begin_user_action();
        self.buffer.insert(&mut at, &format!("\n{indent}"));
        self.buffer.end_user_action();
        self.buffer.place_cursor(&at);
        self.view.scroll_mark_onscreen(&self.buffer.get_insert());
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
    ///
    /// `line_at_y` and not `iter_at_location`: the latter answers with whether the position is
    /// *over text*, and buffer x 0 is the page gutter at every scroll position, so it returned
    /// nothing and this scrolled by nothing. `line_at_y` takes the y alone and clamps, which also
    /// covers the top of the document, where y is the negative of the top margin.
    pub fn scroll_lines(&self, n: i32) {
        let (first, _) = self.view.line_at_y(self.view.visible_rect().y());
        // The display line's height and not the paragraph's: `iter_location` measures the caret
        // at that position, so a wrapped line still steps one screen row at a time.
        let height = self.view.iter_location(&first).height();
        if height <= 0 {
            return;
        }
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value(adjustment.value() + f64::from(n * height));
    }

    /// The note's own answer to what the top of the view is inside: the nearest heading or the
    /// fence the reader is inside, whichever is lower down.
    fn sticky_note_line(&self, first: gtk::TextIter, top: i32) -> Option<i32> {
        // From the *end* of the top line, so a heading or a fence opening on that line is found
        // and then discarded by `sticky_opener` for being on screen already, rather than passed
        // over in favour of the one above it.
        let mut from = first;
        from.forward_to_line_end();
        let table = self.buffer.tag_table();
        let previous = |name: &str| {
            let tag = table.lookup(name)?;
            let mut at = from;
            at.backward_to_tag_toggle(Some(&tag)).then(|| at.line())
        };
        let heading = ["h1", "h2", "h3", "h4", "h5", "h6"]
            .iter()
            .filter_map(|name| previous(name))
            .max();
        let fence = table.lookup("codeblock").and_then(|tag| {
            // Only from inside the block: below it the nearest toggle is its closing one, which
            // is a block the reader has already left.
            let mut at = from;
            if !at.has_tag(&tag) || !at.backward_to_tag_toggle(Some(&tag)) {
                return None;
            }
            at.starts_tag(Some(&tag)).then(|| at.line())
        });
        sticky_opener(top, heading, fence)
    }

    /// Pin the opening line of whatever block the top of the view is inside above the view, or
    /// take it away again. VS Code's sticky scroll, and it answers the same question: what is
    /// this, now that its first line has gone off the top.
    ///
    /// In a note a block is a markdown heading or a fenced code block, which is exactly what the
    /// styling pass has already marked on the buffer — so the answer is two tag-toggle searches
    /// through the buffer's own index rather than a second parse or a walk back through the
    /// lines. In a source file it is the innermost symbol the language server named, which is the
    /// same question asked of the only thing that knows a language's structure. A CSV has no
    /// blocks at all.
    pub fn update_sticky(&self) {
        if self.flavour == Flavour::Csv {
            return;
        }
        // Before the view is allocated its visible rect is empty and `line_at_y` answers with
        // whatever line the layout happens to be at, which pinned a heading over a note that was
        // at its very top. The resize hook recomputes once there is a height.
        if self.view.height() == 0 {
            self.sticky_bar.set_visible(false);
            return;
        }
        let (first, _) = self.view.line_at_y(self.view.visible_rect().y());
        let top = first.line();
        let line = match self.flavour {
            Flavour::Note => self.sticky_note_line(first, top),
            Flavour::Code => {
                lang::innermost(&self.lang.symbols(), top.max(0) as u32).map(|line| line as i32)
            }
            Flavour::Csv => None,
        };
        let Some(line) = line else {
            self.sticky_bar.set_visible(false);
            return;
        };
        let Some(start) = self.buffer.iter_at_line(line) else {
            return;
        };
        let mut end = start;
        end.forward_to_line_end();
        self.sticky
            .set_text(self.buffer.text(&start, &end, false).trim_end());
        // The clamp centres the view in the scroller, so where the text column starts is not
        // something the bar can be told once. The page gutter goes on top of it.
        let origin = graphene::Point::zero();
        let left = self
            .view
            .compute_point(&self.sticky_bar, &origin)
            .map_or(0, |point| point.x() as i32);
        self.sticky
            .set_margin_start((left + self.view.left_margin()).max(0));
        self.sticky_bar.set_visible(true);
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

    /// The selection as the sidebar search takes it. Its first line only: the box is one line
    /// high, and a whole paragraph pasted into it matches nothing anyway.
    pub fn selected_search(&self) -> Option<String> {
        let (s, e) = self.buffer.selection_bounds()?;
        let selected = self.buffer.text(&s, &e, false).to_string();
        let first = selected.lines().next().unwrap_or_default().to_string();
        (!first.is_empty()).then_some(first)
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
            // A match inside a folded block opens it, or the selection is invisible.
            fold::reveal(self.text_buffer(), &s);
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

    /// The one way the caret is sent somewhere: open whatever fold is hiding the destination,
    /// put the caret there, scroll it to `align` down the view and take the focus.
    ///
    /// Every jump goes through here — an outline row, a search hit, a go-to line, a definition —
    /// so none of them can land inside a folded block and leave the window looking unchanged.
    pub fn jump_to(&self, iter: &gtk::TextIter, align: f64) {
        fold::reveal(self.text_buffer(), iter);
        self.buffer.place_cursor(iter);
        self.view
            .scroll_to_mark(&self.buffer.get_insert(), 0.0, true, 0.0, align);
        self.view.grab_focus();
    }

    /// Put the caret on a 1-based line and column, both clamped to what the note has.
    pub fn goto_line(&self, line: i32, column: i32) {
        self.jump_to(&self.line_iter(line, column), 0.25);
    }

    /// Put the caret at a server position, which is zero-based and counts characters.
    pub fn goto_pos(&self, pos: Pos) {
        self.jump_to(&diagnostics::iter_at(&self.buffer, pos), 0.25);
    }

    /// Jump to a character range and mark it the way the find bar marks a match it found: the
    /// range itself is selected, and the text inside it becomes this tab's search query with the
    /// highlight on, so every other occurrence in the note is marked too.
    ///
    /// Nothing here expires. The marks are the find bar's own and go the way they always do — a
    /// new query replaces them, an edit moves them, closing the bar clears them.
    pub fn goto_range(&self, chars: Range<usize>) {
        let last = self.buffer.char_count();
        let start = self
            .buffer
            .iter_at_offset((chars.start as i32).clamp(0, last));
        let end = self
            .buffer
            .iter_at_offset((chars.end as i32).clamp(0, last));
        self.jump_to(&start, 0.3);
        if start == end {
            return;
        }
        self.buffer.select_range(&start, &end);
        self.set_query(&self.buffer.text(&start, &end, false));
        self.set_highlight(true);
    }

    /// Scroll a line into view without moving the caret: what the go-to entry previews while the
    /// number is still being typed.
    pub fn show_line(&self, line: i32) {
        let mut iter = self.line_iter(line, 1);
        fold::reveal(self.text_buffer(), &iter);
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

    /// Called for a Ctrl+click in the view or the Go to Definition chord.
    pub fn connect_follow(self: &Rc<Self>, f: impl Fn(&Rc<Tab>) + 'static) {
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
            Flavour::Note => *self.links.borrow_mut() = highlight::apply(&self.buffer).links,
            Flavour::Csv => highlight::apply_csv(&self.buffer),
            // Code is coloured by its language through the style scheme, with nothing to derive.
            Flavour::Code => {}
        }
        self.update_marks();
        // The tags the sticky title reads are the ones that were just re-applied.
        self.update_sticky();
    }

    /// Redraw the gutter's change bars from the committed text. Rides the same path as styling,
    /// so it follows a keystroke on a small note and the debounce on a large one.
    fn update_marks(&self) {
        let head = self.head.borrow();
        let Some(head) = head.as_ref() else {
            return;
        };
        let lines = accent_core::diff::lines(head, &self.text());
        self.marks.set_marks(crate::marks::marks(
            &lines,
            self.buffer.line_count() as usize,
        ));
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
    ///
    /// A blocked autosave is a plain no-op, with the banner and the tab's dot as the only signal.
    /// It cannot ask — a modal on every focus change and every idle second is not something
    /// anyone can work through — and it deliberately does not write the buffer anywhere else
    /// either: a second copy nothing in the app ever reads back is a second source of truth, and
    /// the buffer is not going anywhere while the window is open.
    fn autosave_now(self: &Rc<Self>) {
        if let Some(id) = self.autosave.borrow_mut().take() {
            id.remove();
        }
        let modified = self.modified.get();
        if modified && may_save(modified, self.disk_changed.get()) {
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
    /// A closed tab takes its pending timeouts, its font provider and its document on the
    /// language layer with it.
    fn drop(&mut self) {
        lang::detach(self);
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
    fn leading_indent_is_the_spaces_and_tabs_a_line_opens_with() {
        assert_eq!(leading_indent(""), "");
        assert_eq!(leading_indent("  - a"), "  ");
        assert_eq!(leading_indent("\tx"), "\t");
        assert_eq!(leading_indent("no indent\n"), "");
        assert_eq!(
            leading_indent("   \n"),
            "   ",
            "a blank line still has its indent"
        );
    }

    /// What a whole-line cut or copy puts on the clipboard: a line, newline included, so the
    /// paste that follows it opens a line of its own.
    #[test]
    fn a_copied_line_carries_its_newline() {
        assert_eq!(paste_ready("- item\n"), "- item\n");
        assert_eq!(
            paste_ready("last line"),
            "last line\n",
            "a last line has none"
        );
        assert_eq!(paste_ready("\n"), "\n", "an empty line is still a line");
    }

    /// The sticky title shows the innermost block that has actually gone off the top.
    #[test]
    fn a_sticky_title_shows_the_innermost_block_above_the_view() {
        assert_eq!(sticky_opener(30, Some(4), None), Some(4), "a heading alone");
        assert_eq!(
            sticky_opener(30, Some(4), Some(20)),
            Some(20),
            "the fence inside the section wins"
        );
        assert_eq!(
            sticky_opener(30, Some(40), None),
            None,
            "a heading below the view is not around it"
        );
        assert_eq!(
            sticky_opener(4, Some(4), None),
            None,
            "the line itself is already on screen"
        );
        assert_eq!(
            sticky_opener(30, None, None),
            None,
            "plain prose pins nothing"
        );
    }

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

    /// A description with no size of its own falls back to GNOME's 11 pt, zoom included, and one
    /// with no family at all to the family the editor is written in. The display-wide rule goes
    /// through the same function, so a second fallback here would be a second face there.
    #[test]
    fn font_css_fills_in_a_missing_size() {
        assert!(font_css("Cantarell", "#doc", 2.0).contains("font-size: 22pt"));
        let css = font_css("11", "#doc", 1.0);
        assert!(css.contains("font-family: \"Adwaita Mono\""), "{css}");
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

    /// The save gate, which is the one thing in this file that costs the user their writing when
    /// it is wrong. VS Code's rule: a file that moved in the background is a question, and a save
    /// is not an answer to it.
    #[test]
    fn a_dirty_buffer_over_a_file_that_moved_is_never_written() {
        assert!(may_save(true, false), "the ordinary save");
        assert!(
            !may_save(true, true),
            "edits over a file that moved: refused"
        );
        assert!(
            may_save(false, true),
            "a clean buffer has nothing to lose, which is how a deleted note is written back"
        );
        assert!(may_save(false, false));
    }

    /// One banner, more than one thing to say: they queue by what each can cost instead of
    /// overwriting each other.
    #[test]
    fn the_banner_shows_the_costliest_standing_question() {
        assert_eq!(banner_alert(&[]), None);
        assert_eq!(
            banner_alert(&[Alert::Conflict, Alert::Compare]),
            Some(Alert::Compare),
            "unsaved edits over a moved file outrank a copy sitting beside the note"
        );
        assert_eq!(
            banner_alert(&[Alert::Compare, Alert::Restore]),
            Some(Alert::Restore)
        );
        assert_eq!(
            banner_alert(&[Alert::ReadOnly, Alert::Conflict]),
            Some(Alert::Conflict),
            "a report never displaces a question"
        );
        assert_eq!(banner_alert(&[Alert::ReadOnly]), Some(Alert::ReadOnly));
    }

    #[test]
    fn a_duplicated_line_brings_its_own_newline_only_when_it_has_none() {
        assert_eq!(duplicated("note\n"), "note\n");
        assert_eq!(duplicated("last line"), "\nlast line");
        assert_eq!(duplicated(""), "\n");
    }
}
