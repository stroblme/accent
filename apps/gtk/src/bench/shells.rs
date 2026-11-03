//! Shell drills for accelerators, titles and lifetime.

use super::*;
use vte4::TerminalExt as _;

/// Check shell accelerator capture belongs only to the active window. Under Xvfb, activate by
/// closing the second window through `active-window` notification. Expected Ctrl+S:
/// `["win.save"]` then `[]`.
///
/// Forward All Keys changes Ctrl+E/Ctrl+W from window actions to `[]`; Copy in Terminal remains
/// a window action.
pub(super) fn bench_shell_keys(app: &Rc<App>) {
    let Some(gtk_app) = app.window.application().and_downcast::<adw::Application>() else {
        return bench_quit(app);
    };
    let Some(other) = app
        .shell
        .upgrade()
        .and_then(|shell| shell.loose_window(&gtk_app, crate::shell::Loose::Documents))
    else {
        return bench_quit(app);
    };
    app.open_terminal();
    let print = move |when: &str| {
        println!(
            "bench shell_keys {when} {:?}",
            gtk_app.actions_for_accel("<Primary>s")
        );
    };
    // The shell takes focus from an idle.
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(200), move || {
        print("shell-elsewhere");
        other.window.close();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            print("shell-here");
            bench_shell_kept(&app, "capture");
            app.config.borrow_mut().forward_keys_to_terminal = true;
            app.config_changed();
            bench_shell_kept(&app, "forward");
            bench_quit(&app);
        });
    });
}

/// What `Ctrl+E`, `Ctrl+W` and `Ctrl+Shift+C` activate from the focused shell.
fn bench_shell_kept(app: &Rc<App>, when: &str) {
    let Some(gtk_app) = app.window.application() else {
        return;
    };
    let [e, w, copy] = ["<Primary>e", "<Primary>w", "<Control><Shift>c"]
        .map(|accel| gtk_app.actions_for_accel(accel));
    println!("bench shell_keys {when} ctrl_e={e:?} ctrl_w={w:?} copy={copy:?}");
}

/// `ACCENT_BENCH_TERM=1` against `accent --terminal`: what the vault-less shell window calls
/// itself. VTE reports its own title some milliseconds after the shell has started, so the state
/// is printed every 100 ms while it is still changing rather than once.
pub(super) fn bench_term(app: &Rc<App>) {
    // A vault window has no shell of its own; opening one here is the same drill over the tab
    // case, and the state printed right after it is the one before VTE has reported anything.
    if app.terminals().is_empty() {
        app.open_terminal();
    }
    let app = app.clone();
    let (mut said, mut left) = (String::new(), 20);
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let shows = format!(
            "window={:?} title={:?} subtitle={:?} page={:?}",
            app.window.title().unwrap_or_default(),
            app.title.title(),
            app.title.subtitle(),
            app.terminals()
                .first()
                .map(|t| t.page.title())
                .unwrap_or_default(),
        );
        if shows != said {
            println!("bench term {shows}");
            said = shows;
        }
        left -= 1;
        if left > 0 {
            return glib::ControlFlow::Continue;
        }
        bench_term_save(&app);
        glib::ControlFlow::Break
    });
}

/// Save Session answered with a name, and whether the primary menu offers Close Session either
/// side of it: the menu used to be the one the window opened with, unnamed, until it was opened
/// again. The shell is closed at the end, so the holder keeps nothing of the drill's. A vault
/// window has no session of shells to save.
fn bench_term_save(app: &Rc<App>) {
    if !app.key.borrow().is_terminal() {
        return bench_quit(app);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        let closes = || {
            let labels = app.menu.menu_model().map(|m| crate::fileops::labels(&m));
            labels.is_some_and(|l| l.iter().any(|l| l == "Close Session"))
        };
        println!("bench term_menu before close_session={}", closes());
        let _ = WidgetExt::activate_action(&app.window, "win.save-session", None);
        glib::timeout_future(Duration::from_millis(500)).await;
        if let Some(dialog) = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
        {
            let entry = dialog
                .extra_child()
                .and_then(|form| find_widget(&form, &|w| w.is::<gtk::Entry>()))
                .and_downcast::<gtk::Entry>();
            if let Some(entry) = entry {
                entry.set_text("bench");
            }
            dialog.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
            dialog.close();
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        // A second run on the same state finds `bench` taken, and is asked before replacing it.
        let replace = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>();
        println!(
            "bench term_menu replace={:?}",
            replace.as_ref().map(|d| (d.heading(), d.body()))
        );
        if let Some(dialog) = replace {
            dialog.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
            dialog.close();
            glib::timeout_future(Duration::from_millis(300)).await;
        }
        println!(
            "bench term_menu after close_session={} window={:?}",
            closes(),
            app.window.title().unwrap_or_default()
        );
        let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
        glib::timeout_future(Duration::from_millis(500)).await;
        println!("bench term_menu held={}", bench_held());
        bench_quit(&app);
    });
}

/// `ACCENT_BENCH_HOLD=open|back` uses two launches on one scratch state with `SHELL=/bin/bash`.
/// Open sets a directory and marker, then quits while retaining the shell. Back reports restored
/// key, directory, replay, rows and focus, then closes the tab. Expected same key, `at=/tmp`,
/// `replayed=true`, `lines=rows`, `focused=true`, `held=0`.
///
/// `=early` closes shells in the same tick and shortly after attach starts; expected `held=0`.
/// Use a vault window and no existing holder so the race includes holder startup.
///
/// `=behind` then `=named`, on one scratch state likewise: the first shell sets its title and a
/// second is opened in front of it before the quit; the restored tab behind prints the title it
/// has before it is ever shown, and how long after the drill's start. Expected
/// `title=held-title mapped=false`, then `held=0`.
pub(super) fn bench_hold(app: &Rc<App>, step: &str) {
    match step {
        "early" => return bench_hold_early(app),
        "behind" => return bench_hold_behind(app),
        "named" => return bench_hold_named(app),
        _ => {}
    }
    if app.terminals().is_empty() {
        app.open_terminal();
    }
    let Some(term) = app.terminals().first().cloned() else {
        return bench_quit(app);
    };
    let (app, open) = (app.clone(), step == "open");
    // Typed once the shell is up rather than into the pty ahead of it: switching the tty to raw
    // mode may flush what is waiting there.
    glib::timeout_add_local_once(Duration::from_millis(1000), move || {
        if open {
            // Enough lines to fill the screen, so a replay drawn at another size shows.
            term.view
                .feed_child(b"cd /tmp && seq 1 100 && echo held-marker\n");
        }
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            bench_hold_step(&app, &term, open)
        });
    });
}

/// The half of [`bench_hold`] once its shell has had time to answer.
fn bench_hold_step(app: &Rc<App>, term: &Rc<crate::terminal::Term>, open: bool) {
    let (key, at) = (term.key(), term.at().unwrap_or_default());
    // A window without a vault toasts nothing else, so this is 0, or 1 when no `accent-cli` is
    // there to hold its shells: that is said once, however many it opens.
    let toasts = app.toasted.get();
    if open {
        println!("bench hold open key={key} at={at} toasts={toasts}");
        if let Some(gtk_app) = app.window.application() {
            gtk_app.activate_action("quit", None);
        }
        return;
    }
    let replayed = term
        .view
        .text_format(vte4::Format::Text)
        .is_some_and(|text| text.contains("held-marker"));
    let text = term
        .view
        .text_format(vte4::Format::Text)
        .unwrap_or_default();
    let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
    let focused = gtk::prelude::GtkWindowExt::focus(&app.window)
        .is_some_and(|widget| widget == *term.view.upcast_ref::<gtk::Widget>());
    println!(
        "bench hold back key={key} at={at} replayed={replayed} rows={} lines={lines} \
         focused={focused} toasts={toasts}",
        term.view.row_count()
    );
    let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(500), move || {
        println!("bench hold held={}", bench_held());
        bench_quit(&app);
    });
}

/// The `=early` half of [`bench_hold`].
fn bench_hold_early(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        for wait in [0, 10, 30, 60, 120] {
            app.open_terminal();
            glib::timeout_future(Duration::from_millis(wait)).await;
            let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
            glib::timeout_future(Duration::from_millis(2500)).await;
            println!("bench hold early wait={wait} held={}", bench_held());
        }
        bench_quit(&app);
    });
}

/// The `=behind` half of [`bench_hold`].
fn bench_hold_behind(app: &Rc<App>) {
    app.open_terminal();
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(1000), move || {
        if let Some(term) = app.terminals().first() {
            term.view
                .feed_child(b"PS1='$ '; unset PROMPT_COMMAND; printf '\\033]0;held-title\\007'\n");
        }
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            app.open_terminal();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                println!("bench hold behind terminals={}", app.terminals().len());
                if let Some(gtk_app) = app.window.application() {
                    gtk_app.activate_action("quit", None);
                }
            });
        });
    });
}

/// The `=named` half of [`bench_hold`].
fn bench_hold_named(app: &Rc<App>) {
    let (app, start) = (app.clone(), Instant::now());
    glib::spawn_future_local(async move {
        let behind = || {
            app.terminals()
                .into_iter()
                .find(|term| !term.view.is_mapped())
        };
        while start.elapsed() < Duration::from_secs(5)
            && behind().is_none_or(|term| term.page.title() != "held-title")
        {
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        let Some(term) = behind() else {
            println!("bench hold named no_tab_behind");
            return bench_quit(&app);
        };
        println!(
            "bench hold named title={} mapped={} ms={:.0}",
            term.page.title(),
            term.view.is_mapped(),
            ms_since(start)
        );
        let _ = WidgetExt::activate_action(&app.window, "win.close-pane-tabs", None);
        glib::timeout_future(Duration::from_millis(500)).await;
        println!("bench hold held={}", bench_held());
        bench_quit(&app);
    });
}

/// How many shells `accent-cli held` lists, or why it could not say.
pub(super) fn bench_held() -> String {
    let Some(cli) = crate::terminal::cli() else {
        return "no-accent-cli".to_string();
    };
    match std::process::Command::new(&cli[0])
        .args(&cli[1..])
        .arg("held")
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .count()
            .to_string(),
        Ok(out) => format!("failed:{}", out.status),
        Err(e) => format!("failed:{e}"),
    }
}
