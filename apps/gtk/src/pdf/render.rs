//! The thread that owns one open document: everything that touches pdfium for a tab.
//!
//! One thread per document, because pdfium is serialised behind one process-wide lock (see
//! `accent_core::pdf`). [`spawn`] is the whole interface: it opens the file, reports the page
//! sizes and then answers [`Request`]s until the tab drops its sender.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

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
///
/// A file that will not open — a LaTeX run that failed, or has not finished writing it — does not
/// end the thread: it says why and waits for the tab to ask for the file again.
pub fn spawn(path: PathBuf, view: glib::SendWeakRef<PdfView>) -> Result<Sender<Request>, String> {
    if !pdf::available() {
        return Err("libpdfium was not found".to_string());
    }
    let (tx, rx) = channel::<Request>();
    std::thread::Builder::new()
        .name("accent-pdf".to_string())
        .spawn(move || {
            let mut path = path;
            loop {
                match open(&path) {
                    Ok((doc, sizes)) => {
                        send(&view, Reply::Reloaded(sizes));
                        match render_loop(doc, path, &rx, &view) {
                            Some(left) => path = left,
                            None => return,
                        }
                    }
                    Err(why) => send(&view, Reply::Failed(why)),
                }
                if !wait(&rx, &mut path) {
                    return;
                }
            }
        })
        .map_err(|e| format!("cannot start the renderer: {e}"))?;
    Ok(tx)
}

/// The document at `path` and its page sizes, or why there is none to show.
fn open(path: &Path) -> Result<(PdfDoc, Vec<(f32, f32)>), String> {
    let doc = PdfDoc::open(path).map_err(|e| format!("{e:#}"))?;
    let sizes = page_sizes(&doc);
    match sizes.is_empty() {
        true => Err("This file has no pages.".to_string()),
        false => Ok((doc, sizes)),
    }
}

/// No document to answer from: drop every request until the tab asks for the file again, and
/// say whether it did — false once the tab has gone. What is dropped answers itself: a save's
/// or a copy's channel closing is what its waiter hears.
fn wait(rx: &Receiver<Request>, path: &mut PathBuf) -> bool {
    while let Ok(request) = rx.recv() {
        match request {
            Request::Reload => return true,
            Request::Retarget(moved) => *path = moved,
            _ => {}
        }
    }
    false
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
/// nobody is looking at any more. Only a request of the same kind from the same asker abandons
/// it — see [`interrupt`] — and the queue is a stack, so the newest work is always what runs next
/// and what is put aside resumes after it.
///
/// Dropping an interrupted batch instead loses it for good. Nothing re-asks: the widget sends a
/// list of tiles again only when that list changes, and the tab sends a query again only when the
/// text does, so a tile nobody rendered stayed blurry and a search pushed aside reported the
/// matches of the pages it had reached and no more.
///
/// It returns when the tab goes away, with `None`, or with the path it was reading once a reload
/// finds the file will not open, the tab told so: the document goes with it.
fn render_loop(
    mut doc: PdfDoc,
    mut path: PathBuf,
    rx: &Receiver<Request>,
    view: &glib::SendWeakRef<PdfView>,
) -> Option<PathBuf> {
    // What the file looked like when this document was read. Every write from here updates it,
    // which is how the tab tells its own save from someone else's and does not reload over it.
    let mut etag = accent_core::fs::Etag::of(&path).ok();
    // What has been drawn here and which pages were edited, so Undo reaches this session's
    // changes and no others.
    let mut ink = Ink::default();
    // What the tab was last told Undo and Redo have to walk, so it hears again only on a change.
    let mut told = (false, false);
    // How many erases this thread has answered, taken or refused — what a list of a page is
    // stamped with, so the widget can tell one read before its latest erase landed.
    let mut erases: u64 = 0;
    // The pages whose text has already been read, for the highlights, the selections and the
    // exports that all want the same glyphs.
    let mut glyphs = Glyphs::new();
    // The pages an Export Highlights wrote on, under their numbers today: the Undo or Redo of a
    // delete swaps in the document from the other side of it, which the export may not have
    // reached, so they are drawn again after every page edit walked.
    let mut exported: HashSet<usize> = HashSet::new();
    // The tab has been told the file was written into under the document.
    let mut changed = false;
    // The channel closing is the tab going away.
    while let Ok(first) = rx.recv() {
        let mut queue = vec![first];
        loop {
            // Before every request and after the last, so the header hears of a step as soon
            // as it is taken, whatever tiles are still to come.
            if ink.history() != told {
                told = ink.history();
                let (undo, redo) = told;
                send(view, Reply::History { undo, redo });
            }
            let Some(current) = queue.pop() else {
                break;
            };
            // Written into in place, as pdflatex writes its output: every page pdfium has not
            // read yet would come out blank, and be kept as rendered. Nothing more is read from
            // this document; the tab is told once, and has the file read again. A save still
            // runs, for the etag gate to refuse it out loud.
            let exempt = matches!(
                current,
                Request::Reload | Request::Retarget(_) | Request::Save(_)
            );
            if !exempt && !doc.intact() {
                if !changed {
                    changed = true;
                    send(view, Reply::Changed);
                }
                continue;
            }
            match current {
                Request::Tiles {
                    from,
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
                                    from,
                                    scale,
                                    dark,
                                    theme,
                                    wants: wants[at..].to_vec(),
                                };
                                interrupt(&mut queue, rest, newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return None,
                            Err(TryRecvError::Empty) => {}
                        }
                        render_want(&doc, view, scale, dark, theme, wants[at]);
                        at += 1;
                    }
                }
                Request::Links(page) => {
                    if let Ok(links) = doc.links(page) {
                        send(view, Reply::Links(page, links));
                    }
                }
                Request::Text(page) => {
                    if let Some(found) = glyphs_of(&doc, &mut glyphs, page) {
                        send(view, Reply::Text(page, found.clone()));
                    }
                }
                Request::Outline => {
                    if let Ok(outline) = doc.outline() {
                        send(view, Reply::Outline(outline));
                    }
                }
                Request::Search {
                    query,
                    text,
                    options,
                    from,
                } => {
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
                                    options,
                                    from: at,
                                };
                                interrupt(&mut queue, rest, newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return None,
                            Err(TryRecvError::Empty) => {}
                        }
                        match doc.search(at, &text, options) {
                            Ok(hits) if !hits.is_empty() => send(
                                view,
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
                            send(view, Reply::PageChanged(page, area));
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
                            send(view, Reply::PageChanged(page, area));
                        }
                        Err(e) => tracing::warn!("drawing on page {page}: {e:#}"),
                    }
                }
                Request::Inks(page) => {
                    let inks = ink.named(&doc, page);
                    send(view, Reply::Inks { page, inks, erases });
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
                            send(view, Reply::PageChanged(page, area));
                        }
                        // Gone or refused, as for an erase below: the page's list goes back.
                        Err(e) => {
                            tracing::debug!("moving a stroke on page {page}: {e:#}");
                            let inks = ink.named(&doc, page);
                            send(view, Reply::Inks { page, inks, erases });
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
                            send(view, Reply::PageChanged(page, cut.area));
                            if behind {
                                let inks = ink.named(&doc, page);
                                send(view, Reply::Inks { page, inks, erases });
                            }
                        }
                        // Gone already — the list the eraser aimed at had not caught up with an
                        // erase before this one — or refused, or a pass that by the page itself
                        // missed. Either way the page's list goes back at once, so the two agree
                        // again rather than at the next change.
                        Err(e) => {
                            tracing::debug!("erasing on page {page}: {e:#}");
                            let inks = ink.named(&doc, page);
                            send(view, Reply::Inks { page, inks, erases });
                        }
                    }
                }
                Request::Pages(edit) => match ink.edit_pages(&mut doc, edit) {
                    Ok(step) => {
                        // Dirty like a stroke, so the tab's own timer writes it out: a page put
                        // in, taken out or moved is a change to the file and nothing else would
                        // save it.
                        ink.dirty = true;
                        repaged(&doc, &mut glyphs, &mut exported, view, edit, step);
                    }
                    Err(e) => tracing::warn!("{edit:?}: {e:#}"),
                },
                request @ (Request::Undo | Request::Redo) => {
                    let redo = matches!(request, Request::Redo);
                    for walked in ink.walk(&mut doc, redo) {
                        ink.dirty = true;
                        match walked {
                            pdf::Walked::Ink(page, area) => {
                                send(view, Reply::PageChanged(page, area))
                            }
                            pdf::Walked::Pages(edit, step) => {
                                repaged(&doc, &mut glyphs, &mut exported, view, edit, step);
                                for &page in &exported {
                                    let Ok(size) = doc.page_size(page) else {
                                        continue;
                                    };
                                    let all = pdf::Rect::from_corners((0.0, 0.0), size);
                                    send(view, Reply::PageChanged(page, all));
                                }
                            }
                        }
                    }
                }
                Request::Save(ack) => {
                    if !ink.dirty {
                        // The ack still goes: a caller waiting on it is waiting for the file to
                        // be right, and it already is.
                        drop(ack);
                        continue;
                    }
                    // ponytail: the whole file is rewritten, so a very large PDF stops this
                    // document's tiles for as long as that takes; `PdfDoc::save` says why
                    // pdfium's incremental save is no way out.
                    match doc.save().map_err(|e| e.to_string()).and_then(|bytes| {
                        accent_core::fs::write_bytes(&path, &bytes, etag).map_err(|e| e.to_string())
                    }) {
                        Ok(written) => {
                            etag = Some(written);
                            ink.dirty = false;
                            send(view, Reply::Saved(written));
                        }
                        // Left dirty on purpose: the next stroke's save tries again, and the
                        // drawing is still in the document either way. A file that cannot be
                        // written at all — read-only, or changed on disk since it was read —
                        // would otherwise take every stroke silently and lose the lot at close;
                        // the reader is told instead, and a foreign write is answered by the
                        // watcher's reload, which is what clears the ledger.
                        Err(e) => {
                            tracing::warn!("saving {}: {e}", path.display());
                            send(view, Reply::SaveFailed(e));
                        }
                    }
                    // Dropping the sender is the signal: the receiver's `recv` returns either
                    // way, so a failed save does not hang the window that is closing.
                    drop(ack);
                }
                Request::Highlights(links) => {
                    let quads: Highlights = pdf::highlight_quads(&doc, &mut glyphs, &links);
                    send(view, Reply::Highlights(quads));
                }
                Request::Export { links, color } => {
                    let quads = pdf::highlight_quads(&doc, &mut glyphs, &links);
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
                    let highlights = as_highlights(quads, &links, color);
                    let written = doc
                        .add_highlights(&highlights)
                        .and_then(|added| match added {
                            // Nothing new is not a write: the file is already what it should be.
                            0 => Ok(0),
                            _ => {
                                let bytes = doc.save()?;
                                let written = accent_core::fs::write_bytes(&path, &bytes, etag)?;
                                etag = Some(written);
                                send(view, Reply::Saved(written));
                                for (page, area) in pages {
                                    exported.insert(page);
                                    send(view, Reply::PageChanged(page, area));
                                }
                                Ok(added)
                            }
                        })
                        .map_err(|e| format!("{e:#}"));
                    send(view, Reply::Exported(written));
                }
                Request::Copy {
                    links,
                    color,
                    dest,
                    done,
                } => {
                    let quads = pdf::highlight_quads(&doc, &mut glyphs, &links);
                    let highlights = as_highlights(quads, &links, color);
                    let copied = copy(&doc, &highlights, &path, &dest);
                    let _ = done.send(copied.map_err(|e| format!("{e:#}")));
                }
                Request::Reload => {
                    // Reported changed and still the file this document was read from or last
                    // wrote: a `chmod`, a remote copy fetched again as it was. Read again, it
                    // would drop whatever has been drawn since the last save.
                    if etag.is_some() && accent_core::fs::Etag::of(&path).ok() == etag {
                        continue;
                    }
                    // A file that will not open takes the document with it: what it showed is
                    // of a file that is not there any more. The tab waits for the next write.
                    match open(&path) {
                        Ok((fresh, sizes)) => {
                            doc = fresh;
                            changed = false;
                            etag = accent_core::fs::Etag::of(&path).ok();
                            // The ledger is of the document that just went: its ids mirror an
                            // `/Annots` array this one need not share, so a later Ctrl+Z would
                            // resolve one to an index and delete whatever now sits there. It
                            // also carries `dirty`, which the fresh document is not. The ids
                            // come from a counter that never goes back, so a request still naming
                            // a stroke of the old document cannot land on one of the new.
                            ink = Ink::default();
                            // The text moved with the document, so what was read of it goes,
                            // and with the history no delete swaps a document back in.
                            glyphs.clear();
                            exported.clear();
                            send(view, Reply::Reloaded(sizes));
                        }
                        Err(why) => {
                            send(view, Reply::Failed(why));
                            return Some(path);
                        }
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
    None
}

/// Where the note links land, as `/Highlight` annotations in `color`, each carrying the text its
/// link quotes.
fn as_highlights(quads: Highlights, links: &[PdfLink], color: [u8; 3]) -> Vec<pdf::Highlight> {
    quads
        .into_iter()
        .flat_map(|(page, found)| found.into_iter().map(move |(quads, at)| (page, quads, at)))
        .map(|(page, quads, at)| pdf::Highlight {
            page,
            quads,
            color: [color[0], color[1], color[2], 255],
            contents: links.get(at).and_then(|l| l.alias.clone()),
        })
        .collect()
}

/// The document with `highlights` in it, written to `dest`. Never onto `path`, the file this
/// thread reads: the copy is what leaves that file as it is.
fn copy(doc: &PdfDoc, highlights: &[pdf::Highlight], path: &Path, dest: &Path) -> Result<()> {
    if let (Ok(from), Ok(to)) = (path.canonicalize(), dest.canonicalize())
        && from == to
    {
        return Err(anyhow!("that is the file being exported"));
    }
    let bytes = doc.copy_with_highlights(highlights)?;
    accent_core::fs::write_bytes(dest, &bytes, None)?;
    Ok(())
}

/// The pages were edited, or an edit taken back: the glyphs read of each page and the pages
/// exported to follow it to their new numbers, and the tab hears the page sizes and where every
/// page went.
fn repaged(
    doc: &PdfDoc,
    glyphs: &mut Glyphs,
    exported: &mut HashSet<usize>,
    view: &glib::SendWeakRef<PdfView>,
    edit: pdf::PageEdit,
    step: u32,
) {
    *glyphs = std::mem::take(glyphs)
        .into_iter()
        .filter_map(|(page, found)| Some((edit.map(page)?, found)))
        .collect();
    *exported = exported.iter().filter_map(|&page| edit.map(page)).collect();
    let sizes = page_sizes(doc);
    send(view, Reply::Repaged { sizes, edit, step });
}

/// Put `newer` at the top of the queue, and `rest` — what the interrupted batch has left to do —
/// under it or not at all.
///
/// Only a request of the same kind from the same asker takes a batch over: a newer viewport makes
/// that view's old tiles pointless, and a newer query makes the old query's remaining pages
/// pointless. It also drops any older remainder of its own still waiting further down, which is
/// the one a batch put aside earlier left there. Anything else — a link, a page's glyphs, an
/// outline, a reload, or the tiles another view of the document wants — is a short detour, and
/// the batch resumes once it is done.
fn interrupt(queue: &mut Vec<Request>, rest: Request, newer: Request) {
    // Only a batch replaces anything waiting, and `rest` is always one: a newer link is no reason
    // to drop an older one.
    if std::mem::discriminant(&rest) == std::mem::discriminant(&newer) {
        queue.retain(|waiting| !same_kind(waiting, &newer));
    }
    if !same_kind(&rest, &newer) {
        queue.push(rest);
    }
    queue.push(newer);
}

/// Whether two requests are the same kind of work, whatever they are for — and for tiles, from
/// the same asker. The reading view and the thumbnail strip each send the whole list they still
/// want, but only when that list changes, so one dropping the other's left its pages blank.
fn same_kind(a: &Request, b: &Request) -> bool {
    match (a, b) {
        (Request::Tiles { from: x, .. }, Request::Tiles { from: y, .. }) => x == y,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
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
    use super::super::protocol::Asker;
    use super::*;

    fn search(query: u64, from: usize) -> Request {
        Request::Search {
            query,
            text: "q".to_string(),
            options: Default::default(),
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

    fn tiles(from: Asker, page: u32) -> Request {
        let want = Want { page, tx: 0, ty: 0 };
        Request::Tiles {
            from,
            scale: 1.0,
            dark: false,
            theme: pdf::Theme::Plain,
            wants: vec![want],
        }
    }

    fn page_of(request: Option<Request>) -> Option<(Asker, u32)> {
        match request? {
            Request::Tiles { from, wants, .. } => Some((from, wants[0].page)),
            _ => None,
        }
    }

    /// With no document to answer from, the thread lets everything go but a reload, which is
    /// what it waits for — a save's waiter hearing its channel close — and follows a rename.
    #[test]
    fn a_thread_without_a_document_waits_for_a_reload() {
        let (tx, rx) = channel();
        let (ack, acked) = channel();
        tx.send(Request::Save(Some(ack))).unwrap();
        tx.send(Request::Retarget(PathBuf::from("moved.pdf")))
            .unwrap();
        tx.send(Request::Reload).unwrap();
        let mut path = PathBuf::from("paper.pdf");
        assert!(wait(&rx, &mut path));
        assert_eq!(path, PathBuf::from("moved.pdf"));
        assert!(acked.recv().is_err(), "the save's waiter is let go");
        drop(tx);
        assert!(!wait(&rx, &mut path), "the tab went away");
    }

    /// The strip's batch is a detour for the reading view's, not its replacement: the reading
    /// view asks again only when its own list changes, so what was dropped stayed blank.
    #[test]
    fn one_view_s_tiles_do_not_abandon_another_s() {
        let mut queue = vec![Request::Outline];
        interrupt(&mut queue, tiles(Asker::Reader, 7), tiles(Asker::Strip, 40));
        assert_eq!(page_of(queue.pop()), Some((Asker::Strip, 40)));
        assert_eq!(page_of(queue.pop()), Some((Asker::Reader, 7)));
        assert!(matches!(queue.pop(), Some(Request::Outline)));
        // A newer batch from the same view still replaces its own, the one waiting included.
        let mut queue = vec![tiles(Asker::Reader, 7)];
        interrupt(&mut queue, tiles(Asker::Strip, 40), tiles(Asker::Reader, 9));
        assert_eq!(page_of(queue.pop()), Some((Asker::Reader, 9)));
        assert_eq!(page_of(queue.pop()), Some((Asker::Strip, 40)));
        assert!(queue.is_empty());
    }
}
