//! Drills over the keyboard: the editor's key semantics and a shell's accelerators.

use super::*;

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
