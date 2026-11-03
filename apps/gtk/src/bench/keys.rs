//! Keyboard drills for editor keys, carets, selections and line commands in a view of their own.

use super::keys_typing::{bench_code_pairs, bench_indent, bench_popup_keys, bench_table};
use super::*;
use accent_core::note::Syntax;

/// Drive word deletion, screen-row movement and multicaret semantics through binding signals
/// and [`multicaret::View::press`]. Use an isolated view/window, requiring a display but no
/// input or vault writes. `make drills` compares every printed buffer/caret result with
/// `keys.expected`.
pub(super) fn bench_keys(app: &Rc<App>) {
    let view = multicaret::View::new();
    // Monospace, as code is: a caret going up and down aims for its place on screen, which is
    // then its column.
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::Word);
    // A tab's whole-line cut and copy, which the column drills go through.
    editor::line_clipboard(view.upcast_ref());
    // Scrolled, as in a tab, so a page is what is on screen rather than the whole buffer.
    let window = gtk::Window::builder()
        .default_width(320)
        .default_height(240)
        .child(&gtk::ScrolledWindow::builder().child(&view).build())
        .build();
    // The buffer off the primary selection while the view is up: GTK lets the primary caret's
    // selection go whenever another widget takes that selection, which the window starting up
    // beside this one may do mid-drill, and `make drills` wants the same printout every run.
    // Realizing puts the buffer on it before this runs, and it is put back before unrealizing
    // takes it off again.
    view.connect_realize(|view| {
        view.buffer()
            .remove_selection_clipboard(&view.primary_clipboard());
    });
    view.connect_unrealize(|view| {
        view.buffer()
            .add_selection_clipboard(&view.primary_clipboard());
    });
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

        // Down is one screen row, the wrapped line's next, in code as in prose.
        buffer.set_text(&format!("{}\nshort\ntail", "wide ".repeat(80)));
        buffer.place_cursor(&buffer.iter_at_offset(3));
        let mut row = buffer.start_iter();
        let wraps = view.forward_display_line(&mut row) && row.line() == 0;
        println!("bench wraps {wraps}");
        view.emit_move_cursor(gtk::MovementStep::DisplayLines, 1, false);
        let at = buffer.iter_at_mark(&buffer.get_insert());
        println!(
            "bench down_line {} {}",
            at.line(),
            at.line_offset() > row.line_offset()
        );

        // End goes to the end of the screen row, a second End to the end of the line, and Home
        // back to the row's start, then to the line's: VS Code's keys on a wrapped line.
        let shown = |step: &str| {
            let at = buffer.iter_at_mark(&buffer.get_insert());
            println!("bench {step} {} {}", at.line(), at.line_offset());
        };
        buffer.place_cursor(&buffer.iter_at_offset(3));
        view.emit_move_cursor(gtk::MovementStep::DisplayLineEnds, 1, false);
        shown("end_row");
        view.emit_move_cursor(gtk::MovementStep::DisplayLineEnds, 1, false);
        shown("end_line");
        buffer.place_cursor(&buffer.iter_at_offset(row.offset() + 3));
        view.emit_move_cursor(gtk::MovementStep::DisplayLineEnds, -1, false);
        println!(
            "bench home_row {}",
            buffer.iter_at_mark(&buffer.get_insert()) == row
        );
        view.emit_move_cursor(gtk::MovementStep::DisplayLineEnds, -1, false);
        shown("home_line");
        // An indented line: Home goes to its first character that is not a space, then to its
        // start, and back.
        buffer.set_text("    indented");
        buffer.place_cursor(&buffer.end_iter());
        for _ in 0..3 {
            view.emit_move_cursor(gtk::MovementStep::DisplayLineEnds, -1, false);
            shown("home_indent");
        }

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

        // Add Caret Above after two Add Caret Below takes the newest caret back rather than
        // adding one above; once only the primary is left, it adds above again.
        buffer.set_text("a\nb\nc\nd\ne");
        buffer.place_cursor(&buffer.iter_at_offset(4));
        view.add_caret(true);
        view.add_caret(true);
        view.add_caret(false);
        println!("bench caret_take_back {:?}", view.caret_positions());
        view.add_caret(false);
        view.add_caret(false);
        println!("bench caret_turn {:?}", view.caret_positions());
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
            bench_box(&view).await;
            bench_grow(&view);
            bench_folds(&view).await;
            bench_table(&view);
            bench_indent(&view);
            bench_code_pairs(&view);
            bench_whole_words(&view);
            bench_popup_keys(&view).await;
            bench_tab_focus().await;
            window.close();
            bench_quit(&app);
        });
    });
}

/// Where a click on a tab leaves the keyboard, and what the accelerator table makes of it: the
/// tab in front's label counts as the tab, editing chords and all, and a shell clicked to the
/// front has the keyboard in its VTE. In a window of its own; the shell starts no process.
async fn bench_tab_focus() {
    let tabs = adw::TabView::new();
    let bar = adw::TabBar::builder().view(&tabs).autohide(false).build();
    let note = gtk::TextView::new();
    let note_page = tabs.append(&note);
    let shell = vte4::Terminal::new();
    let shell_page = tabs.append(&crate::terminal::scroller(&shell));
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&bar);
    column.append(&tabs);
    let window = gtk::Window::builder()
        .default_width(320)
        .default_height(240)
        .child(&column)
        .build();
    window.present();
    glib::timeout_future(Duration::from_millis(200)).await;
    let print = |when: &str| {
        let focus = gtk::prelude::GtkWindowExt::focus(&window);
        let name = focus.as_ref().map(|w| w.type_().name());
        let keys = crate::actions::Keys::at(focus);
        println!("bench tab_focus {when} {name:?} {keys:?}");
    };
    note.grab_focus();
    print("document");
    // Where libadwaita puts the keyboard on a second click on the tab in front.
    bar.child_focus(gtk::DirectionType::TabForward);
    print("label");
    // The shell's tab clicked from the note it was left for: the tab view gives the keyboard back
    // to the shell, then libadwaita asks the page for it.
    tabs.set_selected_page(&shell_page);
    shell.grab_focus();
    tabs.set_selected_page(&note_page);
    tabs.set_selected_page(&shell_page);
    shell_page.child().grab_focus();
    print("shell");
    window.close();
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
    let (none, ctrl) = (
        gdk::ModifierType::empty(),
        crate::platform::PRIMARY_MODIFIER,
    );
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
    // The view scrolls by the same page, so the top line on screen is the primary's again. Lines
    // counted from that top, because how many a page holds is the font's.
    let top = view.line_at_y(view.visible_rect().y()).0.line();
    let carets: Vec<_> = view
        .caret_positions()
        .into_iter()
        .map(|(line, column)| (line - top, column))
        .collect();
    println!("bench column_page_down {carets:?} scrolled={}", top > 0);
    view.clear_carets();
}

/// Box selection as a `Shift+Alt` drag makes it ([`multicaret::View::select_box`]), from a press
/// on one line to the pointer on another, in pixels: every line between gets a caret selecting
/// what lies between the two, a line ending short of the box an empty caret at its end, one
/// ending inside it its text to the end, and a wrapped line one caret, on its row nearest the
/// press. A press that does not move is a caret. Prints every caret, the primary first.
async fn bench_box(view: &multicaret::View) {
    let buffer = view.buffer();
    view.clear_carets();
    let wrapped = "wide ".repeat(80);
    buffer.set_text(&format!(
        "0123456789\n0\n0123\n\n0123456789\n{wrapped}\n0123456789"
    ));
    // Laid out first, or the lines' heights are still estimates.
    glib::timeout_future(Duration::from_millis(200)).await;
    // A pixel inside the character at `column` of `line`, on its first row, in buffer coordinates.
    let point = |line: i32, column: i32| {
        let mut at = buffer.iter_at_line(line).unwrap_or(buffer.end_iter());
        at.set_line_offset(column);
        let rect = view.iter_location(&at);
        (rect.x() + 1, rect.y() + rect.height() / 2)
    };
    let show = |step: &str| {
        println!(
            "bench box_{step} {:?} carets={}",
            view.caret_positions(),
            view.has_carets()
        )
    };
    view.select_box(point(0, 2), point(4, 6));
    show("down");
    view.select_box(point(4, 6), point(0, 2));
    show("up_left");
    // Line 5 is the wrapped one: its first row going down, its last going up.
    view.select_box(point(4, 2), point(6, 6));
    show("wrap_down");
    view.select_box(point(6, 2), point(4, 6));
    show("wrap_up");
    view.select_box(point(2, 1), point(2, 1));
    show("click");
}

/// Add Caret Below and Above with a word selected grow a box from it: the same columns on each
/// line added, a line short of them an empty caret at its end and one ending inside them selected
/// to its end; the other way takes back the line added last, and once only the word is left it
/// grows that way. Prints every selection as its line and columns, top to bottom.
fn bench_grow(view: &multicaret::View) {
    let buffer = view.buffer();
    let show = |step: &str| {
        let spans: Vec<_> = view
            .selections()
            .iter()
            .map(|(from, to)| {
                format!(
                    "{}:{}-{}",
                    from.line(),
                    from.line_offset(),
                    to.line_offset()
                )
            })
            .collect();
        println!("bench grow_{step} {}", spans.join(" "));
    };
    view.clear_carets();
    buffer.set_text("one two three\nalpha beta\nxy\ngamma de\nomega zeta");
    buffer.select_range(&at(&buffer, 1, 10), &at(&buffer, 1, 6));
    for _ in 0..3 {
        view.add_caret(true);
        show("down");
    }
    view.add_caret(false);
    show("take_back");
    for _ in 0..2 {
        view.add_caret(false);
    }
    show("original");
    view.add_caret(false);
    show("up");
    view.press(gdk::Key::X, gdk::ModifierType::SHIFT_MASK);
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    println!("bench grow_typed {text:?}");
    view.clear_carets();
}

/// Up and Down over a shut fold, in code and in prose, at one caret and at a column, and Add Caret
/// onto a line the fold hides. Prints every caret, the primary first, and whether the block is
/// still shut.
async fn bench_folds(view: &multicaret::View) {
    let buffer = view.buffer();
    if let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() {
        crate::fold::install_tag(source);
    }
    let shut = |header: u32, last: u32| {
        let fold = accent_api::Fold {
            start_line: header,
            end_line: last,
        };
        crate::fold::fold(&buffer, fold);
    };
    let show = |step: &str| {
        println!(
            "bench fold_{step} {:?} shut={}",
            view.caret_positions(),
            crate::fold::is_folded(&buffer, 1)
        );
    };
    let text = "x0\nhead\n  a\n  b\ntail\nend";
    view.clear_carets();

    // One caret steps over the block, from its header to the line after it and back.
    buffer.set_text(text);
    shut(1, 3);
    buffer.place_cursor(&at(&buffer, 1, 0));
    glib::timeout_future(Duration::from_millis(100)).await;
    view.emit_move_cursor(gtk::MovementStep::DisplayLines, 1, false);
    show("down");
    view.emit_move_cursor(gtk::MovementStep::DisplayLines, -1, false);
    show("up");
    // A block reaching the end of the text: Down from its header has nowhere to go.
    buffer.set_text("x0\nhead\n  a");
    shut(1, 2);
    buffer.place_cursor(&at(&buffer, 1, 2));
    glib::timeout_future(Duration::from_millis(100)).await;
    view.emit_move_cursor(gtk::MovementStep::DisplayLines, 1, false);
    show("down_at_end");
    // A column at the end of the text: Down from the header has nowhere to go either.
    buffer.place_cursor(&at(&buffer, 0, 1));
    view.add_caret(true);
    glib::timeout_future(Duration::from_millis(100)).await;
    view.press(gdk::Key::Down, gdk::ModifierType::empty());
    show("column_down_at_end");
    view.clear_carets();

    // A column steps over it too.
    buffer.set_text(text);
    buffer.place_cursor(&at(&buffer, 0, 1));
    view.add_caret(true);
    shut(1, 3);
    view.press(gdk::Key::Down, gdk::ModifierType::empty());
    show("column_down");
    view.press(gdk::Key::Up, gdk::ModifierType::empty());
    show("column_up");
    view.clear_carets();

    // Add Caret goes by the lines of the text, and opens the block it adds a caret in.
    buffer.set_text(text);
    shut(1, 3);
    buffer.place_cursor(&at(&buffer, 1, 1));
    view.add_caret(true);
    show("add_below");
    view.clear_carets();

    // A block shut over a column brings the carets it hides out to its header's end, where they
    // merge, the primary too, and the column stays up.
    for (primary, below) in [(0, true), (3, false)] {
        crate::fold::unfold_all(&buffer);
        buffer.place_cursor(&at(&buffer, primary, 1));
        for _ in 0..3 {
            view.add_caret(below);
        }
        view.keeping_carets(|| shut(1, 3));
        show(&format!("shut_over_column_{primary}"));
        view.clear_carets();
    }
    crate::fold::unfold_all(&buffer);
}

/// A note's block of code coloured as the language it names, in the colour a code tab gives the
/// same code, light and dark, and a fence that names none left alone.
async fn bench_code_blocks(buffer: &gtk::TextBuffer) {
    let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() else {
        return;
    };
    let python = sourceview5::LanguageManager::default().language("python3");
    let Some(python) = python else {
        return;
    };
    source.set_text("```python\ndef f():\n    return 1\n```\n```\ndef g():\n```");
    crate::highlight::apply(source, Syntax::Markdown);
    let code = sourceview5::Buffer::with_language(&python);
    code.set_text("def f():");
    let colour = |buffer: &sourceview5::Buffer, line: i32| {
        let at = buffer.iter_at_line(line)?;
        let tags = at.tags().into_iter().rev();
        tags.filter(|t| t.is_foreground_set())
            .find_map(|t| t.foreground_rgba())
    };
    use accent_core::config::Theme;
    let style = adw::StyleManager::default();
    let mut seen = Vec::new();
    for (theme, dark) in [(Theme::Light, false), (Theme::Dark, true)] {
        crate::theme::apply(theme);
        // libadwaita tells the theme's half after a turn of the main loop.
        for _ in 0..40 {
            if style.is_dark() == dark {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        editor::sync_scheme(&code);
        code.ensure_highlight(&code.start_iter(), &code.end_iter());
        crate::codeblocks::restyle(source);
        seen.push(colour(source, 1));
        let tab = colour(source, 1) == colour(&code, 0);
        println!(
            "bench code_block dark={} coloured={} as_code_tab={tab} plain={}",
            style.is_dark(),
            colour(source, 1).is_some(),
            colour(source, 5).is_none()
        );
    }
    crate::theme::apply(Theme::System);
    crate::codeblocks::restyle(source);
    println!("bench code_block themes_differ={}", seen[0] != seen[1]);
}

/// Add Caret at Next Occurrence and Select All Occurrences of a word the tab took from under the
/// caret ([`multicaret::View::match_whole_words`]) match whole words, passing over the `foo` in
/// `foo_bar` and `xfoo`; of a selection made by hand, any. Prints every selection as offsets.
fn bench_whole_words(view: &multicaret::View) {
    let buffer = view.buffer();
    let show = |step: &str| {
        let spans: Vec<(i32, i32)> = view
            .selections()
            .iter()
            .map(|(start, end)| (start.offset(), end.offset()))
            .collect();
        println!("bench occur_{step} {spans:?}");
    };
    for (by, whole) in [("by_hand", false), ("word", true)] {
        view.clear_carets();
        buffer.set_text("a.foo foo_bar xfoo foo");
        buffer.select_range(&at(&buffer, 0, 5), &at(&buffer, 0, 2));
        if whole {
            view.match_whole_words();
        }
        view.add_next_occurrence();
        show(&format!("next_{by}"));
        view.select_all_occurrences();
        show(&format!("all_{by}"));
    }
    view.clear_carets();
}

/// The iter at `column` of `line`, or the end of the buffer past its last line.
pub(super) fn at(buffer: &gtk::TextBuffer, line: i32, column: i32) -> gtk::TextIter {
    let mut at = buffer.iter_at_line(line).unwrap_or(buffer.end_iter());
    at.set_line_offset(column);
    at
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
        crate::platform::PRIMARY_MODIFIER,
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
/// Toggle Comment at a column, Toggle Task over both, `Shift+Delete` left to GTK's Cut binding,
/// and a read-only view refusing the lot. Prints the buffer and every caret after each step.
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
        editor::toggle_comment(view.upcast_ref(), editor::Flavour::Code);
        show(step);
    }
    // A selection over the lines is put back over them whole, markers and all, rather than left
    // to the marks, which the marker would have pushed off the front of the first line.
    buffer.set_text(code);
    buffer.select_range(&buffer.iter_at_offset(4), &buffer.iter_at_offset(13));
    editor::toggle_comment(view.upcast_ref(), editor::Flavour::Code);
    let selection = buffer
        .selection_bounds()
        .map(|(start, end)| (start.offset(), end.offset()));
    show("comment_selection");
    println!("bench lines_comment_selection {selection:?}");

    column(view, code);
    for step in ["comment_column", "uncomment_column"] {
        editor::toggle_comment(view.upcast_ref(), editor::Flavour::Code);
        println!("bench lines_{step}_carets {}", view.has_carets());
        show(step);
    }
    // Carets on a commented line and an uncommented one apart: decided once over both, so both
    // are commented, as VS Code does, where each deciding alone swapped them.
    view.clear_carets();
    buffer.set_text("    // x();\n    b();\n    x();");
    buffer.select_range(&at(&buffer, 0, 8), &at(&buffer, 0, 7));
    view.add_next_occurrence();
    editor::toggle_comment(view.upcast_ref(), editor::Flavour::Code);
    show("comment_mixed_column");

    // A note comments as its syntax does, having no language: markdown puts the lines in one
    // `<!-- … -->` and takes it off, the caret staying on its text; org puts `# ` before each
    // line, a `#+KEY:` line counting as no comment.
    view.clear_carets();
    if let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() {
        source.set_language(None);
    }
    let markdown = editor::Flavour::Note(Syntax::Markdown);
    buffer.set_text("one\ntwo");
    buffer.place_cursor(&buffer.iter_at_offset(1));
    for step in ["note_comment", "note_uncomment"] {
        editor::toggle_comment(view.upcast_ref(), markdown);
        show(step);
    }
    buffer.select_range(&buffer.iter_at_offset(1), &buffer.end_iter());
    editor::toggle_comment(view.upcast_ref(), markdown);
    show("note_comment_lines");
    buffer.set_text("#+TITLE: x\n# y");
    for step in ["org_comment", "org_uncomment"] {
        buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
        editor::toggle_comment(view.upcast_ref(), editor::Flavour::Note(Syntax::Org));
        show(step);
    }

    // Toggle Task: a selection's lines a step each, its blank line passed over and the selection
    // still on its text; every caret's line, a blank one too. Each one undo step.
    view.clear_carets();
    buffer.set_text("- [ ] a\n\nb");
    buffer.select_range(&at(&buffer, 0, 6), &buffer.end_iter());
    editor::toggle_task(view.upcast_ref(), Syntax::Markdown);
    let selection = buffer
        .selection_bounds()
        .map(|(start, end)| buffer.text(&start, &end, true).to_string());
    show("task");
    println!("bench lines_task_selection {selection:?}");
    buffer.undo();
    show("task_undo");
    column(view, "a\n\n- [x] c");
    editor::toggle_task(view.upcast_ref(), Syntax::Markdown);
    show("task_column");
    view.press(gdk::Key::z, crate::platform::PRIMARY_MODIFIER);
    show("task_column_undo");
    // Nor a line of the front matter or of a code block, as the styling pass tags them, a caret's
    // own line included.
    view.clear_carets();
    if let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() {
        crate::highlight::install_tags(source);
        source.set_text("---\na: b\n---\n\n```\nx\n\n```\n    y\nz");
        crate::highlight::apply(source, Syntax::Markdown);
    }
    buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
    editor::toggle_task(view.upcast_ref(), Syntax::Markdown);
    show("task_verbatim");
    for line in [1, 6] {
        buffer.place_cursor(&at(&buffer, line, 0));
        editor::toggle_task(view.upcast_ref(), Syntax::Markdown);
    }
    show("task_verbatim_carets");
    // An org note's: a box along `[-]` and `[X]`, a heading's keyword along the file's `#+TODO`,
    // its priority and tags kept, the `#+` lines and a block passed over, read from its text.
    view.clear_carets();
    buffer
        .set_text("#+TODO: NEXT | DONE\n* [#A] Plan :t:\n- [-] a\n#+begin_src sh\nx\n#+end_src\nb");
    buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
    editor::toggle_task(view.upcast_ref(), Syntax::Org);
    show("task_org");
    bench_code_blocks(&buffer).await;

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
        |view| editor::toggle_comment(view, editor::Flavour::Code),
        |view| editor::toggle_task(view, Syntax::Markdown),
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
