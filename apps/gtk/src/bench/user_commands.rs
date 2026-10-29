//! `ACCENT_BENCH_COMMANDS`: the user's own commands (`[[commands]]`), run from Run Command and by
//! their shortcuts through XTEST, and their tabs put back after a restart only while they run.
//! The lines are harmless (`true`, `false`, `pwd`, `sleep`), under a scratch `HOME`.

use super::*;
use accent_core::config::CustomCommand;
use vte4::TerminalExt as _;

/// `=run`: Pass (`true`) picked in Run Command and by its chord from a shell closes its tab; Fail
/// (`pwd; false`) by its chord keeps its tab at the vault root, saying how it ended. `=open`
/// then `=back`, on one scratch state: Nap (`sleep 60`) and Quick (`sleep 3`) run, the app quits,
/// and once Quick has ended the restart puts back Nap alone. `=prefs`: Preferences → Commands
/// lists them, marks a chord New Terminal holds, adds, renames and removes one;
/// `ACCENT_BENCH_COMMANDS_SHOT=<png>` pictures the group.
pub(super) fn bench_commands(app: &Rc<App>, step: &str) {
    let (app, step) = (app.clone(), step.to_string());
    glib::spawn_future_local(async move {
        match step.as_str() {
            "open" => open(&app).await,
            "back" => back(&app).await,
            "prefs" => prefs(&app).await,
            _ => run(&app).await,
        }
    });
}

fn command(name: &str, run: &str, shortcut: Option<&str>) -> CustomCommand {
    CustomCommand {
        name: name.into(),
        run: run.into(),
        shortcut: shortcut.map(str::to_string),
    }
}

/// Put `commands` in the config, as Preferences or a hand edit would.
fn set(app: &Rc<App>, commands: Vec<CustomCommand>) {
    app.config.borrow_mut().commands = commands;
    app.config_changed();
}

/// Wait up to `ms` for `done`.
async fn until(done: impl Fn() -> bool, ms: u64) -> bool {
    for _ in 0..ms / 50 {
        if done() {
            return true;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    done()
}

fn text(term: &crate::terminal::Term) -> String {
    term.view
        .text_format(vte4::Format::Text)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

async fn run(app: &Rc<App>) {
    set(
        app,
        vec![
            command("Pass", "true", Some("<Control><Shift>k")),
            command("Fail", "pwd; false", Some("<Control><Shift>m")),
        ],
    );
    // Every tab the pane takes, by title once the opener has named it.
    let opened: Rc<RefCell<Vec<adw::TabPage>>> = Rc::default();
    app.pane().tabs.connect_page_attached({
        let opened = opened.clone();
        move |_, page, _| opened.borrow_mut().push(page.clone())
    });
    let titles = |opened: &Rc<RefCell<Vec<adw::TabPage>>>| -> Vec<String> {
        let titles = opened
            .borrow()
            .iter()
            .map(|p| p.title().to_string())
            .collect();
        opened.borrow_mut().clear();
        titles
    };
    let left = || app.terminals().len();

    super::git::xtest("move 700 450; focus; key ctrl+p").await;
    super::git::xtest("type Pass; sleep 0.3; key Return").await;
    let closed = until(|| left() == 0, 5000).await;
    println!(
        "bench commands palette opened={:?} closed={closed} used={:?}",
        titles(&opened),
        app.recent_commands.borrow().first()
    );

    super::git::xtest("key ctrl+shift+m").await;
    let fail = until(
        || {
            app.terminals()
                .first()
                .is_some_and(|t| text(t).contains("[Exited"))
        },
        5000,
    )
    .await;
    let root = app.vault().map(|v| v.root().display().to_string());
    let (said, at_root) = match app.terminals().first() {
        Some(term) => {
            let text = text(term);
            let said = text
                .lines()
                .rfind(|l| l.starts_with('['))
                .map(str::to_string);
            (said, root.is_some_and(|root| text.contains(&root)))
        }
        None => (None, false),
    };
    println!(
        "bench commands shortcut opened={:?} ended={fail} said={said:?} at_root={at_root}",
        titles(&opened)
    );

    // From the shell that has the keyboard: a Ctrl+Shift chord is the window's there.
    super::git::xtest("key ctrl+shift+k").await;
    let closed = until(|| left() == 1, 5000).await;
    println!(
        "bench commands from_shell opened={:?} closed={closed} left={}",
        titles(&opened),
        left()
    );
    folder_terminal(app).await;
    let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
    glib::timeout_future(Duration::from_millis(500)).await;
    println!("bench commands held={}", super::keys::bench_held());
    // Quit as Ctrl+Q does, through the window's close, which writes the session `open` starts
    // from: `bench_quit` ends the process with the closes' debounced save still pending, and the
    // session as last written puts back a closed shell's tab with a new shell in it.
    if let Some(gtk_app) = app.window.application() {
        gtk_app.activate_action("quit", None);
    }
}

/// Open in Terminal, first on a folder row's menu and run through the menu's own action: a shell
/// of its own that starts in that folder and takes the keyboard. Its tab is closed again.
async fn folder_terminal(app: &Rc<App>) {
    let (Some(ops), Some(tree)) = (app.ops().cloned(), app.tree.get()) else {
        return println!("bench commands folder no tree");
    };
    let dir = "Some folder";
    std::fs::create_dir_all(app.root().join(dir)).unwrap();
    let at = gdk::Rectangle::new(0, 0, 1, 1);
    let menu = fileops::context_menu(&ops, tree.widget(), Some((dir, true)), &[], at);
    let first = menu
        .menu_model()
        .and_then(|m| fileops::labels(&m).into_iter().next());
    menu.popdown();
    let before: Vec<String> = app.terminals().iter().map(|t| t.key()).collect();
    let _ = WidgetExt::activate_action(tree.widget(), "fileops.terminal", Some(&dir.to_variant()));
    let new = || (app.terminals().into_iter()).find(|t| !before.contains(&t.key()));
    let (opened, term) = (until(|| new().is_some(), 5000).await, new());
    let mut pwd = false;
    if let Some(term) = &term {
        until(|| !text(term).trim().is_empty(), 5000).await;
        term.view.feed_child(b"pwd\n");
        let path = app.root().join(dir).display().to_string();
        pwd = until(|| text(term).lines().any(|l| l.trim_end() == path), 5000).await;
    }
    let focused = term.as_ref().is_some_and(|t| t.view.has_focus());
    println!("bench commands folder first={first:?} opened={opened} pwd={pwd} focused={focused}");
    let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
    until(|| new().is_none(), 5000).await;
}

async fn open(app: &Rc<App>) {
    set(
        app,
        vec![
            command("Nap", "sleep 60", None),
            command("Quick", "sleep 3", None),
        ],
    );
    for action in ["win.command-0", "win.command-1"] {
        let _ = WidgetExt::activate_action(&app.window, action, None);
    }
    glib::timeout_future(Duration::from_millis(1500)).await;
    println!(
        "bench commands open terminals={} held={}",
        app.terminals().len(),
        super::keys::bench_held()
    );
    if let Some(gtk_app) = app.window.application() {
        gtk_app.activate_action("quit", None);
    }
}

async fn back(app: &Rc<App>) {
    // The restored tabs settle: Quick finds nothing to take up and goes.
    until(|| app.terminals().len() == 1, 5000).await;
    glib::timeout_future(Duration::from_millis(500)).await;
    let titles: Vec<String> = app
        .terminals()
        .iter()
        .map(|t| t.page.title().to_string())
        .collect();
    println!(
        "bench commands back titles={titles:?} held={}",
        super::keys::bench_held()
    );
    let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
    glib::timeout_future(Duration::from_millis(1500)).await;
    println!(
        "bench commands back closed held={}",
        super::keys::bench_held()
    );
    bench_quit(app);
}

/// Every widget under `root` of type `T`, in order.
fn all<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Vec<T> {
    let mut found: Vec<T> = root.clone().downcast::<T>().into_iter().collect();
    let mut child = root.first_child();
    while let Some(widget) = child {
        found.extend(all::<T>(&widget));
        child = widget.next_sibling();
    }
    found
}

async fn prefs(app: &Rc<App>) {
    set(
        app,
        vec![
            command("Claude", "claude", Some("<Primary>j")),
            command("Build", "make -j8", None),
        ],
    );
    let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
    glib::timeout_future(Duration::from_millis(800)).await;
    let Some(dialog) = app.window.visible_dialog() else {
        println!("bench commands prefs dialog=none");
        return bench_quit(app);
    };
    let dialog: gtk::Widget = dialog.upcast();
    let rows = || all::<adw::ExpanderRow>(&dialog);
    let shown: Vec<(String, String)> = rows()
        .iter()
        .map(|r| (r.title().to_string(), r.subtitle().to_string()))
        .collect();
    let clash = rows().first().is_some_and(|row| {
        all::<gtk::Button>(row.upcast_ref())
            .iter()
            .any(|b| b.has_css_class("error"))
    });
    println!("bench commands prefs rows={shown:?} clash={clash}");
    if let Some(row) = rows().first() {
        row.set_expanded(true);
    }
    glib::timeout_future(Duration::from_millis(300)).await;
    // The group is near the end of the page.
    for scroller in all::<gtk::ScrolledWindow>(&dialog) {
        let at = scroller.vadjustment();
        at.set_value(at.upper() - at.page_size());
    }
    glib::timeout_future(Duration::from_millis(300)).await;
    if let Ok(png) = std::env::var("ACCENT_BENCH_COMMANDS_SHOT") {
        let shot = super::minimap::window_png(&app.window, Path::new(&png));
        println!("bench commands prefs shot={shot}");
    }

    let button = |root: &gtk::Widget, title: &str| {
        all::<adw::ButtonRow>(root)
            .into_iter()
            .find(|b| b.title() == title)
    };
    if let Some(add) = button(&dialog, "Add Command") {
        add.emit_by_name::<()>("activated", &[]);
    }
    glib::timeout_future(Duration::from_millis(300)).await;
    let names = || -> Vec<String> {
        let commands = app.config.borrow().commands.clone();
        commands.into_iter().map(|c| c.name).collect()
    };
    let opened: Vec<bool> = rows().iter().map(|r| r.is_expanded()).collect();
    println!("bench commands prefs added={:?} opened={opened:?}", names());
    if let Some(row) = rows().get(2) {
        let entry = all::<adw::EntryRow>(row.upcast_ref())
            .into_iter()
            .find(|e| e.title() == "Name");
        if let Some(entry) = entry {
            entry.set_text("Lint");
        }
        println!(
            "bench commands prefs renamed={:?} title={}",
            names(),
            row.title()
        );
        if let Some(remove) = button(row.upcast_ref(), "Remove Command") {
            remove.emit_by_name::<()>("activated", &[]);
        }
    }
    glib::timeout_future(Duration::from_millis(300)).await;
    println!(
        "bench commands prefs removed={:?} rows={}",
        names(),
        rows().len()
    );
    bench_quit(app);
}
