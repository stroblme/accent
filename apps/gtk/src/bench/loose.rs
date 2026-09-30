//! A vault's files opened in windows of their own, where no vault is behind them (DESIGN.md,
//! Window without a vault): what such a window still reads from the file itself.

use super::*;

/// The folder the drill writes its files into, removed again before it quits.
const DIR: &str = "loose-drill";

/// See `ACCENT_BENCH_TABS=loose:` in `mod.rs`.
pub(super) fn bench_loose(app: &Rc<App>) {
    // A window with no vault is one this drill opened, which runs the hooks as well.
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    if vault.is_remote() {
        println!("bench loose needs a local vault");
        return bench_quit(app);
    }
    let (dir, note) = (vault.root().join(DIR), format!("{DIR}/n.md"));
    let text = "# Loose\n\ntext\n\n## Beside\n\nmore\n";
    let written =
        std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join("n.md"), text));
    if let Err(e) = written {
        println!("bench loose cannot write {note}: {e}");
        return bench_quit(app);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        if let Some(apart) = opened_apart(&app, &note).await {
            apart.show_pane("outline");
            glib::timeout_future(Duration::from_millis(800)).await;
            let rows: Vec<String> = apart
                .active()
                .map(|tab| tab.lang.outline().into_iter().map(|row| row.1).collect())
                .unwrap_or_default();
            println!(
                "bench loose_outline pane={:?} rows={rows:?}",
                panes::bench_outline(&apart)
            );
        }
        let _ = std::fs::remove_dir_all(dir);
        bench_quit(&app);
    });
}

/// The window Open in New Window on `rel`'s tree row builds, once the file has opened there.
async fn opened_apart(app: &Rc<App>, rel: &str) -> Option<Rc<App>> {
    let (shell, tree, ops) = (app.shell.upgrade()?, app.tree.get()?, app.ops()?);
    // The row's menu puts the group its items resolve in on the tree.
    let at = gdk::Rectangle::new(0, 0, 1, 1);
    fileops::context_menu(ops, tree.widget(), Some((rel, false)), &[], at).popdown();
    let _ =
        WidgetExt::activate_action(tree.widget(), "fileops.open-apart", Some(&rel.to_variant()));
    let key = app.root().join(rel).to_string_lossy().into_owned();
    for _ in 0..50 {
        glib::timeout_future(Duration::from_millis(100)).await;
        let apart = shell
            .windows
            .borrow()
            .iter()
            .find(|w| !Rc::ptr_eq(w, app) && w.doc_for(&key).is_some())
            .cloned();
        if apart.is_some() {
            return apart;
        }
    }
    println!("bench loose {rel} did not open apart");
    None
}
