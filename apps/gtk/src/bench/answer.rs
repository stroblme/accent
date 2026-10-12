//! Probe save-conflict choices after external changes under unsaved note edits.

use super::*;
use pdf::online;

/// `ACCENT_BENCH_ANSWER=<rel_note>` probes Overwrite, Keep Mine and saving after disk
/// deletion. After initial indexing, externally change the file (on the host if
/// remote) and type into the tab. Report main-loop blocking, time to become clean,
/// final line and remaining banner. Writes files; use a scratch vault.
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
            let changed = crate::work::off_thread("bench", {
                let (vault, rel) = (vault.clone(), rel.clone());
                move || behind(&vault, &rel, &script)
            })
            .await;
            tab.buffer
                .insert(&mut tab.buffer.end_iter(), &format!("{case} mine\n"));
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
                    "bench answer {case} no_alert changed={changed:?} alert={:?} modified={} \
                     disk_changed={} flight={}",
                    tab.alert(),
                    tab.save.modified.get(),
                    tab.save.disk_changed.get(),
                    tab.save.flight.borrow().is_some()
                );
                return bench_quit(&app);
            }
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
            let t = Instant::now();
            while tab.save.modified.get() && t.elapsed() < Duration::from_secs(5) {
                glib::timeout_future(Duration::from_millis(5)).await;
            }
            let clean = ms_since(t);
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
