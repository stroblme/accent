//! A notebook's cells debugged in its kernel through the window's own commands
//! (`ACCENT_BENCH_NOTEBOOK_DEBUG`).

use super::*;
use crate::debug::breakpoints;

/// How long each step may take, in ms: a kernel's start is given a minute.
const WAIT: u64 = 90_000;

/// The notebook the drill writes: a function, a call of it that prints, the result it left, and a
/// slow loop to stop while paused.
const NOTEBOOK: &str = r#"{
 "cells": [
  {"cell_type": "code", "execution_count": null, "id": "d1", "metadata": {}, "outputs": [],
   "source": ["def double(x):\n", "    y = x * 2\n", "    return y"]},
  {"cell_type": "code", "execution_count": null, "id": "d2", "metadata": {}, "outputs": [],
   "source": ["total = double(21)\n", "print(\"total\", total)"]},
  {"cell_type": "code", "execution_count": null, "id": "d3", "metadata": {}, "outputs": [],
   "source": ["total"]},
  {"cell_type": "code", "execution_count": null, "id": "d4", "metadata": {}, "outputs": [],
   "source": ["import time\n", "for i in range(600):\n", "    time.sleep(0.1)"]}
 ],
 "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
  "language_info": {"name": "python"}},
 "nbformat": 4,
 "nbformat_minor": 5
}
"#;

/// `ACCENT_BENCH_NOTEBOOK_DEBUG=<rel>` writes [`NOTEBOOK`] at `<rel>` and debugs its cells: a
/// breakpoint in the function's cell, the function's cell debugged first, which starts the
/// kernel, Start Debugging over the call's cell, the stop in the
/// function's cell, its call, its locals and the Debug Console's answer, a step, the run going on
/// to its end and the session with it, the kernel holding what the run left, the same cell
/// debugged again, and Stop Debugging while the slow loop is paused, which interrupts it. It
/// prints `bench nbdebug …` lines as the window shows each, timings on `bench nbdebug_ms …`; a
/// step that never comes prints `bench nbdebug timeout <what>` and quits.
pub(super) fn bench_notebook_debug(app: &Rc<App>, rel: &str) {
    scratch_only(app, "ACCENT_BENCH_NOTEBOOK_DEBUG");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(vault) = app.vault().cloned() else {
            return bench_quit(&app);
        };
        let written = crate::work::off_thread("notebook debugger drill", {
            let rel = rel.clone();
            move || vault.save(&rel, NOTEBOOK, None).map(drop)
        });
        if !matches!(written.await, Some(Ok(()))) {
            println!("bench nbdebug timeout write");
            return bench_quit(&app);
        }
        app.open_path(&rel);
        let opened = || app.doc_for(&rel).and_then(|d| d.notebook().cloned());
        let Some(tab) = until(&app, "tab", opened).await else {
            return;
        };
        let act = |action: &str| {
            let _ = WidgetExt::activate_action(&app.window, action, None);
        };
        let (Some(first), Some(slow)) = (tab.cell_at(1), tab.cell_at(4)) else {
            println!("bench nbdebug timeout cells");
            return bench_quit(&app);
        };
        app.toggle_cell_breakpoint(&tab, first, 1);
        app.toggle_cell_breakpoint(&tab, slow, 2);
        for (number, id) in [(1, first), (4, slow)] {
            let marks = tab.marks_of(id);
            for (line, category) in marks.map(|m| breakpoints::shown(&m)).unwrap_or_default() {
                println!("bench nbdebug breakpoint cell {number}:{line} {category}");
            }
        }
        println!("bench nbdebug strip {}", strip(&app));
        // Debug joins the kernel's strip in one row, if it fits the sidebar's narrowest.
        if let Some((debug, with, without)) = app.sidebar.get().and_then(|s| s.kernel_debug()) {
            println!("bench nbdebug debug shown={}", u8::from(debug));
            println!("bench nbdebug_ms row_px {with} without_debug_px {without}");
        }

        // Debug Cell over the function's cell starts its kernel, and runs it to its end: the
        // function's body is not run.
        let t = Instant::now();
        tab.bench_select(0);
        act("win.notebook-debug-cell");
        let ended = || (!app.debugger.running() && !tab.bench_busy()).then_some(());
        glib::timeout_future(Duration::from_millis(100)).await;
        if until(&app, "defined", ended).await.is_none() {
            return;
        }
        println!("bench nbdebug_ms defined {:.0}", ms_since(t));
        super::kernel::print_cell_as(&tab, 0, "nbdebug", "defined ");

        // Start Debugging over the call's cell, which stops in the function's.
        let t = Instant::now();
        tab.bench_select(1);
        act("win.debug-start");
        let Some((number, line)) = until(&app, "stopped", || tab.bench_paused_at()).await else {
            return;
        };
        let state = shown(&app).4;
        let why = state.strip_prefix("Paused on ").unwrap_or(&state);
        println!("bench nbdebug stopped cell {number}:{line} {why}");
        println!("bench nbdebug_ms stopped {:.0}", ms_since(t));
        let Some(top) = until(&app, "top", || shown(&app).1.first().cloned()).await else {
            return;
        };
        println!("bench nbdebug top {top}");
        let locals = || Some(shown(&app).2).filter(|values| !values.is_empty());
        let Some(locals) = until(&app, "locals", locals).await else {
            return;
        };
        println!("bench nbdebug locals {}", locals.join(" "));
        // In front with the keyboard, as a reader brings it forward to type at it.
        act("win.debug-console");
        app.console_typed("x * 2\r");
        let answered = || {
            app.console()
                .and_then(|term| super::debug::answer(&super::debug::text(&term), "x * 2"))
        };
        let Some(said) = until(&app, "repl", answered).await else {
            for line in app
                .console()
                .map(|t| super::debug::text(&t))
                .unwrap_or_default()
                .lines()
            {
                println!("bench nbdebug console {line}");
            }
            return;
        };
        println!("bench nbdebug repl {said}");
        act("win.debug-step-over");
        let stepped = || tab.bench_paused_at().filter(|at| *at != (number, line));
        let Some((number, line)) = until(&app, "step", stepped).await else {
            return;
        };
        println!("bench nbdebug step cell {number}:{line}");
        act("win.debug-continue");
        if until(&app, "ended", ended).await.is_none() {
            return;
        }
        super::kernel::print_cell_as(&tab, 1, "nbdebug", "done ");
        println!("bench nbdebug ended {}", strip(&app));

        // The kernel lives on, holding what the debugged run left.
        tab.bench_select(2);
        act("win.notebook-run-cell");
        if !super::kernel::done(&app, &tab, "alive").await {
            return;
        }
        super::kernel::print_cell_as(&tab, 2, "nbdebug", "alive ");

        // Debugged again on the same kernel.
        tab.bench_select(1);
        act("win.notebook-debug-cell");
        let Some((number, line)) = until(&app, "again", || tab.bench_paused_at()).await else {
            return;
        };
        println!("bench nbdebug again stopped cell {number}:{line}");
        act("win.debug-continue");
        if until(&app, "again ended", ended).await.is_none() {
            return;
        }

        // Stop Debugging while paused interrupts the cell's run, and the kernel lives on.
        tab.bench_select(3);
        act("win.notebook-debug-cell");
        let Some((number, line)) = until(&app, "slow", || tab.bench_paused_at()).await else {
            return;
        };
        println!("bench nbdebug slow stopped cell {number}:{line}");
        let t = Instant::now();
        act("win.debug-stop");
        if until(&app, "interrupted", ended).await.is_none() {
            return;
        }
        println!("bench nbdebug_ms interrupted {:.0}", ms_since(t));
        super::kernel::print_cell_as(&tab, 3, "nbdebug", "stopped ");
        tab.bench_select(2);
        act("win.notebook-run-cell");
        if !super::kernel::done(&app, &tab, "alive after").await {
            return;
        }
        super::kernel::print_cell_as(&tab, 2, "nbdebug", "alive ");
        app.stop_kernel(&tab);
        glib::timeout_future(Duration::from_millis(2500)).await;
        bench_quit(&app);
    });
}

/// `ready`'s answer once it has one, for [`WAIT`]; past that, the toasts, and quit.
async fn until<T>(app: &Rc<App>, what: &str, ready: impl Fn() -> Option<T>) -> Option<T> {
    let found = super::until("nbdebug", what, WAIT, ready).await;
    if found.is_none() {
        for said in app.toasts.shown() {
            println!("bench nbdebug toast {said}");
        }
        for tab in app.notebooks() {
            app.stop_kernel(&tab);
        }
        glib::timeout_future(Duration::from_millis(2500)).await;
        bench_quit(app);
    }
    found
}

/// What the kernel's strip says and shows, or that it is put away.
fn strip(app: &App) -> String {
    let line = app.sidebar.get().and_then(|s| s.kernel_line());
    line.unwrap_or_else(|| "hidden".to_string())
}

fn shown(app: &App) -> sidebar::DebugShown {
    super::debug::shown(app)
}
