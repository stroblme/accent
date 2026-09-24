//! The window's side of a diagram tab: opening one, saving it through the same etag gate a
//! note's save goes through, what the watcher says about its file, and the diagram commands.

use accent_core::fs::{Etag, SaveError};
use accent_drawio::File;

use super::{DiagramTab, Tool};
use crate::*;

/// Pictures larger than this are not embedded: a `data:` URI is a third larger again, and it
/// lives inside the diagram file.
const MAX_IMAGE: usize = 2 * 1024 * 1024;

/// What reading a diagram off the disk came to.
enum Parsed {
    Diagram(File, Etag),
    /// Not something to draw on, and why: a status page's title and sentence.
    Refused(&'static str, String),
    Failed(std::io::Error),
}

/// Read and parse on a worker thread: the file is XML of up to 16 MiB.
fn parse(read: std::io::Result<accent_core::fs::Read>) -> Parsed {
    match read {
        Ok(accent_core::fs::Read::Text(text)) => parse_text(text),
        Ok(accent_core::fs::Read::Binary { .. }) => Parsed::Refused(
            "Not a Diagram",
            "The file is not text, so it cannot be a draw.io diagram.".to_string(),
        ),
        Ok(accent_core::fs::Read::TooLarge { .. }) => Parsed::Refused(
            "File Too Large",
            format!(
                "The file is over the {} MiB accent will read.",
                accent_core::fs::MAX_TEXT / (1024 * 1024)
            ),
        ),
        Err(e) => Parsed::Failed(e),
    }
}

fn parse_text(text: accent_core::fs::Text) -> Parsed {
    match File::from_bytes(text.text.as_bytes()) {
        Ok(file) => Parsed::Diagram(file, text.etag),
        Err(e) => Parsed::Refused("Not a Diagram", format!("It could not be read: {e}.")),
    }
}

impl App {
    /// A diagram in a tab of its own. The read and the parse happen on a worker; the tab goes up
    /// once there is a page to show, as a note's goes up once its text has arrived.
    pub(crate) fn open_diagram(self: &Rc<Self>, key: &str, path: &Path, how: Opened) {
        let vault = self.vault().filter(|_| !doc::is_loose_key(key)).cloned();
        let (key, path) = (key.to_string(), path.to_path_buf());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let parsed = crate::work::off_thread("diagram reader", {
                let (key, path) = (key.clone(), path.clone());
                move || {
                    parse(match vault {
                        Some(vault) => vault.read_text(&key),
                        None => accent_core::fs::read_text(&path),
                    })
                }
            })
            .await;
            let Some(app) = weak.upgrade() else { return };
            if app.doc_for(&key).is_some() {
                return;
            }
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
        let (file, etag) = match parsed {
            Parsed::Diagram(file, etag) => (file, etag),
            Parsed::Refused(title, body) => return self.open_status(key, title, &body, how),
            Parsed::Failed(e) => return self.cannot_open(key, e),
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
            &self.tabs_for(key),
            (file, etag),
            place,
        );
        self.wire_diagram(&tab);
        // Nothing else watches a loose file: the vault's worker only reports on its own tree.
        if doc::is_loose_key(key) {
            tab.watch_file(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tab| app.diagram_changed(tab)
            ));
        }
        {
            let config = self.config.borrow();
            tab.set_spellcheck(config.spellcheck);
            tab.set_font(config.editor_font.as_deref());
        }
        let page = tab.page.clone();
        self.mark_loose(&page, key);
        self.docs.borrow_mut().push(Doc::Diagram(tab));
        self.select_new_page(&page, how);
        self.mark_opened(&page, how);
        self.save_session_soon();
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
            app.sync_status();
            app.save_session_soon();
        }));
        tab.connect_pages(on(|app, tab| {
            if app.is_active_diagram(tab) {
                app.sync_outline();
                app.sync_status();
            }
        }));
        tab.connect_selection(on(|_, _| {}));
        tab.connect_history(on(|app, tab| {
            if app.is_active_diagram(tab) {
                app.sync_history();
                app.sync_status();
            }
        }));
        tab.connect_autosave(on(|app, tab| app.save_diagram(tab, false)));
        tab.connect_image(on(|app, tab| app.pick_image(tab)));
        tab.connect_banner(on(|app, tab| app.resolve_diagram(tab)));
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
            "diagram-duplicate" => tab.duplicate(),
            "diagram-select-all" => tab.select_all(),
            "diagram-edit-label" => tab.edit_label(),
            "diagram-next-page" => tab.step_page(true),
            "diagram-previous-page" => tab.step_page(false),
            "diagram-add-page" => tab.add_page(),
            "diagram-rename-page" => self.rename_diagram_page(&tab),
            "diagram-delete-page" => tab.delete_page(),
            "diagram-to-front" => tab.reorder(accent_drawio::ZOrder::ToFront),
            "diagram-to-back" => tab.reorder(accent_drawio::ZOrder::ToBack),
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

    /// Ctrl+S or the autosave, through the note's save (`App::start_save`): nothing is written
    /// while the file has moved under unsaved edits, one write is on its way at a time, and a
    /// write is gated on the etag the tab last knew.
    pub(crate) fn save_diagram(self: &Rc<Self>, tab: &Rc<DiagramTab>, explicit: bool) {
        if !editor::may_save(tab.save.modified.get(), tab.save.disk_changed.get()) {
            if explicit && tab.save.disk_changed.get() {
                self.resolve_diagram(tab);
            }
            return;
        }
        self.start_save(tab, explicit, Self::save_diagram, Self::land_diagram);
    }

    /// `App::land_flight` for a diagram: `report` is false for a write about to go after it.
    pub(crate) fn land_diagram(
        self: &Rc<Self>,
        tab: &Rc<DiagramTab>,
        report: bool,
    ) -> Option<bool> {
        self.land_flight(
            tab,
            Self::diagram_changed,
            |app, tab, landed, explicit| match landed {
                editor::Landing::Clean(etag) => {
                    tab.mark_clean(etag);
                    app.sync_status();
                    if report && explicit {
                        app.toast("Saved");
                    }
                }
                editor::Landing::Behind(etag) => tab.save.etag.set(Some(etag)),
                editor::Landing::Failed(SaveError::ChangedOnDisk { .. }) if report => {
                    tab.show_changed();
                    if explicit {
                        app.resolve_diagram(tab);
                    }
                }
                editor::Landing::Failed(SaveError::Offline) if report => {
                    app.show_offline();
                    if explicit {
                        app.toast("Not connected, so nothing was saved");
                    }
                }
                editor::Landing::Failed(e) if report => app.cannot("save", e),
                editor::Landing::Failed(_) | editor::Landing::Stale => {}
            },
        )
    }

    /// Write the diagram now, gated on `expected`, and hand the error back: for a tab or a
    /// window on its way out, which has to know whether the bytes landed.
    pub(crate) fn write_diagram(
        self: &Rc<Self>,
        tab: &Rc<DiagramTab>,
        expected: Option<Etag>,
    ) -> Result<(), SaveError> {
        self.land_diagram(tab, false);
        let etag = self.writer(tab)(&tab.text(), expected)?;
        tab.mark_clean(etag);
        tab.clear_changed();
        self.sync_status();
        Ok(())
    }

    /// [`write_diagram`](Self::write_diagram) at the tab's own etag, if it has anything to
    /// write.
    pub(crate) fn flush_diagram(self: &Rc<Self>, tab: &Rc<DiagramTab>) -> Result<(), SaveError> {
        self.land_diagram(tab, false);
        if !tab.save.modified.get() {
            return Ok(());
        }
        self.write_diagram(tab, tab.save.etag.get())
    }

    /// The watcher says the file under a diagram moved, and it is not our own write
    /// (`App::check_disk`): a diagram with no edits reloads, and one with edits asks.
    pub(crate) fn diagram_changed(self: &Rc<Self>, tab: &Rc<DiagramTab>) {
        self.check_disk(tab, |app, tab| match tab.save.modified.get() {
            true => tab.show_changed(),
            false => app.reload_diagram(tab),
        });
    }

    fn reload_diagram(self: &Rc<Self>, tab: &Rc<DiagramTab>) {
        let vault = self
            .vault()
            .filter(|_| !doc::is_loose_key(&tab.key()))
            .cloned();
        let (key, path) = (tab.key(), tab.path());
        let (app, reloading) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let parsed = crate::work::off_thread("diagram reader", move || {
                parse(match vault {
                    Some(vault) => vault.read_text(&key),
                    None => accent_core::fs::read_text(&path),
                })
            })
            .await;
            let (Some(app), Some(tab)) = (app.upgrade(), reloading.upgrade()) else {
                return;
            };
            match parsed {
                Some(Parsed::Diagram(file, etag)) => {
                    tab.reload(file, etag);
                    app.sync_status();
                }
                Some(Parsed::Refused(_, why)) => app.cannot("reload", why),
                Some(Parsed::Failed(e)) => app.cannot("reload", e),
                None => {}
            }
        });
    }

    /// The file moved under edits nobody has saved. Both sides hold work, and there is no
    /// comparison of two diagrams to offer, so the question is which one to keep (DESIGN.md: a
    /// choice that can lose data is a dialog).
    pub(crate) fn resolve_diagram(self: &Rc<Self>, tab: &Rc<DiagramTab>) {
        let dialog = adw::AlertDialog::new(
            Some("Diagram Changed on Disk"),
            Some(&format!(
                "{} was modified elsewhere since you opened it. Reload it and lose your changes, or overwrite it with them.",
                doc::file_name(&tab.key())
            )),
        );
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("reload", "Reload"),
            ("overwrite", "Overwrite"),
        ]);
        dialog.set_response_appearance("reload", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let (app, tab) = (self.clone(), tab.clone());
        dialogs::choose(
            &dialog,
            Some(&self.window),
            move |response| match response.as_str() {
                "reload" => app.reload_diagram(&tab),
                "overwrite" => match app.write_diagram(&tab, None) {
                    Ok(()) => app.toast("Overwritten"),
                    Err(e) => app.cannot("save", e),
                },
                _ => {}
            },
        );
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
                return app.toast("Pictures over 2 MiB are not embedded");
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
