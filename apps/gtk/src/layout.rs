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

/// Sidebar collapse width in sp: 280 px sidebar plus the 480 px column floor.
/// Larger interface fonts collapse sooner because sidebar rows grow.
const COLLAPSE: f64 = 760.0;

/// What leaving presentation mode has to put back. The window's size is not part of it: F5 only
/// takes the chrome away, and fullscreen stays F11's job, so the two compose freely.
#[derive(Clone, Copy)]
pub struct Presenting {
    mode: Mode,
    pub sidebar: bool,
}

thread_local! {
    /// The rule [`App::lift_toasts`] lifts the toasts by, on the display, and the height it was
    /// written for: written again only when the status bar's height changes, with the text size.
    static LIFT: RefCell<Option<(gtk::CssProvider, i32)>> = const { RefCell::new(None) };
}

/// Remember sidebar visibility and divider width while the window is collapsed.
/// GtkPaned clamps positions in narrow windows, so visibility alone cannot restore the chosen width.
#[derive(Clone, Copy)]
pub struct Collapsed {
    sidebar: bool,
    width: i32,
}

impl App {
    /// Hide sidebar below [`COLLAPSE`] and restore its prior width when widening.
    /// F9 can still show it beside the note while collapsed.
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
        // Leaving F5 sets the mode it began in again, which brings the preview back where its
        // divider was rather than on screen anew.
        let opened = mode == Mode::Split && self.mode.get() != Mode::Split;
        self.mode.set(mode);
        self.modes.set_icon_name(mode.icon());
        // Setting `active` re-enters the toggled handler, which compares against `self.mode` and
        // stops there, so this cannot loop.
        self.modes.set_active(mode == Mode::Split);
        self.show_chrome();
        self.apply_layout();
        if opened {
            self.even_split();
        }
        self.save_session_soon();
    }

    /// [`App::lay_out`], and the tab in front rendered wherever the preview shows.
    fn apply_layout(self: &Rc<Self>) {
        self.lay_out();
        if let Some(tab) = self.active() {
            self.render(&tab);
        }
    }

    /// Apply editor, split or presentation layout.
    ///
    /// Split places preview beside the pane tree. Presentation keeps only the active pane,
    /// without its tab bar. Text uses a preview overlay (`Pane::cover`) so find and Ctrl+Tab stay
    /// pane-local; other documents present themselves, fitting PDFs and diagrams to a page.
    /// [`App::sync_active`] reapplies layout when the active tab changes during presentation.
    pub(crate) fn lay_out(self: &Rc<Self>) {
        let presenting = self.presenting.get().is_some();
        let active = self.pane();
        let rendered = presenting && self.active().is_some();
        if rendered || self.mode.get() == Mode::Split && !presenting {
            self.ensure_preview();
        }
        let panes = self.panes.borrow().clone();
        panes::isolate(&panes, presenting.then_some(&*active));
        // The restore is unconditional: `AdwTabBar` reveals and hides itself, and leaving it
        // hidden here would take that decision away from it for good.
        for pane in &panes {
            pane.bar.set_visible(!presenting);
            pane.tabs
                .set_visible(!(rendered && Rc::ptr_eq(pane, &active)));
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            let widget = preview.widget();
            let split = widget.parent().as_ref() == Some(self.paned.upcast_ref());
            match presenting {
                true => {
                    if split {
                        self.paned.set_end_child(gtk::Widget::NONE);
                    }
                    active.cover(widget);
                }
                // Back from the pane presented last, which may have closed since.
                false if !split => {
                    widget.unparent();
                    self.paned.set_end_child(Some(widget));
                }
                false => {}
            }
            let shown = rendered || self.mode.get() == Mode::Split && !presenting;
            widget.set_visible(shown);
            match shown {
                true => self.preview_idle.cancel(),
                false => self.preview_idle.call_once(glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move || app.drop_preview()
                )),
            }
        }
        if presenting {
            if let Some(pdf) = self.active_pdf() {
                pdf.set_presenting(true);
            }
            if let Some(diagram) = self.active_diagram() {
                diagram.set_presenting(true, self.ring_at.get());
            }
        }
    }

    /// Centre the split when preview first appears.
    /// An unallocated paned has no position, otherwise natural editor width can leave preview with no room.
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

    /// Present only the active document with F5.
    /// This temporary window state is excluded from sessions so restored windows retain reachable chrome;
    /// [`App::lay_out`] handles each document kind.
    pub fn set_presenting(self: &Rc<Self>, on: bool) {
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
                // The drawing tools go with the chrome, and the tool in hand is put down.
                self.sync_drawing();
                self.sync_status();
                self.focus_presented();
            }
            (false, Some(before)) => {
                self.presenting.set(None);
                // Each PDF gets the zoom it had back, those a held `Ctrl+Tab` presented too, and
                // each diagram its zoom and its ring.
                for pdf in self.pdfs() {
                    pdf.set_presenting(false);
                }
                for diagram in self.diagrams() {
                    diagram.set_presenting(false, self.ring_at.get());
                }
                // A PDF's ring comes back as it was too, but not the tool `sync_drawing` would
                // pick up again: that stays down until it is picked up from the ring.
                if let Some(pdf) = self.active_pdf() {
                    pdf.set_drawing(self.drawing.get(), self.ring_at.get());
                }
                self.lift_toasts(None);
                self.sidebar_column.set_visible(before.sidebar);
                self.toolbar.set_reveal_top_bars(true);
                self.toolbar.set_reveal_bottom_bars(true);
                self.toolbar.set_extend_content_to_bottom_edge(false);
                // Puts the layout back and, with presenting cleared, lets the chrome show again.
                self.set_mode(before.mode);
                // A note presented rendered had its editor hidden, which took the keyboard away.
                self.focus_document(&self.pane());
            }
            _ => {}
        }
        // The bar searches the rendered note while one is presented, which its toggles say.
        self.retarget_find(&self.pane());
    }

    /// Reveal the presentation status bar while `at` is over its bottom strip.
    ///
    /// `at` uses window coordinates; `None` means the pointer left. An open bar menu keeps the
    /// bar visible because popovers take the pointer away from the window.
    pub fn hover_status(&self, at: Option<(f64, f64)>) {
        if self.presenting.get().is_none() {
            return;
        }
        let bar = self.statusbar.widget();
        let (_, height, _, _) = bar.measure(gtk::Orientation::Vertical, self.toolbar.width());
        let over = self.statusbar.menu_open()
            || at
                .and_then(|(x, y)| {
                    let point = graphene::Point::new(x as f32, y as f32);
                    self.window.compute_point(&self.toolbar, &point)
                })
                .is_some_and(|p| {
                    let (x, y) = (f64::from(p.x()), f64::from(p.y()));
                    self.toolbar.contains(x, y) && y >= f64::from(self.toolbar.height() - height)
                });
        self.toolbar.set_reveal_bottom_bars(over);
        // The toasts are part of the document the bar comes up over, so they go up with it.
        self.lift_toasts(over.then_some(height));
    }

    /// Pick the presentation status-bar widget at `(x, y)` in `from` coordinates, if present.
    ///
    /// GTK normally targets the last motion widget. On Xvfb or X11 without frame sync, a newly
    /// revealed bar can receive a press before GTK updates that target. A fresh pick avoids
    /// sending the press to the document beneath it.
    pub fn bar_widget_at(&self, from: &gtk::Widget, x: f64, y: f64) -> Option<gtk::Widget> {
        self.presenting.get()?;
        let point = from.compute_point(&self.window, &graphene::Point::new(x as f32, y as f32))?;
        let bar = self.statusbar.widget();
        self.window
            .pick(point.x().into(), point.y().into(), gtk::PickFlags::DEFAULT)
            .filter(|picked| picked == bar || picked.is_ancestor(bar))
    }

    /// The pointer in the window's coordinates, `None` while it is off the window.
    pub fn pointer(&self) -> Option<(f64, f64)> {
        let pointer = WidgetExt::display(&self.window).default_seat()?.pointer()?;
        let (x, y, _) = self.window.surface()?.device_position(&pointer)?;
        let (dx, dy) = self.window.surface_transform();
        Some((x - dx, y - dy))
    }

    /// Lift toasts by presentation status-bar height `by`, or reset with `None`.
    /// A height-specific CSS class also positions later toasts, animating with widgets::FADE_MS.
    fn lift_toasts(&self, by: Option<i32>) {
        if let Some(by) = by {
            LIFT.with_borrow_mut(|lift| {
                if lift.as_ref().is_some_and(|(_, at)| *at == by) {
                    return;
                }
                let provider = match lift.take() {
                    Some((provider, _)) => provider,
                    None => {
                        let provider = gtk::CssProvider::new();
                        gtk::style_context_add_provider_for_display(
                            &WidgetExt::display(&self.window),
                            &provider,
                            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                        );
                        provider
                    }
                };
                provider.load_from_string(&format!(
                    ".accent-lifted > .accent-toasts {{ transform: translateY(-{by}px); }}"
                ));
                *lift = Some((provider, by));
            });
        }
        crate::widgets::set_class(self.toasts.widget(), "accent-lifted", by.is_some());
    }

    /// Focus the presented document on F5 and tab cycling so its normal reading or shell keys work.
    /// Window handlers still intercept F5 and Escape to leave presentation.
    pub(crate) fn focus_presented(&self) {
        let widget: gtk::Widget = match self.active_doc() {
            Some(Doc::Text(_)) => match self.preview.borrow().as_ref() {
                Some(preview) => preview.widget().clone(),
                None => return,
            },
            // An image's scroller pages it with the keys a document scrolls by.
            Some(Doc::Image(image)) => image.page.child(),
            _ => return self.focus_document(&self.pane()),
        };
        // From an idle, as `focus_document` does: the preview has only just been laid over the
        // pane, and a widget not mapped yet is not one GTK hands the keyboard to.
        glib::idle_add_local_once(move || {
            widget.grab_focus();
        });
    }

    /// Release a hidden preview after [`preview::idle_for`], ending its private web process.
    /// The next visible render rebuilds and scrolls to the caret. Showing cancels pending release.
    fn drop_preview(&self) {
        let Some(preview) = self.preview.take() else {
            return;
        };
        let widget = preview.widget();
        match widget.parent() {
            Some(parent) if parent == *self.paned.upcast_ref::<gtk::Widget>() => {
                self.paned.set_end_child(gtk::Widget::NONE)
            }
            _ => widget.unparent(),
        }
    }

    fn ensure_preview(self: &Rc<Self>) {
        if self.preview.borrow().is_some() {
            return;
        }
        let resolve = self.asset_resolver();
        let preview = preview::Preview::new(
            move |rel: &str| resolve(rel),
            self.inverted_images.clone(),
            self.web_images.clone(),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |target: &str| app.open_target(target)
            ),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tag: &str| app.show_tag(tag)
            ),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |key: &str| app.invert_image(key)
            ),
        );
        self.paned.set_end_child(Some(preview.widget()));
        preview.set_zoom(self.zoom.get());
        preview.set_column(&self.config.borrow());
        // The preview follows the document zoom, so the wheel over it has to reach the same
        // setting the wheel over the editor does. Capture phase: WebKit answers a Ctrl+scroll
        // itself, with a zoom of its own that nothing else in the window knows about.
        zoom_on_wheel(
            preview.widget(),
            gtk::PropagationPhase::Capture,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |out| app.set_zoom(stepped_zoom(app.zoom.get(), out))
            ),
        );
        preview.connect_found(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |label| app.pane().find.set_matches_text(label)
        ));
        preview.connect_lost(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || {
                if let Some(tab) = app.active().filter(|_| app.shows_preview()) {
                    app.render(&tab);
                }
            }
        ));
        *self.preview.borrow_mut() = Some(preview);
    }

    /// Resolve preview assets through the vault to local files, downloading remote assets as needed.
    /// Loose notes use their own folder through preview::resolve_asset.
    pub(crate) fn asset_resolver(&self) -> Arc<preview::Resolve> {
        let vault = self.vault().cloned();
        Arc::new(move |rel: &str| match &vault {
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
            None => None,
        })
    }

    /// Serve `rel` to the preview again, or every file under it, if it was served: it changed,
    /// went, moved or was inverted, and WebKit would answer the next render with what it holds.
    /// The render asks for it alone again (`Preview::forget_image`).
    pub fn reshow_preview_image(self: &Rc<Self>, rel: &str) {
        let forgot = self
            .preview
            .borrow()
            .as_ref()
            .is_some_and(|p| p.forget_image(rel));
        if forgot && let Some(tab) = self.active().filter(|_| self.shows_preview()) {
            self.render(&tab);
        }
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

    /// Debounce preview rendering and word counting together by [`RENDER`].
    ///
    /// Both consume current buffer text. A shared timer keeps counts current without copying
    /// the entire buffer on each keystroke.
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
                find::PreviewOp::Find(text, options) => pdf.find(&text, options, true),
                find::PreviewOp::Next => pdf.step_match(true),
                find::PreviewOp::Previous => pdf.step_match(false),
                find::PreviewOp::Clear => pdf.find("", Default::default(), false),
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
            find::PreviewOp::Find(text, options) => preview.find(&text, options),
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

    /// Fade chrome and keep a preview tab after typing in its active view.
    ///
    /// Reloads, background edits and loading replacements are not user typing, so they must
    /// not trigger focus mode or change preview status.
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

    /// Fade chrome on document-navigation keys heard before focused widgets.
    /// Completion-popup arrows and presentation mode are excluded because they have separate UI ownership.
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
    pub(crate) fn focused(&self) -> Option<gtk::Widget> {
        gtk::prelude::GtkWindowExt::focus(&self.window)
    }

    /// Apply focus mode: None preserves chrome, Medium fades chrome, and High also fades
    /// dividers, inactive panes and text away from the caret.
    pub fn hide_chrome(&self) {
        let level = self.config.borrow().focus_mode;
        if level == FocusMode::None || self.chrome_hidden.get() || self.chrome_busy() {
            return;
        }
        self.chrome_hidden.set(true);
        for widget in self.chrome() {
            widget.add_css_class("chrome-hidden");
        }
        // And the lines the scrolled windows draw against the bars, which would stay behind
        // (`.bars-hidden` in `install_chrome_css`).
        self.window.add_css_class("bars-hidden");
        if level != FocusMode::High {
            return;
        }
        // The lines between the panes too, which would otherwise frame the panes that are
        // receding (`.dividers-hidden`).
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
        self.window.remove_css_class("bars-hidden");
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

    /// Suspend fading for unanswered dialogs, banners and popovers.
    /// Find bars remain visible with unveiled matches while surrounding text fades.
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

/// Whether `key` navigates `surface`: arrows, Home/End and Page Up/Down with Shift or Ctrl,
/// plus Space and PDF n/p on reading surfaces. Alt/Super denote commands; lock modifiers do not matter.
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
