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
            for accel in [
                "<Shift><Alt>Left",
                "<Shift><Alt>Right",
                "<Control><Alt>Left",
                "<Control><Alt>Up",
            ] {
                println!("bench accel {accel} {:?}", gtk_app.actions_for_accel(accel));
            }
        }
        return bench_dividers(app);
    };
    println!("bench step {action}");
    let _ = WidgetExt::activate_action(&app.window, action, None);
    let (app, page) = (app.clone(), page.clone());
    glib::timeout_add_local_once(Duration::from_millis(200), move || {
        bench_pane_at(&app, &page);
        bench_pane_step(&app, &page, step + 1);
    });
}

/// Move Divider over the split the steps above leave, from where a drag left it at 47 %: a step
/// each way, which lands on the grid anchored at the centre; a step along the axis no split runs,
/// which moves nothing; and a run of steps into the end of the range, which stops at what the
/// pane's minimum width allows.
fn bench_dividers(app: &Rc<App>) {
    let Some(paned) = app.pane().widget().parent().and_downcast::<gtk::Paned>() else {
        println!("bench divider none");
        return bench_quit(app);
    };
    let extent = paned.width();
    paned.set_position(extent * 47 / 100);
    let say = |what: &str| {
        let share = f64::from(paned.position()) / f64::from(extent);
        println!("bench divider {what} {share:.3}");
    };
    say("dragged");
    for side in ["right", "left", "left", "up"] {
        let _ = WidgetExt::activate_action(&app.window, &format!("win.divider-{side}"), None);
        say(side);
    }
    for _ in 0..30 {
        let _ = WidgetExt::activate_action(&app.window, "win.divider-left", None);
    }
    say("left_x30");
    println!("bench divider min {}", paned.min_position());
    bench_quit(app);
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
    let mut rels: Vec<String> = arg.split(',').map(str::to_string).collect();
    // A shell in front of the pane the reader was in at the close: the session writes it like a
    // file, into that pane and as the active tab, and the restore starts it there again.
    let shell = rels.last().is_some_and(|last| last == "shell");
    if shell {
        rels.pop();
    }
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
            if shell {
                // In the left pane, which is not the one whose first tab lands last: what the
                // session writes as the active tab, here the shell, is the whole of what brings
                // the restore back to the pane the reader was in.
                app.reveal_page(&a);
                app.open_terminal();
            }
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                bench_layout_print(&app);
                if let Some(gtk_app) = app.window.application() {
                    gtk_app.activate_action("quit", None);
                }
            });
        });
    });
}

/// See `ACCENT_BENCH_COLLAPSE` in `install_bench_hooks`.
pub(super) fn bench_collapse(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let step = |what: &str| {
            println!(
                "bench collapse {what} width={} sidebar={} divider={} saved={:?}",
                app.window.width(),
                app.sidebar_column.is_visible(),
                app.split.position(),
                app.sidebar_saved()
            )
        };
        let settle = || glib::timeout_future(Duration::from_millis(500));
        let f9 = || WidgetExt::activate_action(&app.window, "win.sidebar", None);
        // A dragged divider, which the round trip must leave where it is.
        app.split.set_position(400);
        settle().await;
        step("wide");
        for hidden in [false, true] {
            if hidden {
                let _ = f9();
                settle().await;
                step("wide_hidden");
            }
            // Narrower than the dragged sidebar, which `GtkPaned` then clamps.
            app.window.set_default_size(360, 700);
            settle().await;
            step("narrow");
            if !hidden {
                let _ = f9();
                settle().await;
                step("narrow_f9");
                let _ = f9();
                settle().await;
                step("narrow_f9_again");
            }
            app.window.set_default_size(1100, 760);
            settle().await;
            step("wide_again");
        }
        bench_quit(&app);
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
        if !hooked && app.restored.get() && &*how == "open" {
            hooked = true;
            // Before any tab has landed, so the pane the restore made active is still empty: the
            // note a reader opens while the window is coming back.
            let landing = app.awaiting.borrow().len();
            app.open_path(&rel);
            done.set(true);
            println!("bench layout_open {rel} with {landing} still landing");
        }
        if !hooked && app.restored.get() {
            hooked = true;
            let keys: Vec<String> = app.awaiting.borrow().keys().cloned().collect();
            for key in keys {
                let Some(restore) = app.awaiting.borrow_mut().remove(&key) else {
                    continue;
                };
                let (how, rel, done) = (how.clone(), rel.clone(), done.clone());
                let Waiting { what, run } = restore;
                let pick: Work = Box::new(move |app, tab| {
                    run(app, tab);
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
                app.awaiting
                    .borrow_mut()
                    .insert(key, Waiting { what, run: pick });
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
    println!(
        "bench layout_stored_active {}",
        app.restorable_active().unwrap_or_default()
    );
    // Back/forward per pane, in the order the panes were made: a restore must write none of its
    // own landings or re-selections into them.
    let history: Vec<String> = app
        .panes
        .borrow()
        .iter()
        .map(|pane| {
            let (back, forward) = pane.nav.borrow().depth();
            format!("{back}/{forward}")
        })
        .collect();
    println!("bench layout_history {}", history.join(" "));
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
            // What the tab menu offers over each kind of tab open right now: Rename and Move to
            // Trash are a vault file's alone, so a shell's menu is two sections shorter.
            for doc in app.docs() {
                println!(
                    "bench tab_menu {} items={}",
                    doc.key(),
                    bench_tab_menu_items(&app, Some(doc.page()))
                );
            }
            println!(
                "bench tab_menu_closed items={}",
                bench_tab_menu_items(&app, None)
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

/// One step of [`bench_pin`], named for its printout.
type PinStep = (&'static str, Box<dyn Fn(&Rc<App>, &[adw::TabPage])>);

/// See `ACCENT_BENCH_TABS=pin:` above.
pub(super) fn bench_pin(app: &Rc<App>, rels: &str) {
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    if rels.len() != 4 {
        return bench_quit(app);
    }
    for rel in &rels {
        app.open_path(rel);
    }
    let steps: Vec<PinStep> = vec![
        ("open", Box::new(|_, _| {})),
        (
            "pin_c_menu",
            Box::new(|app, p| bench_pin_menu(app, &p[2], "win.pin-tab")),
        ),
        (
            "pin_b_palette",
            Box::new(|app, p| {
                app.reveal_page(&p[1]);
                let _ = WidgetExt::activate_action(&app.window, "win.pin-tab", None);
            }),
        ),
        (
            "unpin_c_menu",
            Box::new(|app, p| bench_pin_menu(app, &p[2], "win.unpin-tab")),
        ),
        (
            "pin_c_palette",
            Box::new(|app, p| {
                app.reveal_page(&p[2]);
                let _ = WidgetExt::activate_action(&app.window, "win.pin-tab", None);
            }),
        ),
        // Where a drag along the bar ends: the tab view told to put the page there.
        (
            "drag_a_first",
            Box::new(|app, p| {
                app.tabs().reorder_page(&p[0], 0);
            }),
        ),
        (
            "drag_b_last",
            Box::new(|app, p| {
                app.tabs().reorder_page(&p[1], 3);
            }),
        ),
        (
            "move_d_right",
            Box::new(|app, p| {
                app.reveal_page(&p[3]);
                let _ = WidgetExt::activate_action(&app.window, "win.move-tab-right", None);
            }),
        ),
        (
            "move_c_right",
            Box::new(|app, p| {
                app.reveal_page(&p[2]);
                let _ = WidgetExt::activate_action(&app.window, "win.move-tab-right", None);
            }),
        ),
    ];
    let landed = {
        let (app, rels) = (app.clone(), rels.clone());
        move || rels.iter().all(|rel| app.doc_for(rel).is_some())
    };
    let app = app.clone();
    bench_layout_when(landed, move || {
        let pages: Vec<adw::TabPage> = rels
            .iter()
            .filter_map(|rel| app.doc_for(rel).map(|doc| doc.page().clone()))
            .collect();
        bench_pin_step(&app, Rc::new(steps), Rc::new(pages), 0);
    });
}

/// Run step `i` and print the panes once the holds it queued have run, then the next one.
fn bench_pin_step(app: &Rc<App>, steps: Rc<Vec<PinStep>>, pages: Rc<Vec<adw::TabPage>>, i: usize) {
    let Some((what, step)) = steps.get(i) else {
        println!("bench pinned_tab {}", bench_tab_line(&pages[2]));
        if let Some(gtk_app) = app.window.application() {
            gtk_app.activate_action("quit", None);
        }
        return;
    };
    let what = *what;
    step(app, &pages);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(300), move || {
        println!("bench pins {what} {}", bench_pins_line(&app));
        bench_pin_step(&app, steps, pages, i + 1);
    });
}

/// Open `page`'s tab menu, print the pin item it offers, and take it.
fn bench_pin_menu(app: &Rc<App>, page: &adw::TabPage, action: &str) {
    let Some(tabs) = app.pane_of(page).map(|pane| pane.tabs.clone()) else {
        return;
    };
    tabs.emit_by_name::<()>("setup-menu", &[&Some(page)]);
    let offered = tabs
        .menu_model()
        .and_then(|menu| menu.item_link(1, "section"))
        .and_then(|moves| moves.item_attribute_value(moves.n_items() - 1, "action", None))
        .and_then(|action| action.get::<String>());
    println!("bench pin_menu offers {offered:?}");
    let _ = WidgetExt::activate_action(&app.window, action, None);
    tabs.emit_by_name::<()>("setup-menu", &[&None::<adw::TabPage>]);
}

/// Every pane's tabs in bar order, pinned ones marked `^`, panes in the order they were made.
fn bench_pins_line(app: &Rc<App>) -> String {
    let panes: Vec<String> = app
        .panes
        .borrow()
        .iter()
        .map(|pane| {
            let tabs: Vec<String> = pane
                .pages()
                .iter()
                .map(|page| {
                    let key = app.doc_for_page(page).map(|d| d.key()).unwrap_or_default();
                    match app.is_pinned(page) {
                        true => format!("^{key}"),
                        false => key,
                    }
                })
                .collect();
            format!("[{}]", tabs.join(" "))
        })
        .collect();
    panes.join(" ")
}

/// See `ACCENT_BENCH_TABS=pins` above: the panes once every restored tab has landed.
pub(super) fn bench_pins_restored(app: &Rc<App>) {
    let (app, landing) = (app.clone(), app.clone());
    let landed = move || landing.restored.get() && landing.awaiting.borrow().is_empty();
    bench_layout_when(landed, move || {
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            println!("bench pins restored {}", bench_pins_line(&app));
            let pinned = app.pinned.borrow().clone();
            for page in &pinned {
                println!("bench pinned_tab {}", bench_tab_line(page));
            }
            bench_quit(&app);
        });
    });
}

/// See `ACCENT_BENCH_TABS=pinwin:` above. A drop in another window ends with the page attached to
/// one of that window's tab views, which `transfer_page` does here; what follows is the receiving
/// window's own adoption (`Shell::landed`), which reopens the file there.
pub(super) fn bench_pin_window(app: &Rc<App>, rels: &str) {
    // A remote vault opens nothing before its host has answered.
    if !app.restored.get() {
        let (waiting, app, rels) = (app.clone(), app.clone(), rels.to_string());
        return bench_layout_when(
            move || waiting.restored.get(),
            move || bench_pin_window(&app, &rels),
        );
    }
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    if rels.len() != 3 {
        return bench_quit(app);
    }
    for rel in &rels {
        app.open_path(rel);
    }
    let landed = {
        let (app, rels) = (app.clone(), rels.clone());
        move || rels.iter().all(|rel| app.doc_for(rel).is_some())
    };
    let app = app.clone();
    bench_layout_when(landed, move || {
        let page = |rel: &str| app.doc_for(rel).map(|doc| doc.page().clone());
        let (Some(a), Some(b)) = (page(&rels[0]), page(&rels[1])) else {
            return bench_quit(&app);
        };
        app.set_pinned(&a, true);
        let gtk_app = app.window.application().and_downcast::<adw::Application>();
        let other = app
            .shell
            .upgrade()
            .zip(gtk_app)
            .and_then(|(shell, gtk_app)| {
                shell.loose_window(&gtk_app, crate::shell::Loose::Documents)
            });
        let Some(other) = other else {
            return bench_quit(&app);
        };
        bench_pin_windows("pinned", &app, &other);
        // The plain one first, so the pinned one has a tab there to land in front of.
        bench_hand_over(&app, &other, &b);
        glib::timeout_add_local_once(Duration::from_millis(800), move || {
            bench_pin_windows("moved_b", &app, &other);
            bench_hand_over(&app, &other, &a);
            glib::timeout_add_local_once(Duration::from_millis(800), move || {
                bench_pin_windows("moved_a", &app, &other);
                if let Some(page) = other.pinned.borrow().first() {
                    println!("bench pinned_tab {}", bench_tab_line(page));
                }
                bench_quit(&app);
            });
        });
    });
}

/// Both windows' tabs, pinned ones marked `^`.
fn bench_pin_windows(what: &str, here: &Rc<App>, there: &Rc<App>) {
    println!(
        "bench pinwin {what} here {} there {}",
        bench_pins_line(here),
        bench_pins_line(there)
    );
}

/// Attach `page` to the end of `to`'s active pane, as a tab let go on its bar is.
fn bench_hand_over(from: &Rc<App>, to: &Rc<App>, page: &adw::TabPage) {
    if let Some(pane) = from.pane_of(page) {
        let tabs = to.tabs();
        pane.tabs.transfer_page(page, &tabs, tabs.n_pages());
    }
}

/// How many items a pane's tab menu holds once it has been told which page is about to show it.
///
/// `setup-menu` is emitted by hand: there is no pointer headless, so this runs the handler
/// `wire_pane` connected without `AdwTabBar`'s own popup around it. `None` is what the bar sends
/// once the menu has closed again.
fn bench_tab_menu_items(app: &Rc<App>, page: Option<&adw::TabPage>) -> i32 {
    let tabs = &app.pane().tabs;
    tabs.emit_by_name::<()>("setup-menu", &[&page]);
    tabs.menu_model().map_or(0, |menu| menu.n_items())
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
