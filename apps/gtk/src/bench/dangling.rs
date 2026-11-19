//! The Files tree's links: one to nothing marked, an editor's lock never listed, and a link into
//! the vault listed beside the file it names, the index holding that file once and finding it by
//! the link's name too.

use super::*;

/// `ACCENT_BENCH_DANGLING=1` opens the folder `real` and prints the rows `link.md`, `.#note.md`,
/// `.linked.md`, `pre.md` and `real/pre.md` once listed, with Show Hidden Files' state (`open`).
/// Then it makes an editor's lock `.#other.md` and a link to nothing `gone.md`, and prints both
/// once `gone.md` is listed (`made`); then writes `real/x.md`, the file `link.md` names, and
/// prints both rows once `link.md` is no longer marked, and whether the index holds the file once,
/// under `real/x.md` (`target`). Last it follows `[[shortcut]]`, the name of a second link to
/// `real/pre.md`, and prints the tab it opened (`alias`), then Go to File's rows for `shortcut`
/// (`switcher_row`). A row reads as its kind, `b` for a link to nothing, followed by
/// `i` where the index holds it and `+link` where the link is drawn after its name, or `-` for
/// none. `drill_dangling` makes the links; this writes into the vault, a scratch one. With a
/// folder in place of `1`, it pictures the window there after `open`, light and dark
/// (`dangling-light.png`, `dangling-dark.png`), and quits.
pub(super) fn bench_dangling(app: &Rc<App>, arg: &str) {
    let (app, shots) = (app.clone(), (arg != "1").then(|| arg.to_string()));
    glib::spawn_future_local(async move {
        let (Some(tree), Some(vault)) = (app.tree.get(), app.vault().cloned()) else {
            println!("bench dangling no_tree");
            return bench_quit(&app);
        };
        let model = tree.model();
        let found = |rel: &str| {
            tree::find_row(model, rel)
                .and_then(|row| row.item())
                .and_then(|item| tree::decode(&item))
        };
        // The link after a bound row's name, shown or not.
        let link = |rel: &str| {
            tree::expanders(tree.view()).into_iter().any(|expander| {
                let bound = (expander.list_row().and_then(|row| row.item()))
                    .and_then(|item| tree::decode(&item))
                    .is_some_and(|row| row.rel == rel);
                let drawn = (expander.child().and_then(|row| row.first_child()))
                    .and_then(|icon| icon.next_sibling())
                    .and_then(|label| label.next_sibling())
                    .is_some_and(|link| link.is_visible());
                bound && drawn
            })
        };
        let row = |rel: &str| match found(rel) {
            Some(row) => format!(
                "{}{}{}",
                row.kind,
                if row.indexed { "i" } else { "" },
                if link(rel) { "+link" } else { "" }
            ),
            None => "-".to_string(),
        };
        until("dangling", "reconciled", 60_000, || app.reconciled.get()).await;
        if until("dangling", "real", 10_000, || tree::find_row(model, "real"))
            .await
            .map(|real| real.set_expanded(true))
            .is_none()
        {
            return bench_quit(&app);
        }
        until("dangling", "listed", 10_000, || {
            found("link.md").is_some() && found("real/pre.md").is_some()
        })
        .await;
        let show_hidden = app
            .window
            .lookup_action("show-hidden-files")
            .and_then(|a| a.state())
            .and_then(|s| s.get::<bool>());
        println!(
            "bench dangling open show_hidden={show_hidden:?} link.md={} .#note.md={} .linked.md={} pre.md={} real/pre.md={}",
            row("link.md"),
            row(".#note.md"),
            row(".linked.md"),
            row("pre.md"),
            row("real/pre.md")
        );
        if shots.is_some() {
            for (theme, name) in [
                (accent_core::config::Theme::Light, "dangling-light"),
                (accent_core::config::Theme::Dark, "dangling-dark"),
            ] {
                app.config.borrow_mut().theme = theme;
                app.config_changed();
                glib::timeout_future(Duration::from_millis(800)).await;
                super::menus::shot(&shots, name).await;
            }
            return bench_quit(&app);
        }
        let root = app.root();
        let made = std::os::unix::fs::symlink("user@host.1:1", root.join(".#other.md"))
            .and_then(|()| std::os::unix::fs::symlink("nowhere.md", root.join("gone.md")));
        if let Err(e) = made {
            println!("bench dangling made=false {e}");
            return bench_quit(&app);
        }
        until("dangling", "gone.md", 10_000, || {
            found("gone.md").is_some_and(|row| row.kind == tree::DANGLING)
        })
        .await;
        println!(
            "bench dangling made gone.md={} .#other.md={}",
            row("gone.md"),
            row(".#other.md")
        );
        if let Err(e) = std::fs::write(root.join("real/x.md"), "# X\n") {
            println!("bench dangling target=false {e}");
            return bench_quit(&app);
        }
        until("dangling", "target", 10_000, || {
            found("link.md").is_some_and(|row| row.kind == 'f') && found("real/x.md").is_some()
        })
        .await;
        let once = (vault.file_paths(false)).is_ok_and(|rels| {
            rels.iter().any(|r| r == "real/x.md") && !rels.iter().any(|r| r == "link.md")
        });
        println!(
            "bench dangling target link.md={} real/x.md={} once={once}",
            row("link.md"),
            row("real/x.md")
        );
        app.open_target("shortcut");
        let opened = || app.active_key().is_some();
        until("dangling", "followed", 10_000, opened).await;
        println!("bench dangling alias followed={:?}", app.active_key());
        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        let dialog = app.window.visible_dialog();
        let entry = dialog.and_then(|d| find_search_entry(d.upcast_ref()));
        if let Some(entry) = entry {
            entry.set_text("shortcut");
        }
        // For the palette's names, asked for as it opened, to land.
        glib::timeout_future(Duration::from_millis(1500)).await;
        bench_switcher_rows(&app);
        bench_quit(&app);
    });
}
