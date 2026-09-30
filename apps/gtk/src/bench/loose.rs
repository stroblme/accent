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
    let (dir, note) = (vault.root().join(DIR), format!("{DIR}/n/n.md"));
    // Its images beside it and under it, then one above its folder, named by a relative link and
    // through a symlink out of the folder.
    let text = "# Loose\n\ntext\n\n## Beside\n\n![](dot.png)\n\n![[dot.png]]\n\n\
        ![](sub/dot.png)\n\n## Above\n\n![](../above.png)\n\n![](out.png)\n";
    let written = std::fs::create_dir_all(dir.join("n/sub"))
        .and_then(|_| std::fs::write(dir.join("n/n.md"), text))
        .and_then(|_| std::fs::write(dir.join("n/dot.png"), png(4, 3)))
        .and_then(|_| std::fs::write(dir.join("n/sub/dot.png"), png(4, 3)))
        .and_then(|_| std::fs::write(dir.join("above.png"), png(4, 3)))
        .and_then(|_| std::os::unix::fs::symlink("../above.png", dir.join("n/out.png")));
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
            apart.set_mode(Mode::Split);
            glib::timeout_future(Duration::from_millis(1500)).await;
            let root = format!("accent://file/{}/", dir.display());
            for (src, .., loaded) in image::page_images(&apart).await {
                println!(
                    "bench loose_preview {} loaded={loaded}",
                    src.replace(&root, "…/")
                );
            }
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

/// A `w`×`h` PNG of one grey, which the drill tells apart from another by its size.
fn png(w: i32, h: i32) -> glib::Bytes {
    let pixels = glib::Bytes::from_owned(vec![128u8; (w * h * 4) as usize]);
    let format = gdk::MemoryFormat::R8g8b8a8;
    gdk::MemoryTexture::new(w, h, format, &pixels, (w * 4) as usize).save_to_png_bytes()
}
