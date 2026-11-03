//! Open a recent vault in place of the window's own, through XTEST.

use super::*;

/// `ACCENT_BENCH_SWITCHER=recent:<other vault>,<rel>` puts `<other vault>` (an absolute path) in
/// the recent list, types into `<rel>` without saving it, opens Open Recent and sends Shift+Return
/// on its first row. `bench recent shift_return` says whether the other vault's window opened,
/// this one closed and the note was written by the close. From the other window a Shift+click on
/// this vault's row then opens it again and closes that one (`shift_click`).
pub(super) fn bench_recent(app: &Rc<App>, arg: &str) {
    // Every window installs the drills, and this one opens two more.
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some((other, rel)) = arg.split_once(',') else {
        eprintln!("ACCENT_BENCH_SWITCHER=recent:<other vault>,<rel>");
        return bench_quit(app);
    };
    let (other, rel) = (PathBuf::from(other), rel.to_string());
    app.config.borrow_mut().touch_recent(&other);
    app.open_path(&rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        let (Some(shell), Some(own)) = (
            app.shell.upgrade(),
            app.key.borrow().saved_as().map(Path::to_path_buf),
        ) else {
            return bench_quit(&app);
        };
        let file = app.root().join(&rel);
        let written =
            || std::fs::read_to_string(&file).is_ok_and(|text| text.contains("unsaved line"));
        let _ = WidgetExt::activate_action(&app.window, "win.open-recent", None);
        glib::timeout_future(Duration::from_millis(800)).await;
        super::git::xtest("move 700 450; focus").await;
        // Typed just before the key, so the close has it to write and the second's autosave has
        // not come round yet: `set_text` would load a text and leave the tab unmodified.
        let tab = app.open_tabs().into_iter().find(|t| t.rel() == rel);
        if let Some(tab) = &tab {
            tab.buffer
                .insert(&mut tab.buffer.end_iter(), "\nunsaved line\n");
        }
        let (modified, before) = (
            tab.as_ref().is_some_and(|t| t.save.modified.get()),
            written(),
        );
        super::git::xtest("key shift+Return").await;
        glib::timeout_future(Duration::from_millis(1500)).await;
        let there = shell.app_for(&other);
        println!(
            "bench recent shift_return opened={} closed={} modified={modified} \
             written_before={before} written={}",
            there.is_some(),
            !app.window.is_visible(),
            written(),
        );
        // This window lets go, as the shell has, before its vault opens again.
        drop((tab, app));
        let Some(there) = there else {
            return;
        };

        let _ = WidgetExt::activate_action(&there.window, "win.open-recent", None);
        glib::timeout_future(Duration::from_millis(800)).await;
        let row = there.window.visible_dialog().and_then(|dialog| {
            find_widget(dialog.upcast_ref(), &|w| w.is::<gtk::ListView>())?.first_child()
        });
        let (sx, sy) = there.window.surface_transform();
        let at = row.and_then(|row| {
            let middle = graphene::Point::new(row.width() as f32 / 2.0, row.height() as f32 / 2.0);
            row.compute_point(&there.window, &middle)
        });
        if let Some(at) = at {
            let (x, y) = (at.x() as f64 + sx, at.y() as f64 + sy);
            super::git::xtest(&format!(
                "move {x:.0} {y:.0}; keydown shift; down; up; keyup shift"
            ))
            .await;
        }
        glib::timeout_future(Duration::from_millis(1500)).await;
        let back = shell.app_for(&own);
        println!(
            "bench recent shift_click opened={} closed={}",
            back.is_some(),
            !there.window.is_visible(),
        );
        bench_quit(back.as_ref().unwrap_or(&there));
    });
}
