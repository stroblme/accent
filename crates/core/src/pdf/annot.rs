//! Highlight and ink annotations, with safe appearance-stream colour handling.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::os::raw::{c_int, c_ulong};

use super::refused;
use crate::{Error, Result};
use pdfium_render::prelude::*;

use super::ink::{
    Drawn, Seg, apply, catmull_rom, cut, flatten, points_of, segments_of, thin, transformed,
};
use super::text::{line_top, same_quads, selection_quads};
use super::{
    Comment, Cut, Glyph, Highlight, InkPath, InkShape, InkStyle, Matrix, PdfDoc, Rect, Shape, lock,
};
use crate::index::PdfLink;

/// Where the note links that highlight a document land, per page: the quads to paint and the
/// index of the link they came from.
pub type LinkHighlights = HashMap<usize, Vec<(Vec<Rect>, usize)>>;

/// Locate note-link highlights by page and link index.
///
/// Try the stored glyph numbers first, then the quoted text if a rebuild moved them. Omit quads
/// already covered by an exported `/Highlight` annotation.
///
/// `glyphs` caches extracted page text across selection and export.
pub fn highlight_quads(
    doc: &PdfDoc,
    glyphs: &mut HashMap<usize, Vec<Glyph>>,
    links: &[PdfLink],
) -> LinkHighlights {
    let mut existing: HashMap<usize, Vec<Highlight>> = HashMap::new();
    let mut out = LinkHighlights::new();
    for (at, link) in links.iter().enumerate() {
        let page = link.page;
        let found = glyphs
            .entry(page)
            .or_insert_with(|| doc.page_text(page).unwrap_or_default());
        let quads = selection_quads(found, link.selection)
            .map(|(_, quads)| quads)
            .or_else(|| {
                let text = link.alias.as_deref()?;
                let hits = doc.search(page, text, Default::default()).ok()?;
                // The link's own line number is still a hint at where on the page it was, even
                // when its numbering no longer fits: the nearest hit to that line, rather than
                // the first on the page, is what a second copy of the same phrase above it used
                // to steal.
                let Some(want) = line_top(found, link.selection[0]) else {
                    return hits.into_iter().next();
                };
                let distance =
                    |quads: &[Rect]| quads.first().map_or(f32::MAX, |q| (q.top - want).abs());
                hits.into_iter()
                    .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            });
        let Some(quads) = quads.filter(|q| !q.is_empty()) else {
            continue;
        };
        let already = existing
            .entry(page)
            .or_insert_with(|| doc.highlights_on(page).unwrap_or_default());
        if already.iter().any(|h| same_quads(&h.quads, &quads)) {
            continue;
        }
        out.entry(page).or_default().push((quads, at));
    }
    out
}

/// Where the note links land ([`highlight_quads`]) as `/Highlight` annotations in `color`, each
/// carrying the text its link quotes: what Export Highlights and a print copy write.
pub fn link_annotations(
    found: LinkHighlights,
    links: &[PdfLink],
    color: [u8; 4],
) -> Vec<Highlight> {
    found
        .into_iter()
        .flat_map(|(page, quads)| quads.into_iter().map(move |(quads, at)| (page, quads, at)))
        .map(|(page, quads, at)| Highlight {
            page,
            quads,
            color,
            contents: links.get(at).and_then(|l| l.alias.clone()),
        })
        .collect()
}

impl PdfDoc {
    /// Existing `/Highlight` annotations on one page.
    pub fn highlights_on(&self, page: usize) -> Result<Vec<Highlight>> {
        let _guard = lock();
        Ok(highlights_of(page, &self.page(page)?))
    }

    /// Existing `/Highlight` annotations across the whole document.
    pub fn highlights(&self) -> Result<Vec<Highlight>> {
        let _guard = lock();
        Ok(self
            .doc()
            .pages()
            .iter()
            .enumerate()
            .flat_map(|(page, p)| highlights_of(page, &p))
            .collect())
    }

    /// What other readers wrote on one page: each viewable annotation's `/Contents`, in `/Annots`
    /// order. A link's and a form field's text is no comment, a popup's is its parent's, and a
    /// `/FreeText`'s is already on the page.
    pub fn comments(&self, page: usize) -> Result<Vec<Comment>> {
        use PdfPageAnnotationType::{FreeText, Link, Popup, Widget, XfaWidget};
        let _guard = lock();
        let p = self.page(page)?;
        let height = p.height().value;
        Ok(p.annotations()
            .iter()
            .filter(|a| {
                !matches!(
                    a.annotation_type(),
                    Link | Popup | Widget | XfaWidget | FreeText
                )
            })
            .filter(|a| !a.is_hidden() && !a.is_printable_but_not_viewable())
            .filter_map(|a| {
                let text = a.contents().filter(|t| !t.trim().is_empty())?;
                Some(Comment {
                    areas: areas(&a, height),
                    text,
                    author: a.creator().filter(|t| !t.trim().is_empty()),
                })
            })
            .collect())
    }

    /// Write `/Highlight` annotations into the in-memory document, and say how many were written.
    ///
    /// A highlight whose quads a highlight on that page already covers is skipped, so exporting
    /// the same note links twice adds nothing and the painted overlay can step aside for the
    /// annotation as soon as it is in the file.
    ///
    /// Nothing reaches the disk here: [`PdfDoc::save`] is the second half.
    pub fn add_highlights(&mut self, highlights: &[Highlight]) -> Result<usize> {
        let _guard = lock();
        let mut added = 0;
        let mut ours: HashMap<usize, usize> = HashMap::new();
        // What each page already carries, read once for that page and kept up to date as this
        // call adds to it. Asking the page again per highlight walked its `/Annots` array for
        // every one of them, which on a page with many is quadratic.
        let mut known: HashMap<usize, Vec<Vec<Rect>>> = HashMap::new();
        for h in highlights {
            let mut p = self.page(h.page)?;
            // Manual: under the default every annotation added re-serialises the *page's* content
            // stream, which we never touch. The annotation's own appearance stream is pdfium's,
            // below.
            p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
            let existing = match known.entry(h.page) {
                Entry::Occupied(found) => found.into_mut(),
                Entry::Vacant(slot) => slot.insert(
                    highlights_of(h.page, &p)
                        .into_iter()
                        .map(|e| e.quads)
                        .collect(),
                ),
            };
            if existing.iter().any(|quads| same_quads(quads, &h.quads)) {
                continue;
            }
            existing.push(h.quads.clone());
            let height = p.height().value;
            let bounds = h
                .quads
                .iter()
                .copied()
                .reduce(Rect::union)
                .unwrap_or(Rect::ZERO);
            let mut a = p
                .annotations_mut()
                .create_highlight_annotation()
                .map_err(refused("create highlight annotation"))?;
            // The colour first, and never `fill_color`/`stroke_color` as *getters*: both setters
            // fall back to casting the annotation handle to a page-object handle once an
            // appearance stream exists, which is the crash `annotation_color` documents. On a
            // fresh annotation the safe branch is the one that runs.
            a.set_stroke_color(PdfColor::new(
                h.color[0], h.color[1], h.color[2], h.color[3],
            ))
            .map_err(refused("highlight colour"))?;
            // Before the quads: pdfium copies `/Rect` into the appearance stream's bounding box.
            a.set_bounds(bounds.to_pdf(height))
                .map_err(refused("highlight bounds"))?;
            for q in &h.quads {
                let [tl, tr, bl, br] = q.quad_corners();
                let y = |v: f32| height - v;
                a.attachment_points_mut()
                    .create_attachment_point_at_end(PdfQuadPoints::new_from_values(
                        tl.0,
                        y(tl.1),
                        tr.0,
                        y(tr.1),
                        bl.0,
                        y(bl.1),
                        br.0,
                        y(br.1),
                    ))
                    .map_err(refused("highlight quad"))?;
            }
            if let Some(text) = &h.contents {
                a.set_contents(text)
                    .map_err(refused("highlight contents"))?;
            }
            a.set_is_printed(true)
                .map_err(refused("highlight print flag"))?;
            added += 1;
            *ours.entry(h.page).or_default() += 1;
        }
        // pdfium gives a highlight its appearance stream only as it renders it: a render of each
        // page, as small as one can be, gives ours theirs now, so the file has it however soon it
        // is saved, and nothing else on the page keeps what the render wrote (`drawn`).
        for (&page, &ours) in &ours {
            let p = self.page(page)?;
            let dot = PdfRenderConfig::new()
                .set_target_size(1, 1)
                .render_annotations(true);
            self.drawn(&p, ours, || p.render_with_config(&dot))
                .map_err(refused("draw highlights"))?;
        }
        Ok(added)
    }

    /// The document as it now stands with `highlights` written in, as the bytes of a file of its
    /// own: what Export as PDF writes and Print prints. This document gains nothing — the
    /// highlights go into a second one read from its bytes.
    pub fn copy_with_highlights(&self, highlights: &[Highlight]) -> Result<Vec<u8>> {
        let bytes = self.save()?;
        if highlights.is_empty() {
            return Ok(bytes);
        }
        let mut copy = PdfDoc::from_bytes(bytes)?;
        copy.add_highlights(highlights)?;
        copy.save()
    }

    /// Draw one free-hand stroke onto a page as an `/Ink` annotation.
    ///
    /// `points` are in this module's top-left-origin page points, `width` is the stroke width in
    /// points, `rgb` its colour. `scale` is the screen pixels per point it was drawn at: the curve
    /// is fitted through the points a pixel apart there, so it stays within about a pixel of what
    /// the hand saw at any zoom.
    ///
    // ponytail: the geometry ends up in the annotation's *appearance stream* rather than in an
    // `/InkList` array, because `FPDFAnnot_AddInkStroke` is only reachable through the raw
    // bindings and pdfium-render keeps the annotation handle private. Every viewer renders the
    // appearance stream, so this draws correctly everywhere; what it costs is an editor that
    // wants to reshape the stroke, which would need `/InkList`. Raw bindings are the upgrade.
    /// The box it covers on the page comes back, so a viewer can re-render that much and no
    /// more.
    pub fn add_ink(
        &mut self,
        page: usize,
        points: &[(f32, f32)],
        style: InkStyle,
        scale: f32,
    ) -> Result<Rect> {
        let thinned = thin(points, 1.0 / scale);
        let Some(&first) = thinned.first() else {
            return Ok(Rect::ZERO);
        };
        let mut segs = vec![Seg::Move(first)];
        match thinned.len() {
            // A stroke that never moved: a zero-length segment, which the round cap draws as the
            // dot the reader meant.
            1 => segs.push(Seg::Line(first)),
            _ => segs.extend(
                catmull_rom(&thinned)
                    .into_iter()
                    .map(|[c1, c2, end]| Seg::Bezier(c1, c2, end)),
            ),
        }
        let _guard = lock();
        let mut p = self.page(page)?;
        self.put_ink(&mut p, &segs, style)
    }

    /// Draw a line, a rectangle or a circle as an `/Ink` annotation, exactly as the pen draws.
    ///
    // ponytail: an `/Ink` rather than a `/Line`, `/Square` or `/Circle`: pdfium-render has no
    // line annotation at all, no circle constructor, and only ink and stamp annotations take a
    // path object, so a real `/Square` would be stuck at pdfium's generated 1 pt border. Every
    // viewer renders the appearance stream; what it costs is another editor's shape palette
    // seeing a stroke.
    pub fn add_shape(&mut self, page: usize, shape: Shape, style: InkStyle) -> Result<Rect> {
        let _guard = lock();
        let mut p = self.page(page)?;
        self.put_ink(&mut p, &segments_of(shape), style)
    }

    /// The tail every ink writer shares: the path, then the annotation around it, and the box the
    /// two of them cover. The caller holds the lock, and `segs` starts with a `Move`.
    fn put_ink(&self, p: &mut PdfPage<'_>, segs: &[Seg], style: InkStyle) -> Result<Rect> {
        let InkStyle {
            width,
            rgba,
            multiply,
        } = style;
        let Some(&Seg::Move(first)) = segs.first() else {
            return Err(Error::Invalid("an ink path starts with a move".to_string()));
        };
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let height = p.height().value;
        let y = |v: f32| PdfPoints::new(height - v);
        let colour = PdfColor::new(rgba[0], rgba[1], rgba[2], rgba[3]);

        // Control points included: a Bézier stays inside its control polygon, so this box holds
        // the curve without evaluating it.
        let bounds = points_of(segs)
            .fold(Rect::from_corners(first, first), |r, p| {
                r.union(Rect::from_corners(p, p))
            })
            .grow(width / 2.0 + 1.0);

        let mut path = PdfPagePathObject::new(
            self.doc(),
            PdfPoints::new(first.0),
            y(first.1),
            Some(colour),
            Some(PdfPoints::new(width)),
            None,
        )
        .map_err(refused("ink path"))?;
        path.set_line_cap(PdfPageObjectLineCap::Round)
            .map_err(refused("ink line cap"))?;
        path.set_line_join(PdfPageObjectLineJoin::Round)
            .map_err(refused("ink line join"))?;
        if multiply {
            path.set_blend_mode(PdfPageObjectBlendMode::Multiply)
                .map_err(refused("ink blend mode"))?;
        }
        for seg in &segs[1..] {
            match *seg {
                Seg::Move((x, v)) => path.move_to(PdfPoints::new(x), y(v)),
                Seg::Line((x, v)) => path.line_to(PdfPoints::new(x), y(v)),
                Seg::Bezier(c1, c2, end) => path.bezier_to(
                    PdfPoints::new(end.0),
                    y(end.1),
                    PdfPoints::new(c1.0),
                    y(c1.1),
                    PdfPoints::new(c2.0),
                    y(c2.1),
                ),
                Seg::Close => path.close_path(),
            }
            .map_err(refused("ink segment"))?;
        }

        let mut ink = p
            .annotations_mut()
            .create_ink_annotation()
            .map_err(refused("create ink annotation"))?;
        ink.set_stroke_color(colour)
            .map_err(refused("ink colour"))?;
        // Before the object: pdfium copies `/Rect` into the appearance stream's bounding box, and
        // a box set afterwards would scale what was drawn into it.
        ink.set_bounds(bounds.to_pdf(height))
            .map_err(refused("ink bounds"))?;
        ink.objects_mut()
            .add_object(path.into())
            .map_err(refused("ink object"))?;
        ink.set_is_printed(true)
            .map_err(refused("ink print flag"))?;
        Ok(bounds)
    }

    /// How many annotations of every kind a page carries.
    pub fn annotation_count(&self, page: usize) -> Result<usize> {
        let _guard = lock();
        Ok(self.page(page)?.annotations().len())
    }

    /// Take one `/Ink` annotation off a page by its index in `/Annots` — which is all an erase is,
    /// and all its undo is — and say what box it left. One of ours is deleted, what it drew kept
    /// for [`PdfDoc::redraw_ink`]; another editor's is hidden where it is, for
    /// [`PdfDoc::show_ink`] to show again as it was (see [`Drawn`]). Any other kind of
    /// annotation is refused, so an index that has gone stale cannot take a link or a highlight.
    pub fn take_ink(&mut self, page: usize, index: usize) -> Result<(Drawn, Rect)> {
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        take(&mut p, page, index)
    }

    /// Draw one of our strokes [`PdfDoc::take_ink`] deleted again, at the end of the page's
    /// `/Annots`, and say what box it covers.
    pub fn redraw_ink(&mut self, page: usize, drawn: &Drawn) -> Result<Rect> {
        let _guard = lock();
        let mut p = self.page(page)?;
        self.put_ink(&mut p, &drawn.segs, drawn.style)
    }

    /// Show the stroke [`PdfDoc::take_ink`] hid at `index` again, as it was, and say what box it
    /// covers.
    pub fn show_ink(&mut self, page: usize, index: usize) -> Result<Rect> {
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let height = p.height().value;
        let mut a = annotation(&p, page, index)?;
        if a.annotation_type() != PdfPageAnnotationType::Ink {
            return Err(not_drawn(page, index));
        }
        a.set_is_hidden(false).map_err(refused("show ink"))?;
        Ok(Rect::from_pdf(
            a.bounds().map_err(refused("ink bounds"))?,
            height,
        ))
    }

    /// Delete the `/Ink` annotations at `indices` of a page for good, with the popups they have:
    /// what [`PdfDoc::take_ink`] hid for an Undo that will not come now.
    pub fn delete_inks(&mut self, page: usize, indices: &[usize]) -> Result<()> {
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let mut doomed = Vec::new();
        for &index in indices {
            if annotation(&p, page, index)?.annotation_type() != PdfPageAnnotationType::Ink {
                return Err(not_drawn(page, index));
            }
            doomed.extend([Some(index), popup_of(&p, index)].into_iter().flatten());
        }
        // Every place found before any goes, and the last first, so each still holds.
        doomed.sort_unstable();
        doomed.dedup();
        for index in doomed.into_iter().rev() {
            let a = (p.annotations_mut())
                .get(index as PdfPageAnnotationIndex)
                .map_err(|e| Error::Pdf(format!("annotation {index} of page {page}: {e:?}")))?;
            (p.annotations_mut())
                .delete_annotation(a)
                .map_err(refused("delete annotation"))?;
        }
        Ok(())
    }

    /// Every `/Ink` annotation on a page with the points of its drawn path, for the eraser to
    /// aim at. The index is the annotation's place in `/Annots`, which is what deletes it.
    pub fn ink_paths(&self, page: usize) -> Result<Vec<InkPath>> {
        Ok(self
            .inks(page)?
            .into_iter()
            .map(|ink| (ink.index, ink.points))
            .collect())
    }

    /// Return visible `/Ink` annotations with flattened strokes, bounds and style for the Adjust
    /// tool. Sampling curves makes hit testing follow the painted edge.
    ///
    /// Read external strokes from `/InkList` in page coordinates. Their appearance streams may
    /// use a separate transform that pdfium does not expose.
    ///
    // ponytail: one polyline through every stroke of the `/InkList`, so the gap between two
    // strokes answers to the pointer too, as `flatten` joins two sub-paths.
    pub fn inks(&self, page: usize) -> Result<Vec<InkShape>> {
        let _guard = lock();
        let p = self.page(page)?;
        Ok(p.annotations()
            .iter()
            .enumerate()
            .filter_map(|(index, a)| {
                let read = read(&p, &a, index)?;
                Some(InkShape {
                    index,
                    cuttable: read.cuttable,
                    points: match read.strokes.is_empty() {
                        true => flatten(&read.segs),
                        false => read.strokes.concat(),
                    },
                    bounds: read.bounds,
                    style: read.style,
                })
            })
            .collect())
    }

    /// Erase the painted stroke within `radius` of the path from `from` to `to`. Replace the
    /// original with straight `/Ink` segments at the end of `/Annots`, preserving its style.
    ///
    /// Return `None` without changes when the path misses or the stroke is not cuttable.
    ///
    // ponytail: the pieces are straight polylines at the flattening tolerance rather than curves
    // fitted again through `catmull_rom`, which is a spline *through* its points: fitted through
    // a rectangle's corners it bows each edge outwards by a few points, where straight lines
    // drawn from a curve flattened to `FLAT` stay on it. The cost is a longer path per piece.
    pub fn cut_ink(
        &mut self,
        page: usize,
        index: usize,
        from: (f32, f32),
        to: (f32, f32),
        radius: f32,
    ) -> Result<Option<Cut>> {
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let (strokes, reach) = {
            let read = read(&p, &annotation(&p, page, index)?, index)
                .ok_or_else(|| not_drawn(page, index))?;
            if !read.cuttable {
                return Ok(None);
            }
            let reach = radius + read.style.width / 2.0;
            match read.ours {
                true => (vec![flatten(&read.segs)], reach),
                false => (read.strokes, reach),
            }
        };
        // A stroke of an `/InkList` the path missed is left whole, as a piece of its own.
        let mut touched = false;
        let mut runs = Vec::new();
        for stroke in strokes {
            match cut(&stroke, from, to, reach) {
                Some(left) => {
                    touched = true;
                    runs.extend(left);
                }
                None => runs.push(stroke),
            }
        }
        if !touched {
            return Ok(None);
        }
        let (was, mut area) = take(&mut p, page, index)?;
        let mut left = Vec::with_capacity(runs.len());
        for run in runs {
            let piece = was.along(&run);
            area = area.union(self.put_ink(&mut p, &piece.segs, piece.style)?);
            left.push(piece);
        }
        Ok(Some(Cut { was, left, area }))
    }

    /// Transform one `/Ink` annotation, returning the union of old and new bounds and whether
    /// its `/Annots` position stayed the same.
    ///
    /// Our strokes are redrawn to preserve line width. External strokes keep their position and
    /// are resized through `/Rect`, preserving their appearance and `/InkList`.
    pub fn transform_ink(&mut self, page: usize, index: usize, m: Matrix) -> Result<(Rect, bool)> {
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let read =
            read(&p, &annotation(&p, page, index)?, index).ok_or_else(|| not_drawn(page, index))?;
        if read.ours {
            let (drawn, was) = take(&mut p, page, index)?;
            let now = self.put_ink(&mut p, &transformed(&drawn.segs, m), drawn.style)?;
            return Ok((was.union(now), false));
        }
        let was = read.bounds;
        let now = Rect::from_corners(
            apply(m, (was.left, was.top)),
            apply(m, (was.right, was.bottom)),
        );
        let strokes: Vec<Vec<(f32, f32)>> = (read.strokes.iter())
            .map(|stroke| stroke.iter().map(|&q| apply(m, q)).collect())
            .collect();
        place(&p, index, now, &strokes)?;
        Ok((was.union(now), true))
    }
}

/// One `/Ink` annotation as it was read off its page.
struct Read {
    /// What its appearance stream draws, every path of it, in the stream's own space; for ours
    /// that is the page's.
    segs: Vec<Seg>,
    /// How its first path is drawn, its width in page points.
    style: InkStyle,
    bounds: Rect,
    /// Its `/InkList`, in top-left page points: where it shows. Empty for ours.
    strokes: Vec<Vec<(f32, f32)>>,
    /// One of ours: one path of one stroke inside its `/Rect`, without an `/InkList`, a note or an
    /// author, which `segs` and `style` are all there is to, so it can be drawn again from them.
    /// Anything else is another editor's, and only its own dictionary draws it as it was.
    ours: bool,
    /// See [`InkShape::cuttable`]: one of ours, or another editor's with an `/InkList` to cut by,
    /// every path drawn alike.
    cuttable: bool,
}

/// The annotation `a`, at `index` of a loaded page, as [`Read`]; `None` for a hidden one, any
/// other kind of annotation, or one without a path. The caller holds the lock.
fn read(p: &PdfPage<'_>, a: &PdfPageAnnotation<'_>, index: usize) -> Option<Read> {
    if a.is_hidden() {
        return None;
    }
    let height = p.height().value;
    let (segs, mut style) = read_ink(a, height)?;
    let bounds = Rect::from_pdf(a.bounds().ok()?, height);
    let strokes = ink_list(p, index, height);
    let points = flatten(&segs);
    let inside = bounds.grow(1.0);
    let ours = strokes.is_empty()
        && a.contents().is_none()
        && a.creator().is_none()
        && a.objects().len() == 1
        && segs.iter().skip(1).all(|s| !matches!(s, Seg::Move(_)))
        && points.iter().all(|&q| inside.contains(q));
    if !strokes.is_empty() {
        style.width *= scale(&points, &strokes);
    }
    let cuttable = ours || (!strokes.is_empty() && one_style(a));
    Some(Read {
        segs,
        style,
        bounds,
        strokes,
        ours,
        cuttable,
    })
}

/// How many page points a unit of an appearance stream's own space covers: the extent of its
/// `/InkList`, in page points, over the extent of what the stream draws, the same strokes. 1 for
/// a stroke drawn in the page's own space; other once Adjust has resized it.
fn scale(drawn: &[(f32, f32)], strokes: &[Vec<(f32, f32)>]) -> f32 {
    let reach = |points: &mut dyn Iterator<Item = &(f32, f32)>| {
        (points.map(|&q| Rect::from_corners(q, q)))
            .reduce(Rect::union)
            .map_or(0.0, |r| r.width().hypot(r.height()))
    };
    let (drawn, listed) = (
        reach(&mut drawn.iter()),
        reach(&mut strokes.iter().flatten()),
    );
    match drawn > 0.01 && listed > 0.01 {
        true => listed / drawn,
        false => 1.0,
    }
}

/// Whether every path an annotation's appearance stream draws is drawn alike, so a piece cut from
/// it can be drawn as it was.
fn one_style(a: &PdfPageAnnotation<'_>) -> bool {
    let mut styles = (a.objects().iter()).map(|o| o.as_path_object().and_then(style_of));
    let first = styles.next().flatten();
    first.is_some() && styles.all(|s| s == first)
}

/// The annotation at `index` of a loaded page.
fn annotation<'a>(p: &'a PdfPage<'_>, page: usize, index: usize) -> Result<PdfPageAnnotation<'a>> {
    (p.annotations())
        .get(index as PdfPageAnnotationIndex)
        .map_err(|e| Error::Pdf(format!("annotation {index} of page {page}: {e:?}")))
}

/// The refusal of an annotation that is no ink stroke to take or move.
fn not_drawn(page: usize, index: usize) -> Error {
    Error::Pdf(format!(
        "annotation {index} of page {page} is not a drawn path"
    ))
}

/// The strokes of the `/InkList` of the annotation at `index` of a loaded page, in top-left page
/// points; none for an annotation that has none, as ours do not. The caller holds the lock.
///
/// Raw calls, because pdfium-render wraps neither `/InkList` nor the annotation handle.
pub(super) fn ink_list(p: &PdfPage<'_>, index: usize, height: f32) -> Vec<Vec<(f32, f32)>> {
    let bindings = p.bindings();
    // SAFETY: the page handle is `p`'s own and alive while it is borrowed; the lock is held, so no
    // other thread is inside pdfium; the annotation handle is only read, and closed before
    // returning; each buffer is as long as the point count passed with it.
    unsafe {
        let annot = bindings.FPDFPage_GetAnnot(bindings.get_handle_from_page(p), index as c_int);
        if annot.is_null() {
            return Vec::new();
        }
        let strokes = (0..bindings.FPDFAnnot_GetInkListCount(annot))
            .map(|stroke| {
                let n = bindings.FPDFAnnot_GetInkListPath(annot, stroke, std::ptr::null_mut(), 0);
                let mut points = vec![FS_POINTF { x: 0.0, y: 0.0 }; n as usize];
                bindings.FPDFAnnot_GetInkListPath(annot, stroke, points.as_mut_ptr(), n as c_ulong);
                points.iter().map(|q| (q.x, height - q.y)).collect()
            })
            .collect();
        bindings.FPDFPage_CloseAnnot(annot);
        strokes
    }
}

/// The key pdfium sets beside an appearance stream it generated.
const GENERATED: &str = "PDFIUM_HasGeneratedAP";

/// What [`unbare`] sets `GENERATED` to where pdfium added it, there being no call that takes a
/// key out, and what [`blank_taken_back`] then finds in the saved file.
const TAKEN_BACK: &str = "accent";

/// pdfium's `FPDF_ANNOT_APPEARANCEMODE_NORMAL` and `FPDF_OBJECT_BOOLEAN`, which pdfium-render
/// does not export.
const NORMAL: FPDF_ANNOT_APPEARANCEMODE = 0;
const BOOLEAN: FPDF_OBJECT_TYPE = 1;

/// The annotations of a loaded page without an appearance stream, which pdfium generates into
/// their dictionaries as it renders them: the place of each, and whether `GENERATED` is already
/// there as the file's own. The caller holds the lock.
pub(super) fn bare(p: &PdfPage<'_>) -> Vec<(usize, bool)> {
    let bindings = p.bindings();
    // SAFETY: as in `ink_list`; each annotation handle is only read, and closed before the next.
    unsafe {
        let page = bindings.get_handle_from_page(p);
        (0..bindings.FPDFPage_GetAnnotCount(page))
            .filter_map(|index| {
                let annot = bindings.FPDFPage_GetAnnot(page, index);
                if annot.is_null() {
                    return None;
                }
                let bare = (!bindings.is_true(bindings.FPDFAnnot_HasKey(annot, "AP"))).then(|| {
                    let own = bindings.FPDFAnnot_GetValueType(annot, GENERATED);
                    (index as usize, own == BOOLEAN)
                });
                bindings.FPDFPage_CloseAnnot(annot);
                bare
            })
            .collect()
    }
}

/// Take back from the annotations [`bare`] found what pdfium generated into them as it rendered
/// them: the appearance stream, which goes, and `GENERATED`, which becomes `TAKEN_BACK` unless it
/// was the file's own. Whether there was any. The caller holds the lock.
///
/// Raw calls, because pdfium-render can neither take an appearance stream away nor say whether
/// an annotation has one.
pub(super) fn unbare(p: &PdfPage<'_>, bare: &[(usize, bool)]) -> bool {
    let bindings = p.bindings();
    let mut generated = false;
    // SAFETY: as in `ink_list`; each annotation handle is closed before the next, and a null
    // appearance is pdfium's documented way of taking one away.
    unsafe {
        let page = bindings.get_handle_from_page(p);
        for &(index, own) in bare {
            let annot = bindings.FPDFPage_GetAnnot(page, index as c_int);
            if annot.is_null() {
                continue;
            }
            if bindings.is_true(bindings.FPDFAnnot_HasKey(annot, "AP")) {
                generated = true;
                bindings.FPDFAnnot_SetAP(annot, NORMAL, std::ptr::null());
                if !own {
                    bindings.FPDFAnnot_SetStringValue_str(annot, GENERATED, TAKEN_BACK);
                }
            }
            bindings.FPDFPage_CloseAnnot(annot);
        }
    }
    generated
}

/// Blank out every `GENERATED` key [`unbare`] made `TAKEN_BACK` in a file pdfium just saved,
/// keeping its length so every offset in it still holds: read back, the key is not there.
pub(super) fn blank_taken_back(file: &mut [u8]) {
    let key = format!("/{GENERATED}({TAKEN_BACK})");
    let found: Vec<usize> = memchr::memmem::find_iter(file, key.as_bytes()).collect();
    for at in found {
        file[at..at + key.len()].fill(b' ');
    }
}

/// Where in `/Annots` the popup of the annotation at `index` of a loaded page sits, if it has
/// one there. The caller holds the lock.
///
/// Raw calls, because pdfium-render wraps neither `/Popup` nor the annotation handle.
fn popup_of(p: &PdfPage<'_>, index: usize) -> Option<usize> {
    let bindings = p.bindings();
    // SAFETY: as in `ink_list`; both annotation handles are only read, and closed before
    // returning.
    unsafe {
        let page = bindings.get_handle_from_page(p);
        let annot = bindings.FPDFPage_GetAnnot(page, index as c_int);
        if annot.is_null() {
            return None;
        }
        let popup = bindings.FPDFAnnot_GetLinkedAnnot(annot, "Popup");
        let at = match popup.is_null() {
            true => -1,
            false => {
                let at = bindings.FPDFPage_GetAnnotIndex(page, popup);
                bindings.FPDFPage_CloseAnnot(popup);
                at
            }
        };
        bindings.FPDFPage_CloseAnnot(annot);
        usize::try_from(at).ok()
    }
}

/// Put the annotation at `index` of a loaded page where `rect` and `strokes` say, in top-left
/// page points, and change nothing else of it: its `/Rect`, which every reader fits its
/// appearance stream into, and its `/InkList`, unless it has none. The caller holds the lock.
///
/// Raw calls, because pdfium-render wraps no `/InkList`, and its `set_bounds` stamps `/M` too.
fn place(p: &PdfPage<'_>, index: usize, rect: Rect, strokes: &[Vec<(f32, f32)>]) -> Result<()> {
    let height = p.height().value;
    let bindings = p.bindings();
    // SAFETY: as in `ink_list`; each buffer outlives its call, and its length is what is passed.
    unsafe {
        let annot = bindings.FPDFPage_GetAnnot(bindings.get_handle_from_page(p), index as c_int);
        if annot.is_null() {
            return Err(Error::Pdf(format!("no annotation {index} to move")));
        }
        let r = rect.to_pdf(height);
        let r = FS_RECTF {
            left: r.left().value,
            top: r.top().value,
            right: r.right().value,
            bottom: r.bottom().value,
        };
        let placed = bindings.is_true(bindings.FPDFAnnot_SetRect(annot, &r))
            && (strokes.is_empty() || bindings.is_true(bindings.FPDFAnnot_RemoveInkList(annot)))
            && strokes.iter().all(|stroke| {
                let points: Vec<FS_POINTF> = (stroke.iter())
                    .map(|&(x, y)| FS_POINTF { x, y: height - y })
                    .collect();
                bindings.FPDFAnnot_AddInkStroke(annot, points.as_ptr(), points.len() as _) >= 0
            });
        bindings.FPDFPage_CloseAnnot(annot);
        match placed {
            true => Ok(()),
            false => Err(Error::Pdf(format!(
                "pdfium would not move annotation {index}"
            ))),
        }
    }
}

/// Take the `/Ink` annotation at `index` off a loaded page, as [`PdfDoc::take_ink`] does, and
/// say what box it left. The caller holds the lock.
fn take(p: &mut PdfPage<'_>, page: usize, index: usize) -> Result<(Drawn, Rect)> {
    let (drawn, was) = {
        let read =
            read(p, &annotation(p, page, index)?, index).ok_or_else(|| not_drawn(page, index))?;
        let drawn = Drawn {
            segs: read.segs,
            style: read.style,
            hidden: !read.ours,
        };
        (drawn, read.bounds)
    };
    // Through `annotations_mut` rather than `annotations`: the annotation has to carry the
    // document's lifetime for `delete_annotation` to take it, and the shared accessor hands back
    // one borrowed from `p` instead.
    let mut a = p
        .annotations_mut()
        .get(index as PdfPageAnnotationIndex)
        .map_err(|e| Error::Pdf(format!("annotation {index} of page {page}: {e:?}")))?;
    match drawn.hidden {
        true => a.set_is_hidden(true).map_err(refused("hide ink"))?,
        false => (p.annotations_mut())
            .delete_annotation(a)
            .map_err(refused("delete annotation"))?,
    }
    Ok((drawn, was))
}

/// Read annotation colour without pdfium-render's unsafe appearance-stream fallback.
///
/// On annotations with an appearance stream, `fill_color()` and `stroke_color()` fall back from
/// `FPDFAnnot_GetColor()` by casting an annotation handle to an unrelated page-object handle,
/// causing undefined behaviour and crashes. Rendering can generate the stream, so subsequent
/// reads trigger it. Reproduced with pdfium-render 0.9.3 and pdfium 7881 and 8035 (ISSUES.md).
///
/// Read the actual path object's fill colour when a stream exists; otherwise
/// `FPDFAnnot_GetColor()` safely reads `/C`.
// ponytail: the one uncovered case is an annotation whose `/AP` exists but contains no page
// objects — then `stroke_color()` still takes the crashing branch. Never seen in the wild, and
// the real fix is upstream (or our own `FPDFAnnot_GetColor` call, which needs the raw
// `FPDF_ANNOTATION` handle that pdfium-render keeps private).
/// The `/Highlight` annotations of one loaded page. Shared by the whole-document read, the
/// per-page one and the export's own "is this already here" check, so the three cannot drift.
fn highlights_of(page: usize, p: &PdfPage<'_>) -> Vec<Highlight> {
    let page_height = p.height().value;
    let mut out = Vec::new();
    for a in p.annotations().iter() {
        if a.annotation_type() != PdfPageAnnotationType::Highlight {
            continue;
        }
        let c = annotation_color(&a).unwrap_or(PdfColor::new(255, 255, 0, 255));
        out.push(Highlight {
            page,
            quads: areas(&a, page_height),
            color: [c.red(), c.green(), c.blue(), c.alpha()],
            contents: a.contents(),
        });
    }
    out
}

/// Where an annotation sits: its `/QuadPoints`, or its `/Rect` for one without — not every
/// producer writes quads, and most kinds of annotation have none.
fn areas(a: &PdfPageAnnotation<'_>, page_height: f32) -> Vec<Rect> {
    let mut quads: Vec<Rect> = a
        .attachment_points()
        .iter()
        .map(|q| Rect::from_pdf(q.to_rect(), page_height))
        .collect();
    if quads.is_empty()
        && let Ok(b) = a.bounds()
    {
        quads.push(Rect::from_pdf(b, page_height));
    }
    quads
}

fn annotation_color(a: &PdfPageAnnotation<'_>) -> Option<PdfColor> {
    if a.objects().len() > 0 {
        return a.objects().iter().find_map(|o| o.fill_color().ok());
    }
    a.stroke_color().ok()
}

/// An `/Ink` annotation's paths and the style of the first, read back out of its appearance
/// stream; `None` for any other annotation, or one without a path. pdfium reports a cubic as
/// three consecutive Bézier points — two controls, then the end.
fn read_ink(a: &PdfPageAnnotation<'_>, height: f32) -> Option<(Vec<Seg>, InkStyle)> {
    if a.annotation_type() != PdfPageAnnotationType::Ink {
        return None;
    }
    let mut segs = Vec::new();
    let mut style = None;
    for o in a.objects().iter() {
        let Some(path) = o.as_path_object() else {
            continue;
        };
        let Some(drawn) = style_of(path) else {
            continue;
        };
        style = style.or(Some(drawn));
        let mut controls = Vec::new();
        for s in path.segments().iter() {
            let p = (s.x().value, height - s.y().value);
            match s.segment_type() {
                PdfPathSegmentType::MoveTo => segs.push(Seg::Move(p)),
                PdfPathSegmentType::LineTo => segs.push(Seg::Line(p)),
                PdfPathSegmentType::BezierTo => {
                    controls.push(p);
                    if let [c1, c2, end] = controls[..] {
                        segs.push(Seg::Bezier(c1, c2, end));
                        controls.clear();
                    }
                }
                PdfPathSegmentType::Unknown => {}
            }
            if s.is_close() {
                segs.push(Seg::Close);
            }
        }
    }
    Some((segs, style?))
}

/// How a path is drawn. The page-object getters, never the annotation's own: those cast the
/// handle into a page object once an appearance stream exists (see `annotation_color`).
fn style_of(path: &PdfPagePathObject<'_>) -> Option<InkStyle> {
    let c = path.stroke_color().ok()?;
    Some(InkStyle {
        width: path.stroke_width().ok()?.value,
        rgba: [c.red(), c.green(), c.blue(), c.alpha()],
        // ponytail: pdfium-render has no blend-mode getter, and a translucent stroke of ours is
        // the highlighter, so alpha stands in for `/Multiply`.
        multiply: c.alpha() < 255,
    })
}
