//! Wiring a built window: the signal handlers on a pane's tab view, the window itself and the
//! file tree.

use super::*;

/// Everything one pane's tab view has to answer for. Called for the pane the window is built with
/// and for every pane a split adds, so a new pane behaves exactly like the first one.
pub fn wire_pane(app: &Rc<App>, pane: &Rc<Pane>) {
    // While presenting there is no editor on screen, so find and go to line address the rendered
    // preview instead. Three closures rather than a back-reference, so `find.rs` never sees `App`,
    // and each of them answers for *this* pane: the other pane's PDF must not decide whether this
    // bar counts in pages.
    pane.find.wire(find::Wiring {
        presenting: Box::new(glib::clone!(
            #[weak]
            app,
            #[weak]
            pane,
            #[upgrade_or]
            false,
            // A PDF answers find and go-to itself, whether or not anything is being presented.
            move || app.presenting.get().is_some() || app.pdf_of(&pane).is_some()
        )),
        preview: Box::new(glib::clone!(
            #[weak]
            app,
            #[weak]
            pane,
            move |op| app.preview_find(&pane, op)
        )),
        pages: Box::new(glib::clone!(
            #[weak]
            app,
            #[weak]
            pane,
            #[upgrade_or]
            None,
            move || {
                app.pdf_of(&pane)
                    .map(|pdf| pdf.page_count())
                    .or_else(|| app.diagram_of(&pane).map(|d| d.page_count()))
            }
        )),
        mark: Box::new(glib::clone!(
            #[weak]
            app,
            #[weak]
            pane,
            move || {
                if let Some(here) = app.here(&pane) {
                    app.record(&pane, here);
                }
            }
        )),
    });
    // A tab may only go once its buffer is on disk. When the save fails the close stops here and
    // `AdwTabView` waits for `close_page_finish`, which the dialog calls with the user's answer.
    pane.tabs.connect_close_page(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |tabs, page| {
            let dirty = app
                .open_tabs()
                .into_iter()
                .find(|t| &t.page == page)
                .filter(|t| t.save.modified.get());
            if let Some(tab) = dirty
                && let Err(e) = app.flush_tab(&tab)
            {
                let (tabs, page) = (tabs.clone(), page.clone());
                app.ask_unsaved(&tab, &e, move |app, close| {
                    if close {
                        // Only once the answer is in: a cancelled close must not have moved the
                        // selection off the tab it kept.
                        app.select_survivor(&page);
                        app.forget_page(&page);
                    }
                    tabs.close_page_finish(&page, close);
                });
                return glib::Propagation::Stop;
            }
            // A diagram is the same, through its own save; a label still being typed is part of
            // what it holds, so it goes into the cell first.
            let closing = app.diagrams().into_iter().find(|d| &d.page == page);
            if let Some(diagram) = &closing {
                diagram.finish_label();
            }
            let dirty = closing.filter(|d| d.save.modified.get());
            if let Some(diagram) = dirty
                && let Err(e) = app.flush_diagram(&diagram)
            {
                let (tabs, page) = (tabs.clone(), page.clone());
                app.ask_unsaved_diagram(&diagram, &e, move |app, close| {
                    if close {
                        app.select_survivor(&page);
                        app.forget_page(&page);
                    }
                    tabs.close_page_finish(&page, close);
                });
                return glib::Propagation::Stop;
            }
            app.select_survivor(page);
            app.forget_page(page);
            glib::Propagation::Proceed
        }
    ));
    // `setup-menu` fires with the page just before the popup and with `None` from an idle after
    // it hides. A model button activates its action before the popdown, so the page is still here
    // when the action runs, and afterwards `menu_rel` falls back to the active tab.
    pane.tabs.connect_setup_menu(glib::clone!(
        #[weak]
        app,
        move |tabs, page| {
            *app.menu_page.borrow_mut() = page.cloned();
            // Filled for the page that is about to show it: a `GtkPopoverMenu` follows the model
            // it was built from, so this is what puts Rename and Move to Trash on a vault file's
            // tab and on no other. One model per pane, which is the one asked for here.
            if let Some(menu) = tabs.menu_model().and_downcast::<gio::Menu>() {
                actions::fill_tab_menu(&menu, app.menu_file().is_some());
            }
        }
    ));
    pane.tabs.connect_selected_page_notify(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        move |tabs| {
            if let Some(page) = tabs.selected_page() {
                // The tab being left is still the front of the MRU order until `touch` runs, and
                // it still holds its caret, so this is where the reader was.
                if let Some(left) = pane.recent().first().filter(|p| **p != page) {
                    app.mark_page(left);
                }
                pane.touch(&page);
                // A page with no document yet is still being opened, and was selected by being
                // the first one added; any other is a tab someone picked.
                if app.doc_for_page(&page).is_some() {
                    app.reader_in(&pane);
                }
            }
            // This pane's bar, whether or not this pane has the keyboard: a tab dragged out of a
            // background pane must not leave that pane's bar holding a tab it no longer has.
            app.retarget_find(&pane);
            app.set_active_pane(&pane);
            app.sync_active();
            app.save_session_soon();
        }
    ));
    // Double-clicking a tab is what makes a preview tab a real one, which is VS Code's gesture
    // for it. A pane holding one tab hides its bar, so there is nothing to double-click there —
    // and nothing to protect either, since a preview tab is only ever replaced by the next one.
    pane.on_tab_double_click(glib::clone!(
        #[weak]
        app,
        move |page| app.promote(page)
    ));
    // The placeholder is a property of the window, not of one pane: it shows only when no pane
    // has anything left to show, which with panes that close themselves means the last one.
    pane.tabs.connect_n_pages_notify(glib::clone!(
        #[weak]
        app,
        move |_| app.sync_panes()
    ));
    // A tab let go outside every tab bar. libadwaita reads that as "detach into a window of its
    // own" and this is the only public way to give a dragged page a view again, so a drop on one
    // of our pane zones — outside every tab bar as far as libadwaita knows — arrives here too.
    pane.tabs.connect_create_window(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        None,
        move |view| {
            let aimed = app.shell.upgrade().and_then(|shell| shell.where_to_land());
            // Let go on nothing of ours: back where it came from. A window per detached tab is a
            // gesture one-window-per-vault has no answer for, and `None` is an error here.
            Some(aimed.unwrap_or_else(|| view.clone()))
        }
    ));
    // Where a drag ends: the split the drop asked for, and the move into this window's
    // bookkeeping when the page came out of another one — libadwaita's tab bars take a foreign
    // page natively, so a note of vault A would otherwise land under window B's `docs`, session
    // and backlinks.
    pane.tabs.connect_page_attached(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        move |_, page, _| {
            if let Some(shell) = app.shell.upgrade() {
                shell.landed(&app, &pane, page);
            }
        }
    ));
    // A pane that has just lost its last page has nothing left to be. Closing it from an idle
    // rather than here, because this also fires in the middle of `transfer_page`, which is still
    // holding the page when the source view reports it gone.
    pane.tabs.connect_page_detached(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        move |tabs, _, _| {
            if tabs.n_pages() > 0 {
                return;
            }
            let app = Rc::downgrade(&app);
            glib::idle_add_local_once(move || {
                let Some(app) = app.upgrade() else { return };
                if pane.tabs.n_pages() == 0 {
                    app.close_pane(&pane);
                }
            });
        }
    ));
    // libadwaita sets this on *every* tab view when a tab drag starts anywhere, which is the only
    // notice we get that a drag is in flight and the drop sheets should go up.
    pane.tabs.connect_is_transferring_page_notify(glib::clone!(
        #[weak]
        app,
        move |tabs| app.set_drop_active(tabs.is_transferring_page())
    ));
    // Clicking into a pane's editor makes it the one a note opens into, the same as picking one
    // of its tabs would.
    let focus = gtk::EventControllerFocus::new();
    focus.connect_enter(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        move |_| {
            app.reader_in(&pane);
            if app.set_active_pane(&pane) {
                app.sync_active();
            }
        }
    ));
    pane.widget().add_controller(focus);
    wire_pane_drops(app, pane);
}

/// The drop zones on one pane: the pointer picks an edge or the middle, and letting go there
/// either splits the pane or drops into it.
fn wire_pane_drops(app: &Rc<App>, pane: &Rc<Pane>) {
    pane.drop.connect_motion(glib::clone!(
        #[weak]
        pane,
        #[upgrade_or]
        gdk::DragAction::empty(),
        move |target, x, y| {
            let (w, h) = pane.size();
            pane.show_zone(Some(panes::zone(x, y, w, h)));
            // A tab moves, a path from the tree is only read; offer whichever the drag allows.
            let offered = target
                .current_drop()
                .map(|drop| drop.actions())
                .unwrap_or_else(gdk::DragAction::empty);
            match offered.contains(gdk::DragAction::MOVE) {
                true => gdk::DragAction::MOVE,
                false => gdk::DragAction::COPY,
            }
        }
    ));
    pane.drop.connect_leave(glib::clone!(
        #[weak]
        pane,
        move |_| pane.show_zone(None)
    ));
    // A tree row let go on the bar itself opens in that pane, which is the shortest way to say
    // "over there" and the one libadwaita already draws an insertion point for.
    pane.bar
        .setup_extra_drop_target(gdk::DragAction::COPY, &[String::static_type()]);
    pane.bar.connect_extra_drag_drop(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        #[upgrade_or]
        false,
        move |_, _, value| {
            let Ok(rel) = value.get::<String>() else {
                return false;
            };
            app.set_active_pane(&pane);
            app.open_path(&rel);
            true
        }
    ));
    pane.drop.connect_drop(glib::clone!(
        #[weak]
        app,
        #[weak]
        pane,
        #[upgrade_or]
        false,
        move |_, value, x, y| {
            let (w, h) = pane.size();
            let zone = panes::zone(x, y, w, h);
            // Belt and braces: the drag is over whatever the source has to say about it, and a
            // sheet left up would swallow every click meant for the editor under it.
            app.set_drop_active(false);
            app.dropped(&pane, zone, value)
        }
    ));
}

pub fn wire_window(app: &Rc<App>) {
    // The bottom bar of an `AdwToolbarView` is a `GtkWindowHandle`, so a secondary press anywhere
    // in it asks the shell for the window menu — Restore / Minimize / Maximize / Close under a
    // footer that is one line of the document's own facts. Claim the press and do nothing with it.
    // Only button 3: dragging the window by the bar is button 1 and is left alone.
    let quiet = gtk::GestureClick::new();
    quiet.set_button(gdk::BUTTON_SECONDARY);
    quiet.connect_pressed(|gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    app.statusbar.widget().add_controller(quiet);

    // Stop / Resume beside the indexing readout. Both are round trips on a remote vault, whose
    // walk runs on the host, so neither is made on the main loop.
    //
    // Resume says so at once — a walk is starting, and a second press would post a second one.
    // Stop does not: the walk runs until the batch it is on commits, and it is still Stop until
    // the worker answers with a reconcile that says it stopped.
    app.statusbar.index_control().connect_clicked(glib::clone!(
        #[weak]
        app,
        move |_| {
            let (Some(vault), weak) = (app.vault().cloned(), Rc::downgrade(&app)) else {
                return;
            };
            let resume = app.statusbar.indexing() == statusbar::Indexing::Paused;
            if resume {
                app.statusbar.set_indexing(statusbar::Indexing::Running);
            }
            glib::spawn_future_local(async move {
                let asked = gio::spawn_blocking(move || match resume {
                    true => vault.resume_indexing(),
                    false => vault.stop_indexing(),
                })
                .await;
                if let (Ok(Err(e)), Some(app)) = (asked, weak.upgrade()) {
                    app.toast(&format!("Cannot reach the vault: {e}"));
                }
            });
        }
    ));

    // Right-click over the zoom readout: a PDF's two fitting modes, which otherwise live only in
    // the palette. Parented on the status bar's own button rather than in a header bar, so the
    // popover has a plain widget to hang off.
    //
    // The claim comes before anything else and happens whatever the tab is. `GtkButton`'s own
    // gesture is primary-only, so without it the press bubbled past the readout into the window
    // handle above and the shell's window menu took the pointer over our popover.
    let fit = gtk::GestureClick::new();
    fit.set_button(gdk::BUTTON_SECONDARY);
    fit.connect_pressed(glib::clone!(
        #[weak]
        app,
        move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if app.active_pdf().is_none() {
                return;
            }
            let menu = gio::Menu::new();
            for action in ["win.pdf-fit-width", "win.pdf-fit-page"] {
                menu.append(Some(label_of(action)), Some(action));
            }
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            popover.set_parent(app.statusbar.zoom());
            popover.set_has_arrow(false);
            // A popover parented by hand stays parented until it is unparented by hand — but not
            // while it is closing. `closed` is emitted from inside the item's own `clicked`, and
            // an unparented widget has no path to the window's action muxer, so unparenting there
            // dropped the action the click had just asked for: the menu appeared, Fit Height did
            // nothing, and the page stayed fitted to the width. The idle runs once the click is
            // over.
            popover.connect_closed(|p| {
                let p = p.clone();
                glib::idle_add_local_once(move || p.unparent());
            });
            popover.popup();
        }
    ));
    app.statusbar.zoom().add_controller(fit);

    app.modes.connect_toggled(glib::clone!(
        #[weak]
        app,
        move |button| {
            let picked = if button.is_active() {
                Mode::Split
            } else {
                Mode::Editor
            };
            if picked != app.mode.get() {
                app.set_mode(picked);
            }
        }
    ));
    app.sidebar_column.connect_visible_notify(glib::clone!(
        #[weak]
        app,
        move |_| {
            app.save_session_soon();
            app.follow_outline();
        }
    ));

    // The mouse's back and forward buttons. GTK's own gestures stop at button 3, and a
    // `GtkGestureClick` beside a widget that claims the sequence never sees the press at all
    // (paned.rs says why), so one capture-phase legacy controller on the window is where these
    // can be seen. It goes through the GAction rather than calling the reader directly, which is
    // what gives a mouse click the chrome reveal and the palette bookkeeping a chord gets.
    let nav = gtk::EventControllerLegacy::new();
    nav.set_propagation_phase(gtk::PropagationPhase::Capture);
    nav.connect_event(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, event| {
            let pressed = match event.event_type() {
                gdk::EventType::ButtonPress => true,
                gdk::EventType::ButtonRelease => false,
                _ => return glib::Propagation::Proceed,
            };
            let button = event
                .downcast_ref::<gdk::ButtonEvent>()
                .map(|event| event.button());
            let Some(action) = button.and_then(nav_action) else {
                return glib::Propagation::Proceed;
            };
            if pressed {
                let _ = WidgetExt::activate_action(&app.window, action, None);
            }
            // The release goes with the press, or whatever is under the pointer sees half a click.
            glib::Propagation::Stop
        }
    ));
    app.window.add_controller(nav);

    // Every divider in the window: double-click resets it, and it thickens while dragged.
    paned::watch(
        app.window.upcast_ref(),
        glib::clone!(
            #[weak]
            app,
            move |divider: &gtk::Paned| {
                if divider == &app.split {
                    divider.set_position(Session::default().sidebar_width);
                } else if divider == &app.paned {
                    app.centre_handle();
                } else if !app
                    .sidebar
                    .get()
                    .is_some_and(|sidebar| sidebar.reset_divider(divider))
                {
                    // A divider nobody claims (the pane splitters to come) has no remembered
                    // default, so half of its own extent is the reset.
                    let extent = match divider.orientation() {
                        gtk::Orientation::Vertical => divider.height(),
                        _ => divider.width(),
                    };
                    divider.set_position(extent / 2);
                }
            }
        ),
    );

    // Same rule on the way out of the window: the first buffer that cannot be written stops the
    // close and asks. Answering Discard or Overwrite closes the window again, which picks up
    // where this left off.
    app.window.connect_close_request(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_| {
            // git before any buffer: a pull or a switch still rewriting files must not race the
            // writes below, so the close waits for it and flushes once it is over.
            if let Some(git) = app.git.get()
                && git.busy()
            {
                if !git.closing() {
                    close_after_git(&app, git);
                }
                return glib::Propagation::Stop;
            }
            for tab in app.open_tabs().iter().filter(|t| t.save.modified.get()) {
                let Err(e) = app.flush_tab(tab) else {
                    continue;
                };
                app.ask_unsaved(tab, &e, |app, close| {
                    if close {
                        app.window.close();
                    }
                });
                return glib::Propagation::Stop;
            }
            let diagrams = app.diagrams();
            for diagram in &diagrams {
                diagram.finish_label();
            }
            for diagram in diagrams.iter().filter(|d| d.save.modified.get()) {
                let Err(e) = app.flush_diagram(diagram) else {
                    continue;
                };
                app.ask_unsaved_diagram(diagram, &e, |app, close| {
                    if close {
                        app.window.close();
                    }
                });
                return glib::Propagation::Stop;
            }
            // The process ends when this returns, and a drawn-on PDF's write lives on the
            // render thread, so it is waited for rather than left to be killed.
            for pdf in app.pdfs() {
                pdf.flush_blocking();
            }
            // The close is certain now: a fetch or a push still running is stopped, not waited for.
            if let Some(git) = app.git.get() {
                git.stop();
            }
            app.save_session();
            glib::Propagation::Proceed
        }
    ));

    // Chrome comes back on pointer motion, on Escape and whenever focus moves; hover alone must
    // never be the way back, or a keyboard-only user is stuck (DESIGN.md).
    //
    // GTK also emits `motion` when the widget under a *stationary* pointer changes, which typing
    // does every time the text reflows past it, Return most of all. So compare against the last
    // position and ignore an event that did not actually move the pointer, or the chrome pops
    // back on the first newline.
    let motion = gtk::EventControllerMotion::new();
    let last: Cell<Option<(f64, f64)>> = Cell::new(None);
    motion.connect_motion(glib::clone!(
        #[weak]
        app,
        move |_, x, y| {
            if last.replace(Some((x, y))) != Some((x, y)) {
                app.show_chrome();
                app.hover_status(Some((x, y)));
            }
        }
    ));
    motion.connect_leave(glib::clone!(
        #[weak]
        app,
        move |_| app.hover_status(None)
    ));
    app.window.add_controller(motion);
    // A wheel or touchpad scroll is the reader looking around the document rather than writing
    // in it, so the chrome comes back — a touchpad scroll need not move the pointer at all.
    // Capture phase, so a scroller that takes the event on its way down cannot hide it; the
    // event goes on to it untouched. The keyboard's scroll chords are actions and stay typing.
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    scroll.connect_scroll(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, _, _| {
            app.show_chrome();
            glib::Propagation::Proceed
        }
    ));
    app.window.add_controller(scroll);

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                // The way out of presentation, where there is no chrome to bring back.
                match app.presenting.get().is_some() {
                    true => app.set_presenting(false),
                    false => app.show_chrome(),
                }
            }
            glib::Propagation::Proceed
        }
    ));
    // The other half of `Ctrl+Tab`: an action activation says nothing about the modifier still
    // being down, so the release is what commits the tab the chord landed on to the front of the
    // pane's order. Capture phase at the window, like the press above, because the release goes to
    // whatever has the keyboard and that is the document the chord has just switched to.
    keys.connect_key_released(glib::clone!(
        #[weak]
        app,
        move |_, key, _, _| {
            if matches!(key, gdk::Key::Control_L | gdk::Key::Control_R) {
                app.end_cycle();
            }
        }
    ));
    app.window.add_controller(keys);
    // A chord whose release never arrives because the window stopped being the active one — a
    // dialog, another window, the session locking — ends here instead.
    app.window.connect_is_active_notify(glib::clone!(
        #[weak]
        app,
        move |window| {
            if !window.is_active() {
                app.end_cycle();
            }
        }
    ));

    // The other half of the find bar's Escape: the bar has one of its own, but only for a key
    // pressed inside it, and the user who typed a query and went back to reading has the focus in
    // the document. Bubble phase, and a separate controller from the capture one above, so
    // everything that answers to Escape closer to the focus still gets it first — the signature
    // popover dismissing itself, a popover, a menu.
    let dismiss = gtk::EventControllerKey::new();
    dismiss.connect_key_pressed(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| match key == gdk::Key::Escape && app.pane().find.is_open() {
            true => {
                app.pane().find.close();
                glib::Propagation::Stop
            }
            false => glib::Propagation::Proceed,
        }
    ));
    app.window.add_controller(dismiss);
    app.window.connect_notify_local(
        Some("focus-widget"),
        glib::clone!(
            #[weak]
            app,
            move |_, _| app.show_chrome()
        ),
    );

    // Dark mode, the accent colour and the document font are pure GNOME settings; we only
    // re-colour what we drew ourselves.
    let style = adw::StyleManager::default();
    for property in ["accent-color", "dark"] {
        style.connect_notify_local(
            Some(property),
            glib::clone!(
                #[weak]
                app,
                move |_, _| {
                    // Solarized has one palette per system state, so which half is installed is
                    // decided here, before anything reads the resulting colours back out.
                    theme::refresh();
                    app.restyle_all();
                }
            ),
        );
    }
    // A terminal is code, so it follows the monospace font rather than the document one.
    style.connect_monospace_font_name_notify(glib::clone!(
        #[weak]
        app,
        move |_| {
            for term in app.terminals() {
                term.refont();
            }
        }
    ));
    style.connect_document_font_name_notify(glib::clone!(
        #[weak]
        app,
        move |_| {
            install_document_font();
            // A different document font is a different marker width, so the hanging headings
            // have to be measured again.
            for tab in app.open_tabs() {
                tab.rehang();
            }
            if let Some(preview) = app.preview.borrow().as_ref() {
                preview.restyle();
            }
        }
    ));
}

/// Right-click and Menu open the file-operations menu; Delete trashes. A key controller on the
/// tree, not a global accelerator, so Delete cannot fire while the user is typing.
pub fn wire_tree(app: &Rc<App>) {
    let Some(list) = app.tree.get().map(|tree| tree.view().clone()) else {
        return;
    };

    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    click.connect_pressed(glib::clone!(
        #[weak]
        app,
        move |gesture, _, x, y| {
            let Some(tree) = app.tree.get() else { return };
            gesture.set_state(gtk::EventSequenceState::Claimed);
            // No row under the pointer is the blank area below the last one, and that gets a menu
            // too: it is where a note is created in the vault root. A row the index does not hold
            // gets none: nothing in that menu may happen inside a tree nothing is watching.
            let row = tree.row_at(x, y);
            if row.as_ref().is_some_and(|row| !row.indexed) {
                return;
            }
            // The menu hangs off the host box, so the click has to be translated out of the
            // list's coordinates or it would point at the wrong row once the list is scrolled.
            let Some(at) = tree.view().compute_point(
                tree.widget(),
                &gtk::graphene::Point::new(x as f32, y as f32),
            ) else {
                return;
            };
            let anchor = gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1);
            // A right-click on a marked row is about the whole set; one anywhere else is the
            // reader pointing at a single file, and forgets the marks the way a plain click does.
            let marked = marks_under(tree, row.as_ref());
            if let Some(ops) = app.ops() {
                let popover =
                    fileops::context_menu(ops, tree.widget(), clicked(&row), &marked, anchor);
                pin_row(&app, &popover, row.as_ref().map(|row| row.rel.as_str()));
            }
        }
    ));
    list.add_controller(click);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        app,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            let Some(tree) = app.tree.get() else {
                return glib::Propagation::Proceed;
            };
            let row = tree.selected();
            // Delete takes the marked set when there is one, as the menu over it does, whichever
            // row the highlight is on.
            let marked = tree.marked();
            if key == gdk::Key::Delete
                && !marked.is_empty()
                && let Some(ops) = app.ops()
            {
                fileops::trash_all(ops, marked.into_iter().map(|(rel, _)| rel).collect());
                return glib::Propagation::Stop;
            }
            // The same rule the pointer path follows: a row the index does not hold is listed and
            // opened, never changed. Both keys stop here rather than falling through to the
            // vault-root menu an empty selection would get.
            if row.as_ref().is_some_and(|row| !row.indexed) {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::Delete => {
                    let Some(row) = &row else {
                        return glib::Propagation::Proceed;
                    };
                    if let Some(ops) = app.ops() {
                        fileops::trash(ops, &row.rel);
                    }
                }
                // Escape is how the keyboard lets a Ctrl+click selection go; with nothing marked
                // it is not ours, and whatever else answers Escape gets it.
                gdk::Key::Escape => {
                    if !tree.clear_marks() {
                        return glib::Propagation::Proceed;
                    }
                }
                // Nothing selected is the keyboard's version of a click on blank space, and it
                // gets the same root-scoped menu the pointer path shows there.
                gdk::Key::Menu => {
                    let Some(ops) = app.ops() else {
                        return glib::Propagation::Proceed;
                    };
                    let marked = marks_under(tree, row.as_ref());
                    let popover = fileops::context_menu(
                        ops,
                        tree.widget(),
                        clicked(&row),
                        &marked,
                        row_anchor(tree.view(), tree.widget()),
                    );
                    pin_row(&app, &popover, row.as_ref().map(|row| row.rel.as_str()));
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        }
    ));
    list.add_controller(keys);
}

/// The Keep Theirs / Keep Mine bar under a comparison's panes.
pub fn choice_row(theirs: &gtk::Button, mine: &gtk::Button) -> gtk::Widget {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::End)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    row.append(theirs);
    row.append(mine);
    row.upcast()
}

/// A conflict pane's label: the file, and when it was last written.
///
/// The two sides of a sync conflict are one note twice, and which of them is called Mine is
/// decided by which one kept the original name — that is Syncthing's decision, not ours, and the
/// copy it renames can be the newer of the two. The time is the only thing here that says so.
pub fn written_at(rel: &str, etag: &Etag) -> String {
    match glib::DateTime::from_unix_local(etag.mtime_ns / 1_000_000_000)
        .and_then(|when| when.format("%d %b %H:%M"))
    {
        Ok(when) => format!("{rel} · {when}"),
        Err(_) => rel.to_string(),
    }
}

/// Hold the tree's row highlight on the row `popover` was opened over, and let it go when the
/// menu closes.
///
/// The list selects rows on hover, and a popover taking the pointer is a leave as far as the list
/// is concerned, so without this the highlight went back to the open file the moment the menu
/// appeared and the menu was left pointing at a row nothing lit. `None` is the blank area below
/// the last row, which pins nothing: there is no row there to hold it on.
fn pin_row(app: &Rc<App>, popover: &gtk::PopoverMenu, rel: Option<&str>) {
    let Some(tree) = app.tree.get() else { return };
    tree.pin(rel);
    popover.connect_closed(glib::clone!(
        #[weak]
        app,
        move |_| {
            if let Some(tree) = app.tree.get() {
                tree.pin(None);
            }
        }
    ));
}

/// The marked rows a menu opened over `row` acts on: the whole set when the click landed on one
/// of its rows — a row inside a marked folder included — and nothing otherwise: a click anywhere
/// else forgets them, which is what a plain left click does too.
fn marks_under(tree: &tree::Tree, row: Option<&tree::Row>) -> Vec<(String, bool)> {
    if row.is_some_and(|row| tree.is_marked(&row.rel)) {
        return tree.marked();
    }
    tree.clear_marks();
    Vec::new()
}

/// A tree row as the context menu wants it: its path, and whether it is a directory. `None` stays
/// `None`, which is what the menu reads as the vault root.
fn clicked(row: &Option<tree::Row>) -> Option<(&str, bool)> {
    row.as_ref().map(|row| (row.rel.as_str(), row.is_dir()))
}

/// Where a Menu-key popover points: the focused row, or the top of the list. In `host`'s
/// coordinates, since that is what the popover is parented to.
fn row_anchor(list: &gtk::ListView, host: &gtk::Widget) -> gdk::Rectangle {
    let bounds = list.focus_child().and_then(|row| row.compute_bounds(host));
    match bounds {
        Some(r) => gdk::Rectangle::new(
            r.x() as i32,
            r.y() as i32,
            r.width() as i32,
            r.height() as i32,
        ),
        None => gdk::Rectangle::new(0, 0, 1, 1),
    }
}

/// What the status bar says while a close waits for git.
const CLOSING_AFTER_GIT: &str = "Closing when git has finished…";

/// Closing while git rewrites the working tree: ask, and close once it has finished — or stay,
/// under git's own failure, where it did not go through (DESIGN.md, States). There is no answer
/// that stops git, since stopping it is what would leave the repository half-updated.
fn close_after_git(app: &Rc<App>, git: &Rc<git::Panel>) {
    let dialog = dialogs::alert(
        "Git Is Updating Files",
        "Closing now could leave the repository half-updated. The window closes as soon as git \
         has finished.",
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            (
                "wait",
                "Close When Finished",
                adw::ResponseAppearance::Suggested,
            ),
        ],
        "wait",
    );
    let (weak, git) = (Rc::downgrade(app), git.clone());
    dialogs::choose(&dialog, Some(&app.window), move |response| {
        let Some(app) = weak.upgrade().filter(|_| response == "wait") else {
            return;
        };
        app.statusbar.set_transfer(CLOSING_AFTER_GIT, true);
        let weak = Rc::downgrade(&app);
        git.when_done(move |ok| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            app.statusbar.set_transfer(CLOSING_AFTER_GIT, false);
            if ok {
                app.window.close();
            }
        });
    });
}
