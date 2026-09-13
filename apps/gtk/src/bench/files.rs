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

/// Every row the list has a widget bound to, as its path and whether its label is dimmed.
fn drawn_rows(view: &gtk::ListView) -> Vec<(String, bool)> {
    let mut rows = Vec::new();
    let mut todo = vec![view.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = todo.pop() {
        if let Some(expander) = widget.downcast_ref::<gtk::TreeExpander>() {
            let row = expander.list_row().and_then(|row| row.item());
            let dim = expander
                .child()
                .and_then(|row| row.last_child())
                .is_some_and(|label| label.has_css_class("dim-label"));
            if let Some(row) = row.as_ref().and_then(tree::decode) {
                rows.push((row.rel, dim));
            }
            continue;
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
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
