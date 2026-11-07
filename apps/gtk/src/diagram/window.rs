//! Window integration for diagram opening, commands, file watching and saves through
//! the same etag gate as notes.

use accent_api::Error;
use accent_core::fs::{Digest, Etag};
use accent_drawio::File;

use super::{DiagramTab, Tool};
use crate::save::SavesModel;
use crate::*;

/// Embedded-image size limit; base64 adds a third to the diagram file size.
const MAX_IMAGE: usize = 2 * 1024 * 1024;

/// What reading a diagram off the disk came to.
pub(crate) enum Parsed {
    Diagram(File, Etag, Digest),
    /// Not something to draw on, and why: a status page's title and sentence.
    Refused(&'static str, String),
    Failed(Error),
}

/// Read and parse on a worker thread: the file is XML of up to 16 MiB.
fn parse(read: accent_api::Result<accent_core::fs::Read>) -> Parsed {
    match read {
        Ok(accent_core::fs::Read::Text(text)) => parse_text(text),
        Ok(accent_core::fs::Read::Binary { .. }) => Parsed::Refused(
            "Not a Diagram",
            "A draw.io diagram must be a text file.".to_string(),
        ),
        Ok(accent_core::fs::Read::TooLarge { .. }) => Parsed::Refused(
            "File Too Large",
            format!(
                "This file exceeds the {} MiB text file limit.",
                accent_core::fs::MAX_TEXT / (1024 * 1024)
            ),
        ),
        Err(e) => Parsed::Failed(e),
    }
}

fn parse_text(text: accent_core::fs::Text) -> Parsed {
    match File::from_bytes(text.text.as_bytes()) {
        Ok(file) => Parsed::Diagram(file, text.etag, text.digest()),
        Err(e) => Parsed::Refused("Not a Diagram", format!("Could not read this file: {e}.")),
    }
}

impl App {
    /// Read and parse on a worker, then open a tab once its page is ready.
    pub(crate) fn open_diagram(self: &Rc<Self>, key: &str, path: &Path, how: Opened) {
        let vault = self.vault().filter(|_| !doc::is_loose_key(key)).cloned();
        let (key, path) = (key.to_string(), path.to_path_buf());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let parsed = crate::work::off_thread("diagram reader", {
                let (key, path) = (key.clone(), path.clone());
                move || {
                    #[cfg(feature = "bench")]
                    crate::bench::hold(&crate::bench::SLOW_READ_MS);
                    parse(match vault {
                        Some(vault) => vault.read_text(&key),
                        None => accent_core::fs::read_text(&path).map_err(Into::into),
                    })
                }
            })
            .await;
            // Its tab closed while it was on its way, or holds what an earlier read brought.
            let Some(app) = weak.upgrade().filter(|app| app.holds(&key)) else {
                return;
            };
            if let Some(parsed) = parsed {
                app.adopt_diagram(&key, &path, parsed, how);
            }
        });
    }

    /// An `.xml` file whose bytes turned out to be a diagram: parsed on a worker, then opened as
    /// one. The text is already here, read by the text tab's opener.
    pub(crate) fn open_diagram_text(
        self: &Rc<Self>,
        key: &str,
        text: accent_core::fs::Text,
        how: Opened,
    ) {
        let key = key.to_string();
        let path = self.root().join(&key);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let parsed = crate::work::off_thread("diagram reader", move || parse_text(text)).await;
            if let (Some(app), Some(parsed)) = (weak.upgrade(), parsed) {
                app.adopt_diagram(&key, &path, parsed, how);
            }
        });
    }

    fn adopt_diagram(self: &Rc<Self>, key: &str, path: &Path, parsed: Parsed, how: Opened) {
        let (file, etag, digest) = match parsed {
            Parsed::Diagram(file, etag, digest) => (file, etag, digest),
            Parsed::Refused(title, body) => return self.open_status(key, title, &body, how),
            Parsed::Failed(e) => return self.read_failed(key, e),
        };
        let Some(held) = self.take_held(key) else {
            return;
        };
        let place = self
            .vault()
            .and_then(|v| v.session().diagram.get(key).copied())
            .unwrap_or_default();
        let tab = super::open(
            key,
            path,
            doc::file_name(key),
            &fileops::display_path(&self.root(), key),
            &held,
            (file, etag, digest),
            place,
        );
        self.wire_diagram(&tab);
        tab.allow_web(self.web_images.borrow().contains(key));
        // Nothing else watches a loose file: the vault's worker only reports on its own tree.
        if doc::is_loose_key(key) {
            tab.watch_file(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tab| app.model_changed(tab)
            ));
        }
        {
            let config = self.config.borrow();
            tab.set_spellcheck(config.spellcheck);
            tab.set_font(config.editor_font.as_deref());
            tab.set_options(config.diagram);
        }
        let page = tab.page.clone();
        self.mark_loose(&page, key);
        self.watch_folder_of(key);
        self.docs.borrow_mut().push(Doc::Diagram(tab));
        self.landed(&page, how);
    }

    fn wire_diagram(self: &Rc<Self>, tab: &Rc<DiagramTab>) {
        let weak = Rc::downgrade(self);
        let on = move |f: fn(&Rc<App>, &Rc<DiagramTab>)| {
            let weak = weak.clone();
            move |tab: &Rc<DiagramTab>| {
                if let Some(app) = weak.upgrade() {
                    f(&app, tab);
                }
            }
        };
        tab.connect_zoom(on(|app, _| {
            app.refresh_zoom();
            app.save_session_soon();
        }));
        // A page picked from the Outline pane is a place left, as a PDF's outline row is.
        tab.connect_jump(on(|app, tab| app.mark_page(&tab.page)));
        tab.connect_page(on(|app, _| {
            app.sync_diagram_tools();
            app.sync_status();
            app.follow_outline();
            app.save_session_soon();
        }));
        tab.connect_pages(on(|app, tab| {
            if app.is_active_diagram(tab) {
                app.sync_outline();
                app.sync_status();
            }
        }));
        // The window's Back and Forward places in the diagram follow its pages, as a PDF's do.
        let weak = Rc::downgrade(self);
        tab.connect_repaged(move |tab, map| {
            if let Some(app) = weak.upgrade() {
                app.nav.borrow_mut().repage(&tab.key(), map);
            }
        });
        tab.connect_selection(on(|_, _| {}));
        // An edit keeps a preview, as typing keeps a note's.
        tab.connect_history(on(|app, tab| {
            if tab.history().0 {
                app.promote(&tab.page);
            }
            if app.is_active_diagram(tab) {
                app.sync_diagram_tools();
                app.sync_history();
                app.sync_status();
            }
        }));
        tab.connect_autosave(on(|app, tab| app.save_model(tab, false)));
        tab.connect_image(on(|app, tab| app.pick_image(tab)));
        tab.connect_banner(on(|app, tab| app.resolve_model(tab)));
        // Load: remembered for the file, and the note in the preview drawn again with them.
        tab.connect_web(on(|app, tab| {
            app.web_images.borrow_mut().insert(tab.key());
            app.save_session_soon();
            tab.allow_web(true);
            app.reshow_preview_image(&tab.key());
        }));
        let weak = Rc::downgrade(self);
        tab.connect_toast(move |text| {
            if let Some(app) = weak.upgrade() {
                app.toast(text);
            }
        });
        // Into the config, to every open diagram, and onto disk a second later, as a PDF's picks.
        tab.connect_options(on(|app, tab| {
            app.config.borrow_mut().diagram = tab.options();
            app.config_changed();
        }));
        // Where the next ring opens, as a PDF's drag says.
        let weak = Rc::downgrade(self);
        tab.connect_ring_moved(move |at| {
            if let Some(app) = weak.upgrade() {
                app.ring_at.set(Some(at));
            }
        });
    }

    /// The Properties pane follows the tab in front: a diagram's own, or none.
    pub(crate) fn sync_properties(&self) {
        if let Some(sidebar) = self.sidebar.get() {
            sidebar.set_properties(self.active_diagram().map(|d| d.properties()).as_ref());
        }
    }

    pub(crate) fn active_diagram(&self) -> Option<Rc<DiagramTab>> {
        self.active_doc()?.diagram().cloned()
    }

    fn is_active_diagram(&self, tab: &Rc<DiagramTab>) -> bool {
        self.active_diagram().is_some_and(|d| Rc::ptr_eq(&d, tab))
    }

    pub(crate) fn diagram_of(&self, pane: &Pane) -> Option<Rc<DiagramTab>> {
        self.doc_of(pane)?.diagram().cloned()
    }

    pub(crate) fn diagrams(&self) -> Vec<Rc<DiagramTab>> {
        self.docs
            .borrow()
            .iter()
            .filter_map(|d| d.diagram().cloned())
            .collect()
    }

    /// The diagram commands, over the diagram in front. `false` for a name that is not one.
    pub(crate) fn diagram_action(self: &Rc<Self>, name: &str) -> bool {
        let Some(tab) = self.active_diagram() else {
            return name.starts_with("diagram-");
        };
        match name {
            "diagram-undo" => tab.undo(),
            "diagram-redo" => tab.redo(),
            "diagram-delete" => tab.delete(),
            "diagram-delete-all" => tab.delete_all(),
            "diagram-duplicate" => tab.duplicate(),
            "diagram-group" => tab.group(),
            "diagram-copy" => tab.copy(),
            "diagram-cut" => tab.cut(),
            "diagram-paste" => self.paste_diagram(&tab),
            "diagram-ungroup" => tab.ungroup(),
            "diagram-select-all" => tab.select_all(),
            "diagram-edit-label" => tab.edit_label(),
            "diagram-next-page" => tab.step_page(true),
            "diagram-previous-page" => tab.step_page(false),
            "diagram-add-page" => tab.add_page(),
            "diagram-rename-page" => self.rename_diagram_page(&tab),
            "diagram-delete-page" => tab.delete_page(),
            "diagram-move-page-up" => tab.move_page(false),
            "diagram-move-page-down" => tab.move_page(true),
            "diagram-to-front" => tab.reorder(accent_drawio::ZOrder::ToFront),
            "diagram-to-back" => tab.reorder(accent_drawio::ZOrder::ToBack),
            "diagram-invert" => tab.toggle_invert(),
            // A preference, as Show Hidden Files is: every diagram and every window follow it.
            "diagram-grid" => {
                {
                    let mut config = self.config.borrow_mut();
                    config.diagram.grid = !config.diagram.grid;
                }
                self.config_changed();
            }
            "diagram-select" => self.diagram_tool(Tool::Select),
            "diagram-rect" => self.diagram_tool(Tool::Rect),
            "diagram-ellipse" => self.diagram_tool(Tool::Ellipse),
            "diagram-text" => self.diagram_tool(Tool::Text),
            "diagram-connector" => self.diagram_tool(Tool::Connector),
            "diagram-image" => {
                self.diagram_tool(Tool::Image);
                tab.ask_image();
            }
            _ => return false,
        }
        true
    }

    /// Offer insertion over a diagram alone, disabled when all its layers are hidden or locked,
    /// and clear any active insert tool, matching draw.io (`EditorUi.updateActionStates`).
    pub(crate) fn sync_diagram_tools(&self) {
        let diagram = self.active_diagram();
        let open = diagram.as_ref().is_some_and(|d| d.can_insert());
        let inserts = [
            Tool::Rect,
            Tool::Ellipse,
            Tool::Text,
            Tool::Connector,
            Tool::Image,
        ];
        for tool in inserts {
            let name = tool.action().trim_start_matches("win.");
            let action = self.window.lookup_action(name);
            if let Some(action) = action.and_downcast::<gio::SimpleAction>() {
                action.set_enabled(open);
            }
        }
        if let Some(d) = diagram.filter(|d| !open && d.tool() != Tool::Select) {
            d.set_tool(Tool::Select);
            self.sync_status();
        }
    }

    /// Put a tool in hand over the diagram in front. The same one twice puts it down again, and
    /// reaching one from the palette with the ring away takes the ring out, as on a PDF.
    pub(crate) fn diagram_tool(self: &Rc<Self>, tool: Tool) {
        let Some(tab) = self.active_diagram() else {
            return;
        };
        let tool = match tab.tool() == tool {
            true => Tool::Select,
            false => tool,
        };
        if tool != Tool::Select && !tab.ring_shown() {
            tab.show_ring(true, self.ring_at.get());
            self.drawing_button.set_active(true);
        }
        tab.set_tool(tool);
        self.sync_status();
    }

    fn rename_diagram_page(self: &Rc<Self>, tab: &Rc<DiagramTab>) {
        let names = tab.page_names();
        let current = names.get(tab.page_index()).cloned().unwrap_or_default();
        let entry = gtk::Entry::builder()
            .text(&current)
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::new(Some("Rename Page"), None);
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");
        let tab = tab.clone();
        dialogs::choose(&dialog, Some(&self.window), move |response| {
            let name = entry.text();
            if response == "rename" && !name.trim().is_empty() {
                tab.rename_page(name.trim());
            }
        });
    }

    /// Paste into a diagram: text first, as GTK's own paste reads it, a diagram in it or a text
    /// cell (`DiagramTab::paste_text`); else a picture, embedded as Add Image embeds one.
    fn paste_diagram(self: &Rc<Self>, tab: &Rc<DiagramTab>) {
        let clipboard = tab.key_target().clipboard();
        let formats = clipboard.formats();
        let (app, tab) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let text = formats.contains_type(glib::Type::STRING);
            let picture = !text && formats.contains_type(gdk::Texture::static_type());
            let (Some(app), Some(tab)) = (app.upgrade(), tab.upgrade()) else {
                return;
            };
            if text {
                match clipboard.read_text_future().await {
                    Ok(Some(text)) => tab.paste_text(&text),
                    Ok(None) => {}
                    Err(e) => app.cannot("paste", e),
                }
            } else if picture {
                match clipboard.read_texture_future().await {
                    Ok(Some(texture)) => {
                        let bytes = texture.save_to_png_bytes();
                        if bytes.len() > MAX_IMAGE {
                            return app.toast("Choose an image no larger than 2 MiB");
                        }
                        let size = (f64::from(texture.width()), f64::from(texture.height()));
                        tab.add_image("image/png", &bytes, size);
                    }
                    Ok(None) => {}
                    Err(e) => app.cannot("paste the picture", e),
                }
            }
        });
    }

    /// The Image tool: a picture file, read on a worker, embedded in the page.
    fn pick_image(self: &Rc<Self>, tab: &Rc<DiagramTab>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Pictures"));
        for mime in [
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/svg+xml",
            "image/webp",
        ] {
            filter.add_mime_type(mime);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Add Image")
            .accept_label("Add")
            .filters(&filters)
            .modal(true)
            .build();
        let (app, weak) = (Rc::downgrade(self), Rc::downgrade(tab));
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |picked| {
            let (Some(app), Some(tab)) = (app.upgrade(), weak.upgrade()) else {
                return;
            };
            tab.set_tool(Tool::Select);
            app.sync_status();
            let Some(path) = picked.ok().and_then(|f| f.path()) else {
                return;
            };
            app.embed_image(&tab, path);
        });
    }

    fn embed_image(self: &Rc<Self>, tab: &Rc<DiagramTab>, path: PathBuf) {
        let (app, weak) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let read = crate::work::attempt("read the picture", move || {
                let bytes = std::fs::read(&path)?;
                let mime = gio::content_type_guess(Some(&path), &bytes[..])
                    .0
                    .to_string();
                let mime = gio::content_type_get_mime_type(&mime).map_or(mime, |m| m.to_string());
                Ok::<_, std::io::Error>((mime, bytes))
            })
            .await;
            let (Some(app), Some(tab)) = (app.upgrade(), weak.upgrade()) else {
                return;
            };
            let (mime, bytes) = match read {
                Ok(read) => read,
                Err(why) => return app.toast(&why),
            };
            if bytes.len() > MAX_IMAGE {
                return app.toast("Choose an image no larger than 2 MiB");
            }
            let size = match gdk::Texture::from_bytes(&glib::Bytes::from(&bytes)) {
                Ok(t) => (f64::from(t.width()), f64::from(t.height())),
                Err(_) => {
                    return app.cannot("add the picture", "it is not an image accent can read");
                }
            };
            tab.add_image(&mime, &bytes, size);
        });
    }
}

impl SavesModel for DiagramTab {
    const KIND: &'static str = "Diagram";
    type Read = Parsed;

    /// The model as it is now, written out on the worker: up to 16 MiB of XML.
    fn contents(&self) -> impl FnOnce() -> String + Send + 'static {
        let file = self.file();
        move || file.to_xml()
    }

    fn parse(_: &str, read: accent_api::Result<accent_core::fs::Read>) -> Parsed {
        parse(read)
    }

    fn reloaded(app: &Rc<App>, tab: &Rc<Self>, read: Parsed) {
        match read {
            Parsed::Diagram(file, etag, digest) => {
                tab.reload(file, etag, digest);
                app.sync_status();
            }
            Parsed::Refused(_, why) => app.cannot("reload", why),
            Parsed::Failed(e) => app.cannot("reload", e),
        }
    }

    fn mark_clean(&self, etag: Etag) {
        DiagramTab::mark_clean(self, etag);
    }

    fn show_changed(&self) {
        DiagramTab::show_changed(self);
    }

    fn clear_changed(&self) {
        DiagramTab::clear_changed(self);
    }

    fn discard(&self) {
        DiagramTab::discard(self);
    }

    /// A label still being typed, which is an edit like any other.
    fn finish_editing(self: &Rc<Self>) {
        self.finish_label();
    }
}
