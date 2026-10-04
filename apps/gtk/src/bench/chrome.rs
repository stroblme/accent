//! Drills over focus mode: what the chrome and the panes fade, and the line fade's cost.

use super::*;

/// Focus mode, headless: [`bench_chrome_actions`] first, then — when `notes` is `<relA>,<relB>` —
/// both notes open, one of them split off to the right so the left pane is the one being written
/// in and the right one has something to recede, and [`bench_chrome_levels`] and
/// [`bench_chrome_veil`] run over them. `1` stops after the actions.
///
/// The actions go first because one of them is `win.save`, and an explicit save writes whatever
/// the active tab holds, clean or not: run over the notes it would write them back into the vault.
pub(super) fn bench_chrome(app: &Rc<App>, notes: &str) {
    bench_chrome_actions(app);
    let Some((a, b)) = notes.split_once(',') else {
        return bench_quit(app);
    };
    // The actions end on a find, and its bar stays open through the rest: an open find bar must
    // not keep the fade away.
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
        if let Some(tab) = app.tab_of(&left) {
            tab.view.grab_focus();
        }
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
/// minimap, how many panes recede, whether the active note's line fade is on, and whether the
/// dividers are gone.
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
        "hidden={} sidebar={} map={} away={away} fade={} dividers={}",
        hidden(app.header.upcast_ref()),
        app.sidebar.get().is_some_and(|s| hidden(s.widget())),
        tab.as_ref().is_some_and(|t| hidden(t.minimap())),
        tab.as_ref()
            .and_then(|t| t.ghost_view())
            .is_some_and(|view| view.fading()),
        !app.window.has_css_class("dividers-hidden"),
    )
}

/// The line fade on the active note at High: held for a second and a half with the caret on line
/// 8, the line numbers on and "the" highlighted as a find-bar query, which is long enough for
/// `import -window root` to see the veil, the gutter it leaves alone and the lines holding a
/// match, which it leaves alone too. Then [`fade::paint`] timed over a 2 KB and a 64 KB note,
/// with no match on screen and with one on every line. The veil is a rectangle per line on
/// screen, and the matches are looked for a line at a time, so the sizes should cost the same.
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
    tab.set_query("the");
    tab.set_highlight(true);
    app.hide_chrome();
    println!(
        "bench chrome_veil {} caret_line=8 find_open={} {}",
        tab.rel(),
        app.pane().find.is_open(),
        chrome_state(app)
    );
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
        let mut us = [0.0; 2];
        for (query, us) in ["the", "filler"].into_iter().zip(&mut us) {
            tab.set_query(query);
            // Time for the context to scan the note, as it has by the time anyone is typing.
            glib::timeout_future(Duration::from_millis(300)).await;
            let snapshot = gtk::Snapshot::new();
            let t0 = Instant::now();
            for _ in 0..PAINTS {
                fade::paint(view, &snapshot, 1.0);
            }
            *us = t0.elapsed().as_secs_f64() * 1e6 / f64::from(PAINTS);
            drop(snapshot.to_node());
        }
        println!(
            "bench fade_paint_us chars={chars} {:.1} matched={:.1}",
            us[0], us[1]
        );
    }
    tab.set_highlight(false);
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
    // Find last: it leaves its bar open for the drills after this one.
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

/// Focus mode on the keys that step through a document, through the real key path: a note, the
/// preview beside it and a PDF each get the keyboard in turn, and for every case the drill shows
/// the chrome, prints `bench chrome_key_ready <case> <chord>` for an XTEST press of that chord
/// (`build-aux/xtest.py :N "key <chord>"`), and a second later prints whether the chrome went.
/// It first prints `bench chrome_keys focus_window` and waits for the window to have the X input
/// focus, which under Xvfb is `xtest.py :N "move 700 400; focus"`.
///
/// The cases that must leave the chrome up: a press under a completion popup, whose arrows pick a
/// row; one in the file tree, which is not a document; `Shift+Alt+Down`, which adds a caret; and
/// Space in a PDF being presented, where presentation owns the chrome.
pub(super) fn bench_chrome_keys(app: &Rc<App>, rels: &str) {
    let Some((note, pdf)) = rels.split_once(',') else {
        return bench_quit(app);
    };
    let (app, note, pdf) = (app.clone(), note.to_string(), pdf.to_string());
    glib::spawn_future_local(async move {
        println!("bench chrome_keys focus_window");
        for _ in 0..100 {
            if app.window.is_active() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        app.open_path(&note);
        glib::timeout_future(Duration::from_millis(500)).await;
        let Some(tab) = app.active() else {
            return bench_quit(&app);
        };
        let view = tab.view.clone().upcast::<gtk::Widget>();
        // Long enough to scroll, so the scroll chord can show it still scrolls.
        let own = tab.text();
        tab.set_text(&"a line of the note\n".repeat(300));
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        let scrolled = || tab.view.vadjustment().map_or(0.0, |a| a.value());
        for (case, chord) in [
            ("caret", "Down"),
            ("word", "ctrl+Right"),
            ("select", "shift+End"),
            ("scroll_chord", "ctrl+Down"),
            ("add_caret", "shift+alt+Down"),
        ] {
            let before = scrolled();
            bench_chrome_key(&app, &view, case, chord).await;
            println!("bench chrome_key {case} scrolled={}", scrolled() != before);
        }
        // A column of carets, which replays the arrows itself: it keeps both carets.
        if let Some(column) = tab.ghost_view() {
            println!("bench chrome_keys carets_before={}", column.has_carets());
            bench_chrome_key(&app, &view, "carets", "Down").await;
            println!("bench chrome_keys carets_after={}", column.has_carets());
            column.clear_carets();
        }
        // The popup, as `keys::bench_popup` raises it, and Down inside it.
        let completion = sourceview5::prelude::ViewExt::completion(&tab.view);
        let words = sourceview5::CompletionWords::new(None);
        sourceview5::prelude::CompletionWordsExt::register(&words, &tab.buffer);
        completion.add_provider(&words);
        tab.set_text("completion\n- comp");
        tab.buffer.place_cursor(&tab.buffer.end_iter());
        completion.show();
        glib::timeout_future(Duration::from_millis(800)).await;
        println!("bench chrome_keys popup_up={}", tab.popup_shown());
        bench_chrome_key(&app, &view, "popup", "Down").await;
        completion.hide();
        completion.remove_provider(&words);
        tab.set_text(&own);
        if let Some(tree) = app.tree.get() {
            let list = tree.view().clone().upcast::<gtk::Widget>();
            bench_chrome_key(&app, &list, "tree", "Down").await;
        }
        app.set_mode(Mode::Split);
        glib::timeout_future(Duration::from_millis(800)).await;
        let preview = app.preview.borrow().as_ref().map(|p| p.widget().clone());
        if let Some(preview) = preview {
            for (case, chord) in [("preview_page", "Page_Down"), ("preview_space", "space")] {
                bench_chrome_key(&app, &preview, case, chord).await;
            }
        }
        app.set_mode(Mode::Editor);
        app.open_path(&pdf);
        glib::timeout_future(Duration::from_millis(1500)).await;
        if let Some(reader) = app.active_pdf().map(|pdf| pdf.key_target()) {
            for (case, chord) in [
                ("pdf_page", "Page_Down"),
                ("pdf_space", "space"),
                ("pdf_n", "n"),
            ] {
                bench_chrome_key(&app, &reader, case, chord).await;
            }
            app.set_presenting(true);
            glib::timeout_future(Duration::from_millis(500)).await;
            bench_chrome_key(&app, &reader, "presenting", "space").await;
            app.set_presenting(false);
        }
        bench_quit(&app);
    });
}

/// One case of [`bench_chrome_keys`]: the chrome shown and the keyboard on `target`, then the
/// press asked for and what it did to the chrome.
async fn bench_chrome_key(app: &Rc<App>, target: &gtk::Widget, case: &str, chord: &str) {
    target.grab_focus();
    app.show_chrome();
    glib::timeout_future(Duration::from_millis(200)).await;
    println!("bench chrome_key_ready {case} {chord}");
    glib::timeout_future(Duration::from_millis(1200)).await;
    let focus = gtk::prelude::GtkWindowExt::focus(&app.window);
    println!(
        "bench chrome_key {case} {chord} focused={} hidden={} header_class={}",
        focus.is_some_and(|f| &f == target || f.is_ancestor(target)),
        app.chrome_hidden.get(),
        app.header.has_css_class("chrome-hidden")
    );
}

/// Focus mode and the find bar over two panes, at High, through real input
/// (`build-aux/xtest.py`, spawned per step): `<relA>` on the left, `<relB>` on the right with
/// `<relC>` behind it. A find bar opened on the left and a query typed into it, a letter typed on
/// the right, one on the left; then two ways the right pane becomes the active one with the
/// keyboard left behind on the left: `<relB>`'s tab picked in the right pane's bar, and, after a
/// click on each side, `<relB>`'s file deleted, which closes its tab and brings `<relC>` to the
/// front behind the reader's back. After each, a click into the left note, a letter typed and
/// `Ctrl+F`. It prints which pane is active, which has the keyboard, which recede, whether the
/// left note's text fades and which find bar is open: the pane typed in is the one that stays,
/// and `Ctrl+F` opens its bar. It types into the notes and deletes `<relB>`, so point it at a
/// scratch vault.
pub(super) fn bench_chrome_find(app: &Rc<App>, rels: &str) {
    scratch_only(app, "ACCENT_BENCH_CHROME=find:");
    let [a, b, c] = rels.split(',').collect::<Vec<_>>()[..] else {
        return bench_quit(app);
    };
    let (app, a, b, c) = (app.clone(), a.to_string(), b.to_string(), c.to_string());
    glib::spawn_future_local(async move {
        for (rel, split) in [(&a, false), (&b, true), (&c, false)] {
            app.open_path(rel);
            glib::timeout_future(Duration::from_millis(400)).await;
            if split {
                let _ = WidgetExt::activate_action(&app.window, "win.split-right", None);
                glib::timeout_future(Duration::from_millis(400)).await;
            }
        }
        let (Some(tab_a), Some(tab_b)) = (app.tab_for(&a), app.tab_for(&b)) else {
            println!("bench chrome_find no_tabs");
            return bench_quit(&app);
        };
        let (Some(left), Some(right)) = (app.pane_of(&tab_a.page), app.pane_of(&tab_b.page)) else {
            return bench_quit(&app);
        };
        app.config.borrow_mut().focus_mode = FocusMode::High;
        let middle = |w: &gtk::Widget| {
            let p = graphene::Point::new(w.width() as f32 / 2.0, w.height() as f32 / 2.0);
            let p = w.compute_point(&app.window, &p).unwrap_or(p);
            format!("{:.0} {:.0}", p.x(), p.y())
        };
        let state = |step: &str| {
            let side = |pane: &Rc<Pane>| match Rc::ptr_eq(pane, &left) {
                true => "left",
                false => "right",
            };
            let focus = gtk::prelude::GtkWindowExt::focus(&app.window);
            let keyboard = [&left, &right]
                .into_iter()
                .find(|pane| focus.as_ref().is_some_and(|f| f.is_ancestor(pane.widget())))
                .map_or("none", side);
            println!(
                "bench chrome_find {step} active={} keyboard={keyboard} away_left={} \
                 away_right={} fade_left={} find_left={} find_right={}",
                side(&app.pane()),
                left.widget().has_css_class("chrome-away"),
                right.widget().has_css_class("chrome-away"),
                tab_a.ghost_view().is_some_and(|v| v.fading()),
                left.find.is_open(),
                right.find.is_open(),
            );
        };
        let (va, vb) = (
            middle(tab_a.view.upcast_ref()),
            middle(tab_b.view.upcast_ref()),
        );
        let name = crate::doc::file_name(&b).to_string();
        let tab_label = find_widget(right.bar.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.label() == name)
        })
        .map(|label| middle(&label))
        .unwrap_or_default();
        // A pointer that moves brings the chrome back, so each letter typed fades it afresh.
        let type_left = format!("move 5 5; move {va}; down; up; type z");
        for (step, steps) in [
            (
                "find_left",
                format!("move {va}; focus; down; up; key ctrl+f; type the"),
            ),
            ("type_right", format!("move {vb}; down; up; type x")),
            ("type_left", format!("move {va}; down; up; type y")),
            ("pick_right_tab", format!("move {tab_label}; down; up")),
            ("type_left_after_pick", type_left.clone()),
            ("find_after_pick", "key Escape; key ctrl+f".into()),
            (
                "back_left",
                format!("key Escape; move {vb}; down; up; move {va}; down; up"),
            ),
        ] {
            super::git::xtest(&steps).await;
            state(step);
        }
        // Deleted behind the reader's back, as a sync or a checkout does.
        let _ = std::fs::remove_file(app.root().join(&b));
        glib::timeout_future(Duration::from_millis(1500)).await;
        state("closed_behind");
        super::git::xtest(&type_left).await;
        state("type_left_after_close");
        super::git::xtest("key ctrl+f").await;
        state("find_after_close");
        bench_quit(&app);
    });
}
