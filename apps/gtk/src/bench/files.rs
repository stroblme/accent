//! Drills over files: the Files pane, New from Template, the path fields and closing a window.

use super::*;

/// What New from Template would list: every template, and the destination each one names today.
/// `=open` then makes each one's note as Create does, and prints whether its caret is on screen;
/// `=insert:<rel_note>` puts them into a note instead ([`bench_template_insert`]), and
/// `=walk:<rel_note>` stays up for its stops to be typed into ([`bench_template_walk`]).
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
    let arg = std::env::var("ACCENT_BENCH_TEMPLATE").unwrap_or_default();
    if let Some(rel) = arg.strip_prefix("insert:") {
        return bench_template_insert(app, rel);
    }
    if let Some(rel) = arg.strip_prefix("walk:") {
        return bench_template_walk(app, rel);
    }
    if arg != "open" {
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

/// `=insert:<rel_note>` puts each template that has a stop at the caret of that note, as Insert
/// Template does: at its end, half way down, at its end with every block folded, and at its end
/// followed by a Backspace, which ends the snippet through GtkSourceView's own scroll. Prints the
/// view's vertical adjustment and top line before, after, and after a scroll back to the top; the
/// folded case has to come out with the template on screen. Before them, a Go to Line and its
/// preview into the note with every block folded, which have to come out on their line, a Find
/// Next into a shut block and a Replace All over the folded note, and text typed next to a shut
/// block, which has to stay in sight through the analysis.
fn bench_template_insert(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(800)).await;
        let (Some(vault), Some(tab)) = (app.vault(), app.tab_for(&rel)) else {
            return bench_quit(&app);
        };
        let adj = tab.view.vadjustment().expect("bench adjustment");
        let original = tab.text();
        let say = |template: &str, case: &str, when: &str| {
            let seen = tab.view.visible_rect();
            let (top, _) = tab.view.line_at_y(seen.y());
            let caret = tab.view.iter_location(&editor::caret(&tab.buffer));
            let on =
                seen.y() <= caret.y() && caret.y() + caret.height() <= seen.y() + seen.height();
            println!(
                "bench template_insert {template} {case} {when} value={:.0} upper={:.0} page={:.0} top_line={} caret_line={} caret_on_screen={on}",
                adj.value(),
                adj.upper(),
                adj.page_size(),
                top.line(),
                tab.cursor_line(),
            );
        };
        // First the jump an insertion's reveal copies: Go to Line into a note with every block
        // folded, which has to land on the line once the fold it opens is measured.
        tab.fold_all();
        let line = tab.buffer.line_count() * 9 / 10;
        tab.goto_line(line, 1);
        measured().await;
        glib::timeout_future(Duration::from_millis(600)).await;
        say("goto", &format!("line_{line}"), "folded");
        tab.unfold_all();
        // The go-to entry's preview of the same line, from the top of the note.
        tab.fold_all();
        adj.set_value(0.0);
        tab.show_line(line);
        measured().await;
        glib::timeout_future(Duration::from_millis(600)).await;
        say("show_line", &format!("line_{line}"), "folded");
        tab.unfold_all();
        let hides = tab
            .buffer
            .tag_table()
            .lookup(crate::fold::TAG)
            .expect("fold tag");
        let runs = || {
            let (mut at, mut n) = (tab.buffer.start_iter(), 0);
            while at.forward_to_tag_toggle(Some(&hides)) {
                n += usize::from(at.starts_tag(Some(&hides)));
            }
            n
        };
        // Find Next with every block folded, to the first hidden line with text on it from the
        // same line down: it has to open that block and land on the match. Then Replace All over
        // the note folded again, which has to rewrite the hidden occurrence and leave its block
        // shut. A line a hidden run begins with (`run_start`) is the case GTK gets wrong, since
        // text inserted there lands outside the run; and the replacement holds the query, so
        // Replace All has to stop where it wraps rather than go round again.
        tab.fold_all();
        let text_of = |at: &gtk::TextIter| {
            let mut end = *at;
            if !end.ends_line() {
                end.forward_to_line_end();
            }
            tab.buffer.text(at, &end, true).trim().to_string()
        };
        let mut at = tab.buffer.iter_at_line(line - 1).expect("bench line");
        while (!at.has_tag(&hides) || text_of(&at).is_empty()) && at.forward_line() {}
        let (query, want) = (text_of(&at), at.line() + 1);
        let (was_hidden, run_start) = (at.has_tag(&hides), at.starts_tag(Some(&hides)));
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        tab.set_query(&query);
        tab.step(true, false);
        measured().await;
        glib::timeout_future(Duration::from_millis(600)).await;
        say("find", &format!("line_{want}"), "folded");
        println!(
            "bench template_insert find folded was_hidden={was_hidden} run_start={run_start} hidden={} label={:?}",
            editor::caret(&tab.buffer).has_tag(&hides),
            tab.matches_label()
        );
        tab.set_text(&original);
        glib::timeout_future(Duration::from_millis(1500)).await;
        tab.fold_all();
        let (shut, with) = (runs(), format!("{query} (replaced)"));
        tab.set_query(&query);
        tab.replace_all(&with);
        let text = tab.text();
        let hidden = tab
            .buffer
            .start_iter()
            .forward_search(&with, gtk::TextSearchFlags::empty(), None)
            .is_some_and(|(start, _)| start.has_tag(&hides));
        println!(
            "bench template_insert replace_all folded replaced={} of={} hidden={hidden} shut={shut} after={}",
            text.matches(&with).count(),
            original
                .to_lowercase()
                .matches(&query.to_lowercase())
                .count(),
            runs()
        );
        // Text typed next to a folded block, left through the analysis that folded it away: at
        // the note's end, which the last block's fold reaches, where it has to stay in sight with
        // every fold still shut; and on a new line under the first folded header, which opens
        // that one fold instead.
        for (case, typed) in [
            ("end", "23 characters typed now"),
            ("header", "\n23 characters"),
        ] {
            tab.set_text(&original);
            glib::timeout_future(Duration::from_millis(500)).await;
            tab.fold_all();
            let shut = runs();
            let mut at = tab.buffer.end_iter();
            if case == "header" {
                at = tab.buffer.start_iter();
                at.forward_to_tag_toggle(Some(&hides));
                at.backward_char();
            }
            let from = at.offset();
            tab.buffer.place_cursor(&at);
            tab.buffer.insert_interactive_at_cursor(typed, true);
            measured().await;
            glib::timeout_future(Duration::from_millis(1500)).await;
            let hidden = (from..from + typed.chars().count() as i32)
                .filter(|at| tab.buffer.iter_at_offset(*at).has_tag(&hides))
                .count();
            println!(
                "bench template_insert typed {case} hidden={hidden} of={} shut={shut} after={}",
                typed.chars().count(),
                runs()
            );
        }
        for template in vault.templates().unwrap_or_default() {
            let Ok((text, stops)) = vault.render_template(&template, "Bench") else {
                continue;
            };
            if stops.is_empty() {
                continue;
            }
            for case in ["end", "middle", "folded", "backspace"] {
                tab.set_text(&original);
                tab.unfold_all();
                // The analysis debounce is what knows the note's folds: a whole refresh, since an
                // answer landing after the next edit is dropped, and a debug build is slow at it.
                glib::timeout_future(Duration::from_millis(1500)).await;
                if case == "folded" {
                    tab.fold_all();
                }
                let at = match case {
                    "middle" => tab.buffer.iter_at_offset(tab.buffer.char_count() / 2),
                    _ => tab.buffer.end_iter(),
                };
                tab.buffer.place_cursor(&at);
                tab.scroll_to_caret(0.5, false);
                glib::timeout_future(Duration::from_millis(500)).await;
                say(&template, case, "before");
                tab.insert_stops(&text, &stops);
                if case == "backspace" {
                    let mut at = editor::caret(&tab.buffer);
                    tab.buffer.backspace(&mut at, true, true);
                }
                measured().await;
                glib::timeout_future(Duration::from_millis(600)).await;
                say(&template, case, "after");
                adj.set_value(0.0);
                glib::timeout_future(Duration::from_millis(600)).await;
                say(&template, case, "top");
            }
        }
        bench_quit(&app);
    });
}

/// Until GTK has measured every line of the views: its measuring idle outranks a default idle, so
/// one of those running is the sign. A fold the insertion opened can take seconds in a debug build.
async fn measured() {
    let done = Rc::new(Cell::new(false));
    let flag = done.clone();
    glib::idle_add_local_once(move || flag.set(true));
    while !done.get() {
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}

/// `=walk:<rel_note>` puts the first template with two stops or more at the end of that note,
/// gives the view the keyboard and stays up for XTEST to type into its stops, which only a real
/// key press can walk: `build-aux/xtest.py :<display> "move 700 450; focus; type abc; key Tab;
/// type def; key Tab; type ghi; key Tab"`. Prints how many folders GtkSourceView looks for snippet
/// files in, then what the template became and whether its stops are still being walked, eight
/// seconds after `template_walk ready`.
fn bench_template_walk(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(800)).await;
        let (Some(vault), Some(tab)) = (app.vault(), app.tab_for(&rel)) else {
            return bench_quit(&app);
        };
        let walkable = vault
            .templates()
            .unwrap_or_default()
            .into_iter()
            .find_map(|t| {
                vault
                    .render_template(&t, "Bench")
                    .ok()
                    .filter(|(_, stops)| stops.len() > 1)
            });
        let Some((text, stops)) = walkable else {
            println!("bench template_walk no template with two stops");
            return bench_quit(&app);
        };
        let from = tab.buffer.char_count();
        tab.buffer.place_cursor(&tab.buffer.end_iter());
        tab.view.grab_focus();
        tab.insert_stops(&text, &stops);
        // Nowhere for GtkSourceView to find snippets of its own, so only the template's react.
        let dirs = sourceview5::SnippetManager::default().search_path().len();
        println!("bench template_walk ready snippet_dirs={dirs}");
        glib::timeout_future(Duration::from_secs(8)).await;
        let (start, end) = (tab.buffer.iter_at_offset(from), tab.buffer.end_iter());
        println!(
            "bench template_walk {:?} walking={}",
            tab.buffer.text(&start, &end, true),
            tab.snippet_active()
        );
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
        // A move goes through `plan_moves`, which reads the backlinks out of the index and is
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

        bench_cut_many(&app, &ops, &vault, &dir).await;
        // The drill writes into the vault, so it takes its own leavings back out again.
        let _ = vault.delete(&moved);
        let _ = vault.delete(&copied);
        bench_quit(&app);
    });
}

/// A Cut of two notes pasted into `dir`: one plan for both, so one Update Links? question, and
/// every note rewritten once — the one linking both from the root, and the moved note whose own
/// relative link now has a folder to climb out of. Prints each dialog as it comes, the number of
/// them, and the three texts afterwards.
async fn bench_cut_many(app: &Rc<App>, ops: &Rc<fileops::Ops>, vault: &Arc<Vault>, dir: &str) {
    if dir.is_empty() {
        return println!("bench clip_cut_many none");
    }
    let notes = [
        ("clip-a.md", "[r](clip-ref.md) [b](clip-b.md)\n"),
        ("clip-b.md", "[a](clip-a.md)\n"),
        ("clip-ref.md", "[a](clip-a.md) [b](clip-b.md) [[clip-a]]\n"),
    ];
    for (rel, text) in notes {
        let _ = vault.save(rel, text, None);
    }
    // Saved through the vault, so the index has them within a worker batch.
    for _ in 0..50 {
        if vault.backlinks("clip-b.md").is_ok_and(|b| b.len() >= 2) {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    let both = [
        ("clip-a.md".to_string(), false),
        ("clip-b.md".to_string(), false),
    ];
    fileops::clipboard::cut_all(ops, &both);
    fileops::clipboard::paste(ops, dir);
    // Counted by identity: a dialog closing is still the visible one for its animation.
    let mut seen: Vec<adw::AlertDialog> = Vec::new();
    for _ in 0..40 {
        glib::timeout_future(Duration::from_millis(100)).await;
        let Some(dialog) = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
            .filter(|dialog| !seen.contains(dialog))
        else {
            continue;
        };
        seen.push(dialog.clone());
        println!(
            "bench clip_cut_many_dialog {:?} {:?}",
            dialog.heading().unwrap_or_default(),
            dialog.body()
        );
        dialog.emit_by_name::<()>("response", &[&"update"]);
        dialog.close();
    }
    println!("bench clip_cut_many dialogs={}", seen.len());
    let moved = |rel: &str| format!("{dir}/{rel}");
    for rel in [
        moved("clip-a.md"),
        moved("clip-b.md"),
        "clip-ref.md".to_string(),
    ] {
        let text = vault.read(&rel).map(|(text, _)| text);
        println!("bench clip_cut_many_text {rel} {:?}", text.ok());
        let _ = vault.delete(&rel);
    }
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
