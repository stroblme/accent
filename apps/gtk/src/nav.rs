//! Manage active panes, tab splitting and moves, and Back/Forward history.

use super::*;

impl App {
    /// Choose the focused pane, or the last activated pane when focus is elsewhere.
    /// Background pane selections do not change where new notes open.
    pub fn pane(&self) -> Rc<Pane> {
        self.active_pane.borrow().clone()
    }

    /// The pane holding the keyboard, if one does.
    pub(crate) fn keyboard_pane(&self) -> Option<Rc<Pane>> {
        let focus = self.focused()?;
        self.panes
            .borrow()
            .iter()
            .find(|pane| focus.is_ancestor(pane.widget()))
            .cloned()
    }

    /// Return the active pane's tab view.
    pub fn tabs(&self) -> adw::TabView {
        self.pane().tabs.clone()
    }

    pub fn pane_of(&self, page: &adw::TabPage) -> Option<Rc<Pane>> {
        self.panes.borrow().iter().find(|p| p.has(page)).cloned()
    }

    /// Select an already-open page and activate its pane.
    /// Move focus across panes when following a link, but retain sidebar focus while navigating its lists.
    pub fn reveal_page(self: &Rc<Self>, page: &adw::TabPage) {
        if let Some(pane) = self.pane_of(page) {
            pane.tabs.set_selected_page(page);
            // The reader's pane now, which a restore still landing leaves alone: a tab already in
            // front notifies nothing that would say so.
            self.reader_in(&pane);
            // A tab already in front notifies nothing, and with the keyboard in another pane the
            // `selected-page` handler syncs that one; the focus moved below then finds this pane
            // active already.
            if self.set_active_pane(&pane) {
                self.sync_active();
            }
            if self.keyboard_pane().is_some_and(|k| !Rc::ptr_eq(&k, &pane)) {
                self.focus_document(&pane);
            }
        }
    }

    /// Close a page in the pane that holds it, whichever that is.
    pub fn close_page(&self, page: &adw::TabPage) {
        if let Some(pane) = self.pane_of(page) {
            pane.tabs.close_page(page);
        }
    }

    /// Close unpinned tabs in the active pane through the normal save-and-confirm path.
    /// Close least-recently-used first to preserve the active tab until its turn.
    pub fn close_pane_tabs(&self) {
        let pane = self.pane();
        for page in pane.recent().iter().rev() {
            if !self.is_pinned(page) {
                pane.tabs.close_page(page);
            }
        }
    }

    /// Step through the active pane's MRU tabs with Ctrl+Tab or Ctrl+Shift+Tab.
    ///
    /// Pane-local cycling keeps focus within the split. Holding the chord shows a switcher;
    /// a quick flip to the previous tab shows none.
    pub fn cycle_tab(self: &Rc<Self>, forward: bool) {
        let pane = self.pane();
        if let Some(page) = pane.step(forward) {
            pane.tabs.set_selected_page(&page);
        }
        if pane.switcher.shown() {
            return self.show_switcher(&pane);
        }
        let (app, held) = (Rc::downgrade(self), Rc::downgrade(&pane));
        pane.switcher.soon(move || {
            if let (Some(app), Some(pane)) = (app.upgrade(), held.upgrade())
                && pane.cycling().is_some()
            {
                app.show_switcher(&pane);
            }
        });
    }

    /// Fill `pane`'s card with its tabs.
    fn show_switcher(&self, pane: &Pane) {
        let entries: Vec<switcher::Entry> = pane
            .recent()
            .iter()
            .map(|page| self.switcher_entry(page))
            .collect();
        pane.switcher.show(&entries, pane.cycling().unwrap_or(0));
    }

    /// A file by its file-type icon and its folder, as the file lists show it; a shell or a
    /// comparison by its tab's own icon and title.
    fn switcher_entry(&self, page: &adw::TabPage) -> switcher::Entry {
        let name = page.title().to_string();
        match self.doc_for_page(page).filter(|doc| !doc.is_transient()) {
            Some(doc) => {
                let key = doc.key();
                switcher::Entry {
                    icon: Some(gio::ThemedIcon::new(doc::icon_for(&key)).upcast()),
                    name,
                    folder: accent_core::path::parent_dir(&key).to_string(),
                }
            }
            None => switcher::Entry {
                icon: page.icon(),
                name,
                folder: String::new(),
            },
        }
    }

    /// Finish every pane's cycle on Ctrl release or window deactivation.
    /// Checking all panes prevents abandoned cycles after focus crosses a split.
    pub fn end_cycle(&self) {
        for pane in self.panes.borrow().iter() {
            pane.end_cycle();
        }
    }

    /// Escape with Ctrl still held: the chord goes back to the tab it started from. Says whether
    /// a chord was in flight, which is then all the press does.
    pub fn cancel_cycle(&self) -> bool {
        // A copy: going back selects a tab, and the selection handlers reach the panes too.
        let panes = self.panes.borrow().clone();
        let mut cancelled = false;
        for pane in panes {
            cancelled |= pane.cancel_cycle();
        }
        cancelled
    }

    // --- back and forward ---------------------------------------------------------------------

    /// Where a document is being read: the caret in a text tab, the reading anchor in a PDF, and
    /// the document itself for anything with no position of its own.
    fn place_of(doc: &Doc) -> Place {
        let mut mark = None;
        let at = match doc {
            Doc::Text(tab) => {
                let iter = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
                mark = Some(Rc::new(Held::at(
                    tab.buffer.upcast_ref(),
                    &iter,
                    tab.replaced(),
                )));
                Spot::Caret(iter.line() + 1, iter.line_offset() + 1)
            }
            Doc::Pdf(pdf) => Spot::Page(pdf.anchor()),
            Doc::Diagram(d) => Spot::Sheet(d.page_index()),
            _ => Spot::Whole,
        };
        Place {
            key: doc.key(),
            at,
            mark,
        }
    }

    /// Where the reader is in `pane` right now.
    pub fn here(&self, pane: &Pane) -> Option<Place> {
        let page = pane.tabs.selected_page()?;
        self.doc_for_page(&page).as_ref().map(Self::place_of)
    }

    /// Record where the reader is in the active pane, so Back returns there. Every jump calls
    /// this before it moves; a place that coalesces with the last one replaces it.
    pub fn mark(&self) {
        let pane = self.pane();
        if let Some(here) = self.here(&pane) {
            self.record(&pane, here);
        }
    }

    /// The same, for a document that is about to stop being the selected one: the tab being left
    /// still holds its caret, so this is read before the switch has happened.
    pub fn mark_page(&self, page: &adw::TabPage) {
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        if let Some(doc) = self.doc_for_page(page) {
            self.record(&pane, Self::place_of(&doc));
        }
    }

    pub fn record(&self, pane: &Pane, place: Place) {
        // A restore brings each pane's stored tab to the front as its tabs land; none of that is
        // somewhere the reader went, so a restored window comes up with an empty history.
        if self.navigating.get() || self.restoring() {
            return;
        }
        pane.nav.borrow_mut().record(place, Instant::now());
    }

    /// Step pane-local history for Alt+Left/Right and side mouse buttons.
    /// Skip entries whose documents transferred out of the pane without moving focus to another pane.
    pub fn navigate(self: &Rc<Self>, forward: bool) {
        let pane = self.pane();
        let Some(mut here) = self.here(&pane) else {
            return;
        };
        self.navigating.set(true);
        loop {
            // Bound first, so the history is not borrowed while `go_to` selects a page and the
            // selection handler writes to it.
            let step = match forward {
                true => pane.nav.borrow_mut().forward(here.clone()),
                false => pane.nav.borrow_mut().back(here.clone()),
            };
            let Some(to) = step else { break };
            if self.go_to(&pane, &to) {
                break;
            }
            here = to;
        }
        self.navigating.set(false);
    }

    /// Put the reader at `to`, if the document it names is still one of this pane's.
    fn go_to(&self, pane: &Pane, to: &Place) -> bool {
        let Some(doc) = self
            .docs()
            .into_iter()
            .find(|d| d.key() == to.key && pane.has(d.page()))
        else {
            return false;
        };
        tracing::debug!("navigating to {to:?}");
        pane.tabs.set_selected_page(doc.page());
        match (&doc, to.at) {
            // The mark, which has followed the edits since; the line and column it was at
            // where it can say nothing.
            (Doc::Text(tab), Spot::Caret(line, column)) => {
                let marked = to
                    .mark
                    .as_ref()
                    .and_then(|held| held.iter_in(tab.buffer.upcast_ref(), tab.replaced()));
                match marked {
                    Some(iter) => tab.jump_to(&iter, 0.25),
                    None => tab.goto_line(line, column),
                }
            }
            (Doc::Pdf(pdf), Spot::Page(anchor)) => pdf.scroll_to(anchor),
            (Doc::Diagram(d), Spot::Sheet(page)) => d.show_page(page),
            _ => {}
        }
        true
    }

    /// Select the previous MRU tab before detaching `page`.
    ///
    /// Libadwaita otherwise selects the left neighbour immediately, updating history before the
    /// fallback can be chosen. Preselection prevents that automatic choice.
    pub fn select_survivor(&self, page: &adw::TabPage) {
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        if pane.tabs.selected_page().as_ref() != Some(page) {
            return;
        }
        if let Some(next) = pane.survivor(page) {
            pane.tabs.set_selected_page(&next);
        }
    }

    /// This tab is a real one now, not a preview: it was edited, or its tab was double-clicked.
    pub fn promote(&self, page: &adw::TabPage) {
        if let Some(pane) = self.pane_of(page) {
            pane.keep(page);
        }
    }

    // --- pinned tabs ---------------------------------------------------------------------

    /// Whether the window marks `page` as pinned, surviving transfers between panes.
    pub fn is_pinned(&self, page: &adw::TabPage) -> bool {
        self.pinned.borrow().contains(page)
    }

    /// Pin Tab and Unpin Tab: the page the tab menu was opened on, or the tab in front.
    pub fn pin_tab(self: &Rc<Self>, pin: bool) {
        let page = self.menu_page.borrow().clone();
        if let Some(page) = page.or_else(|| self.tabs().selected_page()) {
            self.set_pinned(&page, pin);
        }
    }

    /// Pin or unpin `page`, keeping it and moving it to the pinned/unpinned boundary.
    pub(crate) fn set_pinned(self: &Rc<Self>, page: &adw::TabPage, pin: bool) {
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        if self.is_pinned(page) == pin {
            return;
        }
        match pin {
            true => {
                pane.keep(page);
                self.pinned.borrow_mut().push(page.clone());
            }
            false => self.pinned.borrow_mut().retain(|p| p != page),
        }
        panes::mark_pinned(page, pin);
        self.hold_pinned(&pane, page);
        self.save_session_soon();
    }

    /// Put `page` back on its side of the pinned tabs in `pane`, if something has taken it across
    /// (`panes::pinned_slot`).
    fn hold_pinned(&self, pane: &Pane, page: &adw::TabPage) {
        let pages = pane.pages();
        let Some(at) = pages.iter().position(|p| p == page) else {
            return;
        };
        let pinned = pages
            .iter()
            .filter(|p| *p != page && self.is_pinned(p))
            .count();
        let to = panes::pinned_slot(at, pinned, self.is_pinned(page));
        if to != at {
            pane.tabs.reorder_page(page, to as i32);
        }
    }

    /// Restore pinned ordering on an idle after libadwaita finishes its reorder or attach handling.
    pub fn hold_pinned_soon(self: &Rc<Self>, pane: &Rc<Pane>, page: &adw::TabPage) {
        let (app, pane, page) = (Rc::downgrade(self), Rc::downgrade(pane), page.clone());
        glib::idle_add_local_once(move || {
            if let (Some(app), Some(pane)) = (app.upgrade(), pane.upgrade()) {
                app.hold_pinned(&pane, &page);
            }
        });
    }

    // --- panes ---------------------------------------------------------------------------

    /// Create an empty pane beside `at`; the caller must populate it because it has no detach event to close it.
    pub(crate) fn split_beside(self: &Rc<Self>, at: &Rc<Pane>, side: Side) -> Rc<Pane> {
        let pane = Pane::new(&tab_menu());
        wire_pane(self, &pane);
        self.panes.borrow_mut().push(pane.clone());
        panes::split(at, &pane, side);
        self.sync_panes();
        self.set_active_pane(&pane);
        pane
    }

    /// Move `page` into a new pane beside `at`, refusing the source pane's sole tab.
    /// Otherwise empty-pane teardown would immediately undo the split.
    pub fn split_page(self: &Rc<Self>, at: &Rc<Pane>, side: Side, page: &adw::TabPage) {
        // A page in no pane of ours: a drag still in flight, which libadwaita has detached from
        // its view. `Shell::landed` waits for the attach before asking for a split, so this is
        // only reachable from the menu and the palette, where there is nothing to split off.
        let Some(from) = self.pane_of(page) else {
            return;
        };
        if Rc::ptr_eq(&from, at) && at.tabs.n_pages() <= 1 {
            return self.toast("This pane has only one note.");
        }
        self.select_survivor(page);
        let pane = self.split_beside(at, side);
        from.tabs.transfer_page(page, &pane.tabs, 0);
        // The page is the new pane's only one, so it is selected already; what it has not got is
        // the keyboard. This is also where `move_tab` lands in a window with nowhere to move to.
        self.focus_document(&pane);
    }

    /// The tab context menu's Split Right and friends: the page that was right-clicked, split off
    /// its own pane.
    pub fn split_active(self: &Rc<Self>, side: Side) {
        let Some(page) = self
            .menu_page
            .borrow()
            .clone()
            .or_else(|| self.tabs().selected_page())
        else {
            return;
        };
        let at = self.pane_of(&page).unwrap_or_else(|| self.pane());
        self.split_page(&at, side, &page);
    }

    /// Move a tab into the neighbouring pane on `side`, splitting if none exists.
    /// This makes movement useful from a single pane; split actions always create a pane.
    pub fn move_tab(self: &Rc<Self>, side: Side) {
        let Some(page) = self
            .menu_page
            .borrow()
            .clone()
            .or_else(|| self.tabs().selected_page())
        else {
            return;
        };
        let Some(from) = self.pane_of(&page) else {
            return;
        };
        // Cloned out, and the borrow dropped: a transfer runs `page-detached` and `page-attached`
        // synchronously, and both reach back into `panes`.
        let panes: Vec<Rc<Pane>> = self.panes.borrow().clone();
        let root = self.window.clone().upcast::<gtk::Widget>();
        let rects: Vec<graphene::Rect> = panes.iter().map(|p| pane_rect(p, &root)).collect();
        let Some(i) = panes
            .iter()
            .position(|p| Rc::ptr_eq(p, &from))
            .and_then(|at| panes::neighbour(rects[at], &rects, side))
        else {
            return self.split_active(side);
        };
        let to = &panes[i];
        from.tabs.transfer_page(&page, &to.tabs, to.tabs.n_pages());
        // Selecting it retargets the destination's find bar and saves the session, through the
        // `selected-page` handler the pane already has; the keyboard, still in the pane it came
        // from until the idle below, leaves the active pane to this.
        to.tabs.set_selected_page(&page);
        if self.set_active_pane(to) {
            self.sync_active();
        }
        self.focus_document(to);
    }

    /// Step the nearest divider on `side`'s axis, respecting pane minimum sizes.
    /// No-op if there is no matching split.
    pub fn move_divider(&self, side: Side) {
        let (orientation, _) = panes::arrange(side);
        let forward = matches!(side, Side::Right | Side::Down);
        let mut inner = self.pane().widget().clone();
        // Up through the pane tree only: its root is a bin, so the walk never reaches the split
        // between the document column and the preview, or the sidebar's.
        while let Some(paned) = inner.parent().and_downcast::<gtk::Paned>() {
            if paned.orientation() == orientation {
                let to = panes::divider_step(paned.position(), session::extent_of(&paned), forward);
                paned.set_position(to.min(paned.max_position()).max(paned.min_position()));
                return;
            }
            inner = paned.upcast();
        }
    }

    /// Finish a middle drop by moving the tab to the bar's end and focusing it.
    /// Libadwaita initially attaches through create-window at the front.
    pub fn move_in(&self, pane: &Pane, page: &adw::TabPage) {
        if !pane.has(page) {
            return;
        }
        pane.tabs.reorder_last(page);
        self.focus_document(pane);
    }

    /// A note from the tree, opened in a pane of its own beside `at`. Unlike [`Self::split_page`]
    /// this always splits: the note may not be open at all, so there is something new to show.
    pub fn open_beside(self: &Rc<Self>, at: &Rc<Pane>, side: Side, rel: &str) {
        // Not open yet: it opens into `at` and is split off once it lands ([`Opened::Beside`]),
        // since a text tab exists only once the worker's read is back. A pane split off ahead of
        // it would stand empty and close itself before the tab arrived.
        let Some(page) = self.doc_for(rel).map(|doc| doc.page().clone()) else {
            self.set_active_pane(at);
            return self.open_as(rel, Opened::Beside(side));
        };
        let pane = self.split_beside(at, side);
        if let Some(from) = self.pane_of(&page) {
            from.tabs.transfer_page(&page, &pane.tabs, 0);
        }
        // Nothing arrived: the tab was in no pane to move it from.
        if pane.tabs.n_pages() == 0 {
            self.close_pane(&pane);
        }
    }

    /// Take a pane out of the window. The last one stays whatever happens: a window with no pane
    /// has nowhere to open a note into.
    pub fn close_pane(self: &Rc<Self>, pane: &Rc<Pane>) {
        if self.panes.borrow().len() <= 1 {
            return;
        }
        panes::detach(pane);
        self.panes.borrow_mut().retain(|p| !Rc::ptr_eq(p, pane));
        if Rc::ptr_eq(&self.pane(), pane)
            && let Some(next) = self.panes.borrow().first().cloned()
        {
            self.set_active_pane(&next);
        }
        self.sync_panes();
        self.sync_active();
    }

    /// `pane`'s last tab is closing: a keyboard on the pane's own bar or in that tab goes to the
    /// tab in front of the pane that takes its place, which its focus makes the active one (user
    /// decision 2026-10-13). Before the tab goes, while all of it is in the window: taken out with
    /// the keyboard in it, GTK would hand the keyboard to whatever comes first at the next frame,
    /// which can come before [`Self::close_pane`].
    pub(crate) fn hand_on_keyboard(&self, pane: &Pane) {
        if !self.focused().is_some_and(|w| w.is_ancestor(pane.widget())) {
            return;
        }
        let heir = panes::heir(pane).and_then(|column| {
            let panes = self.panes.borrow();
            panes.iter().find(|p| p.widget() == &column).cloned()
        });
        if let Some(widget) = heir.and_then(|heir| self.key_target(&heir)) {
            widget.grab_focus();
        }
    }

    /// Activate `pane` for future opens and return whether it changed.
    /// Callers sync title, references and preview even if only its selected page changed.
    pub fn set_active_pane(&self, pane: &Rc<Pane>) -> bool {
        if Rc::ptr_eq(&self.active_pane.borrow(), pane) {
            return false;
        }
        *self.active_pane.borrow_mut() = pane.clone();
        self.mark_active_pane();
        true
    }

    /// Outline selected tabs in inactive panes so the next-open and keyboard destination is visible.
    fn mark_active_pane(&self) {
        let active = self.pane();
        for pane in self.panes.borrow().iter() {
            let idle = !Rc::ptr_eq(pane, &active);
            crate::widgets::set_class(&pane.bar, "accent-idle-pane", idle);
        }
    }

    /// Focus the document shown in `pane` after moving a tab into it.
    ///
    /// Otherwise libadwaita can leave focus on the source pane's survivor or drop it entirely.
    pub(crate) fn focus_document(&self, pane: &Pane) {
        let Some(widget) = self.key_target(pane) else {
            return;
        };
        // From an idle, as a new terminal's own focus is (see [`Self::open_terminal_at`]): the
        // page has only just been attached, and a widget still mid-reparenting is not one GTK
        // hands the keyboard to — measured, the grab does nothing and `GtkPaned` complains about
        // a focus child that is not its child. The idle also runs after the pane the tab left has
        // closed itself, that close being queued first.
        glib::idle_add_local_once(move || {
            widget.grab_focus();
        });
    }

    /// The widget of the document in front of `pane` that takes the keyboard: the document itself,
    /// as container focus may select a banner or a scroller that takes no input. Images and status
    /// pages use their page; blob comparisons use the right column.
    fn key_target(&self, pane: &Pane) -> Option<gtk::Widget> {
        Some(match self.doc_of(pane)? {
            Doc::Text(tab) => tab.view.clone().upcast(),
            Doc::Terminal(term) => term.view.clone().upcast(),
            Doc::Pdf(pdf) => pdf.key_target(),
            Doc::Diagram(d) => d.key_target(),
            Doc::Notebook(n) => n.key_target(),
            Doc::Diff(d) => d.key_target(),
            doc @ (Doc::Image(_) | Doc::Status(_)) => doc.page().child(),
        })
    }

    /// What changes when a pane appears or goes: whether the tab bars may hide themselves, which
    /// of them are not the active one's, and whether there is any note left to show at all.
    pub fn sync_panes(&self) {
        let panes = self.panes.borrow();
        // A single pane's bar disappears with its second tab, as it always did. Several panes have
        // to keep theirs: the bar is what says which notes are in which pane.
        let alone = panes.len() == 1;
        let pages: i32 = panes.iter().map(|p| p.tabs.n_pages()).sum();
        for pane in panes.iter() {
            pane.bar.set_autohide(alone);
        }
        self.mark_active_pane();
        let name = if pages == 0 { "empty" } else { "tabs" };
        self.content.set_visible_child_name(name);
    }

    /// Put the drop sheets in or out of the picture in every pane at once: a drag that started
    /// over one pane has to be droppable on all of them.
    pub fn set_drop_active(&self, on: bool) {
        // A drag starting is where one that was aimed and never spent stops mattering.
        if on && let Some(shell) = self.shell.upgrade() {
            shell.landing.take();
        }
        for pane in self.panes.borrow().iter() {
            pane.set_drop_active(on);
        }
    }

    /// The window a row is being dragged out of, when it is another window: the row names a file
    /// in that window's vault, so here only a tree takes it, as a copy.
    pub fn dragged_from(&self) -> Option<Rc<App>> {
        let shell = self.shell.upgrade()?;
        let from = shell.tree_drag.borrow().as_ref()?.upgrade()?;
        (!std::ptr::eq(Rc::as_ptr(&from), self)).then_some(from)
    }

    /// A tab or a vault path let go over `pane`. `true` when it was taken; a tab is taken but
    /// only recorded, and `create-window` moves it (see [`editor::Landing`]).
    pub fn dropped(self: &Rc<Self>, pane: &Rc<Pane>, zone: Zone, value: &glib::Value) -> bool {
        if value.get::<adw::TabPage>().is_ok() {
            // A dragged page has left its view, and libadwaita gives it one again through the
            // `create-window` it asks for once any drop outside its own tab bars has finished.
            // Declining used to be how that was summoned, which only held on X11: on Wayland the
            // compositor cancels a declined drop, and libadwaita takes a cancel as the tab going
            // back where it came from. Whose tab it is does not matter here —
            // `Shell::adopt_page` sorts that out from `page-attached` once it is attached.
            if let Some(shell) = self.shell.upgrade() {
                shell.aim(self, pane, zone);
            }
            return true;
        }
        let Ok(rel) = value.get::<String>() else {
            return false;
        };
        match zone {
            Zone::Split(side) => self.open_beside(pane, side, &rel),
            Zone::Here => {
                // The note may be in front of this pane already: `open_path` reveals it, and with
                // the pane active by then nothing would sync.
                if self.set_active_pane(pane) {
                    self.sync_active();
                }
                self.open_path(&rel);
            }
        }
        true
    }
}
