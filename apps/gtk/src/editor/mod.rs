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

use crate::{diagnostics, diff, fold, highlight, lang, multicaret};
use accent_api::{Diagnostic, Fold};
use accent_core::fs::{self, Etag};
use accent_core::markdown::Link;
use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

mod banner;
mod compare;
mod keys;
mod lines;
mod page;
mod search;
mod text;

pub use banner::Alert;
use compare::Comparing;
pub use compare::{companion, restyle_companion, style_companion};
use lines::line_clipboard;
pub use page::default_font;
use page::{GUTTER, line_numbers};
pub(crate) use page::{font_css, install_font, next_view_name};
use search::mute;
pub(crate) use text::{caret, line_end, line_prefix};

/// How long a long note waits after the last keystroke before it is re-analysed.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Notes at or below this many characters get a *full* re-style on the keystroke. Measured cost
/// of a full pass, tag churn included, which dominates the parsing: 0.7 ms at 2 KB, 2.5 ms at
/// 8 KB, 10 ms at 32 KB, 21 ms at 64 KB. This size stays inside a frame and still covers the notes
/// people actually write (median 3.5 KB in the test vault). A longer note keeps the debounce for
/// the full pass, because a pass that outlasts a frame is felt as input lag — but it is not left
/// unstyled while typing: [`highlight::apply_line`] re-tags the caret's line on every keystroke,
/// so markup appears as it is typed the way Apostrophe does it either side of the threshold.
const INSTANT: i32 = 16 * 1024;
/// DESIGN.md, Motion: save 1 s after the last edit.
const AUTOSAVE: Duration = Duration::from_secs(1);
/// The cursor callback drives the preview's scroll sync; 100 ms is below what the eye follows.
const CURSOR: Duration = Duration::from_millis(100);
/// A callback the app registered. Stored behind an `Rc` so it can be cloned out of its cell
/// before it runs: a callback is free to reach back into the tab that called it.
type Hook = RefCell<Option<Rc<dyn Fn(&Rc<Tab>)>>>;

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
    pub ghost_text: bool,
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
    /// The page's content and the document inside it, kept so a comparison can put a paned
    /// between the two and take it away again. See [`Tab::compare`].
    content: gtk::Box,
    document: gtk::Box,
    comparing: RefCell<Option<Comparing>>,
    /// The buttons a comparison lays over the editor, kept across comparisons: see `diff::Pool`.
    overlays: Rc<diff::Pool>,
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
    /// Whether ghost text is wanted here. Mirrored onto `lang.ghost` so the request path reads
    /// one cell, and kept here so a tab built while the preference was off can be turned on.
    ghost_text: Cell<bool>,
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
    /// Every other occurrence of what is selected, muted.
    ///
    /// A tag of our own rather than a second `SearchContext` fed the selection, which is the
    /// shorter way to write it: two search contexts on one buffer each keep their tag at the top
    /// of the tag table and re-raise it as they rescan, so which of the two colours paints an
    /// overlap is a race. Measured under `ACCENT_BENCH_OCCUR`, the find bar lost it — every match
    /// rendered in the muted colour with the bar open on the selected word. An ordinary tag made
    /// before the context stays under it whatever either of them does.
    occurrence_tag: gtk::TextTag,
    /// What that tag is showing, so a caret move that changes nothing re-tags nothing.
    occurrence_query: RefCell<Option<String>>,
    spell: RefCell<Option<libspelling::TextBufferAdapter>>,
    /// The note's links, each with the character range it covers. Characters and not the bytes
    /// the parse reports them in: the pointer asks which link it is over on every motion event
    /// while Ctrl is held, and translating a byte offset there meant copying the text up to the
    /// pointer each time.
    links: RefCell<Vec<(Range<i32>, Link)>>,
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
    /// Whether the completion popup is up. Read by every step of [`keys`]'s chain: while the
    /// popup is showing it owns the keyboard, and nothing else in the view may answer a key.
    pub(crate) popup_shown: Cell<bool>,
    /// The last template pushed into the view, kept only to ask whether its stops are still being
    /// walked: a snippet drops its buffer when it finishes, so that is the question's answer.
    snippet: RefCell<Option<sourceview5::Snippet>>,
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

/// Where a reload has to put the reader back: the caret, and the line at the top of the view with
/// where that line sat, so the same text goes back under the same edge however far the new bytes
/// move it.
#[derive(Clone, Copy)]
struct Anchor {
    offset: i32,
    top_line: i32,
    top_y: i32,
    scrolled: f64,
}

/// A buffer and a view over `text`, set up for `flavour`: what the editor and a comparison's
/// read-only companion have in common, so the two sides of a diff render one note alike.
fn build(
    flavour: Flavour,
    language: Option<sourceview5::Language>,
    text: &str,
) -> (sourceview5::View, sourceview5::Buffer) {
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
    buffer.set_text(text);
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
    let subclass = multicaret::View::new();
    // Up and Down by a line of the document, which is what a column of carets in code asks for.
    // In prose a line is a paragraph and one Down would be several screens, so it keeps GTK's.
    subclass.set_logical_lines(!flavour.is_note());
    let view: sourceview5::View = subclass.upcast();
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
    // Apostrophe-like page: generous side gutters, room to breathe at the ends. `set_page`,
    // called from `set_font` below, puts the zoomed values here.
    view.set_pixels_above_lines(2);
    view.set_pixels_below_lines(2);
    (view, buffer)
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
    let (zoom, column_width, ghost_text) = (prefs.zoom, prefs.column_width, prefs.ghost_text);

    // A language for code, none for a note (our own spans do that) and none for a CSV, whose
    // `csv.lang` would colour numbers and strings underneath the column tags and fight them.
    let language = match flavour {
        Flavour::Code => guess_language(&path, &text.text),
        Flavour::Note | Flavour::Csv => None,
    };
    let (view, buffer) = build(flavour, language, &text.text);
    // Not valid UTF-8: what is on screen is lossy, so it must not be written back.
    if text.lossy {
        view.set_editable(false);
    }
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
    // Made before the find bar's search context, so it stays under the tag that context paints
    // with: a tag added later to the table outranks an earlier one, and gtksourceview only ever
    // raises its own. See [`Tab::occurrence_tag`].
    let occurrence_tag = gtk::TextTag::new(Some("occurrence"));
    buffer.tag_table().add(&occurrence_tag);
    mute(&buffer, &occurrence_tag);
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
        content: column.clone(),
        document: document.clone(),
        comparing: RefCell::new(None),
        overlays: Rc::default(),
        scroller: scroller.clone(),
        clamp,
        zoom: Cell::new(zoom),
        column: Cell::new(column_width),
        ghost_text: Cell::new(ghost_text),
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
        occurrence_tag,
        occurrence_query: RefCell::new(None),
        spell: RefCell::new(None),
        links: RefCell::new(Vec::new()),
        diagnostics: RefCell::new(Vec::new()),
        annotations,
        folds: RefCell::new(Vec::new()),
        fold_renderer: folds.clone(),
        font: RefCell::new(None),
        monitor: RefCell::new(None),
        loading: Cell::new(false),
        popup_shown: Cell::new(false),
        snippet: RefCell::new(None),
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
    // One controller for the keys the popup, the signature, the ghost text, the extra carets and
    // the markdown helpers all want; `keys` is where their order is written down.
    keys::install(&tab);
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
    // (a background tab is only mapped when it is first selected). One hook for the lot: the
    // spans, the change bars, the diagnostic underlines and the fold chevrons all mix that same
    // foreground, and `Tab::restyle` is what a theme change already runs.
    tab.restyle();
    view.connect_map(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.restyle()
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
    // `mark-set` rather than `notify::cursor-position`: a drag that ends where the caret already
    // was moves only the other end of the selection, and both ends make a selection anyway.
    buffer.connect_mark_set(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_, _, mark| {
            let moved = mark.name();
            if matches!(moved.as_deref(), Some("insert" | "selection_bound")) {
                tab.highlight_occurrences();
            }
        }
    ));
    banner.connect_button_clicked(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.emit(&tab.on_banner)
    ));

    // Leaving the view is the other autosave trigger: switching tabs or windows mid-sentence
    // should not be the one edit that is lost. It is also where the document settles: the save
    // has just happened, and a provider that re-reads the vault on one is told now rather than
    // after every autosave.
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| {
            tab.autosave_now();
            crate::lang::settle(&tab);
        }
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

// ------------------------------------------------------------------------------------- helpers

/// GtkSourceView paints its background from its own style scheme, so unlike every other widget in
/// the window it has to be told about the theme explicitly.
pub fn sync_scheme(buffer: &sourceview5::Buffer) {
    let id = crate::theme::scheme_id(adw::StyleManager::default().is_dark());
    let scheme = sourceview5::StyleSchemeManager::default().scheme(id);
    buffer.set_style_scheme(scheme.as_ref());
}

fn title_of(rel: &str) -> &str {
    let name = accent_core::path::basename(rel);
    name.strip_suffix(".md").unwrap_or(name)
}

/// The language a file called `key` holding `text` is coloured as.
pub fn language_for(key: &str, text: &str) -> Option<sourceview5::Language> {
    guess_language(Path::new(key), text)
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
    ///
    /// The language layer is told all the same. `loading` is what keeps `on_changed` out of this,
    /// and with it the edit event that would otherwise carry the news: without this a silent
    /// reload left the server, the symbols, the folds and the diagnostics describing the text the
    /// file used to hold until the next keystroke.
    pub fn set_text(self: &Rc<Self>, text: &str) {
        self.loading.set(true);
        self.buffer.set_text(text);
        self.loading.set(false);
        self.analyse();
        lang::changed(self);
    }

    /// Whether the buffer is being replaced by us rather than typed in. The handlers that watch
    /// `insert-text` ask before acting: a template pushed back through the buffer is not somebody
    /// typing an opening bracket, and should raise neither signature help nor a suggestion.
    pub(crate) fn is_loading(&self) -> bool {
        self.loading.get()
    }

    pub fn mark_clean(&self, etag: Etag) {
        self.etag.set(Some(etag));
        self.modified.set(false);
        self.disk_changed.set(false);
        self.page.set_title(&self.tab_title());
    }

    /// Silent reload for a clean tab: the file changed on disk and there is nothing to lose.
    ///
    /// The bytes are read on a worker thread, the way `open.rs` reads a file into a new tab: a
    /// watcher can fire this on any file, and a synchronous read of a large one held the window
    /// for as long as the disk took. `done` is told how it ended, once.
    pub fn reload_keep_cursor(
        self: &Rc<Self>,
        done: impl Fn(&Rc<Tab>, std::io::Result<()>) + 'static,
    ) {
        // Where the caret and the page are, measured before the read: replacing the buffer empties
        // it, which drops the view to line one, and a scroll to the caret from there parks it
        // against whichever edge is nearer instead of putting the page back.
        let (top_iter, _) = self.view.line_at_y(self.view.visible_rect().y());
        let anchor = Anchor {
            offset: caret(&self.buffer).offset(),
            top_line: top_iter.line(),
            top_y: self.view.line_yrange(&top_iter).0,
            scrolled: self.view.vadjustment().map_or(0.0, |v| v.value()),
        };
        let path = self.path();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = gio::spawn_blocking(move || fs::read_text(&path)).await;
            let Some(tab) = weak.upgrade() else { return };
            let text = match read {
                Ok(Ok(fs::Read::Text(text))) => text,
                // It stopped being text while we had it open. The buffer keeps the last readable
                // version rather than showing the user a screen of replacement characters.
                Ok(Ok(_)) => {
                    return done(
                        &tab,
                        Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "not text any more",
                        )),
                    );
                }
                Ok(Err(e)) => return done(&tab, Err(e)),
                Err(_) => return done(&tab, Err(std::io::Error::other("the reader panicked"))),
            };
            tab.adopt_reload(text, anchor);
            done(&tab, Ok(()));
        });
    }

    /// Put a reload's text in the buffer and the reader back where they were looking.
    fn adopt_reload(self: &Rc<Self>, text: fs::Text, anchor: Anchor) {
        self.crlf.set(text.crlf);
        self.lossy.set(text.lossy);
        let etag = text.etag;
        self.set_text(&text.text);
        let iter = self
            .buffer
            .iter_at_offset(anchor.offset.min(self.buffer.char_count()));
        self.buffer.place_cursor(&iter);
        // One idle later, because the buffer has only just been replaced: a position measured
        // against lines the view has not laid out yet lands short of the line it was given.
        let (view, buffer) = (self.view.clone(), self.buffer.clone());
        glib::idle_add_local_once(move || {
            let iter = buffer
                .iter_at_line(anchor.top_line.min(buffer.line_count() - 1))
                .unwrap_or_else(|| buffer.end_iter());
            let moved = view.line_yrange(&iter).0 - anchor.top_y;
            if let Some(vadjustment) = view.vadjustment() {
                vadjustment.set_value(anchor.scrolled + f64::from(moved));
            }
        });
        self.mark_clean(etag);
        self.clear_disk_alert();
    }

    pub fn restyle(&self) {
        // The scheme is what recolours code, and it is also what a note's own tags sit on.
        sync_scheme(&self.buffer);
        // Derived from the scheme that just changed, so it has to be derived again.
        mute(&self.buffer, &self.occurrence_tag);
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
        if let Some(compare) = self.comparison() {
            compare.restyle();
        }
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
        // Taken rather than borrowed across the call: `resync` folds, folding moves the caret off
        // a line it is hiding, and a `mark-set` handler is one hook away from asking this tab what
        // its folds are.
        let old = self.folds.take();
        fold::resync(self.text_buffer(), &old, &folds);
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
        caret(&self.buffer).line().max(0)
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
        caret(&self.buffer).line().max(0) as u32 + 1
    }

    fn tab_title(&self) -> String {
        let rel = self.rel();
        let name = tab_name(&rel, self.flavour);
        let name = match self.comparing.borrow().as_ref() {
            Some(comparing) => format!("{name} ({})", comparing.label),
            None => name.to_string(),
        };
        match self.modified.get() {
            true => format!("• {name}"),
            false => name,
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

    /// VS Code's Add Cursor Above / Below. Multi-caret lives on the view subclass; the tab keeps
    /// the plain `sourceview5::View` type so nothing else has to know about it.
    pub fn add_caret(&self, below: bool) {
        if let Some(view) = self.view.downcast_ref::<multicaret::View>() {
            view.add_caret(below);
        }
    }

    /// Park `snippet` in the view at `at` and remember it, so a Tab pressed while its stops are
    /// still being walked goes to the template rather than to a suggestion.
    pub(crate) fn push_snippet(&self, snippet: &sourceview5::Snippet, at: &mut gtk::TextIter) {
        self.view.push_snippet(snippet, Some(at));
        *self.snippet.borrow_mut() = Some(snippet.clone());
    }

    /// Whether a template's stops are still being walked. A snippet lets its buffer go when it
    /// finishes, which is the only thing GtkSourceView 5.20 says about it from the outside.
    pub(crate) fn snippet_active(&self) -> bool {
        self.snippet
            .borrow()
            .as_ref()
            .is_some_and(|snippet| snippet.buffer().is_some())
    }

    /// The view as the subclass that paints ghost text and holds the extra carets.
    pub fn ghost_view(&self) -> Option<&multicaret::View> {
        self.view.downcast_ref::<multicaret::View>()
    }

    /// Whether this tab was built with ghost text wanted; read once by `ghost::install`.
    pub fn ghost_text_wanted(&self) -> bool {
        self.ghost_text.get()
    }

    /// The preference changed under an open tab. Off clears whatever is on screen at once; on
    /// only arms the path, since the session behind it is decided when the document is opened.
    pub fn set_ghost_text(self: &Rc<Self>, on: bool) {
        self.ghost_text.set(on);
        self.lang.ghost.on.set(on);
        if !on {
            crate::ghost::clear(self);
        }
    }

    // --- links ---------------------------------------------------------------------------

    pub fn link_at_cursor(&self) -> Option<Link> {
        self.link_at_iter(&caret(&self.buffer))
    }

    /// The link under a pointer position in the view's own coordinates.
    pub fn link_at(&self, x: f64, y: f64) -> Option<Link> {
        let (bx, by) =
            self.view
                .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let iter = self.view.iter_at_location(bx, by)?;
        self.link_at_iter(&iter)
    }

    /// The link covering `iter`, compared in the buffer's own coordinates: the translation from
    /// the parse's byte ranges happened once, when the note was analysed.
    fn link_at_iter(&self, iter: &gtk::TextIter) -> Option<Link> {
        let at = iter.offset();
        self.links
            .borrow()
            .iter()
            .find(|(range, _)| range.contains(&at))
            .map(|(_, link)| link.clone())
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
        let instant = self.buffer.char_count() <= INSTANT;
        if instant {
            self.reanalyse();
        } else if self.flavour.is_note() {
            // Too long for a full pass inside a frame, so the line under the caret is styled now
            // and everything else waits: what a typist watches change is the line they are typing.
            let line = caret(&self.buffer).line();
            highlight::apply_line(&self.buffer, line);
        }
        // The change bars are a second whole-buffer copy and a line diff against the committed
        // text, which is too much to spend on a keystroke however short the note is, and nothing
        // a typist watches: they follow the debounce at either size.
        let id = glib::timeout_add_local_once(
            DEBOUNCE,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || {
                    *tab.debounce.borrow_mut() = None;
                    tab.update_marks();
                    if !instant {
                        tab.reanalyse();
                    }
                }
            ),
        );
        *self.debounce.borrow_mut() = Some(id);
        self.schedule_autosave();
    }

    /// Re-read the buffer once and refresh everything derived from it: the styling tags, and the
    /// link table that Ctrl+click and Ctrl+Return follow. The preview listens on `on_edited` and
    /// debounces its own re-render, so calling this per keystroke only re-arms that timer.
    fn reanalyse(self: &Rc<Self>) {
        self.analyse_text();
        self.emit(&self.on_edited);
    }

    /// Everything this tab's text implies, the change bars included: what a reload or a template
    /// needs, where the whole document has moved at once.
    fn analyse(&self) {
        self.analyse_text();
        self.update_marks();
    }

    /// The half of [`Tab::analyse`] a keystroke can afford. A note gets its styling spans and its
    /// link table; code gets nothing, because the style scheme colours it from the language.
    fn analyse_text(&self) {
        match self.flavour {
            Flavour::Note => {
                let (analysis, offsets) = highlight::apply(&self.buffer);
                *self.links.borrow_mut() = analysis
                    .links
                    .into_iter()
                    .map(|link| {
                        let range =
                            offsets.char_of(link.range.start)..offsets.char_of(link.range.end);
                        (range, link)
                    })
                    .collect();
            }
            Flavour::Csv => highlight::apply_csv(&self.buffer),
            // Code is coloured by its language through the style scheme, with nothing to derive.
            Flavour::Code => {}
        }
        // The tags the sticky title reads are the ones that were just re-applied.
        self.update_sticky();
        if let Some(compare) = self.comparison() {
            compare.refresh();
        }
    }

    /// Redraw the gutter's change bars from the committed text. On the debounce, not the
    /// keystroke: it copies the whole buffer and diffs it against the committed text.
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
}
