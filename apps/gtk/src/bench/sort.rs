//! The Files tree's sorts (`win.sort-files`) and Sort Folders Among Files.

use super::*;
use accent_core::order::Sort;

/// `ACCENT_BENCH_SORT=1` opens the folder `beta`, then picks each sort through the Sort Files
/// menu's action and prints the root's rows, `bench sort <sort> <rows> open=<beta still open>`;
/// then `modified-new` with folders among the files, and the sort the session keeps.
/// `shot:<dir>` instead writes `<dir>/sort-light.png` and `-dark.png`, the menu open.
pub(super) fn bench_sort(app: &Rc<App>, arg: &str) {
    let (app, arg) = (app.clone(), arg.to_string());
    glib::spawn_future_local(async move {
        let Some(tree) = app.tree.get() else {
            println!("bench sort no_tree");
            return bench_quit(&app);
        };
        let model = tree.model();
        if until("sort", "beta", 10_000, || tree::find_row(model, "beta"))
            .await
            .map(|row| row.set_expanded(true))
            .is_none()
        {
            return bench_quit(&app);
        }
        until("sort", "beta/inner.md", 10_000, || {
            tree::find_row(model, "beta/inner.md").is_some()
        })
        .await;
        let pick = |sort: Sort| {
            let target = sort.name().to_variant();
            let _ = WidgetExt::activate_action(&app.window, "win.sort-files", Some(&target));
        };
        if let Some(dir) = arg.strip_prefix("shot:") {
            pick(Sort::ModifiedNew);
            glib::timeout_future(Duration::from_millis(600)).await;
            for dark in [false, true] {
                app.config.borrow_mut().theme = match dark {
                    true => accent_core::config::Theme::Dark,
                    false => accent_core::config::Theme::Light,
                };
                app.config_changed();
                app.toasts.dismiss_all();
                tree.show_sort_menu();
                glib::timeout_future(Duration::from_millis(800)).await;
                shot(
                    &app,
                    &format!("{dir}/sort-{}.png", ["light", "dark"][dark as usize]),
                )
                .await;
            }
            return bench_quit(&app);
        }
        let open = || tree::find_row(model, "beta").is_some_and(|row| row.is_expanded());
        for sort in Sort::ALL {
            pick(sort);
            // Listed again on a worker.
            glib::timeout_future(Duration::from_millis(600)).await;
            println!(
                "bench sort {} {:?} open={}",
                sort.name(),
                rows(tree),
                open()
            );
        }
        pick(Sort::ModifiedNew);
        app.config.borrow_mut().folders_among_files = true;
        app.config_changed();
        glib::timeout_future(Duration::from_millis(600)).await;
        println!("bench sort among {:?} open={}", rows(tree), open());
        println!(
            "bench sort session {}",
            app.current_session().file_order.name()
        );
        bench_quit(&app);
    });
}

/// The root's rows, top to bottom.
fn rows(tree: &tree::Tree) -> Vec<String> {
    let model = tree.model();
    (0..model.n_items())
        .filter_map(|i| model.item(i).and_downcast::<gtk::TreeListRow>())
        .filter(|row| row.depth() == 0)
        .filter_map(|row| row.item().as_ref().and_then(tree::decode))
        .map(|row| row.rel)
        .collect()
}

/// The window as the X server shows it, popovers included, into `path`.
async fn shot(app: &Rc<App>, path: &str) {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let size = format!("{}x{}+0+0", app.window.width(), app.window.height());
    let argv = [
        "magick", "import", "-display", &display, "-window", "root", "-crop", &size, "+repage",
        path,
    ];
    let run = gio::Subprocess::newv(&argv.map(std::ffi::OsStr::new), gio::SubprocessFlags::NONE);
    match run {
        Ok(run) => match run.wait_check_future().await {
            Ok(()) => println!("bench sort shot {path}"),
            Err(e) => println!("bench sort shot_failed {e}"),
        },
        Err(e) => println!("bench sort shot_failed {e}"),
    }
}
