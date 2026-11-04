//! The thread that owns one open document: everything that touches pdfium for a tab.
//!
//! One thread per document, because pdfium is serialised behind one process-wide lock (see
//! `accent_core::pdf`). [`spawn`] is the whole interface: it opens the file, reports the page
//! sizes and then answers [`Request`]s until the tab drops its sender.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use accent_core::pdf::{self, Ink, PdfDoc};
use anyhow::{Result, anyhow};
use gtk::glib;

use super::protocol::{Clipping, Request, fresh_id};
use super::{Highlights, LOWRES_W, PdfView, Reply, TILE, TileKey, Want};

/// Open and own `PdfDoc` on the render thread until its sender is dropped. All pdfium calls
/// share a process-wide lock because concurrent calls abort.
///
/// Opening failure reports the reason and waits for a reload request, allowing incomplete LaTeX
/// output to recover.
pub fn spawn(path: PathBuf, view: glib::SendWeakRef<PdfView>) -> Result<Sender<Request>, String> {
    if !pdf::available() {
        return Err("libpdfium was not found".to_string());
    }
    let (tx, rx) = channel::<Request>();
    std::thread::Builder::new()
        .name("accent-pdf".to_string())
        .spawn(move || {
            let mut path = path;
            // How many erases the tab has sent, answered or dropped, across every opening of the
            // file: what a list of a page is stamped with, so the view can tell one read before
            // its latest erase landed. The view counts every erase it sends and never starts
            // again, so neither does this.
            let mut erases: u64 = 0;
            loop {
                match open(&path) {
                    Ok((doc, sizes)) => {
                        send(&view, Reply::Reloaded(sizes));
                        match render_loop(doc, path, &rx, &view, &mut erases) {
                            Some(left) => path = left,
                            None => return,
                        }
                    }
                    Err(why) => send(&view, Reply::Failed(why)),
                }
                if !wait(&rx, &mut path, &mut erases) {
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
/// or a copy's channel closing is what its waiter hears. An erase dropped is still counted.
fn wait(rx: &Receiver<Request>, path: &mut PathBuf, erases: &mut u64) -> bool {
    while let Ok(request) = rx.recv() {
        if relists(&request) {
            *erases += 1;
        }
        match request {
            Request::Reload => return true,
            Request::Retarget(moved) => *path = moved,
            _ => {}
        }
    }
    false
}

/// Whether the view counts `request` as an erase ([`spawn`]): a change to a page's strokes the
/// view makes to its own list as it sends it, so a list read before it is of the page before.
fn relists(request: &Request) -> bool {
    matches!(
        request,
        Request::Erase { .. }
            | Request::Transform { .. }
            | Request::Delete { .. }
            | Request::Recolour { .. }
            | Request::Paste { .. }
            | Request::Clip {
                how: Clipping::Cut,
                ..
            }
    )
}

/// Cache extracted page glyphs until reload, sharing the costly extraction between highlights,
/// selection and export. PDF search uses pdfium's separate text cache.
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

/// Read page sizes without loading page content, avoiding expensive full-page parses before
/// layout.
fn page_sizes(doc: &PdfDoc) -> Vec<(f32, f32)> {
    doc.page_sizes().unwrap_or_default()
}

/// Process requests in a stack, checking new work before every tile or searched page. Only newer
/// work of the same kind and asker replaces a batch ([`interrupt`]); other work suspends it
/// until the detour finishes.
///
/// Retain suspended batches because unchanged tile lists and queries may never be requested
/// again. Return `None` when the tab closes, or the document path after a failed reload reports
/// its error. `erases` is [`spawn`]'s count, which goes on from opening to opening.
fn render_loop(
    mut doc: PdfDoc,
    mut path: PathBuf,
    rx: &Receiver<Request>,
    view: &glib::SendWeakRef<PdfView>,
    erases: &mut u64,
) -> Option<PathBuf> {
    // What the file looked like when this document was read. Every write from here updates it,
    // which is how the tab tells its own save from someone else's and does not reload over it.
    let mut etag = accent_core::fs::Etag::of(&path).ok();
    // What has been drawn here and which pages were edited, so Undo reaches this session's
    // changes and no others.
    let mut ink = Ink::default();
    // What the tab was last told Undo and Redo have to walk, so it hears again only on a change.
    let mut told = (false, false);
    // The pages whose text has already been read, for the highlights, the selections and the
    // exports that all want the same glyphs.
    let mut glyphs = Glyphs::new();
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
            // Counted however it ends, one refused below as well: the view counts what it sends.
            if relists(&current) {
                *erases += 1;
            }
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
                                    theme,
                                    wants: wants[at..].to_vec(),
                                };
                                interrupt(&mut queue, rest, newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return None,
                            Err(TryRecvError::Empty) => {}
                        }
                        render_want(&doc, view, scale, theme, wants[at]);
                        at += 1;
                    }
                }
                Request::Links(page) => {
                    if let Ok(links) = doc.links(page) {
                        send(view, Reply::Links(page, links));
                    }
                    if let Ok(comments) = doc.comments(page) {
                        send(view, Reply::Comments(page, comments));
                    }
                }
                Request::Comments(mut page) => {
                    // A page at a time, as a search walks: a tile asked for meanwhile waits for
                    // one page's annotations at most. A page that will not read is said to have
                    // none, so what the tab kept of it from before goes.
                    let pages = doc.page_count();
                    while page < pages {
                        match rx.try_recv() {
                            Ok(newer) => {
                                interrupt(&mut queue, Request::Comments(page), newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return None,
                            Err(TryRecvError::Empty) => {}
                        }
                        let comments = doc.comments(page).unwrap_or_default();
                        send(view, Reply::Comments(page, comments));
                        page += 1;
                    }
                }
                Request::Text(page) => {
                    if let Some(found) = glyphs_of(&doc, &mut glyphs, page) {
                        send(view, Reply::Text(page, found.clone()));
                    }
                }
                Request::Outline => {
                    if let Ok(outline) = doc.outline() {
                        send(view, Reply::Outline(outline, doc.info()));
                    }
                }
                Request::Search {
                    query,
                    text,
                    options,
                    from,
                    mut walked,
                } => {
                    // An empty query is the bar being cleared or closed: it has already pushed
                    // aside whatever was running, and there is nothing to look for.
                    if text.is_empty() {
                        continue;
                    }
                    let pages = doc.page_count();
                    while walked < pages {
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
                                    from,
                                    walked,
                                };
                                interrupt(&mut queue, rest, newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return None,
                            Err(TryRecvError::Empty) => {}
                        }
                        let at = (from + walked) % pages;
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
                        walked += 1;
                    }
                }
                Request::Ink {
                    page,
                    points,
                    style,
                    scale,
                } => match doc.add_ink(page, &points, style, scale) {
                    Ok((area, at)) => {
                        ink.drew(page, at);
                        ink.dirty = true;
                        let drawn = ink.named_at(&doc, page, at);
                        send(view, Reply::PageChanged(page, area, drawn));
                    }
                    Err(e) => tracing::warn!("drawing on page {page}: {e:#}"),
                },
                Request::Shape { page, shape, style } => match doc.add_shape(page, shape, style) {
                    Ok((area, at)) => {
                        ink.drew(page, at);
                        ink.dirty = true;
                        let drawn = ink.named_at(&doc, page, at);
                        send(view, Reply::PageChanged(page, area, drawn));
                    }
                    Err(e) => tracing::warn!("drawing on page {page}: {e:#}"),
                },
                Request::Inks(page) => {
                    send(view, listed(&mut ink, &doc, page, *erases));
                }
                Request::Type {
                    page,
                    at,
                    text,
                    style,
                } => match doc.add_text(page, at, &text, style) {
                    Ok((area, typed)) => {
                        ink.drew(page, typed);
                        ink.dirty = true;
                        send(view, Reply::PageChanged(page, area, None));
                    }
                    Err(e) => tracing::warn!("typing on page {page}: {e:#}"),
                },
                Request::Retype {
                    page,
                    id,
                    text,
                    style,
                } => {
                    let typed = ink
                        .at(id)
                        .ok_or_else(|| anyhow!("text {id} is gone"))
                        .and_then(|at| Ok((at, doc.retype(page, at, &text, style)?)));
                    match typed {
                        Ok((at, (was, area))) => {
                            ink.retyped(page, at, was);
                            ink.dirty = true;
                            send(view, Reply::PageChanged(page, area, None));
                        }
                        // Gone, as an erase's stroke can be: the page's lists go back.
                        Err(e) => {
                            tracing::debug!("typing again on page {page}: {e:#}");
                            send(view, listed(&mut ink, &doc, page, *erases));
                        }
                    }
                }
                Request::Transform { page, ids, matrix } => {
                    let moved = ink.transform(&mut doc, page, &ids, matrix);
                    answered(&mut ink, &doc, view, page, moved, *erases);
                }
                Request::Delete { page, ids } => {
                    let taken = ink.delete(&mut doc, page, &ids);
                    answered(&mut ink, &doc, view, page, taken, *erases);
                }
                Request::Recolour { page, ids, rgb } => {
                    let made = ink.recolour(&mut doc, page, &ids, rgb);
                    let made = made.map(|(area, pieces)| {
                        if !pieces.is_empty() {
                            send(view, Reply::Redrawn { page, pieces });
                        }
                        area
                    });
                    answered(&mut ink, &doc, view, page, made, *erases);
                }
                Request::Clip { page, ids, how } => {
                    let (copied, clips): (Vec<u32>, Vec<Vec<pdf::Clip>>) =
                        ink.clips(&mut doc, page, &ids).into_iter().unzip();
                    let clips = clips.concat();
                    send(view, Reply::Clipped { page, clips, how });
                    // What would not copy stays: a cut never loses what the clipboard lacks.
                    if how == Clipping::Cut {
                        let taken = ink.delete(&mut doc, page, &copied);
                        answered(&mut ink, &doc, view, page, taken, *erases);
                    }
                }
                Request::Paste {
                    page,
                    clips,
                    matrix,
                    names,
                } => match ink.paste(&mut doc, page, &clips, matrix, &names) {
                    Ok(area) => {
                        ink.dirty = true;
                        send(view, Reply::PageChanged(page, area, None));
                    }
                    Err(e) => tracing::warn!("pasting on page {page}: {e:#}"),
                },
                Request::Erase {
                    page,
                    id,
                    joined,
                    partial,
                } => {
                    let erased = ink
                        .at(id)
                        .ok_or_else(|| anyhow!("stroke {id} is gone"))
                        .and_then(|at| {
                            let cut = match &partial {
                                None => {
                                    let (was, area) = doc.take_ink(page, at)?;
                                    let left = Vec::new();
                                    Some(pdf::Cut { was, left, area })
                                }
                                Some(pass) => {
                                    doc.cut_ink(page, at, pass.from, pass.to, pass.radius)?
                                }
                            };
                            let missed = || anyhow!("the pass missed stroke {id}");
                            cut.map(|cut| (at, cut)).ok_or_else(missed)
                        });
                    match erased {
                        Ok((at, cut)) => {
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
                            ink.erased(page, at, cut.was, left, joined);
                            ink.dirty = true;
                            send(view, Reply::PageChanged(page, cut.area, None));
                            if behind {
                                send(view, listed(&mut ink, &doc, page, *erases));
                            }
                        }
                        // Gone already — the list the eraser aimed at had not caught up with an
                        // erase before this one — or refused, or a pass that by the page itself
                        // missed. Either way the page's list goes back at once, so the two agree
                        // again rather than at the next change.
                        Err(e) => {
                            tracing::debug!("erasing on page {page}: {e:#}");
                            send(view, listed(&mut ink, &doc, page, *erases));
                        }
                    }
                }
                Request::Pages(edit) => match ink.edit_pages(&mut doc, edit) {
                    Ok(step) => {
                        // Dirty like a stroke, so the tab's own timer writes it out: a page put
                        // in, taken out or moved is a change to the file and nothing else would
                        // save it.
                        ink.dirty = true;
                        repaged(&doc, &mut glyphs, view, edit, step);
                    }
                    Err(e) => tracing::warn!("{edit:?}: {e:#}"),
                },
                Request::Import { at, source, name } => {
                    let before = doc.page_count();
                    // pdfium's reason goes to the log; the reader can only drop another file.
                    let imported = PdfDoc::open(&source)
                        .map_err(|e| (e, "it does not open as a PDF"))
                        .and_then(|source| {
                            let made = ink.import_pages(&mut doc, at, &source);
                            made.map_err(|e| (e, "its pages would not copy"))
                        });
                    let pages = match imported {
                        Ok((edit, step)) => {
                            ink.dirty = true;
                            repaged(&doc, &mut glyphs, view, edit, step);
                            Ok(doc.page_count() - before)
                        }
                        Err((e, why)) => {
                            tracing::warn!("importing {}: {e:#}", source.display());
                            Err(why.to_string())
                        }
                    };
                    send(view, Reply::Imported { name, pages });
                }
                request @ (Request::Undo | Request::Redo) => {
                    let redo = matches!(request, Request::Redo);
                    for walked in ink.walk(&mut doc, redo) {
                        ink.dirty = true;
                        match walked {
                            pdf::Walked::Ink(page, area) => {
                                send(view, Reply::PageChanged(page, area, None))
                            }
                            pdf::Walked::Pages(edit, step) => {
                                repaged(&doc, &mut glyphs, view, edit, step)
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
                    match doc.save_to(&path, etag).map_err(|e| e.to_string()) {
                        Ok(written) => {
                            etag = Some(written);
                            ink.dirty = false;
                            let paged = std::mem::take(&mut ink.paged);
                            send(
                                view,
                                Reply::Saved {
                                    etag: written,
                                    paged,
                                },
                            );
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
                Request::Export {
                    links,
                    color: [r, g, b],
                } => {
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
                    let highlights = pdf::link_annotations(quads, &links, [r, g, b, 255]);
                    let written = doc
                        .add_highlights(&highlights)
                        .map_err(anyhow::Error::from)
                        .and_then(|added| match added {
                            // Nothing new is not a write: the file is already what it should be.
                            0 => Ok(0),
                            _ => {
                                let written = doc.save_to(&path, etag)?;
                                etag = Some(written);
                                let paged = std::mem::take(&mut ink.paged);
                                send(
                                    view,
                                    Reply::Saved {
                                        etag: written,
                                        paged,
                                    },
                                );
                                for (page, area) in pages {
                                    send(view, Reply::PageChanged(page, area, None));
                                }
                                Ok(added)
                            }
                        })
                        .map_err(|e| format!("{e:#}"));
                    send(view, Reply::Exported(written));
                }
                Request::Copy {
                    links,
                    color: [r, g, b],
                    dest,
                    done,
                } => {
                    let quads = pdf::highlight_quads(&doc, &mut glyphs, &links);
                    let highlights = pdf::link_annotations(quads, &links, [r, g, b, 255]);
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
                            // The text moved with the document, so what was read of it goes.
                            glyphs.clear();
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

/// A change the Select tool asked of `page` was made, or none of it could be: the page's tiles
/// go, or its lists go back at once, so the view and the page agree again (as an erase's do).
fn answered(
    ink: &mut Ink,
    doc: &PdfDoc,
    view: &glib::SendWeakRef<PdfView>,
    page: usize,
    made: accent_core::Result<pdf::Rect>,
    erases: u64,
) {
    match made {
        Ok(area) => {
            ink.dirty = true;
            send(view, Reply::PageChanged(page, area, None));
        }
        Err(e) => {
            tracing::debug!("changing strokes on page {page}: {e:#}");
            send(view, listed(ink, doc, page, erases));
        }
    }
}

/// Every stroke, text and picture of `page` under its name, stamped with the erases counted so
/// far ([`spawn`]).
fn listed(ink: &mut Ink, doc: &PdfDoc, page: usize, erases: u64) -> Reply {
    let inks = ink.named(doc, page);
    let texts = ink.named_texts(doc, page);
    let pictures = ink.named_pictures(doc, page);
    Reply::Inks {
        page,
        inks,
        texts,
        pictures,
        erases,
    }
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

/// The pages were edited, or an edit taken back: the glyphs read of each page follow it to their
/// new numbers, and the tab hears the page sizes and where every page went.
fn repaged(
    doc: &PdfDoc,
    glyphs: &mut Glyphs,
    view: &glib::SendWeakRef<PdfView>,
    edit: pdf::PageEdit,
    step: u32,
) {
    *glyphs = std::mem::take(glyphs)
        .into_iter()
        .filter_map(|(page, found)| Some((edit.map(page)?, found)))
        .collect();
    let sizes = page_sizes(doc);
    send(view, Reply::Repaged { sizes, edit, step });
}

/// Push `newer` above the unfinished `rest`. Same-kind work from the same asker replaces the
/// batch and removes its older queued remainders. Other requests are detours, after which the
/// batch resumes.
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
                    theme,
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
            theme,
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

    /// A search started on the fourth page, `walked` pages on from there.
    fn search(query: u64, walked: usize) -> Request {
        Request::Search {
            query,
            text: "q".to_string(),
            options: Default::default(),
            from: 3,
            walked,
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
                from: 3,
                walked: 10,
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
                walked: 0,
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
    /// what it waits for — a save's waiter hearing its channel close — and follows a rename. An
    /// erase it lets go still counts, or every list it sends after the file opens again would
    /// be older than the view's last erase.
    #[test]
    fn a_thread_without_a_document_waits_for_a_reload() {
        let (tx, rx) = channel();
        let (ack, acked) = channel();
        tx.send(Request::Save(Some(ack))).unwrap();
        tx.send(Request::Retarget(PathBuf::from("moved.pdf")))
            .unwrap();
        tx.send(Request::Erase {
            page: 0,
            id: 1,
            joined: false,
            partial: None,
        })
        .unwrap();
        tx.send(Request::Reload).unwrap();
        let (mut path, mut erases) = (PathBuf::from("paper.pdf"), 1);
        assert!(wait(&rx, &mut path, &mut erases));
        assert_eq!(path, PathBuf::from("moved.pdf"));
        assert!(acked.recv().is_err(), "the save's waiter is let go");
        assert_eq!(erases, 2, "the erase it dropped is counted on");
        drop(tx);
        assert!(!wait(&rx, &mut path, &mut erases), "the tab went away");
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
