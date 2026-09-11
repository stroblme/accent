//! Changing a diagram, with undo.

use crate::Error;
use crate::geom::{Point, Rect};
use crate::model::{CellId, File, Page};

/// Where [`Editor::reorder`] moves cells among their siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZOrder {
    ToFront,
    ToBack,
    Forward,
    Backward,
}

/// A file being edited: every change goes through here, is one undo step, and marks the file
/// dirty until [`Editor::mark_saved`].
#[derive(Debug, Clone)]
pub struct Editor {
    file: File,
}

impl Editor {
    pub fn new(file: File) -> Editor {
        Editor { file }
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn page(&self, i: usize) -> Result<&Page, Error> {
        self.file.page(i)
    }

    pub fn dirty(&self) -> bool {
        todo!("Editor::dirty")
    }

    pub fn mark_saved(&mut self) {
        todo!("Editor::mark_saved")
    }

    pub fn can_undo(&self) -> bool {
        todo!("Editor::can_undo")
    }

    pub fn can_redo(&self) -> bool {
        todo!("Editor::can_redo")
    }

    /// Take back the last change; `false` when there is none.
    pub fn undo(&mut self) -> bool {
        todo!("Editor::undo")
    }

    pub fn redo(&mut self) -> bool {
        todo!("Editor::redo")
    }

    /// A vertex at `rect` (absolute), inside `parent` or the page's first layer.
    pub fn add_vertex(
        &mut self,
        page: usize,
        parent: Option<&str>,
        rect: Rect,
        style: &str,
        label: &str,
    ) -> Result<CellId, Error> {
        let _ = (page, parent, rect, style, label);
        todo!("Editor::add_vertex")
    }

    /// An edge in the page's first layer. An end without a cell dangles at its point; an end
    /// with one keeps the point too, as draw.io writes it.
    pub fn add_edge(
        &mut self,
        page: usize,
        source: (Option<&str>, Point),
        target: (Option<&str>, Point),
        style: &str,
    ) -> Result<CellId, Error> {
        let _ = (page, source, target, style);
        todo!("Editor::add_edge")
    }

    pub fn move_cells(
        &mut self,
        page: usize,
        ids: &[CellId],
        dx: f64,
        dy: f64,
    ) -> Result<(), Error> {
        let _ = (page, ids, dx, dy);
        todo!("Editor::move_cells")
    }

    /// Give vertex `id` the absolute rectangle `rect`.
    pub fn resize(&mut self, page: usize, id: &str, rect: Rect) -> Result<(), Error> {
        let _ = (page, id, rect);
        todo!("Editor::resize")
    }

    /// Set (or with `None` remove) one style key on every cell in `ids`, as one step.
    pub fn set_style(
        &mut self,
        page: usize,
        ids: &[CellId],
        key: &str,
        value: Option<&str>,
    ) -> Result<(), Error> {
        let _ = (page, ids, key, value);
        todo!("Editor::set_style")
    }

    /// Replace a cell's whole style string.
    pub fn set_style_string(&mut self, page: usize, id: &str, style: &str) -> Result<(), Error> {
        let _ = (page, id, style);
        todo!("Editor::set_style_string")
    }

    /// Set a label from the editor's Markdown: stored as HTML with `html=1`.
    pub fn set_label_markdown(
        &mut self,
        page: usize,
        id: &str,
        markdown: &str,
    ) -> Result<(), Error> {
        let _ = (page, id, markdown);
        todo!("Editor::set_label_markdown")
    }

    /// Remove cells, everything under them and every edge left without an end.
    pub fn delete(&mut self, page: usize, ids: &[CellId]) -> Result<(), Error> {
        let _ = (page, ids);
        todo!("Editor::delete")
    }

    /// Copies of cells and their subtrees, 10 units down and right, edges between copied cells
    /// reconnected to the copies. Returns the new top-level ids.
    pub fn duplicate(&mut self, page: usize, ids: &[CellId]) -> Result<Vec<CellId>, Error> {
        let _ = (page, ids);
        todo!("Editor::duplicate")
    }

    pub fn reorder(&mut self, page: usize, ids: &[CellId], z: ZOrder) -> Result<(), Error> {
        let _ = (page, ids, z);
        todo!("Editor::reorder")
    }

    /// Set (or remove) a page attribute: `pageWidth`, `pageHeight`, `background`, `gridSize`.
    pub fn set_page_attr(
        &mut self,
        page: usize,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), Error> {
        let _ = (page, key, value);
        todo!("Editor::set_page_attr")
    }

    /// A blank page at the end; returns its index.
    pub fn add_page(&mut self, name: &str) -> usize {
        let _ = name;
        todo!("Editor::add_page")
    }

    pub fn rename_page(&mut self, page: usize, name: &str) -> Result<(), Error> {
        let _ = (page, name);
        todo!("Editor::rename_page")
    }

    /// Refused for the last page: a file always has one.
    pub fn delete_page(&mut self, page: usize) -> Result<(), Error> {
        let _ = page;
        todo!("Editor::delete_page")
    }

    pub fn move_page(&mut self, from: usize, to: usize) -> Result<(), Error> {
        let _ = (from, to);
        todo!("Editor::move_page")
    }
}
