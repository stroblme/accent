//! The thread that owns one open document: everything that touches pdfium for a tab.
//!
//! One thread per document, because pdfium is serialised behind one process-wide lock (see
//! `accent_core::pdf`). [`spawn`] is the whole interface: it opens the file, reports the page
//! sizes and then answers [`Request`]s until the tab drops its sender.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, TryRecvError, channel};

use accent_api::PdfLink;
use accent_core::pdf::{self, PdfDoc};
use gtk::glib;

use super::protocol::Request;
use super::{Highlights, LOWRES_W, PdfView, Reply, TILE, TileKey, Want};

/// Open `path` on a thread of its own and answer for it until the sender is dropped.
///
/// The thread owns the `PdfDoc` for its whole life, opening included. Nothing else may touch
/// pdfium while it runs: the library is serialised by one process-wide lock, and two threads
/// inside it abort the process.
pub fn spawn(path: PathBuf, view: glib::SendWeakRef<PdfView>) -> Result<Sender<Request>, String> {
    if !pdf::available() {
        return Err("libpdfium was not found".to_string());
    }
    let (tx, rx) = channel::<Request>();
    std::thread::Builder::new()
        .name("accent-pdf".to_string())
        .spawn(move || {
            let doc = match PdfDoc::open(&path) {
                Ok(doc) => doc,
                Err(e) => return send(&view, Reply::Failed(format!("{e:#}"))),
            };
            let sizes = page_sizes(&doc);
            if sizes.is_empty() {
                return send(&view, Reply::Failed("This file has no pages.".to_string()));
            }
            send(&view, Reply::Reloaded(sizes));
            render_loop(doc, path, rx, view);
        })
        .map_err(|e| format!("cannot start the renderer: {e}"))?;
    Ok(tx)
}

/// One step of this session's drawing, for `Ctrl+Z`: a stroke to take off, or a move to make
/// backwards. Each names its annotation by an id rather than an index, because every erase and
/// every move shuffles the indices.
#[derive(Debug, Clone, Copy)]
enum Step {
    Stroke {
        page: usize,
        id: u32,
    },
    Moved {
        page: usize,
        id: u32,
        inverse: pdf::Matrix,
    },
}

impl Step {
    fn id(&self) -> u32 {
        match *self {
            Step::Stroke { id, .. } | Step::Moved { id, .. } => id,
        }
    }

    fn page(&self) -> usize {
        match *self {
            Step::Stroke { page, .. } | Step::Moved { page, .. } => page,
        }
    }
}

/// What the render thread knows about the ink it has touched, so that Undo can reach this
/// session's work and nothing else.
///
/// `ids` mirrors a page's `/Annots` from the moment we first touch it: `None` is one the
/// document already had, `Some` one we drew or moved. `steps` is the undo list, newest last.
#[derive(Default)]
struct Ink {
    ids: HashMap<usize, Vec<Option<u32>>>,
    steps: Vec<Step>,
    next: u32,
    dirty: bool,
}

impl Ink {
    /// A page is about to be touched: remember what was on it before, once.
    fn note(&mut self, page: usize, count: usize) {
        self.ids.entry(page).or_insert_with(|| vec![None; count]);
    }

    fn fresh(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    /// A stroke of ours went onto the end of `page`'s `/Annots`.
    fn added(&mut self, page: usize) {
        let id = self.fresh();
        self.ids.entry(page).or_default().push(Some(id));
        self.steps.push(Step::Stroke { page, id });
    }

    /// The annotation at `index` was removed from `page`, and with it every step that named it.
    fn erased(&mut self, page: usize, index: usize) {
        let slots = self.ids.get_mut(&page).filter(|s| index < s.len());
        if let Some(id) = slots.and_then(|s| s.remove(index)) {
            self.steps.retain(|step| step.id() != id);
        }
    }

    /// The annotation at `index` was drawn again at the end of `page`'s `/Annots`, keeping its
    /// identity — given one, if it was the document's own. Which id it has now.
    fn requeued(&mut self, page: usize, index: usize) -> u32 {
        let slots = self.ids.get_mut(&page).filter(|s| index < s.len());
        let had = slots.and_then(|s| s.remove(index));
        let id = had.unwrap_or_else(|| self.fresh());
        self.ids.entry(page).or_default().push(Some(id));
        id
    }

    /// A move of the annotation at `index`, undone by `inverse`.
    fn moved(&mut self, page: usize, index: usize, inverse: pdf::Matrix) {
        let id = self.requeued(page, index);
        self.steps.push(Step::Moved { page, id, inverse });
    }

    /// Where the annotation with this id sits in `page`'s `/Annots` today.
    fn index_of(&self, page: usize, id: u32) -> Option<usize> {
        self.ids
            .get(&page)?
            .iter()
            .position(|slot| *slot == Some(id))
    }
}

/// Where each note link lands on the page today, per page, with the index of the link it is.
///
/// The four numbers first, and the text the link quotes as the fallback — a document rebuilt
/// with different line breaks moves the numbers but not the sentence. A link whose quads a real
/// `/Highlight` already covers is left out: it has been exported, and the annotation is in the
/// page's own pixels.
fn highlight_quads(doc: &PdfDoc, links: &[PdfLink]) -> Highlights {
    let mut glyphs: HashMap<usize, Vec<pdf::Glyph>> = HashMap::new();
    let mut existing: HashMap<usize, Vec<pdf::Highlight>> = HashMap::new();
    let mut out = Highlights::new();
    for (at, link) in links.iter().enumerate() {
        let page = link.page;
        let found = glyphs
            .entry(page)
            .or_insert_with(|| doc.page_text(page).unwrap_or_default());
        let quads = pdf::selection_quads(found, link.selection)
            .map(|(_, quads)| quads)
            .or_else(|| {
                let text = link.alias.as_deref()?;
                doc.search(page, text).ok()?.into_iter().next()
            });
        let Some(quads) = quads.filter(|q| !q.is_empty()) else {
            continue;
        };
        let already = existing
            .entry(page)
            .or_insert_with(|| doc.highlights_on(page).unwrap_or_default());
        if already.iter().any(|h| pdf::same_quads(&h.quads, &quads)) {
            continue;
        }
        out.entry(page).or_default().push((quads, at));
    }
    out
}

/// Every page's size in points, which is all the widget needs to lay the document out.
///
/// One pdfium call for the whole document, not one per page: asking a loaded page for its size
/// costs a full parse of that page, and 1 554 of those is eleven seconds before anything appears.
fn page_sizes(doc: &PdfDoc) -> Vec<(f32, f32)> {
    doc.page_sizes().unwrap_or_default()
}

/// The render thread.
///
/// One request at a time, with one twist: before every tile and every searched page it drains the
/// queue, so a batch that has been overtaken is put aside rather than finished into a viewport
/// nobody is looking at any more. Only a request of the same kind abandons it — see
/// [`interrupt`] — and the queue is a stack, so the newest work is always what runs next and
/// what is put aside resumes after it.
///
/// Dropping an interrupted batch instead loses it for good. Nothing re-asks: the widget sends a
/// list of tiles again only when that list changes, and the tab sends a query again only when the
/// text does, so a tile nobody rendered stayed blurry and a search pushed aside reported the
/// matches of the pages it had reached and no more.
fn render_loop(
    mut doc: PdfDoc,
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<Request>,
    view: glib::SendWeakRef<PdfView>,
) {
    // What the file looked like when this document was read. Every write from here updates it,
    // which is how the tab tells its own save from someone else's and does not reload over it.
    let mut etag = accent_core::fs::Etag::of(&path).ok();
    // What has been drawn here, so Undo reaches this session's strokes and no others.
    let mut ink = Ink::default();
    // The channel closing is the tab going away, which is the only way this thread ends.
    while let Ok(first) = rx.recv() {
        let mut queue = vec![first];
        while let Some(current) = queue.pop() {
            match current {
                Request::Tiles {
                    scale,
                    dark,
                    theme,
                    wants,
                } => {
                    let mut at = 0;
                    while at < wants.len() {
                        match rx.try_recv() {
                            Ok(newer) => {
                                let rest = Request::Tiles {
                                    scale,
                                    dark,
                                    theme,
                                    wants: wants[at..].to_vec(),
                                };
                                interrupt(&mut queue, rest, newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return,
                            Err(TryRecvError::Empty) => {}
                        }
                        render_want(&doc, &view, scale, dark, theme, wants[at]);
                        at += 1;
                    }
                }
                Request::Links(page) => {
                    if let Ok(links) = doc.links(page) {
                        send(&view, Reply::Links(page, links));
                    }
                }
                Request::Text(page) => {
                    if let Ok(glyphs) = doc.page_text(page) {
                        send(&view, Reply::Text(page, glyphs));
                    }
                }
                Request::Outline => {
                    if let Ok(outline) = doc.outline() {
                        send(&view, Reply::Outline(outline));
                    }
                }
                Request::Search { query, text, from } => {
                    let pages = doc.page_count();
                    let mut at = from;
                    while at < pages {
                        // Between pages, and no finer: pdfium loads a page's text whole
                        // (`FPDFText_LoadPage`) and the search cursor runs over that, so one page
                        // is the smallest unit there is to stop at. Measured on a 500-page A4
                        // document, that is 0.5 ms typical and 1.6 ms at worst — well inside a
                        // frame, so a tile asked for mid-query waits no longer than that.
                        match rx.try_recv() {
                            Ok(newer) => {
                                let rest = Request::Search {
                                    query,
                                    text,
                                    from: at,
                                };
                                interrupt(&mut queue, rest, newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return,
                            Err(TryRecvError::Empty) => {}
                        }
                        match doc.search(at, &text) {
                            Ok(hits) if !hits.is_empty() => send(
                                &view,
                                Reply::Found {
                                    query,
                                    page: at,
                                    hits,
                                },
                            ),
                            _ => {}
                        }
                        at += 1;
                    }
                }
                Request::Ink {
                    page,
                    points,
                    style,
                } => {
                    let before = doc.annotation_count(page).unwrap_or(0);
                    match doc.add_ink(page, &points, style) {
                        Ok(()) => {
                            ink.note(page, before);
                            ink.added(page);
                            ink.dirty = true;
                            send(&view, Reply::PageChanged(page));
                        }
                        Err(e) => tracing::warn!("drawing on page {page}: {e:#}"),
                    }
                }
                Request::Shape { page, shape, style } => {
                    let before = doc.annotation_count(page).unwrap_or(0);
                    match doc.add_shape(page, shape, style) {
                        Ok(()) => {
                            ink.note(page, before);
                            ink.added(page);
                            ink.dirty = true;
                            send(&view, Reply::PageChanged(page));
                        }
                        Err(e) => tracing::warn!("drawing on page {page}: {e:#}"),
                    }
                }
                Request::Inks(page) => send(
                    &view,
                    Reply::Inks {
                        page,
                        inks: doc.inks(page).unwrap_or_default(),
                    },
                ),
                Request::Transform {
                    page,
                    index,
                    matrix,
                } => {
                    let before = doc.annotation_count(page).unwrap_or(0);
                    ink.note(page, before);
                    match doc.transform_ink(page, index, matrix) {
                        Ok(()) => {
                            ink.moved(page, index, pdf::invert(matrix));
                            ink.dirty = true;
                            send(&view, Reply::PageChanged(page));
                        }
                        Err(e) => tracing::warn!("moving a stroke on page {page}: {e:#}"),
                    }
                }
                Request::Erase { page, at, radius } => {
                    let hit = doc.ink_paths(page).unwrap_or_default();
                    let found = hit.iter().find(|(_, points)| pdf::hit(points, at, radius));
                    if let Some((index, _)) = found {
                        let before = doc.annotation_count(page).unwrap_or(0);
                        ink.note(page, before);
                        if let Err(e) = doc.delete_annotation(page, *index) {
                            tracing::warn!("erasing on page {page}: {e:#}");
                            continue;
                        }
                        ink.erased(page, *index);
                        ink.dirty = true;
                        send(&view, Reply::PageChanged(page));
                    }
                }
                Request::Undo => {
                    // One step per `Ctrl+Z`; a step whose annotation was erased since is skipped.
                    while let Some(step) = ink.steps.pop() {
                        let Some(index) = ink.index_of(step.page(), step.id()) else {
                            continue;
                        };
                        let page = step.page();
                        let done = match step {
                            Step::Stroke { .. } => doc.delete_annotation(page, index).map(|()| {
                                ink.erased(page, index);
                            }),
                            Step::Moved { inverse, .. } => {
                                doc.transform_ink(page, index, inverse).map(|()| {
                                    ink.requeued(page, index);
                                })
                            }
                        };
                        match done {
                            Ok(()) => {
                                ink.dirty = true;
                                send(&view, Reply::PageChanged(page));
                            }
                            Err(e) => tracing::warn!("undoing on page {page}: {e:#}"),
                        }
                        break;
                    }
                }
                Request::Save(ack) => {
                    if !ink.dirty {
                        // The ack still goes: a caller waiting on it is waiting for the file to
                        // be right, and it already is.
                        drop(ack);
                        continue;
                    }
                    // ponytail: `save_to_bytes` rewrites the whole file under the pdfium lock, so
                    // a very large PDF stops the tiles for as long as that takes. Saving
                    // incrementally is the upgrade.
                    match doc.save().map_err(|e| e.to_string()).and_then(|bytes| {
                        accent_core::fs::write_bytes(&path, &bytes, etag).map_err(|e| e.to_string())
                    }) {
                        Ok(written) => {
                            etag = Some(written);
                            ink.dirty = false;
                            send(&view, Reply::Saved(written));
                        }
                        // Left dirty on purpose: the next stroke's save tries again, and the
                        // drawing is still in the document either way. A file that cannot be
                        // written at all — read-only, or changed on disk since it was read —
                        // would otherwise take every stroke silently and lose the lot at close;
                        // the reader is told instead, and a foreign write is answered by the
                        // watcher's reload, which is what clears the ledger.
                        Err(e) => {
                            tracing::warn!("saving {}: {e}", path.display());
                            send(&view, Reply::SaveFailed(e));
                        }
                    }
                    // Dropping the sender is the signal: the receiver's `recv` returns either
                    // way, so a failed save does not hang the window that is closing.
                    drop(ack);
                }
                Request::Highlights(links) => {
                    send(&view, Reply::Highlights(highlight_quads(&doc, &links)));
                }
                Request::Export { links, color } => {
                    let quads = highlight_quads(&doc, &links);
                    let pages: Vec<usize> = quads.keys().copied().collect();
                    let highlights: Vec<pdf::Highlight> = quads
                        .into_iter()
                        .flat_map(|(page, found)| {
                            found.into_iter().map(move |(quads, at)| (page, quads, at))
                        })
                        .map(|(page, quads, at)| pdf::Highlight {
                            page,
                            quads,
                            color: [color[0], color[1], color[2], 255],
                            contents: links.get(at).and_then(|l| l.alias.clone()),
                        })
                        .collect();
                    let written = doc
                        .add_highlights(&highlights)
                        .and_then(|added| match added {
                            // Nothing new is not a write: the file is already what it should be.
                            0 => Ok(0),
                            _ => {
                                let bytes = doc.save()?;
                                let written = accent_core::fs::write_bytes(&path, &bytes, etag)?;
                                etag = Some(written);
                                send(&view, Reply::Saved(written));
                                for page in pages {
                                    send(&view, Reply::PageChanged(page));
                                }
                                Ok(added)
                            }
                        })
                        .map_err(|e| format!("{e:#}"));
                    send(&view, Reply::Exported(written));
                }
                Request::Reload => {
                    // Swapped only on success: a half-written PDF fails to open often while a
                    // LaTeX run is going, and the next event tries again.
                    match PdfDoc::open(&path) {
                        Ok(fresh) => {
                            doc = fresh;
                            etag = accent_core::fs::Etag::of(&path).ok();
                            // The ledger is of the document that just went: its ids mirror an
                            // `/Annots` array this one need not share, so a later Ctrl+Z would
                            // resolve one to an index and delete whatever now sits there. It
                            // also carries `dirty`, which the fresh document is not.
                            ink = Ink::default();
                            send(&view, Reply::Reloaded(page_sizes(&doc)));
                        }
                        Err(e) => tracing::debug!("reloading {}: {e:#}", path.display()),
                    }
                }
            }
        }
    }
}

/// Put `newer` at the top of the queue, and `rest` — what the interrupted batch has left to do —
/// under it or not at all.
///
/// Only a request of the same kind takes a batch over: a newer viewport makes the old tiles
/// pointless, and a newer query makes the old query's remaining pages pointless. It also drops
/// any older remainder of that kind still waiting further down, which is the one a batch put
/// aside earlier left there. Anything else — a link, a page's glyphs, an outline, a reload — is a
/// short detour, and the batch resumes once it is done.
fn interrupt(queue: &mut Vec<Request>, rest: Request, newer: Request) {
    match same_kind(&rest, &newer) {
        false => queue.push(rest),
        true => queue.retain(|waiting| !same_kind(waiting, &newer)),
    }
    queue.push(newer);
}

/// Whether two requests are the same kind of work, whatever they are for.
fn same_kind(a: &Request, b: &Request) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// Render one wanted tile, or the low-resolution stand-in for a whole page.
fn render_want(
    doc: &PdfDoc,
    view: &glib::SendWeakRef<PdfView>,
    scale: f32,
    dark: bool,
    theme: pdf::Theme,
    want: Want,
) {
    let page = want.page as usize;
    if want.is_lowres() {
        let Ok((w, _)) = doc.page_size(page) else {
            return;
        };
        let low = LOWRES_W as f32 / w.max(1.0);
        if let Ok(image) = doc.render_page(page, low, theme) {
            send(
                &view.clone(),
                Reply::Lowres {
                    page: want.page,
                    dark,
                    image,
                },
            );
        }
        return;
    }
    let Ok((w, h)) = doc.page_size(page) else {
        return;
    };
    let (full_w, full_h) = ((w * scale).round() as i32, (h * scale).round() as i32);
    let (x, y) = (i32::from(want.tx) * TILE, i32::from(want.ty) * TILE);
    // Clamped to the page: pdfium clears only the page's own area, so a tile hanging off the
    // edge would come back with uninitialised pixels in it.
    let (tw, th) = (TILE.min(full_w - x), TILE.min(full_h - y));
    if tw <= 0 || th <= 0 {
        return;
    }
    if let Ok(image) = doc.render_tile(page, scale, x, y, tw, th, theme) {
        let key = TileKey {
            page: want.page,
            scale_milli: (scale * 1000.0).round() as u32,
            tx: want.tx,
            ty: want.ty,
            dark,
        };
        send(view, Reply::Tile(key, image));
    }
}

/// Hand one answer to the main loop.
///
/// Each reply travels in its own idle callback holding a weak reference to the view, so a tab
/// closed while a render was in flight simply drops the result.
fn send(view: &glib::SendWeakRef<PdfView>, reply: Reply) {
    let view = view.clone();
    glib::idle_add_once(move || {
        if let Some(view) = view.upgrade() {
            view.deliver(reply);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Undo takes back this session's strokes and stops at whatever the document already had,
    /// however the eraser moved the line in between.
    #[test]
    fn undo_never_reaches_a_pre_existing_annotation() {
        let mut ink = Ink::default();
        // A page that already carried three annotations, then two strokes of ours.
        ink.note(0, 3);
        ink.added(0);
        ink.added(0);
        let newest = ink.steps[1].id();
        assert_eq!(ink.index_of(0, newest), Some(4));

        // The reader erases one of the document's own: ours move down and stay ours.
        ink.erased(0, 1);
        assert_eq!(ink.index_of(0, newest), Some(3));
        assert_eq!(ink.steps.len(), 2);

        // Erasing one of ours leaves one stroke to undo, and then nothing.
        ink.erased(0, 3);
        assert_eq!(ink.steps.len(), 1);
        assert_eq!(ink.index_of(0, ink.steps[0].id()), Some(2));
        ink.erased(0, 2);
        assert!(ink.steps.is_empty(), "the document's own are not ours");

        // A page never drawn on has nothing to undo, whatever it carries.
        assert_eq!(ink.index_of(9, newest), None);
    }

    /// A moved annotation is found by what it is, not where it was: after a move it sits at
    /// the end, a stroke can come after it, and erasing it drops its step.
    #[test]
    fn undo_of_a_move_finds_the_annotation_wherever_it_went() {
        let mut ink = Ink::default();
        ink.note(0, 2);
        ink.moved(0, 0, accent_core::pdf::IDENTITY);
        ink.added(0);
        let Step::Moved { id, .. } = ink.steps[0] else {
            panic!("the move comes first");
        };
        assert_eq!(ink.index_of(0, id), Some(1), "{:?}", ink.ids);
        assert_eq!(ink.index_of(0, ink.steps[1].id()), Some(2));
        // Undone: it is drawn again at the end, keeping its id.
        ink.requeued(0, 1);
        assert_eq!(ink.index_of(0, id), Some(2));
        ink.erased(0, 2);
        assert_eq!(ink.steps.len(), 1);
        assert!(matches!(ink.steps[0], Step::Stroke { .. }));
    }

    fn search(query: u64, from: usize) -> Request {
        Request::Search {
            query,
            text: "q".to_string(),
            from,
        }
    }

    /// A batch pushed aside by a detour comes back; one pushed aside by its own kind does not,
    /// and takes any older remainder of that kind with it.
    #[test]
    fn only_the_same_kind_of_request_abandons_a_batch() {
        let mut queue = vec![search(1, 40)];
        // A page's glyphs are a detour: the query that was running resumes after them.
        interrupt(&mut queue, search(2, 10), Request::Text(3));
        assert!(matches!(queue.pop(), Some(Request::Text(3))));
        assert!(matches!(
            queue.pop(),
            Some(Request::Search {
                query: 2,
                from: 10,
                ..
            })
        ));
        // A newer query replaces the one running and the older one still waiting under it.
        let mut queue = vec![search(1, 40), Request::Outline];
        interrupt(&mut queue, search(2, 10), search(3, 0));
        assert!(matches!(
            queue.pop(),
            Some(Request::Search {
                query: 3,
                from: 0,
                ..
            })
        ));
        assert!(matches!(queue.pop(), Some(Request::Outline)));
        assert!(queue.is_empty());
    }
}
