//! Drills over files: the Files pane, New from Template, the path fields and closing a window.

use super::*;

/// What New from Template would list: every template, and the destination each one names today.
/// `=open` then makes each one's note as Create does, and prints whether its caret is on screen.
///
/// The dialog itself cannot be driven under Xvfb, so this is what proves `templates()`, the
/// `accent-target:` directive and the rendered target end to end without a widget.
pub(super) fn bench_templates(app: &Rc<App>) {
    let Some(vault) = app.vault() else {
        return bench_quit(app);
    };
    let templates = vault.templates().unwrap_or_default();
    println!("bench templates {}", templates.len());
    for rel in &templates {
        if let Ok(Some(target)) = vault.template_target(rel) {
            println!("bench template_target {rel} {target}");
        }
    }
    if std::env::var("ACCENT_BENCH_TEMPLATE").as_deref() != Ok("open") {
        return bench_quit(app);
    }
    let (app, vault) = (app.clone(), vault.clone());
    glib::spawn_future_local(async move {
        for template in templates {
            let Ok(Some((rel, stops))) = vault.note_from_template(&template) else {
                continue;
            };
            app.with_tab(&rel, Opened::Kept, "open", move |_, tab| {
                tab.place_stops(&stops)
            });
            glib::timeout_future(Duration::from_secs(1)).await;
            let Some(tab) = app.tab_for(&rel) else {
                continue;
            };
            let caret = tab
                .view
                .iter_location(&tab.buffer.iter_at_mark(&tab.buffer.get_insert()));
            let seen = tab.view.visible_rect();
            let on =
                seen.y() <= caret.y() && caret.y() + caret.height() <= seen.y() + seen.height();
            println!("bench template_caret {rel} on_screen={on}");
        }
        bench_quit(&app);
    });
}

/// Cancel two dialogs and say whether either outlived its close, then quit with a note open, the
/// way Ctrl+Q does, and count the vault's references once the window is gone. The application is
/// held so the process outlives its last window, and the main loop gets up to three seconds to let
/// go of whatever was still in flight. Every reference left is a closed window keeping its vault
/// open — on a remote one, its `serve` session and its forwards.
pub(super) fn bench_close(app: &Rc<App>) {
    let (Some(vault), Some(gtk_app)) = (app.vault(), app.window.application()) else {
        return bench_quit(app);
    };
    // A tab open, so that a tab holding the window's state is caught as well as the sidebar.
    let note = vault
        .list_dir("")
        .unwrap_or_default()
        .into_iter()
        .find(|row| row.kind == accent_core::walk::FileKind::Markdown);
    if let Some(note) = note {
        app.open_path(&note.rel_path);
    }
    // Nothing below holds the `App`: the close has to be what lets it go.
    let (app, vault) = (Rc::downgrade(app), Arc::downgrade(vault));
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        let dialogs = match app.upgrade() {
            Some(app) => bench_dialogs(&app).await,
            None => Vec::new(),
        };
        // Past the close animation, so a dialog still alive is one its close did not let go of.
        glib::timeout_future(Duration::from_millis(600)).await;
        let mut kept = 0;
        for (heading, dialog) in dialogs {
            let alive = dialog.upgrade().is_some();
            println!(
                "bench dialog_kept_after_cancel {heading:?} {}",
                u8::from(alive)
            );
            kept += usize::from(alive);
        }
        println!("bench vault_refs_open {}", vault.strong_count());
        let _hold = gtk_app.hold();
        gtk_app.activate_action("quit", None);
        for _ in 0..30 {
            if vault.strong_count() == 0 {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let held = vault.strong_count();
        println!("bench vault_refs_after_close {held}");
        std::process::exit(i32::from(held > 0 || kept > 0));
    });
}

/// Cancel a New File dialog and, over the open note, an Unsaved Changes one: a dialog that
/// outlives its close keeps whatever its handlers hold, and New File's path field completes from
/// the vault.
async fn bench_dialogs(app: &Rc<App>) -> Vec<(glib::GString, glib::WeakRef<adw::AlertDialog>)> {
    let _ = WidgetExt::activate_action(&app.window, "win.new-file", None);
    // The dialog is built once a worker has the templates.
    for _ in 0..40 {
        if app.window.visible_dialog().is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    let mut dialogs = vec![bench_cancel(app)];
    if let Some(tab) = app.open_tabs().into_iter().next() {
        app.ask_unsaved(&tab, &SaveError::Offline, |_, _| {});
        dialogs.push(bench_cancel(app));
    }
    dialogs.into_iter().flatten().collect()
}

/// Close the window's topmost dialog the way Escape does, printing the response that answers,
/// and keep its heading and a weak reference to it.
fn bench_cancel(app: &App) -> Option<(glib::GString, glib::WeakRef<adw::AlertDialog>)> {
    let dialog = app
        .window
        .visible_dialog()?
        .downcast::<adw::AlertDialog>()
        .ok()?;
    let heading = dialog.heading()?;
    dialog.connect_response(None, {
        let heading = heading.clone();
        move |_, response| println!("bench dialog_response {heading:?} {response}")
    });
    dialog.close();
    Some((heading, dialog.downgrade()))
}

/// Print the tree's rows as drawn, then fire Show Hidden Files twice, the way its menu item and
/// the palette do, and print them after each. The headless image has no pointer, so this is how
/// "dot-named rows are dimmed, `.git` is never there, and the toggle takes them away and brings
/// them back" is seen rather than claimed.
pub(super) fn bench_hidden(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        for step in 0..3 {
            if step > 0 {
                let _ = WidgetExt::activate_action(&app.window, "win.show-hidden-files", None);
                // The listing is asked for again and lands from a worker thread.
                glib::timeout_future(Duration::from_millis(500)).await;
            }
            let state = app
                .window
                .lookup_action("show-hidden-files")
                .and_then(|a| a.state())
                .and_then(|s| s.get::<bool>());
            println!("bench show_hidden {state:?}");
            if let Some(tree) = app.tree.get() {
                for (rel, dim) in drawn_rows(tree.view()) {
                    println!("bench tree_row {rel} dim={}", u8::from(dim));
                }
            }
        }
        bench_quit(&app);
    });
}

/// Copy a file and paste it beside itself, then cut the copy and paste it in the vault root.
///
/// What no unit test reaches: the real GDK clipboard a local vault writes and reads back, the
/// `(copy)` mark a paste beside its source takes, the rows a Cut dims, and that the paste of a Cut
/// moves the file rather than copying it again. Every step is a clipboard read plus a worker — a
/// `stat` per candidate name and then the copy — which is what the waits are for.
///
/// The files are asked after with `exists`, a plain `stat`: `list_dir` reads the index, and a copy
/// reaches that only once the watcher and the worker have caught up. It waits up to a minute for
/// the first reconcile, a move being refused until then, so it wants a small vault rather than the
/// generated one — `clip_reconciled 0` is the drill saying it never got that far.
pub(super) fn bench_clip(app: &Rc<App>, rel: &str) {
    let (Some(ops), Some(vault)) = (app.ops().cloned(), app.vault().cloned()) else {
        return bench_quit(app);
    };
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // A move goes through `plan_rename`, which reads the backlinks out of the index and is
        // refused while the first reconcile is still running — on the generated vault that is
        // half a minute of walking.
        for _ in 0..300 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        println!("bench clip_reconciled {}", u8::from(app.reconciled.get()));
        let dir = accent_core::path::parent_dir(&rel).to_string();
        // The name a paste beside its source has to land under, spelled out here rather than
        // asked of `free_path`, so the drill checks the rule instead of repeating it.
        let copied = match rel.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem} (copy).{ext}"),
            None => format!("{rel} (copy)"),
        };
        fileops::clipboard::copy(&ops, &rel, false);
        fileops::clipboard::paste(&ops, &dir);
        glib::timeout_future(Duration::from_secs(2)).await;
        println!(
            "bench clip_copied {copied} there={} source_kept={}",
            u8::from(vault.exists(&copied)),
            u8::from(vault.exists(&rel))
        );

        // The row has to be on screen before anything can be said about how it is drawn, and a
        // file this new is inside a folder the reader never opened.
        let tree = app.tree.get().expect("a tree");
        tree.reveal(&copied);
        glib::timeout_future(Duration::from_millis(500)).await;
        fileops::clipboard::cut(&ops, &copied, false);
        // The dim is a re-bind of the rows already on screen, which happens on the spot.
        glib::timeout_future(Duration::from_millis(200)).await;
        let dim = drawn_rows(tree.view())
            .into_iter()
            .find(|(row, _)| *row == copied)
            .map(|(_, dim)| dim);
        println!("bench clip_dim {copied} dim={dim:?}");

        let moved = accent_core::path::basename(&copied).to_string();
        fileops::clipboard::paste(&ops, "");
        glib::timeout_future(Duration::from_secs(2)).await;
        println!(
            "bench clip_moved to={moved} there={} source_gone={}",
            u8::from(vault.exists(&moved)),
            u8::from(!vault.exists(&copied))
        );
        // Two files on the clipboard at once, which is what a Ctrl+click set puts there: the
        // real GDK clipboard carries a list in both of its formats, and the paste reads them
        // back and lands both beside each other in the vault root.
        let second = vault
            .list_dir(&dir)
            .unwrap_or_default()
            .into_iter()
            .find(|row| row.kind != accent_core::walk::FileKind::Dir && row.rel_path != rel)
            .map(|row| row.rel_path);
        if let Some(second) = second {
            let both = [(rel.clone(), false), (second, false)];
            fileops::clipboard::copy_all(&ops, &both);
            fileops::clipboard::paste(&ops, "");
            glib::timeout_future(Duration::from_secs(2)).await;
            let landed: Vec<(String, bool)> = both
                .iter()
                .map(|(rel, _)| accent_core::path::basename(rel).to_string())
                .map(|name| (name.clone(), vault.exists(&name)))
                .collect();
            println!("bench clip_copied_many {landed:?}");
            for (name, _) in &landed {
                let _ = vault.delete(name);
            }
        }

        // The drill writes into the vault, so it takes its own leavings back out again.
        let _ = vault.delete(&moved);
        let _ = vault.delete(&copied);
        bench_quit(&app);
    });
}

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
        println!("bench menu_row {:?}", tree.selected().map(|row| row.rel));

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
        bench_quit(&app);
    });
}

/// Reveal a row, print where it is on screen and stay up, for an XTEST Ctrl+click held against
/// it: the modifier is the one half no drill can fake, the mark being made in a gesture that
/// reads the press's own state. Prints the marked rows and how many documents are open twice —
/// before the press and after it — so one run says both what the Ctrl+click marked and that it
/// opened nothing, and then that a plain click let the marks go again.
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
        let at = tree::expanders(tree.view())
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
            });
        match at {
            Some(at) => println!("bench menu_press {} {}", at.x() as i32, at.y() as i32),
            None => println!("bench menu_press none"),
        }
        for step in 0..3 {
            glib::timeout_future(Duration::from_secs(5)).await;
            println!(
                "bench menu_marks {step} {:?} docs={}",
                marked_rows(tree.view()),
                app.docs().len()
            );
        }
        bench_quit(&app);
    });
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
fn drawn_rows(view: &gtk::ListView) -> Vec<(String, bool)> {
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

/// What a path entry's completion does with the keyboard, and whether the entry takes the width
/// the list gives the dialog.
///
/// Layout is real under Xvfb — an allocation wants a mapped window, not a window manager — so the
/// width either side of the list appearing is measured rather than argued. The keys are emitted on
/// the entry's own controller, which proves the handler, the selection and the text it applies but
/// **not** the propagation phase: emitting a signal skips phase dispatch altogether, so that Return
/// beats `GtkText`'s own binding is still a claim only a real session can settle.
pub(super) fn bench_paths(app: &Rc<App>) {
    let entry = gtk::Entry::new();
    let field = crate::pathfield::path_field(&entry, "bench", |_| {
        ["Archive/", "Attachments/", "Notes/"]
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    });
    let window = gtk::Window::builder()
        .default_width(600)
        .child(&field)
        .build();
    window.present();
    // Fills the list. The toplevel never goes active under Xvfb, so `show_completions` leaves it
    // put away and the first Down is what opens it.
    entry.set_text("A");
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        print_width("shut", &entry);
        // Nothing has been aimed at, so Return belongs to the dialog and the text stays as typed.
        press_key(&entry, gdk::Key::Return);
        println!("bench path_applied none {:?}", entry.text());
        press_key(&entry, gdk::Key::Down);
        println!("bench path_selected {:?}", selected_offer(&field));
        // A second frame, because the row the list just grew is what widens the dialog.
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            print_width("open", &entry);
            press_key(&entry, gdk::Key::Down);
            println!("bench path_selected {:?}", selected_offer(&field));
            press_key(&entry, gdk::Key::Return);
            println!("bench path_applied selected {:?}", entry.text());
            // The pointer's way in, which applies from an idle rather than on the spot.
            if let Some(list) = completion_list(&field)
                && let Some(row) = list.row_at_index(2)
            {
                list.emit_by_name::<()>("row-activated", &[&row]);
            }
            glib::idle_add_local_once(move || {
                println!("bench path_activated {:?}", entry.text());
                window.close();
                bench_quit(&app);
            });
        });
    });
}

/// The entry's allocated width against the `.linked` row it sits in, whose surplus is the folder
/// button. `state` says whether the completion list was showing.
fn print_width(state: &str, entry: &gtk::Entry) {
    let row = entry.parent().map_or(0, |row| row.width());
    println!("bench path_entry_width {state} {} {row}", entry.width());
}

/// A path field's completion list: the revealer's, and the scroller hands back the viewport it
/// wrapped a `GtkListBox` in rather than the list itself.
fn completion_list(field: &gtk::Widget) -> Option<gtk::ListBox> {
    field
        .last_child()
        .and_downcast::<gtk::Revealer>()
        .and_then(|revealer| revealer.child())
        .and_downcast::<gtk::ScrolledWindow>()
        .and_then(|scroller| scroller.child())
        .and_then(|viewport| viewport.first_child())
        .and_downcast::<gtk::ListBox>()
}

/// The completion that list has highlighted, by the text it stands for.
fn selected_offer(field: &gtk::Widget) -> Option<String> {
    completion_list(field)
        .and_then(|list| list.selected_row())
        .and_then(|row| row.child().and_downcast::<gtk::Label>())
        .map(|label| label.label().into())
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
