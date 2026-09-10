//! The `ACCENT_BENCH_*` drills: headless runs under Xvfb that time or probe one interaction,
//! print what they saw to stdout and quit. `install_bench_hooks` says which variable starts which.

use super::*;

/// `ACCENT_BENCH_EXPAND=<rel_path>` and `ACCENT_BENCH_SWITCHER=<query>` time the two interactions
/// that used to stall the main loop, print the numbers to stdout and quit. Both run headless under
/// Xvfb, so "expanding a big directory is still fast" stays a command anyone can re-run rather
/// than a claim in a commit message. `RUST_LOG=accent=debug` adds the per-query breakdown.
/// `ACCENT_BENCH_GIT=1` is the same idea for the Git pane, and prints row counts rather than
/// times, plus the branch readout and how many history rows a background fetch marked as not
/// pulled yet.
/// `ACCENT_BENCH_KEYS=1` likewise for the editor's key semantics, and prints text and caret
/// positions. `ACCENT_BENCH_CHROME=1` fires actions at a faded window and prints whether the
/// chrome stayed away; `=<relA>,<relB>` then opens the two notes side by side, prints what each
/// focus level fades, and holds the line fade on screen and times it. `ACCENT_BENCH_PATHS=1`
/// drives a path entry's completion, and prints widths and the text its keys apply.
/// `ACCENT_BENCH_STYLE=<rel_path>` types a heading into a note at two sizes and prints whether it
/// was styled on the keystroke or on the debounce.
/// `ACCENT_BENCH_PANES=<relA>,<relB>` moves a tab between panes and prints where it landed.
/// `ACCENT_BENCH_COMPARE=<rel_path>` compares a note with its disk copy inside its tab and prints
/// what the panes hold and whether their rows line up.
/// `ACCENT_BENCH_SHELL_KEYS=1` focuses a shell in a window that does not have the keyboard and
/// prints what `Ctrl+S` activates.
/// `ACCENT_BENCH_PDF=<rel_path>` opens a PDF, fits it to the page from a mid-page scroll position
/// and prints the layout either side of it. Point it at a document of several pages: a one-page
/// PDF is wholly on screen whatever the scroll offset was.
/// `ACCENT_BENCH_TABS=<rel_note>,<rel_pdf>` walks a note, a shell and a PDF through one pane and
/// closes the lot, printing what the find bar and the Outline pane say at each step: what a tab
/// switch and the last tab's close leave behind. The note opens as a preview and is kept by its
/// eye first, and its title and indicator are printed either side of that.
/// `ACCENT_BENCH_FOLLOW=<rel_note>` puts the pointer on a wikilink and on a plain word with Ctrl
/// held, and prints what the Ctrl+hover underline covers.
///
/// `ACCENT_BENCH_OCCUR=<rel_note>` selects things in a note and prints what the muted occurrence
/// highlight made of each selection, plus the two match colours and the priorities of the tags
/// they are painted with.
///
/// `ACCENT_BENCH_CLOSE=1` opens a note, quits, and prints how many references to the vault the
/// closed window left behind; it exits 1 unless that is 0.
pub fn install_bench_hooks(app: &Rc<App>) {
    let expand = std::env::var("ACCENT_BENCH_EXPAND").ok();
    let switcher = std::env::var("ACCENT_BENCH_SWITCHER").ok();
    let style = std::env::var("ACCENT_BENCH_STYLE").ok();
    let git = std::env::var("ACCENT_BENCH_GIT").is_ok();
    let keys = std::env::var("ACCENT_BENCH_KEYS").is_ok();
    let chrome = std::env::var("ACCENT_BENCH_CHROME").ok();
    let templates = std::env::var("ACCENT_BENCH_TEMPLATE").is_ok();
    let paths = std::env::var("ACCENT_BENCH_PATHS").is_ok();
    let panes = std::env::var("ACCENT_BENCH_PANES").ok();
    let shell_keys = std::env::var("ACCENT_BENCH_SHELL_KEYS").is_ok();
    let compare = std::env::var("ACCENT_BENCH_COMPARE").ok();
    let pdf = std::env::var("ACCENT_BENCH_PDF").ok();
    let tabs = std::env::var("ACCENT_BENCH_TABS").ok();
    let occur = std::env::var("ACCENT_BENCH_OCCUR").ok();
    let follow = std::env::var("ACCENT_BENCH_FOLLOW").ok();
    let close = std::env::var("ACCENT_BENCH_CLOSE").is_ok();
    if expand.is_none()
        && switcher.is_none()
        && style.is_none()
        && panes.is_none()
        && compare.is_none()
        && pdf.is_none()
        && tabs.is_none()
        && occur.is_none()
        && follow.is_none()
        && !git
        && !keys
        && chrome.is_none()
        && !templates
        && !paths
        && !shell_keys
        && !close
    {
        return;
    }
    let app = app.clone();
    // After the first frame, so widget realisation is not counted in the numbers.
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        if let Some(rels) = panes {
            return bench_panes(&app, &rels);
        }
        if let Some(rel) = compare {
            return bench_compare(&app, &rel);
        }
        if let Some(rel) = pdf {
            return bench_pdf(&app, &rel);
        }
        if let Some(rels) = tabs {
            return bench_tabs(&app, &rels);
        }
        if let Some(rel) = occur {
            return bench_occurrences(&app, &rel);
        }
        if let Some(rel) = follow {
            return bench_follow(&app, &rel);
        }
        if shell_keys {
            return bench_shell_keys(&app);
        }
        if close {
            return bench_close(&app);
        }
        if paths {
            return bench_paths(&app);
        }
        if templates {
            return bench_templates(&app);
        }
        if let Some(notes) = chrome {
            return bench_chrome(&app, &notes);
        }
        if keys {
            return bench_keys(&app);
        }
        if git {
            return bench_git(&app);
        }
        if let Some(rel) = style {
            return bench_style(&app, &rel);
        }
        if let Some(rel) = expand {
            bench_expand(&app, &rel);
        }
        let Some(query) = switcher else {
            bench_quit(&app);
            return;
        };
        let t0 = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        println!("bench switcher_open_ms {:.1}", ms_since(t0));

        // A query of "1" just means "open it"; anything else is typed into the entry so the
        // debounce, the lazy corpus load and the match all get exercised.
        let entry = (query != "1")
            .then(|| {
                app.window
                    .visible_dialog()
                    .and_then(|d| find_search_entry(d.upcast_ref()))
            })
            .flatten();
        let Some(entry) = entry else {
            bench_quit(&app);
            return;
        };
        let t1 = Instant::now();
        entry.set_text(&query);
        // Debounced, so the keystroke itself must return immediately.
        println!("bench switcher_keystroke_ms {:.1}", ms_since(t1));
        // Long enough for GtkSearchEntry's own ~150 ms delay plus our 50 ms debounce.
        glib::timeout_add_local_once(Duration::from_millis(1500), move || bench_quit(&app));
    });
}

/// First `GtkSearchEntry` in `w`'s subtree, which the bench drives directly because the headless
/// image has no xdotool.
///
/// The caller must pass the palette dialog, not the window: a window holds the sidebar's search
/// entry too, and it comes first in tree order, so searching from the window typed the benchmark's
/// query into the sidebar and measured nothing.
fn find_search_entry(w: &gtk::Widget) -> Option<gtk::SearchEntry> {
    if let Ok(e) = w.clone().downcast::<gtk::SearchEntry>() {
        return Some(e);
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(found) = find_search_entry(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// What New from Template would list: every template, and the destination each one names today.
///
/// The dialog itself cannot be driven under Xvfb, so this is what proves `templates()`, the
/// `accent-target:` directive and the rendered target end to end without a widget.
fn bench_templates(app: &Rc<App>) {
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
    bench_quit(app);
}

/// Quit with a note open, the way Ctrl+Q does, and count the vault's references once the window
/// is gone. The application is held so the process outlives its last window, and the main loop
/// gets up to three seconds to let go of whatever was still in flight. Every reference left is a
/// closed window keeping its vault open — on a remote one, its `serve` session and its forwards.
fn bench_close(app: &Rc<App>) {
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
    let vault = Arc::downgrade(vault);
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
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
        std::process::exit(i32::from(held > 0));
    });
}

/// Show the Git pane, print how many rows its two lists hold, flip the changes list between the
/// tree and the flat view, and activate the history's last row — the Load More row — printing the
/// counts again. The headless image has no pointer, so this is the only way "Load More is the end
/// of the list and paging it in works" and "the tree adds a row per folder" are provable.
fn bench_git(app: &Rc<App>) {
    app.show_pane("git");
    let app = app.clone();
    // Long enough for the debounced refresh and its `git status` and `git log` to land.
    glib::timeout_add_local_once(Duration::from_millis(2500), move || {
        let Some(git) = app.git.get() else {
            return bench_quit(&app);
        };
        // Again: the session restore runs in the post-present idle and deliberately puts Files
        // back, so the pane this hook is about is not the one on screen by the time it reads it.
        app.show_pane("git");
        println!(
            "bench git_changes tree={} {}",
            git.tree(),
            git.changes_rows()
        );
        git.set_tree(!git.tree());
        println!(
            "bench git_changes tree={} {}",
            git.tree(),
            git.changes_rows()
        );
        println!("bench git_rows {}", git.log_rows());
        // What the fetch on opening the vault bought: the branch readout the status bar shows —
        // the dot in front of it means uncommitted work — and how many history rows are drawn as
        // not pulled yet, which only a fetch can have found.
        println!(
            "bench git_branch {}",
            git.branch_label(None).unwrap_or_default()
        );
        let (local, remote) = git.branch_counts();
        println!("bench git_branches local={local} remote={remote}");
        println!("bench git_not_pulled {}", git.not_pulled_rows());
        println!("bench git_sync {}", git.sync_hint().unwrap_or_default());
        let (live, tip) = git.commit_hint();
        println!("bench git_commit live={live} {}", tip.unwrap_or_default());
        git.activate_last_log_row();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Some(git) = app.git.get() {
                println!("bench git_rows {}", git.log_rows());
            }
            bench_quit(&app);
        });
    });
}

/// Drive the key semantics [`multicaret::View`] corrects — the wordwise deletes, logical-line
/// Up/Down, and the same chords at a column of carets — through the very signals the key bindings
/// emit, and print what the buffer and the carets came out as.
///
/// A view of its own in a window of its own, so nothing is written into a vault and the drills do
/// not depend on a document being open. It needs a display, which is why this is a bench hook and
/// not a unit test, but it needs no key press and no pointer: the two signals are actions, and
/// [`multicaret::View::press`] is the key controller's own handler.
fn bench_keys(app: &Rc<App>) {
    let view = multicaret::View::new();
    // The drills are about the code flavours, which is where the logical-line moves are wanted.
    view.set_logical_lines(true);
    view.set_wrap_mode(gtk::WrapMode::Word);
    let window = gtk::Window::builder()
        .default_width(320)
        .default_height(240)
        .child(&view)
        .build();
    window.present();
    let app = app.clone();
    // After a frame, so the view has a size and its lines have been laid out.
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let buffer = view.buffer();
        let text = |buffer: &gtk::TextBuffer| {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };

        // Ctrl+Delete and Ctrl+Backspace take the whitespace run and stop.
        buffer.set_text("   a b");
        buffer.place_cursor(&buffer.start_iter());
        view.emit_delete_from_cursor(gtk::DeleteType::WordEnds, 1);
        println!("bench ctrl_delete {:?}", text(&buffer));
        buffer.set_text("a   b");
        buffer.place_cursor(&buffer.iter_at_offset(4));
        view.emit_delete_from_cursor(gtk::DeleteType::WordEnds, -1);
        println!("bench ctrl_backspace {:?}", text(&buffer));

        // Down is one line of the document even where that line wraps over several rows.
        buffer.set_text(&format!("{}\nshort\ntail", "wide ".repeat(80)));
        buffer.place_cursor(&buffer.iter_at_offset(3));
        let mut row = buffer.start_iter();
        let wraps = view.forward_display_line(&mut row) && row.line() == 0;
        println!("bench wraps {wraps}");
        view.emit_move_cursor(gtk::MovementStep::DisplayLines, 1, false);
        let at = buffer.iter_at_mark(&buffer.get_insert());
        println!("bench down_line {} {}", at.line(), at.line_offset());

        // End goes to the end of the line, not to the end of the screen row it is on.
        buffer.place_cursor(&buffer.iter_at_offset(3));
        view.emit_move_cursor(gtk::MovementStep::DisplayLineEnds, 1, false);
        let at = buffer.iter_at_mark(&buffer.get_insert());
        println!("bench end_line {} {}", at.line(), at.line_offset());

        // Every caret answers Ctrl+Delete, not only the primary one.
        buffer.set_text("a   b\nc   d");
        buffer.place_cursor(&buffer.iter_at_offset(1));
        view.add_caret(true);
        view.press(gdk::Key::Delete, gdk::ModifierType::CONTROL_MASK);
        println!("bench caret_delete {:?}", text(&buffer));
        view.clear_carets();

        // Every caret moves wordwise, and a trip down over a short line and back up restores the
        // constellation rather than flattening it.
        buffer.set_text("alpha beta\nxy\ngamma delta\nomega zeta");
        buffer.place_cursor(&buffer.start_iter());
        view.add_caret(true);
        view.add_caret(true);
        view.press(gdk::Key::Right, gdk::ModifierType::CONTROL_MASK);
        println!("bench caret_words {:?}", view.caret_positions());
        view.press(gdk::Key::Down, gdk::ModifierType::empty());
        println!("bench caret_down {:?}", view.caret_positions());
        view.press(gdk::Key::Up, gdk::ModifierType::empty());
        println!("bench caret_columns {:?}", view.caret_positions());
        view.clear_carets();

        // Tab at every caret is what the view says it is, from the column each caret is in.
        view.set_tab_width(4);
        for spaces in [true, false] {
            view.set_insert_spaces_instead_of_tabs(spaces);
            buffer.set_text("ab\ncd");
            buffer.place_cursor(&buffer.iter_at_offset(1));
            view.add_caret(true);
            view.press(gdk::Key::Tab, gdk::ModifierType::empty());
            println!("bench caret_tab spaces={spaces} {:?}", text(&buffer));
            view.clear_carets();
        }

        // A column takes the blink over, GTK's own caret going transparent with the class, and
        // hands it back when it goes. How it looks is a manual check; that it toggles is not.
        view.add_caret(true);
        println!("bench caret_blink {}", view.has_css_class("accent-carets"));
        view.clear_carets();
        println!("bench caret_blink {}", view.has_css_class("accent-carets"));

        window.close();
        bench_quit(&app);
    });
}

/// What a path entry's completion does with the keyboard, and whether the entry takes the width
/// the list gives the dialog.
///
/// Layout is real under Xvfb — an allocation wants a mapped window, not a window manager — so the
/// width either side of the list appearing is measured rather than argued. The keys are emitted on
/// the entry's own controller, which proves the handler, the selection and the text it applies but
/// **not** the propagation phase: emitting a signal skips phase dispatch altogether, so that Return
/// beats `GtkText`'s own binding is still a claim only a real session can settle.
fn bench_paths(app: &Rc<App>) {
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

/// Emit a key press on the entry's own key controller: the headless image has no window manager
/// to give the toplevel the keyboard, and no xdotool to press anything with.
fn press_key(entry: &gtk::Entry, key: gdk::Key) {
    use glib::translate::IntoGlib;
    let controllers = entry.observe_controllers();
    for i in 0..controllers.n_items() {
        let Some(keys) = controllers
            .item(i)
            .and_downcast::<gtk::EventControllerKey>()
        else {
            continue;
        };
        keys.emit_by_name::<bool>(
            "key-pressed",
            &[&key.into_glib(), &0u32, &gdk::ModifierType::empty()],
        );
    }
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

/// Focus mode, headless: [`bench_chrome_actions`] first, then — when `notes` is `<relA>,<relB>` —
/// both notes open, one of them split off to the right so the left pane is the one being written
/// in and the right one has something to recede, and [`bench_chrome_levels`] and
/// [`bench_chrome_veil`] run over them. `1` stops after the actions.
///
/// The actions go first because one of them is `win.save`, and an explicit save writes whatever
/// the active tab holds, clean or not: run over the notes it would write them back into the vault.
fn bench_chrome(app: &Rc<App>, notes: &str) {
    bench_chrome_actions(app);
    let Some((a, b)) = notes.split_once(',') else {
        return bench_quit(app);
    };
    // The actions end on a find, whose open bar would suspend the fade.
    app.pane().find.close();
    app.open_path(a);
    app.open_path(b);
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(300)).await;
        let left = app.pane();
        let _ = WidgetExt::activate_action(&app.window, "win.split-right", None);
        // The split hands the keyboard to the note it moved, from an idle; this takes it back
        // once that has run, since nothing headless can click into the left note.
        glib::timeout_future(Duration::from_millis(200)).await;
        app.set_active_pane(&left);
        bench_chrome_levels(&app);
        bench_chrome_veil(&app).await;
        bench_quit(&app);
    });
}

/// Show and then hide the chrome at each focus level, printing what that faded. The level is set
/// in memory only; nothing is saved.
fn bench_chrome_levels(app: &Rc<App>) {
    let was = app.config.borrow().focus_mode;
    for level in [FocusMode::None, FocusMode::Medium, FocusMode::High] {
        app.config.borrow_mut().focus_mode = level;
        app.show_chrome();
        app.hide_chrome();
        println!("bench chrome_level {level:?} {}", chrome_state(app));
    }
    app.show_chrome();
    println!("bench chrome_level shown {}", chrome_state(app));
    app.config.borrow_mut().focus_mode = was;
}

/// What the chrome and the panes carry: the header's and the sidebar's fade, the active note's
/// minimap, how many panes recede, and whether the active note's line fade is on.
fn chrome_state(app: &Rc<App>) -> String {
    let hidden = |w: &gtk::Widget| w.has_css_class("chrome-hidden");
    let tab = app.active();
    let away = app
        .panes
        .borrow()
        .iter()
        .filter(|pane| pane.widget().has_css_class("chrome-away"))
        .count();
    format!(
        "hidden={} sidebar={} map={} away={away} fade={}",
        hidden(app.header.upcast_ref()),
        app.sidebar.get().is_some_and(|s| hidden(s.widget())),
        tab.as_ref().is_some_and(|t| hidden(t.minimap())),
        tab.as_ref()
            .and_then(|t| t.ghost_view())
            .is_some_and(|view| view.fading()),
    )
}

/// The line fade on the active note at High: held for a second and a half with the caret on line
/// 8 and the line numbers on, which is long enough for `import -window root` to see the veil and
/// the gutter it leaves alone, then [`fade::paint`] timed over a 2 KB and a 64 KB note. The veil is
/// a rectangle per line on screen, so the two should cost the same.
///
/// `Tab::set_text` leaves the tab clean and the note's own text goes back at the end, so the
/// tab closes as it opened and nothing asks to write it.
async fn bench_chrome_veil(app: &Rc<App>) {
    let Some(tab) = app.active() else {
        return;
    };
    let was = app.config.borrow().focus_mode;
    app.config.borrow_mut().focus_mode = FocusMode::High;
    tab.set_line_numbers(true);
    if let Some(iter) = tab.buffer.iter_at_line(8) {
        tab.buffer.place_cursor(&iter);
    }
    app.hide_chrome();
    println!("bench chrome_veil {} caret_line=8", tab.rel());
    glib::timeout_future(Duration::from_millis(1500)).await;
    let own = tab.text();
    for chars in [2 * 1024, 64 * 1024] {
        // Exactly 32 bytes, so the body is exactly the size the numbers are labelled with.
        let body = "filler text for a long-ish note\n";
        tab.set_text(&body.repeat(chars / body.len()));
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        // A frame to lay the new text out, which is what the veil measures its lines against.
        glib::timeout_future(Duration::from_millis(300)).await;
        let Some(view) = tab.ghost_view() else {
            break;
        };
        const PAINTS: u32 = 200;
        let snapshot = gtk::Snapshot::new();
        let t0 = Instant::now();
        for _ in 0..PAINTS {
            fade::paint(view, &snapshot, 1.0);
        }
        let us = t0.elapsed().as_secs_f64() * 1e6 / f64::from(PAINTS);
        drop(snapshot.to_node());
        println!("bench fade_paint_us chars={chars} {us:.1}");
    }
    tab.set_text(&own);
    app.show_chrome();
    app.config.borrow_mut().focus_mode = was;
}

/// Fire the actions the chords go through at a faded window, and print whether the chrome came
/// back. An action on its own must not bring it back: focus mode ends on pointer motion, a scroll,
/// Escape, a focus change or a view-mode change, and an action is none of those (DESIGN.md,
/// Chrome auto-hide). Find is the counter-example that proves the rule — its bar takes the
/// keyboard, so the chrome returns through `focus-widget` rather than through the activation.
///
/// `Ctrl+Left` and `Ctrl+Right` are printed alongside as the accelerators they are not: nothing in
/// [`ACTIONS`] claims either chord, so they activate nothing and reach no `show_chrome` at all. If
/// focus mode still drops on them, the cause is elsewhere.
fn bench_chrome_actions(app: &Rc<App>) {
    // Find last: it leaves its bar open, and an open find bar suspends the fade entirely.
    for action in ["win.save", "win.scroll-down", "win.zoom-in", "win.find"] {
        app.hide_chrome();
        let _ = WidgetExt::activate_action(&app.window, action, None);
        println!("bench chrome_hidden {action} {}", app.chrome_hidden.get());
    }
    if let Some(gtk_app) = app.window.application() {
        for accel in ["<Control>Left", "<Control>Right"] {
            println!(
                "bench chrome_accel {accel} {:?}",
                gtk_app.actions_for_accel(accel)
            );
        }
    }
}

/// Open the two notes `rels` names in one pane, then split the second one off to the right, move
/// it back, and ask for a move where there is no pane to move into.
///
/// What is printed is the **geometry** of the pane holding that tab, not its index: panes are
/// kept in the order they were made, which is not the order they are drawn in, so only the
/// rectangle says a tab really changed side.
fn bench_panes(app: &Rc<App>, rels: &str) {
    let Some((a, b)) = rels.split_once(',') else {
        return bench_quit(app);
    };
    app.open_path(a);
    app.open_path(b);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(200), move || {
        let Some(page) = app.tabs().selected_page() else {
            return bench_quit(&app);
        };
        // Staged by hand, because nothing headless clicks into a view: the reader is typing in
        // this pane, which is what makes "does the keyboard go with the tab" a question at all.
        if let Some(tab) = app.active() {
            tab.view.grab_focus();
        }
        bench_pane_at(&app, &page);
        bench_pane_step(&app, &page, 0);
    });
}

/// One step of [`bench_panes`], read back after the frame it needs: `panes::neighbour` is
/// geometric, so it wants the allocation a split has not been given yet, and an emptied pane
/// closes itself from an idle.
fn bench_pane_step(app: &Rc<App>, page: &adw::TabPage, step: usize) {
    const STEPS: &[&str] = &["win.split-right", "win.move-tab-left", "win.move-tab-right"];
    let Some(action) = STEPS.get(step) else {
        if let Some(gtk_app) = app.window.application() {
            for accel in ["<Shift><Alt>Left", "<Shift><Alt>Right"] {
                println!("bench accel {accel} {:?}", gtk_app.actions_for_accel(accel));
            }
        }
        return bench_quit(app);
    };
    println!("bench step {action}");
    let _ = WidgetExt::activate_action(&app.window, action, None);
    let (app, page) = (app.clone(), page.clone());
    glib::timeout_add_local_once(Duration::from_millis(200), move || {
        bench_pane_at(&app, &page);
        bench_pane_step(&app, &page, step + 1);
    });
}

/// How many panes there are, and where in the window three things sit: the pane holding `page`,
/// the pane a note would open into, and the pane holding the keyboard. All three have to name the
/// same pane after a move, or the window says the tab went somewhere the caret did not.
fn bench_pane_at(app: &Rc<App>, page: &adw::TabPage) {
    println!("bench panes {}", app.panes.borrow().len());
    let root = app.window.clone().upcast::<gtk::Widget>();
    let at = |what: &str, pane: Option<&Rc<Pane>>| match pane {
        Some(pane) => {
            let r = pane_rect(pane, &root);
            println!("bench {what} x={} y={}", r.x().round(), r.y().round());
        }
        None => println!("bench {what} none"),
    };
    at("tab_at", app.pane_of(page).as_ref());
    at("active_at", Some(&app.pane()));
    let focused = gtk::prelude::GtkWindowExt::focus(&app.window).and_then(|w| {
        app.panes
            .borrow()
            .iter()
            .find(|pane| w.is_ancestor(pane.widget()))
            .cloned()
    });
    at("focus_at", focused.as_ref());
}

/// The find bar and the Outline pane across a tab switch and a close: a note with the bar open, a
/// shell in front of it, the chord over that shell, back to the note, then a PDF, then every tab
/// closed. One line per step, so what a switch and the last close leave behind is a printout
/// rather than an argument.
fn bench_tabs(app: &Rc<App>, rels: &str) {
    let Some((note, pdf)) = rels.split_once(',') else {
        return bench_quit(app);
    };
    let (note, pdf) = (note.to_string(), pdf.to_string());
    // Looked at rather than named, so the note arrives as a preview and says so on its tab.
    app.open_preview(&note);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        if let Some(tab) = app.tab_for(&note) {
            println!("bench tab_preview {}", bench_tab_line(&tab.page));
            // A click on the eye: the one way to keep a preview that does not go through the app.
            app.pane()
                .tabs
                .emit_by_name::<()>("indicator-activated", &[&tab.page]);
            println!("bench tab_kept {}", bench_tab_line(&tab.page));
        }
        let _ = WidgetExt::activate_action(&app.window, "win.find", None);
        println!("bench find_over_note {}", app.pane().find.is_open());
        app.open_terminal();
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            println!("bench find_over_shell {}", app.pane().find.is_open());
            let _ = WidgetExt::activate_action(&app.window, "win.find", None);
            println!("bench find_chord_over_shell {}", app.pane().find.is_open());
            // Back to the note: the bar belongs to the pane, so this says whether it comes back
            // on its own or wants the chord again.
            if let Some(tab) = app.tab_for(&note) {
                app.reveal_page(&tab.page);
            }
            println!("bench find_back_on_note {}", app.pane().find.is_open());
            // And the chord still opens it: closing the bar over a shell must not leave it dead
            // for the tab the reader comes back to.
            let _ = WidgetExt::activate_action(&app.window, "win.find", None);
            println!(
                "bench find_chord_back_on_note {}",
                app.pane().find.is_open()
            );
            app.open_path(&pdf);
            // Long enough for the render thread to open the document: an outline read before that
            // says "Opening…" whatever else is wrong.
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                println!("bench outline_pdf {}", bench_outline(&app));
                for doc in app.docs() {
                    app.close_page(doc.page());
                }
                glib::timeout_add_local_once(Duration::from_millis(400), move || {
                    println!("bench outline_closed {}", bench_outline(&app));
                    bench_quit(&app);
                });
            });
        });
    });
}

/// A tab as its bar draws it: the title, and what the indicator slot holds and says.
fn bench_tab_line(page: &adw::TabPage) -> String {
    let icon = page
        .indicator_icon()
        .and_then(|icon| IconExt::to_string(&icon));
    format!(
        "title={} indicator={} tip={:?}",
        page.title(),
        icon.as_deref().unwrap_or("none"),
        page.indicator_tooltip()
    )
}

/// What the Outline pane holds, by widget type — or by title where that is one of its status
/// pages, "No Outline" being the empty state a closed document has to leave behind.
fn bench_outline(app: &Rc<App>) -> String {
    let Some(sidebar) = app.sidebar.get() else {
        return "no sidebar".to_string();
    };
    match sidebar.outline_child() {
        Some(child) => match child.downcast::<adw::StatusPage>() {
            Ok(page) => format!("status {}", page.title()),
            Err(child) => child.type_().name().to_string(),
        },
        None => "nothing".to_string(),
    }
}

/// Type a heading into the note at `rel`, at a size that styles on the keystroke and at one that
/// used to wait for the debounce, and print whether the `h1` tag is on the line *before the main
/// loop turns again*. `changed` is emitted from inside the insert, so a `true` here can only have
/// come from the synchronous path — which is the whole question this bench answers.
fn bench_style(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        for chars in [2 * 1024, 32 * 1024] {
            bench_style_typing(&tab, chars);
        }
        bench_style_fenced(&tab);
        // A heading typed far from the caret is what the fast path deliberately leaves out: it
        // belongs to the debounced pass, and this says the pass still lands and still fixes it.
        tab.buffer.insert(&mut tab.buffer.start_iter(), "# Far\n");
        println!("bench style_far_sync {}", bench_heading_at(&tab, 0));
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            println!("bench style_debounced {}", bench_heading_at(&tab, 0));
            bench_quit(&app);
        });
    });
}

/// Fill `tab` with `chars` of body, then type `# Heading` on a line of its own, one character at a
/// time the way a keyboard delivers it.
fn bench_style_typing(tab: &Rc<Tab>, chars: usize) {
    // Exactly 32 bytes, so the body is exactly the size the numbers are labelled with.
    let body = "filler text for a long-ish note\n";
    tab.set_text(&body.repeat(chars / body.len()));
    tab.buffer.place_cursor(&tab.buffer.end_iter());
    for ch in "\n# Headin".chars() {
        tab.buffer.insert_at_cursor(&ch.to_string());
    }
    let t0 = Instant::now();
    tab.buffer.insert_at_cursor("g");
    let us = t0.elapsed().as_micros();
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    println!(
        "bench style_sync chars={chars} {}",
        bench_heading_at(tab, line)
    );
    println!("bench style_us {us}");
}

/// Type the same heading *inside a fenced block* on a note too long for a full pass. The line is
/// tagged from a parse of the whole document, so the fence above it is what decides what it is:
/// this is the claim a per-line pass stands or falls on, printed rather than argued.
fn bench_style_fenced(tab: &Rc<Tab>) {
    let body = "filler text for a long-ish note\n".repeat(1024);
    tab.set_text(&format!("{body}```\n\n```\n"));
    let Some(inside) = tab.buffer.iter_at_line(1025) else {
        return;
    };
    tab.buffer.place_cursor(&inside);
    for ch in "# Heading".chars() {
        tab.buffer.insert_at_cursor(&ch.to_string());
    }
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    println!(
        "bench style_fenced h1={} codeblock={}",
        bench_tag_at(tab, line, "h1"),
        bench_tag_at(tab, line, "codeblock")
    );
}

fn bench_heading_at(tab: &Rc<Tab>, line: i32) -> bool {
    bench_tag_at(tab, line, "h1")
}

fn bench_tag_at(tab: &Rc<Tab>, line: i32, name: &str) -> bool {
    let Some(tag) = tab.buffer.tag_table().lookup(name) else {
        return false;
    };
    tab.buffer
        .iter_at_line(line)
        .is_some_and(|iter| iter.has_tag(&tag))
}

/// Select things in the note at `rel` and print what the muted occurrence highlight made of each:
/// the query it took and the character ranges it painted. Then open the find bar on the same word,
/// so the last lines say what happens where a find-bar match and a muted occurrence land on the
/// same text: both tags are on it, and the find bar's is the higher priority of the two.
///
/// The buffer is filled with text of its own first: the ranges are the point, and they have to be
/// the bench's rather than whatever the vault generator wrote. Nothing is saved — the run quits
/// well inside the one-second autosave.
/// What a Ctrl+hover underlines. The link half only: a plain word is a question for a language
/// server, and the vault a drill runs against holds notes rather than code.
fn bench_follow(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        // ASCII throughout, so `find` gives the character offset the buffer counts in.
        let text = "See [[Other Note]] and a plain word here.\n";
        tab.set_text(text);
        // The link table is filled by the analysis debounce, not by the edit.
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            let at = |needle: &str| text.find(needle).expect("bench needle") as i32;
            let probe = |label: &str, needle: &str, ctrl: bool| {
                let where_ = tab
                    .view
                    .iter_location(&tab.buffer.iter_at_offset(at(needle)));
                let (x, y) = tab.view.buffer_to_window_coords(
                    gtk::TextWindowType::Widget,
                    where_.x() + 1,
                    where_.y() + 1,
                );
                tab.follow_hint(x as f64, y as f64, ctrl);
                println!("bench follow {label} underlined={:?}", tab.follow_shown());
            };
            // The link underlines whole, markers and all; the prose beside it does not.
            probe("wikilink", "Other", true);
            probe("plain_word", "plain", true);
            probe("wikilink_again", "Other", true);
            // Ctrl up over the same link takes it off again.
            probe("ctrl_released", "Other", false);
            bench_quit(&app);
        });
    });
}

fn bench_occurrences(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        // ASCII throughout, so the byte offset `find` gives is also the character offset the
        // buffer counts in.
        let text = "Alpha beta alpha\ngamma ALPHA delta\nx y\n";
        tab.set_text(text);
        let (find_colour, muted_colour) = tab.match_colours();
        println!("bench occur colours find={find_colour:?} muted={muted_colour:?}");

        for (label, selected) in [
            // Two characters, and the three occurrences differ in case: the shortest selection
            // that highlights anything, matched the way the find bar matches.
            ("two_chars", "al"),
            ("word", "beta"),
            // A selection whose own case is the odd one out still finds the other two.
            ("cased", "ALPHA"),
            // Neither of these highlights anything.
            ("one_char", "x"),
            ("multi_line", "alpha\ngamma"),
        ] {
            let at = text.find(selected).expect("bench needle") as i32;
            tab.buffer.select_range(
                &tab.buffer.iter_at_offset(at),
                &tab.buffer
                    .iter_at_offset(at + selected.chars().count() as i32),
            );
            let (query, tag) = tab.occurrence_highlight();
            println!(
                "bench occur case={label} select={selected:?} query={query:?} at={:?}",
                bench_tag_ranges(&tab, &tag)
            );
        }

        // Both highlights on the same word. The find bar's tag has to be the higher priority of
        // the two, or the muted hint would paint over the match the user is stepping through —
        // and it has to stay that way across an edit, which is when gtksourceview re-raises its
        // own tag.
        tab.set_query("al");
        tab.set_highlight(true);
        bench_pump();
        let at = text.find("al").expect("bench needle") as i32;
        tab.buffer.select_range(
            &tab.buffer.iter_at_offset(at),
            &tab.buffer.iter_at_offset(at + 2),
        );
        let (_, muted) = tab.occurrence_highlight();
        let find = bench_search_tag(&tab, find_colour.as_deref());
        println!(
            "bench occur overlap find={:?} muted={:?}",
            find.as_ref()
                .map(|tag| bench_tag_ranges(&tab, tag))
                .unwrap_or_default(),
            bench_tag_ranges(&tab, &muted)
        );
        for what in ["unedited", "edited"] {
            println!(
                "bench occur priority {what} find={:?} muted={}",
                bench_search_tag(&tab, find_colour.as_deref()).map(|tag| tag.priority()),
                muted.priority()
            );
            tab.buffer.insert(&mut tab.buffer.end_iter(), "al\n");
            bench_pump();
        }
        bench_quit(&app);
    });
}

/// Turn the main loop until it has nothing left to dispatch. The find bar's own highlight is
/// scanned on an idle, so its tag is on nothing at all the instant its query is set.
fn bench_pump() {
    let context = glib::MainContext::default();
    for _ in 0..10_000 {
        if !context.iteration(false) {
            return;
        }
    }
}

/// The tag the find bar's search context paints with, found by its colour: gtksourceview keeps
/// that tag to itself, and the scheme's `search-match` background is what it was given.
fn bench_search_tag(tab: &Rc<Tab>, colour: Option<&str>) -> Option<gtk::TextTag> {
    let wanted = gdk::RGBA::parse(colour?).ok()?;
    let mut found = None;
    tab.buffer.tag_table().foreach(|tag| {
        if found.is_none() && tag.is_background_set() && tag.background_rgba() == Some(wanted) {
            found = Some(tag.clone());
        }
    });
    found
}

/// Where `tag` is on, as character offsets.
fn bench_tag_ranges(tab: &Rc<Tab>, tag: &gtk::TextTag) -> Vec<(i32, i32)> {
    let mut ranges = Vec::new();
    let mut iter = tab.buffer.start_iter();
    loop {
        if !iter.starts_tag(Some(tag)) && !iter.forward_to_tag_toggle(Some(tag)) {
            return ranges;
        }
        let start = iter.offset();
        if !iter.forward_to_tag_toggle(Some(tag)) {
            ranges.push((start, tab.buffer.end_iter().offset()));
            return ranges;
        }
        ranges.push((start, iter.offset()));
    }
}

/// A shell focused in a window that does not have the keyboard must not narrow the application's
/// accelerator table, and one in the window that does must. Under Xvfb no window is ever
/// activated, so the active one is the last added: a second window is opened first and the shell
/// then opens in this one, which is the state after switching windows away from a shell. Closing
/// the second window hands the keyboard back through the same `active-window` notify a real
/// switch goes through. Prints what `Ctrl+S` activates: `["win.save"]`, then `[]`.
fn bench_shell_keys(app: &Rc<App>) {
    let Some(gtk_app) = app.window.application().and_downcast::<adw::Application>() else {
        return bench_quit(app);
    };
    let Some(other) = app
        .shell
        .upgrade()
        .and_then(|shell| shell.loose_window(&gtk_app))
    else {
        return bench_quit(app);
    };
    app.open_terminal();
    let print = move |when: &str| {
        println!(
            "bench shell_keys {when} {:?}",
            gtk_app.actions_for_accel("<Control>s")
        );
    };
    // The shell takes focus from an idle.
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(200), move || {
        print("shell-elsewhere");
        other.window.close();
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            print("shell-here");
            bench_quit(&app);
        });
    });
}

/// Closing the window is not enough to end the process while a dialog is up: quit the
/// application so the bench always terminates.
/// The note is given fifty lines, written out, then edited in two places: a rewrite near the
/// top and a line added at the end. The comparison with the disk copy is then read back — rows,
/// hunks, hidden runs, buttons, and how many rows GTK lays out at a height other than the one
/// the alignment asked for (0 is the claim) — before the first hunk is taken from Theirs, the
/// hidden run is opened, and the same is read again. Then two blobs in a tab of their own, at a
/// zoom, for the same numbers. With the vault under git, last, the working tree against the index
/// in the note's tab: whether it opened with the run before the first change folded and the caret
/// on that change, and then a character typed into it, see [`bench_compare_type`]. That half wants
/// a scratch repository whose committed note differs from the fifty lines in a few places, one of
/// them a long line where the drill writes a short one, so the change is padded and the view has
/// room to scroll.
fn bench_compare(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        let body: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        tab.set_text(&body);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare write_failed {e}");
            return bench_quit(&app);
        }
        let (mut a, mut b) = (
            tab.buffer
                .iter_at_line(2)
                .unwrap_or_else(|| tab.buffer.end_iter()),
            tab.buffer
                .iter_at_line(3)
                .unwrap_or_else(|| tab.buffer.end_iter()),
        );
        tab.buffer.delete(&mut a, &mut b);
        tab.buffer.insert(&mut a, "line three\n");
        tab.buffer
            .insert(&mut tab.buffer.end_iter(), "added at the end\n");
        app.compare_with_disk(&tab);
        glib::timeout_add_local_once(Duration::from_millis(600), move || {
            let Some(compare) = tab.comparison() else {
                println!("bench compare none");
                return bench_quit(&app);
            };
            println!("bench compare {}", bench_compare_line(&compare));
            compare.take_hunk(0, false);
            compare.open_gap(0);
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                let line = tab
                    .buffer
                    .iter_at_line(2)
                    .map(|start| {
                        let mut end = start;
                        end.forward_to_line_end();
                        tab.buffer.text(&start, &end, true).to_string()
                    })
                    .unwrap_or_default();
                println!(
                    "bench compare_after {} line3={line:?}",
                    bench_compare_line(&compare)
                );
                tab.leave_compare();
                println!(
                    "bench compare_left comparing={}",
                    tab.comparison().is_some()
                );
                let new = body.replace("line 10\n", "line ten\n");
                let diff = app.open_diff(
                    "diff:bench",
                    "bench.md",
                    "bench",
                    ("old", &body),
                    ("new", &new),
                );
                app.set_zoom(1.5);
                glib::timeout_add_local_once(Duration::from_millis(500), move || {
                    println!(
                        "bench compare_blobs {}",
                        bench_compare_line(diff.comparison())
                    );
                    // With the vault under git: the working tree against the index, in the
                    // note's tab, which the Git pane reaches through the same door as a row.
                    let Some(git) = app.git.get().filter(|git| git.has_repos()) else {
                        println!("bench compare_worktree no_repo");
                        return bench_quit(&app);
                    };
                    // Where a note that has just been opened has its caret.
                    tab.buffer.place_cursor(&tab.buffer.start_iter());
                    git.compare_worktree(&tab.rel());
                    glib::timeout_add_local_once(Duration::from_millis(800), move || {
                        let Some(compare) = tab.comparison() else {
                            println!("bench compare_worktree none");
                            return bench_quit(&app);
                        };
                        println!(
                            "bench compare_worktree title={:?} {} first={:?}",
                            tab.page.title(),
                            bench_compare_line(&compare),
                            compare.first_misaligned()
                        );
                        let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
                        println!(
                            "bench compare_open leading_hidden={} caret_on_first_change={}",
                            compare.hides_row(0),
                            compare.opens_at() == Some(caret.offset())
                        );
                        let then = tab.clone();
                        bench_compare_type(&tab, 0.5, move || {
                            bench_compare_type(&then, 0.0, move || bench_quit(&app))
                        });
                    });
                });
            });
        });
    });
}

/// Type one character into the first change on the comparing editor's side, `at` of the way
/// along its line, and print what moved while the comparison caught up: how often the shared
/// scroll range changed, whether the scroll position did, how many rows were off right after the
/// keystroke and once it had settled, and how often something laid over the editor was hidden or
/// shown. The claim is 0, false, 0, 0, 0. `padded` says whether the line carried alignment
/// padding, which is the case the flash was about, and `scroll` is the position against the
/// furthest it can go, which says whether it had room to drift.
fn bench_compare_type(tab: &Rc<Tab>, at: f64, then: impl FnOnce() + 'static) {
    let tab = tab.clone();
    glib::timeout_add_local_once(Duration::from_millis(300), move || {
        let (Some(compare), Some(adj)) = (tab.comparison(), tab.view.vadjustment()) else {
            return then();
        };
        let Some(line) = compare.opens_at().map(|o| tab.buffer.iter_at_offset(o)) else {
            println!("bench compare_type at={at} no_change");
            return then();
        };
        let base = tab.view.pixels_above_lines();
        let padded = line
            .tags()
            .iter()
            .any(|t| t.is_pixels_above_lines_set() && t.pixels_above_lines() > base);
        let mut end = line;
        end.forward_to_line_end();
        let offset = line.offset() + ((end.offset() - line.offset()) as f64 * at) as i32;
        let mark = tab.buffer.create_mark(None, &line, true);
        tab.view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.5);
        tab.buffer.delete_mark(&mark);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let (value, moves, flips) = (adj.value(), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
            let count = |n: &Rc<Cell<u32>>| {
                let n = n.clone();
                move || n.set(n.get() + 1)
            };
            let tick = count(&moves);
            let mut ids = vec![(
                adj.clone().upcast::<glib::Object>(),
                adj.connect_upper_notify(move |_| tick()),
            )];
            // The overlaid buttons sit one level down, on the view's text child.
            let mut stack = vec![tab.view.clone().upcast::<gtk::Widget>()];
            while let Some(widget) = stack.pop() {
                let mut child = widget.first_child();
                while let Some(c) = child {
                    let tick = count(&flips);
                    ids.push((
                        c.clone().upcast(),
                        c.connect_visible_notify(move |_| tick()),
                    ));
                    child = c.next_sibling();
                    stack.push(c);
                }
            }
            tab.buffer
                .insert(&mut tab.buffer.iter_at_offset(offset), "x");
            let now = compare.misaligned();
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                println!(
                    "bench compare_type at={at} padded={padded} upper_moves={} value_moved={} misaligned_now={now} misaligned={} flips={} scroll={value}/{}",
                    moves.get(),
                    adj.value() != value,
                    compare.misaligned(),
                    flips.get(),
                    adj.upper() - adj.page_size(),
                );
                for (object, id) in ids {
                    object.disconnect(id);
                }
                then();
            });
        });
    });
}

fn bench_compare_line(compare: &diff::Compare) -> String {
    let (rows, hunks, hidden, buttons) = compare.counts();
    format!(
        "rows={rows} hunks={hunks} hidden={hidden} buttons={buttons} misaligned={}",
        compare.misaligned()
    )
}

/// Open a PDF, leave the reader halfway down its second page, and fit the page from there.
///
/// Fit Page is fired as the window action the status bar's menu and the palette both fire, so a
/// route that never reaches the tab shows up here as a zoom that did not change.
fn bench_pdf(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    // The pages are measured on the render thread, so nothing about the layout is known until it
    // has reported back.
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        let Some(pdf) = app.active_pdf() else {
            println!("bench pdf no_tab");
            return bench_quit(&app);
        };
        println!("bench pdf pages={} {}", pdf.page_count(), pdf.geometry());
        let page = 1.min(pdf.page_count().saturating_sub(1));
        pdf.scroll_to(pdfview::Anchor {
            page,
            u: 0.0,
            v: 0.5,
        });
        println!("bench pdf mid_page {}", pdf.geometry());
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-fit-page", None);
        println!(
            "bench pdf fit_page {} label={:?}",
            pdf.geometry(),
            pdf.zoom_label()
        );
        bench_quit(&app);
    });
}

fn bench_quit(app: &Rc<App>) {
    match app.window.application() {
        Some(gtk_app) => gtk_app.quit(),
        None => app.window.close(),
    }
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn bench_expand(app: &Rc<App>, rel: &str) {
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
