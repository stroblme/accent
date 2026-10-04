//! A drill over the answers to a note's file moving under unsaved edits.

use super::*;
use pdf::online;

/// `ACCENT_BENCH_ANSWER=<rel_note>` answers the three questions a note asks when its file moves
/// under unsaved edits: Overwrite in the File Changed on Disk dialog that Ctrl+S raises, Keep
/// Mine in the comparison the banner opens, and the Save of the banner over a file deleted on
/// disk. Before each it rewrites or deletes the file behind the app's back — on the host, on a
/// remote vault — and types a line into the tab. It prints how long each answer held the main
/// loop, how long the tab took to come clean, the file's last line then and the banner left
/// standing. It starts once the vault's first walk is done, and writes into the vault, so point
/// it at a scratch one.
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

/// `ACCENT_BENCH_ANSWER=same:<rel>` opens `<rel>` afresh for each way its stamp can move while
/// its bytes stay the ones the tab read — `touch`, a copy renamed over it, the same bytes written
/// in place, a touch the watcher reports before the typing, a touch under a Ctrl+S that goes
/// before the watcher has spoken — and for one real change, makes it behind the tab's back as
/// soon as the tab is up and types a line at once, as a reader typing into a note just opened.
/// `racing` changes the file, has the tab reload it as the watcher would and types before the
/// read is in. After the autosave it prints whether the tab was dirty on opening, the banner
/// standing, whether a dialog is up, whether the tab came clean, the file's last line and whether
/// the line typed is still in the buffer: only `changed` and `racing` may raise the banner, and
/// no line typed is lost. It writes into the vault, so point it at a scratch one.
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
