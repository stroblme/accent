//! Changing a diagram, with undo.

use std::collections::{HashMap, HashSet};

use crate::Error;
use crate::geom::{Point, Rect};
use crate::model::{Cell, CellId, File, Geometry, Page, guid, set_attr};
use crate::route::Constraint;
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
    /// The layer the reader picked for new cells: kept for the session, never written and never
    /// an undo step, as draw.io's default parent is.
    current: Option<CellId>,
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
            current: None,
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

    /// A vertex at `rect` (absolute), inside `parent` or the layer new cells go into
    /// ([`Editor::set_current_layer`]).
    pub fn add_vertex(
        &mut self,
        page: usize,
        parent: Option<&str>,
        rect: Rect,
        style: &str,
        label: &str,
    ) -> Result<CellId, Error> {
        let current = self.current.clone();
        self.edit(page, |p, ids| {
            let parent = match parent {
                Some(id) => check(p, [id]).map(|()| id.to_string())?,
                None => default_layer(p, current.as_deref())?,
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

    /// An edge in the layer new cells go into. An end without a cell dangles at its point; an
    /// end with one keeps the point too, as draw.io writes it.
    pub fn add_edge(
        &mut self,
        page: usize,
        source: (Option<&str>, Point),
        target: (Option<&str>, Point),
        style: &str,
    ) -> Result<CellId, Error> {
        let current = self.current.clone();
        self.edit(page, |p, ids| {
            check(p, [source.0, target.0].into_iter().flatten())?;
            let layer = default_layer(p, current.as_deref())?;
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

    /// [`set_end`] on page `page`.
    pub fn set_end(
        &mut self,
        page: usize,
        id: &str,
        source: bool,
        on: (Option<&str>, Point),
        constraint: Option<&Constraint>,
    ) -> Result<(), Error> {
        self.edit(page, |p, _| set_end(p, id, source, on, constraint))
    }

    /// [`move_label`] on page `page`.
    pub fn move_label(
        &mut self,
        page: usize,
        id: &str,
        route: &[Point],
        at: Point,
    ) -> Result<(), Error> {
        self.edit(page, |p, _| move_label(p, id, route, at))
    }

    /// [`set_points`] on page `page`.
    pub fn set_points(&mut self, page: usize, id: &str, points: &[Point]) -> Result<(), Error> {
        self.edit(page, |p, _| set_points(p, id, points))
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
            delete(p, ids);
            Ok(())
        })
    }

    /// Group `ids` as draw.io's Group does (`mxGraph.groupCells`): those sharing the parent of
    /// the first in the page's order (`getCellsForGroup`), two at least, go into a new `group`
    /// cell sized to their box (`getBoundsForGroup`), added in front of its siblings, the cells
    /// staying where they are on the page. The group's id; `None` where there is nothing to
    /// group.
    // mxGraph.js 4043-4150; Graph.js 13290-13345 (bounds), 18879 (the group's style)
    pub fn group(&mut self, page: usize, ids: &[CellId]) -> Result<Option<CellId>, Error> {
        self.edit(page, |p, fresh| {
            check(p, ids)?;
            let chosen: HashSet<&str> = ids.iter().map(String::as_str).collect();
            let ordered = p.cells.iter().filter(|c| chosen.contains(c.id.as_str()));
            let ordered: Vec<&Cell> = ordered.collect();
            let Some(parent) = ordered.first().and_then(|c| c.parent.clone()) else {
                return Ok(None);
            };
            let cells: Vec<CellId> = ordered
                .iter()
                .filter(|c| c.parent.as_ref() == Some(&parent))
                .map(|c| c.id.clone())
                .collect();
            let bounds = group_bounds(p, &cells);
            let (true, Some(bounds)) = (cells.len() > 1, bounds) else {
                return Ok(None);
            };
            let id = fresh.fresh(p);
            let mut group = Cell::new_vertex(&id, &parent, bounds, "group", "");
            group.attrs.push(("connectable".into(), "0".into()));
            let at = subtree_end(p, &parent);
            p.cells.insert(at, group);
            append_to(p, &cells, &id);
            for cell in p.cells.iter_mut().filter(|c| cells.contains(&c.id)) {
                if let Some(g) = &mut cell.geometry {
                    translate(g, -bounds.x, -bounds.y);
                }
            }
            Ok(Some(id))
        })
    }

    /// [`ungroup`] on page `page`.
    pub fn ungroup(&mut self, page: usize, ids: &[CellId]) -> Result<Vec<CellId>, Error> {
        self.edit(page, |p, _| ungroup(p, ids))
    }

    /// Copies of cells and their subtrees, 10 units down and right, edges between copied cells
    /// reconnected to the copies. Returns the new top-level ids.
    pub fn duplicate(&mut self, page: usize, ids: &[CellId]) -> Result<Vec<CellId>, Error> {
        self.edit(page, |p, new_ids| {
            check(p, ids)?;
            let top = topmost(p, ids);
            let copied = with_subtrees(p, top.iter().cloned());
            let copied = p.cells.iter().filter(|c| copied.contains(&c.id));
            let fresh = fresh_ids(p, new_ids, copied);
            // Each copy goes right after the last cell of its original's subtree.
            let mut blocks = Vec::new();
            for original in &top {
                let subtree = with_subtrees(p, [original.clone()]);
                let (mut end, mut block) = (0, Vec::new());
                for (i, cell) in p.cells.iter().enumerate() {
                    if !subtree.contains(&cell.id) {
                        continue;
                    }
                    let mut copy = copy_of(cell, &fresh);
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

    /// Paste `from`, the pages read off the clipboard, as one step: the first onto page `page`
    /// ([`paste_into`]), and any others after the last page as pages of their own, with ids of
    /// their own and a name where they had none. What was pasted onto `page`, to select.
    // diagramly/EditorUi.js 11240-11330
    pub fn paste(
        &mut self,
        page: usize,
        from: &[Page],
        dx: f64,
        dy: f64,
    ) -> Result<Vec<CellId>, Error> {
        let [first, rest @ ..] = from else {
            return Ok(Vec::new());
        };
        let current = self.current.clone();
        let current = current.as_deref();
        if rest.is_empty() {
            return self.edit(page, |p, ids| paste_into(p, ids, current, first, dx, dy));
        }
        self.file.page(page)?;
        let mut pages = self.file.pages.clone();
        let pasted = paste_into(&mut pages[page], &mut self.ids, current, first, dx, dy)?;
        for from in rest {
            let mut added = from.clone();
            added.set_id(&guid());
            if added.name().is_empty() {
                added.set_name(&format!("Page-{}", pages.len() + 1));
            }
            pages.push(added);
        }
        let before = std::mem::replace(&mut self.file.pages, pages);
        self.record(Snapshot::Pages(before));
        Ok(pasted)
    }

    /// [`remove`] on page `page`.
    pub fn remove(&mut self, page: usize, ids: &[CellId], drawn: &Scene) -> Result<(), Error> {
        self.edit(page, |p, _| remove(p, ids, drawn))
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

    /// Pick layer `id` for new cells, `None` for the page's default ([`Page::default_parent`]),
    /// which also takes them while the one picked is locked or hidden. Not an edit; the caller
    /// lets go of it on a page switch, as draw.io's root change resets its default parent.
    pub fn set_current_layer(&mut self, id: Option<CellId>) {
        self.current = id;
    }

    /// The layer picked for new cells while it is still one of page `page`'s, the page's
    /// default otherwise: the layer the Layers list marks.
    pub fn current_layer(&self, page: usize) -> Option<&str> {
        let p = self.file.page(page).ok()?;
        let layers = p.layers();
        let picked = self.current.as_deref();
        picked
            .filter(|id| layers.iter().any(|l| l.id == *id))
            .or_else(|| p.default_parent())
    }

    /// A layer on top of the others, named `name`, as draw.io's Add Layer makes one. Its id.
    pub fn add_layer(&mut self, page: usize, name: &str) -> Result<CellId, Error> {
        self.edit(page, |p, ids| {
            let root = p.root().ok_or(Error::Refused("the page has no root"))?;
            let root = root.id.clone();
            let id = ids.fresh(p);
            let mut layer = Cell::layer(&id, &root);
            layer.set_label(name);
            p.cells.push(layer);
            Ok(id)
        })
    }

    /// Rename layer `id`: its value, which draw.io's Layers dialog shows.
    pub fn rename_layer(&mut self, page: usize, id: &str, name: &str) -> Result<(), Error> {
        self.edit(page, |p, _| {
            cell_mut(p, id)?.set_label(name);
            Ok(())
        })
    }

    /// Show or hide cell `id`, a layer above all, as draw.io writes it: `visible="0"`, or no
    /// attribute at all.
    pub fn set_visible(&mut self, page: usize, id: &str, visible: bool) -> Result<(), Error> {
        self.edit(page, |p, _| {
            let attrs = &mut cell_mut(p, id)?.attrs;
            match visible {
                true => attrs.retain(|(k, _)| k != "visible"),
                false => set_attr(attrs, "visible", "0"),
            }
            Ok(())
        })
    }

    /// Delete layer `id` with everything on it and every edge left without an end, as one step.
    /// Refused for the page's last layer: a page always has one to draw on.
    pub fn delete_layer(&mut self, page: usize, id: &str) -> Result<(), Error> {
        self.edit(page, |p, _| {
            let layers = p.layers();
            if !layers.iter().any(|l| l.id == id) {
                return Err(Error::NoCell(id.to_string()));
            }
            if layers.len() == 1 {
                return Err(Error::Refused("a page keeps at least one layer"));
            }
            let gone = with_subtrees(p, [id.to_string()]);
            remove_with_edges(p, gone);
            Ok(())
        })
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

/// Paste `from`, a page read off the clipboard, onto `page` as draw.io's `importGraphModel`
/// does: the cells of a page with one layer go into the layer new cells go into, moved by
/// (`dx`, `dy`), and each layer of one with several comes as a layer of its own; every cell
/// takes a fresh id, and a reference to a cell that was not pasted goes. What was pasted onto
/// a layer.
// Graph.js 17723-17810
fn paste_into(
    page: &mut Page,
    new_ids: &mut Ids,
    current: Option<&str>,
    from: &Page,
    dx: f64,
    dy: f64,
) -> Result<Vec<CellId>, Error> {
    let root = from.root().map(|c| c.id.clone());
    let cells = from.cells.iter().filter(|c| c.parent.is_some());
    let fresh = fresh_ids(page, new_ids, cells.clone());
    let mut copies: Vec<Cell> = cells.map(|c| copy_of(c, &fresh)).collect();
    let pasted_ids: HashSet<&CellId> = fresh.values().collect();
    let known = |id: &Option<CellId>| id.as_ref().is_some_and(|id| pasted_ids.contains(id));
    for copy in &mut copies {
        if !known(&copy.source) {
            copy.source = None;
        }
        if !known(&copy.target) {
            copy.target = None;
        }
    }
    let layers: Vec<CellId> = from.layers().iter().map(|l| fresh[&l.id].clone()).collect();
    let pasted: Vec<CellId>;
    if let [layer] = layers.as_slice() {
        let into = default_layer(page, current)?;
        copies.retain(|c| &c.id != layer);
        for copy in &mut copies {
            if copy.parent.as_ref() == Some(layer) {
                copy.parent = Some(into.clone());
                if let Some(g) = &mut copy.geometry {
                    translate(g, dx, dy);
                }
            }
        }
        pasted = copies
            .iter()
            .filter(|c| c.parent.as_ref() == Some(&into))
            .map(|c| c.id.clone())
            .collect();
        let at = subtree_end(page, &into);
        page.cells.splice(at..at, copies);
    } else {
        let into = page.root().map(|c| c.id.clone());
        for copy in copies.iter_mut().filter(|c| layers.contains(&c.id)) {
            copy.parent = into.clone().or(root.clone());
        }
        pasted = copies
            .iter()
            .filter(|c| c.parent.as_ref().is_some_and(|l| layers.contains(l)))
            .map(|c| c.id.clone())
            .collect();
        page.cells.extend(copies);
    }
    Ok(pasted)
}

/// Remove the topmost of `ids` and everything under them, as draw.io's Cut does
/// (`mxGraph.removeCells` without their edges): an edge not removed with them lets go of them
/// where `drawn` has its end (`cellsRemoved`, `disconnectTerminal`).
// mxGraph.js 5037-5210
pub fn remove(p: &mut Page, ids: &[CellId], drawn: &Scene) -> Result<(), Error> {
    check(p, ids)?;
    let gone = with_subtrees(p, topmost(p, ids));
    let cut = |end: &Option<CellId>| end.as_ref().is_some_and(|id| gone.contains(id));
    let loose: Vec<CellId> = p
        .cells
        .iter()
        .filter(|c| !gone.contains(&c.id) && (cut(&c.source) || cut(&c.target)))
        .map(|c| c.id.clone())
        .collect();
    for id in loose {
        let origin = p.origin_of(&id);
        let route = drawn.route(&id).unwrap_or_default().to_vec();
        let Some(cell) = p.cell_mut(&id) else {
            continue;
        };
        let ends = [
            (cut(&cell.source), route.first()),
            (cut(&cell.target), route.last()),
        ];
        let g = cell.geometry.get_or_insert_with(|| Geometry {
            relative: true,
            ..Geometry::default()
        });
        for (i, (loose, at)) in ends.into_iter().enumerate() {
            let at = at.map(|a| Point::new(a.x - origin.x, a.y - origin.y));
            match (loose, i) {
                (false, _) => {}
                (true, 0) => {
                    g.source_point = at.or(g.source_point);
                    cell.source = None;
                }
                (true, _) => {
                    g.target_point = at.or(g.target_point);
                    cell.target = None;
                }
            }
        }
    }
    p.cells.retain(|c| !gone.contains(&c.id));
    Ok(())
}

/// Fresh ids for `cells`, none of them taken on `page`: what pasted and duplicated cells get.
fn fresh_ids<'a>(
    page: &Page,
    ids: &mut Ids,
    cells: impl IntoIterator<Item = &'a Cell>,
) -> HashMap<CellId, CellId> {
    cells
        .into_iter()
        .map(|c| (c.id.clone(), ids.fresh(page)))
        .collect()
}

/// `cell` under its fresh id, its parent, source and target following theirs where they have
/// one and kept where they do not.
fn copy_of(cell: &Cell, fresh: &HashMap<CellId, CellId>) -> Cell {
    let mut copy = cell.clone();
    copy.id = fresh[&cell.id].clone();
    for id in [&mut copy.parent, &mut copy.source, &mut copy.target] {
        if let Some(new) = id.as_ref().and_then(|old| fresh.get(old)) {
            *id = Some(new.clone());
        }
    }
    copy
}

/// Remove cells, everything under them and every edge left without an end. The root and the
/// layers are not removed this way.
fn delete(p: &mut Page, ids: &[CellId]) {
    let kept: HashSet<&str> = p
        .root()
        .into_iter()
        .chain(p.layers())
        .map(|c| c.id.as_str())
        .collect();
    let chosen = ids.iter().filter(|id| !kept.contains(id.as_str()));
    let gone = with_subtrees(p, chosen.cloned());
    remove_with_edges(p, gone);
}

/// Remove the cells in `gone` and every edge they leave without an end.
fn remove_with_edges(p: &mut Page, mut gone: HashSet<CellId>) {
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
}

/// Ungroup `ids` as draw.io's Ungroup does: each shape among them with children hands them to
/// its own parent (`mxGraph.ungroupCells`), where they keep their place on the page and paint in
/// front; a group then left with no fill, line or picture goes, with its edges
/// (`removeCellsAfterUngroup`), and a shape kept is no longer a container. What to select: the
/// children, then the cells of `ids` still there.
// mxGraph.js 4180-4290; Graph.js 20266; Actions.js 662-701
pub fn ungroup(p: &mut Page, ids: &[CellId]) -> Result<Vec<CellId>, Error> {
    check(p, ids)?;
    let groups: Vec<CellId> = ids
        .iter()
        .filter(|id| p.cell(id).is_some_and(|c| c.vertex) && p.children(id).next().is_some())
        .cloned()
        .collect();
    let mut chosen = Vec::new();
    for group in &groups {
        let Some(parent) = p.cell(group).and_then(|c| c.parent.clone()) else {
            continue;
        };
        let children: Vec<CellId> = p.children(group).map(|c| c.id.clone()).collect();
        // Kept where they are on the page: moved by the group's own place in its parent, a
        // child placed on the group by fractions of it placed from the new parent instead.
        let shift = p.absolute_rect(group).unwrap_or_default();
        let into = p.origin_of(group);
        let places: Vec<Option<Rect>> = children.iter().map(|c| p.absolute_rect(c)).collect();
        for (id, place) in children.iter().zip(places) {
            let Some(cell) = p.cell_mut(id) else { continue };
            let vertex = cell.vertex;
            let Some(g) = &mut cell.geometry else {
                continue;
            };
            match (vertex && g.relative, place) {
                (true, Some(r)) => {
                    (g.x, g.y, g.relative) = (r.x - into.x, r.y - into.y, false);
                }
                _ => translate(g, shift.x - into.x, shift.y - into.y),
            }
        }
        append_to(p, &children, &parent);
        chosen.extend(children);
    }
    let transparent: Vec<CellId> = groups
        .into_iter()
        .filter(|g| {
            p.cell(g).is_some_and(|c| {
                let style = c.style.resolve(false);
                let none = |key: &str| style.get(key).is_none_or(|v| v == "none");
                none("fillColor") && none("strokeColor") && style.get("image").is_none()
            })
        })
        .collect();
    delete(p, &transparent);
    for id in ids {
        let Some(cell) = p.cell(id) else { continue };
        if cell.vertex && p.children(id).next().is_none() {
            cell_mut(p, id)?.style.set("container", Some("0"));
        }
        chosen.push(id.clone());
    }
    Ok(chosen)
}

/// The box `cells`, siblings, take in their parent (`Graph.getBoundsForGroup`): a shape's
/// rectangle, an edge's loose ends and waypoints; a shape placed on its parent by fractions of
/// it counts for nothing.
fn group_bounds(p: &Page, cells: &[CellId]) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    let mut add = |r: Rect| bounds = Some(bounds.map_or(r, |b| b.union(&r)));
    for cell in cells.iter().filter_map(|id| p.cell(id)) {
        let Some(g) = &cell.geometry else { continue };
        if cell.edge {
            let loose = [
                g.source_point.filter(|_| cell.source.is_none()),
                g.target_point.filter(|_| cell.target.is_none()),
            ];
            let points = g.points.iter().flatten().copied();
            for pt in loose.into_iter().flatten().chain(points) {
                add(Rect::new(pt.x, pt.y, 0.0, 0.0));
            }
        } else if cell.vertex && !g.relative {
            add(g.rect());
        }
    }
    bounds
}

/// Where a cell added last under `parent` goes in the page's order: after everything under it.
fn subtree_end(p: &Page, parent: &str) -> usize {
    let inside = with_subtrees(p, [parent.to_string()]);
    p.cells
        .iter()
        .rposition(|c| inside.contains(&c.id))
        .map_or(p.cells.len(), |i| i + 1)
}

/// Make each of `ids`, everything under it going along, the last child of `parent`, in the
/// page's order (`mxGraphModel.add` at the parent's child count: in front of its siblings).
fn append_to(p: &mut Page, ids: &[CellId], parent: &str) {
    let moving = with_subtrees(p, ids.iter().cloned());
    let (mut taken, rest): (Vec<Cell>, Vec<Cell>) = std::mem::take(&mut p.cells)
        .into_iter()
        .partition(|c| moving.contains(&c.id));
    p.cells = rest;
    for cell in taken.iter_mut().filter(|c| ids.contains(&c.id)) {
        cell.parent = Some(parent.to_string());
    }
    let at = subtree_end(p, parent);
    p.cells.splice(at..at, taken);
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

/// Put the source end (else the target end) of edge `id` on `on`: on shape `on.0`, pinned at
/// `constraint` when there is one and floating on its outline otherwise
/// (`mxEdgeHandler.connect`); with no shape, dangling at the absolute point `on.1`
/// (`changeTerminalPoint`). The `exit` or `entry` keys say which, as `setConnectionConstraint`
/// writes them.
// mxGraph.setConnectionConstraint, mxGraph.js 7163-7213; mxEdgeHandler.js 1984-2046
pub fn set_end(
    page: &mut Page,
    id: &str,
    source: bool,
    (on, at): (Option<&str>, Point),
    constraint: Option<&Constraint>,
) -> Result<(), Error> {
    check(page, [id].into_iter().chain(on))?;
    let origin = page.origin_of(id);
    let cell = cell_mut(page, id)?;
    if !cell.edge {
        return Err(Error::Refused("only an edge has ends"));
    }
    let end = if source { "exit" } else { "entry" };
    for key in ["X", "Y", "Dx", "Dy", "Perimeter"] {
        cell.style.set(&format!("{end}{key}"), None);
    }
    if let Some(c) = constraint.filter(|_| on.is_some()) {
        let keys = [
            ("X", c.point.x),
            ("Y", c.point.y),
            ("Dx", c.dx),
            ("Dy", c.dy),
        ];
        for (key, n) in keys {
            cell.style.set(&format!("{end}{key}"), Some(&n.to_string()));
        }
        cell.style
            .set(&format!("{end}Perimeter"), (!c.perimeter).then_some("0"));
    }
    if on.is_none() {
        let g = cell.geometry.get_or_insert_with(|| Geometry {
            relative: true,
            ..Geometry::default()
        });
        let local = Some(Point::new(at.x - origin.x, at.y - origin.y));
        match source {
            true => g.source_point = local,
            false => g.target_point = local,
        }
    }
    let on = on.map(str::to_string);
    match source {
        true => cell.source = on,
        false => cell.target = on,
    }
    Ok(())
}

/// Put the label of edge `id`, routed along `route` (absolute), at `at` (`mxEdgeHandler.moveLabel`):
/// a relative geometry keeps where along the edge the nearest point is and how far across, in
/// its `x` and `y`, and the rest as its offset; any other keeps the offset from the middle of its
/// ends.
// mxEdgeHandler.moveLabel, mxEdgeHandler.js 1924-1966
pub fn move_label(page: &mut Page, id: &str, route: &[Point], at: Point) -> Result<(), Error> {
    check(page, [id])?;
    let (Some(&first), Some(&last)) = (route.first(), route.last()) else {
        return Err(Error::Refused("the edge is not drawn"));
    };
    let cell = cell_mut(page, id)?;
    let Some(g) = cell.geometry.as_mut().filter(|_| cell.edge) else {
        return Err(Error::Refused("only an edge's label moves along it"));
    };
    if g.relative {
        let (x, y) = relative_point(route, at);
        g.x = (x * 10000.0).round() / 10000.0;
        g.y = y.round();
        g.offset = None;
        let on = crate::scene::edge_label_at(route, g);
        g.offset = Some(Point::new((at.x - on.x).round(), (at.y - on.y).round()));
    } else {
        let middle = Point::new((first.x + last.x) / 2.0, (first.y + last.y) / 2.0);
        g.offset = Some(Point::new(
            (at.x - middle.x).round(),
            (at.y - middle.y).round(),
        ));
        (g.x, g.y) = (0.0, 0.0);
    }
    Ok(())
}

/// Where along an edge routed through `points` the point nearest `p` is, from -1 at its start to
/// 1 at its end, and how far `p` is across it, negative on its left.
// mxGraphView.getRelativePoint, mxGraphView.js 2101-2193
fn relative_point(points: &[Point], p: Point) -> (f64, f64) {
    let segments: Vec<f64> = points.windows(2).map(|w| w[0].distance(w[1])).collect();
    let total: f64 = segments.iter().sum();
    // The segment nearest the point, the last of equals, and how far along the edge it starts.
    let (mut index, mut length, mut walked) = (0, 0.0, 0.0);
    let mut nearest = crate::geom::distance_to_segment(p, points[0], points[1]);
    for i in 2..points.len() {
        let d = crate::geom::distance_to_segment(p, points[i - 1], points[i]);
        walked += segments[i - 2];
        if d <= nearest {
            (nearest, index, length) = (d, i - 1, walked);
        }
    }
    let (p0, pe) = (points[index], points[index + 1]);
    let (sx, sy) = (p0.x - pe.x, p0.y - pe.y);
    let (px, py) = (sx - (p.x - pe.x), sy - (p.y - pe.y));
    let dot = px * sx + py * sy;
    let projected = match dot <= 0.0 {
        true => 0.0,
        false => (dot * dot / (sx * sx + sy * sy)).sqrt(),
    };
    let projected = projected.min(segments[index]);
    let mut across = crate::geom::distance_to_segment(p, p0, pe);
    if relative_ccw(p0, pe, p) == -1 {
        across = -across;
    }
    let along = match total > 0.0 {
        true => (total / 2.0 - length - projected) / total * -2.0,
        false => 0.0,
    };
    (along, across)
}

/// Which side of the line from `a` to `b` point `p` is on (`mxUtils.relativeCcw`).
fn relative_ccw(a: Point, b: Point, p: Point) -> i32 {
    let (x2, y2) = (b.x - a.x, b.y - a.y);
    let (mut px, mut py) = (p.x - a.x, p.y - a.y);
    let mut ccw = px * y2 - py * x2;
    if ccw == 0.0 {
        ccw = px * x2 + py * y2;
        if ccw > 0.0 {
            px -= x2;
            py -= y2;
            ccw = (px * x2 + py * y2).max(0.0);
        }
    }
    match ccw {
        c if c < 0.0 => -1,
        c if c > 0.0 => 1,
        _ => 0,
    }
}

/// Give edge `id` the waypoints `points`, absolute, none for an empty list
/// (`mxEdgeHandler.changePoints`).
pub fn set_points(page: &mut Page, id: &str, points: &[Point]) -> Result<(), Error> {
    check(page, [id])?;
    let origin = page.origin_of(id);
    let cell = cell_mut(page, id)?;
    if !cell.edge {
        return Err(Error::Refused("only an edge has waypoints"));
    }
    let local = points
        .iter()
        .map(|p| Point::new(p.x - origin.x, p.y - origin.y));
    let g = cell.geometry.get_or_insert_with(|| Geometry {
        relative: true,
        ..Geometry::default()
    });
    g.points = (!points.is_empty()).then(|| local.collect());
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

/// The layer new cells go into: `current` while it is a layer of `page` neither locked nor
/// hidden, [`Page::default_parent`] otherwise; refused when there is none.
fn default_layer(page: &Page, current: Option<&str>) -> Result<CellId, Error> {
    let layers = page.layers();
    let picked = layers
        .iter()
        .find(|l| Some(l.id.as_str()) == current && l.is_visible() && !l.is_locked());
    match picked
        .map(|l| l.id.as_str())
        .or_else(|| page.default_parent())
    {
        Some(id) => Ok(id.to_string()),
        None if layers.is_empty() => Err(Error::Refused("the page has no layer")),
        None => Err(Error::Refused(
            "every layer on this page is locked or hidden",
        )),
    }
}

/// `roots` and every cell under them. A file in draw.io's order, parents before children,
/// takes one pass over the cells; the loop is for any other order.
pub(crate) fn with_subtrees(
    page: &Page,
    roots: impl IntoIterator<Item = CellId>,
) -> HashSet<CellId> {
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
pub(crate) fn topmost(page: &Page, ids: &[CellId]) -> Vec<CellId> {
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
    fn new_cells_go_into_the_current_layer_else_the_topmost_open_one() {
        let locked = |id| Cell {
            style: Style::parse("locked=1;"),
            ..Cell::layer(id, "0")
        };
        let hidden = Cell {
            attrs: vec![("visible".into(), "0".into())],
            ..Cell::layer("3", "0")
        };
        let mut e = open([Cell::layer("2", "0"), hidden]);
        e.file.pages[0].cells[1] = locked("1");
        // Picked, but locked: the topmost layer neither locked nor hidden takes them.
        e.set_current_layer(Some("1".into()));
        assert_eq!(e.current_layer(0), Some("1"));
        let v = e.add_vertex(0, None, Rect::default(), "", "").unwrap();
        let ends = (None, Point::default());
        let f = e.add_edge(0, ends, ends, "").unwrap();
        let parent = |e: &Editor, id: &str| e.page(0).unwrap().cell(id).unwrap().parent.clone();
        assert_eq!(parent(&e, &v).as_deref(), Some("2"));
        assert_eq!(parent(&e, &f).as_deref(), Some("2"));
        e.set_visible(0, "3", true).unwrap();
        e.set_current_layer(None);
        assert_eq!(e.current_layer(0), Some("3"));
        let v = e.add_vertex(0, None, Rect::default(), "", "").unwrap();
        assert_eq!(parent(&e, &v).as_deref(), Some("3"));

        let mut e = open(Vec::new());
        e.file.pages[0].cells[1] = locked("1");
        let r = e.add_vertex(0, None, Rect::default(), "", "");
        assert!(matches!(
            r,
            Err(Error::Refused(
                "every layer on this page is locked or hidden"
            ))
        ));
        assert!(!e.can_undo());
    }

    #[test]
    fn layers_are_added_locked_hidden_reordered_and_deleted_a_step_each() {
        let mut e = editor();
        let layers = |e: &Editor| -> Vec<String> {
            let page = e.page(0).unwrap();
            page.layers().iter().map(|l| l.id.clone()).collect()
        };
        let top = e.add_layer(0, "Untitled Layer").unwrap();
        assert_eq!(layers(&e), ["1", top.as_str()]);
        assert_eq!(e.current_layer(0), Some(top.as_str()));
        let v = e.add_vertex(0, None, Rect::default(), "", "").unwrap();
        let ends = (Some("a"), Point::default());
        let f = e
            .add_edge(0, ends, (Some(&v), Point::default()), "")
            .unwrap();
        e.set_style(0, std::slice::from_ref(&top), "locked", Some("1"))
            .unwrap();
        e.set_visible(0, "1", false).unwrap();
        e.rename_layer(0, &top, "Top").unwrap();
        // Written as draw.io writes them, and read back.
        let read = File::from_bytes(e.file().to_xml().as_bytes()).unwrap();
        let page = &read.pages[0];
        assert!(!page.cell("1").unwrap().is_visible());
        let layer = page.cell(&top).unwrap();
        assert!(layer.is_locked() && layer.label() == "Top");
        assert_eq!(page.default_parent(), None);
        e.reorder(0, std::slice::from_ref(&top), ZOrder::Backward)
            .unwrap();
        assert_eq!(layers(&e), [top.as_str(), "1"]);
        // Its cells go with it, and the edge from a shape on another layer to one of them.
        e.delete_layer(0, &top).unwrap();
        assert_eq!(layers(&e), ["1"]);
        assert_eq!(order(&e), ["0", "1", "a", "b", "e"]);
        assert!(matches!(
            e.delete_layer(0, "1"),
            Err(Error::Refused("a page keeps at least one layer"))
        ));
        assert_eq!(e.undo.len(), 8, "one step each");
        e.undo();
        let page = e.page(0).unwrap();
        assert!(page.cell(&v).is_some() && page.cell(&f).is_some());
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
    fn an_end_pins_floats_or_dangles() {
        let mut e = editor();
        let pin = Constraint {
            point: Point::new(0.5, 0.0),
            dx: 0.0,
            dy: 0.0,
            perimeter: false,
        };
        e.set_end(0, "e", false, (Some("a"), Point::default()), Some(&pin))
            .unwrap();
        let cell = e.page(0).unwrap().cell("e").unwrap();
        assert_eq!(cell.target.as_deref(), Some("a"));
        let style = cell.style.to_string();
        assert!(
            style.contains("entryX=0.5") && style.contains("entryPerimeter=0"),
            "{style}"
        );
        // Floating on b: the constraint goes.
        e.set_end(0, "e", false, (Some("b"), Point::default()), None)
            .unwrap();
        let cell = e.page(0).unwrap().cell("e").unwrap();
        assert!(!cell.style.to_string().contains("entry"));
        // Dangling: the point is the end.
        e.set_end(0, "e", true, (None, Point::new(5.0, 6.0)), None)
            .unwrap();
        let cell = e.page(0).unwrap().cell("e").unwrap();
        assert_eq!(cell.source, None);
        assert_eq!(geometry(&e, "e").source_point, Some(Point::new(5.0, 6.0)));
        assert!(matches!(
            e.set_end(0, "a", true, (None, Point::default()), None),
            Err(Error::Refused(_))
        ));
        e.set_points(0, "e", &[Point::new(1.0, 2.0)]).unwrap();
        assert_eq!(geometry(&e, "e").points, Some(vec![Point::new(1.0, 2.0)]));
        e.set_points(0, "e", &[]).unwrap();
        assert_eq!(geometry(&e, "e").points, None);
    }

    #[test]
    fn an_edge_label_moves_along_and_off_its_edge() {
        let mut e = editor();
        let route = [Point::new(0.0, 0.0), Point::new(100.0, 0.0)];
        // A quarter of the way along, 10 below the line.
        e.move_label(0, "e", &route, Point::new(25.0, 10.0))
            .unwrap();
        let g = geometry(&e, "e");
        assert_eq!((g.x, g.y, g.offset), (-0.5, -10.0, Some(Point::default())));
        assert_eq!(
            crate::scene::edge_label_at(&route, g),
            Point::new(25.0, 10.0)
        );
        // Beyond the end: at the end, as far across as it is away, the rest as offset.
        e.move_label(0, "e", &route, Point::new(130.0, 0.0))
            .unwrap();
        let g = geometry(&e, "e");
        assert_eq!((g.x, g.y), (1.0, 30.0));
        assert_eq!(g.offset, Some(Point::new(30.0, 30.0)));
        assert_eq!(
            crate::scene::edge_label_at(&route, g),
            Point::new(130.0, 0.0)
        );
    }

    #[test]
    fn a_group_holds_its_cells_where_they_were_and_ungrouping_lets_them_go() {
        let mut e = editor();
        // One cell is not a group.
        assert_eq!(e.group(0, &list(&["a"])).unwrap(), None);
        let g = e.group(0, &list(&["b", "a", "e"])).unwrap().unwrap();
        let page = e.page(0).unwrap();
        assert_eq!(order(&e), ["0", "1", &g, "a", "b", "e"]);
        let group = page.cell(&g).unwrap();
        assert_eq!(group.style.to_string(), "group;");
        // Boxes a and b, and the edge's waypoint (70, 60): the group is sized to them.
        assert_eq!(geometry(&e, &g).rect(), Rect::new(0.0, 0.0, 140.0, 60.0));
        assert_eq!(
            e.page(0).unwrap().absolute_rect("b"),
            Some(Rect::new(100.0, 0.0, 40.0, 40.0))
        );
        // Moved as one, then ungrouped: the transparent group goes, the cells stay put.
        e.move_cells(0, &list(&[g.as_str()]), 10.0, 5.0, &Scene::default())
            .unwrap();
        let chosen = e.ungroup(0, std::slice::from_ref(&g)).unwrap();
        assert_eq!(chosen, list(&["a", "b", "e"]));
        assert!(e.page(0).unwrap().cell(&g).is_none());
        assert_eq!(geometry(&e, "b").rect(), Rect::new(110.0, 5.0, 40.0, 40.0));
        assert_eq!(geometry(&e, "e").points, Some(vec![Point::new(80.0, 65.0)]));
        // A group with a fill of its own stays, as a shape.
        let g = e.group(0, &list(&["a", "b"])).unwrap().unwrap();
        e.set_style(0, std::slice::from_ref(&g), "fillColor", Some("#ff0000"))
            .unwrap();
        let chosen = e.ungroup(0, std::slice::from_ref(&g)).unwrap();
        assert_eq!(chosen, list(&["a", "b", g.as_str()]));
        assert!(
            e.page(0)
                .unwrap()
                .cell(&g)
                .unwrap()
                .style
                .to_string()
                .contains("container=0")
        );
    }

    #[test]
    fn a_paste_takes_fresh_ids_on_the_current_layer_and_a_cut_lets_edges_go() {
        let mut e = editor();
        let drawn = crate::scene::scene(e.page(0).unwrap());
        let xml = crate::clipboard::copy(e.page(0).unwrap(), &list(&["a", "e"]), &drawn);
        let from = crate::clipboard::pages_in(&xml);
        let pasted = e.paste(0, &from, 10.0, 10.0).unwrap();
        assert_eq!(pasted.len(), 2);
        let page = e.page(0).unwrap();
        let (a2, e2) = (
            page.cell(&pasted[0]).unwrap(),
            page.cell(&pasted[1]).unwrap(),
        );
        assert_eq!(a2.parent.as_deref(), Some("1"));
        assert_eq!(
            a2.geometry.as_ref().unwrap().rect(),
            Rect::new(10.0, 10.0, 40.0, 40.0)
        );
        // The pasted edge is on the pasted shape and let go of b, which was not copied.
        assert_eq!(
            (e2.source.as_ref(), e2.target.as_ref()),
            (Some(&pasted[0]), None)
        );
        assert_eq!(order(&e).len(), 7);
        // A file of two pages: the first onto the page, the second a page of its own.
        let file = format!(
            "<mxfile><diagram name=\"One\" id=\"x\">{xml}</diagram><diagram id=\"y\">{xml}</diagram></mxfile>"
        );
        let before = e.file().pages.len();
        let pasted = e
            .paste(0, &crate::clipboard::pages_in(&file), 0.0, 0.0)
            .unwrap();
        assert_eq!((pasted.len(), e.file().pages.len()), (2, before + 1));
        let added = &e.file().pages[before];
        assert_eq!(added.name(), "Page-2");
        assert_ne!(crate::model::attr(&added.attrs, "id"), Some("y"));
        assert!(e.undo());
        assert_eq!(e.file().pages.len(), before);
        // A cut keeps the edge, loose where it was drawn.
        let mut e = editor();
        let drawn = crate::scene::scene(e.page(0).unwrap());
        e.remove(0, &list(&["b"]), &drawn).unwrap();
        let cell = e.page(0).unwrap().cell("e").unwrap();
        assert_eq!(cell.target, None);
        let end = drawn.route("e").unwrap().last().copied();
        assert_eq!(geometry(&e, "e").target_point, end);
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
