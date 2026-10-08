//! Drills over the suggestions a note gets as it is typed: the word popup, ghost text and the
//! index behind it (`ACCENT_BENCH_SUGGEST`).

use super::*;

/// Where an Escape the suggestion does not take would land: the find bar, or the comparison the
/// tab is hosting.
#[derive(Clone, Copy)]
enum Under {
    Find,
    Compare,
}

/// `=escape:<rel>`: Escape over a word popup and over painted ghost text, through real XTEST
/// presses, with the find bar open and with the note compared with its disk copy. For each case
/// it prints `bench suggest_ready <case> <steps>`, steps for `build-aux/xtest.py :N "<steps>"` (a
/// word typed for the popup, then Escape), and after the Escape whether the popup, the ghost, the
/// find bar and the comparison are still up; then a second Escape, which has to reach the bar or
/// the comparison. It first prints `bench suggest focus_window` and waits for the window to have
/// the X input focus (`xtest.py :N "move 700 400; focus"`), without which no popup shows.
pub(super) fn bench_suggest_escape(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        bench_connected(&app).await;
        app.open_path(&rel);
        println!("bench suggest focus_window");
        for _ in 0..100 {
            if app.window.is_active() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let own = tab.text();
        for under in [Under::Find, Under::Compare] {
            for popup in [true, false] {
                bench_escape_case(&app, &tab, under, popup).await;
            }
        }
        tab.leave_compare();
        tab.set_text(&own);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench suggest write_failed {e}");
        }
        bench_quit(&app);
    });
}

/// One case of [`bench_suggest_escape`]: the bar or the comparison put up under a fresh note, the
/// popup or the ghost raised at the end of its last line, and two Escapes.
async fn bench_escape_case(app: &Rc<App>, tab: &Rc<Tab>, under: Under, popup: bool) {
    let case = format!(
        "{}_{}",
        match under {
            Under::Find => "find",
            Under::Compare => "compare",
        },
        if popup { "popup" } else { "ghost" }
    );
    let find = app.pane().find.clone();
    find.close();
    tab.leave_compare();
    // A toast takes Escape for itself (`toasts::Toasts`), ahead of the window: none may stand.
    app.toasts.dismiss_all();
    // The words the popup offers, then the line the caret ends on.
    tab.set_text("theorem theory thermal\nwritten on disk\n");
    match under {
        Under::Find => find.open(crate::find::Mode::Find),
        Under::Compare => {
            if let Err(e) = app.write_tab(tab, None) {
                println!("bench suggest write_failed {e}");
            }
            tab.buffer
                .insert(&mut tab.buffer.end_iter(), "typed since\n");
            app.compare_with_disk(tab);
        }
    }
    glib::timeout_future(Duration::from_millis(600)).await;
    tab.view.grab_focus();
    tab.buffer.place_cursor(&tab.buffer.end_iter());
    let steps = match popup {
        true => "type th; sleep 1; key Escape",
        false => {
            if let Some(view) = tab.ghost_view() {
                view.set_ghost(Some("eorem and more".to_string()));
            }
            "key Escape"
        }
    };
    let ghost = || tab.ghost_view().is_some_and(|v| v.ghost().is_some());
    // What stands just before the Escape, read while the steps run.
    let (seen_popup, seen_ghost) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(ghost())));
    let watch = glib::timeout_add_local(
        Duration::from_millis(20),
        glib::clone!(
            #[strong]
            tab,
            #[strong]
            seen_popup,
            move || {
                seen_popup.set(seen_popup.get() || tab.popup_shown());
                glib::ControlFlow::Continue
            }
        ),
    );
    println!("bench suggest_ready {case} {steps}");
    glib::timeout_future(Duration::from_millis(if popup { 2200 } else { 800 })).await;
    watch.remove();
    println!(
        "bench suggest_escape {case} before popup={} ghost={} after popup={} ghost={} find_open={} comparing={}",
        seen_popup.get(),
        seen_ghost.get(),
        tab.popup_shown(),
        ghost(),
        find.is_open(),
        tab.comparison().is_some()
    );
    println!("bench suggest_ready {case}_again key Escape");
    glib::timeout_future(Duration::from_millis(800)).await;
    println!(
        "bench suggest_escape {case}_again find_open={} comparing={}",
        find.is_open(),
        tab.comparison().is_some()
    );
}

/// `=words:<rel>`: Word Suggestions on, off and on again, switched as the Preferences row does.
/// Each time it prints the words the vault offers at the end of `th` (`bench suggest_words <case>
/// <count> [<first six>]`), then asks for the word typed by XTEST (`bench suggest_ready <case>
/// type th`) and prints whether the popup came up. With `RUST_LOG=accent_api=debug` the log says
/// when the dictionary is read and let go, on the host for a remote vault. Waits for the window's
/// X input focus first, as [`bench_suggest_escape`] does.
pub(super) fn bench_suggest_words(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        bench_connected(&app).await;
        app.open_path(&rel);
        println!("bench suggest focus_window");
        for _ in 0..100 {
            if app.window.is_active() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let (Some(tab), Some(vault)) = (
            app.open_tabs().into_iter().find(|tab| tab.rel() == rel),
            app.vault().cloned(),
        ) else {
            return bench_quit(&app);
        };
        let own = tab.text();
        for (case, on) in [("on", true), ("off", false), ("on_again", true)] {
            app.config.borrow_mut().word_suggestions = on;
            app.config_changed();
            app.toasts.dismiss_all();
            tab.set_text("theorem theory thermal\n");
            tab.view.grab_focus();
            tab.buffer.place_cursor(&tab.buffer.end_iter());
            // What the vault offers for `th`, asked as the popup asks.
            tab.buffer.insert_at_cursor("th");
            crate::lang::flush(tab.clone()).await;
            let pos = crate::lang::pos_of(&tab.buffer.end_iter());
            let labels: Vec<String> = match vault.completion(&tab.rel(), pos, None).await {
                Ok(answer) => answer.items.into_iter().map(|c| c.label).collect(),
                Err(e) => vec![format!("{e:#}")],
            };
            println!(
                "bench suggest_words {case} {} {:?}",
                labels.len(),
                &labels[..labels.len().min(6)]
            );
            // And the popup, for a word typed.
            tab.set_text("theorem theory thermal\n");
            tab.buffer.place_cursor(&tab.buffer.end_iter());
            let seen = Rc::new(Cell::new(false));
            let watch = glib::timeout_add_local(
                Duration::from_millis(20),
                glib::clone!(
                    #[strong]
                    tab,
                    #[strong]
                    seen,
                    move || {
                        seen.set(seen.get() || tab.popup_shown());
                        glib::ControlFlow::Continue
                    }
                ),
            );
            println!("bench suggest_ready {case} type th");
            glib::timeout_future(Duration::from_millis(1500)).await;
            watch.remove();
            println!("bench suggest_words {case} popup={}", seen.get());
            if let Some(session) = crate::completion::session(&tab) {
                session.close();
            }
        }
        tab.set_text(&own);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench suggest write_failed {e}");
        }
        bench_quit(&app);
    });
}

/// `=ghost:<rel>`: the ghost-text index from a cold start, and Ghost Text switched off and on
/// under an open note. From the drill's start it prints every line the status bar's busy slot
/// shows (`bench suggest_bar <ms> "<line>"`) until both the vault and merl have finished
/// indexing, then the `merl-rt` processes this one started and their resident size. A
/// suggestion is painted and the preference switched off as the Preferences row does: it prints
/// whether the suggestion is still painted, how long until no `merl-rt` is left, and the busy
/// line. Then on again, with the bar's lines and the new process, and a suggestion asked for at
/// the end of a line another note opens with; off again while merl is indexing (`mid`), and while
/// a suggestion waits on that index (`mid_asked`), neither of which may leave "Indexing
/// suggestions…" behind; and last on again with the vault walked at the same time.
pub(super) fn bench_suggest_ghost(app: &Rc<App>, rel: &str) {
    let start = Instant::now();
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        bench_bar_lines(&app, start, 60_000, |app| app.reconciled.get()).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        println!("bench suggest_merl start {}", merl_processes());

        if let Some(view) = tab.ghost_view() {
            tab.buffer.place_cursor(&tab.buffer.end_iter());
            view.set_ghost(Some(" a suggestion".to_string()));
        }
        bench_ghost_switch(&app, false);
        let gone = bench_merl_gone().await;
        println!(
            "bench suggest_off painted={} merl_gone_ms={gone:?} bar={:?}",
            tab.ghost_view().is_some_and(|v| v.ghost().is_some()),
            app.statusbar.progress_text()
        );

        bench_ghost_switch(&app, true);
        bench_bar_lines(&app, Instant::now(), 20_000, |_| true).await;
        println!("bench suggest_on merl={}", merl_processes());
        bench_ghost_asked(&app, &tab).await;
        // The refresh the text going back sets off asks too, and is answered.
        glib::timeout_future(Duration::from_millis(800)).await;

        for case in ["mid", "mid_asked"] {
            bench_ghost_switch(&app, false);
            bench_merl_gone().await;
            bench_ghost_switch(&app, true);
            glib::timeout_future(Duration::from_millis(400)).await;
            // A suggestion asked for before the index is built, which merl answers once it is.
            let pending = (case == "mid_asked").then(|| {
                let vault = app.vault().cloned()?;
                let (rel, pos) = (tab.rel(), crate::lang::pos_of(&tab.buffer.end_iter()));
                Some(glib::spawn_future_local(async move {
                    let _ = vault.inline_completion(&rel, pos).await;
                }))
            });
            glib::timeout_future(Duration::from_millis(100)).await;
            println!(
                "bench suggest_{case} bar={:?} merl={}",
                app.statusbar.progress_text(),
                merl_processes()
            );
            bench_ghost_switch(&app, false);
            let gone = bench_merl_gone().await;
            glib::timeout_future(Duration::from_millis(200)).await;
            println!(
                "bench suggest_{case}_off merl_gone_ms={gone:?} bar={:?}",
                app.statusbar.progress_text()
            );
            if let Some(Some(pending)) = pending {
                pending.abort();
            }
        }

        // Both indexes at once: the vault's line first, and merl's once the walk is over.
        bench_ghost_switch(&app, true);
        if let Some(Err(e)) = app.vault().map(|vault| vault.rescan()) {
            println!("bench suggest rescan_failed {e:#}");
        }
        bench_bar_lines(&app, Instant::now(), 20_000, |app| app.reconciled.get()).await;
        bench_quit(&app);
    });
}

/// A remote vault answering, waited for up to 30 s; a local one is answering from the start.
async fn bench_connected(app: &Rc<App>) {
    for _ in 0..300 {
        if !app.offline() {
            return;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
}

/// Ghost Text switched as the Preferences row does: the config changed and put into effect in
/// every window.
fn bench_ghost_switch(app: &Rc<App>, on: bool) {
    app.config.borrow_mut().ghost_text = on;
    app.config_changed();
}

/// How long until no `merl-rt` of this process is left, polled for up to 4 s.
async fn bench_merl_gone() -> Option<u128> {
    let since = Instant::now();
    while since.elapsed() < Duration::from_secs(4) {
        if merl_processes() == "[]" {
            return Some(since.elapsed().as_millis());
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    None
}

/// Print every line the busy slot shows, until `done` holds and the slot has been empty for
/// 1.5 s, or `limit` ms have passed.
async fn bench_bar_lines(
    app: &Rc<App>,
    since: Instant,
    limit: u64,
    done: impl Fn(&Rc<App>) -> bool,
) {
    let mut last = String::new();
    let mut quiet = Instant::now();
    while since.elapsed() < Duration::from_millis(limit) {
        let line = app.statusbar.progress_text();
        if line != last {
            println!("bench suggest_bar {:.0} {line:?}", ms_since(since));
            last = line;
            quiet = Instant::now();
        }
        if last.is_empty() && done(app) && quiet.elapsed() > Duration::from_millis(1500) {
            break;
        }
        glib::timeout_future(Duration::from_millis(10)).await;
    }
}

/// A suggestion for the opening words of another note, asked at the end of the tab's last line:
/// whether merl answers again once it has been started again.
async fn bench_ghost_asked(app: &Rc<App>, tab: &Rc<Tab>) {
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    let line = prose_line(&app.root(), &app.root().join(tab.rel())).unwrap_or_default();
    let prefix: String = line
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
        + " ";
    let own = tab.text();
    tab.set_text(&format!("{own}\n{prefix}"));
    crate::lang::flush(tab.clone()).await;
    let pos = crate::lang::pos_of(&tab.buffer.end_iter());
    let answer = vault.inline_completion(&tab.rel(), pos).await;
    println!("bench suggest_asked {prefix:?} -> {answer:?}");
    tab.set_text(&own);
}

/// A line of prose in some note under `dir` other than `skip`: one starting with a capital and
/// running past eight words.
fn prose_line(dir: &Path, skip: &Path) -> Option<String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries.into_iter().find_map(|path| match path.is_dir() {
        true => prose_line(&path, skip),
        false if path.extension().is_some_and(|x| x == "md") && path != skip => {
            std::fs::read_to_string(&path).ok()?.lines().find_map(|l| {
                (l.starts_with(char::is_uppercase) && l.split_whitespace().count() > 8)
                    .then(|| l.to_string())
            })
        }
        false => None,
    })
}

/// The `merl-rt` processes this process started, each as `pid:<resident MB>`, read from `/proc`.
fn merl_processes() -> String {
    let me = std::process::id().to_string();
    let found: Vec<String> = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_string_lossy().to_string();
            let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
            // `pid (comm) state ppid …`; the name may hold spaces, so it is cut at the last ')'.
            let (head, tail) = stat.rsplit_once(')')?;
            let ppid = tail.split_whitespace().nth(1)?;
            (head.ends_with("(merl-rt") && ppid == me).then_some(pid)
        })
        .map(|pid| {
            let rss = std::fs::read_to_string(format!("/proc/{pid}/status"))
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find_map(|l| l.strip_prefix("VmRSS:"))
                        .and_then(|kb| kb.trim().trim_end_matches(" kB").parse::<u64>().ok())
                })
                .unwrap_or(0);
            format!("{pid}:{}MB", rss / 1024)
        })
        .collect();
    format!("[{}]", found.join(","))
}
