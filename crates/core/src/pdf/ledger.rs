//! The undo ledger: what this session drew, erased and moved on a document's pages, and which
//! pages it put in, took out and moved.
//!
//! It lives here rather than in a viewer because both viewers need it — the GTK render thread
//! and the Android session — and because it is pure bookkeeping over [`PdfDoc`]: no widget, no
//! channel, no thread. What a viewer adds on top is where the work runs, not what it means.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Result, anyhow};

use crate::pdf::{self, PageEdit, PdfDoc};

/// One ink stroke as the tools address it: the id it was given, which unlike its place in
/// `/Annots` survives every erase and move made before it lands, and its shape.
pub type NamedInk = (u32, pdf::InkShape);

/// A name no stroke in the process has had. One counter for every thread, so a widget can name
/// the pieces its eraser cuts before the document has seen them.
pub fn fresh_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One change this session made to a page's ink or to the pages, for Undo to walk back and Redo
/// to walk forward again. A stroke is named by an id rather than an index, because every erase
/// and every move shuffles the indices.
///
/// A step's page number is the page's number when the step was made, and is never rewritten:
/// Undo and Redo walk the one history newest first, so whenever a step is walked the pages are
/// in the order they were in when it was made.
pub enum Step {
    /// A stroke or a shape was drawn. What it drew is kept once Undo has taken it off, so that
    /// Redo can draw it again.
    Drawn {
        page: usize,
        id: u32,
        kept: Option<pdf::Drawn>,
    },
    /// A stroke was erased, and this is what it drew.
    Erased {
        page: usize,
        id: u32,
        kept: Option<pdf::Drawn>,
    },
    /// A stroke was moved or resized by `matrix`.
    Moved {
        page: usize,
        id: u32,
        matrix: pdf::Matrix,
    },
    /// A page was put in, taken out or moved. A delete keeps the document from the other side of
    /// it, which Undo and Redo swap in. `id` names the step to whatever keeps something of its
    /// own about it: the window keeps the note links a delete left naming the page it took out.
    Paged {
        edit: PageEdit,
        kept: Option<Box<Kept>>,
        id: u32,
    },
}

/// The document as it stood on the other side of a page delete, and the names its annotations
/// had there: before the delete until Undo swaps it in for the document as it then stands, which
/// waits here in turn for Redo. Swapped whole, so the steps on either side find their strokes
/// where they left them, and what an Export Highlights wrote on one side stays on that side.
pub struct Kept {
    doc: PdfDoc,
    ids: HashMap<usize, Vec<Option<u32>>>,
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

/// What the render thread knows about the ink of the pages it has touched or listed, so that the
/// tools can name a stroke wherever it has moved to, and Undo and Redo reach this session's
/// changes and nothing else.
///
/// `ids` mirrors a page's `/Annots` from the first time it is touched or listed: `Some` is an
/// annotation that has been given a name, `None` one nobody has asked about. It is filed under
/// the page's number today, and follows every page edit. `done` is the undo list and `undone`
/// the redo list, newest last, one entry per gesture: the strokes one eraser drag takes come back
/// together.
#[derive(Default)]
pub struct Ink {
    ids: HashMap<usize, Vec<Option<u32>>>,
    done: Vec<Vec<Step>>,
    undone: Vec<Vec<Step>>,
    pub dirty: bool,
}

impl Ink {
    /// A page carrying `count` annotations is about to be touched or listed: mirror it the first
    /// time, and after that account for any that joined its end without us — an export appends
    /// its highlights there.
    pub fn note(&mut self, page: usize, count: usize) {
        let slots = self.ids.entry(page).or_default();
        if slots.len() < count {
            slots.resize(count, None);
        }
    }

    /// The id of the annotation at `index` of a noted page, giving it one if it has none yet.
    pub fn id_at(&mut self, page: usize, index: usize) -> Option<u32> {
        if let Some(id) = *self.ids.get(&page)?.get(index)? {
            return Some(id);
        }
        let id = fresh_id();
        self.ids.get_mut(&page)?[index] = Some(id);
        Some(id)
    }

    /// Where the annotation with this id sits in `page`'s `/Annots` today.
    pub fn index_of(&self, page: usize, id: u32) -> Option<usize> {
        self.ids
            .get(&page)?
            .iter()
            .position(|slot| *slot == Some(id))
    }

    /// The annotation at `index` left `page`, and everything after it moved up one. Its id — a
    /// fresh one, if it had none.
    pub fn removed(&mut self, page: usize, index: usize) -> u32 {
        let slots = self.ids.get_mut(&page).filter(|s| index < s.len());
        let had = slots.and_then(|s| s.remove(index));
        had.unwrap_or_else(fresh_id)
    }

    /// The annotation named `id` went onto the end of `page`'s `/Annots`.
    pub fn appended(&mut self, page: usize, id: u32) {
        self.ids.entry(page).or_default().push(Some(id));
    }

    /// The annotation at `index` was drawn again at the end of `page`, keeping its identity.
    /// Which id it has now.
    pub fn requeued(&mut self, page: usize, index: usize) -> u32 {
        let id = self.removed(page, index);
        self.appended(page, id);
        id
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

    /// A stroke of ours went onto the end of `page`, which carried `before` annotations.
    pub fn drew(&mut self, page: usize, before: usize) {
        self.note(page, before);
        let id = fresh_id();
        self.appended(page, id);
        self.record(
            Step::Drawn {
                page,
                id,
                kept: None,
            },
            false,
        );
    }

    /// The stroke at `index` was erased — `was` being what it drew — and `left` drawn in its
    /// place under the names given: what a partial eraser leaves of it, nothing for a whole one.
    /// Undo takes the pieces off and puts the stroke back as one step.
    pub fn erased(
        &mut self,
        page: usize,
        index: usize,
        was: pdf::Drawn,
        left: Vec<(u32, pdf::Drawn)>,
        joined: bool,
    ) {
        let id = self.removed(page, index);
        let kept = Some(was);
        self.record(Step::Erased { page, id, kept }, joined);
        for (id, piece) in left {
            self.appended(page, id);
            let kept = Some(piece);
            self.record(Step::Drawn { page, id, kept }, true);
        }
    }

    /// The stroke at `index` was moved by `matrix`, which drew it again at the end of `page`.
    pub fn moved(&mut self, page: usize, index: usize, matrix: pdf::Matrix) {
        let id = self.requeued(page, index);
        self.record(Step::Moved { page, id, matrix }, false);
    }

    /// Put a page in, take one out or move one, as a step of its own: Undo takes it back as it
    /// takes back a stroke. A delete first keeps the whole document ([`PdfDoc::snapshot`]), so
    /// its Undo brings the page back as itself, with its ink and with what points at it. Nothing
    /// reaches the disk here: [`PdfDoc::save`] is the second half, as it is for ink. The step's
    /// id, which Undo and Redo hand back with it.
    pub fn edit_pages(&mut self, doc: &mut PdfDoc, edit: PageEdit) -> Result<u32> {
        let kept = match edit {
            PageEdit::Delete(_) => Some(Box::new(Kept {
                doc: doc.snapshot()?,
                ids: self.ids.clone(),
            })),
            PageEdit::Insert(_) | PageEdit::Move { .. } => None,
        };
        self.edit(doc, edit)?;
        let id = fresh_id();
        self.record(Step::Paged { edit, kept, id }, false);
        Ok(id)
    }

    /// Make one page edit, a page put in being a blank one. What the ledger knows of every other
    /// page follows it to its new number.
    fn edit(&mut self, doc: &mut PdfDoc, edit: PageEdit) -> Result<()> {
        match edit {
            PageEdit::Insert(at) => doc.insert_page(at)?,
            PageEdit::Delete(page) => doc.delete_page(page)?,
            PageEdit::Move { from, to } => doc.move_page(from, to)?,
        }
        self.ids = std::mem::take(&mut self.ids)
            .into_iter()
            .filter_map(|(page, slots)| Some((edit.map(page)?, slots)))
            .collect();
        Ok(())
    }

    /// Whether Undo, and then Redo, has anything to walk.
    pub fn history(&self) -> (bool, bool) {
        (!self.done.is_empty(), !self.undone.is_empty())
    }

    /// Every ink stroke of `page`, each under the id the tools will name it by.
    pub fn named(&mut self, doc: &PdfDoc, page: usize) -> Vec<NamedInk> {
        self.note(page, doc.annotation_count(page).unwrap_or(0));
        doc.inks(page)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|shape| Some((self.id_at(page, shape.index)?, shape)))
            .collect()
    }

    /// Walk the last gesture back (Undo), or the last one Undo took forward again (Redo), and
    /// say what each of its steps changed.
    ///
    /// A step that cannot be made — pdfium refusing, or a stroke the ledger lost track of — drops
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
    /// by the map or by its inverse, or the pages are edited or edited back. A delete and its
    /// Undo swap the document for the one kept from the other side of it. The page an insert put
    /// in is blank again when its Undo takes it out, every step on it having been walked back
    /// first, so a blank one is what Redo puts in.
    fn apply(&mut self, doc: &mut PdfDoc, step: &mut Step, forwards: bool) -> Result<Walked> {
        match (step, forwards) {
            (Step::Paged { edit, kept, id }, _) => {
                let made = match forwards {
                    true => *edit,
                    false => edit.inverse(),
                };
                match kept {
                    Some(kept) => {
                        std::mem::swap(doc, &mut kept.doc);
                        std::mem::swap(&mut self.ids, &mut kept.ids);
                    }
                    None => self.edit(doc, made)?,
                }
                Ok(Walked::Pages(made, *id))
            }
            (Step::Moved { page, id, matrix }, _) => {
                let index = self.locate(doc, *page, *id)?;
                let m = match forwards {
                    true => *matrix,
                    false => pdf::invert(*matrix),
                };
                let area = doc.transform_ink(*page, index, m)?;
                self.requeued(*page, index);
                Ok(Walked::Ink(*page, area))
            }
            // Drawing forwards puts a stroke on, and so does erasing backwards.
            (Step::Drawn { page, id, kept }, true) | (Step::Erased { page, id, kept }, false) => {
                self.note(*page, doc.annotation_count(*page)?);
                let drawn = kept
                    .as_ref()
                    .ok_or_else(|| anyhow!("nothing kept of {id}"))?;
                let area = doc.redraw_ink(*page, drawn)?;
                self.appended(*page, *id);
                Ok(Walked::Ink(*page, area))
            }
            (Step::Drawn { page, id, kept }, false) | (Step::Erased { page, id, kept }, true) => {
                let index = self.locate(doc, *page, *id)?;
                let (drawn, area) = doc.take_ink(*page, index)?;
                self.removed(*page, index);
                *kept = Some(drawn);
                Ok(Walked::Ink(*page, area))
            }
        }
    }

    /// Where the stroke named `id` sits in `page`'s `/Annots` today, the page mirrored first.
    fn locate(&mut self, doc: &PdfDoc, page: usize, id: u32) -> Result<usize> {
        self.note(page, doc.annotation_count(page)?);
        self.index_of(page, id)
            .ok_or_else(|| anyhow!("stroke {id} is not on page {page}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::PageEdit;
    use crate::pdf::tests::{open_tiny, page_texts, reopen};

    /// A stroke's id follows it through the moves and erases that shuffle `/Annots`, and the id
    /// of one that is gone names nothing — which is what keeps a list that has not caught up
    /// from erasing the stroke after the one it meant.
    #[test]
    fn an_id_follows_its_stroke_and_a_gone_one_names_nothing() {
        let mut ink = Ink::default();
        // A page that already carried three annotations, the first and last of them strokes.
        ink.note(0, 3);
        let (a, c) = (ink.id_at(0, 0).unwrap(), ink.id_at(0, 2).unwrap());
        assert_eq!(ink.id_at(0, 0), Some(a), "asked again, the same name");

        // `a` is moved, which puts it at the end; then `c` is erased.
        ink.moved(0, 0, pdf::IDENTITY);
        assert_eq!((ink.index_of(0, a), ink.index_of(0, c)), (Some(2), Some(1)));
        ink.removed(0, 1);
        assert_eq!((ink.index_of(0, a), ink.index_of(0, c)), (Some(1), None));

        // An export's highlight joins the end unseen, and the next stroke counts it.
        ink.drew(0, 3);
        let drawn = newest(&ink, 0);
        assert_eq!(ink.index_of(0, drawn), Some(3), "{:?}", ink.ids);
        // A page nobody touched names nothing, whatever it carries.
        assert_eq!(ink.index_of(9, a), None);
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
        doc.add_shape(0, line(40.0), style).unwrap();
        ink.drew(0, 1);
        let at = ink.index_of(0, newest(&ink, 0)).unwrap();
        let down = [1.0, 0.0, 0.0, 1.0, 0.0, 10.0];
        doc.transform_ink(0, at, down).unwrap();
        ink.moved(0, at, down);
        assert_eq!(lines(&doc), [20, 50]);
        // One drag takes both, so the eraser's list names the file's own too.
        let named: Vec<u32> = ink.named(&doc, 0).iter().map(|(id, _)| *id).collect();
        for (n, id) in named.into_iter().enumerate() {
            let at = ink.index_of(0, id).unwrap();
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
        let before = doc.annotation_count(0).unwrap();
        doc.add_shape(0, line(60.0), style).unwrap();
        ink.drew(0, before);
        assert_eq!(lines(&doc), [20, 50, 60]);
        assert_eq!(ink.history(), (true, false));
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
        let insert = ink.edit_pages(&mut doc, PageEdit::Insert(1)).unwrap();
        let moved = ink
            .edit_pages(&mut doc, PageEdit::Move { from: 2, to: 0 })
            .unwrap();
        assert_eq!(page_texts(&doc), ["Second page", "Hello accent", ""]);

        let undo = PageEdit::Move { from: 0, to: 2 };
        assert_eq!(ink.walk(&mut doc, false), [Walked::Pages(undo, moved)]);
        assert_eq!(page_texts(&doc), ["Hello accent", "", "Second page"]);
        let undo = PageEdit::Delete(1);
        assert_eq!(ink.walk(&mut doc, false), [Walked::Pages(undo, insert)]);
        assert_eq!(page_texts(&doc), ["Hello accent", "Second page"]);
        assert_eq!(ink.history(), (false, true));

        let redo = PageEdit::Insert(1);
        assert_eq!(ink.walk(&mut doc, true), [Walked::Pages(redo, insert)]);
        ink.walk(&mut doc, true);
        assert_eq!(page_texts(&doc), ["Second page", "Hello accent", ""]);
        assert_eq!(ink.history(), (true, false));
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
            let before = doc.annotation_count(page).unwrap();
            let line = pdf::Shape::Line {
                a: (20.0, 20.0),
                b: (80.0, 20.0),
            };
            doc.add_shape(page, line, STYLE).unwrap();
            ink.drew(page, before);
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
        ink.edit_pages(&mut doc, PageEdit::Delete(0)).unwrap();
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
            ink.edit_pages(&mut doc, PageEdit::Delete(page)).unwrap();
            ink.walk(&mut doc, false);
            assert_eq!(targets(&doc), before, "page {page} put back");
            ink.walk(&mut doc, true);
            assert_eq!(doc.page_count(), 1, "page {page} taken out again");
            ink.walk(&mut doc, false);
        }
        assert_eq!(targets(&reopen(&dir, &doc)), before);

        // An Export Highlights made since a delete goes with its Undo and comes back with Redo.
        ink.edit_pages(&mut doc, PageEdit::Delete(1)).unwrap();
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
        assert_eq!(doc.annotation_count(0).unwrap(), had);
        ink.walk(&mut doc, true);
        assert_eq!(doc.annotation_count(0).unwrap(), had + 1);
    }

    const STYLE: pdf::InkStyle = pdf::InkStyle {
        width: 2.0,
        rgba: [0, 0, 0, 255],
        multiply: false,
    };

    /// The name of the annotation last put on the end of `page`.
    fn newest(ink: &Ink, page: usize) -> u32 {
        ink.ids[&page].last().copied().flatten().unwrap()
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
        doc.add_shape(0, line, style).unwrap();
        ink.drew(0, 0);
        let at = ink.index_of(0, newest(&ink, 0)).unwrap();
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
        assert!(names.iter().all(|id| ink.index_of(0, *id).is_some()));
        // Back past the cut and the stroke it was made in.
        ink.walk(&mut doc, false);
        ink.walk(&mut doc, false);
        assert!(spans(&doc).is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
