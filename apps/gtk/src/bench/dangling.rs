//! The Files tree's links: one to nothing marked, an editor's lock never listed, and a link into
//! the vault listed beside the file it names, the index holding that file once.

use super::*;

/// `ACCENT_BENCH_DANGLING=1` opens the folder `real` and prints the rows `link.md`, `.#note.md`,
/// `.linked.md`, `pre.md` and `real/pre.md` once listed, with Show Hidden Files' state (`open`).
/// Then it makes an editor's lock `.#other.md` and a link to nothing `gone.md`, and prints both
/// once `gone.md` is listed (`made`); then writes `real/x.md`, the file `link.md` names, and
/// prints both rows once `link.md` is no longer marked, and whether the index holds the file once,
/// under `real/x.md` (`target`). A row reads as its kind, `b` for a link to nothing, followed by
/// `i` where the index holds it, or `-` for none. `drill_dangling` makes the links; this writes
/// into the vault, a scratch one.
pub(super) fn bench_dangling(app: &Rc<App>) {
    let app = app.clone();
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
        let row = |rel: &str| match found(rel) {
            Some(row) => format!("{}{}", row.kind, if row.indexed { "i" } else { "" }),
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
        bench_quit(&app);
    });
}
