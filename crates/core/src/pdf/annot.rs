//! Annotations: reading and writing `/Highlight` and `/Ink`, and the colour trap around both.

use anyhow::{Context, Result, anyhow};
use pdfium_render::prelude::*;

use super::ink::{Seg, catmull_rom, flatten, points_of, segments_of, thin, transformed};
use super::text::same_quads;
use super::{Highlight, InkPath, InkShape, InkStyle, Matrix, PdfDoc, Rect, Shape, lock};

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
        for h in highlights {
            let mut p = self.page(h.page)?;
            // Manual: under the default every annotation added re-serialises the *page's* content
            // stream, which we never touch. The annotation's own appearance stream is written by
            // pdfium either way.
            p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
            if highlights_of(h.page, &p)
                .iter()
                .any(|e| same_quads(&e.quads, &h.quads))
            {
                continue;
            }
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
                .context("create highlight annotation")?;
            // The colour first, and never `fill_color`/`stroke_color` as *getters*: both setters
            // fall back to casting the annotation handle to a page-object handle once an
            // appearance stream exists, which is the crash `annotation_color` documents. On a
            // fresh annotation the safe branch is the one that runs.
            a.set_stroke_color(PdfColor::new(
                h.color[0], h.color[1], h.color[2], h.color[3],
            ))
            .context("highlight colour")?;
            // Before the quads: pdfium copies `/Rect` into the appearance stream's bounding box.
            a.set_bounds(bounds.to_pdf(height))
                .context("highlight bounds")?;
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
                    .context("highlight quad")?;
            }
            if let Some(text) = &h.contents {
                a.set_contents(text).context("highlight contents")?;
            }
            a.set_is_printed(true).context("highlight print flag")?;
            added += 1;
        }
        Ok(added)
    }

    /// Draw one free-hand stroke onto a page as an `/Ink` annotation.
    ///
    /// `points` are in this module's top-left-origin page points, `width` is the stroke width in
    /// points, `rgb` its colour.
    ///
    // ponytail: the geometry ends up in the annotation's *appearance stream* rather than in an
    // `/InkList` array, because `FPDFAnnot_AddInkStroke` is only reachable through the raw
    // bindings and pdfium-render keeps the annotation handle private. Every viewer renders the
    // appearance stream, so this draws correctly everywhere; what it costs is an editor that
    // wants to reshape the stroke, which would need `/InkList`. Raw bindings are the upgrade.
    pub fn add_ink(&mut self, page: usize, points: &[(f32, f32)], style: InkStyle) -> Result<()> {
        let thinned = thin(points, 1.5);
        let Some(&first) = thinned.first() else {
            return Ok(());
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
    pub fn add_shape(&mut self, page: usize, shape: Shape, style: InkStyle) -> Result<()> {
        let _guard = lock();
        let mut p = self.page(page)?;
        self.put_ink(&mut p, &segments_of(shape), style)
    }

    /// The tail every ink writer shares: the path, then the annotation around it. The caller
    /// holds the lock, and `segs` starts with a `Move`.
    fn put_ink(&self, p: &mut PdfPage<'_>, segs: &[Seg], style: InkStyle) -> Result<()> {
        let InkStyle {
            width,
            rgba,
            multiply,
        } = style;
        let Some(&Seg::Move(first)) = segs.first() else {
            return Err(anyhow!("an ink path starts with a move"));
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
        .context("ink path")?;
        path.set_line_cap(PdfPageObjectLineCap::Round)
            .context("ink line cap")?;
        path.set_line_join(PdfPageObjectLineJoin::Round)
            .context("ink line join")?;
        if multiply {
            path.set_blend_mode(PdfPageObjectBlendMode::Multiply)
                .context("ink blend mode")?;
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
            .context("ink segment")?;
        }

        let mut ink = p
            .annotations_mut()
            .create_ink_annotation()
            .context("create ink annotation")?;
        ink.set_stroke_color(colour).context("ink colour")?;
        // Before the object: pdfium copies `/Rect` into the appearance stream's bounding box, and
        // a box set afterwards would scale what was drawn into it.
        ink.set_bounds(bounds.to_pdf(height))
            .context("ink bounds")?;
        ink.objects_mut()
            .add_object(path.into())
            .context("ink object")?;
        ink.set_is_printed(true).context("ink print flag")?;
        Ok(())
    }

    /// How many annotations of every kind a page carries.
    pub fn annotation_count(&self, page: usize) -> Result<usize> {
        let _guard = lock();
        Ok(self.page(page)?.annotations().len())
    }

    /// Remove one annotation by its index in the page's `/Annots`.
    pub fn delete_annotation(&mut self, page: usize, index: usize) -> Result<()> {
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        // Through `annotations_mut` rather than `annotations`: the annotation has to carry the
        // document's lifetime for `delete_annotation` to take it, and the shared accessor hands
        // back one borrowed from `p` instead.
        let a = p
            .annotations_mut()
            .get(index as PdfPageAnnotationIndex)
            .map_err(|e| anyhow!("annotation {index} of page {page}: {e:?}"))?;
        p.annotations_mut()
            .delete_annotation(a)
            .context("delete annotation")?;
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

    /// Every `/Ink` annotation on a page, flattened, with its box and style: what the Adjust tool
    /// takes hold of. Curves are sampled, so a circle's rim answers to the pointer and not the
    /// control polygon around it.
    ///
    // ponytail: the points are read straight out of the appearance path, so a stroke drawn by
    // another editor whose appearance stream carries a `/Matrix` is hit-tested in form space and
    // may not answer to the pointer. Ours never do; a transform-aware read is the upgrade.
    pub fn inks(&self, page: usize) -> Result<Vec<InkShape>> {
        let _guard = lock();
        let p = self.page(page)?;
        let height = p.height().value;
        Ok(p.annotations()
            .iter()
            .enumerate()
            .filter_map(|(index, a)| {
                let (segs, style) = read_ink(&a, height)?;
                let bounds = Rect::from_pdf(a.bounds().ok()?, height);
                Some(InkShape {
                    index,
                    points: flatten(&segs),
                    bounds,
                    style,
                })
            })
            .collect())
    }

    /// Move or resize one `/Ink` annotation by an affine map over its page. The annotation is
    /// deleted and drawn again, so it comes back at the end of `/Annots`.
    ///
    // ponytail: deleted and re-created rather than transformed in place, because pdfium only
    // ever grows an appearance stream's `/BBox` when `/Rect` changes: shrinking a box scales
    // what was drawn into it instead of moving it.
    pub fn transform_ink(&mut self, page: usize, index: usize, m: Matrix) -> Result<()> {
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let height = p.height().value;
        let (segs, style) = {
            let a = p
                .annotations()
                .get(index as PdfPageAnnotationIndex)
                .map_err(|e| anyhow!("annotation {index} of page {page}: {e:?}"))?;
            read_ink(&a, height)
                .ok_or_else(|| anyhow!("annotation {index} of page {page} is not a drawn path"))?
        };
        let a = p
            .annotations_mut()
            .get(index as PdfPageAnnotationIndex)
            .map_err(|e| anyhow!("annotation {index} of page {page}: {e:?}"))?;
        p.annotations_mut()
            .delete_annotation(a)
            .context("delete annotation")?;
        self.put_ink(&mut p, &transformed(&segs, m), style)
    }
}

/// The colour an annotation is drawn in.
///
/// Deliberately avoids `PdfPageAnnotationCommon::fill_color()` **and** `stroke_color()` on any
/// annotation carrying an appearance stream. Both call `FPDFAnnot_GetColor()`, which pdfium
/// documents as returning false in exactly that case, and both then fall back to
/// `FPDFPageObj_GetFillColor(self.handle() as FPDF_PAGEOBJECT)` — casting an `FPDF_ANNOTATION`
/// to an unrelated opaque pointer type. That is UB and segfaults in practice; rendering a page
/// makes pdfium synthesise the appearance stream, so it fires on the second call and not the
/// first. Reproduced on pdfium-render 0.9.3 against both pdfium 7881 (the build its bindings are
/// generated from) and 8035, so it is a crate bug, not an ABI mismatch.
///
/// The split below keeps us on the safe branch of each: with an appearance stream we read the
/// generated path object's real fill colour, without one `FPDFAnnot_GetColor()` succeeds on `/C`
/// and the bad fallback is never reached.
///
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
        let mut quads: Vec<Rect> = a
            .attachment_points()
            .iter()
            .map(|q| Rect::from_pdf(q.to_rect(), page_height))
            .collect();
        // Not every producer writes /QuadPoints; /Rect is the coarse fallback.
        if quads.is_empty()
            && let Ok(b) = a.bounds()
        {
            quads.push(Rect::from_pdf(b, page_height));
        }
        let c = annotation_color(&a).unwrap_or(PdfColor::new(255, 255, 0, 255));
        out.push(Highlight {
            page,
            quads,
            color: [c.red(), c.green(), c.blue(), c.alpha()],
            contents: a.contents(),
        });
    }
    out
}

fn annotation_color(a: &PdfPageAnnotation<'_>) -> Option<PdfColor> {
    if a.objects().len() > 0 {
        return a.objects().iter().find_map(|o| o.fill_color().ok());
    }
    a.stroke_color().ok()
}

/// An `/Ink` annotation's path and style, read back out of its appearance stream; `None` for
/// any other annotation, or one without a path. pdfium reports a cubic as three consecutive
/// Bézier points — two controls, then the end.
fn read_ink(a: &PdfPageAnnotation<'_>, height: f32) -> Option<(Vec<Seg>, InkStyle)> {
    if a.annotation_type() != PdfPageAnnotationType::Ink {
        return None;
    }
    a.objects().iter().find_map(|o| {
        let path = o.as_path_object()?;
        let mut segs = Vec::new();
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
        // The page-object getters, never the annotation's own: those cast the handle into a
        // page object once an appearance stream exists (see `annotation_color`).
        let c = path.stroke_color().ok()?;
        let style = InkStyle {
            width: path.stroke_width().ok()?.value,
            rgba: [c.red(), c.green(), c.blue(), c.alpha()],
            // ponytail: pdfium-render has no blend-mode getter, and a translucent stroke of ours
            // is the highlighter, so alpha stands in for `/Multiply`.
            multiply: c.alpha() < 255,
        };
        Some((segs, style))
    })
}
