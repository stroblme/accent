//! The Files tree's links to nothing: marked, an editor's lock never listed, and taken in once the
//! file a link names appears.

use super::*;

/// `ACCENT_BENCH_DANGLING=1` prints the vault root's rows `link.md`, `.#note.md` and `.linked.md`
/// once listed, with Show Hidden Files' state (`open`). Then it makes an editor's lock
/// `.#other.md` and a link to nothing `gone.md`, and prints both once `gone.md` is listed
/// (`made`); then writes `real/x.md`, the file `link.md` names, and prints that row once it is no
/// longer marked, and whether the index holds it (`target`). A row reads as its kind, `b` for a
/// link to nothing, or `-` for none. `drill_dangling` makes the links; this writes into the vault,
/// a scratch one.
pub(super) fn bench_dangling(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let (Some(tree), Some(vault)) = (app.tree.get(), app.vault().cloned()) else {
            println!("bench dangling no_tree");
            return bench_quit(&app);
        };
        let model = tree.model();
        let kind = |rel: &str| {
            tree::find_row(model, rel)
                .and_then(|row| row.item())
                .and_then(|item| tree::decode(&item))
                .map_or('-', |row| row.kind)
        };
        until("dangling", "reconciled", 60_000, || app.reconciled.get()).await;
        until("dangling", "listed", 10_000, || kind("link.md") != '-').await;
        let show_hidden = app
            .window
            .lookup_action("show-hidden-files")
            .and_then(|a| a.state())
            .and_then(|s| s.get::<bool>());
        println!(
            "bench dangling open show_hidden={show_hidden:?} link.md={} .#note.md={} .linked.md={}",
            kind("link.md"),
            kind(".#note.md"),
            kind(".linked.md")
        );
        let root = app.root();
        let made = std::os::unix::fs::symlink("user@host.1:1", root.join(".#other.md"))
            .and_then(|()| std::os::unix::fs::symlink("nowhere.md", root.join("gone.md")));
        if let Err(e) = made {
            println!("bench dangling made=false {e}");
            return bench_quit(&app);
        }
        until("dangling", "gone.md", 10_000, || {
            kind("gone.md") == tree::DANGLING
        })
        .await;
        println!(
            "bench dangling made gone.md={} .#other.md={}",
            kind("gone.md"),
            kind(".#other.md")
        );
        if let Err(e) = std::fs::write(root.join("real/x.md"), "# X\n") {
            println!("bench dangling target=false {e}");
            return bench_quit(&app);
        }
        until("dangling", "target", 10_000, || kind("link.md") == 'f').await;
        let indexed =
            (vault.file_paths(false)).is_ok_and(|rels| rels.iter().any(|r| r == "link.md"));
        println!(
            "bench dangling target link.md={} indexed={indexed}",
            kind("link.md")
        );
        bench_quit(&app);
    });
}
