//! Panes and tabs as places: which pane is active, splitting and moving tabs between panes, and
//! the back/forward history of where the reader has been.

use super::*;

impl App {
    /// The pane a note opens into: the last one whose tab was selected or whose editor had focus.
    pub fn pane(&self) -> Rc<Pane> {
        self.active_pane.borrow().clone()
    }

    /// The active pane's tab view. Every `self.tabs` of the single-pane window went through here.
    pub fn tabs(&self) -> adw::TabView {
        self.pane().tabs.clone()
    }

    pub fn pane_of(&self, page: &adw::TabPage) -> Option<Rc<Pane>> {
        self.panes.borrow().iter().find(|p| p.has(page)).cloned()
    }

    /// Bring a page to the front of whichever pane holds it, and make that pane the active one.
    /// A note that is already open is never opened twice, so this is what "open" does for it.
    pub fn reveal_page(&self, page: &adw::TabPage) {
        if let Some(pane) = self.pane_of(page) {
            pane.tabs.set_selected_page(page);
            *self.active_pane.borrow_mut() = pane;
        }
    }

    /// Close a page in the pane that holds it, whichever that is.
    pub fn close_page(&self, page: &adw::TabPage) {
        if let Some(pane) = self.pane_of(page) {
            pane.tabs.close_page(page);
        }
    }

    /// `Ctrl+Tab` and `Ctrl+Shift+Tab`: one step through the active pane's tabs in the order they
    /// were last used. Per pane, because a pane owns its tab view and its bar is what says which
    /// notes are in it; a window-wide order would have to move the keyboard across a split, which
    /// is not what a split is for.
    pub fn cycle_tab(&self, forward: bool) {
        let pane = self.pane();
        if let Some(page) = pane.step(forward) {
            pane.tabs.set_selected_page(&page);
        }
    }

    /// Ctrl came up, or the window stopped being the active one mid-chord: whichever pane was
    /// cycling commits the tab it landed on. Every pane rather than the active one, because a
    /// chord started in one pane and abandoned in another must not leave a cursor behind.
    pub fn end_cycle(&self) {
        for pane in self.panes.borrow().iter() {
            pane.end_cycle();
        }
    }

    // --- back and forward ---------------------------------------------------------------------

    /// Where a document is being read: the caret in a text tab, the reading anchor in a PDF, and
    /// the document itself for anything with no position of its own.
    fn place_of(doc: &Doc) -> Place {
        let at = match doc {
            Doc::Text(tab) => {
                let iter = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
                Spot::Caret(iter.line() + 1, iter.line_offset() + 1)
            }
            Doc::Pdf(pdf) => Spot::Page(pdf.anchor()),
            _ => Spot::Whole,
        };
        Place { key: doc.key(), at }
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
        if self.navigating.get() {
            return;
        }
        pane.nav.borrow_mut().record(place, Instant::now());
    }

    /// `Alt+Left` / `Alt+Right` and the mouse's side buttons: one step through the active pane's
    /// history. It may switch tabs inside the pane; it never moves the keyboard to another one.
    ///
    /// Entries whose document has left the pane are stepped over rather than dropped on the
    /// floor, which is what a tab dragged into a neighbouring pane leaves behind.
    pub fn navigate(self: &Rc<Self>, forward: bool) {
        let pane = self.pane();
        let Some(mut here) = self.here(&pane) else {
            return;
        };
        self.navigating.set(true);
        while let Some(to) = match forward {
            true => pane.nav.borrow_mut().forward(here.clone()),
            false => pane.nav.borrow_mut().back(here.clone()),
        } {
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
            (Doc::Text(tab), Spot::Caret(line, column)) => tab.goto_line(line, column),
            (Doc::Pdf(pdf), Spot::Page(anchor)) => pdf.scroll_to(anchor),
            _ => {}
        }
        true
    }

    /// Put the pane on the tab its reader was on before `page`, in time for `page` to go.
    ///
    /// Here rather than after the detach, because `AdwTabView` moves the selection to the left
    /// neighbour itself the moment the selected page leaves — and by the time anything could
    /// correct that, the neighbour is the newest thing in the history and the answer is lost.
    /// Selecting first means the page that is closing is no longer the selected one, so
    /// libadwaita has nothing to pick.
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

    // --- panes ---------------------------------------------------------------------------

    /// A new, empty pane beside `at`. The caller has to put something in it: an empty pane closes
    /// itself as soon as a page leaves it, but one that never held a page has nothing to react to.
    fn split_beside(self: &Rc<Self>, at: &Rc<Pane>, side: Side) -> Rc<Pane> {
        let pane = Pane::new(&tab_menu());
        wire_pane(self, &pane);
        self.panes.borrow_mut().push(pane.clone());
        panes::split(at, &pane, side);
        self.sync_panes();
        self.set_active_pane(&pane);
        pane
    }

    /// Move `page` into a new pane beside `at`. Splitting a pane's only note off it would empty
    /// the pane, which closes it again, so that one is refused rather than done and undone.
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

    /// Move the tab into the pane on `side`, or split one off when there is none that way. The
    /// fallback is what makes the chord worth having in the common single-pane window, where
    /// there is nowhere to move to yet; `win.split-*` stays the always-split.
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
        // Selecting it is what makes the destination the active pane, retargets its find bar and
        // saves the session, all through the `selected-page` handler the pane already has.
        to.tabs.set_selected_page(&page);
        self.focus_document(to);
    }

    /// A note from the tree, opened in a pane of its own beside `at`. Unlike [`Self::split_page`]
    /// this always splits: the note may not be open at all, so there is something new to show.
    pub fn open_beside(self: &Rc<Self>, at: &Rc<Pane>, side: Side, rel: &str) {
        let pane = self.split_beside(at, side);
        match self.tab_for(rel).map(|tab| tab.page.clone()) {
            Some(page) => {
                if let Some(from) = self.pane_of(&page) {
                    from.tabs.transfer_page(&page, &pane.tabs, 0);
                }
            }
            None => self.open_path(rel),
        }
        // Nothing arrived: the path was unopenable, or it was the only note in the pane it came
        // from, which has closed itself and left this one holding the same note it already had.
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
        // Whatever presentation mode borrowed goes with the pane rather than being left behind
        // in a column that no longer has an owner for it.
        pane.hold_find();
        panes::detach(pane);
        self.panes.borrow_mut().retain(|p| !Rc::ptr_eq(p, pane));
        if Rc::ptr_eq(&self.pane(), pane)
            && let Some(next) = self.panes.borrow().first().cloned()
        {
            *self.active_pane.borrow_mut() = next;
        }
        self.sync_panes();
        self.sync_active();
    }

    /// Make `pane` the one notes open into, reporting whether that was a change. Syncing the
    /// title, the backlinks and the preview is the caller's, because the commonest caller is a
    /// page selection that has to sync whether the pane changed or not.
    pub fn set_active_pane(&self, pane: &Rc<Pane>) -> bool {
        if Rc::ptr_eq(&self.active_pane.borrow(), pane) {
            return false;
        }
        *self.active_pane.borrow_mut() = pane.clone();
        true
    }

    /// Give the keyboard to what `pane` is showing, so a document moved into it takes the caret
    /// with it.
    ///
    /// Without this the focus stays behind: the pane the tab came from selects its survivor while
    /// the moved child still holds the keyboard, so libadwaita hands it to *that* document, and a
    /// transfer that empties the pane drops the focus altogether. Everything a focused view draws
    /// — the caret, GtkSourceView's current-line highlight — is then in the pane the reader has
    /// just left, and the next keystroke goes there too.
    ///
    /// The document's own widget rather than the page's child: `grab_focus` on a container takes
    /// the first thing in it that will have it, which for a note is whatever its banner is showing
    /// and for a shell is the scroller around vte, which cannot hear a keystroke. An image, a
    /// status page and a two-blob comparison have no keys of their own and are left alone.
    fn focus_document(&self, pane: &Pane) {
        let widget: gtk::Widget = match self.doc_of(pane) {
            Some(Doc::Text(tab)) => tab.view.clone().upcast(),
            Some(Doc::Terminal(term)) => term.view.clone().upcast(),
            Some(Doc::Pdf(pdf)) => pdf.key_target(),
            _ => return,
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

    /// What changes when a pane appears or goes: whether the tab bars may hide themselves, and
    /// whether there is any note left to show at all.
    pub fn sync_panes(&self) {
        let panes = self.panes.borrow();
        // A single pane's bar disappears with its second tab, as it always did. Several panes have
        // to keep theirs: the bar is what says which notes are in which pane.
        let alone = panes.len() == 1;
        let pages: i32 = panes.iter().map(|p| p.tabs.n_pages()).sum();
        for pane in panes.iter() {
            pane.bar.set_autohide(alone);
        }
        let name = if pages == 0 { "empty" } else { "tabs" };
        self.content.set_visible_child_name(name);
    }

    /// Put the drop sheets in or out of the picture in every pane at once: a drag that started
    /// over one pane has to be droppable on all of them.
    pub fn set_drop_active(&self, on: bool) {
        for pane in self.panes.borrow().iter() {
            pane.set_drop_active(on);
        }
    }

    /// A tab or a vault path let go over `pane`. `true` when it was taken — never for a tab,
    /// which is declined on purpose so that `create-window` fires; see [`Landing`].
    pub fn dropped(self: &Rc<Self>, pane: &Rc<Pane>, zone: Zone, value: &glib::Value) -> bool {
        if value.get::<adw::TabPage>().is_ok() {
            // Recorded, and deliberately declined: a dragged page has left its view, and
            // declining is what summons the `create-window` that can give it one again. Whose
            // tab it is does not matter here — `Shell::adopt_page` sorts that out from
            // `page-attached` once libadwaita has attached it.
            if let Some(shell) = self.shell.upgrade() {
                shell.aim(self, pane, zone);
            }
            return false;
        }
        let Ok(rel) = value.get::<String>() else {
            return false;
        };
        match zone {
            Zone::Split(side) => self.open_beside(pane, side, &rel),
            Zone::Here => {
                self.set_active_pane(pane);
                self.open_path(&rel);
            }
        }
        true
    }
}
