//! Drills over the suggestions a note gets as it is typed: the word popup and ghost text
//! (`ACCENT_BENCH_SUGGEST`).

use super::*;

/// Where an Escape the suggestion does not take would land: the find bar, or the comparison the
/// tab is hosting.
#[derive(Clone, Copy)]
enum Under {
    Find,
    Compare,
}

/// `=escape:<rel>`: Escape over a word popup and over painted ghost text, through real XTEST
/// presses, with the find bar open and with the note compared with its disk copy. For each case
/// it prints `bench suggest_ready <case> <steps>`, steps for `build-aux/xtest.py :N "<steps>"` (a
/// word typed for the popup, then Escape), and after the Escape whether the popup, the ghost, the
/// find bar and the comparison are still up; then a second Escape, which has to reach the bar or
/// the comparison. It first prints `bench suggest focus_window` and waits for the window to have
/// the X input focus (`xtest.py :N "move 700 400; focus"`), without which no popup shows.
pub(super) fn bench_suggest_escape(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        println!("bench suggest focus_window");
        for _ in 0..100 {
            if app.window.is_active() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let own = tab.text();
        for under in [Under::Find, Under::Compare] {
            for popup in [true, false] {
                bench_escape_case(&app, &tab, under, popup).await;
            }
        }
        tab.leave_compare();
        tab.set_text(&own);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench suggest write_failed {e}");
        }
        bench_quit(&app);
    });
}

/// One case of [`bench_suggest_escape`]: the bar or the comparison put up under a fresh note, the
/// popup or the ghost raised at the end of its last line, and two Escapes.
async fn bench_escape_case(app: &Rc<App>, tab: &Rc<Tab>, under: Under, popup: bool) {
    let case = format!(
        "{}_{}",
        match under {
            Under::Find => "find",
            Under::Compare => "compare",
        },
        if popup { "popup" } else { "ghost" }
    );
    let find = app.pane().find.clone();
    find.close();
    tab.leave_compare();
    // A toast takes Escape for itself (`AdwToastOverlay`), ahead of the window: none may stand.
    app.toasts.dismiss_all();
    // The words the popup offers, then the line the caret ends on.
    tab.set_text("theorem theory thermal\nwritten on disk\n");
    match under {
        Under::Find => find.open(crate::find::Mode::Find),
        Under::Compare => {
            if let Err(e) = app.write_tab(tab, None) {
                println!("bench suggest write_failed {e}");
            }
            tab.buffer
                .insert(&mut tab.buffer.end_iter(), "typed since\n");
            app.compare_with_disk(tab);
        }
    }
    glib::timeout_future(Duration::from_millis(600)).await;
    tab.view.grab_focus();
    tab.buffer.place_cursor(&tab.buffer.end_iter());
    let steps = match popup {
        true => "type th; sleep 1; key Escape",
        false => {
            if let Some(view) = tab.ghost_view() {
                view.set_ghost(Some("eorem and more".to_string()));
            }
            "key Escape"
        }
    };
    let ghost = || tab.ghost_view().is_some_and(|v| v.ghost().is_some());
    // What stands just before the Escape, read while the steps run.
    let (seen_popup, seen_ghost) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(ghost())));
    let watch = glib::timeout_add_local(
        Duration::from_millis(20),
        glib::clone!(
            #[strong]
            tab,
            #[strong]
            seen_popup,
            move || {
                seen_popup.set(seen_popup.get() || tab.popup_shown());
                glib::ControlFlow::Continue
            }
        ),
    );
    println!("bench suggest_ready {case} {steps}");
    glib::timeout_future(Duration::from_millis(if popup { 2200 } else { 800 })).await;
    watch.remove();
    println!(
        "bench suggest_escape {case} before popup={} ghost={} after popup={} ghost={} find_open={} comparing={}",
        seen_popup.get(),
        seen_ghost.get(),
        tab.popup_shown(),
        ghost(),
        find.is_open(),
        tab.comparison().is_some()
    );
    println!("bench suggest_ready {case}_again key Escape");
    glib::timeout_future(Duration::from_millis(800)).await;
    println!(
        "bench suggest_escape {case}_again find_open={} comparing={}",
        find.is_open(),
        tab.comparison().is_some()
    );
}
