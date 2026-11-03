//! The Select tool's commands: copy, cut, paste, duplicate and delete what it holds. A copy goes
//! onto the system clipboard as JSON in a format of our own, beside the typed texts it holds as
//! plain text, so another PDF tab, in this window or another, pastes it.

use std::rc::Rc;

use accent_core::pdf::{Clip, InkStyle, Matrix};
use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::protocol::{Clipping, Request, fresh_id};
use super::tab::PdfTab;
use super::tools::{Picked, clamp_move};

/// What the clipboard calls strokes and texts copied off a PDF: a JSON list of [`Clip`]s.
pub const MIME: &str = "application/x-accent-pdf-ink";

/// How far a paste in place, or a duplicate, lands down and right of what it copies, in page
/// points.
const STEP: f32 = 10.0;

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

    /// Ask the render thread for the strokes and texts `ids` of `page`, for `how`.
    fn clip(&self, page: usize, ids: Vec<u32>, how: Clipping) {
        if how != Clipping::Duplicate {
            self.copying.set(self.copying.get() + 1);
        }
        self.ask(Request::Clip { page, ids, how });
    }

    /// Whether there are drawings to paste: on the clipboard, or on their way to it.
    pub fn pasteable(&self) -> bool {
        self.copying.get() > 0 || self.view.clipboard().formats().contain_mime_type(MIME)
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
                    Clip::Ink { .. } => None,
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
            let copied = (how == Clipping::Copy).then_some(json);
            self.pasted.replace((copied, 0));
        }
        if self.copying.get() == 0
            && let Some(at) = self.pending_paste.take()
        {
            self.paste_at(at);
        }
    }

    /// Paste drawings from the clipboard, as strokes and texts of ours, held by the Select tool:
    /// centred where the page's menu was opened or `Ctrl+V` pressed, else where they were copied
    /// from, a step further each time, on the page being read. Never off the paper.
    pub fn paste(self: &Rc<Self>) {
        let at = self.pointed.get();
        match self.copying.get() {
            0 => self.paste_at(at),
            _ => self.pending_paste.set(Some(at)),
        }
    }

    fn paste_at(self: &Rc<Self>, at: Option<(usize, f32, f32)>) {
        let clipboard = self.view.clipboard();
        let tab = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(json) = read(&clipboard).await else {
                return;
            };
            let clips: Vec<Clip> = match serde_json::from_slice(&json) {
                Ok(clips) => clips,
                Err(e) => return tracing::warn!("the clipboard's drawing does not read: {e}"),
            };
            if let Some(tab) = tab.upgrade()
                && !clips.is_empty()
            {
                tab.land(clips, &json, at);
            }
        });
    }

    /// Where `clips`, `json` on the clipboard, go: see [`PdfTab::paste`].
    fn land(&self, clips: Vec<Clip>, json: &[u8], at: Option<(usize, f32, f32)>) {
        let bounds = bounds_of(&clips);
        let (page, (dx, dy)) = match at {
            Some((page, x, y)) => {
                let middle = (
                    (bounds.left + bounds.right) / 2.0,
                    (bounds.top + bounds.bottom) / 2.0,
                );
                (page, (x - middle.0, y - middle.1))
            }
            None => {
                let mut pasted = self.pasted.borrow_mut();
                let json = String::from_utf8_lossy(json).into_owned();
                match pasted.0.as_deref() == Some(json.as_str()) {
                    true => pasted.1 += 1,
                    false => *pasted = (Some(json), 0),
                }
                let d = pasted.1 as f32 * STEP;
                (self.current_page(), (d, d))
            }
        };
        let size = self.page_size(page).unwrap_or((f32::MAX, f32::MAX));
        let m = clamp_move(bounds, [1.0, 0.0, 0.0, 1.0, dx, dy], size);
        self.paste_clips(page, clips, m);
    }

    /// `clips` drawn onto `page` under `m`, held by the Select tool at once under the names the
    /// render thread will list them by.
    fn paste_clips(&self, page: usize, clips: Vec<Clip>, matrix: Matrix) {
        if clips.is_empty() {
            return;
        }
        let names: Vec<u32> = clips.iter().map(|_| fresh_id()).collect();
        let items = (clips.iter().zip(&names))
            .map(|(clip, &id)| picked_of(&clip.under(matrix), id))
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

    /// Show on the ring what can be done with what the Select tool holds, and Paste while there
    /// is something to paste.
    pub(super) fn sync_commands(&self) {
        let picked = self.view.picked().is_some();
        self.ring.set_selection(picked, self.pasteable());
    }
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
            typed: false,
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
                typed: true,
            }
        }
    }
}

/// The drawing on `clipboard`, as the JSON a copy put there, or `None` when it holds none.
async fn read(clipboard: &gdk::Clipboard) -> Option<Vec<u8>> {
    let (stream, _) = (clipboard
        .read_future(&[MIME], glib::Priority::DEFAULT)
        .await)
        .inspect_err(|e| tracing::debug!("no drawing on the clipboard: {e}"))
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
