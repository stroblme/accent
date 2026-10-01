//! Opening a tab: the view and the buffer a tab is built on, and everything [`open`] ties to
//! them — the gutters, the page around the view, the tags, and the signals that drive the tab.

use super::*;

/// A buffer and a view over `text`, set up for `flavour`: what the editor and a comparison's
/// read-only companion have in common, so the two sides of a diff render one note alike.
pub(super) fn build(
    flavour: Flavour,
    language: Option<sourceview5::Language>,
    text: &str,
) -> (sourceview5::View, sourceview5::Buffer) {
    let buffer = sourceview5::Buffer::new(None);
    buffer.set_language(language.as_ref());
    // First: a note's heading tags set an indent too, and have to outrank these.
    wrap::install(&buffer);
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
    // Tab walks a template's `{{cursor}}` stops and a completion's placeholders
    // (`Tab::push_snippet`): GtkSourceView hands a key to its snippets only with this on. With it
    // on, Tab after a word would also expand whatever snippet files GtkSourceView finds, so the
    // process's one manager is given nowhere to look, before the first Tab ever asks it.
    view.set_enable_snippets(true);
    sourceview5::SnippetManager::default().set_search_path(&[]);
    caret_to_click(&view);
    view.set_show_line_numbers(false);
    // The Indent Width preference's default, which `Tab::set_indent_width` replaces in a tab. A
    // companion beside a tab takes the tab's (`diff::Compare::follow_editor`) and two companions
    // keep this, so both sides of a comparison count a tab and a wrap level alike.
    view.set_tab_width(4);
    if !flavour.is_note() {
        view.set_auto_indent(true);
        view.set_indent_on_tab(true);
        view.set_smart_backspace(true);
        view.set_highlight_current_line(true);
        // Everything but a makefile, where a leading tab is syntax.
        let tabs_are_syntax = language.as_ref().is_some_and(|l| l.id() == "makefile");
        view.set_insert_spaces_instead_of_tabs(!tabs_are_syntax);
    }
    // Apostrophe-like page: generous side gutters, room to breathe at the ends. `set_page`,
    // called from `set_font` below, puts the zoomed values here.
    view.set_pixels_above_lines(2);
    view.set_pixels_below_lines(2);
    // Once the tab width is set, which the columns are counted in.
    wrap::follow(&view, flavour.is_note());
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
    // Plain-text cut and copy, and whole-line with nothing selected, whatever the tab holds: an
    // editor where Ctrl+X on no selection does nothing is one that makes the user select the line
    // first.
    line_clipboard(&view);
    primary_paste(&view);
    drag::install(&view);
    let paste_link = paste::link_paste(&view, flavour);
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
    // It goes with the chrome while the user types (`App::hide_chrome`).
    map.add_css_class("chrome-fade");
    let document = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    document.append(&overlay);
    document.append(&map);

    let banner = adw::Banner::new("");
    // Made before the find bar's match tag, so it stays under it: a tag added later to the table
    // outranks an earlier one, and the match tag is only ever raised. See [`Tab::occurrence_tag`].
    let occurrence_tag = gtk::TextTag::new(Some("occurrence"));
    buffer.tag_table().add(&occurrence_tag);
    mute(&buffer, &occurrence_tag);
    // After the muted hint and before the find bar's match tag, which is the order the three
    // paint in: see [`Tab::reveal_range`].
    let reveal_tag = gtk::TextTag::new(Some("reveal"));
    buffer.tag_table().add(&reveal_tag);
    matched(&buffer, &reveal_tag);
    // No colour of its own: the word keeps whatever the style scheme paints it, and gains the
    // underline that says a Ctrl+click would land somewhere.
    let follow_tag = gtk::TextTag::new(Some("follow"));
    follow_tag.set_underline(pango::Underline::Single);
    buffer.tag_table().add(&follow_tag);
    // Last of the three. The query matches folded text too, as VS Code finds into folds, and
    // stepping to a match in a shut block opens it (`Tab::step`).
    let find_tag = gtk::TextTag::new(Some("find"));
    buffer.tag_table().add(&find_tag);
    match_style(&buffer, &find_tag);
    if let Some(column) = view.downcast_ref::<multicaret::View>() {
        column.set_find_tag(&find_tag);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&banner);
    column.append(&document);
    let page = tabs.append(&column);
    page.set_title(crate::doc::file_name(key));
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
        conflicts: crate::conflict::Conflicts::new(&view, &buffer, &scroller),
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
        save: SaveState::at(text.etag),
        alerts: RefCell::new(Vec::new()),
        find: RefCell::default(),
        find_tag,
        on_found: RefCell::new(None),
        occurrence_tag,
        occurrence_query: RefCell::new(None),
        reveal_tag,
        revealed: Cell::new(false),
        spell: RefCell::new(None),
        links: RefCell::new(Vec::new()),
        follow_tag,
        follow: RefCell::new(Follow::default()),
        diagnostics: RefCell::new(Vec::new()),
        annotations,
        diagnostics_hidden: Cell::new(false),
        annotated: Cell::new(0),
        folds: RefCell::new(Vec::new()),
        fold_renderer: folds.clone(),
        font: RefCell::new(None),
        monitor: RefCell::new(None),
        loading: Cell::new(false),
        replaced: Cell::new(0),
        snippet: RefCell::new(None),
        paste_link,
        debounce: crate::widgets::Debounce::new(DEBOUNCE),
        autosave: crate::widgets::Debounce::new(AUTOSAVE),
        cursor: crate::widgets::Debounce::new(CURSOR),
        turn: Cell::new((false, false)),
        moved: Cell::new(false),
        refit: crate::widgets::Debounce::new(DEBOUNCE),
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
    tab.set_indent_width(prefs.indent_width);
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

    // An end-of-line message is cut to the column it was laid in (`diagnostics::fit`), so a new
    // column width lays them again. The adjustment's page size is that width, the view being the
    // scrollable child.
    scroller
        .hadjustment()
        .connect_page_size_notify(glib::clone!(
            #[weak(rename_to = tab)]
            tab,
            move |_| {
                if tab.annotated.get() > 0 {
                    let weak = Rc::downgrade(&tab);
                    tab.refit.call(move || {
                        if let Some(tab) = weak.upgrade() {
                            tab.paint_diagnostics();
                        }
                    });
                }
            }
        ));

    // What the top of the view is inside changes on every scroll, and the widget the title has
    // to line up with moves with the clamp, so the bar is recomputed rather than positioned once.
    scroller.vadjustment().connect_value_changed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.update_sticky()
    ));
    // The find bar paints the matches around what is on screen, so a scroll or a resize that
    // shows other lines has them painted too (`Tab::paint_matches`).
    let adjustment = scroller.vadjustment();
    adjustment.connect_value_changed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.queue_paint()
    ));
    adjustment.connect_changed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.queue_paint()
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
                // A caret move is the reader answering the jump that put the reveal up — by a
                // click, an arrow key or a keystroke — so it is what takes the reveal back down.
                tab.clear_reveal();
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
            if let Some(iter) = crate::fold::iter_at_location(&tab.view, bx, by) {
                tab.buffer.place_cursor(&iter);
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
            tab.clear_follow();
            tab.emit(&tab.on_follow);
        }
    ));
    view.add_controller(click);

    // The pointer and the underline only change when the answer does: `follow_hint` compares
    // what it is about to show with what is already showing, so an ordinary drag across the view
    // costs a lookup in the link table and nothing else.
    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |controller, x, y| {
            let ctrl = controller
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK);
            tab.follow_hint(x, y, ctrl);
        }
    ));
    motion.connect_leave(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.clear_follow()
    ));
    view.add_controller(motion);

    tab
}
