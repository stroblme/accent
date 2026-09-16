//! The thread that owns one open document: everything that touches pdfium for a tab.
//!
//! One thread per document, because pdfium is serialised behind one process-wide lock (see
//! `accent_core::pdf`). [`spawn`] is the whole interface: it opens the file, reports the page
//! sizes and then answers [`Request`]s until the tab drops its sender.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, TryRecvError, channel};

use accent_api::PdfLink;
use accent_core::pdf::{self, Ink, PdfDoc};
use anyhow::{Result, anyhow};
use gtk::glib;

use super::protocol::{Request, fresh_id};
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

/// Where each note link lands on the page today, per page, with the index of the link it is.
///
/// The four numbers first, and the text the link quotes as the fallback — a document rebuilt
/// with different line breaks moves the numbers but not the sentence. A link whose quads a real
/// `/Highlight` already covers is left out: it has been exported, and the annotation is in the
/// page's own pixels.
fn highlight_quads(doc: &PdfDoc, glyphs: &mut Glyphs, links: &[PdfLink]) -> Highlights {
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
                let hits = doc.search(page, text).ok()?;
                // The link's own line number is still a hint at where on the page it was, even
                // when its numbering no longer fits: the nearest hit to that line, rather than
                // the first on the page, is what a second copy of the same phrase above it used
                // to steal.
                let Some(want) = pdf::line_top(found, link.selection[0]) else {
                    return hits.into_iter().next();
                };
                let distance =
                    |quads: &[pdf::Rect]| quads.first().map_or(f32::MAX, |q| (q.top - want).abs());
                hits.into_iter()
                    .min_by(|a, b| distance(a).total_cmp(&distance(b)))
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

/// The glyphs of the pages this thread has read, kept until the document is re-read.
///
/// Extracting a page's text is most of a `Highlights` request, and that fires 300 ms after every
/// note save; the same page is asked for again by a drag over it and again by an export. pdfium's
/// own search keeps its own page text, so a query does not come through here.
type Glyphs = HashMap<usize, Vec<pdf::Glyph>>;

/// One page's glyphs, read once. `None` for a page whose text pdfium would not give us, which is
/// left uncached so the next ask tries again.
fn glyphs_of<'a>(doc: &PdfDoc, cache: &'a mut Glyphs, page: usize) -> Option<&'a Vec<pdf::Glyph>> {
    // Not `entry`: a page pdfium would not read must not be remembered as an empty one, so the
    // next ask tries again.
    match cache.entry(page) {
        std::collections::hash_map::Entry::Occupied(found) => Some(found.into_mut()),
        std::collections::hash_map::Entry::Vacant(slot) => {
            Some(slot.insert(doc.page_text(page).ok()?))
        }
    }
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
    mut path: PathBuf,
    rx: std::sync::mpsc::Receiver<Request>,
    view: glib::SendWeakRef<PdfView>,
) {
    // What the file looked like when this document was read. Every write from here updates it,
    // which is how the tab tells its own save from someone else's and does not reload over it.
    let mut etag = accent_core::fs::Etag::of(&path).ok();
    // What has been drawn here, so Undo reaches this session's strokes and no others.
    let mut ink = Ink::default();
    // What the tab was last told Undo and Redo have to walk, so it hears again only on a change.
    let mut told = (false, false);
    // How many erases this thread has answered, taken or refused — what a list of a page is
    // stamped with, so the widget can tell one read before its latest erase landed.
    let mut erases: u64 = 0;
    // The pages whose text has already been read, for the highlights, the selections and the
    // exports that all want the same glyphs.
    let mut glyphs = Glyphs::new();
    // The channel closing is the tab going away, which is the only way this thread ends.
    while let Ok(first) = rx.recv() {
        let mut queue = vec![first];
        loop {
            // Before every request and after the last, so the header hears of a step as soon
            // as it is taken, whatever tiles are still to come.
            if ink.history() != told {
                told = ink.history();
                let (undo, redo) = told;
                send(&view, Reply::History { undo, redo });
            }
            let Some(current) = queue.pop() else {
                break;
            };
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
                    if let Some(found) = glyphs_of(&doc, &mut glyphs, page) {
                        send(&view, Reply::Text(page, found.clone()));
                    }
                }
                Request::Outline => {
                    if let Ok(outline) = doc.outline() {
                        send(&view, Reply::Outline(outline));
                    }
                }
                Request::Search { query, text, from } => {
                    // An empty query is the bar being cleared or closed: it has already pushed
                    // aside whatever was running, and there is nothing to look for.
                    if text.is_empty() {
                        continue;
                    }
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
                        Ok(area) => {
                            ink.drew(page, before);
                            ink.dirty = true;
                            send(&view, Reply::PageChanged(page, area));
                        }
                        Err(e) => tracing::warn!("drawing on page {page}: {e:#}"),
                    }
                }
                Request::Shape { page, shape, style } => {
                    let before = doc.annotation_count(page).unwrap_or(0);
                    match doc.add_shape(page, shape, style) {
                        Ok(area) => {
                            ink.drew(page, before);
                            ink.dirty = true;
                            send(&view, Reply::PageChanged(page, area));
                        }
                        Err(e) => tracing::warn!("drawing on page {page}: {e:#}"),
                    }
                }
                Request::Inks(page) => {
                    let inks = ink.named(&doc, page);
                    send(&view, Reply::Inks { page, inks, erases });
                }
                Request::Transform { page, id, matrix } => {
                    ink.note(page, doc.annotation_count(page).unwrap_or(0));
                    let moved = ink
                        .index_of(page, id)
                        .ok_or_else(|| anyhow!("stroke {id} is gone"))
                        .and_then(|index| Ok((index, doc.transform_ink(page, index, matrix)?)));
                    match moved {
                        Ok((index, area)) => {
                            ink.moved(page, index, matrix);
                            ink.dirty = true;
                            send(&view, Reply::PageChanged(page, area));
                        }
                        // Gone or refused, as for an erase below: the page's list goes back.
                        Err(e) => {
                            tracing::debug!("moving a stroke on page {page}: {e:#}");
                            let inks = ink.named(&doc, page);
                            send(&view, Reply::Inks { page, inks, erases });
                        }
                    }
                }
                Request::Erase {
                    page,
                    id,
                    joined,
                    partial,
                } => {
                    erases += 1;
                    ink.note(page, doc.annotation_count(page).unwrap_or(0));
                    let erased = ink
                        .index_of(page, id)
                        .ok_or_else(|| anyhow!("stroke {id} is gone"))
                        .and_then(|index| {
                            let cut = match &partial {
                                None => {
                                    let (was, area) = doc.take_ink(page, index)?;
                                    let left = Vec::new();
                                    Some(pdf::Cut { was, left, area })
                                }
                                Some(pass) => {
                                    doc.cut_ink(page, index, pass.from, pass.to, pass.radius)?
                                }
                            };
                            let missed = || anyhow!("the pass missed stroke {id}");
                            cut.map(|cut| (index, cut)).ok_or_else(missed)
                        });
                    match erased {
                        Ok((index, cut)) => {
                            // The pieces keep the names the widget gave them when it cut the
                            // stroke the same way. When it did not, its list was behind the page,
                            // and it hears the page again.
                            let given = partial.map(|pass| pass.pieces).unwrap_or_default();
                            let behind = given.len() != cut.left.len();
                            let names: Vec<u32> = match behind {
                                false => given,
                                true => cut.left.iter().map(|_| fresh_id()).collect(),
                            };
                            let left = names.into_iter().zip(cut.left).collect();
                            ink.erased(page, index, cut.was, left, joined);
                            ink.dirty = true;
                            send(&view, Reply::PageChanged(page, cut.area));
                            if behind {
                                let inks = ink.named(&doc, page);
                                send(&view, Reply::Inks { page, inks, erases });
                            }
                        }
                        // Gone already — the list the eraser aimed at had not caught up with an
                        // erase before this one — or refused, or a pass that by the page itself
                        // missed. Either way the page's list goes back at once, so the two agree
                        // again rather than at the next change.
                        Err(e) => {
                            tracing::debug!("erasing on page {page}: {e:#}");
                            let inks = ink.named(&doc, page);
                            send(&view, Reply::Inks { page, inks, erases });
                        }
                    }
                }
                Request::AddPage => match doc.add_page() {
                    Ok(()) => {
                        // Dirty like a stroke, so the tab's own timer writes it out: an appended
                        // page is a change to the file and nothing else would save it.
                        ink.dirty = true;
                        send(&view, Reply::Paged(page_sizes(&doc)));
                    }
                    Err(e) => tracing::warn!("appending a page: {e:#}"),
                },
                request @ (Request::Undo | Request::Redo) => {
                    for (page, area) in ink.walk(&mut doc, matches!(request, Request::Redo)) {
                        ink.dirty = true;
                        send(&view, Reply::PageChanged(page, area));
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
                    let quads = highlight_quads(&doc, &mut glyphs, &links);
                    send(&view, Reply::Highlights(quads));
                }
                Request::Export { links, color } => {
                    let quads = highlight_quads(&doc, &mut glyphs, &links);
                    // What each page gains, so only those tiles are rendered again.
                    let pages: Vec<(usize, pdf::Rect)> = quads
                        .iter()
                        .filter_map(|(page, found)| {
                            let area = found
                                .iter()
                                .flat_map(|(quads, _)| quads)
                                .copied()
                                .reduce(pdf::Rect::union)?;
                            Some((*page, area))
                        })
                        .collect();
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
                                for (page, area) in pages {
                                    send(&view, Reply::PageChanged(page, area));
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
                            // also carries `dirty`, which the fresh document is not. The ids
                            // come from a counter that never goes back, so a request still naming
                            // a stroke of the old document cannot land on one of the new.
                            ink = Ink::default();
                            // The text moved with the document, so what was read of it goes.
                            glyphs.clear();
                            send(&view, Reply::Reloaded(page_sizes(&doc)));
                        }
                        Err(e) => tracing::debug!("reloading {}: {e:#}", path.display()),
                    }
                }
                Request::Retarget(moved) => {
                    // Only where the bytes live moves. The open document is the same document
                    // and every tile of it is still of that document, so re-opening would throw
                    // away the cache and the ink ledger for a change of name.
                    //
                    // The etag comes from the new name: a local rename carries it over intact,
                    // and on a remote vault the copy under the new name may not have been
                    // fetched yet, which leaves `None` and lets the next save write it.
                    etag = accent_core::fs::Etag::of(&moved).ok();
                    path = moved;
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
