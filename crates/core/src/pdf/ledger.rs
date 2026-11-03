//! Shared undo/redo bookkeeping for ink and page edits. GTK and Android choose where work runs;
//! this ledger defines the operations without widgets, channels or threads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use accent_pdf::Ref;

use crate::{Error, Result};

use crate::pdf::{self, PageEdit, PdfDoc};

/// An ink stroke's name and shape: the name the tools know it by, which stays with its object.
pub type NamedInk = (u32, pdf::InkShape);

/// A text of ours under its name, as [`NamedInk`] names a stroke.
pub type NamedText = (u32, pdf::TypedText);

/// Allocate a process-wide name, including for eraser fragments not yet in the document.
pub fn fresh_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One undo/redo step for ink or pages. A stroke is named by its object, which every edit of it
/// keeps, so the steps need not follow the `/Annots` order that erases and moves shuffle.
///
/// Page numbers remain those at step creation: sequential Undo and Redo restore the page order
/// needed by each step.
pub enum Step {
    /// A stroke or a shape was drawn. What it drew is kept once Undo has taken it off, so that
    /// Redo can put it back.
    Drawn {
        page: usize,
        at: Ref,
        kept: Option<pdf::Drawn>,
    },
    /// A stroke was erased, and this is what puts it back.
    Erased {
        page: usize,
        at: Ref,
        kept: Option<pdf::Drawn>,
    },
    /// A stroke was moved or resized by `matrix`, or a text moved.
    Moved {
        page: usize,
        at: Ref,
        matrix: pdf::Matrix,
    },
    /// A text of ours was typed again, in its place: `kept` is what it said before, and once
    /// Undo has put that back, what Redo makes it say again.
    Retyped {
        page: usize,
        at: Ref,
        kept: pdf::Typed,
    },
    /// A page was put in, taken out or moved. A delete, and an import, keep the pages they take
    /// out, which Undo and Redo put back: the page objects stay in the file, so each comes back
    /// as itself, its ink, links and bookmarks into it included. `id` names the step to whatever
    /// keeps something of its own about it: the window keeps the note links a delete left naming
    /// the page it took out.
    Paged {
        edit: PageEdit,
        kept: Option<Vec<Ref>>,
        id: u32,
    },
}

/// What one step of Undo or Redo changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Walked {
    /// This much of this page's ink.
    Ink(usize, pdf::Rect),
    /// The pages, as this edit moved them: taking an edit back is making its inverse. The id is
    /// the step's, as [`Ink::edit_pages`] gave it.
    Pages(PageEdit, u32),
}

/// Name the strokes the tools are given, and keep this session's ink and page edits for Undo and
/// Redo.
///
/// `names` and `objects` are one map read both ways: the name each stroke or text was listed
/// under, by its object. `done` and `undone` store one step per gesture, newest last.
#[derive(Default)]
pub struct Ink {
    names: HashMap<Ref, u32>,
    objects: HashMap<u32, Ref>,
    done: Vec<Vec<Step>>,
    undone: Vec<Vec<Step>>,
    pub dirty: bool,
    /// Whether a page went in, came out or moved since the file was last written: what changes
    /// its text, which ink never does. Set by every page edit; the writer clears it.
    pub paged: bool,
}

impl Ink {
    /// The object the tools name `id`, if they were ever given that name; whether it is still on
    /// its page is the document's to say.
    pub fn at(&self, id: u32) -> Option<Ref> {
        self.objects.get(&id).copied()
    }

    /// The name of the object `at`, giving it one the first time.
    fn name(&mut self, at: Ref) -> u32 {
        if let Some(&id) = self.names.get(&at) {
            return id;
        }
        let id = fresh_id();
        self.bind(id, at);
        id
    }

    /// The object `at` goes by `id`, a name given before it was drawn.
    fn bind(&mut self, id: u32, at: Ref) {
        self.names.insert(at, id);
        self.objects.insert(id, at);
    }

    /// A change was made: a step of its own, or one more of the gesture before it when
    /// `joined`. Whatever Undo had taken back cannot be put back on top of it.
    fn record(&mut self, step: Step, joined: bool) {
        match self.done.last_mut().filter(|_| joined) {
            Some(gesture) => gesture.push(step),
            None => self.done.push(vec![step]),
        }
        self.undone.clear();
    }

    /// A stroke or text of ours went onto `page` as the object `at`.
    pub fn drew(&mut self, page: usize, at: Ref) {
        self.record(
            Step::Drawn {
                page,
                at,
                kept: None,
            },
            false,
        );
    }

    /// The stroke `at` was erased — `was` being what puts it back — and the pieces `left` drawn
    /// in its place under the names given: what a partial eraser leaves of it, nothing for a
    /// whole one. Undo takes the pieces off and puts the stroke back as one step.
    pub fn erased(
        &mut self,
        page: usize,
        at: Ref,
        was: pdf::Drawn,
        left: Vec<(u32, Ref)>,
        joined: bool,
    ) {
        let kept = Some(was);
        self.record(Step::Erased { page, at, kept }, joined);
        for (id, at) in left {
            self.bind(id, at);
            let kept = None;
            self.record(Step::Drawn { page, at, kept }, true);
        }
    }

    /// The stroke or text `at` was moved by `matrix`.
    pub fn moved(&mut self, page: usize, at: Ref, matrix: pdf::Matrix) {
        self.record(Step::Moved { page, at, matrix }, false);
    }

    /// Move, stretch or turn the strokes and texts `ids` of `page` by `m`, as one step: the box
    /// they covered and now cover.
    pub fn transform(
        &mut self,
        doc: &mut PdfDoc,
        page: usize,
        ids: &[u32],
        m: pdf::Matrix,
    ) -> Result<pdf::Rect> {
        self.each(doc, page, ids, |doc, at| {
            let area = doc.transform_ink(page, at, m)?;
            Ok((
                area,
                Step::Moved {
                    page,
                    at,
                    matrix: m,
                },
            ))
        })
    }

    /// Take the strokes and texts `ids` off `page`, as one step: the box they covered.
    pub fn delete(&mut self, doc: &mut PdfDoc, page: usize, ids: &[u32]) -> Result<pdf::Rect> {
        self.each(doc, page, ids, |doc, at| {
            let (drawn, area) = doc.take_ink(page, at)?;
            let kept = Some(drawn);
            Ok((area, Step::Erased { page, at, kept }))
        })
    }

    /// The strokes and texts `ids` of `page` as plain geometry, each under its name, in the
    /// order they are drawn in; one that will not copy ([`PdfDoc::clip`]) is left out.
    pub fn clips(&self, doc: &PdfDoc, page: usize, ids: &[u32]) -> Vec<(u32, Vec<pdf::Clip>)> {
        (self.ordered(doc, page, ids).into_iter())
            .filter_map(|(id, at)| Some((id, doc.clip(page, at).ok()?)))
            .filter(|(_, clips)| !clips.is_empty())
            .collect()
    }

    /// `clips` drawn onto `page` under `m` as strokes and texts of ours, each under the name
    /// `names` gives it, as one step: the box they cover.
    pub fn paste(
        &mut self,
        doc: &mut PdfDoc,
        page: usize,
        clips: &[pdf::Clip],
        m: pdf::Matrix,
        names: &[u32],
    ) -> Result<pdf::Rect> {
        let (area, objects) = doc.paste(page, clips, m)?;
        for (n, at) in objects.into_iter().enumerate() {
            match names.get(n) {
                Some(&id) => self.bind(id, at),
                None => drop(self.name(at)),
            }
            self.record(
                Step::Drawn {
                    page,
                    at,
                    kept: None,
                },
                n > 0,
            );
        }
        Ok(area)
    }

    /// `change` made to each of the objects `ids` name on `page`, in the order they are drawn
    /// in, as one step: the box the changes covered. One no longer on the page, or that will not
    /// change, is passed over; none changed at all is refused.
    fn each(
        &mut self,
        doc: &mut PdfDoc,
        page: usize,
        ids: &[u32],
        mut change: impl FnMut(&mut PdfDoc, Ref) -> Result<(pdf::Rect, Step)>,
    ) -> Result<pdf::Rect> {
        let mut area: Option<pdf::Rect> = None;
        for (_, at) in self.ordered(doc, page, ids) {
            match change(doc, at) {
                Ok((changed, step)) => {
                    self.record(step, area.is_some());
                    area = Some(area.map_or(changed, |a| a.union(changed)));
                }
                Err(e) => tracing::debug!("object {} of page {page}: {e:#}", at.num),
            }
        }
        area.ok_or_else(|| Error::Pdf(format!("none of {ids:?} changed on page {page}")))
    }

    /// The objects `ids` name on `page`, in its `/Annots` order, which is the order they are
    /// drawn in; a name no longer on the page is left out.
    fn ordered(&self, doc: &PdfDoc, page: usize, ids: &[u32]) -> Vec<(u32, Ref)> {
        let mut found: Vec<(usize, u32, Ref)> = (ids.iter())
            .filter_map(|&id| {
                let at = self.at(id)?;
                Some((doc.cos.index_of(page, at).ok()?, id, at))
            })
            .collect();
        found.sort_unstable_by_key(|(index, ..)| *index);
        found.into_iter().map(|(_, id, at)| (id, at)).collect()
    }

    /// The text `at` was typed again; `was` is what it said before.
    pub fn retyped(&mut self, page: usize, at: Ref, was: pdf::Typed) {
        self.record(
            Step::Retyped {
                page,
                at,
                kept: was,
            },
            false,
        );
    }

    /// Put a page in, take one out or move one, as a step of its own: Undo takes it back as it
    /// takes back a stroke. A delete keeps the page it takes out of the page tree, so its Undo
    /// puts back the page itself, with its ink and with what points at it. Nothing reaches the
    /// disk here: [`PdfDoc::save_to`] is the second half, as it is for ink. The step's id, which
    /// Undo and Redo hand back with it.
    pub fn edit_pages(&mut self, doc: &mut PdfDoc, edit: PageEdit) -> Result<u32> {
        let mut kept = matches!(edit, PageEdit::Delete { .. }).then(Vec::new);
        self.edit(doc, edit, kept.as_mut())?;
        let id = fresh_id();
        self.record(Step::Paged { edit, kept, id }, false);
        Ok(id)
    }

    /// Put every page of `source` in at `at`, after the last page for `None`, as one step. Undo
    /// takes the pages out and keeps them, as it keeps a deleted page, so Redo puts them back
    /// without reading `source` again. The edit the pages made, and the step's id.
    pub fn import_pages(
        &mut self,
        doc: &mut PdfDoc,
        at: Option<usize>,
        source: &PdfDoc,
    ) -> Result<(PageEdit, u32)> {
        let at = at.unwrap_or_else(|| doc.page_count());
        let count = doc.import_pages(source, at)?;
        let edit = PageEdit::Insert { at, count };
        self.paged = true;
        let id = fresh_id();
        let kept = Some(Vec::new());
        self.record(Step::Paged { edit, kept, id }, false);
        Ok((edit, id))
    }

    /// Make one page edit. With `kept`, the pages a delete takes out are kept there, and an
    /// insert puts those back; without, the pages an insert puts in are blank ones.
    fn edit(
        &mut self,
        doc: &mut PdfDoc,
        edit: PageEdit,
        kept: Option<&mut Vec<Ref>>,
    ) -> Result<()> {
        match (edit, kept) {
            (PageEdit::Delete { at, count }, Some(kept)) => *kept = doc.take_pages(at, count)?,
            (PageEdit::Insert { at, .. }, Some(kept)) => doc.put_pages(at, kept)?,
            (PageEdit::Insert { at, count }, None) => {
                (0..count).try_for_each(|_| doc.insert_page(at))?
            }
            (PageEdit::Delete { at, count }, None) => doc.take_pages(at, count).map(drop)?,
            (PageEdit::Move { from, to }, _) => doc.move_page(from, to)?,
        }
        self.paged = true;
        Ok(())
    }

    /// Whether Undo, and then Redo, has anything to walk.
    pub fn history(&self) -> (bool, bool) {
        (!self.done.is_empty(), !self.undone.is_empty())
    }

    /// Every ink stroke of `page`, each under the name the tools will know it by.
    pub fn named(&mut self, doc: &PdfDoc, page: usize) -> Vec<NamedInk> {
        (doc.inks(page).unwrap_or_default().into_iter())
            .map(|shape| (self.name(shape.at), shape))
            .collect()
    }

    /// Every text of ours on `page`, each under the name the tools will know it by.
    pub fn named_texts(&mut self, doc: &PdfDoc, page: usize) -> Vec<NamedText> {
        (doc.texts(page).unwrap_or_default().into_iter())
            .map(|text| (self.name(text.at), text))
            .collect()
    }

    /// The stroke `at` of `page` under its name, read alone: what a stroke just drawn hands the
    /// tools, rather than every stroke of its page read again.
    pub fn named_at(&mut self, doc: &PdfDoc, page: usize, at: Ref) -> Option<NamedInk> {
        let shape = doc.ink_at(page, at).ok()??;
        Some((self.name(at), shape))
    }

    /// Walk the last gesture back (Undo), or the last one Undo took forward again (Redo), and
    /// say what each of its steps changed.
    ///
    /// A step that cannot be made — pdfium refusing, or a stroke no longer on its page — drops
    /// its gesture from the history, after whatever steps of it came first. A page edit that
    /// cannot be made drops the whole history: every step on either side of it names pages by
    /// the numbers it would have given them.
    pub fn walk(&mut self, doc: &mut PdfDoc, forwards: bool) -> Vec<Walked> {
        let gesture = match forwards {
            true => self.undone.pop(),
            false => self.done.pop(),
        };
        let Some(mut gesture) = gesture else {
            return Vec::new();
        };
        // Backwards in the reverse of the order it was made in.
        if !forwards {
            gesture.reverse();
        }
        let mut changed = Vec::new();
        for step in &mut gesture {
            match self.apply(doc, step, forwards) {
                Ok(walked) => changed.push(walked),
                Err(e) => {
                    tracing::warn!("walking the history: {e:#}");
                    if matches!(step, Step::Paged { .. }) {
                        self.done.clear();
                        self.undone.clear();
                    }
                    return changed;
                }
            }
        }
        if !forwards {
            gesture.reverse();
        }
        match forwards {
            true => self.done.push(gesture),
            false => self.undone.push(gesture),
        }
        changed
    }

    /// Make one step (`forwards`) or take it back: a stroke goes on or comes off the page or moves
    /// by the map or by its inverse, or the pages are edited or edited back. A delete and the
    /// Undo of an import keep the pages they take out, which their Undo and Redo put back. The
    /// page a blank insert put in is blank again when its Undo takes it out, every step on it
    /// having been walked back first, so a blank one is what Redo puts in.
    fn apply(&mut self, doc: &mut PdfDoc, step: &mut Step, forwards: bool) -> Result<Walked> {
        match (step, forwards) {
            (Step::Paged { edit, kept, id }, _) => {
                let made = match forwards {
                    true => *edit,
                    false => edit.inverse(),
                };
                self.edit(doc, made, kept.as_mut())?;
                Ok(Walked::Pages(made, *id))
            }
            // Either way, what it says swaps with what was kept.
            (Step::Retyped { page, at, kept }, _) => {
                let (was, area) = doc.retype(*page, *at, &kept.text, kept.style)?;
                *kept = was;
                Ok(Walked::Ink(*page, area))
            }
            (Step::Moved { page, at, matrix }, _) => {
                let m = match forwards {
                    true => *matrix,
                    false => pdf::invert(*matrix),
                };
                let area = doc.transform_ink(*page, *at, m)?;
                Ok(Walked::Ink(*page, area))
            }
            // Drawing forwards puts a stroke on, and so does erasing backwards.
            (Step::Drawn { page, at, kept }, true) | (Step::Erased { page, at, kept }, false) => {
                let drawn = (kept.as_ref())
                    .ok_or_else(|| Error::Pdf(format!("nothing kept of object {}", at.num)))?;
                let area = doc.redraw_ink(*page, drawn)?;
                Ok(Walked::Ink(*page, area))
            }
            (Step::Drawn { page, at, kept }, false) | (Step::Erased { page, at, kept }, true) => {
                let (drawn, area) = doc.take_ink(*page, *at)?;
                *kept = Some(drawn);
                Ok(Walked::Ink(*page, area))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::PageEdit;
    use crate::pdf::tests::{open_tiny, page_texts, reopen};

    /// A stroke keeps its object, and with it its name, through the moves and erases that
    /// shuffle `/Annots` and the highlights an export appends: pdfium lists each where the file
    /// lists its object. The name of one erased finds it no longer on the page, which is what
    /// keeps a list that has not caught up from erasing the stroke after the one it meant.
    #[test]
    fn a_stroke_keeps_its_name_through_moves_and_erases() {
        let Some((_dir, mut doc)) = open_tiny() else {
            return;
        };
        let mut ink = Ink::default();
        // After the page's link, three lines of ours.
        let mut drawn = Vec::new();
        for y in [20.0, 40.0, 60.0] {
            let line = pdf::Shape::Line {
                a: (20.0, y),
                b: (80.0, y),
            };
            let (_, at) = doc.add_shape(0, line, STYLE).unwrap();
            ink.drew(0, at);
            drawn.push(at);
        }
        let names: Vec<u32> = ink.named(&doc, 0).iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ink.named(&doc, 0)
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            names
        );

        // The first is moved 10 pt down, which puts it on top; then the second is erased, and an
        // export's highlight joins the end.
        let down = [1.0, 0.0, 0.0, 1.0, 0.0, 10.0];
        doc.transform_ink(0, drawn[0], down).unwrap();
        ink.moved(0, drawn[0], down);
        let (was, _) = doc.take_ink(0, drawn[1]).unwrap();
        ink.erased(0, drawn[1], was, Vec::new(), false);
        let mark = pdf::Highlight {
            page: 0,
            quads: vec![pdf::Rect::from_corners((20.0, 30.0), (90.0, 50.0))],
            color: [255, 255, 0, 255],
            contents: None,
        };
        doc.add_highlights(&[mark]).unwrap();

        let listed: Vec<_> = (ink.named(&doc, 0).into_iter())
            .map(|(id, shape)| (id, shape.at, shape.points[0].1.round() as i32))
            .collect();
        assert_eq!(listed, [(names[2], drawn[2], 60), (names[0], drawn[0], 30)]);
        assert_eq!(ink.at(names[1]), Some(drawn[1]));
        assert!(doc.take_ink(0, drawn[1]).is_err(), "gone from the page");
    }

    /// Undo walks every kind of step back and Redo walks it forward again, on a real page: a
    /// stroke, a move of it, and one eraser drag over it and over a stroke the file already had.
    #[test]
    fn undo_and_redo_walk_every_kind_of_step() {
        if !pdf::available() {
            eprintln!("skipping: no libpdfium");
            return;
        }
        let path = std::env::temp_dir().join(format!("accent-undo-{}.pdf", std::process::id()));
        std::fs::write(&path, pdf::blank_pdf(pdf::A4).unwrap()).unwrap();
        let mut doc = PdfDoc::open(&path).unwrap();
        let style = pdf::InkStyle {
            width: 2.0,
            rgba: [0, 0, 0, 255],
            multiply: false,
        };
        let line = |y: f32| pdf::Shape::Line {
            a: (20.0, y),
            b: (80.0, y),
        };
        // Where the page's lines are, top to bottom.
        let lines = |doc: &PdfDoc| {
            let mut ys: Vec<i32> = doc
                .inks(0)
                .unwrap()
                .iter()
                .map(|i| i.points[0].1 as i32)
                .collect();
            ys.sort();
            ys
        };
        let mut ink = Ink::default();
        // The file's own line, then one of ours, moved 10 pt down.
        doc.add_shape(0, line(20.0), style).unwrap();
        let (_, at) = doc.add_shape(0, line(40.0), style).unwrap();
        ink.drew(0, at);
        let down = [1.0, 0.0, 0.0, 1.0, 0.0, 10.0];
        doc.transform_ink(0, at, down).unwrap();
        ink.moved(0, at, down);
        assert_eq!(lines(&doc), [20, 50]);
        // One drag takes both, so the eraser's list names the file's own too.
        let named: Vec<u32> = ink.named(&doc, 0).iter().map(|(id, _)| *id).collect();
        for (n, id) in named.into_iter().enumerate() {
            let at = ink.at(id).unwrap();
            let (kept, _) = doc.take_ink(0, at).unwrap();
            ink.erased(0, at, kept, Vec::new(), n > 0);
        }
        assert!(lines(&doc).is_empty());
        assert_eq!(ink.history(), (true, false));

        // Back through the drag, the move and the stroke; the file's own line stays.
        let walked: Vec<Vec<i32>> = (0..4)
            .map(|_| {
                ink.walk(&mut doc, false);
                lines(&doc)
            })
            .collect();
        assert_eq!(walked, [vec![20, 50], vec![20, 40], vec![20], vec![20]]);
        assert_eq!(ink.history(), (false, true));
        // And forward again.
        let walked: Vec<Vec<i32>> = (0..3)
            .map(|_| {
                ink.walk(&mut doc, true);
                lines(&doc)
            })
            .collect();
        assert_eq!(walked, [vec![20, 40], vec![20, 50], vec![]]);
        // A new stroke after an undo leaves nothing to redo.
        ink.walk(&mut doc, false);
        let (_, at) = doc.add_shape(0, line(60.0), style).unwrap();
        ink.drew(0, at);
        assert_eq!(lines(&doc), [20, 50, 60]);
        assert_eq!(ink.history(), (true, false));
        assert!(!ink.paged, "ink, erases and moves leave the text as it was");
        let _ = std::fs::remove_file(&path);
    }

    /// Undo takes an added page out again and moves a moved one back, each as the edit that
    /// undoes it, and Redo makes both again.
    #[test]
    fn undo_and_redo_walk_an_insert_and_a_move() {
        let Some((_dir, mut doc)) = open_tiny() else {
            return;
        };
        let mut ink = Ink::default();
        let insert = ink.edit_pages(&mut doc, PageEdit::insert(1)).unwrap();
        assert!(ink.paged, "a page edit changes the text");
        ink.paged = false;
        let moved = ink
            .edit_pages(&mut doc, PageEdit::Move { from: 2, to: 0 })
            .unwrap();
        assert_eq!(page_texts(&doc), ["Second page", "Hello accent", ""]);

        let undo = PageEdit::Move { from: 0, to: 2 };
        assert_eq!(ink.walk(&mut doc, false), [Walked::Pages(undo, moved)]);
        assert!(ink.paged, "and so does its Undo");
        assert_eq!(page_texts(&doc), ["Hello accent", "", "Second page"]);
        let undo = PageEdit::delete(1);
        assert_eq!(ink.walk(&mut doc, false), [Walked::Pages(undo, insert)]);
        assert_eq!(page_texts(&doc), ["Hello accent", "Second page"]);
        assert_eq!(ink.history(), (false, true));

        let redo = PageEdit::insert(1);
        assert_eq!(ink.walk(&mut doc, true), [Walked::Pages(redo, insert)]);
        ink.walk(&mut doc, true);
        assert_eq!(page_texts(&doc), ["Second page", "Hello accent", ""]);
        assert_eq!(ink.history(), (true, false));
    }

    /// Another document's pages go in where they were dropped, as one step: the bookmarks into the
    /// page they went before follow it, Undo takes them all out, Redo puts them back without the
    /// other document, and the file saved holds them.
    #[test]
    fn undo_and_redo_walk_an_import() {
        let Some((dir, mut doc)) = open_tiny() else {
            return;
        };
        let source = reopen(&dir, &doc);
        let mut ink = Ink::default();
        let (edit, id) = ink.import_pages(&mut doc, Some(1), &source).unwrap();
        drop(source);
        assert_eq!(edit, PageEdit::Insert { at: 1, count: 2 });
        let imported = ["Hello accent", "Hello accent", "Second page", "Second page"];
        assert_eq!(page_texts(&doc), imported);
        let marks =
            |doc: &PdfDoc| -> Vec<_> { doc.outline().unwrap().iter().map(|o| o.page).collect() };
        assert_eq!(marks(&doc), [Some(3), Some(3)]);

        let undo = PageEdit::Delete { at: 1, count: 2 };
        assert_eq!(ink.walk(&mut doc, false), [Walked::Pages(undo, id)]);
        assert_eq!(page_texts(&doc), ["Hello accent", "Second page"]);
        assert_eq!(ink.walk(&mut doc, true), [Walked::Pages(edit, id)]);
        assert_eq!(page_texts(&reopen(&dir, &doc)), imported);

        // With no place given, after the last page.
        let source = reopen(&dir, &doc);
        let (edit, _) = ink.import_pages(&mut doc, None, &source).unwrap();
        assert_eq!(edit, PageEdit::Insert { at: 4, count: 4 });
        assert_eq!(doc.page_count(), 8);
    }

    /// Strokes and page edits walk back in the order they were made, through one history: a
    /// deleted page comes back with its text and its ink, the stroke drawn on it before the
    /// delete is still Undo's to take, and Redo deletes it again. The page put back is the one
    /// the save writes.
    #[test]
    fn undo_puts_a_deleted_page_back_with_its_ink() {
        let Some((dir, mut doc)) = open_tiny() else {
            return;
        };
        let mut ink = Ink::default();
        let draw = |doc: &mut PdfDoc, ink: &mut Ink, page| {
            let line = pdf::Shape::Line {
                a: (20.0, 20.0),
                b: (80.0, 20.0),
            };
            let (_, at) = doc.add_shape(page, line, STYLE).unwrap();
            ink.drew(page, at);
        };
        // What each page reads, and how many strokes it carries.
        let pages = |doc: &PdfDoc| -> Vec<(String, usize)> {
            let texts = page_texts(doc).into_iter().enumerate();
            texts
                .map(|(p, t)| (t, doc.inks(p).unwrap().len()))
                .collect()
        };
        let hello = |n| ("Hello accent".to_string(), n);
        let second = |n| ("Second page".to_string(), n);
        // A stroke on each page, the first page deleted, and a stroke on the page that took its
        // number.
        draw(&mut doc, &mut ink, 0);
        draw(&mut doc, &mut ink, 1);
        ink.edit_pages(&mut doc, PageEdit::delete(0)).unwrap();
        draw(&mut doc, &mut ink, 0);
        assert_eq!(pages(&doc), [second(2)]);

        let mut walk = |forwards| {
            ink.walk(&mut doc, forwards);
            pages(&doc)
        };
        let back: Vec<_> = (0..4).map(|_| walk(false)).collect();
        assert_eq!(
            back,
            [
                vec![second(1)],
                vec![hello(1), second(1)],
                vec![hello(1), second(0)],
                vec![hello(0), second(0)],
            ]
        );
        let forward: Vec<_> = (0..4).map(|_| walk(true)).collect();
        assert_eq!(
            forward,
            [
                vec![hello(1), second(0)],
                vec![hello(1), second(1)],
                vec![second(1)],
                vec![second(2)],
            ]
        );
        // The redone delete kept the page again, for the next Undo.
        walk(false);
        assert_eq!(walk(false), [hello(1), second(1)]);
        let saved = reopen(&dir, &doc);
        assert_eq!(page_texts(&saved), ["Hello accent", "Second page"]);
    }

    /// Undo of a delete brings back the page itself rather than a copy: the link on it into the
    /// document, and the link and the bookmarks elsewhere naming it, lead to it again, in the
    /// file it saves too. Redo takes it out again.
    #[test]
    fn undo_of_a_delete_brings_back_what_points_at_the_page() {
        let Some((dir, mut doc)) = open_tiny() else {
            return;
        };
        let mut ink = Ink::default();
        // Where the first page's link and each bookmark lead.
        let targets = |doc: &PdfDoc| {
            let links = doc.links(0).unwrap();
            let link = links.iter().find_map(|l| match l.target {
                pdf::LinkTarget::Page { page, .. } => Some(page),
                pdf::LinkTarget::Uri(_) => None,
            });
            let marks: Vec<_> = doc.outline().unwrap().iter().map(|o| o.page).collect();
            (link, marks)
        };
        let before = targets(&doc);
        assert_eq!(before, (Some(1), vec![Some(1), Some(1)]));
        // The second page, which the link and both bookmarks name; then the first, which holds
        // the link.
        for page in [1, 0] {
            ink.edit_pages(&mut doc, PageEdit::delete(page)).unwrap();
            ink.walk(&mut doc, false);
            assert_eq!(targets(&doc), before, "page {page} put back");
            ink.walk(&mut doc, true);
            assert_eq!(doc.page_count(), 1, "page {page} taken out again");
            ink.walk(&mut doc, false);
        }
        assert_eq!(targets(&reopen(&dir, &doc)), before);

        // An Export Highlights made since a delete is not the delete's: it stays through its
        // Undo and Redo.
        ink.edit_pages(&mut doc, PageEdit::delete(1)).unwrap();
        let had = doc.annotation_count(0).unwrap();
        let quads = vec![pdf::Rect::from_corners((20.0, 30.0), (90.0, 50.0))];
        let color = [255, 255, 0, 255];
        let mark = pdf::Highlight {
            page: 0,
            quads,
            color,
            contents: None,
        };
        doc.add_highlights(&[mark]).unwrap();
        ink.walk(&mut doc, false);
        assert_eq!(doc.annotation_count(0).unwrap(), had + 1);
        ink.walk(&mut doc, true);
        assert_eq!(doc.annotation_count(0).unwrap(), had + 1);
    }

    /// A stroke just drawn, read alone, is as reading its whole page lists it: the same name and
    /// shape, after the page's link and a stroke already on it.
    #[test]
    fn a_stroke_read_alone_is_as_its_page_lists_it() {
        let Some((_dir, mut doc)) = open_tiny() else {
            return;
        };
        let mut ink = Ink::default();
        let line = |y: f32| pdf::Shape::Line {
            a: (20.0, y),
            b: (80.0, y),
        };
        doc.add_shape(0, line(20.0), STYLE).unwrap();
        let (_, at) = doc.add_shape(0, line(40.0), STYLE).unwrap();
        ink.drew(0, at);
        let alone = ink.named_at(&doc, 0, at).unwrap();
        assert_eq!(ink.named(&doc, 0).last(), Some(&alone));
    }

    const STYLE: pdf::InkStyle = pdf::InkStyle {
        width: 2.0,
        rgba: [0, 0, 0, 255],
        multiply: false,
    };

    /// A selection is one step for a move, a delete and a paste each: two lines and a text of
    /// ours move together and come back together, one taken off comes back, and their copies,
    /// through JSON, go onto the second page as they were drawn, under the names given them, and
    /// into the file saved.
    #[test]
    fn a_selection_moves_deletes_and_pastes_as_one_step_each() {
        let Some((dir, mut doc)) = open_tiny() else {
            return;
        };
        let mut ink = Ink::default();
        for y in [20.0, 40.0] {
            let line = pdf::Shape::Line {
                a: (20.0, y),
                b: (80.0, y),
            };
            let (_, at) = doc.add_shape(0, line, STYLE).unwrap();
            ink.drew(0, at);
        }
        let red = pdf::TextStyle {
            size: 10.0,
            rgb: [200, 0, 0],
        };
        let (_, at) = doc.add_text(0, (20.0, 60.0), "Hi", red).unwrap();
        ink.drew(0, at);
        // Where each stroke starts and each text's corner is on a page, and how each is drawn.
        let placed = |doc: &PdfDoc, page| {
            let inks = doc.inks(page).unwrap().into_iter();
            let inks = inks.map(|i| (i.points[0], format!("{:?}", i.style)));
            let texts = doc.texts(page).unwrap().into_iter();
            let texts = texts.map(|t| (t.typed.at, format!("{:?}", t.typed.style)));
            let mut out: Vec<_> = (inks.chain(texts))
                .map(|((x, y), look)| (x.round() as i32, y.round() as i32, look))
                .collect();
            out.sort();
            out
        };
        let corners = |doc: &PdfDoc, page| -> Vec<(i32, i32)> {
            placed(doc, page)
                .into_iter()
                .map(|(x, y, _)| (x, y))
                .collect()
        };
        let mut ids: Vec<u32> = ink.named(&doc, 0).iter().map(|(id, _)| *id).collect();
        ids.extend(ink.named_texts(&doc, 0).iter().map(|(id, _)| *id));
        let before = placed(&doc, 0);

        let right = [1.0, 0.0, 0.0, 1.0, 10.0, 0.0];
        ink.transform(&mut doc, 0, &ids, right).unwrap();
        assert_eq!(corners(&doc, 0), [(30, 20), (30, 40), (30, 60)]);
        ink.walk(&mut doc, false);
        assert_eq!(placed(&doc, 0), before, "one Undo puts all three back");

        ink.delete(&mut doc, 0, &ids[1..]).unwrap();
        assert_eq!(corners(&doc, 0), [(20, 20)]);
        ink.walk(&mut doc, false);
        assert_eq!(placed(&doc, 0), before, "one Undo puts both back");

        let clips: Vec<pdf::Clip> = (ink.clips(&doc, 0, &ids).into_iter())
            .flat_map(|(_, clips)| clips)
            .collect();
        let json = serde_json::to_string(&clips).unwrap();
        let clips: Vec<pdf::Clip> = serde_json::from_str(&json).unwrap();
        let names: Vec<u32> = clips.iter().map(|_| fresh_id()).collect();
        let down = [1.0, 0.0, 0.0, 1.0, 0.0, 100.0];
        ink.paste(&mut doc, 1, &clips, down, &names).unwrap();
        let moved = |(x, y, look): (i32, i32, String)| (x, y + 100, look);
        let pasted: Vec<_> = before.iter().cloned().map(moved).collect();
        assert_eq!(placed(&doc, 1), pasted);
        let mut listed: Vec<u32> = ink.named(&doc, 1).iter().map(|(id, _)| *id).collect();
        listed.extend(ink.named_texts(&doc, 1).iter().map(|(id, _)| *id));
        listed.sort();
        assert_eq!(listed, names, "under the names given");
        ink.walk(&mut doc, false);
        assert!(corners(&doc, 1).is_empty(), "one Undo takes all three off");
        ink.walk(&mut doc, true);
        assert_eq!(placed(&reopen(&dir, &doc), 1), pasted);
    }

    /// A cut is one step: Undo takes the pieces off and puts the stroke back whole, and Redo cuts
    /// it again, the pieces under the names the widget gave them.
    #[test]
    fn undo_puts_a_cut_stroke_back_whole() {
        if !pdf::available() {
            eprintln!("skipping: no libpdfium");
            return;
        }
        let path = std::env::temp_dir().join(format!("accent-cut-{}.pdf", std::process::id()));
        std::fs::write(&path, pdf::blank_pdf(pdf::A4).unwrap()).unwrap();
        let mut doc = PdfDoc::open(&path).unwrap();
        let style = pdf::InkStyle {
            width: 2.0,
            rgba: [0, 0, 0, 255],
            multiply: false,
        };
        let line = pdf::Shape::Line {
            a: (20.0, 50.0),
            b: (180.0, 50.0),
        };
        // Where each stroke on the page begins and ends, left to right.
        let spans = |doc: &PdfDoc| {
            let ends = |i: &pdf::InkShape| (i.points[0].0, i.points[i.points.len() - 1].0);
            let mut spans: Vec<(i32, i32)> = doc
                .inks(0)
                .unwrap()
                .iter()
                .map(|i| (ends(i).0.round() as i32, ends(i).1.round() as i32))
                .collect();
            spans.sort();
            spans
        };
        let mut ink = Ink::default();
        let (_, at) = doc.add_shape(0, line, style).unwrap();
        ink.drew(0, at);
        // A 4 pt eraser straight down across the middle takes 5 pt either side of it.
        let cut = doc.cut_ink(0, at, (100.0, 20.0), (100.0, 80.0), 4.0);
        let cut = cut.unwrap().expect("the pass crossed the line");
        let names: Vec<u32> = cut.left.iter().map(|_| fresh_id()).collect();
        let left = names.iter().copied().zip(cut.left).collect();
        ink.erased(0, at, cut.was, left, false);
        assert_eq!(spans(&doc), [(20, 95), (105, 180)]);

        ink.walk(&mut doc, false);
        assert_eq!(spans(&doc), [(20, 180)]);
        ink.walk(&mut doc, true);
        assert_eq!(spans(&doc), [(20, 95), (105, 180)]);
        let listed: Vec<u32> = ink.named(&doc, 0).iter().map(|(id, _)| *id).collect();
        assert!(
            names.iter().all(|id| listed.contains(id)),
            "{names:?} {listed:?}"
        );
        // Back past the cut and the stroke it was made in.
        ink.walk(&mut doc, false);
        ink.walk(&mut doc, false);
        assert!(spans(&doc).is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
