//! The diagnostics drill: what pressing the status bar's count keeps out of the text, and what it
//! deliberately leaves behind.

use super::*;
use accent_api::{Diagnostic, Pos, Range, Severity};

/// `ACCENT_BENCH_DIAG=<rel_code_file>` opens a code file, hands its tab the answer a language
/// server would publish — an error, a warning and a hint — and prints what the status bar says,
/// whether the count is a control, and how much of that answer the text is carrying. Then it
/// presses the count twice, printing the same each time.
///
/// `counted` and `hover` must not change across the three lines: the readout and the pointer are
/// what a hidden diagnostic is still readable through. `underlined` and `marks` must go to 0 on
/// the press and come back on the second — as do the messages at the ends of the lines, which the
/// same pass adds and which the provider has no way to count back.
///
/// The server is not waited for: a real publish would replace what this hands over, so point the
/// drill at a file no language server here answers for, or read the first line only.
pub(super) fn bench_diagnostics(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        // By name: a session restore can have put other tabs back, and the count belongs to the
        // document in front rather than to whichever tab is first.
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        tab.set_diagnostics(vec![
            diagnostic(Severity::Error, 0, 0, 3),
            diagnostic(Severity::Warning, 1, 0, 4),
            // A hint is never loud: no gutter mark and no message, an underline and nothing else.
            diagnostic(Severity::Hint, 2, 0, 2),
        ]);
        app.sync_status();
        let count = app.statusbar.facts_control();
        for label in ["published", "pressed", "pressed_again"] {
            let (underlined, marks) = crate::diagnostics::painted(&tab.buffer);
            println!(
                "bench diag case={label} counted={:?} pressable={} tooltip={:?} hidden={} \
                 underlined={underlined} marks={marks} hover={}",
                crate::diagnostics::counts(&tab.diagnostics()),
                count.can_target(),
                count.tooltip_text().map(|t| t.to_string()),
                tab.diagnostics_hidden(),
                crate::diagnostics::at(
                    &tab.diagnostics(),
                    Pos {
                        line: 1,
                        character: 1
                    }
                )
                .len(),
            );
            count.emit_clicked();
            bench_pump();
        }
        bench_gutter(&tab);
        bench_refit(&tab);
        bench_moved(&tab);
        bench_quit(&app);
    });
}

/// Where the gutter last drew the caret's highlight, beside where the caret really is: the
/// line-number column's own current-line mark, which a code tab has and which has to follow every
/// way the caret moves.
///
/// `caret` and `gutter` must name the same line in all four steps. They did not before the
/// renderer was told to redraw itself on a caret move: GTK4 keeps a widget's render node until
/// that widget is invalidated, a caret move invalidates the view and not the gutter renderer
/// inside it, and the highlight stayed on the line the caret had left until the pointer entered
/// the column and changed its opacity.
///
/// The numbers are turned on first, whatever the scratch config says: a hidden renderer draws
/// nothing, and `gutter` then reads `None`.
fn bench_gutter(tab: &Rc<Tab>) {
    tab.set_line_numbers(true);
    let step = |label: &str| {
        bench_frame();
        println!(
            "bench diag gutter case={label} caret={} gutter={:?}",
            tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line(),
            tab.gutter_cursor()
        );
    };
    step("opened");
    // What a click in the text, or a Go to Line, does to the caret.
    if let Some(line) = tab.buffer.iter_at_line(3) {
        tab.buffer.place_cursor(&line);
    }
    step("placed");
    // And what Down does. The signal is the key binding's own handler, so no key is pressed.
    tab.view
        .emit_move_cursor(gtk::MovementStep::DisplayLines, 1, false);
    step("arrow");
    tab.buffer.place_cursor(&tab.buffer.start_iter());
    step("home");
}

/// An error whose message is too long for its line's room, then typing at the end of that line:
/// `cut` is the characters of the message shown and `fits` those the line has room for, which
/// must agree as published, as typed (read before the frame that shows the keystroke) and once
/// settled. As typed they did not: the message was cut again only by a publish or by the
/// width's own refit 150 ms after a layout, and ran past the column's edge until then. The file's own text goes back at the end, but the typing has marked the tab,
/// which writes it on its way out: point the drill at a scratch vault (`make vault VAULT=/tmp/…`).
fn bench_refit(tab: &Rc<Tab>) {
    let own = tab.text();
    let mut long = diagnostic(Severity::Error, 0, 0, 1);
    long.message = "a message long enough to be cut at the column's edge ".repeat(8);
    // The steps above end on a third press of the count, which hid them.
    tab.hide_diagnostics(false);
    tab.set_diagnostics(vec![long]);
    bench_frame();
    let says = |case: &str| {
        for (cut, fits) in tab.cuts() {
            println!("bench diag refit case={case} cut={cut} fits={fits}");
        }
    };
    says("published");
    let Some(mut end) = tab.buffer.iter_at_line(0) else {
        return;
    };
    end.forward_to_line_end();
    tab.buffer.place_cursor(&end);
    // A word at a time, and short of a wrap: a wrapped line ends further left again.
    for _ in 0..3 {
        tab.buffer.insert_at_cursor(" typed");
    }
    says("typed");
    bench_frame();
    says("settled");
    tab.set_text(&own);
}

/// An error on line 2, then a line typed in above it with no publish after: the lines its
/// underline, gutter mark and end-of-line message are on, as published, as typed, and once the
/// width refit's 150 ms debounce has passed. All three must say 3 from the typing on. The message
/// said 2 throughout, GtkSourceView pinning it to a line number, and once settled the other two
/// did as well: the view notifies its width on every layout, and the refit laid the publish again
/// at its old positions.
fn bench_moved(tab: &Rc<Tab>) {
    let own = tab.text();
    tab.set_diagnostics(vec![diagnostic(Severity::Error, 2, 0, 4)]);
    bench_frame();
    let says = |case: &str| {
        let (underline, mark) = crate::diagnostics::error_lines(&tab.buffer);
        println!(
            "bench diag moved case={case} underline={underline:?} mark={mark:?} message={:?}",
            tab.message_lines()
        );
    };
    says("published");
    tab.buffer
        .insert(&mut tab.buffer.start_iter(), "typed above\n");
    says("typed");
    // Two frames' waits: past the refit's debounce.
    bench_frame();
    bench_frame();
    says("settled");
    tab.set_text(&own);
}

/// Wait for a frame. The frame clock ticks on a timer, so pumping the main loop alone paints
/// nothing and a probe that reads what was painted would read the frame before the move.
fn bench_frame() {
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(10));
        bench_pump();
    }
}

pub(super) fn diagnostic(severity: Severity, line: u32, from: u32, to: u32) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Pos {
                line,
                character: from,
            },
            end: Pos {
                line,
                character: to,
            },
        },
        severity,
        message: format!("{severity:?} on line {}", line + 1),
        source: None,
    }
}
