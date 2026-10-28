//! `ACCENT_BENCH_COMMANDS`: the user's own commands (`[[commands]]`), run from Run Command and by
//! their shortcuts through XTEST, and their tabs put back after a restart only while they run.
//! The lines are harmless (`true`, `false`, `pwd`, `sleep`), under a scratch `HOME`.

use super::*;
use accent_core::config::CustomCommand;
use vte4::TerminalExt as _;

/// `=run`: Pass (`true`) picked in Run Command and by its chord from a shell closes its tab; Fail
/// (`pwd; false`) by its chord keeps its tab at the vault root, saying how it ended. `=open`
/// then `=back`, on one scratch state: Nap (`sleep 60`) and Quick (`sleep 3`) run, the app quits,
/// and once Quick has ended the restart puts back Nap alone.
pub(super) fn bench_commands(app: &Rc<App>, step: &str) {
    let (app, step) = (app.clone(), step.to_string());
    glib::spawn_future_local(async move {
        match step.as_str() {
            "open" => open(&app).await,
            "back" => back(&app).await,
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
    let _ = WidgetExt::activate_action(&app.window, "win.close-tab", None);
    glib::timeout_future(Duration::from_millis(500)).await;
    println!("bench commands held={}", super::keys::bench_held());
    bench_quit(app);
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
