//! Drills over the Files tree: a row's context menu and the marked set, expanding and unfolding
//! folders, the folders the index does not walk, a drop from a file manager and Move to….

use super::files::until;
use super::*;

/// Open a tree row's context menu and then take the pointer away from the list, which is what the
/// popover itself does: the highlight has to stay on the row the menu is pointing at. Then mark a
/// second row as a Ctrl+click does and open the menu again, which is the marked set's whole
/// mechanism bar the modifier: the rows that carry the mark class, and the items a menu over one
/// of them offers.
///
/// The leave is emitted on the list's own motion controller, found among its controllers, because
/// under Xvfb nothing moves a pointer. That is the event the popover's grab really sends, so this
/// drives the mechanism the bug was in; what it does not show is the menu on screen over the lit
/// row, which wants eyes — nor does anything here press Ctrl, there being no pointer to hold it
/// with.
pub(super) fn bench_menu(app: &Rc<App>, rel: &str) {
    let Some(ops) = app.ops().cloned() else {
        return bench_quit(app);
    };
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        // The root listing lands from a worker thread and each folder above the row is listed
        // again as it is expanded, so the path is not in the model on the first frame.
        for _ in 0..50 {
            if tree.reveal(&rel) {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        glib::timeout_future(Duration::from_millis(400)).await;
        let row = tree.selected();
        println!("bench menu_row {:?}", row.as_ref().map(|row| &row.rel));
        // Whether the tree would open a menu here at all, and what it would hold: a row inside a
        // dependency tree gets none (`wire::wire_tree`), a gitignored one the whole of it.
        if let Some(row) = &row {
            let at = gdk::Rectangle::new(0, 0, 1, 1);
            let items = (!row.dependency).then(|| {
                let menu = fileops::context_menu(
                    &ops,
                    tree.widget(),
                    Some((&row.rel, row.is_dir())),
                    &[],
                    at,
                );
                let items = menu.menu_model().map(|m| fileops::labels(&m));
                menu.popdown();
                items
            });
            let items = items.flatten();
            println!(
                "bench menu_items {} dir={} dependency={} {items:?}",
                row.rel,
                row.is_dir(),
                row.dependency,
            );
        }

        let at = gdk::Rectangle::new(0, 0, 1, 1);
        let popover = fileops::context_menu(&ops, tree.widget(), Some((&rel, false)), &[], at);
        // What `wire_tree` does with the popover it was handed.
        tree.pin(Some(&rel));
        leave(tree.view());
        println!(
            "bench menu_open selected={:?}",
            tree.selected().map(|row| row.rel)
        );

        popover.popdown();
        tree.pin(None);
        leave(tree.view());
        println!(
            "bench menu_closed selected={:?}",
            tree.selected().map(|row| row.rel)
        );

        // The marked half: this row and one more, the menu over one of them, and the rows drawn
        // with the mark on them once the factory has re-bound what is on screen.
        let other = tree::expanders(tree.view())
            .into_iter()
            .filter_map(|expander| expander.list_row()?.item().as_ref().and_then(tree::decode))
            .find(|row| row.rel != rel && row.indexed);
        tree.toggle_mark(&rel);
        if let Some(other) = &other {
            tree.toggle_mark(&other.rel);
        }
        let marked = tree.marked();
        println!("bench menu_marked {marked:?}");
        let popover = fileops::context_menu(&ops, tree.widget(), Some((&rel, false)), &marked, at);
        let items = popover.menu_model().map(|m| fileops::labels(&m));
        println!("bench menu_marked_items {items:?}");
        glib::timeout_future(Duration::from_millis(200)).await;
        println!("bench menu_marked_drawn {:?}", marked_rows(tree.view()));
        popover.popdown();
        // Escape's half, which is what the key controller calls.
        tree.clear_marks();
        glib::timeout_future(Duration::from_millis(200)).await;
        println!("bench menu_marked_cleared {:?}", marked_rows(tree.view()));
        bench_range(tree, &rel).await;
        bench_quit(&app);
    });
}

/// A Shift+click's range, from `rel` down to the first shut folder below it, which is marked
/// whole; then that folder opened, which shows everything in it marked, and a Ctrl+click on the
/// first of its rows, which takes that one alone out of the set and leaves its siblings marked.
async fn bench_range(tree: &tree::Tree, rel: &str) {
    let model = tree.model();
    let rows: Vec<(tree::Row, bool)> = (0..model.n_items())
        .filter_map(|i| model.item(i).and_downcast::<gtk::TreeListRow>())
        .filter_map(|row| Some((tree::decode(&row.item()?)?, row.is_expanded())))
        .collect();
    let below = rows.iter().skip_while(|(row, _)| row.rel != rel);
    let Some((folder, _)) = below
        .skip(1)
        .find(|(row, open)| row.is_dir() && row.indexed && !open)
    else {
        return println!("bench menu_range none");
    };
    let folder = folder.rel.clone();
    tree.mark_range(rel, &folder, false);
    glib::timeout_future(Duration::from_millis(200)).await;
    println!("bench menu_range {:?}", tree.marked());
    println!("bench menu_range_drawn {:?}", marked_rows(tree.view()));

    if let Some(row) = tree::find_row(model, &folder) {
        row.set_expanded(true);
    }
    // The folder's listing lands from a worker thread.
    glib::timeout_future(Duration::from_millis(800)).await;
    let inside = |rows: Vec<String>| {
        rows.into_iter()
            .filter(|row| row.starts_with(&format!("{folder}/")))
            .collect::<Vec<_>>()
    };
    let shown = inside(
        drawn_rows(tree.view())
            .into_iter()
            .map(|(rel, _)| rel)
            .collect(),
    );
    println!(
        "bench menu_range_opened drawn={} of={}",
        inside(marked_rows(tree.view())).len(),
        shown.len()
    );
    let (Some(first), Some(second)) = (shown.first(), shown.get(1)) else {
        return println!("bench menu_range_split none");
    };
    tree.toggle_mark(first);
    glib::timeout_future(Duration::from_millis(200)).await;
    println!(
        "bench menu_range_split folder_in_set={} {first}={} {second}={} set={} drawn={} of={}",
        tree.marked().iter().any(|(rel, _)| *rel == folder),
        tree.is_marked(first),
        tree.is_marked(second),
        tree.marked().len(),
        inside(marked_rows(tree.view())).len(),
        shown.len()
    );
    tree.clear_marks();
}

/// Reveal a row, print where it is on screen and stay up, for an XTEST Ctrl+click held against
/// it: the modifier is the one half no drill can fake, the mark being made in a gesture that
/// reads the press's own state. Prints the rows drawn marked and how many documents are open
/// three times, five seconds apart, so one run says both what the Ctrl+click marked and that it
/// opened nothing, and then that a plain click let the marks go again; and the colour each of the
/// rows about it is painted in, which is whether a mark shows at all, in whatever theme the
/// scratch `config.toml` names.
pub(super) fn bench_menu_press(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        for _ in 0..50 {
            if tree.reveal(&rel) {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        glib::timeout_future(Duration::from_millis(400)).await;
        match centre(&app, tree, &rel) {
            Some(at) => println!("bench menu_press {} {}", at.x() as i32, at.y() as i32),
            None => println!("bench menu_press none"),
        }
        // And the row two below it, for a Shift+click range to end on.
        let model = tree.model();
        let to = tree::find_row(model, &rel)
            .and_then(|row| model.item(row.position() + 2))
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|row| row.item())
            .and_then(|item| tree::decode(&item))
            .and_then(|row| Some((row.rel.clone(), centre(&app, tree, &row.rel)?)));
        match to {
            Some((rel, at)) => println!(
                "bench menu_press_to {rel} {} {}",
                at.x() as i32,
                at.y() as i32
            ),
            None => println!("bench menu_press_to none"),
        }
        // The row above as well, which is never marked: the colour a mark has to differ from.
        let rows: Vec<String> = [-1, 0, 1, 2]
            .into_iter()
            .filter_map(|off| {
                let at = tree::find_row(model, &rel)?
                    .position()
                    .checked_add_signed(off)?;
                let row = model.item(at).and_downcast::<gtk::TreeListRow>()?.item()?;
                Some(tree::decode(&row)?.rel)
            })
            .collect();
        for step in 0..3 {
            glib::timeout_future(Duration::from_secs(5)).await;
            println!(
                "bench menu_marks {step} {:?} docs={}",
                marked_rows(tree.view()),
                app.docs().len()
            );
            println!(
                "bench menu_colours {step} {:?}",
                row_colours(&app, tree, &rows)
            );
        }
        bench_quit(&app);
    });
}

/// The colour each of `rels`' rows is painted in on screen at its left end, clear of its icon and
/// name: whether a mark shows, which the style class alone does not say.
fn row_colours(app: &App, tree: &tree::Tree, rels: &[String]) -> Vec<(String, String)> {
    let window = app.window.upcast_ref::<gtk::Widget>();
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let (Some(node), Some(renderer)) = (
        snapshot.to_node(),
        window.native().and_then(|n| n.renderer()),
    ) else {
        return Vec::new();
    };
    let mut downloader = gdk::TextureDownloader::new(&renderer.render_texture(&node, None));
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let expanders = tree::expanders(tree.view());
    rels.iter()
        .filter_map(|rel| {
            let row = expanders.iter().find(|e| {
                let item = e.list_row().and_then(|row| row.item());
                item.as_ref()
                    .and_then(tree::decode)
                    .is_some_and(|r| r.rel == *rel)
            })?;
            let middle = graphene::Point::new(4.0, row.height() as f32 / 2.0);
            let at = row.compute_point(window, &middle)?;
            let i = at.y() as usize * stride + at.x() as usize * 4;
            let rgb = bytes.get(i..i + 3)?;
            Some((rel.clone(), format!("{},{},{}", rgb[0], rgb[1], rgb[2])))
        })
        .collect()
}

/// The middle of `rel`'s row in window coordinates, which under Xvfb are the screen's.
fn centre(app: &App, tree: &tree::Tree, rel: &str) -> Option<graphene::Point> {
    tree::expanders(tree.view())
        .into_iter()
        .find(|expander| {
            let row = expander.list_row().and_then(|row| row.item());
            row.as_ref()
                .and_then(tree::decode)
                .is_some_and(|r| r.rel == rel)
        })
        .and_then(|expander| {
            let middle = graphene::Point::new(
                expander.width() as f32 / 2.0,
                expander.height() as f32 / 2.0,
            );
            expander.compute_point(&app.window, &middle)
        })
}

/// The pointer leaving the list, as the popover's own grab sends it.
fn leave(view: &gtk::ListView) {
    let controllers = view.observe_controllers();
    for i in 0..controllers.n_items() {
        if let Some(motion) = controllers
            .item(i)
            .and_downcast::<gtk::EventControllerMotion>()
        {
            motion.emit_by_name::<()>("leave", &[]);
        }
    }
}

/// Every row the list has a widget bound to, as its path and whether its label is dimmed.
pub(super) fn drawn_rows(view: &gtk::ListView) -> Vec<(String, bool)> {
    let mut rows: Vec<(String, bool)> = tree::expanders(view)
        .into_iter()
        .filter_map(|expander| {
            let row = expander.list_row().and_then(|row| row.item());
            let dim = expander
                .child()
                .and_then(|row| row.last_child())
                .is_some_and(|label| label.has_css_class("dim-label"));
            Some((tree::decode(row.as_ref()?)?.rel, dim))
        })
        .collect();
    rows.sort();
    rows
}

/// The rows drawn with the Ctrl+click mark on them, which is what the factory binds off the
/// marked set.
fn marked_rows(view: &gtk::ListView) -> Vec<String> {
    let mut rows: Vec<String> = tree::expanders(view)
        .into_iter()
        .filter(|expander| expander.has_css_class("accent-marked"))
        .filter_map(|expander| {
            let row = expander.list_row().and_then(|row| row.item());
            Some(tree::decode(row.as_ref()?)?.rel)
        })
        .collect();
    rows.sort();
    rows
}

pub(super) fn bench_expand(app: &Rc<App>, rel: &str) {
    let Some(tree) = app.tree.get() else { return };
    let model = tree.model();
    let mut path = String::new();
    for seg in rel.split('/') {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(seg);
        let Some(row) = tree::find_row(model, &path) else {
            println!("bench expand {path} NOT-FOUND");
            return;
        };
        let before = model.n_items();
        let t0 = Instant::now();
        row.set_expanded(true);
        println!(
            "bench expand {path} revealed {} rows in {:.1} ms",
            model.n_items().saturating_sub(before),
            ms_since(t0)
        );
    }
    // `is_expandable` is what `GtkTreeExpander::set_list_row` calls for every row the ListView
    // binds, i.e. the per-row cost paid while scrolling.
    let n = model.n_items();
    let t0 = Instant::now();
    for i in 0..n {
        if let Some(row) = model.item(i).and_downcast::<gtk::TreeListRow>() {
            let _ = row.is_expandable();
        }
    }
    println!("bench bind_probe {n} rows in {:.1} ms", ms_since(t0));
}

/// `ACCENT_BENCH_DROP="<rel_folder> <abs_file> <abs_file>"`: the half of a drag from GNOME Files
/// a headless run can drive. Xvfb carries a drag inside one process and not between two, so this
/// builds the `GdkFileList` a file manager would offer and takes the two ends the app owns.
///
/// First the spring-open: the folder's row is shut, `enter` is emitted on its own drop target the
/// way a drag resting over it does, and the row has to be open a second later. Then the drop
/// itself: `tree::dropped_paths` over the list, then `fileops::import` into that folder, which is
/// the path a paste of GNOME Files' clipboard already takes — a copy first, then a move, which
/// has to leave nothing behind. What no drill sees is the three lines of `connect_drop` glue
/// between the two, and the action a real file manager reports with Shift held.
pub(super) fn bench_drop(app: &Rc<App>, arg: &str) {
    let Some(ops) = app.ops().cloned() else {
        return bench_quit(app);
    };
    let mut words = arg.split_whitespace();
    let Some(dir) = words.next().map(str::to_string) else {
        return bench_quit(app);
    };
    let files: Vec<std::path::PathBuf> = words.map(std::path::PathBuf::from).collect();
    let app = app.clone();
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        for _ in 0..50 {
            if tree.reveal(&dir) {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        glib::timeout_future(Duration::from_millis(400)).await;

        // Shut again, so the spring has something to open.
        let row = tree::find_row(tree.model(), &dir).expect("the folder's row");
        row.set_expanded(false);
        let target = import_target_on(tree.view(), &dir).expect("the row's import target");
        println!("bench drop_before expanded={}", row.is_expanded());
        let _ = target.emit_by_name::<gdk::DragAction>("enter", &[&1.0f64, &1.0f64]);
        glib::timeout_future(Duration::from_millis(1200)).await;
        println!("bench drop_spring expanded={}", row.is_expanded());

        let list =
            gdk::FileList::from_array(&files.iter().map(gio::File::for_path).collect::<Vec<_>>());
        let carried = tree::dropped_paths(&list.to_value());
        println!("bench drop_paths {carried:?}");
        let Some(carried) = carried else {
            return bench_quit(&app);
        };
        // A plain drag copies; the sources stay where they are.
        fileops::import(&ops, &dir, carried.clone(), false);
        glib::timeout_future(Duration::from_millis(1500)).await;
        println!(
            "bench drop_copied {:?} sources_kept={}",
            landed(&app, &dir),
            carried.iter().filter(|p| p.exists()).count()
        );
        // Shift held in the file manager: the same path with the sources taken away.
        fileops::import(&ops, &dir, carried.clone(), true);
        glib::timeout_future(Duration::from_millis(1500)).await;
        println!(
            "bench drop_moved {:?} sources_left={}",
            landed(&app, &dir),
            carried.iter().filter(|p| p.exists()).count()
        );
        bench_quit(&app);
    });
}

/// The drop target on `dir`'s row that takes files from another application, found among the
/// controllers the row expander carries.
fn import_target_on(view: &gtk::ListView, dir: &str) -> Option<gtk::DropTarget> {
    let expander = tree::expanders(view).into_iter().find(|expander| {
        expander
            .list_row()
            .and_then(|row| row.item())
            .as_ref()
            .and_then(tree::decode)
            .is_some_and(|row| row.rel == dir)
    })?;
    expander
        .observe_controllers()
        .into_iter()
        .flatten()
        .filter_map(|c| c.downcast::<gtk::DropTarget>().ok())
        .find(|t| {
            t.formats()
                .is_some_and(|f| f.contains_type(gdk::FileList::static_type()))
        })
}

/// What the vault lists in `dir` now, by name.
fn landed(app: &Rc<App>, dir: &str) -> Vec<String> {
    let Some(vault) = app.vault() else {
        return Vec::new();
    };
    let mut names: Vec<String> = vault
        .list_dir(dir)
        .unwrap_or_default()
        .into_iter()
        .map(|row| accent_core::path::basename(&row.rel_path).to_string())
        .collect();
    names.sort();
    names
}

/// `ACCENT_BENCH_WATCH="<rel_gitignored_dir> <rel_dependency_dir>"`: whether a file written into
/// an open folder the index does not walk reaches the tree.
///
/// Both rows are expanded, a file is written into each from outside the app and then removed
/// again, and the tree's own rows are read after each step. The gitignored folder has to follow
/// the disk (`tree::watch_unindexed`); the dependency tree has to *not*, which is the half that
/// keeps a 40 000-file `node_modules` unwatched.
pub(super) fn bench_watch(app: &Rc<App>, arg: &str) {
    let Some(vault) = app.vault().cloned() else {
        return bench_quit(app);
    };
    let mut words = arg.split_whitespace().map(str::to_string);
    let (Some(ignored), Some(dependency)) = (words.next(), words.next()) else {
        return bench_quit(app);
    };
    let app = app.clone();
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        for dir in [&ignored, &dependency] {
            for _ in 0..50 {
                if tree.reveal(dir) {
                    break;
                }
                glib::timeout_future(Duration::from_millis(200)).await;
            }
            if let Some(row) = tree::find_row(tree.model(), dir) {
                row.set_expanded(true);
            }
        }
        glib::timeout_future(Duration::from_millis(800)).await;
        let children = |dir: &str| {
            let model = tree.model();
            let mut names: Vec<String> = (0..model.n_items())
                .filter_map(|i| model.item(i).and_downcast::<gtk::TreeListRow>()?.item())
                .filter_map(|item| tree::decode(&item))
                .filter_map(|row| row.rel.strip_prefix(&format!("{dir}/")).map(str::to_string))
                .filter(|rest| !rest.contains('/'))
                .collect();
            names.sort();
            names
        };
        for (what, dir) in [("ignored", &ignored), ("dependency", &dependency)] {
            println!("bench watch_{what}_before {:?}", children(dir));
        }
        // Written the way anything outside accent writes: straight to the disk, with nothing
        // telling the app about it.
        let made: Vec<std::path::PathBuf> = [&ignored, &dependency]
            .iter()
            .map(|dir| vault.root().join(dir).join("made.md"))
            .collect();
        for path in &made {
            let _ = std::fs::write(path, "# made\n");
        }
        glib::timeout_future(Duration::from_secs(3)).await;
        for (what, dir) in [("ignored", &ignored), ("dependency", &dependency)] {
            println!("bench watch_{what}_added {:?}", children(dir));
        }
        for path in &made {
            let _ = std::fs::remove_file(path);
        }
        glib::timeout_future(Duration::from_secs(3)).await;
        for (what, dir) in [("ignored", &ignored), ("dependency", &dependency)] {
            println!("bench watch_{what}_removed {:?}", children(dir));
        }
        bench_quit(&app);
    });
}

/// `ACCENT_BENCH_UNFOLD=<rel>,<rel>,…`: open each folder in the tree as clicks on the rows above
/// it would, one level at a time, and print what the tree lists under it once it has.
///
/// `=race:<rel_ignored_dir>` is the folder a build fills while the tree opens it: a folder is made
/// in the open gitignored `<rel_ignored_dir>` with one file, and opened as soon as the tree lists
/// it, with the vault's worker busy on a walk; a second file is written the moment the first is
/// listed, before the new folder's watch can be in place, and the folder's rows are printed once
/// the worker has caught up. The files are written the way anything outside accent writes them,
/// on the host for a remote vault. `=reload:<rel_dir>` is Reload on a folder's menu
/// ([`bench_unfold_reload`]).
pub(super) fn bench_unfold(app: &Rc<App>, arg: &str) {
    if arg.contains(':') {
        scratch_only(app, "ACCENT_BENCH_UNFOLD");
    }
    if let Some(dir) = arg.strip_prefix("race:") {
        return bench_unfold_race(app, dir);
    }
    if let Some(dir) = arg.strip_prefix("renew:") {
        return bench_unfold_renew(app, dir);
    }
    if let Some(rel) = arg.strip_prefix("tab:") {
        return bench_unfold_tab(app, rel);
    }
    if let Some(dir) = arg.strip_prefix("again:") {
        return bench_unfold_again(app, dir);
    }
    if let Some(dir) = arg.strip_prefix("reload:") {
        return bench_unfold_reload(app, dir);
    }
    let (app, dirs) = (
        app.clone(),
        arg.split(',').map(str::to_string).collect::<Vec<_>>(),
    );
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        for dir in &dirs {
            unfold(tree, dir).await;
            let expanded = tree::find_row(tree.model(), dir).map(|row| row.is_expanded());
            println!(
                "bench unfold {dir} expanded={expanded:?} {:?}",
                listed(tree, dir)
            );
        }
        bench_quit(&app);
    });
}

fn bench_unfold_race(app: &Rc<App>, dir: &str) {
    let (Some(vault), app, dir) = (app.vault().cloned(), app.clone(), dir.to_string()) else {
        return bench_quit(app);
    };
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        unfold(tree, &dir).await;
        let race = format!("{dir}/race");
        let quoted = accent_api::ssh::quote(&race);
        let made = scroll::in_vault(
            &app,
            &format!("mkdir {quoted} && echo 1 > {quoted}/first.md"),
        );
        until(|| tree::find_row(tree.model(), &race).is_some()).await;
        let _ = vault.rescan();
        if let Some(row) = tree::find_row(tree.model(), &race) {
            row.set_expanded(true);
        }
        for _ in 0..300 {
            if !listed(tree, &race).is_empty() {
                break;
            }
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        let first = listed(tree, &race);
        scroll::in_vault(&app, &format!("echo 2 > {quoted}/second.md"));
        glib::timeout_future(Duration::from_secs(5)).await;
        println!(
            "bench unfold_race made={made} first={first:?} after={:?}",
            listed(tree, &race)
        );
        bench_quit(&app);
    });
}

/// `=renew:<rel_dir>`: the open folder removed and made again at once with a file in it, as a
/// build cleaning its output does, then a second file written into it a little later; the tree's
/// rows under it are printed after each.
fn bench_unfold_renew(app: &Rc<App>, dir: &str) {
    let (app, dir) = (app.clone(), dir.to_string());
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        unfold(tree, &dir).await;
        println!("bench unfold_renew before {:?}", listed(tree, &dir));
        let quoted = accent_api::ssh::quote(&dir);
        scroll::in_vault(
            &app,
            &format!("rm -rf {quoted} && mkdir {quoted} && echo 1 > {quoted}/new.md"),
        );
        glib::timeout_future(Duration::from_secs(3)).await;
        println!("bench unfold_renew made {:?}", listed(tree, &dir));
        scroll::in_vault(&app, &format!("echo 2 > {quoted}/later.md"));
        glib::timeout_future(Duration::from_secs(3)).await;
        println!("bench unfold_renew later {:?}", listed(tree, &dir));
        bench_quit(&app);
    });
}

/// `=again:<rel_dir>`: the folder opened, shut, given a file from outside accent while shut, and
/// opened again, the tree's rows under it printed then — for a folder nothing watches, a
/// dependency tree, the one thing that can bring the file in is the opening itself.
fn bench_unfold_again(app: &Rc<App>, dir: &str) {
    let (app, dir) = (app.clone(), dir.to_string());
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        unfold(tree, &dir).await;
        println!("bench unfold_again before {:?}", listed(tree, &dir));
        if let Some(row) = tree::find_row(tree.model(), &dir) {
            row.set_expanded(false);
        }
        let quoted = accent_api::ssh::quote(&format!("{dir}/again.md"));
        scroll::in_vault(&app, &format!("echo again > {quoted}"));
        glib::timeout_future(Duration::from_secs(2)).await;
        if let Some(row) = tree::find_row(tree.model(), &dir) {
            row.set_expanded(true);
        }
        glib::timeout_future(Duration::from_secs(1)).await;
        println!("bench unfold_again opened {:?}", listed(tree, &dir));
        bench_quit(&app);
    });
}

/// `=tab:<rel_file>`: the file opened in a tab, rewritten from outside accent, and what the tab
/// holds printed either side — for a file in a folder the index does not walk, which only its
/// folder's own watch reports on.
fn bench_unfold_tab(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // A remote vault opens nothing before its host has answered.
        until(|| app.reconciled.get()).await;
        app.open_path(&rel);
        let mut tab = None;
        for _ in 0..100 {
            tab = app.open_tabs().into_iter().find(|t| t.rel() == rel);
            if tab.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let Some(tab) = tab else {
            println!("bench unfold_tab no_tab");
            return bench_quit(&app);
        };
        // Its folder's watch is asked for as it opens, and in place a moment later.
        glib::timeout_future(Duration::from_secs(1)).await;
        println!("bench unfold_tab before {:?}", tab.text());
        let quoted = accent_api::ssh::quote(&rel);
        scroll::in_vault(&app, &format!("printf 'edited outside\\n' > {quoted}"));
        glib::timeout_future(Duration::from_secs(3)).await;
        println!("bench unfold_tab after {:?}", tab.text());
        bench_quit(&app);
    });
}

/// `=reload:<rel_dir>`: Reload from the open folder's menu, the moment a file is written into it
/// from outside accent — inside the 300 ms the watcher holds its news back. Prints the folder's
/// rows before and once the file is listed, or 250 ms on; then, 5 s later, whether a walk said
/// it had indexed the vault and whether the index holds the file. A folder the index holds walks,
/// a gitignored one does not.
fn bench_unfold_reload(app: &Rc<App>, dir: &str) {
    let (Some(ops), Some(vault)) = (app.ops().cloned(), app.vault().cloned()) else {
        return bench_quit(app);
    };
    let (app, dir) = (app.clone(), dir.to_string());
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        until(|| app.reconciled.get()).await;
        unfold(tree, &dir).await;
        // The first walk's own toast gone, so one seen later is the Reload's.
        until(|| indexed_said(&app).is_none()).await;
        let at = gdk::Rectangle::new(0, 0, 1, 1);
        let menu = fileops::context_menu(&ops, tree.widget(), Some((&dir, true)), &[], at);
        let offered = menu
            .menu_model()
            .is_some_and(|m| fileops::labels(&m).iter().any(|l| l == "Reload"));
        menu.popdown();
        let rel = format!("{dir}/reloaded.md");
        let quoted = accent_api::ssh::quote(&rel);
        scroll::in_vault(&app, &format!("echo reloaded > {quoted}"));
        let t = Instant::now();
        println!("bench unfold_reload before {:?}", listed(tree, &dir));
        let _ =
            WidgetExt::activate_action(tree.widget(), "fileops.reload", Some(&dir.to_variant()));
        while !listed(tree, &dir).contains(&rel) && t.elapsed() < Duration::from_millis(250) {
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        println!(
            "bench unfold_reload offered={offered} after_ms={} {:?}",
            t.elapsed().as_millis(),
            listed(tree, &dir)
        );
        glib::timeout_future(Duration::from_secs(5)).await;
        let held = vault
            .list_dir(&dir)
            .is_ok_and(|rows| rows.iter().any(|r| r.rel_path == rel && r.id != 0));
        println!(
            "bench unfold_reload walked={:?} indexed={held}",
            indexed_said(&app)
        );
        scroll::in_vault(&app, &format!("rm -f {quoted}"));
        bench_quit(&app);
    });
}

/// The "Indexed …" toast a finished walk leaves over the window, if one is up.
fn indexed_said(app: &Rc<App>) -> Option<String> {
    let label = find_widget(app.window.upcast_ref(), &|w| {
        w.downcast_ref::<gtk::Label>()
            .is_some_and(|l| l.label().starts_with("Indexed "))
    })?;
    Some(label.downcast::<gtk::Label>().ok()?.label().to_string())
}

/// Open `dir`'s row and every row above it, as clicks would, and give its listing a second.
async fn unfold(tree: &tree::Tree, dir: &str) {
    for _ in 0..50 {
        if tree.reveal(dir) {
            break;
        }
        glib::timeout_future(Duration::from_millis(200)).await;
    }
    if let Some(row) = tree::find_row(tree.model(), dir) {
        row.set_expanded(true);
    }
    glib::timeout_future(Duration::from_millis(1000)).await;
}

/// The rows the tree lists directly under `dir`.
fn listed(tree: &tree::Tree, dir: &str) -> Vec<String> {
    let model = tree.model();
    let prefix = format!("{dir}/");
    (0..model.n_items())
        .filter_map(|i| model.item(i).and_downcast::<gtk::TreeListRow>()?.item())
        .filter_map(|item| tree::decode(&item))
        .filter(|row| {
            row.rel
                .strip_prefix(&prefix)
                .is_some_and(|r| !r.contains('/'))
        })
        .map(|row| row.rel)
        .collect()
}

/// `ACCENT_BENCH_MOVE=<rel>,<rel>,…`: Move to… on those paths as a marked set, past its dialog.
///
/// Prints what the dialog opens on — its heading, the folder in its entry and the line under it —
/// then types `Moved/Here`, a folder that is not there yet, and prints the line again, presses
/// Move, answers an Update Links? question if one comes and prints the toast and where each path
/// is afterwards. Then the same set into the folder it is now in, and the folder `Moved` into its
/// own `Here`, which are the two toasts that refuse. It moves files, so point it at a scratch copy.
pub(super) fn bench_move(app: &Rc<App>, arg: &str) {
    scratch_only(app, "ACCENT_BENCH_MOVE");
    let (Some(ops), Some(vault)) = (app.ops().cloned(), app.vault().cloned()) else {
        return bench_quit(app);
    };
    let rels: Vec<String> = arg.split(',').map(str::to_string).collect();
    let app = app.clone();
    glib::spawn_future_local(async move {
        // A move asks the index what links to it, which the first reconcile has to have filled.
        until(|| app.reconciled.get()).await;
        let set = |rels: &[String]| -> Vec<(String, bool)> {
            rels.iter()
                .map(|rel| {
                    let folder = matches!(fileops::taken(&vault, rel), fileops::Taken::Folder);
                    (rel.clone(), folder)
                })
                .collect()
        };
        fileops::move_to(&ops, set(&rels));
        let moved: Vec<String> = rels
            .iter()
            .map(|rel| format!("Moved/Here/{}", accent_core::path::basename(rel)))
            .collect();
        println!(
            "bench move_said {:?}",
            move_through(&app, "Moved/Here").await
        );
        for (from, to) in rels.iter().zip(&moved) {
            println!(
                "bench move_landed {from} gone={} there={}",
                !vault.exists(from),
                vault.exists(to)
            );
        }
        fileops::move_to(&ops, set(&moved));
        println!(
            "bench move_again {:?}",
            move_through(&app, "Moved/Here").await
        );
        fileops::move_to(&ops, vec![("Moved".to_string(), true)]);
        println!(
            "bench move_itself {:?}",
            move_through(&app, "Moved/Here").await
        );
        bench_quit(&app);
    });
}

/// Answer the Move to… dialog that is up with `dir`, and any Update Links? question after it, and
/// return the toast that says what came of it. Prints what the dialog showed on the way.
async fn move_through(app: &Rc<App>, dir: &str) -> Option<String> {
    glib::timeout_future(Duration::from_millis(500)).await;
    let dialog = app
        .window
        .visible_dialog()
        .and_downcast::<adw::AlertDialog>()?;
    let form = dialog.extra_child()?;
    let entry = find_widget(&form, &|w| w.is::<gtk::Entry>()).and_downcast::<gtk::Entry>()?;
    let line = || {
        find_widget(&form, &|w| w.has_css_class("dim-label"))
            .and_downcast::<gtk::Label>()
            .map(|l| l.label().to_string())
    };
    println!(
        "bench move_dialog heading={:?} entry={:?} line={:?}",
        dialog.heading().unwrap_or_default(),
        entry.text(),
        line()
    );
    entry.set_text(dir);
    println!("bench move_typed line={:?}", line());
    // What was said before would otherwise stay up in front of the answer, queued behind it.
    app.toasts.dismiss_all();
    let said = app.toasted.get();
    dialog.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
    dialog.close();
    glib::timeout_future(Duration::from_millis(1500)).await;
    if let Some(update) = app
        .window
        .visible_dialog()
        .and_downcast::<adw::AlertDialog>()
    {
        println!(
            "bench move_update {:?}",
            update.heading().unwrap_or_default()
        );
        update.emit_by_name::<()>("response", &[&"update"]);
        update.close();
    }
    until(|| app.toasted.get() > said).await;
    compare::bench_toast(app)
}
