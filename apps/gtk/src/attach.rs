//! Images pasted or dropped into a note. The file is written where the vault's
//! `attachment_folder` says, through the vault so that a host takes it as well, and the note gets
//! Obsidian's embed of it, `![[Pasted image 20260925143012.png]]`, where the caret or the drop was.
//!
//! Only images, and only in a note of the window's vault: a code tab, a loose note and anything
//! else on the clipboard or in a drag keep GTK's own paste and drop.

use super::*;
use accent_core::attachment;
use accent_core::markdown::is_image;
use accent_core::path::basename;

/// What arrived: an image off the clipboard, or image files let go over the text.
enum Incoming {
    Pasted(gdk::Texture),
    Dropped(Vec<PathBuf>),
}

/// Where the embed goes: the selection a paste replaces, or the point a drop was let go at. Held
/// as marks, so typing while the file is written does not move it.
struct InsertAt {
    buffer: gtk::TextBuffer,
    from: gtk::TextMark,
    to: gtk::TextMark,
}

impl InsertAt {
    fn new(buffer: &gtk::TextBuffer, from: &gtk::TextIter, to: &gtk::TextIter) -> InsertAt {
        InsertAt {
            buffer: buffer.clone(),
            from: buffer.create_mark(None, from, true),
            to: buffer.create_mark(None, to, false),
        }
    }

    /// Put `text` in as one undo step, with the caret after it.
    fn fill(&self, text: &str) {
        let (mut from, mut to) = (
            self.buffer.iter_at_mark(&self.from),
            self.buffer.iter_at_mark(&self.to),
        );
        self.buffer.begin_user_action();
        self.buffer.delete(&mut from, &mut to);
        self.buffer.insert(&mut from, text);
        self.buffer.end_user_action();
        self.buffer.place_cursor(&from);
    }
}

impl Drop for InsertAt {
    fn drop(&mut self) {
        self.buffer.delete_mark(&self.from);
        self.buffer.delete_mark(&self.to);
    }
}

impl App {
    /// Take image pastes and image drops into `tab`, a note of this window's vault.
    pub(crate) fn wire_attachments(self: &Rc<Self>, tab: &Rc<Tab>) {
        // Connected after the tab's own link paste, which stands aside when there is no text.
        tab.view.connect_paste_clipboard(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |view| {
                // Text wins, as it does in GTK's own paste, which reads nothing else.
                let clipboard = view.clipboard();
                let formats = clipboard.formats();
                if formats.contains_type(glib::Type::STRING)
                    || !formats.contains_type(gdk::Texture::static_type())
                    || !view.is_editable()
                {
                    return;
                }
                view.stop_signal_emission_by_name("paste-clipboard");
                let buffer = view.buffer();
                let (from, to) = buffer.selection_bounds().unwrap_or_else(|| {
                    let caret = buffer.iter_at_mark(&buffer.get_insert());
                    (caret, caret)
                });
                let at = InsertAt::new(&buffer, &from, &to);
                glib::spawn_future_local(async move {
                    match clipboard.read_texture_future().await {
                        Ok(Some(texture)) => app.attach(&tab, at, Incoming::Pasted(texture)),
                        Ok(None) => {}
                        Err(e) => app.cannot("paste the image", e),
                    }
                });
            }
        ));

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        // Read as the drag comes in, so the motion can tell images from anything else.
        drop.set_preload(true);
        drop.connect_motion(glib::clone!(
            #[weak]
            tab,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target, _, _| match target.value() {
                // Still being read: the view's own target answers meanwhile.
                None => gdk::DragAction::empty(),
                Some(value) if tab.view.is_editable() && images(&value).is_some() => {
                    gdk::DragAction::COPY
                }
                // Anything else is the view's own drop, which writes the paths in as text.
                Some(_) => {
                    target.reject();
                    gdk::DragAction::empty()
                }
            }
        ));
        drop.connect_drop(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            #[upgrade_or]
            false,
            move |_, value, x, y| {
                let Some(files) = images(value).filter(|_| tab.view.is_editable()) else {
                    return false;
                };
                let (x, y) = tab.view.window_to_buffer_coords(
                    gtk::TextWindowType::Widget,
                    x as i32,
                    y as i32,
                );
                let point = crate::editor::pressed_at(&tab.view, x, y);
                let at = InsertAt::new(tab.buffer.upcast_ref(), &point, &point);
                app.attach(&tab, at, Incoming::Dropped(files));
                true
            }
        ));
        tab.view.add_controller(drop);
    }

    /// Store what arrived on a worker, then put its embeds in at `at`, a line each.
    fn attach(self: &Rc<Self>, tab: &Rc<Tab>, at: InsertAt, incoming: Incoming) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let (note, app, tab) = (tab.rel(), Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let stored = crate::work::off_thread("attach", move || store(&vault, &note, incoming))
                .await
                .unwrap_or_else(|| (Vec::new(), Some("Cannot add the image".to_string())));
            let (embeds, failed) = stored;
            if let Some(tab) = tab.upgrade().filter(|_| !embeds.is_empty()) {
                at.fill(&embeds.join("\n"));
                tab.view.scroll_mark_onscreen(&tab.buffer.get_insert());
            }
            if let (Some(why), Some(app)) = (failed, app.upgrade()) {
                app.toast(&why);
            }
        });
    }
}

/// The files a drop carries, when there are some and every one is an image the preview shows.
fn images(value: &glib::Value) -> Option<Vec<PathBuf>> {
    crate::tree::dropped_paths(value).filter(|files| {
        files.iter().all(|f| {
            f.file_name()
                .is_some_and(|n| is_image(&n.to_string_lossy()))
        })
    })
}

/// Put each image in the vault, or find it there, and say what the note at `note` calls it: the
/// embeds of those that landed, and why the first one that did not stopped the rest.
fn store(vault: &Vault, note: &str, incoming: Incoming) -> (Vec<String>, Option<String>) {
    let dir = attachment::folder(&vault.config().attachment_folder, note);
    let embed = |rel: &str| {
        let by_name = vault.resolve_link(basename(rel)).ok().flatten();
        attachment::embed(rel, by_name.as_deref())
    };
    let mut embeds = Vec::new();
    match incoming {
        Incoming::Pasted(texture) => {
            let name = attachment::pasted_name(chrono::Local::now().naive_local());
            match write(vault, &dir, &name, &texture.save_to_png_bytes()) {
                Ok(rel) => embeds.push(embed(&rel)),
                Err(why) => return (embeds, Some(why)),
            }
        }
        Incoming::Dropped(files) => {
            for file in files {
                let name = basename(&file.to_string_lossy()).to_string();
                // A file the vault already holds is named where it is, not copied.
                let rel = match in_vault(vault, &file) {
                    Some(rel) => Ok(rel),
                    None => std::fs::read(&file)
                        .map_err(|e| format!("Cannot read {name}: {e}"))
                        .and_then(|bytes| write(vault, &dir, &name, &bytes)),
                };
                match rel {
                    Ok(rel) => embeds.push(embed(&rel)),
                    Err(why) => return (embeds, Some(why)),
                }
            }
        }
    }
    (embeds, None)
}

/// Write `bytes` as `name` in `dir`, numbered where the name is taken, and say where it went.
fn write(vault: &Vault, dir: &str, name: &str, bytes: &[u8]) -> Result<String, String> {
    let rel = attachment::free_path(dir, name, |rel| vault.exists(rel));
    crate::fileops::make_parents(vault, &rel)?;
    vault
        .write_file(&rel, bytes)
        .map_err(|e| format!("Cannot add {name}: {e}"))?;
    Ok(rel)
}

/// Where `file` is in the vault, when the vault is on this machine and holds it.
fn in_vault(vault: &Vault, file: &Path) -> Option<String> {
    if vault.is_remote() {
        return None;
    }
    let (root, file) = (vault.root().canonicalize().ok()?, file.canonicalize().ok()?);
    Some(file.strip_prefix(root).ok()?.to_string_lossy().into_owned())
}
