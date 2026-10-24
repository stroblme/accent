//! Probe save-conflict choices after external changes under unsaved note edits, and a
//! notebook's and a diagram's reloads on a slow host.

use super::*;
use crate::diagram::DiagramTab;
use crate::editor::Saves;
use crate::notebook::NotebookTab;
use pdf::online;
use std::sync::atomic::{AtomicU64, Ordering};

/// How long a diagram's or a notebook's reload holds what it read before answering, and a save
/// its write before writing, as a slow host's round trips would: set by `ACCENT_BENCH_ANSWER=doc:`
/// for the case it probes.
pub(crate) static SLOW_RELOAD_MS: AtomicU64 = AtomicU64::new(0);
pub(crate) static SLOW_WRITE_MS: AtomicU64 = AtomicU64::new(0);

/// Wait as long as `slow` says, on the worker it is called from.
pub(crate) fn hold(slow: &AtomicU64) {
    let ms = slow.load(Ordering::Relaxed);
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// `ACCENT_BENCH_ANSWER=<rel_note>` probes Overwrite, Keep Mine and saving after disk
/// deletion. After initial indexing, externally change the file (on the host if
/// remote) and type into the tab, then answer on a slow host (`SLOW_WRITE_MS`) and type
/// again while the answer is written. Report main-loop blocking, time to become clean,
/// final line, which is what was typed during the write once the save after it lands,
/// and remaining banner. Writes files; use a scratch vault.
pub(super) fn bench_answer(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        online(&app).await;
        // Past the first walk, whose traffic a host answers a save behind.
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let Some(vault) = app.vault().cloned() else {
            return bench_quit(&app);
        };
        app.open_path(&rel);
        let mut tab = None;
        for _ in 0..100 {
            tab = app.open_tabs().into_iter().find(|t| t.rel() == rel);
            if tab.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let Some(tab) = tab else {
            println!("bench answer no_tab {rel}");
            return bench_quit(&app);
        };
        tab.set_text("answer drill\n");
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench answer write_failed {e}");
            return bench_quit(&app);
        }
        for case in ["overwrite", "keep_mine", "restore"] {
            let script = match case {
                "restore" => "rm -f {}".to_string(),
                _ => format!("printf '{case} theirs\\n' > {{}}"),
            };
            // Typed first where the file goes: a clean tab closes with its file. And past the
            // watcher's debounce, which drops a file made and removed inside one, as the last
            // save's rename and the removal would be.
            let typed = format!("{case} mine\n");
            if case == "restore" {
                tab.buffer.insert(&mut tab.buffer.end_iter(), &typed);
                glib::timeout_future(Duration::from_millis(600)).await;
            }
            let changed = crate::work::off_thread("bench", {
                let (vault, rel) = (vault.clone(), rel.clone());
                move || behind(&vault, &rel, &script)
            })
            .await;
            if case != "restore" {
                tab.buffer.insert(&mut tab.buffer.end_iter(), &typed);
            }
            let wanted = match case {
                "restore" => Alert::Restore,
                _ => Alert::Compare,
            };
            for _ in 0..100 {
                if tab.alert() == Some(wanted) {
                    break;
                }
                glib::timeout_future(Duration::from_millis(50)).await;
            }
            if tab.alert() != Some(wanted) {
                println!(
                    "bench answer {case} no_alert changed={changed:?} open={} alert={:?} \
                     modified={} disk_changed={} flight={}",
                    app.doc_for(&rel).is_some(),
                    tab.alert(),
                    tab.save.modified.get(),
                    tab.save.disk_changed.get(),
                    tab.save.flight.borrow().is_some()
                );
                return bench_quit(&app);
            }
            SLOW_WRITE_MS.store(1500, Ordering::Relaxed);
            let held = match case {
                "overwrite" => {
                    app.save_tab(&tab, true);
                    let Some(dialog) = app
                        .window
                        .visible_dialog()
                        .and_downcast::<adw::AlertDialog>()
                    else {
                        println!("bench answer overwrite no_dialog");
                        return bench_quit(&app);
                    };
                    let t = Instant::now();
                    dialog.emit_by_name::<()>("response", &[&"overwrite"]);
                    let held = ms_since(t);
                    dialog.close();
                    held
                }
                "keep_mine" => {
                    app.answer_banner(&tab);
                    let mut button = None;
                    for _ in 0..100 {
                        button = find_widget(app.window.upcast_ref(), &|w| {
                            w.downcast_ref::<gtk::Button>()
                                .is_some_and(|b| b.label().as_deref() == Some("Keep Mine"))
                        })
                        .and_downcast::<gtk::Button>();
                        if button.is_some() {
                            break;
                        }
                        glib::timeout_future(Duration::from_millis(50)).await;
                    }
                    let Some(button) = button else {
                        println!("bench answer keep_mine no_button");
                        return bench_quit(&app);
                    };
                    let t = Instant::now();
                    button.emit_clicked();
                    ms_since(t)
                }
                _ => {
                    let t = Instant::now();
                    app.answer_banner(&tab);
                    ms_since(t)
                }
            };
            tab.buffer
                .insert(&mut tab.buffer.end_iter(), &format!("{case} during\n"));
            let t = Instant::now();
            while tab.save.modified.get() && t.elapsed() < Duration::from_secs(8) {
                glib::timeout_future(Duration::from_millis(5)).await;
            }
            let clean = ms_since(t);
            SLOW_WRITE_MS.store(0, Ordering::Relaxed);
            let read = crate::work::off_thread("bench", {
                let (vault, rel) = (vault.clone(), rel.clone());
                move || vault.read(&rel).map(|(text, _)| text)
            })
            .await
            .and_then(Result::ok)
            .unwrap_or_default();
            println!(
                "bench answer {case} held_ms={held:.1} clean_ms={clean:.0} modified={} file_last={:?} alert={:?}",
                tab.save.modified.get(),
                read.lines().last().unwrap_or_default(),
                tab.alert()
            );
        }
        bench_quit(&app);
    });
}

/// `ACCENT_BENCH_ANSWER=same:<rel>` tests unchanged bytes with changed etags: touch,
/// rename replacement, in-place rewrite, watcher-before-typing and Save-before-watcher.
/// Also test real changes and typing while a reload is pending (`changed`, `racing`).
/// After autosave, print dirty/dialog/banner state and file/buffer text; only real
/// changes may raise banners, and typed text must survive. Use a scratch vault.
pub(super) fn bench_answer_same(app: &Rc<App>, rel: &str) {
    scratch_only(app, "ACCENT_BENCH_ANSWER");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let Some(vault) = app.vault().cloned() else {
            return bench_quit(&app);
        };
        for case in [
            "none", "touch", "copy", "inplace", "late", "save", "changed", "racing",
        ] {
            app.open_path(&rel);
            let mut tab = None;
            for _ in 0..100 {
                tab = app.open_tabs().into_iter().find(|t| t.rel() == rel);
                if tab.is_some() {
                    break;
                }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
            let Some(tab) = tab else {
                println!("bench answer same {case} no_tab");
                return bench_quit(&app);
            };
            let script = match case {
                "none" => None,
                "touch" | "late" | "save" => Some("touch {}"),
                "copy" => Some("cp {} {}.same && mv {}.same {}"),
                "inplace" => Some("cp {} {}.same && cat {}.same > {} && rm {}.same"),
                _ => Some("printf 'changed theirs\\n' >> {}"),
            };
            if let Some(script) = script {
                crate::work::off_thread("bench", {
                    let (vault, rel) = (vault.clone(), rel.clone());
                    move || behind(&vault, &rel, script)
                })
                .await;
            }
            // The watcher's news lands while the tab is still clean.
            if case == "late" {
                glib::timeout_future(Duration::from_millis(1500)).await;
            }
            let opened_dirty = tab.save.modified.get();
            // The read a watcher's news starts, still out when the key lands.
            if case == "racing" {
                app.refresh_tab(&tab);
            }
            let typed = format!("{case} mine\n");
            tab.buffer.insert(&mut tab.buffer.end_iter(), &typed);
            if case == "save" {
                app.save_tab(&tab, true);
            }
            glib::timeout_future(Duration::from_millis(2500)).await;
            let dialog = app.window.visible_dialog();
            let read = crate::work::off_thread("bench", {
                let (vault, rel) = (vault.clone(), rel.clone());
                move || vault.read(&rel).map(|(text, _)| text)
            })
            .await
            .and_then(Result::ok)
            .unwrap_or_default();
            println!(
                "bench answer same {case} opened_dirty={opened_dirty} alert={:?} dialog={} \
                 modified={} file_last={:?} kept={}",
                tab.alert(),
                dialog.is_some(),
                tab.save.modified.get(),
                read.lines().last().unwrap_or_default(),
                tab.text().contains(&typed),
            );
            if let Some(dialog) = dialog {
                dialog.close();
            }
            tab.discard();
            app.close_page(&tab.page);
            glib::timeout_future(Duration::from_millis(300)).await;
        }
        bench_quit(&app);
    });
}

/// `ACCENT_BENCH_ANSWER=doc:<rel_ipynb>,<rel_drawio>` writes a notebook and a diagram at those
/// paths and reloads and overwrites each on a slow host (`SLOW_RELOAD_MS`, `SLOW_WRITE_MS`), a
/// fresh tab a case. `racing`: an edit made while the read is out stays, and the banner asks.
/// `overlap`: of two reloads out at once, the newer one's read is what the tab holds, though the
/// older is answered last. `overwrite`: the conflict dialog's Overwrite holds the main loop for
/// no write (`held_ms`), and an edit made during it is written after it. `failed`: a write the
/// folder refuses says so, the edits and the banner kept. Writes files, and makes the folder
/// read only for a moment; use a scratch vault.
pub(super) fn bench_answer_doc(app: &Rc<App>, arg: &str) {
    let Some((notebook, diagram)) = arg.split_once(',') else {
        eprintln!("ACCENT_BENCH_ANSWER=doc:<rel_ipynb>,<rel_drawio>");
        return bench_quit(app);
    };
    let (app, notebook, diagram) = (app.clone(), notebook.to_string(), diagram.to_string());
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        doc_drill::<NotebookTab>(&app, &notebook).await;
        doc_drill::<DiagramTab>(&app, &diagram).await;
        bench_quit(&app);
    });
}

/// What [`doc_drill`] does to a notebook or a diagram.
trait Probe: Saves + Sized {
    const KIND: &'static str;
    /// The file holding `version` where an edit shows.
    fn file(version: &str) -> String;
    fn find(app: &App, rel: &str) -> Option<Rc<Self>>;
    /// An edit putting `mark` where [`Probe::holds`] finds it.
    fn edit(self: &Rc<Self>, mark: &str);
    /// What the tab would write now.
    fn holds(&self) -> String;
    fn reload(app: &Rc<App>, tab: &Rc<Self>);
    fn resolve(app: &Rc<App>, tab: &Rc<Self>);
}

impl Probe for NotebookTab {
    const KIND: &'static str = "notebook";
    fn file(version: &str) -> String {
        format!(
            r#"{{"cells":[{{"cell_type":"markdown","metadata":{{}},"source":["{version}"]}}],"metadata":{{}},"nbformat":4,"nbformat_minor":5}}"#
        )
    }
    fn find(app: &App, rel: &str) -> Option<Rc<Self>> {
        app.doc_for(rel).and_then(|d| d.notebook().cloned())
    }
    fn edit(self: &Rc<Self>, mark: &str) {
        self.bench_type(0, &format!(" {mark}"));
    }
    fn holds(&self) -> String {
        self.text()
    }
    fn reload(app: &Rc<App>, tab: &Rc<Self>) {
        app.reload_notebook(tab);
    }
    fn resolve(app: &Rc<App>, tab: &Rc<Self>) {
        app.resolve_notebook(tab);
    }
}

impl Probe for DiagramTab {
    const KIND: &'static str = "diagram";
    fn file(version: &str) -> String {
        super::diagram::SAMPLE.replace(r#"name="One""#, &format!(r#"name="{version}""#))
    }
    fn find(app: &App, rel: &str) -> Option<Rc<Self>> {
        app.doc_for(rel).and_then(|d| d.diagram().cloned())
    }
    fn edit(self: &Rc<Self>, mark: &str) {
        let name = self.page_names().first().cloned().unwrap_or_default();
        self.rename_page(&format!("{name} {mark}"));
    }
    fn holds(&self) -> String {
        self.text()
    }
    fn reload(app: &Rc<App>, tab: &Rc<Self>) {
        app.reload_diagram(tab);
    }
    fn resolve(app: &Rc<App>, tab: &Rc<Self>) {
        app.resolve_diagram(tab);
    }
}

async fn doc_drill<T: Probe>(app: &Rc<App>, rel: &str) {
    let kind = T::KIND;
    let path = app.root().join(rel);
    let write = |version: &str| std::fs::write(&path, T::file(version)).is_ok();
    for case in ["racing", "overlap", "overwrite", "failed"] {
        SLOW_RELOAD_MS.store(0, Ordering::Relaxed);
        SLOW_WRITE_MS.store(0, Ordering::Relaxed);
        app.toasts.dismiss_all();
        write("base");
        // A file written a moment ago is only in the vault once the watcher has seen it.
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(rel);
        let mut tab = None;
        for _ in 0..100 {
            tab = T::find(app, rel);
            if tab.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        let Some(tab) = tab else {
            println!("bench answer doc {kind} {case} no_tab");
            return;
        };
        let mut said = String::new();
        match case {
            "racing" => {
                SLOW_RELOAD_MS.store(800, Ordering::Relaxed);
                write("theirs");
                T::reload(app, &tab);
                tab.edit("mine");
            }
            "overlap" => {
                SLOW_RELOAD_MS.store(1500, Ordering::Relaxed);
                write("theirs A");
                T::reload(app, &tab);
                glib::timeout_future(Duration::from_millis(300)).await;
                SLOW_RELOAD_MS.store(200, Ordering::Relaxed);
                write("theirs B");
                T::reload(app, &tab);
            }
            _ => {
                tab.edit("mine");
                write("theirs");
                for _ in 0..100 {
                    if tab.save_state().disk_changed.get() {
                        break;
                    }
                    glib::timeout_future(Duration::from_millis(50)).await;
                }
                let folder = path.parent().unwrap_or(&path).to_path_buf();
                let mode = |mode| {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode));
                };
                match case {
                    "overwrite" => SLOW_WRITE_MS.store(1500, Ordering::Relaxed),
                    _ => mode(0o555),
                }
                T::resolve(app, &tab);
                let Some(dialog) = app
                    .window
                    .visible_dialog()
                    .and_downcast::<adw::AlertDialog>()
                else {
                    println!("bench answer doc {kind} {case} no_dialog");
                    mode(0o755);
                    return;
                };
                let t = Instant::now();
                dialog.emit_by_name::<()>("response", &[&"overwrite"]);
                said = format!(" held_ms={:.0}", ms_since(t));
                dialog.close();
                if case == "overwrite" {
                    tab.edit("during");
                }
                let t = Instant::now();
                // The first toast standing while it lands, which outlasts no wait.
                let mut toast = String::new();
                while t.elapsed() < Duration::from_secs(6)
                    && (tab.save_state().modified.get() || case == "failed")
                {
                    glib::timeout_future(Duration::from_millis(50)).await;
                    if toast.is_empty() {
                        toast = app.toasts.shown().into_iter().next().unwrap_or_default();
                    }
                    if case == "failed" && tab.save_state().flight.borrow().is_none() {
                        break;
                    }
                }
                mode(0o755);
                let file = std::fs::read_to_string(&path).unwrap_or_default();
                said += &format!(
                    " file_mine={} file_during={} toast={:?}",
                    file.contains("mine"),
                    file.contains("during"),
                    toast.split(':').next().unwrap_or_default(),
                );
            }
        }
        glib::timeout_future(Duration::from_millis(2500)).await;
        let (save, holds) = (tab.save_state(), tab.holds());
        println!(
            "bench answer doc {kind} {case} modified={} banner={} mine={} theirs={}{said}",
            save.modified.get(),
            save.disk_changed.get(),
            holds.contains("mine"),
            ["theirs B", "theirs A", "theirs"]
                .into_iter()
                .find(|v| holds.contains(v))
                .unwrap_or("none"),
        );
        save.modified.set(false);
        app.close_page(&doc_page(app, rel));
        glib::timeout_future(Duration::from_millis(300)).await;
    }
    SLOW_RELOAD_MS.store(0, Ordering::Relaxed);
    SLOW_WRITE_MS.store(0, Ordering::Relaxed);
}

/// The page of the tab showing `rel`.
fn doc_page(app: &App, rel: &str) -> adw::TabPage {
    app.doc_for(rel).expect("the drill's tab").page().clone()
}

/// Run `script`, its `{}` the file's quoted path, where the vault's files are: on the host over
/// the vault's master, or here.
fn behind(vault: &Vault, rel: &str, script: &str) -> bool {
    let path = vault.resolve(rel).unwrap_or_default();
    let script = script.replace("{}", &accent_api::ssh::quote(&path.to_string_lossy()));
    let argv = match vault.remote() {
        Some(remote) => accent_api::ssh::run(remote.url(), remote.control_path(), &script),
        None => vec!["sh".to_string(), "-c".to_string(), script],
    };
    std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .is_ok_and(|s| s.success())
}
