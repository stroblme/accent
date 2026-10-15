//! Wire PDF view signals, page menus and keys to tab or window actions in `open`.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::protocol::{Asker, Request};
use super::tab::{PAGE_ACTIONS, PdfTab};
use super::{self as pdfview, PdfView};

impl PdfTab {
    /// Hook up one of the two views: what it wants rendered, and what comes back.
    pub(super) fn wire(self: &Rc<Self>, view: &PdfView) {
        self.wire_strip(view);
        view.connect_reply(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, reply| tab.on_reply(reply)
        ));
        // A page's stand-in landed. It is all the strip paints of a page, and the strip hears of
        // it only here: what it asks for is answered to the reading view, which repaints itself.
        view.connect_lowres(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page| {
                tab.thumbs.queue_draw();
                tab.band_landed(page);
            }
        ));
        view.connect_page(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page| {
                tab.thumbs.set_framed(page);
                tab.on_page.emit(&tab);
            }
        ));
        view.connect_shown(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || {
                tab.ask_links();
                if tab.wants_inks() {
                    tab.ask_inks();
                }
            }
        ));
        view.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |view, x, y| tab.click(view, x, y)
        ));
        view.connect_clicked(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |view, x, y, state| tab.clicked_highlight(view, x, y, state)
        ));
        view.connect_select(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, span| tab.selected_between(span)
        ));
        view.connect_motion(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |view, x, y| {
                // The pointer only changes when the answer does: a GDK call per pixel of travel
                // is what the editor's link hover deliberately avoids too.
                // While a tool is out, the cursor says so and nothing here takes it back: the
                // page is not text to be selected, and a link is not to be followed.
                if view.mode() != pdfview::Mode::Select {
                    return;
                }
                let over = tab.link_at(view, x, y).is_some();
                let on_page = view.page_point(x, y).is_some();
                view.set_cursor_from_name(Some(match (over, on_page) {
                    (true, _) => "pointer",
                    // A page is text to be dragged across, and says so before anyone tries.
                    (false, true) => "text",
                    (false, false) => "default",
                }));
            }
        ));
        view.connect_ink(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page, points| {
                let mode = tab.view.mode();
                let style = tab.view.ink_style(mode);
                match (mode.shapes(), points.as_slice()) {
                    (true, &[a, b]) => {
                        if let Some(shape) = pdfview::shape_of(mode, a, b) {
                            tab.ask(Request::Shape { page, shape, style });
                        }
                    }
                    (true, _) => {}
                    (false, _) => tab.ask(Request::Ink {
                        page,
                        points,
                        style,
                    }),
                }
            }
        ));
        view.connect_erase(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page, id, partial, joined| tab.ask(Request::Erase {
                page,
                id,
                joined,
                partial
            })
        ));
        view.connect_transform(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page, id, matrix| tab.ask(Request::Transform { page, id, matrix })
        ));
    }

    /// Wire render requests and page clicks shared by reading and thumbnail views. Selection,
    /// pointer and page-content handling remain exclusive to the reading view because the strip
    /// is navigation.
    pub(super) fn wire_strip(self: &Rc<Self>, view: &PdfView) {
        view.connect_wants(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |view, scale, theme, wants| {
                tab.ask(Request::Tiles {
                    from: match *view == tab.thumbs {
                        true => Asker::Strip,
                        false => Asker::Reader,
                    },
                    scale,
                    theme,
                    wants,
                });
            }
        ));
        view.connect_goto(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page| tab.goto_page(page)
        ));
    }

    /// Page context menu: Go to Source when available, selection copy actions, page edits and
    /// highlight export. Commands target the pointer's page; palette and status-bar commands
    /// target the reading page. Read-only and remote refusals are reported by toast. Drawing
    /// tools use the ring.
    ///
    /// Use `win.` actions for palette entries and rebindable accelerators; Ctrl+C stays in the
    /// tab's key controller.
    pub(super) fn wire_menu(self: &Rc<Self>) {
        let secondary = gtk::GestureClick::builder()
            .button(gtk::gdk::BUTTON_SECONDARY)
            .build();
        secondary.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, _, x, y| {
                tab.selection_menu(x, y);
            }
        ));
        self.view.add_controller(secondary);
    }

    /// Put the menu under the pointer, at a point in the view's coordinates.
    pub(crate) fn selection_menu(self: &Rc<Self>, x: f64, y: f64) -> gtk::PopoverMenu {
        let menu = gio::Menu::new();
        // Window actions, in sections, the way the terminal's menu is built: that is what puts
        // them in the palette and lets them be rebound, which a tab-local group could not.
        self.pointed.set(self.view.page_point(x, y));
        let missing = self.without_synctex.get();
        let source = crate::synctex::menu_item("win.pdf-go-to-source", missing);
        let open = gio::Menu::new();
        open.append_item(&source);
        menu.append_section(None, &open);
        if !self.selected.borrow().is_empty() {
            let clipboard = gio::Menu::new();
            for action in ["win.pdf-copy", "win.pdf-copy-link"] {
                clipboard.append(Some(crate::actions::label_of(action)), Some(action));
            }
            menu.append_section(None, &clipboard);
        }
        let file = gio::Menu::new();
        for action in PAGE_ACTIONS
            .into_iter()
            .chain(["win.pdf-export-highlights"])
        {
            file.append(Some(crate::actions::label_of(action)), Some(action));
        }
        menu.append_section(None, &file);
        // Parented to the box rather than to the view, and pointed at the box's own coordinates:
        // a popover hung off a widget with a `size_allocate` of its own never re-presents and
        // freezes at its first-frame size (DESIGN.md, States).
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = self.view.compute_point(&self.host, &at).unwrap_or(at);
        let anchor = gtk::gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1);
        let popover = crate::widgets::popup_menu(&self.host, &menu, Some(anchor));
        // From an idle: an item's action runs after `closed`, and Go to Source reads the point.
        popover.connect_closed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| {
                glib::idle_add_local_once(move || tab.pointed.set(None));
            }
        ));
        popover
    }

    /// The keys a reader uses. Page Up, Page Down, Home and End are `GtkScrolledWindow`'s own;
    /// the arrows are not — it binds a scroll step to `Ctrl+Up`/`Ctrl+Down` and leaves the bare
    /// arrow keys to move the focus off the page — so they are wired here.
    pub(super) fn wire_keys(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                // Through the window's actions, not past them: the key is on the tab because
                // `Ctrl+C` and `Ctrl+Z` belong to whatever has the keyboard, but what they do is
                // the command the palette and the menus name.
                if key == gtk::gdk::Key::c && state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                    tab.run("win.pdf-copy");
                    return glib::Propagation::Stop;
                }
                // Undo and Redo, on the tab like Copy, with a tool in hand or not: `Ctrl+Z`
                // belongs to whatever has the keyboard, and here that is the page, whose strokes
                // and page edits are one history. Redo is `Ctrl+Shift+Z` or `Ctrl+Y`, the two a
                // note's own undo answers to.
                if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                    let history = match (key.to_lower(), shift) {
                        (gtk::gdk::Key::z, false) => Some("win.pdf-undo"),
                        (gtk::gdk::Key::z, true) | (gtk::gdk::Key::y, false) => {
                            Some("win.pdf-redo")
                        }
                        _ => None,
                    };
                    if let Some(action) = history {
                        tab.run(action);
                        return glib::Propagation::Stop;
                    }
                }
                // `Escape` puts the pen down.
                if tab.mode() != pdfview::Mode::Select && key == gtk::gdk::Key::Escape {
                    tab.set_mode(pdfview::Mode::Select);
                    return glib::Propagation::Stop;
                }
                // `Space`, `n`, `p` and the arrows stay bare keys here rather than joining the
                // table: an application accelerator is dispatched at the window ahead of whatever
                // has the keyboard, so a bare `space` in it would stop every entry in the app
                // from taking one. Paging still goes through the window's own commands below.
                //
                // Alt+Left and Alt+Right are Back and Forward, and Ctrl with an arrow is the
                // scroller's own step: only the bare key reads the document.
                let bare = !state.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK
                        | gtk::gdk::ModifierType::ALT_MASK
                        | gtk::gdk::ModifierType::SUPER_MASK,
                );
                match key {
                    gtk::gdk::Key::space if shift => tab.page(false),
                    gtk::gdk::Key::space => tab.page(true),
                    gtk::gdk::Key::n => tab.page(true),
                    gtk::gdk::Key::p => tab.page(false),
                    // A page back and a page forth whatever the zoom: horizontal movement is
                    // Shift and the wheel, and one key cannot mean two things.
                    gtk::gdk::Key::Left if bare => tab.page(false),
                    gtk::gdk::Key::Right if bare => tab.page(true),
                    gtk::gdk::Key::Up if bare => tab.view.scroll_step(false),
                    gtk::gdk::Key::Down if bare => tab.view.scroll_step(true),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        // Ctrl let go with the pointer still on the link: the preview was only ever the
        // modifier's, so it goes with it.
        keys.connect_key_released(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, key, _, _| {
                if matches!(key, gtk::gdk::Key::Control_L | gtk::gdk::Key::Control_R) {
                    tab.hide_preview();
                }
            }
        ));
        self.view.add_controller(keys);
    }

    /// Dispatch paging keys through window actions so palette and scripted commands share
    /// behaviour. Bind bare keys here rather than globally so entries elsewhere retain space and
    /// arrows.
    fn page(&self, forward: bool) {
        self.run(match forward {
            true => "win.pdf-next-page",
            false => "win.pdf-previous-page",
        });
    }

    /// Fire one of the window's commands from the page. What a key over a PDF does is a command
    /// like any other, so it lists in the palette and can be rebound.
    fn run(&self, action: &str) {
        let _ = self.view.activate_action(action, None);
    }
}
