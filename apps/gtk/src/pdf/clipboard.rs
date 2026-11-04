//! The Select tool's commands: copy, cut, paste, duplicate, delete and recolour what it holds. A
//! copy goes onto the system clipboard as JSON in a format of our own, beside the typed texts it
//! holds as plain text, so another PDF tab, in this window or another, pastes it. A picture
//! another application copied pastes too.

use std::rc::Rc;

use accent_core::pdf::{Clip, InkStyle, Matrix, then};
use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::pictures::Incoming;
use super::protocol::{Clipping, Request, fresh_id};
use super::tab::PdfTab;
use super::tools::{Kind, Picked, clamp_move};

/// What the clipboard calls strokes and texts copied off a PDF: a JSON list of [`Clip`]s.
pub const MIME: &str = "application/x-accent-pdf-ink";

/// What another application's JPEG is on the clipboard, which goes into the file as it is.
const JPEG: &str = "image/jpeg";
/// What a source that encodes its pixels offers beside its JPEG, which is then no file's own.
const PNG: &str = "image/png";

/// How far a paste in place, or a duplicate, lands down and right of what it copies, in page
/// points: and each picture of a drop of several from the one before.
pub(super) const STEP: f32 = 10.0;

impl PdfTab {
    /// Copy what the Select tool holds, or else the selected text: `Ctrl+C` and Copy Selection.
    pub fn copy(&self) {
        match self.view.picked() {
            Some((page, ids)) => self.clip(page, ids, Clipping::Copy),
            None => self.copy_selection(),
        }
    }

    /// Copy what the Select tool holds and take it off the page, as one step.
    pub fn cut(&self) {
        if let Some((page, ids)) = self.view.drop_picked() {
            self.clip(page, ids, Clipping::Cut);
        }
    }

    /// What the Select tool holds pasted again a step down and right, as one step, the clipboard
    /// left alone: the copies are held after it, so another Duplicate steps on from them.
    pub fn duplicate(&self) {
        if let Some((page, ids)) = self.view.picked() {
            self.clip(page, ids, Clipping::Duplicate);
        }
    }

    /// Take what the Select tool holds off the page, as one step.
    pub fn delete_picked(&self) {
        if let Some((page, ids)) = self.view.drop_picked() {
            self.ask(Request::Delete { page, ids });
        }
    }

    /// Give what the Select tool holds a swatch's colour, `None` being the accent, as one step:
    /// what the ring's Colour dots do. The file keeps the swatch's own colour, the accent as it is
    /// now, which the page's recolouring then shows as it shows the pen's ink.
    pub fn recolour_picked(&self, colour: Option<[u8; 3]>) {
        let rgb = colour.unwrap_or_else(crate::theme::accent_rgb);
        if let Some((page, ids)) = self.view.recolour_picked(rgb) {
            self.ask(Request::Recolour { page, ids, rgb });
        }
    }

    /// Ask the render thread for the strokes and texts `ids` of `page`, for `how`.
    fn clip(&self, page: usize, ids: Vec<u32>, how: Clipping) {
        if how != Clipping::Duplicate {
            self.copying.set(self.copying.get() + 1);
        }
        self.ask(Request::Clip { page, ids, how });
    }

    /// Whether there is something to paste: drawings on the clipboard or on their way to it, or a
    /// picture.
    pub fn pasteable(&self) -> bool {
        let formats = self.view.clipboard().formats();
        self.copying.get() > 0
            || formats.contain_mime_type(MIME)
            || formats.contain_mime_type(JPEG)
            || formats.contains_type(gdk::Texture::static_type())
    }

    /// The render thread's copy of what the Select tool held on `page` landed. A paste asked for
    /// while it was on its way is made now, from the clipboard it just filled.
    pub(super) fn clipped(self: &Rc<Self>, page: usize, clips: Vec<Clip>, how: Clipping) {
        if how == Clipping::Duplicate {
            let step = [1.0, 0.0, 0.0, 1.0, STEP, STEP];
            let bounds = bounds_of(&clips);
            let size = self.page_size(page).unwrap_or((f32::MAX, f32::MAX));
            return self.paste_clips(page, clips, clamp_move(bounds, step, size));
        }
        self.copying.set(self.copying.get().saturating_sub(1));
        if !clips.is_empty() {
            let json = serde_json::to_string(&clips).unwrap_or_default();
            let mut providers = vec![gdk::ContentProvider::for_bytes(
                MIME,
                &glib::Bytes::from(json.as_bytes()),
            )];
            let typed: Vec<&str> = (clips.iter())
                .filter_map(|clip| match clip {
                    Clip::Text(typed) => Some(typed.text.as_str()),
                    Clip::Ink { .. } | Clip::Image { .. } => None,
                })
                .collect();
            if !typed.is_empty() {
                let text = typed.join("\n");
                providers.push(gdk::ContentProvider::for_value(&text.to_value()));
            }
            let provider = gdk::ContentProvider::new_union(&providers);
            if let Err(e) = self.view.clipboard().set_content(Some(&provider)) {
                tracing::warn!("the clipboard would not take the drawing: {e}");
            }
            // The first paste of a copy steps off it, of a cut lands where it was.
            let copied = (how == Clipping::Copy).then(|| hash(json.as_bytes()));
            self.pasted.replace((copied, 0));
        }
        if self.copying.get() == 0
            && let Some(page) = self.pending_paste.take()
        {
            self.paste_at(page);
        }
    }

    /// Paste drawings from the clipboard, as strokes, texts and pictures of ours, held by the
    /// Select tool: where they were copied from, a step further down and right for each paste of
    /// the same copy, on the page the page's menu was opened on, else on the page being read. A
    /// picture from another application lands in the middle of what is on screen of that page,
    /// at its natural size up to half the page, and steps on the same way. Never off the paper.
    /// Text is no drawing, and is left alone.
    pub fn paste(self: &Rc<Self>) {
        let page = self.pointed.get().map(|(page, ..)| page);
        match self.copying.get() {
            0 => self.paste_at(page),
            _ => self.pending_paste.set(Some(page)),
        }
    }

    /// Our drawing first, then a JPEG's bytes where they are the source's own, which go into the
    /// file as they are, then any picture, decoded by GDK.
    fn paste_at(self: &Rc<Self>, page: Option<usize>) {
        let clipboard = self.view.clipboard();
        let tab = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let formats = clipboard.formats();
            if formats.contain_mime_type(MIME) {
                let Some(json) = read(&clipboard, MIME).await else {
                    return;
                };
                let clips: Vec<Clip> = match serde_json::from_slice(&json) {
                    Ok(clips) => clips,
                    Err(e) => return tracing::warn!("the clipboard's drawing does not read: {e}"),
                };
                if let Some(tab) = tab.upgrade()
                    && !clips.is_empty()
                {
                    tab.land(clips, &json, page);
                }
                return;
            }
            // A source offering PNG too encodes both from its pixels, GTK's from every image
            // it copies, and its JPEG would lose detail and every clear pixel.
            let jpeg = match formats.contain_mime_type(JPEG) && !formats.contain_mime_type(PNG) {
                true => read(&clipboard, JPEG).await.and_then(Incoming::jpeg),
                false => None,
            };
            let incoming = match jpeg {
                Some(jpeg) => Some(jpeg),
                None => picture(&clipboard).await,
            };
            if let (Some(tab), Some(incoming)) = (tab.upgrade(), incoming) {
                tab.land_picture(incoming, page);
            }
        });
    }

    /// Where `clips`, `json` on the clipboard, go: see [`PdfTab::paste`].
    fn land(&self, clips: Vec<Clip>, json: &[u8], page: Option<usize>) {
        let page = page.unwrap_or_else(|| self.current_page());
        let d = self.step(hash(json));
        let size = self.page_size(page).unwrap_or((f32::MAX, f32::MAX));
        let m = clamp_move(bounds_of(&clips), [1.0, 0.0, 0.0, 1.0, d, d], size);
        self.paste_clips(page, clips, m);
    }

    /// Where a picture from another application goes: see [`PdfTab::paste`].
    fn land_picture(&self, incoming: Incoming, page: Option<usize>) {
        let page = page.unwrap_or_else(|| self.current_page());
        let Some(size) = self.page_size(page) else {
            return;
        };
        let shown = (self.view.visible_part(page))
            .unwrap_or(accent_core::pdf::Rect::from_corners((0.0, 0.0), size));
        let middle = (
            (shown.left + shown.right) / 2.0,
            (shown.top + shown.bottom) / 2.0,
        );
        let d = self.step(incoming.key);
        let place = incoming.place(middle, size);
        let clip = Clip::Image {
            place,
            picture: incoming.picture,
        };
        let m = clamp_move(clip.bounds(), [1.0, 0.0, 0.0, 1.0, d, d], size);
        self.paste_clips(page, vec![clip], m);
    }

    /// How far down and right the paste of the copy `key` goes: none for its first, one step
    /// further for each after it.
    fn step(&self, key: u64) -> f32 {
        let mut pasted = self.pasted.borrow_mut();
        match pasted.0 == Some(key) {
            true => pasted.1 += 1,
            false => *pasted = (Some(key), 0),
        }
        pasted.1 as f32 * STEP
    }

    /// `clips` drawn onto `page` under `m`, held by the Select tool at once under the names the
    /// render thread will list them by.
    pub(super) fn paste_clips(&self, page: usize, clips: Vec<Clip>, matrix: Matrix) {
        if clips.is_empty() {
            return;
        }
        let names: Vec<u32> = clips.iter().map(|_| fresh_id()).collect();
        let items = (clips.iter().zip(&names))
            .map(|(clip, &id)| match clip {
                // Moved by its place alone, its bytes not copied.
                Clip::Image { place, .. } => Picked::placed(id, then(*place, matrix)),
                clip => picked_of(&clip.under(matrix), id),
            })
            .collect();
        self.view.pick(page, items);
        self.ask(Request::Paste {
            page,
            clips,
            matrix,
            names,
        });
    }

    /// Keep the ring's Paste in step with the clipboard while the tab lives.
    pub(super) fn watch_clipboard(self: &Rc<Self>) {
        let tab = Rc::downgrade(self);
        let watch = self.view.clipboard().connect_changed(move |_| {
            if let Some(tab) = tab.upgrade() {
                tab.sync_commands();
            }
        });
        self.clip_watch.replace(Some(watch));
    }

    /// Show on the ring what can be done with what the Select tool holds, its colours among it,
    /// and Paste while there is something to paste.
    pub(super) fn sync_commands(&self) {
        let picked = self.view.picked().is_some();
        let colours = self.view.picked_colours();
        self.ring.set_selection(picked, colours, self.pasteable());
    }
}

/// What tells one copy on the clipboard from another: its bytes, hashed.
fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// The box round every clip.
fn bounds_of(clips: &[Clip]) -> accent_core::pdf::Rect {
    (clips.iter().map(Clip::bounds))
        .reduce(accent_core::pdf::Rect::union)
        .unwrap_or(accent_core::pdf::Rect::ZERO)
}

/// A clip as the Select tool holds it until the render thread lists it under `id`.
fn picked_of(clip: &Clip, id: u32) -> Picked {
    match clip {
        Clip::Ink { points, style } => Picked {
            id,
            points: points.clone(),
            bounds: clip.bounds(),
            style: *style,
            kind: Kind::Stroke,
            recolourable: true,
        },
        Clip::Text(typed) => {
            let [r, g, b] = typed.style.rgb;
            Picked {
                id,
                points: Vec::new(),
                bounds: typed.bounds(),
                style: InkStyle {
                    width: 0.0,
                    rgba: [r, g, b, 255],
                    multiply: false,
                },
                kind: Kind::Text,
                recolourable: true,
            }
        }
        Clip::Image { place, .. } => Picked::placed(id, *place),
    }
}

/// The picture on `clipboard`, decoded by GDK and read on a worker, or `None` when it holds none.
async fn picture(clipboard: &gdk::Clipboard) -> Option<Incoming> {
    let texture = (clipboard.read_texture_future().await)
        .inspect_err(|e| tracing::debug!("no picture on the clipboard: {e}"))
        .ok()??;
    let read = move || Incoming::pasted(&texture);
    crate::work::off_thread("read the pasted picture", read).await?
}

/// What `clipboard` holds as `mime`: the JSON a copy put there, or a JPEG's bytes. `None` when it
/// holds none.
async fn read(clipboard: &gdk::Clipboard, mime: &str) -> Option<Vec<u8>> {
    let (stream, _) = (clipboard
        .read_future(&[mime], glib::Priority::DEFAULT)
        .await)
        .inspect_err(|e| tracing::debug!("no {mime} on the clipboard: {e}"))
        .ok()?;
    // Read whole, however large: a page of strokes is easily more than a buffer's guess.
    let out = gio::MemoryOutputStream::new_resizable();
    let flags =
        gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET;
    (out.splice_future(&stream, flags, glib::Priority::DEFAULT)
        .await)
        .inspect_err(|e| tracing::warn!("reading the clipboard's drawing: {e}"))
        .ok()?;
    Some(out.steal_as_bytes().to_vec())
}
