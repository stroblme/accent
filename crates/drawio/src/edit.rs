//! Changing a diagram, with undo.

use std::collections::{HashMap, HashSet};

use crate::Error;
use crate::geom::{Point, Rect};
use crate::model::{Cell, CellId, File, Geometry, Page, guid};
use crate::scene::Scene;
use crate::style::Style;

/// Where [`Editor::reorder`] moves cells among their siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZOrder {
    ToFront,
    ToBack,
    Forward,
    Backward,
}

/// Undo steps kept, as many as draw.io keeps (`mxUndoManager`'s default size).
const UNDO_LIMIT: usize = 100;

/// A file being edited: every change goes through here, is one undo step, and marks the file
/// dirty until [`Editor::mark_saved`].
#[derive(Debug, Clone)]
pub struct Editor {
    file: File,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    dirty: bool,
    ids: Ids,
}

/// What an undo or redo step puts back: one page, or the whole page list when pages were added
/// or removed.
// ponytail: snapshots clone a whole page, fine at a few thousand cells for the hundred steps
// kept; command objects are the upgrade when memory or diffing matters.
#[derive(Debug, Clone)]
enum Snapshot {
    Page { index: usize, page: Page },
    Pages(Vec<Page>),
}

/// New cell ids in draw.io's form, `<guid>-<n>`: `mxGraphModel.createId` behind the
/// `Editor.guid()` prefix draw.io gives each model.
#[derive(Debug, Clone)]
struct Ids {
    prefix: String,
    next: u64,
}

impl Ids {
    /// The next id that is not already on `page`.
    fn fresh(&mut self, page: &Page) -> CellId {
        loop {
            let id = format!("{}-{}", self.prefix, self.next);
            self.next += 1;
            if page.cell(&id).is_none() {
                return id;
            }
        }
    }
}

impl Editor {
    pub fn new(file: File) -> Editor {
        Editor {
            file,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            ids: Ids {
                prefix: guid(),
                next: 1,
            },
        }
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn page(&self, i: usize) -> Result<&Page, Error> {
        self.file.page(i)
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Take back the last change; `false` when there is none.
    pub fn undo(&mut self) -> bool {
        let Some(step) = self.undo.pop() else {
            return false;
        };
        let back = self.swap(step);
        self.redo.push(back);
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(step) = self.redo.pop() else {
            return false;
        };
        let back = self.swap(step);
        self.undo.push(back);
        self.dirty = true;
        true
    }

    /// A vertex at `rect` (absolute), inside `parent` or the page's first unlocked layer.
    pub fn add_vertex(
        &mut self,
        page: usize,
        parent: Option<&str>,
        rect: Rect,
        style: &str,
        label: &str,
    ) -> Result<CellId, Error> {
        self.edit(page, |p, ids| {
            let parent = match parent {
                Some(id) => check(p, [id]).map(|()| id.to_string())?,
                None => default_layer(p)?,
            };
            // A layer has no rectangle and places its children from the page origin.
            let origin = p.absolute_rect(&parent).unwrap_or_default();
            let id = ids.fresh(p);
            let local = rect.translate(-origin.x, -origin.y);
            p.cells
                .push(Cell::new_vertex(&id, &parent, local, style, label));
            Ok(id)
        })
    }

    /// An edge in the page's first unlocked layer. An end without a cell dangles at its point;
    /// an end with one keeps the point too, as draw.io writes it.
    pub fn add_edge(
        &mut self,
        page: usize,
        source: (Option<&str>, Point),
        target: (Option<&str>, Point),
        style: &str,
    ) -> Result<CellId, Error> {
        self.edit(page, |p, ids| {
            check(p, [source.0, target.0].into_iter().flatten())?;
            let layer = default_layer(p)?;
            let id = ids.fresh(p);
            p.cells
                .push(Cell::new_edge(&id, &layer, source, target, style));
            Ok(id)
        })
    }

    /// [`move_cells`] on page `page`, drawn as `drawn`.
    pub fn move_cells(
        &mut self,
        page: usize,
        ids: &[CellId],
        dx: f64,
        dy: f64,
        drawn: &Scene,
    ) -> Result<(), Error> {
        self.edit(page, |p, _| move_cells(p, ids, dx, dy, drawn))
    }

    /// [`resize`] on page `page`.
    pub fn resize(&mut self, page: usize, id: &str, rect: Rect) -> Result<(), Error> {
        self.edit(page, |p, _| resize(p, id, rect))
    }

    /// Set (or with `None` remove) one style key on every cell in `ids`, as one step.
    pub fn set_style(
        &mut self,
        page: usize,
        ids: &[CellId],
        key: &str,
        value: Option<&str>,
    ) -> Result<(), Error> {
        self.set_styles(page, ids, &[(key, value)])
    }

    /// [`set_styles`] on page `page`, as one step.
    pub fn set_styles(
        &mut self,
        page: usize,
        ids: &[CellId],
        pairs: &[(&str, Option<&str>)],
    ) -> Result<(), Error> {
        self.edit(page, |p, _| set_styles(p, ids, pairs))
    }

    /// Replace a cell's whole style string.
    pub fn set_style_string(&mut self, page: usize, id: &str, style: &str) -> Result<(), Error> {
        self.edit(page, |p, _| {
            cell_mut(p, id)?.style = Style::parse(style);
            Ok(())
        })
    }

    /// Set a label from the editor's Markdown: stored as HTML with `html=1`.
    pub fn set_label_markdown(
        &mut self,
        page: usize,
        id: &str,
        markdown: &str,
    ) -> Result<(), Error> {
        let html = crate::label::markdown_to_html(markdown);
        self.edit(page, |p, _| {
            let cell = cell_mut(p, id)?;
            cell.set_label(&html);
            cell.style.set("html", Some("1"));
            Ok(())
        })
    }

    /// Remove cells, everything under them and every edge left without an end. The root and
    /// the layers are not removed this way.
    pub fn delete(&mut self, page: usize, ids: &[CellId]) -> Result<(), Error> {
        self.edit(page, |p, _| {
            check(p, ids)?;
            let kept: HashSet<&str> = p
                .root()
                .into_iter()
                .chain(p.layers())
                .map(|c| c.id.as_str())
                .collect();
            let chosen = ids.iter().filter(|id| !kept.contains(id.as_str()));
            let mut gone = with_subtrees(p, chosen.cloned());
            // An edge can end on another edge, so removing one can leave the next without an end.
            loop {
                let cut = |end: &Option<CellId>| end.as_ref().is_some_and(|id| gone.contains(id));
                let loose: Vec<CellId> = p
                    .cells
                    .iter()
                    .filter(|c| !gone.contains(&c.id) && (cut(&c.source) || cut(&c.target)))
                    .map(|c| c.id.clone())
                    .collect();
                if loose.is_empty() {
                    break;
                }
                gone = with_subtrees(p, gone.into_iter().chain(loose));
            }
            p.cells.retain(|c| !gone.contains(&c.id));
            Ok(())
        })
    }

    /// Copies of cells and their subtrees, 10 units down and right, edges between copied cells
    /// reconnected to the copies. Returns the new top-level ids.
    pub fn duplicate(&mut self, page: usize, ids: &[CellId]) -> Result<Vec<CellId>, Error> {
        self.edit(page, |p, new_ids| {
            check(p, ids)?;
            let top = topmost(p, ids);
            let copied = with_subtrees(p, top.iter().cloned());
            let fresh: HashMap<CellId, CellId> = p
                .cells
                .iter()
                .filter(|c| copied.contains(&c.id))
                .map(|c| (c.id.clone(), new_ids.fresh(p)))
                .collect();
            // Inside the copied set a reference goes to the copy; outside it stays.
            let remap = |id: &mut Option<CellId>| {
                if let Some(new) = id.as_ref().and_then(|old| fresh.get(old)) {
                    *id = Some(new.clone());
                }
            };
            // Each copy goes right after the last cell of its original's subtree.
            let mut blocks = Vec::new();
            for original in &top {
                let subtree = with_subtrees(p, [original.clone()]);
                let (mut end, mut block) = (0, Vec::new());
                for (i, cell) in p.cells.iter().enumerate() {
                    if !subtree.contains(&cell.id) {
                        continue;
                    }
                    let mut copy = cell.clone();
                    copy.id = fresh[&cell.id].clone();
                    remap(&mut copy.parent);
                    remap(&mut copy.source);
                    remap(&mut copy.target);
                    if &cell.id == original
                        && let Some(g) = &mut copy.geometry
                    {
                        translate(g, 10.0, 10.0);
                    }
                    block.push(copy);
                    end = i + 1;
                }
                blocks.push((end, block));
            }
            // The last place first, so that inserting leaves the earlier places where they are.
            blocks.sort_by_key(|(end, _)| std::cmp::Reverse(*end));
            for (end, block) in blocks {
                p.cells.splice(end..end, block);
            }
            Ok(top.iter().map(|id| fresh[id].clone()).collect())
        })
    }

    /// Move each cell, with everything under it, among its siblings: above or below all of
    /// them, or past one. Cells moved together keep their order and do not pass each other.
    pub fn reorder(&mut self, page: usize, ids: &[CellId], z: ZOrder) -> Result<(), Error> {
        self.edit(page, |p, _| {
            check(p, ids)?;
            let chosen: HashSet<&str> = ids.iter().map(String::as_str).collect();
            let up = matches!(z, ZOrder::ToFront | ZOrder::Forward);
            let mut order: Vec<CellId> = p
                .cells
                .iter()
                .filter(|c| chosen.contains(c.id.as_str()))
                .map(|c| c.id.clone())
                .collect();
            // The cell nearest to where they go moves first.
            if up {
                order.reverse();
            }
            for id in order {
                let at = p.index_of(&id).expect("checked above");
                let parent = p.cells[at].parent.clone();
                // The siblings on the side it moves to, bottom first.
                let mut passed = p.cells.iter().enumerate().filter_map(|(i, c)| {
                    let side = if up { i > at } else { i < at };
                    let sibling = c.parent == parent && !chosen.contains(c.id.as_str());
                    (side && sibling).then(|| c.id.clone())
                });
                let past = match z {
                    ZOrder::ToFront | ZOrder::Backward => passed.next_back(),
                    ZOrder::ToBack | ZOrder::Forward => passed.next(),
                };
                let Some(past) = past else {
                    continue;
                };
                let block = with_subtrees(p, [id]);
                let (moving, rest): (Vec<Cell>, Vec<Cell>) = std::mem::take(&mut p.cells)
                    .into_iter()
                    .partition(|c| block.contains(&c.id));
                p.cells = rest;
                let to = if up {
                    let over = with_subtrees(p, [past]);
                    p.cells
                        .iter()
                        .rposition(|c| over.contains(&c.id))
                        .map_or(p.cells.len(), |i| i + 1)
                } else {
                    p.index_of(&past).unwrap_or(0)
                };
                p.cells.splice(to..to, moving);
            }
            Ok(())
        })
    }

    /// Set (or remove) a page attribute: `pageWidth`, `pageHeight`, `background`, `gridSize`.
    pub fn set_page_attr(
        &mut self,
        page: usize,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), Error> {
        self.edit(page, |p, _| {
            p.set_model_attr(key, value);
            Ok(())
        })
    }

    /// A blank page at the end; returns its index.
    pub fn add_page(&mut self, name: &str) -> usize {
        self.edit_pages(|pages| {
            pages.push(Page::blank(name, &guid()));
            pages.len() - 1
        })
    }

    pub fn rename_page(&mut self, page: usize, name: &str) -> Result<(), Error> {
        self.edit(page, |p, _| {
            p.set_name(name);
            Ok(())
        })
    }

    /// Refused for the last page: a file always has one.
    pub fn delete_page(&mut self, page: usize) -> Result<(), Error> {
        self.file.page(page)?;
        if self.file.pages.len() == 1 {
            return Err(Error::Refused("a diagram keeps at least one page"));
        }
        self.edit_pages(|pages| pages.remove(page));
        Ok(())
    }

    /// Change a copy of page `index` and put it in place if `change` succeeds and changed
    /// something, the page as it was becoming the undo step.
    fn edit<T>(
        &mut self,
        index: usize,
        change: impl FnOnce(&mut Page, &mut Ids) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut page = self.file.page(index)?.clone();
        let out = change(&mut page, &mut self.ids)?;
        if page != self.file.pages[index] {
            let before = std::mem::replace(&mut self.file.pages[index], page);
            self.record(Snapshot::Page {
                index,
                page: before,
            });
        }
        Ok(out)
    }

    /// The same for the page list, for changes that cannot fail.
    fn edit_pages<T>(&mut self, change: impl FnOnce(&mut Vec<Page>) -> T) -> T {
        let mut pages = self.file.pages.clone();
        let out = change(&mut pages);
        if pages != self.file.pages {
            let before = std::mem::replace(&mut self.file.pages, pages);
            self.record(Snapshot::Pages(before));
        }
        out
    }

    fn record(&mut self, before: Snapshot) {
        if self.undo.len() == UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
        self.dirty = true;
    }

    /// Put `step` in place and return what it replaced, which is the step back.
    fn swap(&mut self, step: Snapshot) -> Snapshot {
        match step {
            Snapshot::Page { index, page } => Snapshot::Page {
                index,
                page: std::mem::replace(&mut self.file.pages[index], page),
            },
            Snapshot::Pages(pages) => {
                Snapshot::Pages(std::mem::replace(&mut self.file.pages, pages))
            }
        }
    }
}

// The edits a drag previews, as functions of a page: the editor applies them to the page in
// the file as one undo step, and the canvas to a copy of it while the drag is under way.

/// Move cells by (`dx`, `dy`): a vertex by its position, an edge by its waypoints and end
/// points. A cell inside another that moves goes with it, and so does an edge whose both ends
/// move. A label on an edge stays where it is along the edge. An edge moved without the shape
/// one of its ends is on lets go of that shape, the end staying where it is drawn and moving
/// with the rest, as mxGraph's `disconnectOnMove` has it; `drawn` is the page's display list,
/// which says where the ends are.
pub fn move_cells(
    page: &mut Page,
    ids: &[CellId],
    dx: f64,
    dy: f64,
    drawn: &Scene,
) -> Result<(), Error> {
    start_move(page, ids, drawn)?.shift(page, dx, dy);
    Ok(())
}

/// The first half of [`move_cells`], the part that does not depend on how far: check `ids` and
/// let the edges among them go of the shapes they leave, where `drawn` has their ends. A drag
/// does it once and [`Moving::shift`]s a copy of the result on each frame.
pub fn start_move(page: &mut Page, ids: &[CellId], drawn: &Scene) -> Result<Moving, Error> {
    check(page, ids)?;
    let top: HashSet<CellId> = topmost(page, ids).into_iter().collect();
    let moved = with_subtrees(page, top.iter().cloned());
    disconnect(page, &top, &moved, drawn);
    let moves = |end: &Option<CellId>| end.as_ref().is_some_and(|id| moved.contains(id));
    let between = page
        .cells
        .iter()
        .filter(|c| c.edge && !moved.contains(&c.id) && moves(&c.source) && moves(&c.target));
    let between: Vec<CellId> = between.map(|c| c.id.clone()).collect();
    Ok(Moving {
        count: moved.len(),
        shifted: top.into_iter().chain(between).collect(),
    })
}

/// A move [`start_move`] prepared.
#[derive(Debug, Clone)]
pub struct Moving {
    /// The cells whose geometry a move shifts: the topmost moved cells, and the edges between
    /// two of them.
    shifted: HashSet<CellId>,
    /// How many cells move, those inside the moved ones included.
    pub count: usize,
}

impl Moving {
    /// The second half of [`move_cells`]: shift the moved cells by (`dx`, `dy`).
    pub fn shift(&self, page: &mut Page, dx: f64, dy: f64) {
        for cell in &mut page.cells {
            if self.shifted.contains(&cell.id)
                && let Some(g) = &mut cell.geometry
            {
                translate(g, dx, dy);
            }
        }
    }
}

/// Give vertex `id` the absolute rectangle `rect`.
pub fn resize(page: &mut Page, id: &str, rect: Rect) -> Result<(), Error> {
    check(page, [id])?;
    page.absolute_rect(id)
        .ok_or(Error::Refused("only a shape can be resized"))?;
    let origin = page.origin_of(id);
    if let Some(g) = page.cell_mut(id).and_then(|c| c.geometry.as_mut()) {
        (g.x, g.y) = (rect.x - origin.x, rect.y - origin.y);
        (g.width, g.height) = (rect.w, rect.h);
    }
    Ok(())
}

/// Set (or with `None` remove) several style keys on every cell in `ids`: a connector's route
/// is two of them.
pub fn set_styles(
    page: &mut Page,
    ids: &[CellId],
    pairs: &[(&str, Option<&str>)],
) -> Result<(), Error> {
    for id in ids {
        let style = &mut cell_mut(page, id)?.style;
        for (key, value) in pairs {
            style.set(key, *value);
        }
    }
    Ok(())
}

/// `NoCell` for the first of `ids` that is not on `page`.
fn check<S: AsRef<str>>(page: &Page, ids: impl IntoIterator<Item = S>) -> Result<(), Error> {
    for id in ids {
        let id = id.as_ref();
        if page.cell(id).is_none() {
            return Err(Error::NoCell(id.to_string()));
        }
    }
    Ok(())
}

fn cell_mut<'a>(page: &'a mut Page, id: &str) -> Result<&'a mut Cell, Error> {
    page.cell_mut(id)
        .ok_or_else(|| Error::NoCell(id.to_string()))
}

/// The layer new cells go into ([`Page::default_parent`]), refused when there is none.
fn default_layer(page: &Page) -> Result<CellId, Error> {
    match page.default_parent() {
        Some(id) => Ok(id.to_string()),
        None if page.layers().is_empty() => Err(Error::Refused("the page has no layer")),
        None => Err(Error::Refused("every layer on this page is locked")),
    }
}

/// `roots` and every cell under them. A file in draw.io's order, parents before children,
/// takes one pass over the cells; the loop is for any other order.
fn with_subtrees(page: &Page, roots: impl IntoIterator<Item = CellId>) -> HashSet<CellId> {
    let mut set: HashSet<CellId> = roots.into_iter().collect();
    loop {
        let size = set.len();
        for cell in &page.cells {
            if !set.contains(&cell.id) && cell.parent.as_ref().is_some_and(|p| set.contains(p)) {
                set.insert(cell.id.clone());
            }
        }
        if set.len() == size {
            return set;
        }
    }
}

/// The cells of `ids` that are not inside another of them, in document order.
fn topmost(page: &Page, ids: &[CellId]) -> Vec<CellId> {
    let chosen: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let children = page
        .cells
        .iter()
        .filter(|c| c.parent.as_deref().is_some_and(|p| chosen.contains(p)));
    let inner = with_subtrees(page, children.map(|c| c.id.clone()));
    page.cells
        .iter()
        .filter(|c| chosen.contains(c.id.as_str()) && !inner.contains(&c.id))
        .map(|c| c.id.clone())
        .collect()
}

/// Let go of the shapes the edges among `top` leave behind: each end whose shape is not in
/// `moved` becomes a point of its own where `drawn` has the end now, for the move to take along.
/// Its `exit`/`entry` keys stay, as draw.io leaves them.
// mxGraph.disconnectGraph, mxGraph.js 7459-7535; mxGraph.disconnectOnMove, 1557
fn disconnect(page: &mut Page, top: &HashSet<CellId>, moved: &HashSet<CellId>, drawn: &Scene) {
    let stays = |end: &Option<CellId>| end.as_ref().is_some_and(|id| !moved.contains(id));
    let edges: Vec<CellId> = page
        .cells
        .iter()
        .filter(|c| c.edge && top.contains(&c.id) && (stays(&c.source) || stays(&c.target)))
        .map(|c| c.id.clone())
        .collect();
    if edges.is_empty() {
        return;
    }
    for id in edges {
        // An edge not drawn (on a hidden layer) has no end to keep.
        let Some(route) = drawn.route(&id).filter(|p| p.len() >= 2) else {
            continue;
        };
        let origin = page.origin_of(&id);
        let local = |p: Point| Some(Point::new(p.x - origin.x, p.y - origin.y));
        let Some(cell) = page.cell_mut(&id) else {
            continue;
        };
        let (from, to) = (stays(&cell.source), stays(&cell.target));
        let g = cell.geometry.get_or_insert_with(|| Geometry {
            relative: true,
            ..Geometry::default()
        });
        if from {
            g.source_point = local(route[0]);
        }
        if to {
            g.target_point = local(route[route.len() - 1]);
        }
        if from {
            cell.source = None;
        }
        if to {
            cell.target = None;
        }
    }
}

/// Move a geometry as `mxGeometry.translate` does: a vertex's position, an edge's end points
/// and waypoints. A relative geometry, a label on an edge, keeps its place along the edge.
fn translate(g: &mut Geometry, dx: f64, dy: f64) {
    if !g.relative {
        g.x += dx;
        g.y += dy;
    }
    let waypoints = g.points.iter_mut().flatten();
    for p in waypoints
        .chain(&mut g.source_point)
        .chain(&mut g.target_point)
    {
        p.x += dx;
        p.y += dy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::scene;

    /// An editor on one page holding `cells` after the root and the layer.
    fn open(cells: impl IntoIterator<Item = Cell>) -> Editor {
        let mut page = Page::blank("P", "p");
        page.cells.extend(cells);
        Editor::new(File {
            attrs: Vec::new(),
            pages: vec![page],
        })
    }

    /// Boxes `a` and `b`, joined by `e` through one waypoint.
    fn editor() -> Editor {
        let square = |id, x| Cell::new_vertex(id, "1", Rect::new(x, 0.0, 40.0, 40.0), "", id);
        let mut e = Cell::new_edge(
            "e",
            "1",
            (Some("a"), Point::new(40.0, 20.0)),
            (Some("b"), Point::new(100.0, 20.0)),
            "",
        );
        e.geometry.as_mut().unwrap().points = Some(vec![Point::new(70.0, 60.0)]);
        open([square("a", 0.0), square("b", 100.0), e])
    }

    fn list(ids: &[&str]) -> Vec<CellId> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn order(e: &Editor) -> Vec<&str> {
        e.file.pages[0]
            .cells
            .iter()
            .map(|c| c.id.as_str())
            .collect()
    }

    fn geometry<'a>(e: &'a Editor, id: &str) -> &'a Geometry {
        let cell = e.file.pages[0].cell(id);
        cell.and_then(|c| c.geometry.as_ref()).unwrap()
    }

    #[test]
    fn undo_redo_restore_cells_and_dirty() {
        let mut e = editor();
        let before = e.page(0).unwrap().clone();
        assert!(!e.dirty() && !e.can_undo() && !e.undo());
        let id = e
            .add_vertex(0, None, Rect::new(0.0, 100.0, 10.0, 10.0), "", "C")
            .unwrap();
        assert!(e.dirty() && e.can_undo() && !e.can_redo());
        e.mark_saved();
        assert!(!e.dirty());
        assert!(e.undo());
        assert_eq!(e.page(0).unwrap(), &before);
        assert!(e.dirty() && e.can_redo());
        assert!(e.redo());
        assert!(e.page(0).unwrap().cell(&id).is_some());
        assert!(!e.redo());
        e.undo();
        e.set_style(0, &list(&["a"]), "rounded", Some("1")).unwrap();
        assert!(!e.can_redo(), "a new change drops the steps to redo");
    }

    #[test]
    fn new_ids_are_unique_and_draw_io_shaped() {
        let mut e = editor();
        let taken = format!("{}-1", e.ids.prefix);
        e.file.pages[0]
            .cells
            .push(Cell::new_vertex(&taken, "1", Rect::default(), "", ""));
        let made: Vec<CellId> = (0..3)
            .map(|_| e.add_vertex(0, None, Rect::default(), "", "").unwrap())
            .collect();
        assert!(!made.contains(&taken), "an id on the page is skipped");
        for id in &made {
            let (prefix, n) = id.rsplit_once('-').unwrap();
            let guid_char = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
            assert!(prefix.len() == 20 && prefix.bytes().all(guid_char), "{id}");
            assert!(
                !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()),
                "{id}"
            );
        }
        assert_eq!(made.iter().collect::<HashSet<_>>().len(), made.len());
    }

    #[test]
    fn new_cells_go_into_the_first_unlocked_layer() {
        let locked = |id| Cell {
            style: Style::parse("locked=1;"),
            ..Cell::layer(id, "0")
        };
        let mut e = open([Cell::layer("2", "0"), Cell::layer("3", "0")]);
        e.file.pages[0].cells[1] = locked("1");
        let v = e.add_vertex(0, None, Rect::default(), "", "").unwrap();
        let ends = (None, Point::default());
        let f = e.add_edge(0, ends, ends, "").unwrap();
        let parent = |id: &str| e.page(0).unwrap().cell(id).unwrap().parent.clone();
        assert_eq!(parent(&v).as_deref(), Some("2"));
        assert_eq!(parent(&f).as_deref(), Some("2"));

        let mut e = open(Vec::new());
        e.file.pages[0].cells[1] = locked("1");
        let r = e.add_vertex(0, None, Rect::default(), "", "");
        assert!(matches!(
            r,
            Err(Error::Refused("every layer on this page is locked"))
        ));
        assert!(!e.can_undo());
    }

    #[test]
    fn a_vertex_in_a_group_is_stored_relative() {
        let mut e = editor();
        let group = Rect::new(100.0, 50.0, 200.0, 100.0);
        let g = e.add_vertex(0, None, group, "group", "").unwrap();
        let r = Rect::new(110.0, 70.0, 30.0, 40.0);
        let c = e.add_vertex(0, Some(&g), r, "", "").unwrap();
        assert_eq!(geometry(&e, &c).rect(), Rect::new(10.0, 20.0, 30.0, 40.0));
        assert_eq!(e.page(0).unwrap().absolute_rect(&c), Some(r));
        e.resize(0, &c, Rect::new(120.0, 60.0, 50.0, 50.0)).unwrap();
        assert_eq!(geometry(&e, &c).rect(), Rect::new(20.0, 10.0, 50.0, 50.0));
    }

    #[test]
    fn an_edge_moved_on_its_own_lets_go_of_its_shapes_where_it_is_drawn() {
        let mut e = editor();
        let shown = scene(e.page(0).unwrap());
        let drawn = shown.route("e").unwrap().to_vec();
        e.move_cells(0, &list(&["e"]), 10.0, 5.0, &shown).unwrap();
        let cell = e.page(0).unwrap().cell("e").unwrap();
        assert_eq!(
            (cell.source.as_deref(), cell.target.as_deref()),
            (None, None)
        );
        let g = geometry(&e, "e");
        let shifted = |p: Point| Some(Point::new(p.x + 10.0, p.y + 5.0));
        assert_eq!(g.source_point, shifted(drawn[0]));
        assert_eq!(g.target_point, shifted(drawn[drawn.len() - 1]));
        assert_eq!(g.points, Some(vec![Point::new(80.0, 65.0)]));
        // Moved with one of its shapes, it keeps that end and lets go of the other.
        let mut e = editor();
        let drawn = scene(e.page(0).unwrap());
        e.move_cells(0, &list(&["a", "e"]), 10.0, 0.0, &drawn)
            .unwrap();
        let cell = e.page(0).unwrap().cell("e").unwrap();
        assert_eq!(
            (cell.source.as_deref(), cell.target.as_deref()),
            (Some("a"), None)
        );
    }

    #[test]
    fn a_move_started_once_shifts_as_move_cells_does() {
        let page = editor().page(0).unwrap().clone();
        let ids = list(&["a", "e"]);
        let (mut started, mut whole) = (page.clone(), page.clone());
        let drawn = scene(&page);
        let moving = start_move(&mut started, &ids, &drawn).unwrap();
        assert_eq!(moving.count, 2);
        for (dx, dy) in [(10.0, 5.0), (-30.0, 20.0)] {
            let mut shifted = started.clone();
            moving.shift(&mut shifted, dx, dy);
            let mut moved = page.clone();
            move_cells(&mut moved, &ids, dx, dy, &drawn).unwrap();
            assert_eq!(shifted, moved);
        }
        assert!(matches!(
            start_move(&mut whole, &list(&["nope"]), &drawn),
            Err(Error::NoCell(_))
        ));
    }

    #[test]
    fn move_takes_edges_between_moved_cells() {
        let mut e = editor();
        let c = e
            .add_vertex(0, None, Rect::new(0.0, 200.0, 40.0, 40.0), "", "")
            .unwrap();
        let f = e
            .add_edge(
                0,
                (Some("a"), Point::default()),
                (Some(&c), Point::default()),
                "",
            )
            .unwrap();
        let drawn = scene(e.page(0).unwrap());
        e.move_cells(0, &list(&["a", "b"]), 10.0, 5.0, &drawn)
            .unwrap();
        assert_eq!(geometry(&e, "a").rect(), Rect::new(10.0, 5.0, 40.0, 40.0));
        assert_eq!(geometry(&e, "b").x, 110.0);
        assert_eq!(geometry(&e, "e").points, Some(vec![Point::new(80.0, 65.0)]));
        assert_eq!(
            geometry(&e, &f).source_point,
            Some(Point::default()),
            "an edge with one end moved stays"
        );
        let group = Rect::new(300.0, 0.0, 100.0, 100.0);
        let g = e.add_vertex(0, None, group, "group", "").unwrap();
        let inner = Rect::new(310.0, 10.0, 10.0, 10.0);
        let inner = e.add_vertex(0, Some(&g), inner, "", "").unwrap();
        let drawn = scene(e.page(0).unwrap());
        e.move_cells(0, &[g.clone(), inner.clone()], 5.0, 0.0, &drawn)
            .unwrap();
        assert_eq!(geometry(&e, &g).x, 305.0);
        assert_eq!(
            geometry(&e, &inner).x,
            10.0,
            "a child moves with its group, not again"
        );
    }

    #[test]
    fn delete_removes_connected_edges() {
        let mut e = editor();
        let mut label = Cell::new_vertex("l", "e", Rect::default(), "edgeLabel", "x");
        label.geometry.as_mut().unwrap().relative = true;
        e.file.pages[0].cells.push(label);
        e.delete(0, &list(&["0", "1"])).unwrap();
        assert!(!e.can_undo(), "the root and the layer stay");
        e.delete(0, &list(&["a"])).unwrap();
        assert_eq!(order(&e), ["0", "1", "b"]);
    }

    #[test]
    fn duplicate_remaps_terminals_and_offsets() {
        let mut e = editor();
        let made = e.duplicate(0, &list(&["a", "b", "e"])).unwrap();
        let [a2, b2, e2] = &made[..] else {
            panic!("{made:?}")
        };
        assert_eq!(order(&e), ["0", "1", "a", a2, "b", b2, "e", e2]);
        let copy = e.page(0).unwrap().cell(e2).unwrap();
        assert_eq!(copy.source.as_ref(), Some(a2));
        assert_eq!(copy.target.as_ref(), Some(b2));
        assert_eq!(geometry(&e, e2).points, Some(vec![Point::new(80.0, 70.0)]));
        assert_eq!(geometry(&e, a2).rect(), Rect::new(10.0, 10.0, 40.0, 40.0));
        let alone = e.duplicate(0, &list(&["e"])).unwrap();
        let copy = e.page(0).unwrap().cell(&alone[0]).unwrap();
        assert_eq!(
            (copy.source.as_deref(), copy.target.as_deref()),
            (Some("a"), Some("b")),
            "an edge copied alone keeps its ends"
        );
    }

    #[test]
    fn reorder_moves_among_siblings() {
        let tree = [("a", "1"), ("g", "1"), ("g1", "g"), ("g2", "g"), ("b", "1")];
        let mut e =
            open(tree.map(|(id, parent)| Cell::new_vertex(id, parent, Rect::default(), "", "")));
        e.reorder(0, &list(&["a"]), ZOrder::ToFront).unwrap();
        assert_eq!(order(&e), ["0", "1", "g", "g1", "g2", "b", "a"]);
        e.reorder(0, &list(&["a"]), ZOrder::Backward).unwrap();
        assert_eq!(order(&e), ["0", "1", "g", "g1", "g2", "a", "b"]);
        e.reorder(0, &list(&["g"]), ZOrder::Forward).unwrap();
        assert_eq!(order(&e), ["0", "1", "a", "g", "g1", "g2", "b"]);
        e.reorder(0, &list(&["b"]), ZOrder::ToBack).unwrap();
        assert_eq!(order(&e), ["0", "1", "b", "a", "g", "g1", "g2"]);
        e.reorder(0, &list(&["g2"]), ZOrder::ToBack).unwrap();
        assert_eq!(order(&e), ["0", "1", "b", "a", "g", "g2", "g1"]);
        e.reorder(0, &list(&["a", "b"]), ZOrder::ToFront).unwrap();
        assert_eq!(order(&e), ["0", "1", "g", "g2", "g1", "b", "a"]);
    }

    #[test]
    fn the_last_page_cannot_be_deleted() {
        let mut e = editor();
        assert!(matches!(e.delete_page(0), Err(Error::Refused(_))));
        assert!(matches!(e.delete_page(3), Err(Error::NoPage(3))));
        assert_eq!(e.file().pages.len(), 1);
    }

    #[test]
    fn page_ops_undo() {
        let mut e = editor();
        let before = e.file().pages.clone();
        assert_eq!(e.add_page("Two"), 1);
        e.rename_page(1, "Second").unwrap();
        e.set_page_attr(0, "background", Some("#ff0000")).unwrap();
        assert_eq!(e.page(1).unwrap().name(), "Second");
        assert_eq!(e.page(0).unwrap().model_attr("background"), Some("#ff0000"));
        e.delete_page(1).unwrap();
        assert_eq!(e.file().pages.len(), 1);
        while e.undo() {}
        assert_eq!(e.file().pages, before);
    }

    #[test]
    fn failed_edits_leave_no_undo_step() {
        let mut e = editor();
        let r = e.move_cells(0, &list(&["a", "nope"]), 1.0, 1.0, &Scene::default());
        assert!(matches!(r, Err(Error::NoCell(_))));
        assert_eq!(geometry(&e, "a").x, 0.0);
        let r = e.set_style(2, &list(&["a"]), "rounded", None);
        assert!(matches!(r, Err(Error::NoPage(2))));
        let r = e.resize(0, "e", Rect::default());
        assert!(matches!(r, Err(Error::Refused(_))));
        let r = e.add_vertex(0, Some("nope"), Rect::default(), "", "");
        assert!(matches!(r, Err(Error::NoCell(_))));
        e.file.pages[0].cells.truncate(1);
        let r = e.add_vertex(0, None, Rect::default(), "", "");
        assert!(matches!(r, Err(Error::Refused(_))), "no layer to go in");
        assert!(!e.can_undo() && !e.dirty());
    }
}
