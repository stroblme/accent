//! Drills over the keyboard: the editor's key semantics and a shell's accelerators.

use super::*;

/// Drive the key semantics [`multicaret::View`] corrects — the wordwise deletes, logical-line
/// Up/Down, and the same chords at a column of carets — through the very signals the key bindings
/// emit, and print what the buffer and the carets came out as.
///
/// A view of its own in a window of its own, so nothing is written into a vault and the drills do
/// not depend on a document being open. It needs a display, which is why this is a bench hook and
/// not a unit test, but it needs no key press and no pointer: the two signals are actions, and
/// [`multicaret::View::press`] is the key controller's own handler.
pub(super) fn bench_keys(app: &Rc<App>) {
    let view = multicaret::View::new();
    // The drills are about the code flavours, which is where the logical-line moves are wanted.
    view.set_logical_lines(true);
    view.set_wrap_mode(gtk::WrapMode::Word);
    let window = gtk::Window::builder()
        .default_width(320)
        .default_height(240)
        .child(&view)
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

        window.close();
        bench_quit(&app);
    });
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
        .and_then(|shell| shell.loose_window(&gtk_app))
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
