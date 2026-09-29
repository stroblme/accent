//! Whether the Files tree and the Git pane's two lists keep their place through what changes
//! under them.

use super::*;

/// `ACCENT_BENCH_SCROLL=<rel_dir>`: scroll each list half way down, put the keyboard on a row on
/// screen as a click does, change the vault under it from outside the app, and print where the
/// list is and whether it still has the keyboard after each change.
///
/// It makes the vault a repository ignoring `*.tmp` with 80 commits, so point it at a scratch
/// vault, and at a folder of a hundred files or so: the tree is scrolled with that folder open,
/// and 60 of its files are changed for the Git pane's Changes. The Files half makes a file git
/// ignores, which redraws every row, then two files at once, sorting first and last in the
/// folder, so the tree's splice spans the row with the keyboard, edits one and removes all three.
/// The Git half asks for a refresh that finds nothing new, stages a file below the rows on
/// screen, clicks the Stage button of one on screen, whose row leaves Changes, and commits with
/// the keyboard on a history row, which replaces the whole page. Last the Search pane, with the
/// keyboard on a result of `calibration` when a note holding the word is made, and the Tags pane,
/// with it on a tag when a note with a new one is. On a remote vault the changes are made on the
/// host, over the vault's own ssh master.
pub(super) fn bench_scroll(app: &Rc<App>, dir: &str) {
    scratch_only(app, "ACCENT_BENCH_SCROLL");
    let (app, dir) = (app.clone(), dir.to_string());
    glib::spawn_future_local(async move {
        let d = accent_api::ssh::quote(&dir);
        let git = "git -c user.name=bench -c user.email=bench@localhost";
        let made = in_vault(
            &app,
            &format!(
                "git init -q && printf '*.tmp\\n' >> .gitignore && git add -A && \
                 {git} commit -qm base && \
                 for i in $(seq 80); do {git} commit -q --allow-empty -m \"step $i\"; done && \
                 git ls-files {d} | head -60 | while read -r f; do echo changed >> \"$f\"; done"
            ),
        );
        println!("bench scroll repository={made}");

        let tree = app.tree.get().expect("a tree");
        app.show_pane("files");
        // A minute: a remote vault may still be uploading its server.
        for _ in 0..300 {
            if tree.reveal(&dir) {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        if let Some(row) = tree::find_row(tree.model(), &dir) {
            row.set_expanded(true);
        }
        let files = tree.view().vadjustment().expect("the tree scrolls");
        halfway(&files).await;
        focus_row(tree.view());
        let files_at = |step: &str| say(&app, "files", step, tree.view());
        files_at("before");
        let (first, last, ignored) = (
            format!("{d}/0000-scroll.md"),
            format!("{d}/zzzz-scroll.md"),
            format!("{d}/scroll.tmp"),
        );
        for (step, script) in [
            ("ignored", format!("echo x > {ignored}")),
            ("made", format!("touch {first} {last}")),
            ("edited", format!("echo more >> {first}")),
            ("removed", format!("rm {first} {last} {ignored}")),
        ] {
            in_vault(&app, &script);
            glib::timeout_future(Duration::from_secs(3)).await;
            files_at(step);
        }

        app.show_pane("git");
        let Some(panel) = app.git.get().cloned() else {
            println!("bench scroll no_git_pane");
            return bench_quit(&app);
        };
        let list = |child: Option<gtk::Widget>| {
            child
                .and_then(|w| find_widget(&w, &|w| w.is::<gtk::ListView>()))
                .and_downcast::<gtk::ListView>()
                .expect("a list")
        };
        let changes = list(panel.divider().start_child());
        let log = list(panel.divider().end_child());
        for view in [&changes, &log] {
            halfway(&view.vadjustment().expect("the list scrolls")).await;
        }
        let both = |step: &str| {
            say(&app, "changes", step, &changes);
            say(&app, "log", step, &log);
        };
        both("before");
        panel.schedule_refresh(crate::git::Depth::Everything);
        glib::timeout_future(Duration::from_secs(2)).await;
        both("refresh");
        // Git lists the changes by path, so the last one is below the rows on screen.
        in_vault(&app, "git add \"$(git diff --name-only | tail -1)\"");
        glib::timeout_future(Duration::from_secs(3)).await;
        both("stage_below");
        // A click leaves the keyboard on the button it pressed.
        let stage = shown_rows(&changes)
            .iter()
            .filter_map(|row| super::git::row_button(row, "Stage"))
            .nth(8);
        println!("bench scroll stage_button={}", stage.is_some());
        if let Some(stage) = stage {
            stage.grab_focus();
            stage.emit_clicked();
        }
        glib::timeout_future(Duration::from_secs(3)).await;
        both("stage_clicked");
        focus_row(&log);
        in_vault(&app, &format!("{git} commit -qm staged"));
        glib::timeout_future(Duration::from_secs(3)).await;
        both("commit");

        // The Search pane asks its query again whenever the vault changes, and replaces its rows
        // whole.
        app.show_pane("search");
        let sidebar = app.sidebar.get().expect("a sidebar");
        sidebar.set_search_text("calibration");
        let results = sidebar.search_view().expect("the results");
        halfway(&results.vadjustment().expect("the results scroll")).await;
        focus_row(&results);
        say(&app, "search", "before", &results);
        in_vault(&app, "echo calibration > scroll-calibration.md");
        glib::timeout_future(Duration::from_secs(3)).await;
        say(&app, "search", "made", &results);

        // The Tags pane asks for every tag again once the vault has been still for a moment.
        app.show_pane("tags");
        let tags = find_widget(sidebar.widget(), &|w| w.is::<adw::ViewStack>())
            .and_downcast::<adw::ViewStack>()
            .and_then(|stack| stack.child_by_name("tags"))
            .and_then(|pane| find_widget(&pane, &|w| w.is::<gtk::ListView>()))
            .and_downcast::<gtk::ListView>()
            .expect("the tag list");
        halfway(&tags.vadjustment().expect("the tags scroll")).await;
        focus_row(&tags);
        say(&app, "tags", "before", &tags);
        in_vault(&app, "echo '#aaaa-scroll' > scroll-tag.md");
        glib::timeout_future(Duration::from_secs(4)).await;
        say(&app, "tags", "made", &tags);
        bench_quit(&app);
    });
}

/// Scroll to half way down, once the list has laid out whatever it was just given.
async fn halfway(adjustment: &gtk::Adjustment) {
    glib::timeout_future(Duration::from_millis(1500)).await;
    adjustment.set_value((adjustment.upper() - adjustment.page_size()) / 2.0);
    glib::timeout_future(Duration::from_millis(300)).await;
}

/// The row widgets `view` has on screen, top to bottom.
fn shown_rows(view: &gtk::ListView) -> Vec<gtk::Widget> {
    let mut rows = Vec::new();
    let mut row = view.first_child();
    while let Some(r) = row {
        row = r.next_sibling();
        if r.is_mapped() {
            rows.push(r);
        }
    }
    rows
}

/// Put the keyboard on the eighth row on screen, where a click on it would leave it.
fn focus_row(view: &gtk::ListView) {
    let focused = shown_rows(view).get(8).is_some_and(|row| row.grab_focus());
    println!("bench scroll focused={focused}");
}

fn say(app: &Rc<App>, list: &str, step: &str, view: &gtk::ListView) {
    let Some(adjustment) = view.vadjustment() else {
        return;
    };
    let focus = GtkWindowExt::focus(&app.window).is_some_and(|f| f.is_ancestor(view));
    println!(
        "bench scroll {list} {step} value={:.0} upper={:.0} focus={focus}",
        adjustment.value(),
        adjustment.upper()
    );
}

/// Run `script` in the vault root: here with `sh`, or on the host over the vault's own master.
pub(super) fn in_vault(app: &Rc<App>, script: &str) -> bool {
    let Some(vault) = app.vault() else {
        return false;
    };
    let root = vault.root();
    let mut command = match vault.remote() {
        Some(remote) => {
            let script = format!(
                "cd {} && {script}",
                accent_api::ssh::quote(&root.to_string_lossy())
            );
            let argv = accent_api::ssh::run(remote.url(), remote.control_path(), &script);
            let mut command = std::process::Command::new(&argv[0]);
            command.args(&argv[1..]);
            command
        }
        None => {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", script]).current_dir(&root);
            command
        }
    };
    command.status().is_ok_and(|s| s.success())
}
