//! A Jupyter notebook run through the window's own commands (`ACCENT_BENCH_KERNEL`).

use super::*;
use crate::notebook::NotebookTab;
use accent_core::notebook::output::{Data, Output};

/// How long each step may take, in ms: a kernel's start is given a minute, and a remote one the
/// link.
const WAIT: u64 = 90_000;

/// The notebook the drill writes and runs: a print, a result, `input()`, a sleep to interrupt,
/// a cell an interrupt leaves unrun, one of a stream that grows across two flushes, an 80 × 40
/// PNG, a table with a plain text of its own and a traceback, a markdown cell with the PNG
/// attached, for Copy Output, a flood: a line past what a line holds, then a print loop, and that
/// line printed alone.
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
   "source": ["print(\"after\")"]},
  {"cell_type": "code", "execution_count": null, "id": "a6", "metadata": {}, "outputs": [],
   "source": ["import base64, time\n", "from IPython.display import Image, display\n",
    "print(\"one\", flush=True); time.sleep(0.5)\n", "print(\"two\", flush=True); time.sleep(0.5)\n",
    "display(Image(base64.b64decode(\"iVBORw0KGgoAAAANSUhEUgAAAFAAAAAoCAIAAADmAupWAAAARElEQVR42u3PQQ0AAAgEoItiVKMaxRZ+ZKMAqZ5XIiwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsfGsBdbbBD06BAAwAAAAASUVORK5CYII=\")))\n",
    "display({\"text/html\": \"<table><thead><tr><th></th><th>a</th></tr></thead><tbody><tr><th>0</th><td>1</td></tr></tbody></table>\", \"text/plain\": \"   a\\n0  1\"}, raw=True)\n",
    "1/0"]},
  {"cell_type": "markdown", "id": "a7", "metadata": {}, "source": ["![dot](attachment:dot.png)"],
   "attachments": {"dot.png": {"image/png": "iVBORw0KGgoAAAANSUhEUgAAAFAAAAAoCAIAAADmAupWAAAARElEQVR42u3PQQ0AAAgEoItiVKMaxRZ+ZKMAqZ5XIiwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsLCwsfGsBdbbBD06BAAwAAAAASUVORK5CYII="}}},
  {"cell_type": "code", "execution_count": null, "id": "a8", "metadata": {}, "outputs": [],
   "source": ["print('x' * 100_000)\n", "while True: print('x' * 1000)"]},
  {"cell_type": "code", "execution_count": null, "id": "a9", "metadata": {}, "outputs": [],
   "source": ["print('x' * 100_000)"]}
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
/// with no output clearing its cell, new outputs fading in, Copy Output on a right-click on each
/// kind of output, a long line timed until it is drawn, a flood stopped, Clear Outputs and its
/// undo, the save, and the tab closed with its kernel. It prints `bench kernel …` lines as the
/// window shows each cell, timings on `bench kernel_ms …` and opacities on `bench kernel_fade …`;
/// a step that never comes prints `bench kernel timeout <what>` and quits.
pub(super) fn bench_kernel(app: &Rc<App>, rel: &str) {
    scratch_only(app, "ACCENT_BENCH_KERNEL");
    if let Some(rel) = rel.strip_prefix("spec:") {
        return bench_kernelspec(app, rel);
    }
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
        // Its kernel starting, the strip offers to stop that, and the cell waits for it.
        glib::timeout_future(Duration::from_millis(100)).await;
        println!(
            "bench kernel strip {} cell 1 {}",
            strip(&app),
            tab.bench_turn(0)
        );
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
        // The run's "hi" fades in as the first's did, the same text though it is.
        println!("bench kernel faded cell 1 {}", tab.bench_faded(0).0);
        let code = app.sidebar.get().is_some_and(|s| s.has_pane("code"));
        println!(
            "bench kernel pane {} {}",
            if code { "shown" } else { "hidden" },
            strip(&app)
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

        tab.bench_select(5);
        act("win.notebook-run-cell");
        // Each output's least opacity over the run's second and the picture's decoding, to look
        // at: from 0 as it fades in, and 1 throughout with GTK's animations off.
        let (mut least, t) = (Vec::<f64>::new(), Instant::now());
        while t.elapsed() < Duration::from_secs(3) {
            for (n, opacity) in tab.bench_faded(5).1.into_iter().enumerate() {
                match least.get_mut(n) {
                    Some(at) => *at = at.min(opacity),
                    None => least.push(opacity),
                }
            }
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        println!("bench kernel_fade least {least:.2?}");
        if !done(&app, &tab, "outputs").await {
            return;
        }
        print_cell(&tab, 5, "");
        // Four new outputs, the stream's second flush running on in the first.
        println!("bench kernel faded cell 6 {}", tab.bench_faded(5).0);
        for (n, what) in ["stream", "picture", "table", "error"]
            .into_iter()
            .enumerate()
        {
            copy_output(&app, &tab, 5, n, what).await;
        }
        copy_output(&app, &tab, 6, 0, "attachment").await;
        if !long_print(&app, &tab, 8).await || !flood(&app, &tab, 7).await {
            return;
        }
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

/// Two kernelspecs of a language of their own, `fakelang`, which run ipykernel: the one the
/// notebook names, under `setsid -w` so that only an `interrupt_request` reaches the kernel and
/// SIGINT to the group ends it, and another with an `env` of its own.
const SPECS: [(&str, &str); 2] = [
    (
        "fakelang",
        r#"{"argv": ["setsid", "-w", "python3", "-m", "ipykernel_launcher", "-f",
            "{connection_file}"], "display_name": "Fake Lang", "language": "fakelang",
            "interrupt_mode": "message"}"#,
    ),
    (
        "fakelang2",
        r#"{"argv": ["python3", "-m", "ipykernel_launcher", "-f", "{connection_file}"],
            "display_name": "Fake Lang 2", "language": "fakelang", "env": {"FAKE_KERNEL": "two"}}"#,
    ),
];

/// A notebook naming the kernelspec `name` of `language`, its code cells `sources`.
fn notebook_of(name: &str, language: &str, sources: &[&str]) -> String {
    let cell = |source: &&str| {
        serde_json::json!({"cell_type": "code", "execution_count": null, "metadata": {},
            "outputs": [], "source": source})
    };
    let metadata = serde_json::json!({
        "kernelspec": {"display_name": name, "language": language, "name": name},
        "language_info": {"name": language},
    });
    let cells: Vec<serde_json::Value> = sources.iter().map(cell).collect();
    let notebook = serde_json::json!({"cells": cells, "metadata": metadata, "nbformat": 4,
        "nbformat_minor": 4});
    serde_json::to_string_pretty(&notebook).unwrap_or_default()
}

/// `ACCENT_BENCH_KERNEL=spec:<rel>`: [`SPECS`] written into `JUPYTER_PATH`, a scratch folder
/// under `/tmp`, and a notebook at `<rel>` naming the first, whose dropdown names it and lists
/// both: a run, a message interrupt, the second picked, which a restart runs with its `env`; and
/// a notebook naming a kernel that is not installed, which Run refuses. Prints `bench kernel
/// spec …` lines; `ACCENT_BENCH_KERNEL_SHOT=<png>` adds a picture of the window, the Code
/// pane in front, after the first run.
fn bench_kernelspec(app: &Rc<App>, rel: &str) {
    let jupyter = std::env::var("JUPYTER_PATH").unwrap_or_default();
    if !jupyter.starts_with("/tmp/") || jupyter.contains(':') {
        eprintln!(
            "ACCENT_BENCH_KERNEL=spec: writes kernelspecs: set JUPYTER_PATH to one folder under /tmp"
        );
        std::process::exit(2);
    }
    for (name, json) in SPECS {
        let at = Path::new(&jupyter).join("kernels").join(name);
        let written = std::fs::create_dir_all(&at);
        if written
            .and_then(|()| std::fs::write(at.join("kernel.json"), json))
            .is_err()
        {
            println!("bench kernel spec timeout kernelspec");
            return bench_quit(app);
        }
    }
    let (app, rel) = (app.clone(), rel.to_string());
    let missing = format!("missing-{rel}");
    glib::spawn_future_local(async move {
        let Some(vault) = app.vault().cloned() else {
            return bench_quit(&app);
        };
        let sources = [
            "print(\"hi\")",
            "print(\"sleeping\"); import time; time.sleep(30)",
            "import os; print(os.environ.get(\"FAKE_KERNEL\"))",
        ];
        let notebooks = [
            (rel.clone(), notebook_of("fakelang", "fakelang", &sources)),
            (missing.clone(), notebook_of("nokernel", "nolang", &["1"])),
        ];
        let written = crate::work::off_thread("kernelspec drill", {
            let vault = vault.clone();
            move || {
                let saved = notebooks
                    .iter()
                    .map(|(rel, text)| vault.save(rel, text, None));
                saved.collect::<Result<Vec<_>, _>>().map(drop)
            }
        });
        if !matches!(written.await, Some(Ok(()))) {
            println!("bench kernel spec timeout write");
            return bench_quit(&app);
        }
        let act = |action: &str| {
            let _ = WidgetExt::activate_action(&app.window, action, None);
        };
        let face = |want: &'static str| {
            let face = app.sidebar.get().and_then(|s| s.picker_face());
            face.filter(|face| face == want)
        };

        app.open_path(&rel);
        let opened = || app.doc_for(&rel).and_then(|d| d.notebook().cloned());
        let Some(tab) = until(&app, "tab", opened).await else {
            return;
        };
        let Some(named) = until(&app, "face", || face("Fake Lang")).await else {
            return;
        };
        let (sent, listed) = futures_channel::oneshot::channel();
        app.kernel_choices(Box::new(move |choices| {
            let _ = sent.send(choices);
        }));
        let choices = match listed.await {
            Ok(Ok(choices)) => {
                let names: Vec<String> = choices.specs.into_iter().map(|s| s.name).collect();
                let picked = choices.picked.unwrap_or_default();
                format!("{} picked={picked}", names.join(","))
            }
            _ => "unlisted".to_string(),
        };
        println!("bench kernel spec face {named} choices {choices}");

        tab.bench_select(0);
        act("win.notebook-run-cell");
        if !done(&app, &tab, "cell 1").await {
            return;
        }
        print_cell(&tab, 0, "spec ");
        println!("bench kernel spec on {}", kernel_of(&tab));
        if let Ok(path) = std::env::var("ACCENT_BENCH_KERNEL_SHOT") {
            app.show_pane("code");
            glib::timeout_future(Duration::from_millis(300)).await;
            bench_pump();
            super::diagram::shoot(app.window.upcast_ref(), Path::new(&path));
        }

        // A message, as SIGINT to its group would end `setsid -w`, and the kernel with it.
        tab.bench_select(1);
        act("win.notebook-run-cell");
        if !interrupt(&app, &tab, 1, false).await {
            return;
        }
        print_cell(&tab, 1, "spec interrupted ");

        app.pick_kernel("fakelang2".to_string());
        let Some(named) = until(&app, "picked face", || face("Fake Lang 2")).await else {
            return;
        };
        let kept = app.current_session().kernels.get(&rel).cloned();
        let kept = kept.unwrap_or_default();
        println!("bench kernel spec picked {named} session={kept}");
        act("win.notebook-restart");
        tab.bench_select(2);
        act("win.notebook-run-cell");
        if !done(&app, &tab, "cell 3").await {
            return;
        }
        print_cell(&tab, 2, "spec restarted ");
        println!("bench kernel spec on {}", kernel_of(&tab));
        app.stop_kernel(&tab);

        app.open_path(&missing);
        let opened = || app.doc_for(&missing).and_then(|d| d.notebook().cloned());
        let Some(other) = until(&app, "missing tab", opened).await else {
            return;
        };
        let Some(named) = until(&app, "missing face", || face("No nolang kernel")).await else {
            return;
        };
        other.bench_select(0);
        act("win.notebook-run-cell");
        // The kernel's toast, among any other, once no kernel is left starting.
        let refused = || {
            let said = app
                .toasts
                .shown()
                .into_iter()
                .rfind(|t| t.contains(" kernel"));
            said.filter(|_| !other.kernel_runs())
        };
        let Some(said) = until(&app, "refusal", refused).await else {
            return;
        };
        println!("bench kernel spec missing {named} toast {said:?}");
        // The first kernel's shutdown, within its 2 s.
        glib::timeout_future(Duration::from_millis(2500)).await;
        bench_quit(&app);
    });
}

/// What the tab's kernel answered as runs in, its implementation's version left out, which
/// differs by machine.
fn kernel_of(tab: &NotebookTab) -> String {
    let what = tab.bench_kernel_what();
    what.split_once(" in ")
        .map_or(what.clone(), |(_, on)| on.to_string())
}

/// Run cell `n`, a flood, and stop it after two seconds of output: Stop is answered at once, and
/// the cell keeps what a notebook keeps of a stream, saying what it left out.
async fn flood(app: &Rc<App>, tab: &Rc<NotebookTab>, n: usize) -> bool {
    use accent_core::notebook::bound::{HEAD, LINE, TAIL};
    tab.bench_select(n);
    let _ = WidgetExt::activate_action(&app.window, "win.notebook-run-cell", None);
    let text = || match tab.bench_cell(n)?.1.first() {
        Some(Output::Stream { text, .. }) => Some(text.clone()),
        _ => None,
    };
    if until(app, "flood", text).await.is_none() {
        return false;
    }
    glib::timeout_future(Duration::from_secs(2)).await;
    let t = Instant::now();
    let _ = WidgetExt::activate_action(&app.window, "win.notebook-interrupt", None);
    if !done(app, tab, "flood interrupt").await {
        return false;
    }
    let ms = ms_since(t);
    println!("bench kernel_ms flood_interrupt {ms:.0}");
    let text = text().unwrap_or_default();
    let first = text.lines().next().unwrap_or_default();
    let cut = first.ends_with(" characters left out …") && first.chars().count() <= LINE;
    let bounded = text.len() <= HEAD + TAIL + 1024 && text.contains(" left out …\n");
    println!(
        "bench kernel flood interrupted={} bounded={} cut={} responsive={}",
        u8::from(tab.bench_execution(n).as_deref() == Some("Failed")),
        u8::from(bounded),
        u8::from(cut),
        u8::from(ms < 10_000.0)
    );
    true
}

/// Run cell `n`, one print of a line past what a line holds, and time its output until it is
/// drawn: its text view laid out the line a character a row as it was put on screen, for 8 s
/// (2026-11-08). The output still wraps at its width once drawn.
async fn long_print(app: &Rc<App>, tab: &Rc<NotebookTab>, n: usize) -> bool {
    tab.bench_select(n);
    let t = Instant::now();
    let _ = WidgetExt::activate_action(&app.window, "win.notebook-run-cell", None);
    let shown = || tab.bench_output_widget(n, 0);
    let Some(view) = until(app, "long print", shown).await else {
        return false;
    };
    let (painted, frame) = futures_channel::oneshot::channel();
    if let Some(clock) = app.window.frame_clock() {
        let (painted, handler) = (Cell::new(Some(painted)), Rc::new(Cell::new(None)));
        let id = clock.connect_after_paint({
            let handler = handler.clone();
            move |clock| {
                if let Some(id) = handler.take() {
                    clock.disconnect(id);
                }
                if let Some(painted) = painted.take() {
                    let _ = painted.send(());
                }
            }
        });
        handler.set(Some(id));
        clock.request_phase(gdk::FrameClockPhase::PAINT);
    }
    let _ = frame.await;
    let ms = ms_since(t);
    println!("bench kernel_ms long_print {ms:.0}");
    let wraps = view
        .downcast_ref::<gtk::TextView>()
        .is_some_and(|v| v.wrap_mode() == gtk::WrapMode::WordChar);
    println!(
        "bench kernel long_print wraps={} prompt={}",
        u8::from(wraps),
        u8::from(ms < 3000.0)
    );
    done(app, tab, "long print").await
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
        // Stop in Run All's place and in the running cell's, the cell behind it waiting.
        println!(
            "bench kernel strip {} cell {} {} cell {} {}",
            strip(app),
            n + 1,
            tab.bench_turn(n),
            n + 2,
            tab.bench_turn(n + 1)
        );
        // `ACCENT_BENCH_KERNEL_SHOT=<png>`: the window as the run has left it, for looking at.
        if let Ok(path) = std::env::var("ACCENT_BENCH_KERNEL_SHOT") {
            bench_pump();
            super::diagram::shoot(app.window.upcast_ref(), Path::new(&path));
        }
    }
    let _ = WidgetExt::activate_action(&app.window, "win.notebook-interrupt", None);
    done(app, tab, "interrupt").await
}

/// What the Code pane's kernel strip says and which of Run All and Stop it shows.
fn strip(app: &App) -> String {
    let line = app.sidebar.get().and_then(|s| s.kernel_line());
    line.unwrap_or_else(|| "hidden".to_string())
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

/// Right-click output `i` of cell `n` through XTEST, pick Copy Output in the menus that open,
/// and print `bench kernel copy <what> menus=<open> <held>`, what the clipboard then holds: a
/// picture's size, a text, or for a traceback its last line and whether it keeps escapes the
/// model's has.
async fn copy_output(app: &Rc<App>, tab: &NotebookTab, n: usize, i: usize, what: &str) {
    let clipboard = app.window.clipboard();
    let _ = clipboard.set_content(None::<&gdk::ContentProvider>);
    let Some(widget) = tab.bench_output_widget(n, i) else {
        return println!("bench kernel copy {what} missing");
    };
    // A table's first named cell, a label with GTK's own menu.
    let named = |w: &gtk::Widget| {
        w.downcast_ref::<gtk::Label>()
            .is_some_and(|l| !l.text().is_empty())
    };
    let widget = match widget.is::<gtk::ScrolledWindow>() {
        true => find_widget(&widget, &named).unwrap_or(widget),
        false => widget,
    };
    glib::timeout_future(Duration::from_millis(300)).await;
    let middle = graphene::Point::new(widget.width() as f32 / 2.0, widget.height() as f32 / 2.0);
    let at = widget.compute_point(&app.window, &middle).unwrap_or(middle);
    super::git::xtest(&format!("move {} {}; down 3; up 3", at.x(), at.y())).await;
    let menus = menus_up(app.window.upcast_ref());
    let picked = menus.first().is_some_and(|menu| pick(menu, "Copy Output"));
    menus.iter().for_each(|menu| menu.popdown());
    let texture = gdk::Texture::static_type();
    let has = || {
        let formats = clipboard.formats();
        (picked && (formats.contains_type(texture) || formats.contains_type(String::static_type())))
            .then_some(())
    };
    let held = match until(app, "clipboard", has).await {
        None => return,
        Some(()) if clipboard.formats().contains_type(texture) => {
            match clipboard.read_texture_future().await {
                Ok(Some(t)) => format!("picture={}x{}", t.width(), t.height()),
                _ => "picture unread".to_string(),
            }
        }
        Some(()) => {
            let text = clipboard.read_text_future().await.ok().flatten();
            let text = text.map(|t| t.to_string()).unwrap_or_default();
            match what {
                "error" => {
                    let model = tab.bench_cell(n).is_some_and(|(_, outputs, _)| {
                        matches!(outputs.get(i), Some(Output::Error { traceback, .. })
                            if traceback.iter().any(|line| line.contains('\u{1b}')))
                    });
                    format!(
                        "last={:?} escapes model={} copied={}",
                        text.lines().last().unwrap_or_default(),
                        u8::from(model),
                        u8::from(text.contains('\u{1b}'))
                    )
                }
                _ => format!("text={text:?}"),
            }
        }
    };
    println!("bench kernel copy {what} menus={} {held}", menus.len());
}

/// The menus up in the window.
fn menus_up(root: &gtk::Widget) -> Vec<gtk::PopoverMenu> {
    let (mut found, mut todo) = (Vec::new(), vec![root.clone()]);
    while let Some(widget) = todo.pop() {
        if let Some(menu) = widget.downcast_ref::<gtk::PopoverMenu>()
            && menu.is_visible()
        {
            found.push(menu.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
    found
}

/// Activate the item of `menu` that `label` names, from the menu as a click on it does: its
/// action, with its target, looked for from where the menu is. Whether there was one.
fn pick(menu: &gtk::PopoverMenu, label: &str) -> bool {
    fn find(model: &gio::MenuModel, label: &str) -> Option<(String, Option<glib::Variant>)> {
        (0..model.n_items()).find_map(|i| {
            let section = model.item_link(i, gio::MENU_LINK_SECTION);
            if let Some(hit) = section.and_then(|s| find(&s, label)) {
                return Some(hit);
            }
            let string = |key| {
                let value = model.item_attribute_value(i, key, Some(glib::VariantTy::STRING));
                value.and_then(|v| v.get::<String>())
            };
            let target = model.item_attribute_value(i, gio::MENU_ATTRIBUTE_TARGET, None);
            (string(gio::MENU_ATTRIBUTE_LABEL).as_deref() == Some(label))
                .then(|| string(gio::MENU_ATTRIBUTE_ACTION).map(|action| (action, target)))
                .flatten()
        })
    }
    let found = menu.menu_model().and_then(|model| find(&model, label));
    found.is_some_and(|(action, target)| menu.activate_action(&action, target.as_ref()).is_ok())
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

/// `ready`'s answer once it has one, for [`WAIT`] ([`super::until`]); past that, say what the
/// toasts said and quit, the kernels stopped first: a quit drops no vault, so nothing else would.
async fn until<T>(app: &Rc<App>, what: &str, ready: impl Fn() -> Option<T>) -> Option<T> {
    let found = super::until("kernel", what, WAIT, ready).await;
    if found.is_none() {
        for said in app.toasts.shown() {
            println!("bench kernel toast {said}");
        }
        for tab in app.notebooks() {
            app.stop_kernel(&tab);
        }
        glib::timeout_future(Duration::from_millis(2500)).await;
        bench_quit(app);
    }
    found
}
