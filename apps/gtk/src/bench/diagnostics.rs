//! Probe diagnostic visibility toggles while retaining status counts and hover text.

use super::*;
use accent_api::{Diagnostic, Pos, Range, Severity};

/// `ACCENT_BENCH_DIAG=<rel_code_file>` injects an error, warning and hint, then toggles
/// visibility twice. `counted` and `hover` must remain unchanged; `underlined`, `marks`
/// and end-of-line messages disappear and return. Use a file without a running language
/// server, whose real publish could replace the injected diagnostics.
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

/// Check that `caret` and `gutter` report the same line after four caret moves.
/// GTK caches the gutter render node separately, so caret moves must invalidate it.
/// Enable line numbers first; a hidden renderer cannot report its painted line.
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

/// Type after an overlong diagnostic message and require `cut` to equal `fits`
/// before the next frame and after settling, catching delayed clipping updates.
/// Use a scratch vault (`make vault VAULT=/tmp/…`): restored text still saves on quit.
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

/// Insert a line above a diagnostic on line 2 without republishing. Its underline,
/// gutter mark and message must all move to line 3 immediately and remain there
/// after the 150 ms width-refit debounce.
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
        code: None,
    }
}
