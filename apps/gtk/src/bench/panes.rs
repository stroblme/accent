//! Drills over panes and tabs: moving a tab, the session's layout, and what a tab switch leaves.

use super::*;

/// Open the two notes `rels` names in one pane, then split the second one off to the right, move
/// it back, and ask for a move where there is no pane to move into.
///
/// What is printed is the **geometry** of the pane holding that tab, not its index: panes are
/// kept in the order they were made, which is not the order they are drawn in, so only the
/// rectangle says a tab really changed side.
pub(super) fn bench_panes(app: &Rc<App>, rels: &str) {
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

/// See `ACCENT_BENCH_LAYOUT` above. Each step waits for the one before it to be laid out: a split
/// has no size to put a handle in until it is allocated, and a tab moved into a split takes the
/// keyboard, and with it the active pane, from an idle.
pub(super) fn bench_layout(app: &Rc<App>, arg: &str) {
    // A remote vault restores, and opens anything at all, once its host has answered.
    if !app.restored.get() {
        return bench_layout_waiting(app, arg);
    }
    if arg == "1" {
        let (app, landing) = (app.clone(), app.clone());
        let landed = move || landing.awaiting.borrow().is_empty();
        bench_layout_when(landed, move || {
            glib::timeout_add_local_once(Duration::from_millis(1000), move || {
                bench_layout_print(&app);
                bench_quit(&app);
            });
        });
        return;
    }
    let rels: Vec<String> = arg.split(',').map(str::to_string).collect();
    let [a, b, c, d] = &rels[..] else {
        return bench_quit(app);
    };
    for rel in [a, b, c, d] {
        app.open_path(rel);
    }
    let landed = {
        let (app, rels) = (app.clone(), rels.clone());
        move || rels.iter().all(|rel| app.doc_for(rel).is_some())
    };
    let (app, a, c, d) = (app.clone(), a.clone(), c.clone(), d.clone());
    bench_layout_when(landed, move || {
        let page = |rel: &str| app.doc_for(rel).map(|doc| doc.page().clone());
        let (Some(a), Some(c), Some(d)) = (page(&a), page(&c), page(&d)) else {
            return bench_quit(&app);
        };
        app.split_page(&app.pane(), Side::Right, &c);
        let Some(right) = app.pane_of(&c) else {
            return bench_quit(&app);
        };
        app.split_page(&right, Side::Down, &d);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            app.reveal_page(&a);
            app.reveal_page(&c);
            let outer = app
                .content
                .child_by_name("tabs")
                .and_downcast::<adw::Bin>()
                .and_then(|bin| bin.child())
                .and_downcast::<gtk::Paned>();
            let inner = outer
                .as_ref()
                .and_then(|paned| paned.end_child())
                .and_downcast::<gtk::Paned>();
            let (Some(outer), Some(inner)) = (outer, inner) else {
                return bench_quit(&app);
            };
            outer.set_position(outer.width() * 3 / 10);
            inner.set_position(inner.height() * 6 / 10);
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                bench_layout_print(&app);
                if let Some(gtk_app) = app.window.application() {
                    gtk_app.activate_action("quit", None);
                }
            });
        });
    });
}

/// Run `then` once `ready` holds, looked at every 50 ms: on a remote vault a read is a round trip
/// away, so the layout drill waits for what it needs rather than for a set time.
fn bench_layout_when(ready: impl Fn() -> bool + 'static, then: impl FnOnce() + 'static) {
    let mut then = Some(then);
    glib::timeout_add_local(Duration::from_millis(50), move || {
        if !ready() {
            return glib::ControlFlow::Continue;
        }
        if let Some(then) = then.take() {
            then();
        }
        glib::ControlFlow::Break
    });
}

/// What a window that has not restored yet shows, printed each time it changes, until the restore
/// runs and the drill goes on; `=quit` quits there instead.
fn bench_layout_waiting(app: &Rc<App>, arg: &str) {
    let (app, arg) = (app.clone(), arg.to_string());
    let mut said = String::new();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        if app.restored.get() {
            bench_layout(&app, &arg);
            return glib::ControlFlow::Break;
        }
        let page = app
            .content
            .child_by_name("empty")
            .and_downcast::<adw::StatusPage>();
        let shows = format!(
            "{} {:?} {:?}",
            app.content.visible_child_name().unwrap_or_default(),
            page.as_ref().map(|p| p.title()).unwrap_or_default(),
            page.and_then(|p| p.description()).unwrap_or_default()
        );
        if shows != said {
            println!("bench layout_waiting {shows}");
            said = shows;
        }
        if arg == "quit"
            && let Some(gtk_app) = app.window.application()
        {
            gtk_app.activate_action("quit", None);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// See `ACCENT_BENCH_LAYOUT` above. However fast the reads are, the pick comes between two tabs
/// landing: the restore's work waiting on each tab is wrapped before any lands, and the first
/// landing after which `<rel>` is in its pane — behind another tab to be picked, in front to be
/// given the keyboard — with a tab still to come does it.
pub(super) fn bench_layout_pick(app: &Rc<App>, arg: &str) {
    let Some((how, rel)) = arg.split_once(':') else {
        return bench_quit(app);
    };
    let (app, how, rel) = (app.clone(), Rc::<str>::from(how), Rc::<str>::from(rel));
    let done = Rc::new(Cell::new(false));
    let mut hooked = false;
    // From before the restore, every millisecond: the first tick after it runs ahead of the reads.
    glib::timeout_add_local(Duration::from_millis(1), move || {
        if !hooked && app.restored.get() {
            hooked = true;
            let keys: Vec<String> = app.awaiting.borrow().keys().cloned().collect();
            for key in keys {
                let Some(restore) = app.awaiting.borrow_mut().remove(&key) else {
                    continue;
                };
                let (how, rel, done) = (how.clone(), rel.clone(), done.clone());
                let pick: Waiting = Box::new(move |app, tab| {
                    restore(app, tab);
                    let landing = app.awaiting.borrow().len();
                    let Some(tab) = app.tab_for(&rel).filter(|_| !done.get() && landing > 0) else {
                        return;
                    };
                    let Some(pane) = app.pane_of(&tab.page) else {
                        return;
                    };
                    let front = pane.tabs.selected_page().as_ref() == Some(&tab.page);
                    match (&*how, front) {
                        ("focus", true) => {
                            tab.view.grab_focus();
                        }
                        ("pick", false) => pane.tabs.set_selected_page(&tab.page),
                        _ => return,
                    }
                    done.set(true);
                    println!("bench layout_{how} {rel} with {landing} still landing");
                });
                app.awaiting.borrow_mut().insert(key, pick);
            }
        }
        if !app.restored.get() || !app.awaiting.borrow().is_empty() {
            return glib::ControlFlow::Continue;
        }
        if !done.get() {
            println!("bench layout_{how} {rel} missed");
        }
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            bench_layout_print(&app);
            bench_quit(&app);
        });
        glib::ControlFlow::Break
    });
}

/// The tree the session would write, the tab notes open next to, and how many panes there are.
fn bench_layout_print(app: &Rc<App>) {
    let tree = app
        .layout()
        .map_or_else(|| "none".to_string(), |layout| bench_layout_line(&layout));
    println!("bench layout {tree}");
    println!(
        "bench layout_active {}",
        app.active_key().unwrap_or_default()
    );
    println!("bench layout_panes {}", app.panes.borrow().len());
}

/// `(h 0.300 [a.md b.md *a.md] (v 0.600 [c.md *c.md] [d.md *d.md]))`: each split's axis and
/// share, and each pane's tabs with the one in front.
fn bench_layout_line(layout: &Layout) -> String {
    match layout {
        Layout::Pane { tabs, selected } => {
            format!(
                "[{} *{}]",
                tabs.join(" "),
                selected.as_deref().unwrap_or("-")
            )
        }
        Layout::Split {
            vertical,
            ratio,
            start,
            end,
        } => format!(
            "({} {ratio:.3} {} {})",
            if *vertical { "v" } else { "h" },
            bench_layout_line(start),
            bench_layout_line(end)
        ),
    }
}

/// The find bar and the Outline pane across a tab switch and a close: a note with the bar open, a
/// shell in front of it, the chord over that shell, back to the note, then a PDF, then every tab
/// closed. One line per step, so what a switch and the last close leave behind is a printout
/// rather than an argument.
pub(super) fn bench_tabs(app: &Rc<App>, rels: &str) {
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
