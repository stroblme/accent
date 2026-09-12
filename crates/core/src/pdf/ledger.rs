//! The ink undo ledger: what this session drew, erased and moved on a document's pages.
//!
//! It lives here rather than in a viewer because both viewers need it — the GTK render thread
//! and the Android session — and because it is pure bookkeeping over [`PdfDoc`]: no widget, no
//! channel, no thread. What a viewer adds on top is where the work runs, not what it means.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Result, anyhow};

use crate::pdf::{self, PdfDoc};

/// One ink stroke as the tools address it: the id it was given, which unlike its place in
/// `/Annots` survives every erase and move made before it lands, and its shape.
pub type NamedInk = (u32, pdf::InkShape);

/// A name no stroke in the process has had. One counter for every thread, so a widget can name
/// the pieces its eraser cuts before the document has seen them.
pub fn fresh_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One change this session made to a page's ink, for Undo to walk back and Redo to walk forward
/// again. Each names its annotation by an id rather than an index, because every erase and every
/// move shuffles the indices.
#[derive(Debug, Clone)]
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
}

impl Step {
    /// The page it changed and the stroke it names.
    fn at(&self) -> (usize, u32) {
        match *self {
            Step::Drawn { page, id, .. }
            | Step::Erased { page, id, .. }
            | Step::Moved { page, id, .. } => (page, id),
        }
    }
}

/// What the render thread knows about the ink of the pages it has touched or listed, so that the
/// tools can name a stroke wherever it has moved to, and Undo and Redo reach this session's
/// changes and nothing else.
///
/// `ids` mirrors a page's `/Annots` from the first time it is touched or listed: `Some` is an
/// annotation that has been given a name, `None` one nobody has asked about. `done` is the undo
/// list and `undone` the redo list, newest last, one entry per gesture: the strokes one eraser
/// drag takes come back together.
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
    /// say which page each of its steps changed and how much of it.
    ///
    /// A step that cannot be made — pdfium refusing, or a stroke the ledger lost track of — drops
    /// its gesture from the history, after whatever steps of it came first.
    pub fn walk(&mut self, doc: &mut PdfDoc, forwards: bool) -> Vec<(usize, pdf::Rect)> {
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
                Ok(area) => changed.push((step.at().0, area)),
                Err(e) => {
                    tracing::warn!("walking the ink history on page {}: {e:#}", step.at().0);
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

    /// Make one step (`forwards`) or take it back: a stroke goes on or comes off the page, or
    /// moves by the map or by its inverse. How much of the page that changed.
    fn apply(&mut self, doc: &mut PdfDoc, step: &mut Step, forwards: bool) -> Result<pdf::Rect> {
        let (page, id) = step.at();
        self.note(page, doc.annotation_count(page)?);
        let index = self.index_of(page, id);
        let gone = || anyhow!("stroke {id} is not on page {page}");
        match (step, forwards) {
            (Step::Moved { matrix, .. }, _) => {
                let index = index.ok_or_else(gone)?;
                let m = match forwards {
                    true => *matrix,
                    false => pdf::invert(*matrix),
                };
                let area = doc.transform_ink(page, index, m)?;
                self.requeued(page, index);
                Ok(area)
            }
            // Drawing forwards puts a stroke on, and so does erasing backwards.
            (Step::Drawn { kept, .. }, true) | (Step::Erased { kept, .. }, false) => {
                let drawn = kept
                    .as_ref()
                    .ok_or_else(|| anyhow!("nothing kept of {id}"))?;
                let area = doc.redraw_ink(page, drawn)?;
                self.appended(page, id);
                Ok(area)
            }
            (Step::Drawn { kept, .. }, false) | (Step::Erased { kept, .. }, true) => {
                let index = index.ok_or_else(gone)?;
                let (drawn, area) = doc.take_ink(page, index)?;
                self.removed(page, index);
                *kept = Some(drawn);
                Ok(area)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::write(&path, pdf::blank_pdf().unwrap()).unwrap();
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
        std::fs::write(&path, pdf::blank_pdf().unwrap()).unwrap();
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
