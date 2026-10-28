//! Highlight and ink annotations, with safe appearance-stream colour handling.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::{Error, Result};
use accent_pdf::{Content, Dict, Object, form};
use pdfium_render::prelude::*;

use super::cos::{Objects, dict, name, number, real, reals, text};
use super::ink::{
    Drawn, Seg, apply, catmull_rom, cut, flatten, points_of, polyline, segments_of, thin,
    transformed,
};
use super::text::{line_top, same_quads, selection_quads};
use super::{
    Comment, Cut, Glyph, Highlight, InkShape, InkStyle, Matrix, PdfDoc, Rect, Shape, lock,
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

    /// Write `/Highlight` annotations into the document, and say how many were written.
    ///
    /// A highlight whose quads a highlight on that page already covers is skipped, so exporting
    /// the same note links twice adds nothing and the painted overlay can step aside for the
    /// annotation as soon as it is in the file.
    ///
    /// Nothing reaches the disk here: [`PdfDoc::save_to`] is the second half.
    pub fn add_highlights(&mut self, highlights: &[Highlight]) -> Result<usize> {
        let new = self.new_highlights(highlights)?;
        for (h, height) in &new {
            self.cos.put_highlight(h, *height)?;
        }
        if !new.is_empty() {
            self.reopen()?;
        }
        Ok(new.len())
    }

    /// The document as it now stands with `highlights` written in, as the bytes of a file of its
    /// own: what Export as PDF writes and Print prints. This document gains nothing — the
    /// highlights go into a copy of the session's update.
    pub fn copy_with_highlights(&self, highlights: &[Highlight]) -> Result<Vec<u8>> {
        let new = self.new_highlights(highlights)?;
        if new.is_empty() {
            return self.save();
        }
        let mut copy = self.cos.clone();
        for (h, height) in &new {
            copy.put_highlight(h, *height)?;
        }
        let mut bytes = self.base_bytes()?;
        bytes.extend(copy.update.section());
        Ok(bytes)
    }

    /// Those of `highlights` no highlight on their page already covers, nor one before them, each
    /// with its page's height.
    fn new_highlights<'h>(&self, highlights: &'h [Highlight]) -> Result<Vec<(&'h Highlight, f32)>> {
        let _guard = lock();
        // What each page already carries, read once for that page and kept up to date as the
        // list grows. Asking the page again per highlight walked its `/Annots` array for every one
        // of them, which on a page with many is quadratic.
        let mut known: HashMap<usize, (Vec<Vec<Rect>>, f32)> = HashMap::new();
        let mut new = Vec::new();
        for h in highlights {
            let (existing, height) = match known.entry(h.page) {
                Entry::Occupied(found) => found.into_mut(),
                Entry::Vacant(slot) => {
                    let p = self.page(h.page)?;
                    let quads = highlights_of(h.page, &p).into_iter().map(|e| e.quads);
                    slot.insert((quads.collect(), p.height().value))
                }
            };
            if existing.iter().any(|quads| same_quads(quads, &h.quads)) {
                continue;
            }
            existing.push(h.quads.clone());
            new.push((h, *height));
        }
        Ok(new)
    }

    /// Draw one free-hand stroke onto a page as an `/Ink` annotation.
    ///
    /// `points` are in this module's top-left-origin page points, `width` is the stroke width in
    /// points, `rgb` its colour. `scale` is the screen pixels per point it was drawn at: the curve
    /// is fitted through the points a pixel apart there, so it stays within about a pixel of what
    /// the hand saw at any zoom.
    ///
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
        self.drawn(page, &segs, style)
    }

    /// Draw a line, a rectangle or a circle as an `/Ink` annotation, exactly as the pen draws.
    ///
    // ponytail: an `/Ink` rather than a `/Line`, `/Square` or `/Circle`: the eraser, Adjust and
    // every read here know ink alone. Every viewer renders the appearance stream; what it costs is
    // another editor's shape palette seeing a stroke.
    pub fn add_shape(&mut self, page: usize, shape: Shape, style: InkStyle) -> Result<Rect> {
        self.drawn(page, &segments_of(shape), style)
    }

    /// `segs` drawn onto `page` as a stroke of ours, the document opened again, and the box it
    /// covers.
    fn drawn(&mut self, page: usize, segs: &[Seg], style: InkStyle) -> Result<Rect> {
        let (_, height) = self.page_size(page)?;
        let area = self.cos.put_ink(page, height, segs, style)?;
        self.reopen()?;
        Ok(area)
    }

    /// How many annotations of every kind a page carries.
    pub fn annotation_count(&self, page: usize) -> Result<usize> {
        let _guard = lock();
        Ok(self.page(page)?.annotations().len())
    }

    /// Take one `/Ink` annotation off a page by its index in `/Annots` — which is all an erase is,
    /// and all its undo is — and say what box it left. One of ours goes, what it drew kept for
    /// [`PdfDoc::redraw_ink`]; another editor's goes with its popup, both kept as they were for
    /// the same to put back (see [`Drawn`]). Any other kind of annotation is refused, so an index
    /// that has gone stale cannot take a link or a highlight.
    pub fn take_ink(&mut self, page: usize, index: usize) -> Result<(Drawn, Rect)> {
        let taken = self.take(page, index)?;
        self.reopen()?;
        Ok(taken)
    }

    /// Put back a stroke [`PdfDoc::take_ink`] took, and say what box it covers: one of ours drawn
    /// again at the end of the page's `/Annots`, another editor's where it was.
    pub fn redraw_ink(&mut self, page: usize, drawn: &Drawn) -> Result<Rect> {
        match drawn {
            Drawn::Ours { segs, style } => self.drawn(page, segs, *style),
            Drawn::Foreign { entries, area, .. } => {
                let mut list = self.cos.annots(page)?;
                for (at, entry) in entries {
                    list.insert((*at).min(list.len()), entry.clone());
                }
                self.cos.set_annots(page, list)?;
                self.reopen()?;
                Ok(*area)
            }
        }
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
        let annots = self.cos.annots(page).unwrap_or_default();
        Ok(p.annotations()
            .iter()
            .enumerate()
            .filter_map(|(index, a)| {
                let read = self.read(&p, &a, annots.get(index))?;
                Some(InkShape {
                    index,
                    cuttable: read.cuttable,
                    points: match read.ours || read.strokes.is_empty() {
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
        let read = self.read_at(page, index)?;
        if !read.cuttable {
            return Ok(None);
        }
        let reach = radius + read.style.width / 2.0;
        let strokes = match read.ours {
            true => vec![flatten(&read.segs)],
            false => read.strokes,
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
        let (was, mut area) = self.take(page, index)?;
        let (_, height) = self.page_size(page)?;
        let mut left = Vec::with_capacity(runs.len());
        for run in runs {
            let (segs, style) = (polyline(&run), read.style);
            area = area.union(self.cos.put_ink(page, height, &segs, style)?);
            left.push(Drawn::Ours { segs, style });
        }
        self.reopen()?;
        Ok(Some(Cut { was, left, area }))
    }

    /// Transform one `/Ink` annotation, returning the union of old and new bounds and whether
    /// its `/Annots` position stayed the same.
    ///
    /// Our strokes are redrawn to preserve line width. External strokes keep their position and
    /// are resized through `/Rect`, preserving their appearance and `/InkList`.
    pub fn transform_ink(&mut self, page: usize, index: usize, m: Matrix) -> Result<(Rect, bool)> {
        let read = self.read_at(page, index)?;
        if read.ours {
            let (_, was) = self.take(page, index)?;
            let now = self.drawn(page, &transformed(&read.segs, m), read.style)?;
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
        let (_, height) = self.page_size(page)?;
        self.cos.place(page, index, height, now, &strokes)?;
        self.reopen()?;
        Ok((was.union(now), true))
    }

    /// The `/Ink` annotation at `index` of a page, as [`Read`]; refused for anything else.
    fn read_at(&self, page: usize, index: usize) -> Result<Read> {
        let _guard = lock();
        let p = self.page(page)?;
        let annots = self.cos.annots(page)?;
        (self.read(&p, &annotation(&p, page, index)?, annots.get(index)))
            .ok_or_else(|| not_drawn(page, index))
    }

    /// Take the `/Ink` annotation at `index` off a page, as [`PdfDoc::take_ink`] does, without
    /// opening the document again, and say what box it left.
    fn take(&mut self, page: usize, index: usize) -> Result<(Drawn, Rect)> {
        let read = self.read_at(page, index)?;
        let mut entries = self.cos.annots(page)?;
        if index >= entries.len() {
            return Err(not_drawn(page, index));
        }
        let drawn = match read.ours {
            true => {
                entries.remove(index);
                Drawn::Ours {
                    segs: read.segs,
                    style: read.style,
                }
            }
            false => {
                let popup = (read.popup.map(Object::Ref))
                    .and_then(|popup| entries.iter().position(|e| *e == popup))
                    .filter(|&at| at != index);
                let mut places: Vec<usize> = [Some(index), popup].into_iter().flatten().collect();
                places.sort_unstable();
                // The last first, so each place still holds; put back in the order they had.
                let mut taken: Vec<(usize, Object)> = (places.iter().rev())
                    .map(|&at| (at, entries.remove(at)))
                    .collect();
                taken.reverse();
                Drawn::Foreign {
                    index,
                    entries: taken,
                    area: read.bounds,
                }
            }
        };
        self.cos.set_annots(page, entries)?;
        Ok((drawn, read.bounds))
    }

    /// The annotation `a`, at `entry` of its page's `/Annots`, as [`Read`]; `None` for a hidden
    /// one, any other kind of annotation, or one without a path. The caller holds the lock.
    fn read(
        &self,
        p: &PdfPage<'_>,
        a: &PdfPageAnnotation<'_>,
        entry: Option<&Object>,
    ) -> Option<Read> {
        if a.is_hidden() {
            return None;
        }
        let height = p.height().value;
        let (segs, mut style) = read_ink(a, height)?;
        let bounds = Rect::from_pdf(a.bounds().ok()?, height);
        let dict = (entry.and_then(|e| self.cos.resolve(e).ok()))
            .and_then(|o| o.as_dict().cloned())
            .unwrap_or_default();
        let strokes = self.ink_list(&dict, height);
        let named = matches!(dict.get(b"NM"), Some(Object::String(nm)) if nm.starts_with(OURS));
        let points = flatten(&segs);
        let inside = bounds.grow(1.0);
        // Named as ours, or one of ours from before they were named: one path of one stroke
        // inside its `/Rect`, without an `/InkList`, a note or an author.
        let ours = named
            || (strokes.is_empty()
                && a.contents().is_none()
                && a.creator().is_none()
                && a.objects().len() == 1
                && segs.iter().skip(1).all(|s| !matches!(s, Seg::Move(_)))
                && points.iter().all(|&q| inside.contains(q)));
        if !ours && !strokes.is_empty() {
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
            popup: dict.get(b"Popup").and_then(Object::reference),
        })
    }

    /// The strokes of an annotation's `/InkList`, in top-left page points; none for one that has
    /// none.
    fn ink_list(&self, annot: &Dict, height: f32) -> Vec<Vec<(f32, f32)>> {
        let resolved = |o: &Object| self.cos.resolve(o).ok();
        let Some(Object::Array(strokes)) = annot.get(b"InkList").and_then(resolved) else {
            return Vec::new();
        };
        (strokes.iter().filter_map(resolved))
            .filter_map(|stroke| {
                let values: Vec<f32> = (stroke.as_array()?.iter())
                    .filter_map(|v| number(&resolved(v)?))
                    .collect();
                let points: Vec<(f32, f32)> = (values.as_chunks::<2>().0.iter())
                    .map(|&[x, y]| (x, height - y))
                    .collect();
                (!points.is_empty()).then_some(points)
            })
            .collect()
    }
}

/// What every stroke of ours is named by (`/NM`), followed by its object number: unique on its
/// page, as the name must be, and ours to recognise.
const OURS: &[u8] = b"accent-";

impl Objects {
    /// One stroke of ours, `segs` in top-left page points on a page `height` points tall, at the
    /// end of the page's `/Annots`, and the box it covers.
    ///
    /// The appearance stream draws in the page's own space, its `/BBox` being the `/Rect`, so a
    /// reader fits it where it was drawn; the `/InkList`, the flattened path, is what another
    /// editor reshapes it by.
    pub(super) fn put_ink(
        &mut self,
        page: usize,
        height: f32,
        segs: &[Seg],
        style: InkStyle,
    ) -> Result<Rect> {
        let InkStyle {
            width,
            rgba,
            multiply,
        } = style;
        let Some(&Seg::Move(first)) = segs.first() else {
            return Err(Error::Invalid("an ink path starts with a move".to_string()));
        };
        let up = |(x, y): (f32, f32)| (x, height - y);
        // Control points included: a Bézier stays inside its control polygon, so this box holds
        // the curve without evaluating it.
        let bounds = points_of(segs)
            .fold(Rect::from_corners(first, first), |r, p| {
                r.union(Rect::from_corners(p, p))
            })
            .grow(width / 2.0 + 1.0);
        let rect = [
            bounds.left,
            height - bounds.bottom,
            bounds.right,
            height - bounds.top,
        ];
        let [r, g, b, alpha] = rgba.map(|c| f32::from(c) / 255.0);

        let mut drawing = Content::default();
        drawing.save();
        let shaded = alpha < 1.0 || multiply;
        if shaded {
            drawing.state(b"GS");
        }
        drawing.stroke_rgb([r, g, b]).width(width).cap(1).join(1);
        for seg in segs {
            match *seg {
                Seg::Move(p) => drawing.move_to(up(p)),
                Seg::Line(p) => drawing.line_to(up(p)),
                Seg::Bezier(c1, c2, end) => drawing.curve_to(up(c1), up(c2), up(end)),
                Seg::Close => drawing.close(),
            };
        }
        drawing.stroke().restore();
        let mut state = dict([(b"Type", name(b"ExtGState")), (b"CA", real(alpha))]);
        if multiply {
            state.set(b"BM", name(b"Multiply"));
        }
        let resources = match shaded {
            true => dict([(
                b"ExtGState",
                Object::Dict(dict([(b"GS", Object::Dict(state))])),
            )]),
            false => Dict::default(),
        };
        let appearance = form(rect, drawing.finish(), resources);
        let appearance = self.update.add(Object::Stream(appearance));

        let ink_list = (segs.chunk_by(|_, next| !matches!(next, Seg::Move(_))))
            .map(|path| {
                reals(
                    flatten(path)
                        .into_iter()
                        .flat_map(|q| <[f32; 2]>::from(up(q))),
                )
            })
            .collect();
        let annot = dict([
            (b"Type", name(b"Annot")),
            (b"Subtype", name(b"Ink")),
            (b"Rect", reals(rect)),
            (b"InkList", Object::Array(ink_list)),
            (b"C", reals([r, g, b])),
            (b"CA", real(alpha)),
            (b"BS", Object::Dict(dict([(b"W", real(width))]))),
            (b"F", Object::Int(PRINT)),
            (b"AP", Object::Dict(dict([(b"N", Object::Ref(appearance))]))),
        ]);
        let at = self.append_annot(page, annot)?;
        let mut named = self.dict(at)?;
        let mut nm = OURS.to_vec();
        nm.extend(at.num.to_string().bytes());
        named.set(b"NM", Object::String(nm));
        self.update.set(at, Object::Dict(named));
        Ok(bounds)
    }

    /// One `/Highlight` over `h.quads` on a page `height` points tall, at the end of its page's
    /// `/Annots`, its quote its `/Contents`.
    ///
    /// Its appearance stream is the one pdfium generates for a highlight without one: each quad
    /// filled in `/C`, multiplied onto the page at `/CA`, so it reads as it did when pdfium drew it.
    pub(super) fn put_highlight(&mut self, h: &Highlight, height: f32) -> Result<()> {
        let bounds = h
            .quads
            .iter()
            .copied()
            .reduce(Rect::union)
            .unwrap_or(Rect::ZERO);
        let rect = [
            bounds.left,
            height - bounds.bottom,
            bounds.right,
            height - bounds.top,
        ];
        let [r, g, b, alpha] = h.color.map(|c| f32::from(c) / 255.0);
        let mut fill = Content::default();
        fill.state(b"GS").fill_rgb([r, g, b]);
        let mut corners = Vec::with_capacity(h.quads.len() * 8);
        for q in &h.quads {
            let (top, bottom) = (height - q.top, height - q.bottom);
            fill.move_to((q.left, top))
                .line_to((q.right, top))
                .line_to((q.right, bottom))
                .line_to((q.left, bottom))
                .close()
                .fill();
            corners.extend(
                q.quad_corners()
                    .into_iter()
                    .flat_map(|(x, y)| [x, height - y]),
            );
        }
        let state = dict([
            (b"Type", name(b"ExtGState")),
            (b"CA", real(alpha)),
            (b"ca", real(alpha)),
            (b"AIS", Object::Bool(false)),
            (b"BM", name(b"Multiply")),
        ]);
        let resources = dict([(
            b"ExtGState",
            Object::Dict(dict([(b"GS", Object::Dict(state))])),
        )]);
        let appearance = self
            .update
            .add(Object::Stream(form(rect, fill.finish(), resources)));
        let mut annot = dict([
            (b"Type", name(b"Annot")),
            (b"Subtype", name(b"Highlight")),
            (b"Rect", reals(rect)),
            (b"QuadPoints", reals(corners)),
            (b"C", reals([r, g, b])),
            (b"CA", real(alpha)),
            (b"F", Object::Int(PRINT)),
            (b"AP", Object::Dict(dict([(b"N", Object::Ref(appearance))]))),
        ]);
        if let Some(quote) = &h.contents {
            annot.set(b"Contents", text(quote));
        }
        self.append_annot(h.page, annot).map(drop)
    }

    /// The annotation at `index` of page `page` moved where `rect` and `strokes` say, in top-left
    /// page points on a page `height` points tall, and nothing else of it changed: its `/Rect`,
    /// which every reader fits its appearance stream into, and its `/InkList`, unless it has none.
    pub(super) fn place(
        &mut self,
        page: usize,
        index: usize,
        height: f32,
        rect: Rect,
        strokes: &[Vec<(f32, f32)>],
    ) -> Result<()> {
        let Some(Object::Ref(at)) = self.annots(page)?.get(index).cloned() else {
            return Err(Error::Pdf(format!(
                "annotation {index} of page {page} is no object"
            )));
        };
        let mut annot = self.dict(at)?;
        let edges = [
            rect.left,
            height - rect.bottom,
            rect.right,
            height - rect.top,
        ];
        annot.set(b"Rect", reals(edges));
        if !strokes.is_empty() {
            let up = |&(x, y): &(f32, f32)| [x, height - y];
            let list = strokes
                .iter()
                .map(|s| reals(s.iter().flat_map(up)))
                .collect();
            annot.set(b"InkList", Object::Array(list));
        }
        self.update.set(at, Object::Dict(annot));
        Ok(())
    }
}

/// `/F 4`: printed, as a mark on the page should be.
const PRINT: i64 = 4;

/// One `/Ink` annotation as it was read off its page.
struct Read {
    /// What its appearance stream draws, every path of it, in the stream's own space; for ours
    /// that is the page's.
    segs: Vec<Seg>,
    /// How its first path is drawn, its width in page points.
    style: InkStyle,
    bounds: Rect,
    /// Its `/InkList`, in top-left page points: where it shows.
    strokes: Vec<Vec<(f32, f32)>>,
    /// One of ours, which `segs` and `style` are all there is to, so it can be drawn again from
    /// them. Anything else is another editor's, and only its own dictionary draws it as it was.
    ours: bool,
    /// See [`InkShape::cuttable`]: one of ours, or another editor's with an `/InkList` to cut by,
    /// every path drawn alike.
    cuttable: bool,
    /// Its `/Popup`, which goes with another editor's stroke.
    popup: Option<accent_pdf::Ref>,
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
