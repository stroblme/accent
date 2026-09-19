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
