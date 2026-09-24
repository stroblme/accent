//! Drills over the keyboard: the editor's key semantics, and a shell's accelerators, title and
//! life beyond its window.

use super::*;
use vte4::TerminalExt as _;

/// Drive the key semantics [`multicaret::View`] corrects — the wordwise deletes, logical-line
/// Up/Down, and the same chords at a column of carets — through the very signals the key bindings
/// emit, and print what the buffer and the carets came out as.
///
/// A view of its own in a window of its own, so nothing is written into a vault and the drills do
/// not depend on a document being open. It needs a display, which is why this is a bench hook and
/// not a unit test, but it needs no key press and no pointer: the signals are actions, and
/// [`multicaret::View::press`] is the key controller's own handler.
pub(super) fn bench_keys(app: &Rc<App>) {
    let view = multicaret::View::new();
    // The drills are about the code flavours, which is where the logical-line moves are wanted.
    view.set_logical_lines(true);
    view.set_wrap_mode(gtk::WrapMode::Word);
    // A tab's whole-line cut and copy, which the column drills go through.
    editor::line_clipboard(view.upcast_ref());
    // Scrolled, as in a tab, so a page is what is on screen rather than the whole buffer.
    let window = gtk::Window::builder()
        .default_width(320)
        .default_height(240)
        .child(&gtk::ScrolledWindow::builder().child(&view).build())
        .build();
    window.present();
    let app = app.clone();
    // After a frame, so the view has a size and its lines have been laid out.
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let buffer = view.buffer();
        let text = |buffer: &gtk::TextBuffer| {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };

        // Ctrl+Delete and Ctrl+Backspace take the whitespace run and stop.
        buffer.set_text("   a b");
        buffer.place_cursor(&buffer.start_iter());
        view.emit_delete_from_cursor(gtk::DeleteType::WordEnds, 1);
        println!("bench ctrl_delete {:?}", text(&buffer));
        buffer.set_text("a   b");
        buffer.place_cursor(&buffer.iter_at_offset(4));
        view.emit_delete_from_cursor(gtk::DeleteType::WordEnds, -1);
        println!("bench ctrl_backspace {:?}", text(&buffer));

        // Down is one line of the document even where that line wraps over several rows.
        buffer.set_text(&format!("{}\nshort\ntail", "wide ".repeat(80)));
        buffer.place_cursor(&buffer.iter_at_offset(3));
        let mut row = buffer.start_iter();
        let wraps = view.forward_display_line(&mut row) && row.line() == 0;
        println!("bench wraps {wraps}");
        view.emit_move_cursor(gtk::MovementStep::DisplayLines, 1, false);
        let at = buffer.iter_at_mark(&buffer.get_insert());
        println!("bench down_line {} {}", at.line(), at.line_offset());

        // End goes to the end of the line, not to the end of the screen row it is on.
        buffer.place_cursor(&buffer.iter_at_offset(3));
        view.emit_move_cursor(gtk::MovementStep::DisplayLineEnds, 1, false);
        let at = buffer.iter_at_mark(&buffer.get_insert());
        println!("bench end_line {} {}", at.line(), at.line_offset());

        // Every caret answers Ctrl+Delete, not only the primary one.
        buffer.set_text("a   b\nc   d");
        buffer.place_cursor(&buffer.iter_at_offset(1));
        view.add_caret(true);
        view.press(gdk::Key::Delete, gdk::ModifierType::CONTROL_MASK);
        println!("bench caret_delete {:?}", text(&buffer));
        view.clear_carets();

        // Every caret moves wordwise, and a trip down over a short line and back up restores the
        // constellation rather than flattening it.
        buffer.set_text("alpha beta\nxy\ngamma delta\nomega zeta");
        buffer.place_cursor(&buffer.start_iter());
        view.add_caret(true);
        view.add_caret(true);
        view.press(gdk::Key::Right, gdk::ModifierType::CONTROL_MASK);
        println!("bench caret_words {:?}", view.caret_positions());
        view.press(gdk::Key::Down, gdk::ModifierType::empty());
        println!("bench caret_down {:?}", view.caret_positions());
        view.press(gdk::Key::Up, gdk::ModifierType::empty());
        println!("bench caret_columns {:?}", view.caret_positions());
        view.clear_carets();

        // Tab at every caret is what the view says it is, from the column each caret is in.
        view.set_tab_width(4);
        for spaces in [true, false] {
            view.set_insert_spaces_instead_of_tabs(spaces);
            buffer.set_text("ab\ncd");
            buffer.place_cursor(&buffer.iter_at_offset(1));
            view.add_caret(true);
            view.press(gdk::Key::Tab, gdk::ModifierType::empty());
            println!("bench caret_tab spaces={spaces} {:?}", text(&buffer));
            view.clear_carets();
        }

        // A column takes the blink over, GTK's own caret going transparent with the class, and
        // hands it back when it goes. How it looks is a manual check; that it toggles is not.
        view.add_caret(true);
        println!("bench caret_blink {}", view.has_css_class("accent-carets"));
        view.clear_carets();
        println!("bench caret_blink {}", view.has_css_class("accent-carets"));

        // The rest waits on the clipboard, which is read asynchronously even from this process.
        glib::spawn_future_local(async move {
            bench_column(&view).await;
            bench_selections(&view).await;
            bench_lines(&view).await;
            window.close();
            bench_quit(&app);
        });
    });
}

/// A column of carets under VS Code's rules: the keys it survives and the ones that end it, a
/// paste spread over it or not, the line commands and the clipboard at every caret, Page Down, and
/// Undo and Redo putting the carets back. Prints the buffer and every caret after each step.
async fn bench_column(view: &multicaret::View) {
    let buffer = view.buffer();
    let show = |step: &str| {
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        println!("bench column_{step} {text:?} {:?}", view.caret_positions());
    };
    let column = |text: &str| column(view, text);
    let (none, ctrl) = (gdk::ModifierType::empty(), gdk::ModifierType::CONTROL_MASK);
    let undo = || view.press(gdk::Key::z, ctrl);

    // Modifiers pressed on their own leave the column up, so what AltGr and Shift type — `@` and
    // `Q` arrive as plain keys with nothing held that the column reads — lands at every caret.
    column("ab\ncd\nef");
    for key in [
        gdk::Key::ISO_Level3_Shift,
        gdk::Key::Shift_L,
        gdk::Key::Control_L,
        gdk::Key::Caps_Lock,
    ] {
        view.press(key, none);
    }
    view.press(gdk::Key::at, none);
    view.press(gdk::Key::Q, gdk::ModifierType::SHIFT_MASK);
    show("modifiers");
    // Undo takes the keys back one at a time with the carets where each found them; Redo repeats.
    undo();
    show("undo");
    undo();
    show("undo");
    view.press(gdk::Key::Z, ctrl | gdk::ModifierType::SHIFT_MASK);
    show("redo");

    // A forward delete undone leaves every caret in front of what came back, not after it.
    column("abc\ndef\nghi");
    view.press(gdk::Key::Delete, none);
    show("delete");
    undo();
    show("delete_undo");

    // A chord nothing binds leaves the column up; Escape ends it, the primary where it was.
    view.press(gdk::Key::F4, none);
    println!("bench column_f4 {}", view.has_carets());
    view.press(gdk::Key::Escape, none);
    show("escape");
    // A dead key hands the keys after it to the input method, so it ends the column.
    column("ab\ncd\nef");
    view.press(gdk::Key::dead_acute, none);
    println!("bench column_dead_key {}", view.has_carets());

    // Ctrl+A is GTK's, and the select-all it answers with moves the caret, ending the column.
    column("ab\ncd\nef");
    let passed = view.press(gdk::Key::a, ctrl) == glib::Propagation::Proceed;
    view.emit_select_all(true);
    let selection = buffer
        .selection_bounds()
        .map(|(start, end)| (start.offset(), end.offset()));
    println!(
        "bench column_select_all passed={passed} carets={} selection={selection:?}",
        view.has_carets()
    );
    // So does an edit the column did not make, rather than landing at one caret of it.
    column("ab\ncd\nef");
    buffer.insert_at_cursor("z");
    println!("bench column_foreign_edit {}", view.has_carets());

    // A paste with one line per caret hands them out; any other goes whole to every caret.
    for clip in ["1\n2\n3\n", "x\ny"] {
        column("ab\ncd\nef");
        view.clipboard().set_text(clip);
        view.emit_paste_clipboard();
        glib::timeout_future(Duration::from_millis(100)).await;
        show(&format!("paste {clip:?}"));
        undo();
        show("paste_undo");
    }

    // The line commands take every caret's line once. Duplicate moves each caret onto its copy,
    // Insert Line Below onto its new line at the indent; Delete merges the carets it strands.
    column("ab\ncd\nef\ngh");
    editor::duplicate_line(view.upcast_ref());
    show("duplicate");
    undo();
    show("duplicate_undo");
    column("  ab\n  cd");
    editor::newline_below(view.upcast_ref());
    show("newline_below");
    undo();
    show("newline_below_undo");
    column("ab\ncd\nef\ngh");
    editor::delete_line(view.upcast_ref());
    show("delete_line");

    // Copy and cut take every caret's whole line, top to bottom, the cut as one step.
    column("ab\ncd\nef\ngh");
    view.emit_copy_clipboard();
    let copied = view.clipboard().read_text_future().await;
    println!("bench column_copy {copied:?} carets={}", view.has_carets());
    view.emit_cut_clipboard();
    show("cut");

    // Two edits that each changed the text at one caret only — the caret above sits at the start
    // of the buffer, where Backspace does nothing — are a single action in GTK's history, which it
    // joins onto the one before it the way it joins typing. One Undo takes both back, and the
    // carets come back with the text rather than a step behind it.
    view.clear_carets();
    buffer.set_text("ab\ncd");
    buffer.place_cursor(&buffer.iter_at_offset(3));
    view.add_caret(false);
    view.press(gdk::Key::BackSpace, none);
    view.press(gdk::Key::BackSpace, none);
    show("joined");
    undo();
    show("joined_undo");
    view.clear_carets();

    // Page Down moves every caret by the lines on screen, the column's shape kept.
    let lines: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
    column(&lines.join("\n"));
    // Laid out first, or the lines' heights are still estimates and "on screen" is all of them.
    glib::timeout_future(Duration::from_millis(200)).await;
    view.press(gdk::Key::Page_Down, none);
    glib::timeout_future(Duration::from_millis(200)).await;
    // The view scrolls by the same page, so the top line on screen is the primary's again.
    let top = view.line_at_y(view.visible_rect().y()).0.line();
    println!(
        "bench column_page_down {:?} top={top}",
        view.caret_positions()
    );
    view.clear_carets();
}

/// Three carets down column 1 of `text`, or as many as it has lines for, the primary on top.
fn column(view: &multicaret::View, text: &str) {
    let buffer = view.buffer();
    view.clear_carets();
    buffer.set_text(text);
    buffer.place_cursor(&buffer.iter_at_offset(1));
    view.add_caret(true);
    view.add_caret(true);
}

/// A selection at every caret, VS Code's way: Shift extends each caret's own, typing and a delete
/// take them, a plain arrow collapses them, copy and cut take their text, a paste spreads over
/// them, the line commands take every line they cover, selections that grow into each other merge,
/// Undo and Redo put them back, and Escape leaves the primary's. Prints the buffer and every
/// selection as offsets, top to bottom, after each step.
async fn bench_selections(view: &multicaret::View) {
    let buffer = view.buffer();
    let show = |step: &str| {
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        let selections: Vec<(i32, i32)> = view
            .selections()
            .iter()
            .map(|(start, end)| (start.offset(), end.offset()))
            .collect();
        println!("bench selection_{step} {text:?} {selections:?}");
    };
    let none = gdk::ModifierType::empty();
    let (shift, ctrl) = (
        gdk::ModifierType::SHIFT_MASK,
        gdk::ModifierType::CONTROL_MASK,
    );
    let select = |key, times| {
        for _ in 0..times {
            view.press(key, shift);
        }
    };

    // Shift+Right twice selects two characters at every caret; what is typed takes their place,
    // and Undo and Redo put the selections back with the text.
    column(view, "abcd\nefgh\nijkl");
    select(gdk::Key::Right, 2);
    show("extend");
    view.press(gdk::Key::X, shift);
    show("type_over");
    view.press(gdk::Key::z, ctrl);
    show("undo");
    view.press(gdk::Key::Z, ctrl | shift);
    show("redo");

    // Backspace takes each selection and nothing more.
    column(view, "abcd\nefgh\nijkl");
    select(gdk::Key::Right, 2);
    view.press(gdk::Key::BackSpace, none);
    show("backspace");

    // A plain Left collapses each onto its start and Right onto its end, going no further; Down
    // leaves a selection from its end.
    column(view, "abcd\nefgh\nijkl\nmnop");
    select(gdk::Key::Right, 2);
    view.press(gdk::Key::Left, none);
    show("left");
    select(gdk::Key::Right, 2);
    view.press(gdk::Key::Right, none);
    show("right");
    select(gdk::Key::Left, 2);
    view.press(gdk::Key::Down, none);
    show("down");

    // Copy takes each selection, joined by newlines; the cut deletes them as one step. Each on a
    // column of its own: while the clipboard is read, the X server can hand the primary selection
    // to someone else, and GTK lets the primary caret's selection go when it does.
    column(view, "abcd\nefgh\nijkl");
    select(gdk::Key::Right, 2);
    view.emit_copy_clipboard();
    let copied = view.clipboard().read_text_future().await;
    println!("bench selection_copy {copied:?}");
    column(view, "abcd\nefgh\nijkl");
    select(gdk::Key::Right, 2);
    view.emit_cut_clipboard();
    show("cut");
    let cut = view.clipboard().read_text_future().await;
    println!("bench selection_cut_clipboard {cut:?}");

    // A paste with a line per selection hands one to each, in place of it.
    column(view, "abcd\nefgh\nijkl");
    select(gdk::Key::Right, 2);
    view.clipboard().set_text("1\n2\n3");
    view.emit_paste_clipboard();
    glib::timeout_future(Duration::from_millis(100)).await;
    show("paste");

    // Selections that only meet stay apart; grown into each other they are one caret, and the
    // column with it.
    column(view, "ab\ncd\nef\ngh");
    select(gdk::Key::Down, 1);
    show("meet");
    select(gdk::Key::Down, 1);
    show("merge");
    println!("bench selection_merge_carets {}", view.has_carets());

    // The line commands take every line a selection covers, each once. Selections within their
    // lines duplicate line by line and ride down onto the copies; selections running into the
    // next line cover one run, which is copied whole, deleted whole, or opened under once.
    column(view, "ab\ncd\nef\ngh");
    select(gdk::Key::End, 1);
    editor::duplicate_line(view.upcast_ref());
    show("duplicate");
    for (step, command) in [
        (
            "duplicate_run",
            editor::duplicate_line as fn(&sourceview5::View),
        ),
        ("delete_run", editor::delete_line),
        ("newline_below_run", editor::newline_below),
    ] {
        column(view, "ab\ncd\nef\ngh\nij");
        select(gdk::Key::Down, 1);
        command(view.upcast_ref());
        show(step);
    }

    // Escape is the column's own: it ends it and leaves the primary's selection.
    column(view, "abcd\nefgh");
    select(gdk::Key::Right, 2);
    let stopped = view.press(gdk::Key::Escape, none) == glib::Propagation::Stop;
    let primary = buffer
        .selection_bounds()
        .map(|(start, end)| (start.offset(), end.offset()));
    println!(
        "bench selection_escape stopped={stopped} carets={} primary={primary:?}",
        view.has_carets()
    );

    // Focus mode's line fade spans every caret *and* every anchor, so a selection made upwards
    // at a column keeps the lines it was started from out of the veil.
    view.clear_carets();
    buffer.set_text("ab\ncd\nef\ngh\nij");
    buffer.place_cursor(&buffer.iter_at_offset(4));
    view.add_caret(true);
    view.add_caret(true);
    select(gdk::Key::Up, 1);
    show("fade_up");
    println!("bench selection_fade_span {:?}", crate::fade::span(view));

    // The colour the other selections are painted in: the scheme's own where it names one.
    use sourceview5::prelude::BufferExt as _;
    let source = buffer.downcast_ref::<sourceview5::Buffer>();
    for id in ["Adwaita", "solarized-light"] {
        let scheme = sourceview5::StyleSchemeManager::default().scheme(id);
        if let Some(source) = source {
            source.set_style_scheme(scheme.as_ref());
        }
        println!("bench selection_colour {id} {}", view.selection_colour());
    }
    view.clear_carets();
}

/// The line commands where a caret and a column answer differently: Delete Line over a selection,
/// Toggle Comment at a column, `Shift+Delete` left to GTK's Cut binding, and a read-only view
/// refusing the lot. Prints the buffer and every caret after each step.
async fn bench_lines(view: &multicaret::View) {
    use sourceview5::prelude::BufferExt as _;
    let buffer = view.buffer();
    let show = |step: &str| {
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        println!("bench lines_{step} {text:?} {:?}", view.caret_positions());
    };
    let none = gdk::ModifierType::empty();
    let shift = gdk::ModifierType::SHIFT_MASK;

    // Delete Line with one caret takes every line the selection covers, as Duplicate Line does,
    // and a selection ending at a line's start leaves that line out.
    view.clear_carets();
    for (step, to) in [("delete_selected", 6), ("delete_to_line_start", 3)] {
        buffer.set_text("ab\ncd\nef\ngh");
        buffer.select_range(&buffer.iter_at_offset(1), &buffer.iter_at_offset(to));
        editor::delete_line(view.upcast_ref());
        show(step);
    }

    // Toggle Comment: the lines the caret covers, its column kept across the marker, and at a
    // column every caret's line, the column still up afterwards.
    let language = sourceview5::LanguageManager::default().language("rust");
    println!("bench lines_language {}", language.is_some());
    if let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() {
        source.set_language(language.as_ref());
    }
    let code = "    a();\n    b();\n    c();";
    buffer.set_text(code);
    buffer.place_cursor(&buffer.iter_at_offset(6));
    for step in ["comment", "uncomment"] {
        editor::toggle_comment(view.upcast_ref());
        show(step);
    }
    // A selection over the lines is put back over them whole, markers and all, rather than left
    // to the marks, which the marker would have pushed off the front of the first line.
    buffer.set_text(code);
    buffer.select_range(&buffer.iter_at_offset(4), &buffer.iter_at_offset(13));
    editor::toggle_comment(view.upcast_ref());
    let selection = buffer
        .selection_bounds()
        .map(|(start, end)| (start.offset(), end.offset()));
    show("comment_selection");
    println!("bench lines_comment_selection {selection:?}");

    column(view, code);
    for step in ["comment_column", "uncomment_column"] {
        editor::toggle_comment(view.upcast_ref());
        println!("bench lines_{step}_carets {}", view.has_carets());
        show(step);
    }

    // Shift+Delete is GTK's Cut binding, so the column leaves the press to it rather than deleting
    // a character at every caret, and answers the signal it emits with its own whole-line cut.
    column(view, "ab\ncd\nef");
    let passed = view.press(gdk::Key::Delete, shift) == glib::Propagation::Proceed;
    println!("bench lines_shift_delete passed={passed}");
    show("shift_delete");
    view.emit_cut_clipboard();
    show("shift_delete_cut");

    // A read-only view refuses every one of them, as GTK's own keys and the whole-line cut do: a
    // tab holding a file that is not valid UTF-8 is one. The carets still move.
    view.set_editable(false);
    column(view, "ab\ncd\nef");
    view.press(gdk::Key::X, shift);
    view.press(gdk::Key::BackSpace, none);
    view.clipboard().set_text("zz");
    view.emit_paste_clipboard();
    glib::timeout_future(Duration::from_millis(100)).await;
    for command in [
        editor::duplicate_line as fn(&sourceview5::View),
        editor::delete_line,
        editor::newline_below,
        editor::toggle_comment,
    ] {
        command(view.upcast_ref());
    }
    show("read_only");
    view.press(gdk::Key::Right, none);
    show("read_only_moved");
    view.set_editable(true);
    view.clear_carets();
    if let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() {
        source.set_language(None);
    }
}

/// Return at the end of every line of the note at `rel` that opens a list, a quote or an
/// enumeration, then Tab on the line it leaves behind — both through the chain a real press goes
/// through (`editor::keys`), against what [`typing::continuation`] and [`typing::list_indent`] say
/// that line should carry on with. Prints a line per item that came out wrong and a count, so a
/// note where the marker is not carried over names the lines rather than the symptom. Then Tab on
/// lines that already have text on them.
///
/// [`bench_popup`] goes first, while the tab is still untouched: a press marks it dirty and an
/// autosave a second later would write the drill's own text into the note.
pub(super) fn bench_list(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        bench_popup(&app, &tab, tab.text());
    });
}

/// Return and Tab down every list line of the note, and then Tab on lines with text on them.
fn bench_lines_of(app: &Rc<App>, tab: &Rc<Tab>, original: String) {
    // What a tab is worth here: the Indent Width preference.
    println!("bench list_indent_width {}", tab.view.tab_width());
    let lines: Vec<String> = original.lines().map(str::to_string).collect();
    let (mut asked, mut wrong) = (0, 0);
    for (n, line) in lines.iter().enumerate() {
        let Some(typing::Continue::Insert(want)) = typing::continuation(line) else {
            continue;
        };
        asked += 1;
        // From the note as it is every time: each press is about the document the reader has,
        // not about what the press before it left.
        tab.set_text(&original);
        tab.buffer
            .place_cursor(&editor::line_end(&tab.buffer, n as i32));
        let none = gdk::ModifierType::empty();
        editor::press(tab, gdk::Key::Return, none);
        let carried = caret_prefix(tab);
        editor::press(tab, gdk::Key::Tab, none);
        let indented = caret_prefix(tab);
        let wanted_indent = typing::list_indent(&want).map(|step| format!("{step}{want}"));
        if carried != want || Some(&indented) != wanted_indent.as_ref() {
            wrong += 1;
            println!(
                "bench list_line {} want={want:?} carried={carried:?} indent={indented:?}",
                n + 1
            );
        }
    }
    println!("bench list_wrong {wrong} of {asked}");

    // Tab from anywhere on a list line, and the lines where it is still the view's own key. The
    // line the caret ends on and the column it ends in: the indent goes in at the head of the
    // line, so the caret keeps its place in the text.
    for (what, text, at) in [
        ("text", "- item", 4),
        ("head", "- item", 0),
        ("nested", "  - nested", 8),
        ("task", "- [x] done", 9),
        ("quote", "> quoted", 5),
        ("enumerated", "12. twelfth", 6),
        ("prose", "plain text", 5),
        ("blank", "", 0),
    ] {
        tab.set_text(text);
        tab.buffer.place_cursor(&tab.buffer.iter_at_offset(at));
        let answer = editor::press(tab, gdk::Key::Tab, gdk::ModifierType::empty());
        let caret = editor::caret(&tab.buffer);
        println!(
            "bench list_tab_{what} {:?} column={} {answer:?}",
            caret_line(tab),
            caret.line_offset()
        );
    }

    // The note back as it was: the presses above marked the tab dirty, and closing it saves.
    tab.set_text(&original);
    bench_quit(app);
}

/// "The completion popup is up" against the popup itself: an answer that no `hide` ever took back
/// used to leave every key to the view for the rest of a tab's life.
///
/// A words provider gives the tab a popup of its own to raise, so what is read is a real
/// `GtkSourceCompletion` popup and not a stand-in. Prints the view's children either side of it —
/// the widgets the check reads, each `visible/mapped` — then Return at the end of a list item
/// under it, once with no row selected, which is the list's and ends the popup, and once with a
/// row selected, which is the popup's. Then it stages the two ways the old cell was stranded: the
/// view taken off screen with the popup still up and no `hide` emitted, which is a tab switched
/// away from, and the completion's own `show` forged with nothing on screen. Neither may say yes,
/// and Return must still continue a list after both. On a scratch vault:
/// `popup_up true … GtkSourceCompletionList=true/true`, `popup_return Stop "- " up=false`,
/// `popup_selected_return Proceed "- comp" up=true`, `popup_unmapped false …=false/false`,
/// `popup_stale false` and `popup_stale_return "- "`.
///
/// `GtkSourceCompletion` refuses to show while the view has no input focus, and under Xvfb no
/// window manager hands it out: run `build-aux/xtest.py :<display> "move 700 500; focus"` in a
/// loop beside the drill to get `focus=true`, and with it the popup. Without it the run still
/// prints the stale-flag half, `up=false focus=false` naming why the other half is empty.
fn bench_popup(app: &Rc<App>, tab: &Rc<Tab>, original: String) {
    use sourceview5::prelude::CompletionWordsExt as _;

    let completion = sourceview5::prelude::ViewExt::completion(&tab.view);
    let words = sourceview5::CompletionWords::new(None);
    words.register(&tab.buffer);
    completion.add_provider(&words);
    // `set_text` is not an edit, so nothing here arms the autosave that would write it out.
    let staged = "completion\n- comp";
    tab.set_text(staged);

    let (app, tab) = (app.clone(), tab.clone());
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        tab.view.grab_focus();
        tab.buffer.place_cursor(&tab.buffer.end_iter());
        completion.show();
        // The proposals arrive from an idle, and the popup with them.
        glib::timeout_add_local_once(Duration::from_millis(800), move || {
            let none = gdk::ModifierType::empty();
            println!(
                "bench popup_up {} focus={} {:?}",
                tab.popup_shown(),
                tab.view.has_focus(),
                children(&tab)
            );
            let answer = editor::press(&tab, gdk::Key::Return, none);
            println!(
                "bench popup_return {answer:?} {:?} up={}",
                caret_prefix(&tab),
                tab.popup_shown()
            );

            // The popup again, with its first row selected, as an arrow key would leave it. The
            // list paints the row selected on the next frame, which the wait covers.
            tab.set_text(staged);
            tab.buffer.place_cursor(&tab.buffer.end_iter());
            completion.set_select_on_show(true);
            completion.show();
            glib::timeout_add_local_once(Duration::from_millis(800), move || {
                let answer = editor::press(&tab, gdk::Key::Return, none);
                println!(
                    "bench popup_selected_return {answer:?} {:?} up={}",
                    caret_prefix(&tab),
                    tab.popup_shown()
                );
                completion.set_select_on_show(false);

                // The popup still up and the view taken off screen under it: GTK takes the
                // popover with it, but the completion's own `hide` is never emitted, so this is
                // the shape that used to strand the cached answer.
                tab.view.set_visible(false);
                println!(
                    "bench popup_unmapped {} {:?}",
                    tab.popup_shown(),
                    children(&tab)
                );
                tab.view.set_visible(true);

                // The popup gone, and then its `show` forged as a missed `hide` would have left it.
                completion.hide();
                completion.remove_provider(&words);
                words.unregister(&tab.buffer);
                println!(
                    "bench popup_down {} {:?}",
                    tab.popup_shown(),
                    children(&tab)
                );
                completion.emit_show();
                println!("bench popup_stale {}", tab.popup_shown());
                tab.buffer.place_cursor(&editor::line_end(&tab.buffer, 1));
                editor::press(&tab, gdk::Key::Return, none);
                println!("bench popup_stale_return {:?}", caret_prefix(&tab));

                bench_lines_of(&app, &tab, original);
            });
        });
    });
}

/// The view's own children, each as `visible/mapped`: where the completion popup is, the hover
/// assistant and the signature popover beside it. The two flags are printed apart because they
/// disagree exactly where the old cached answer went stale.
fn children(tab: &Rc<Tab>) -> Vec<String> {
    let mut out = Vec::new();
    let mut child = tab.view.first_child();
    while let Some(widget) = child {
        out.push(format!(
            "{}={}/{}",
            widget.type_().name(),
            widget.get_visible(),
            widget.is_mapped()
        ));
        child = widget.next_sibling();
    }
    out
}

/// The whole line the caret is on.
fn caret_line(tab: &Rc<Tab>) -> String {
    let caret = editor::caret(&tab.buffer);
    let mut start = caret;
    start.set_line_offset(0);
    tab.buffer
        .text(&start, &editor::line_end(&tab.buffer, caret.line()), true)
        .to_string()
}

/// The caret's line up to it: the marker a press left in front of what would be typed next.
fn caret_prefix(tab: &Rc<Tab>) -> String {
    editor::line_prefix(&tab.buffer, &editor::caret(&tab.buffer)).to_string()
}

/// A shell focused in a window that does not have the keyboard must not narrow the application's
/// accelerator table, and one in the window that does must. Under Xvfb no window is ever
/// activated, so the active one is the last added: a second window is opened first and the shell
/// then opens in this one, which is the state after switching windows away from a shell. Closing
/// the second window hands the keyboard back through the same `active-window` notify a real
/// switch goes through. Prints what `Ctrl+S` activates: `["win.save"]`, then `[]`.
pub(super) fn bench_shell_keys(app: &Rc<App>) {
    let Some(gtk_app) = app.window.application().and_downcast::<adw::Application>() else {
        return bench_quit(app);
    };
    let Some(other) = app
        .shell
        .upgrade()
        .and_then(|shell| shell.loose_window(&gtk_app, crate::shell::Loose::Documents))
    else {
        return bench_quit(app);
    };
    app.open_terminal();
    let print = move |when: &str| {
        println!(
            "bench shell_keys {when} {:?}",
            gtk_app.actions_for_accel("<Control>s")
        );
    };
    // The shell takes focus from an idle.
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(200), move || {
        print("shell-elsewhere");
        other.window.close();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            print("shell-here");
            bench_quit(&app);
        });
    });
}

/// `ACCENT_BENCH_TERM=1` against `accent --terminal`: what the vault-less shell window calls
/// itself. VTE reports its own title some milliseconds after the shell has started, so the state
/// is printed every 100 ms while it is still changing rather than once.
pub(super) fn bench_term(app: &Rc<App>) {
    // A vault window has no shell of its own; opening one here is the same drill over the tab
    // case, and the state printed right after it is the one before VTE has reported anything.
    if app.terminals().is_empty() {
        app.open_terminal();
    }
    let app = app.clone();
    let (mut said, mut left) = (String::new(), 20);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let shows = format!(
            "window={:?} title={:?} subtitle={:?} page={:?}",
            app.window.title().unwrap_or_default(),
            app.title.title(),
            app.title.subtitle(),
            app.terminals()
                .first()
                .map(|t| t.page.title())
                .unwrap_or_default(),
        );
        if shows != said {
            println!("bench term {shows}");
            said = shows;
        }
        left -= 1;
        if left > 0 {
            return glib::ControlFlow::Continue;
        }
        bench_term_save(&app);
        glib::ControlFlow::Break
    });
}

/// Save Session answered with a name, and whether the primary menu offers Close Session either
/// side of it: the menu used to be the one the window opened with, unnamed, until it was opened
/// again. The shell is closed at the end, so the holder keeps nothing of the drill's. A vault
/// window has no session of shells to save.
fn bench_term_save(app: &Rc<App>) {
    if !app.key.borrow().is_terminal() {
        return bench_quit(app);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        let closes = || {
            let labels = app.menu.menu_model().map(|m| crate::fileops::labels(&m));
            labels.is_some_and(|l| l.iter().any(|l| l == "Close Session"))
        };
        println!("bench term_menu before close_session={}", closes());
        let _ = WidgetExt::activate_action(&app.window, "win.save-session", None);
        glib::timeout_future(Duration::from_millis(500)).await;
        if let Some(dialog) = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
        {
            let entry = dialog
                .extra_child()
                .and_then(|form| find_widget(&form, &|w| w.is::<gtk::Entry>()))
                .and_downcast::<gtk::Entry>();
            if let Some(entry) = entry {
                entry.set_text("bench");
            }
            dialog.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
            dialog.close();
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        // A second run on the same state finds `bench` taken, and is asked before replacing it.
        let replace = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>();
        println!(
            "bench term_menu replace={:?}",
            replace.as_ref().map(|d| (d.heading(), d.body()))
        );
        if let Some(dialog) = replace {
            dialog.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
            dialog.close();
            glib::timeout_future(Duration::from_millis(300)).await;
        }
        println!(
            "bench term_menu after close_session={} window={:?}",
            closes(),
            app.window.title().unwrap_or_default()
        );
        let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
        glib::timeout_future(Duration::from_millis(500)).await;
        println!("bench term_menu held={}", bench_held());
        bench_quit(&app);
    });
}

/// `ACCENT_BENCH_HOLD=open|back`: a shell outliving its window. Run twice on one scratch state,
/// `open` first. `open` sends a `cd` and a marker into the window's shell and quits the way
/// Ctrl+Q does, which writes the session and leaves the shell held; `back` prints what the
/// restore brought back — the key, where the shell is, and whether the marker came back on its
/// screen — then closes the tab and prints how many shells the holder still has. Expected: the
/// same key twice, `at=/tmp`, `replayed=true`, `held=0`. Under `SHELL=/bin/bash`, whose `vte.sh`
/// reports the directory.
///
/// `=early` closes new shells before their `attach` can have had them started: in the same tick,
/// and a few milliseconds in, while the attach waits for the holder it started. Expected
/// `held=0`. Against a vault, whose window opens no shell of its own, so the first one also starts
/// the holder: with one already up (a holder waits ten seconds before it leaves) the same tick
/// proves nothing.
pub(super) fn bench_hold(app: &Rc<App>, step: &str) {
    if step == "early" {
        return bench_hold_early(app);
    }
    if app.terminals().is_empty() {
        app.open_terminal();
    }
    let Some(term) = app.terminals().first().cloned() else {
        return bench_quit(app);
    };
    let (app, open) = (app.clone(), step == "open");
    // Typed once the shell is up rather than into the pty ahead of it: switching the tty to raw
    // mode may flush what is waiting there.
    glib::timeout_add_local_once(Duration::from_millis(1000), move || {
        if open {
            term.view.feed_child(b"cd /tmp && echo held-marker\n");
        }
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            bench_hold_step(&app, &term, open)
        });
    });
}

/// The half of [`bench_hold`] once its shell has had time to answer.
fn bench_hold_step(app: &Rc<App>, term: &Rc<crate::terminal::Term>, open: bool) {
    let (key, at) = (term.key(), term.at().unwrap_or_default());
    // A window without a vault toasts nothing else, so this is 0, or 1 when no `accent-cli` is
    // there to hold its shells: that is said once, however many it opens.
    let toasts = app.toasted.get();
    if open {
        println!("bench hold open key={key} at={at} toasts={toasts}");
        if let Some(gtk_app) = app.window.application() {
            gtk_app.activate_action("quit", None);
        }
        return;
    }
    let replayed = term
        .view
        .text_format(vte4::Format::Text)
        .is_some_and(|text| text.contains("held-marker"));
    println!("bench hold back key={key} at={at} replayed={replayed} toasts={toasts}");
    let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(500), move || {
        println!("bench hold held={}", bench_held());
        bench_quit(&app);
    });
}

/// The `=early` half of [`bench_hold`].
fn bench_hold_early(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        for wait in [0, 10, 30, 60, 120] {
            app.open_terminal();
            glib::timeout_future(Duration::from_millis(wait)).await;
            let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
            glib::timeout_future(Duration::from_millis(2500)).await;
            println!("bench hold early wait={wait} held={}", bench_held());
        }
        bench_quit(&app);
    });
}

/// How many shells `accent-cli held` lists, or why it could not say.
fn bench_held() -> String {
    let Some(cli) = crate::terminal::cli() else {
        return "no-accent-cli".to_string();
    };
    match std::process::Command::new(cli).arg("held").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .count()
            .to_string(),
        Ok(out) => format!("failed:{}", out.status),
        Err(e) => format!("failed:{e}"),
    }
}
