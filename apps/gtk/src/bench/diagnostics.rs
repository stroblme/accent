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
        bench_quit(&app);
    });
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
