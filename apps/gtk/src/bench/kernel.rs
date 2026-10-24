//! A Jupyter notebook run through the window's own commands (`ACCENT_BENCH_KERNEL`).

use super::*;
use crate::notebook::NotebookTab;
use accent_core::notebook::output::{Data, Output};

/// How long each step may take: a kernel's start is given a minute, and a remote one the link.
const WAIT: Duration = Duration::from_secs(90);

/// The notebook the drill writes and runs: a print, a result, `input()`, a sleep to interrupt,
/// and a cell an interrupt leaves unrun.
const NOTEBOOK: &str = r#"{
 "cells": [
  {"cell_type": "code", "execution_count": null, "id": "a1", "metadata": {}, "outputs": [],
   "source": ["print(\"hi\")"]},
  {"cell_type": "code", "execution_count": null, "id": "a2", "metadata": {}, "outputs": [],
   "source": ["1+1"]},
  {"cell_type": "code", "execution_count": null, "id": "a3", "metadata": {}, "outputs": [],
   "source": ["name = input(\"name? \")\n", "print(\"hello\", name)"]},
  {"cell_type": "code", "execution_count": null, "id": "a4", "metadata": {}, "outputs": [],
   "source": ["import time; time.sleep(30)"]},
  {"cell_type": "code", "execution_count": null, "id": "a5", "metadata": {}, "outputs": [],
   "source": ["print(\"after\")"]}
 ],
 "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
  "language_info": {"name": "python"}},
 "nbformat": 4,
 "nbformat_minor": 5
}
"#;

/// `ACCENT_BENCH_KERNEL=<rel>` writes [`NOTEBOOK`] at `<rel>` and runs it: Run Cell on
/// Insert Line Below's chord, Run Cell and Advance, `input()` answered, an interrupt, Run All
/// stopped by one, the last run's outputs dimmed until the new ones replace them, Restart, a run
/// with no output clearing its cell, Clear Outputs and its undo, the save, and the tab closed
/// with its kernel. It prints `bench kernel …` lines as the window shows each cell, timings on
/// `bench kernel_ms …`; a step that never comes prints `bench kernel timeout <what>` and quits.
pub(super) fn bench_kernel(app: &Rc<App>, rel: &str) {
    scratch_only(app, "ACCENT_BENCH_KERNEL");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        if let Some(remote) = app.vault().and_then(|v| v.remote()).cloned() {
            let connected = || remote.state() == accent_api::remote::State::Connected;
            let linked = || (app.reconciled.get() && connected()).then_some(());
            if until(&app, "link", linked).await.is_none() {
                return;
            }
        }
        let Some(vault) = app.vault().cloned() else {
            return bench_quit(&app);
        };
        let written = crate::work::off_thread("kernel drill", {
            let (vault, rel) = (vault.clone(), rel.clone());
            move || vault.save(&rel, NOTEBOOK, None).map(drop)
        });
        if !matches!(written.await, Some(Ok(()))) {
            println!("bench kernel timeout write");
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

        let t = Instant::now();
        tab.bench_select(0);
        act("win.newline-below");
        if !done(&app, &tab, "cell 1").await {
            return;
        }
        println!("bench kernel_ms start {:.0}", ms_since(t));
        print_cell(&tab, 0, "");
        let succeeded = tab.bench_execution(0).as_deref() == Some("Succeeded");
        tab.bench_type(0, " ");
        let edited = tab.bench_execution(0).as_deref() == Some("StaleSucceeded");
        tab.undo();
        let undone = tab.bench_execution(0).as_deref() == Some("StaleSucceeded");
        println!(
            "bench kernel status succeeded={} edited={} undone={}",
            u8::from(succeeded),
            u8::from(edited),
            u8::from(undone)
        );
        act("win.notebook-run-cell");
        tab.bench_type(0, " ");
        let stale_running = tab.bench_execution(0).as_deref() == Some("StaleSucceeded");
        if !done(&app, &tab, "old source").await {
            return;
        }
        let stale_done = tab.bench_execution(0).as_deref() == Some("StaleSucceeded");
        println!(
            "bench kernel status stale_running={} stale_done={}",
            u8::from(stale_running),
            u8::from(stale_done)
        );
        let code = app.sidebar.get().is_some_and(|s| s.has_pane("code"));
        let line = app.sidebar.get().and_then(|s| s.kernel_line());
        println!(
            "bench kernel pane {} {}",
            if code { "shown" } else { "hidden" },
            line.unwrap_or_default()
        );

        tab.bench_select(1);
        act("win.notebook-run-advance");
        if !done(&app, &tab, "cell 2").await {
            return;
        }
        print_cell(&tab, 1, "");
        // The text views of its output and of every editor are as tall as their text once laid
        // out, not as they were before they wrapped at their width.
        glib::timeout_future(Duration::from_millis(300)).await;
        println!("bench kernel fit unfit={}", tab.bench_unfit());

        // Run and Advance left cell 3 selected.
        act("win.notebook-run-cell");
        if !answer(&app, &tab, "ada", true).await || !done(&app, &tab, "cell 3").await {
            return;
        }
        print_cell(&tab, 2, "");

        tab.bench_select(3);
        act("win.notebook-run-cell");
        tab.bench_type(3, " ");
        let edited_running = tab.bench_execution(3).as_deref() == Some("Unrun");
        let t = Instant::now();
        if !interrupt(&app, &tab, 3, false).await {
            return;
        }
        println!("bench kernel_ms interrupt {:.0}", ms_since(t));
        print_cell(&tab, 3, "interrupted ");
        let late_done = tab.bench_execution(3).as_deref() == Some("Unrun");
        println!(
            "bench kernel status edited_running={} late_done={}",
            u8::from(edited_running),
            u8::from(late_done)
        );
        tab.undo();

        act("win.notebook-run-all");
        if !answer(&app, &tab, "ada", false).await || !interrupt(&app, &tab, 3, true).await {
            return;
        }
        print_cell(&tab, 3, &stale(&tab, 3, "replaced"));
        print_cell(&tab, 4, "run_all stops ");

        let t = Instant::now();
        act("win.notebook-restart");
        let reset = tab.bench_execution(1).as_deref() == Some("StaleSucceeded")
            && tab.bench_execution(3).as_deref() == Some("StaleFailed")
            && tab.bench_execution(4).as_deref() == Some("Unrun");
        println!("bench kernel status restart={}", u8::from(reset));
        tab.bench_select(0);
        act("win.notebook-run-cell");
        if !done(&app, &tab, "restart").await {
            return;
        }
        println!("bench kernel_ms restart {:.0}", ms_since(t));
        let count = tab.bench_cell(0).map(|(count, ..)| count);
        println!(
            "bench kernel restart cell 1 count={}",
            count.unwrap_or_default()
        );
        // IPython shows no result for a line ending in `;`.
        tab.bench_type(1, ";");
        act("win.notebook-run-cell");
        if !done(&app, &tab, "quiet").await {
            return;
        }
        print_cell(&tab, 1, &stale(&tab, 1, "quiet"));
        tab.bench_select(0);

        act("win.notebook-clear-outputs");
        let cleared = shown_outputs(&tab, 0);
        act("win.notebook-undo");
        let undone = shown_outputs(&tab, 0);
        println!("bench kernel clear outputs={cleared} undo outputs={undone}");

        act("win.save");
        let clean = || (!tab.save.modified.get()).then_some(());
        if until(&app, "save", clean).await.is_none() {
            return;
        }
        let read = crate::work::off_thread("kernel drill", {
            let (vault, rel) = (vault.clone(), rel.clone());
            move || saved(&vault, &rel)
        });
        match read.await.flatten() {
            Some((cells, counted, outputs)) => {
                println!("bench kernel saved cells={cells} counted={counted} outputs={outputs}")
            }
            None => println!("bench kernel saved unread"),
        }

        let remote = vault.is_remote();
        app.close_page(&tab.page);
        drop(tab);
        let gone = || (remote || kernel_gone()).then_some(());
        if until(&app, "kernel gone", gone).await.is_none() {
            return;
        }
        match remote {
            true => println!("bench kernel closed kernel_gone=unchecked on the host"),
            false => println!("bench kernel closed kernel_gone=true"),
        }
        bench_quit(&app);
    });
}

/// Wait until no run waits or runs, saying `what` timed out otherwise.
pub(super) async fn done(app: &Rc<App>, tab: &NotebookTab, what: &str) -> bool {
    // A turn first, so a run just asked for is counted.
    glib::timeout_future(Duration::from_millis(50)).await;
    until(app, what, || (!tab.bench_busy()).then_some(()))
        .await
        .is_some()
}

/// Answer `input()` with `value` once it asks, printing what it asked when `say`.
async fn answer(app: &Rc<App>, tab: &NotebookTab, value: &str, say: bool) -> bool {
    let Some((prompt, password, entry)) = until(app, "input", || tab.bench_asking()).await else {
        return false;
    };
    if say {
        println!("bench kernel input prompt={prompt:?} password={password}");
    }
    if let Some(editable) = entry.dynamic_cast_ref::<gtk::Editable>() {
        editable.set_text(value);
    }
    // Return's: an entry's own `activate`, which `Widget::activate` does not emit.
    entry.emit_by_name::<()>("activate", &[]);
    true
}

/// Interrupt cell `n`'s run once it has begun, and wait for the kernel to be idle; `say` prints
/// the cell as the run left it till then.
async fn interrupt(app: &Rc<App>, tab: &NotebookTab, n: usize, say: bool) -> bool {
    // Its outputs going say it began; the sleep is under way a moment later.
    let running = || {
        let busy = tab.bench_busy();
        let others_done = (0..n).all(|m| tab.bench_cell(m).is_some_and(|(c, ..)| c != "[*]"));
        (busy && others_done).then_some(())
    };
    if until(app, "running", running).await.is_none() {
        return false;
    }
    glib::timeout_future(Duration::from_millis(1500)).await;
    if say {
        print_cell(tab, n, &stale(tab, n, "running"));
        // `ACCENT_BENCH_KERNEL_SHOT=<png>`: the window as the run has left it, for looking at.
        if let Ok(path) = std::env::var("ACCENT_BENCH_KERNEL_SHOT") {
            bench_pump();
            super::diagram::shoot(app.window.upcast_ref(), Path::new(&path));
        }
    }
    let _ = WidgetExt::activate_action(&app.window, "win.notebook-interrupt", None);
    done(app, tab, "interrupt").await
}

/// `bench kernel <said>cell <n+1> count=<label> outputs=<each>`, the outputs as the model holds
/// them, and the view's own count where it differs.
fn print_cell(tab: &NotebookTab, n: usize, said: &str) {
    let Some((count, outputs, shown)) = tab.bench_cell(n) else {
        return println!("bench kernel {said}cell {} missing", n + 1);
    };
    let each: Vec<String> = outputs.iter().map(describe).collect();
    let outputs = match each.is_empty() {
        true => "0".to_string(),
        false => each.join(","),
    };
    let differs = match shown == each.len() {
        true => String::new(),
        false => format!(" shown={shown}"),
    };
    println!(
        "bench kernel {said}cell {} count={count} outputs={outputs}{differs}",
        n + 1
    );
}

/// `print_cell`'s `said` for cell `n`: `what`, and whether its outputs are dimmed.
fn stale(tab: &NotebookTab, n: usize, what: &str) -> String {
    format!("{what} dimmed={} ", u8::from(tab.bench_stale(n)))
}

fn describe(output: &Output) -> String {
    match output {
        Output::Stream { text, .. } => format!("stream:{text:?}"),
        Output::Data { count, items } => {
            let kind = match count {
                Some(_) => "execute_result",
                None => "display_data",
            };
            let plain = items.iter().find(|i| i.mime == "text/plain");
            match plain.map(|i| &i.data) {
                Some(Data::Text(text)) => format!("{kind}:{text:?}"),
                _ => kind.to_string(),
            }
        }
        Output::Error { name, .. } => format!("error:{name}"),
    }
}

/// How many outputs cell `n`'s view shows.
fn shown_outputs(tab: &NotebookTab, n: usize) -> usize {
    tab.bench_cell(n).map_or(0, |(_, _, shown)| shown)
}

/// The cells of the notebook as saved, those counted, and their outputs; the file read as
/// JSON first.
fn saved(vault: &Vault, rel: &str) -> Option<(usize, usize, usize)> {
    let accent_core::fs::Read::Text(text) = vault.read_text(rel).ok()? else {
        return None;
    };
    serde_json::from_str::<serde_json::Value>(&text.text).ok()?;
    let notebook = accent_core::notebook::read(rel, &text.text).ok()?;
    let cells = notebook.cells();
    let counted = cells.iter().filter(|c| c.count().is_some()).count();
    let outputs = cells.iter().map(|c| c.outputs().len()).sum();
    Some((cells.len(), counted, outputs))
}

/// Whether no kernel is left of this machine's: no socket folder where the kernel API puts
/// them, no process started on one. A drill runs under a scratch `TMPDIR`, so whatever is there
/// is its; in the Flatpak the folder is the app's runtime one, and the host's processes are out
/// of sight.
fn kernel_gone() -> bool {
    let sandbox = std::env::var_os("FLATPAK_ID").filter(|_| Path::new("/.flatpak-info").exists());
    let under = match (sandbox, std::env::var_os("XDG_RUNTIME_DIR")) {
        (Some(id), Some(runtime)) => Path::new(&runtime).join("app").join(id),
        _ => std::env::temp_dir(),
    };
    let prefix = under.join("accent-kernel-");
    let prefix = prefix.to_string_lossy().into_owned();
    let folders = std::fs::read_dir(&under).into_iter().flatten().flatten();
    let folder_left = folders
        .into_iter()
        .any(|e| e.path().to_string_lossy().starts_with(&prefix));
    let processes = std::fs::read_dir("/proc").into_iter().flatten().flatten();
    let process_left = processes.into_iter().any(|e| {
        std::fs::read(e.path().join("cmdline"))
            .is_ok_and(|cmdline| String::from_utf8_lossy(&cmdline).contains(&prefix))
    });
    !folder_left && !process_left
}

/// `ready`'s answer once it has one, polled every 50 ms for [`WAIT`]; past that, say what never
/// came and quit, the kernels stopped first: a quit drops no vault, so nothing else would.
async fn until<T>(app: &Rc<App>, what: &str, ready: impl Fn() -> Option<T>) -> Option<T> {
    let t = Instant::now();
    loop {
        if let Some(it) = ready() {
            return Some(it);
        }
        if t.elapsed() > WAIT {
            println!("bench kernel timeout {what}");
            for said in app.toasts.shown() {
                println!("bench kernel toast {said}");
            }
            for tab in app.notebooks() {
                app.stop_kernel(&tab);
            }
            glib::timeout_future(Duration::from_millis(2500)).await;
            bench_quit(app);
            return None;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}
