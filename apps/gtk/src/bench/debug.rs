//! A debugpy session walked through the window's own commands (`ACCENT_BENCH_DEBUG`).

use super::*;
use crate::debug::breakpoints;
use sourceview5::prelude::BufferExt as _;
use vte4::TerminalExt as _;

/// How long each step may take; on a remote vault the adapter and the program start on the host.
const WAIT: Duration = Duration::from_secs(30);

/// What the Debug Console is asked while the program is paused.
const REPL: &str = "x * 2";

/// `ACCENT_BENCH_DEBUG=<rel>:<line>` debugs the program `make drills` writes, whose `double(x)`
/// is called with 21, from a breakpoint on 1-based `<line>`, `y = x * 2`. It prints what the
/// window shows at each step, `bench debug …`: the breakpoint, the stop, the top call, its
/// locals, the Debug Console's answer to [`REPL`], the hover's value of the first name after the
/// line's `=`, the line a step over lands on, the session's exit code, the program's output in
/// its tab, that tab kept, and the Code pane shown with the program's file in front and hidden
/// behind its tab. Timings go on `bench debug_ms …` lines; a step that never comes prints
/// `bench debug timeout <what>` and quits.
pub(super) fn bench_debug(app: &Rc<App>, arg: &str) {
    let place = arg
        .rsplit_once(':')
        .and_then(|(rel, line)| Some((rel.to_string(), line.parse::<u32>().ok()?.checked_sub(1)?)));
    let Some((rel, line)) = place else {
        println!("bench debug usage <rel>:<line>");
        return bench_quit(app);
    };
    let app = app.clone();
    glib::spawn_future_local(async move {
        // A remote vault opens a file once it is connected and has listed the host's tree.
        if let Some(remote) = app.vault().and_then(|v| v.remote()).cloned() {
            let connected = || remote.state() == accent_api::remote::State::Connected;
            let linked = || (app.reconciled.get() && connected()).then_some(());
            if until(&app, "link", linked).await.is_none() {
                return;
            }
        }
        app.open_path(&rel);
        let opened = || app.tab_for(&rel).filter(|tab| tab.buffer.char_count() > 0);
        let Some(tab) = until(&app, "tab", opened).await else {
            return;
        };
        app.toggle_breakpoint(&tab, line);
        for at in breakpoints::lines(&tab) {
            println!("bench debug breakpoint {rel}:{}", at + 1);
        }

        let t = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.debug-start", None);
        let Some(at) = until(&app, "stopped", || paused_line(&tab)).await else {
            return;
        };
        let state = shown(&app).0;
        let why = state.strip_prefix("Paused on ").unwrap_or(&state);
        println!("bench debug stopped {rel}:{} {why}", at + 1);
        println!("bench debug_ms stopped {:.0}", ms_since(t));
        let Some(top) = until(&app, "top", || shown(&app).1.first().cloned()).await else {
            return;
        };
        println!("bench debug top {top}");
        let locals = || Some(shown(&app).2).filter(|values| !values.is_empty());
        let Some(locals) = until(&app, "locals", locals).await else {
            return;
        };
        println!("bench debug locals {}", locals.join(" "));

        // In front with the keyboard, as a reader brings it forward to type at it: the console
        // opened behind the program's tab. VTE takes in what is fed to it unrealised as well.
        let t = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.debug-console", None);
        app.console_typed(&format!("{REPL}\r"));
        let answered = || app.console().and_then(|term| answer(&text(&term), REPL));
        let Some(answer) = until(&app, "repl", answered).await else {
            return;
        };
        println!("bench debug repl {answer}");
        println!("bench debug_ms repl {:.0}", ms_since(t));

        let t = Instant::now();
        let hovered = line_text(&tab, line)
            .find("= ")
            .and_then(|column| {
                tab.buffer
                    .iter_at_line_offset(line as i32, column as i32 + 2)
            })
            .and_then(|start| crate::hover::value_of(&tab, &start));
        let Some((expression, asked)) = hovered else {
            println!("bench debug timeout hover");
            return bench_quit(&app);
        };
        match glib::future_with_timeout(WAIT, asked).await {
            Ok(Ok(value)) => println!("bench debug hover {expression} = {}", value.result),
            Ok(Err(e)) => println!("bench debug hover {expression} failed: {e}"),
            Err(_) => {
                println!("bench debug timeout hover");
                return bench_quit(&app);
            }
        }
        println!("bench debug_ms hover {:.0}", ms_since(t));

        let t = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.debug-step-over", None);
        let stepped = || paused_line(&tab).filter(|now| *now != at);
        let Some(at) = until(&app, "step", stepped).await else {
            return;
        };
        println!("bench debug step {rel}:{}", at + 1);
        println!("bench debug_ms step {:.0}", ms_since(t));

        let t = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.debug-continue", None);
        let ended = || (!app.debugger.running()).then_some(());
        if until(&app, "ended", ended).await.is_none() {
            return;
        }
        println!("bench debug_ms ended {:.0}", ms_since(t));
        let code = || app.console().and_then(|term| exit_code(&text(&term)));
        let Some(code) = until(&app, "exit code", code).await else {
            return;
        };
        println!("bench debug ended {code}");
        let Some(program) = program_tab(&app) else {
            println!("bench debug timeout program");
            return bench_quit(&app);
        };
        let output = || {
            let text = text(&program);
            text.lines()
                .find(|l| l.starts_with("total "))
                .map(String::from)
        };
        let Some(output) = until(&app, "output", output).await else {
            return;
        };
        println!("bench debug output {output}");
        // Once it has said how it ended: a shell's tab would close there.
        let said = || text(&program).contains("[Exited with code").then_some(());
        if until(&app, "program end", said).await.is_none() {
            return;
        }
        let kept = program_tab(&app).is_some_and(|now| Rc::ptr_eq(&now, &program));
        println!(
            "bench debug program {}",
            if kept { "kept" } else { "closed" }
        );
        // The end leaves the strip over the program's file, which can be debugged again; a tab
        // that cannot takes the pane away.
        let code = || match app.sidebar.get().is_some_and(|s| s.has_pane("code")) {
            true => "shown",
            false => "hidden",
        };
        let front = |page: &adw::TabPage| {
            if let Some(pane) = app.pane_of(page) {
                pane.tabs.set_selected_page(page);
            }
        };
        front(&tab.page);
        println!("bench debug pane {} {}", code(), shown(&app).0);
        front(&program.page);
        println!("bench debug pane {} behind the program", code());
        bench_quit(&app);
    });
}

/// `ready`'s answer once it has one, polled every 50 ms for [`WAIT`]; past that, say what never
/// came and quit.
async fn until<T>(app: &Rc<App>, what: &str, ready: impl Fn() -> Option<T>) -> Option<T> {
    let t = Instant::now();
    loop {
        if let Some(it) = ready() {
            return Some(it);
        }
        if t.elapsed() > WAIT {
            println!("bench debug timeout {what}");
            bench_quit(app);
            return None;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}

/// The line the paused program's mark is on.
pub(super) fn paused_line(tab: &Tab) -> Option<u32> {
    (0..tab.buffer.line_count())
        .find(|&line| {
            !tab.buffer
                .source_marks_at_line(line, Some(breakpoints::EXECUTION))
                .is_empty()
        })
        .map(|line| line as u32)
}

/// The strip's line, the calls and the values the Code pane shows.
pub(super) fn shown(app: &App) -> (String, Vec<String>, Vec<String>) {
    app.sidebar
        .get()
        .and_then(|sidebar| sidebar.debug_shown())
        .unwrap_or_default()
}

fn line_text(tab: &Tab, line: u32) -> String {
    let Some(start) = tab.buffer.iter_at_line(line as i32) else {
        return String::new();
    };
    let mut end = start;
    end.forward_to_line_end();
    start.text(&end).to_string()
}

pub(super) fn text(term: &terminal::Term) -> String {
    term.view
        .text_format(vte4::Format::Text)
        .map(String::from)
        .unwrap_or_default()
}

/// The debugged program's tab.
pub(super) fn program_tab(app: &App) -> Option<Rc<terminal::Term>> {
    app.program_tab()
}

/// The line the console printed under `asked`, once it has.
pub(super) fn answer(transcript: &str, asked: &str) -> Option<String> {
    let echo = format!(">>> {asked}");
    let mut lines = transcript.lines().map(str::trim_end);
    lines.find(|line| *line == echo)?;
    lines
        .find(|line| !line.is_empty())
        .filter(|line| !line.starts_with(">>>"))
        .map(String::from)
}

/// The code the console's last line says the session ended with.
fn exit_code(transcript: &str) -> Option<String> {
    let (_, code) = transcript.rsplit_once("Session ended (exit code ")?;
    code.split_once(')').map(|(code, _)| code.to_string())
}
