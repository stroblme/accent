//! The window's layout: the editor and split modes, presentation mode, the preview and the
//! chrome that fades while typing.

use super::*;

/// The two layouts to work in. Reading the rendered note alone is presentation mode, which is
/// temporary and belongs to the window rather than here.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Editor,
    Split,
}

impl Mode {
    pub fn next(self) -> Mode {
        match self {
            Mode::Editor => Mode::Split,
            Mode::Split => Mode::Editor,
        }
    }

    /// The icon that names this mode in the header toggle.
    pub fn icon(self) -> &'static str {
        match self {
            Mode::Editor => "document-edit-symbolic",
            Mode::Split => "view-dual-symbolic",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Editor => "editor",
            Mode::Split => "split",
        }
    }

    /// Anything unrecognised is the editor: a hand-edited session file, or one written when
    /// "preview" was still a mode, must not break the window.
    pub fn from_name(name: &str) -> Mode {
        match name {
            "split" => Mode::Split,
            _ => Mode::Editor,
        }
    }
}

/// Below this width the sidebar steps aside for the note: the default sidebar (280 px) beside the
/// narrowest cap the note's column ever gets (`COLUMN_FLOOR`, 480 px), under which the clamp stops
/// applying and the text takes whatever width is left. In `sp`, so a larger interface text size
/// collapses it sooner, as it makes the sidebar's rows wider.
const COLLAPSE: f64 = 760.0;

/// What leaving presentation mode has to put back. The window's size is not part of it: F5 only
/// takes the chrome away, and fullscreen stays F11's job, so the two compose freely.
#[derive(Clone, Copy)]
pub struct Presenting {
    mode: Mode,
    pub sidebar: bool,
}

/// What a window narrower than [`COLLAPSE`] puts back when it widens: whether the sidebar was up,
/// and where the divider was. The divider has to be kept as well as the sidebar, because
/// `GtkPaned` clamps it to a window narrower than it, and a dragged 400 px came back as 349 from a
/// 350 px window.
#[derive(Clone, Copy)]
pub struct Collapsed {
    sidebar: bool,
    width: i32,
}

impl App {
    /// Hide the sidebar while the window is narrower than [`COLLAPSE`], where `AdwOverlaySplitView`
    /// would have collapsed it, and put back what was there, at the width it was dragged to, on the
    /// way up. F9 still shows the sidebar while collapsed, beside the note rather than over it.
    pub(crate) fn install_collapse(self: &Rc<Self>) {
        let condition = adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            COLLAPSE,
            adw::LengthUnit::Sp,
        );
        let breakpoint = adw::Breakpoint::new(condition);
        // The clamp comes first: libadwaita lays the window out at its new width, sidebar and
        // all, before it applies a breakpoint. So the width kept is the last one the divider had
        // short of the far edge, where a clamp leaves it.
        let dragged = Rc::new(Cell::new(self.split.position()));
        self.split.connect_position_notify(glib::clone!(
            #[strong]
            dragged,
            move |split| {
                if split.position() < split.max_position() {
                    dragged.set(split.position());
                }
            }
        ));
        breakpoint.connect_apply(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                app.collapsed.set(Some(Collapsed {
                    sidebar: app.sidebar_shown(),
                    width: dragged.get(),
                }));
                app.show_sidebar(false);
            }
        ));
        breakpoint.connect_unapply(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                let Some(before) = app.collapsed.take() else {
                    return;
                };
                // Unless F9 has it up, when the width may be one the reader dragged since.
                if !app.sidebar_column.is_visible() {
                    app.split.set_position(before.width);
                }
                app.show_sidebar(before.sidebar);
            }
        ));
        self.window.add_breakpoint(breakpoint);
    }

    /// Whether the sidebar is up once presentation mode is over: what leaving it puts back while
    /// it lasts, what is on screen otherwise.
    fn sidebar_shown(&self) -> bool {
        self.presenting
            .get()
            .map_or_else(|| self.sidebar_column.is_visible(), |p| p.sidebar)
    }

    /// Put the sidebar up or down, or, while presenting, have leaving presentation do it.
    fn show_sidebar(&self, on: bool) {
        match self.presenting.get() {
            Some(p) => self.presenting.set(Some(Presenting { sidebar: on, ..p })),
            None => self.sidebar_column.set_visible(on),
        }
    }

    /// The sidebar and its width as a session keeps them: as they were before a narrow window or
    /// presentation mode took the sidebar away.
    pub(crate) fn sidebar_saved(&self) -> (bool, i32) {
        match self.collapsed.get() {
            Some(before) => (before.sidebar, before.width),
            None => (self.sidebar_shown(), self.split.position()),
        }
    }

    /// Put back the sidebar a session saved: now, or once the window is wide enough for it.
    pub(crate) fn restore_sidebar(&self, sidebar: bool, width: i32) {
        self.split.set_position(width);
        match self.collapsed.get() {
            Some(_) => self.collapsed.set(Some(Collapsed { sidebar, width })),
            None => self.sidebar_column.set_visible(sidebar),
        }
    }

    pub fn set_mode(self: &Rc<Self>, mode: Mode) {
        self.mode.set(mode);
        self.modes.set_icon_name(mode.icon());
        // Setting `active` re-enters the toggled handler, which compares against `self.mode` and
        // stops there, so this cannot loop.
        self.modes.set_active(mode == Mode::Split);
        self.show_chrome();
        self.apply_layout();
        self.save_session_soon();
    }

    /// Which of the editor column and the preview are on screen. Split shows both; presenting
    /// shows the preview alone, whatever mode the user will come back to — unless the tab renders
    /// itself, in which case it is the thing being presented and the preview stays away.
    fn apply_layout(self: &Rc<Self>) {
        let presenting = self.presenting.get().is_some();
        // A PDF, an image, a diff, a terminal: anything that is not a note in a buffer. There is
        // nothing for the preview to render, so hiding the document column would present a blank
        // window.
        let own_view = presenting && self.active().is_none();
        if self.shows_preview() && !own_view {
            self.ensure_preview();
        }
        self.content.set_visible(!presenting || own_view);
        self.hoist_find(presenting && !own_view);
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview
                .widget()
                .set_visible(self.shows_preview() && !own_view);
        }
        // The tab bars go with the rest of the chrome, since the column they live in stays. The
        // restore is unconditional: `AdwTabBar` reveals and hides itself, and leaving it hidden
        // here would take that decision away from it for good.
        for pane in self.panes.borrow().iter() {
            pane.bar.set_visible(!own_view);
        }
        if self.mode.get() == Mode::Split && !presenting {
            self.even_split();
        }
        if let Some(tab) = self.active() {
            self.render(&tab);
        }
    }

    /// Lend the presented pane's find bar to the editor column, or give every bar back.
    ///
    /// A bar lives in its pane, and presentation mode takes the whole pane tree off screen — the
    /// one thing the old window-wide bar had over this. `Ctrl+F` over a rendered note still has to
    /// reach something visible, so the pane being presented lends its bar to the column above for
    /// as long as that lasts. A tab that draws its own document keeps its pane, and its bar with
    /// it, so this only ever moves one bar and only while a *note* is being presented.
    fn hoist_find(&self, up: bool) {
        let active = self.pane();
        let column: &gtk::Widget = self.editor_column.upcast_ref();
        for pane in self.panes.borrow().iter() {
            let bar = pane.find.widget();
            if !(up && Rc::ptr_eq(pane, &active)) {
                pane.hold_find();
            } else if bar.parent().as_ref() != Some(column) {
                if let Some(old) = bar.parent().and_downcast::<gtk::Box>() {
                    old.remove(bar);
                }
                self.editor_column.append(bar);
                // Above the document, not below it: appending put it after the toasts.
                self.editor_column
                    .reorder_child_after(&self.toasts, Some(bar));
            }
        }
    }

    /// Put the handle back in the middle whenever the preview comes on screen. A `GtkPaned` keeps
    /// whatever position it was left at, and one that has never been allocated has none at all, so
    /// the editor's natural width could take the whole row and the preview open with nothing to
    /// show: the "clicked the button and nothing happened" report. Presentation mode never hit it,
    /// because there the editor column is hidden outright.
    fn even_split(self: &Rc<Self>) {
        if self.centre_handle() {
            return;
        }
        // No allocation yet, which is where a session restored straight into split mode lands.
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || {
                app.centre_handle();
            }
        ));
    }

    /// Centres the paned handle, or reports that there is no width to centre within yet.
    pub fn centre_handle(&self) -> bool {
        let width = self.paned.width();
        if width > 0 {
            self.paned.set_position(width / 2);
        }
        width > 0
    }

    /// Whether the rendered note is visible at all; nothing is rendered into a hidden preview.
    fn shows_preview(&self) -> bool {
        self.mode.get() == Mode::Split || self.presenting.get().is_some()
    }

    /// F5: the document alone, with the sidebar, the tab bars and both header bars gone. A state
    /// of the window rather than a [`Mode`], because it is a way of looking at the current tab
    /// instead of a layout to work in, and it is deliberately not part of the session: a window
    /// restored chromeless would be hard to get out of.
    ///
    /// A note is presented through the preview, rendered. A tab that draws its own document — a
    /// PDF, an image, a diff, a terminal — is presented as it is: `apply_layout` keeps the
    /// document column and takes the tab bars instead.
    pub fn set_presenting(self: &Rc<Self>, on: bool) {
        // A PDF presents itself: one whole page, and the zoom it had back afterwards.
        if let Some(pdf) = self.active_pdf() {
            pdf.set_presenting(on);
        }
        match (on, self.presenting.get()) {
            (true, None) => {
                // Presentation owns the chrome from here, so whatever typing faded comes back
                // first: the status bar is only unrevealed, and a hover shows it as it is.
                self.show_chrome();
                self.presenting.set(Some(Presenting {
                    mode: self.mode.get(),
                    sidebar: self.sidebar_column.is_visible(),
                }));
                self.sidebar_column.set_visible(false);
                self.toolbar.set_reveal_top_bars(false);
                self.toolbar.set_reveal_bottom_bars(false);
                // The status bar comes back over the document on a hover rather than pushing it
                // up, so the presentation never reflows under the pointer.
                self.toolbar.set_extend_content_to_bottom_edge(true);
                self.apply_layout();
            }
            (false, Some(before)) => {
                self.presenting.set(None);
                self.sidebar_column.set_visible(before.sidebar);
                self.toolbar.set_reveal_top_bars(true);
                self.toolbar.set_reveal_bottom_bars(true);
                self.toolbar.set_extend_content_to_bottom_edge(false);
                // Puts the layout back and, with presenting cleared, lets the chrome show again.
                self.set_mode(before.mode);
            }
            _ => {}
        }
    }

    /// While presenting, the status bar shows for as long as the pointer is over the strip at the
    /// bottom of the editor column where it sits, and goes again when it leaves. `at` is the
    /// pointer in the window's coordinates, `None` once it has left the window.
    pub fn hover_status(&self, at: Option<(f64, f64)>) {
        if self.presenting.get().is_none() {
            return;
        }
        let bar = self.statusbar.widget();
        let (_, height, _, _) = bar.measure(gtk::Orientation::Vertical, self.toolbar.width());
        let over = at
            .and_then(|(x, y)| {
                let point = graphene::Point::new(x as f32, y as f32);
                self.window.compute_point(&self.toolbar, &point)
            })
            .is_some_and(|p| {
                let (x, y) = (f64::from(p.x()), f64::from(p.y()));
                self.toolbar.contains(x, y) && y >= f64::from(self.toolbar.height() - height)
            });
        self.toolbar.set_reveal_bottom_bars(over);
    }

    fn ensure_preview(self: &Rc<Self>) {
        if self.preview.borrow().is_some() {
            return;
        }
        // The preview's assets come through the vault, so a note's images load whether the file
        // is on this disk or on a host. A window with no vault has only absolute keys, which the
        // resolver hands straight back.
        let vault = self.vault().cloned();
        let root = self.root();
        let preview = preview::Preview::new(
            move |rel: &str| match &vault {
                // `fetch` refuses a `rel` that climbs out lexically, on either backend. What it
                // cannot see is a symlink *inside* the vault pointing outside it, and the answer
                // here is handed to a WebView, so that is worth one `canonicalize`: a note
                // linking `escape.png -> ~/.ssh/id_rsa` must not render it.
                // `asset` first, because `![[img.png]]` names the file the way a wikilink does
                // and the index is what knows it lives in `Attachments/`.
                Some(vault) => {
                    let key = vault.asset(rel)?;
                    let path = vault.fetch(&key).ok()?;
                    let inside = match (path.canonicalize(), vault.root().canonicalize()) {
                        (Ok(real), Ok(root)) => real.starts_with(root),
                        // A remote vault's copy lives in the cache, not under the root, and the
                        // host already refused anything that escapes it there.
                        _ => vault.is_remote(),
                    };
                    inside.then_some((key, path))
                }
                // A loose file's key is its path, as its tab's is.
                None => {
                    let path = root.join(rel);
                    Some((path.to_string_lossy().into_owned(), path))
                }
            },
            self.inverted_images.clone(),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |target: &str| app.open_target(target)
            ),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |key: &str| app.invert_image(key)
            ),
        );
        self.paned.set_end_child(Some(preview.widget()));
        preview.set_zoom(self.zoom.get());
        // The preview follows the document zoom, so the wheel over it has to reach the same
        // setting the wheel over the editor does. Capture phase: WebKit answers a Ctrl+scroll
        // itself, with a zoom of its own that nothing else in the window knows about.
        zoom_on_wheel(
            preview.widget(),
            gtk::PropagationPhase::Capture,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |out, _| app.set_zoom(stepped_zoom(app.zoom.get(), out))
            ),
        );
        preview.connect_found(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |label| app.pane().find.set_matches_text(label)
        ));
        *self.preview.borrow_mut() = Some(preview);
    }

    /// Serve the preview's images again, after their look changed: WebKit keeps what it was
    /// served, so its cache goes first and the note is rendered again after it.
    pub fn reshow_preview_images(self: &Rc<Self>) {
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.forget_images(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || {
                    if let Some(tab) = app.active().filter(|_| app.shows_preview()) {
                        app.render(&tab);
                    }
                }
            ));
        }
    }

    pub fn render(self: &Rc<Self>, tab: &Rc<Tab>) {
        if !self.shows_preview() {
            return;
        }
        self.ensure_preview();
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.render(&tab.rel(), &tab.text());
            preview.scroll_to_line(tab.cursor_line());
        }
    }

    /// What follows an edit into the tab in front, [`RENDER`] after the last keystroke: the
    /// preview is re-rendered, and the status bar is asked for the word count again.
    ///
    /// One timer for both, because they are the same question — what does the buffer say now.
    /// The count used to be read only when a tab was opened or switched to, so a draft grew
    /// under a number that never moved; counting per keystroke instead would copy the whole
    /// buffer out on every key, and a number that settles a third of a second later reads the
    /// same to anyone watching it.
    pub fn queue_refresh(self: &Rc<Self>, tab: &Rc<Tab>) {
        if !self.is_active(tab) {
            return;
        }
        self.refresh.call(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || {
                app.sync_status();
                if let Some(tab) = app.active().filter(|_| app.shows_preview()) {
                    app.render(&tab);
                }
            }
        ));
    }

    /// A pane's find bar addressing the rendered preview, which is what it does while presenting.
    pub fn preview_find(&self, pane: &Pane, op: find::PreviewOp) {
        // A PDF gets first refusal: it is what the user is looking at, and it counts its own
        // matches rather than letting the bar count them.
        if let Some(pdf) = self.pdf_of(pane) {
            match op {
                find::PreviewOp::Find(text) => pdf.find(&text),
                find::PreviewOp::Next => pdf.step_match(true),
                find::PreviewOp::Previous => pdf.step_match(false),
                find::PreviewOp::Clear => pdf.find(""),
                // Only the Return moves a PDF. A live preview under a half-typed page number
                // renders pages nobody asked to read, and it has already left the page Back is
                // supposed to return to, so the committed jump would have nothing to remember.
                find::PreviewOp::Line { line, commit: true } => {
                    pdf.goto_page((line as usize).saturating_sub(1))
                }
                find::PreviewOp::Line { commit: false, .. } => {}
            }
            return;
        }
        let preview = self.preview.borrow();
        let Some(preview) = preview.as_ref() else {
            return;
        };
        match op {
            find::PreviewOp::Find(text) => preview.find(&text),
            find::PreviewOp::Next => preview.find_next(),
            find::PreviewOp::Previous => preview.find_previous(),
            find::PreviewOp::Clear => preview.find_clear(),
            find::PreviewOp::Line { line, .. } => preview.scroll_to_line(line),
        }
    }

    pub fn sync_scroll(&self, tab: &Rc<Tab>) {
        if !self.shows_preview() {
            return;
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.scroll_to_line(tab.cursor_line());
        }
    }

    // --- chrome --------------------------------------------------------------------------

    /// A change in `tab`'s buffer: the chrome fades while the user types, which is the point of
    /// the app (DESIGN.md), and the tab stops being a preview.
    ///
    /// Only a keystroke into *this* tab's own view counts. A reload writing into a background
    /// buffer is not the user typing, and neither is one arriving in another pane while the
    /// keyboard is here, nor one replacing the text under the caret (`Tab::is_loading`).
    pub fn on_edit(&self, tab: &Rc<Tab>) {
        if !tab.view.has_focus() || tab.is_loading() {
            return;
        }
        // Where the edit is. Consecutive keystrokes in one paragraph coalesce into one entry, so
        // typing leaves a mark rather than hundreds (`panes::coalesces`).
        self.mark_page(&tab.page);
        self.hide_chrome();
        self.promote(&tab.page);
    }

    /// A key pressed anywhere in the window, heard before whatever has the keyboard: one that moves
    /// through the document the keyboard is in fades the chrome, as typing does (DESIGN.md). Not
    /// under a completion popup, whose arrows pick a row, and not while presenting, which owns the
    /// chrome and shows the status bar on a hover.
    pub fn on_key(&self, key: gdk::Key, state: gdk::ModifierType) {
        if self.presenting.get().is_some() {
            return;
        }
        let Some(focus) = self.focused() else {
            return;
        };
        let editor = self
            .open_tabs()
            .into_iter()
            .find(|tab| tab.view.upcast_ref::<gtk::Widget>() == &focus);
        let preview = || {
            let preview = self.preview.borrow();
            preview.as_ref().is_some_and(|p| p.widget() == &focus)
        };
        let surface = match editor {
            Some(tab) if tab.popup_shown() => return,
            Some(_) => Surface::Text,
            None if self.pdfs().iter().any(|pdf| pdf.key_target() == focus) => Surface::Pdf,
            None if preview() => Surface::Preview,
            None => return,
        };
        if navigates(key, state, surface) {
            self.hide_chrome();
        }
    }

    /// `Root` and `GtkWindow` both spell this `focus`, so the window's one is named here once.
    fn focused(&self) -> Option<gtk::Widget> {
        gtk::prelude::GtkWindowExt::focus(&self.window)
    }

    /// Fade what the focus mode preference says to: nothing at None; the chrome at Medium; and at
    /// High the chrome, the window's dividers, every pane but the one being written in, and the
    /// text away from the caret (DESIGN.md, Chrome auto-hide).
    pub fn hide_chrome(&self) {
        let level = self.config.borrow().focus_mode;
        if level == FocusMode::None || self.chrome_hidden.get() || self.chrome_busy() {
            return;
        }
        self.chrome_hidden.set(true);
        for widget in self.chrome() {
            widget.add_css_class("chrome-hidden");
        }
        if level != FocusMode::High {
            return;
        }
        // The lines between the panes and along their edges too, which would otherwise frame the
        // panes that are receding (`.dividers-hidden` in `install_chrome_css`).
        self.window.add_css_class("dividers-hidden");
        let active = self.pane();
        for pane in self.panes.borrow().iter() {
            if !Rc::ptr_eq(pane, &active) {
                pane.widget().add_css_class("chrome-away");
            }
        }
        if let Some(tab) = self.active() {
            tab.set_fade(true);
        }
    }

    /// Put back everything [`App::hide_chrome`] can have faded, at whatever level it was faded.
    pub fn show_chrome(&self) {
        // Presentation owns the chrome while it lasts: a pointer that crosses the window must not
        // undo it, or the mode is useless.
        if self.presenting.get().is_some() || !self.chrome_hidden.replace(false) {
            return;
        }
        for widget in self.chrome() {
            widget.remove_css_class("chrome-hidden");
        }
        self.window.remove_css_class("dividers-hidden");
        for pane in self.panes.borrow().iter() {
            pane.widget().remove_css_class("chrome-away");
        }
        for tab in self.open_tabs() {
            tab.set_fade(false);
        }
    }

    /// What fades at Medium: both header bars, the status bar, every pane's tab bar, the
    /// sidebar's panes and the minimaps.
    fn chrome(&self) -> Vec<gtk::Widget> {
        let mut chrome: Vec<gtk::Widget> = vec![
            self.sidebar_header.clone().upcast(),
            self.header.clone().upcast(),
            self.statusbar.widget().clone(),
        ];
        chrome.extend(self.panes.borrow().iter().map(|p| p.bar.clone().upcast()));
        chrome.extend(self.sidebar.get().map(|s| s.widget().clone()));
        chrome.extend(self.open_tabs().iter().map(|t| t.minimap().clone()));
        chrome
    }

    /// Never fade over something that is waiting for an answer: a dialog, a banner or an open
    /// popover. An open find bar is not one: it stays, and its matches stay unveiled
    /// (`fade::cover`), while the rest fades around the note being written in.
    fn chrome_busy(&self) -> bool {
        if self.window.visible_dialog().is_some() {
            return true;
        }
        let in_popover = self
            .focused()
            .is_some_and(|w| w.ancestor(gtk::Popover::static_type()).is_some());
        in_popover || self.active().is_some_and(|tab| tab.banner.is_revealed())
    }
}

/// What has the keyboard when a key may be a step through a document.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// An editor tab's text: a note, code or a CSV.
    Text,
    Pdf,
    /// The rendered note beside the editor.
    Preview,
}

/// Whether `key` moves through a document on `surface` rather than writing in it or firing a
/// command: the caret keys anywhere, which are the arrows, Home, End, Page Up and Page Down, alone
/// or with Shift and Ctrl; and where the document is read rather than written, Space too, which
/// pages there, and a PDF's own `n` and `p`. Alt or Super makes a chord instead — Back, Forward,
/// a caret added above — and Caps Lock and Num Lock change nothing.
pub fn navigates(key: gdk::Key, state: gdk::ModifierType, surface: Surface) -> bool {
    use gdk::{Key, ModifierType as M};
    if state.intersects(M::ALT_MASK | M::SUPER_MASK | M::META_MASK | M::HYPER_MASK) {
        return false;
    }
    match key {
        Key::Up | Key::Down | Key::Left | Key::Right => true,
        Key::KP_Up | Key::KP_Down | Key::KP_Left | Key::KP_Right => true,
        Key::Home | Key::End | Key::Page_Up | Key::Page_Down => true,
        Key::KP_Home | Key::KP_End | Key::KP_Page_Up | Key::KP_Page_Down => true,
        Key::space | Key::KP_Space => surface != Surface::Text,
        // `Ctrl+N` is New Note, and a capital is Shift.
        Key::n | Key::p => surface == Surface::Pdf && !state.contains(M::CONTROL_MASK),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{Surface, navigates};
    use gtk::gdk::{Key, ModifierType as M};

    /// The caret keys step through every document with any mix of Shift and Ctrl and none of Alt,
    /// and the reading keys only through one that is read.
    #[test]
    fn a_step_through_the_document_is_a_caret_key_or_a_reading_one() {
        let chords = [
            M::empty(),
            M::SHIFT_MASK,
            M::CONTROL_MASK,
            M::CONTROL_MASK | M::SHIFT_MASK,
            M::LOCK_MASK,
        ];
        for key in [Key::Up, Key::Right, Key::Home, Key::Page_Down, Key::KP_End] {
            for state in chords {
                assert!(navigates(key, state, Surface::Text), "{key:?} {state:?}");
                assert!(navigates(key, state, Surface::Preview), "{key:?} {state:?}");
            }
            assert!(
                !navigates(key, M::ALT_MASK, Surface::Text),
                "Back and Forward"
            );
            assert!(!navigates(key, M::ALT_MASK | M::SHIFT_MASK, Surface::Pdf));
        }
        assert!(
            !navigates(Key::space, M::empty(), Surface::Text),
            "a space is typed"
        );
        assert!(navigates(Key::space, M::SHIFT_MASK, Surface::Preview));
        assert!(navigates(Key::space, M::empty(), Surface::Pdf));
        assert!(navigates(Key::n, M::empty(), Surface::Pdf));
        assert!(
            !navigates(Key::n, M::CONTROL_MASK, Surface::Pdf),
            "New Note"
        );
        assert!(!navigates(Key::n, M::empty(), Surface::Preview));
        assert!(!navigates(Key::a, M::empty(), Surface::Pdf));
        assert!(!navigates(Key::Return, M::empty(), Surface::Text));
    }
}
