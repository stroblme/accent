//! Notebooks: a Jupyter notebook (`.ipynb`, nbformat 4) or a marimo notebook (a `.py` marimo
//! wrote) as one model of cells, read and written by a backend per format. Nothing is run.
//!
//! A save writes back only what changed: each cell keeps the text it was read from, written as
//! it was until its source or its kind changes, and then only its source is rewritten, in the
//! format's own style. Every change goes through [`Notebook::apply`], which answers the edit that
//! undoes it, so one history can hold the cell operations and the typing in the cells alike.

mod ipynb;
mod marimo;
pub mod output;

use std::borrow::Cow;

pub use output::{Attachment, Data, Item, Output};

use crate::{Error, Result, conflict, path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Ipynb,
    Marimo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Code,
    Markdown,
    /// Text a notebook passes through untouched; `.ipynb` only.
    Raw,
}

/// A cell's name in the model, unchanged for as long as its notebook is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellId(u64);

#[derive(Debug, Clone)]
pub struct Cell {
    id: CellId,
    kind: Kind,
    source: String,
    count: Option<u32>,
    outputs: Vec<Output>,
    attachments: Vec<Attachment>,
    /// Boxed, as the largest part of a cell an [`Edit`] carries.
    kept: Box<Kept>,
}

impl Cell {
    pub fn id(&self) -> CellId {
        self.id
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// The execution count the file stored, shown as `[n]`.
    pub fn count(&self) -> Option<u32> {
        self.count
    }

    /// The outputs the file stored. A save writes them back as they were.
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }
}

/// How a cell was written, for writing it back: its backend's own.
#[derive(Debug, Clone)]
enum Kept {
    Ipynb(ipynb::Kept),
    Marimo(marimo::Kept),
}

/// The text of the file around its cells, and what a new cell is written like.
#[derive(Debug, Clone)]
enum Frame {
    Ipynb(ipynb::Frame),
    Marimo(marimo::Frame),
}

/// One change to a notebook. [`Notebook::apply`] answers the change that undoes it.
#[derive(Debug, Clone)]
pub enum Edit {
    Source {
        id: CellId,
        text: String,
    },
    /// Put a cell [`Notebook::new_cell`] made, or one a [`Edit::Remove`] took out, at `at`.
    Insert {
        at: usize,
        cell: Cell,
    },
    Remove {
        id: CellId,
    },
    /// Move a cell to be the `to`th.
    Move {
        id: CellId,
        to: usize,
    },
    /// Change a cell's kind and nothing else; [`Notebook::convert`] converts its text too.
    Convert {
        id: CellId,
        kind: Kind,
    },
}

#[derive(Debug, Clone)]
pub struct Notebook {
    frame: Frame,
    cells: Vec<Cell>,
    language: String,
    extension: String,
    /// Whether every line of the file ended in CRLF, which the backends read as LF.
    crlf: bool,
    next: u64,
}

/// Read the notebook `rel` holds: an `.ipynb` as nbformat 4, anything else as a marimo
/// notebook. A file that does not read as one is an [`Error::Invalid`] saying why, and opens as
/// text.
pub fn read(rel: &str, text: &str) -> Result<Notebook> {
    if !conflict::blocks(text).is_empty() {
        return Err(invalid("it holds conflict markers"));
    }
    let crlf = text.contains("\r\n") && !text.replace("\r\n", "").contains('\n');
    let text = match crlf {
        true => Cow::Owned(text.replace("\r\n", "\n")),
        false => Cow::Borrowed(text),
    };
    let mut notebook = match path::is_notebook(rel) {
        true => ipynb::read(&text)?,
        false => marimo::read(&text)?,
    };
    notebook.crlf = crlf;
    Ok(notebook)
}

/// Whether `text` is a marimo notebook: a module that imports marimo and makes its `app`.
pub fn is_marimo(text: &str) -> bool {
    marimo::sniff(text)
}

fn invalid(why: impl Into<String>) -> Error {
    Error::Invalid(why.into())
}

impl Notebook {
    pub fn format(&self) -> Format {
        match self.frame {
            Frame::Ipynb(_) => Format::Ipynb,
            Frame::Marimo(_) => Format::Marimo,
        }
    }

    /// The code cells' language as the notebook names it, such as `python`.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// The file extension of the code cells' language, such as `.py`, or empty when unknown.
    pub fn extension(&self) -> &str {
        &self.extension
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn position(&self, id: CellId) -> Option<usize> {
        self.cells.iter().position(|c| c.id == id)
    }

    /// Whether this notebook's format has cells of `kind`: marimo has no raw cells.
    pub fn holds(&self, kind: Kind) -> bool {
        kind != Kind::Raw || self.format() == Format::Ipynb
    }

    /// The file's text, every cell no edit changed written as it was read.
    pub fn to_text(&self) -> String {
        let text = match &self.frame {
            Frame::Ipynb(frame) => frame.write(&self.cells),
            Frame::Marimo(frame) => frame.write(&self.cells),
        };
        match self.crlf {
            true => text.replace('\n', "\r\n"),
            false => text,
        }
    }

    /// A cell of `kind` holding `source`, written as the format writes a new one, for an
    /// [`Edit::Insert`]; `None` for a kind the format has not.
    pub fn new_cell(&mut self, kind: Kind, source: &str) -> Option<Cell> {
        if !self.holds(kind) {
            return None;
        }
        let id = CellId(self.next);
        self.next += 1;
        let kept = match &self.frame {
            Frame::Ipynb(frame) => frame.blank(kind, &self.cells, id),
            Frame::Marimo(frame) => frame.blank(kind),
        };
        Some(Cell {
            id,
            kind,
            source: source.to_string(),
            count: None,
            outputs: Vec::new(),
            attachments: Vec::new(),
            kept: Box::new(kept),
        })
    }

    /// Make `edit`, answering the edit that undoes it, or `None` when it names no cell here or
    /// asks for what cannot be.
    pub fn apply(&mut self, edit: Edit) -> Option<Edit> {
        Some(match edit {
            Edit::Source { id, text } => {
                let at = self.position(id)?;
                let text = std::mem::replace(&mut self.cells[at].source, text);
                Edit::Source { id, text }
            }
            Edit::Insert { at, cell } => {
                let ours = matches!(
                    (&self.frame, cell.kept.as_ref()),
                    (Frame::Ipynb(_), Kept::Ipynb(_)) | (Frame::Marimo(_), Kept::Marimo(_))
                );
                if !ours || at > self.cells.len() || self.position(cell.id).is_some() {
                    return None;
                }
                let id = cell.id;
                self.cells.insert(at, cell);
                Edit::Remove { id }
            }
            Edit::Remove { id } => {
                let at = self.position(id)?;
                let cell = self.cells.remove(at);
                Edit::Insert { at, cell }
            }
            Edit::Move { id, to } => {
                let from = self.position(id)?;
                if to >= self.cells.len() {
                    return None;
                }
                let cell = self.cells.remove(from);
                self.cells.insert(to, cell);
                Edit::Move { id, to: from }
            }
            Edit::Convert { id, kind } => {
                let at = self.position(id).filter(|_| self.holds(kind))?;
                let kind = std::mem::replace(&mut self.cells[at].kind, kind);
                Edit::Convert { id, kind }
            }
        })
    }

    /// Make cell `id` a cell of `kind`, as its format converts one: an `.ipynb` cell keeps its
    /// text, while a marimo markdown cell becomes the `mo.md(…)` call it is, and a marimo cell
    /// that is only that call becomes its string. Answers the edits that undo it.
    pub fn convert(&mut self, id: CellId, kind: Kind) -> Option<Vec<Edit>> {
        let cell = &self.cells[self.position(id)?];
        if cell.kind == kind || !self.holds(kind) {
            return None;
        }
        let mut edits = Vec::new();
        if self.format() == Format::Marimo {
            let text = match kind {
                Kind::Markdown => marimo::to_markdown(&cell.source),
                Kind::Code | Kind::Raw => marimo::to_code(&cell.source),
            };
            edits.push(Edit::Source { id, text });
        }
        edits.push(Edit::Convert { id, kind });
        Some(self.apply_all(edits))
    }

    /// Make `edits` in order, answering the edits that undo them all, in the order to make them.
    pub fn apply_all(&mut self, edits: Vec<Edit>) -> Vec<Edit> {
        let mut undo: Vec<Edit> = edits.into_iter().filter_map(|e| self.apply(e)).collect();
        undo.reverse();
        undo
    }

    /// Split cell `id` at byte `at` of its source, as VS Code's Split Cell does: what follows
    /// becomes a new cell of its kind below it, the line break between the two goes, and the
    /// outputs stay with the first. Answers the edits that undo it.
    pub fn split(&mut self, id: CellId, at: usize) -> Option<Vec<Edit>> {
        let pos = self.position(id)?;
        let cell = &self.cells[pos];
        let (head, tail) = (cell.source.get(..at)?, cell.source.get(at..)?);
        let head = head.strip_suffix('\n').unwrap_or(head).to_string();
        let tail = tail.strip_prefix('\n').unwrap_or(tail).to_string();
        let new = self.new_cell(self.cells[pos].kind, &tail)?;
        let insert = Edit::Insert {
            at: pos + 1,
            cell: new,
        };
        Some(self.apply_all(vec![Edit::Source { id, text: head }, insert]))
    }

    /// Join cell `id` with the cell below it, of the same kind, their sources a line apart; the
    /// outputs are the first cell's. Answers the edits that undo it.
    pub fn join(&mut self, id: CellId) -> Option<Vec<Edit>> {
        let pos = self.position(id)?;
        let (first, next) = (&self.cells[pos], self.cells.get(pos + 1)?);
        if first.kind != next.kind {
            return None;
        }
        let text = format!("{}\n{}", first.source, next.source);
        let next = next.id;
        Some(self.apply_all(vec![Edit::Source { id, text }, Edit::Remove { id: next }]))
    }

    /// The cells' sources as the index holds them, a line break between each two.
    pub fn sources(&self) -> String {
        let sources: Vec<&str> = self.cells.iter().map(|c| c.source.as_str()).collect();
        sources.join("\n")
    }

    /// The cell whose source in [`sources`](Self::sources) holds byte `at`, by its place, and
    /// where in its source that byte is.
    pub fn locate(&self, at: usize) -> Option<(usize, usize)> {
        let mut start = 0;
        for (i, cell) in self.cells.iter().enumerate() {
            let end = start + cell.source.len();
            if at <= end {
                return Some((i, at - start));
            }
            start = end + 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANALYSIS: &str = include_str!("../../tests/fixtures/notebook/analysis.ipynb");
    const EXPLORE: &str = include_str!("../../tests/fixtures/notebook/explore.py");

    #[test]
    fn every_edit_is_undone_by_the_edit_it_answers() {
        for (rel, text) in [("analysis.ipynb", ANALYSIS), ("explore.py", EXPLORE)] {
            let mut nb = read(rel, text).unwrap();
            let ids: Vec<CellId> = nb.cells().iter().map(Cell::id).collect();
            let new = nb.new_cell(Kind::Markdown, "# New").unwrap();
            let groups = [
                vec![Edit::Source {
                    id: ids[2],
                    text: "edited = True".into(),
                }],
                vec![Edit::Insert { at: 1, cell: new }],
                vec![Edit::Remove { id: ids[0] }],
                vec![Edit::Move { id: ids[3], to: 0 }],
                vec![Edit::Convert {
                    id: ids[4],
                    kind: Kind::Markdown,
                }],
            ];
            for edits in groups {
                let undo = nb.apply_all(edits);
                assert!(!undo.is_empty());
                assert_ne!(nb.to_text(), text);
                nb.apply_all(undo);
                assert_eq!(nb.to_text(), text, "{rel}");
            }
            let undo = nb.split(ids[3], 0).unwrap();
            nb.apply_all(undo);
            let undo = nb.join(ids[3]).unwrap();
            nb.apply_all(undo);
            assert_eq!(nb.to_text(), text, "{rel}");
        }
    }

    #[test]
    fn a_split_cell_joins_back_into_itself() {
        let mut nb = read("analysis.ipynb", ANALYSIS).unwrap();
        let id = nb.cells()[1].id();
        let before = nb.cells()[1].source().to_string();
        let at = before.find("import pandas").unwrap();
        nb.split(id, at).unwrap();
        let (head, tail) = (&nb.cells()[1], &nb.cells()[2]);
        assert_eq!(head.source(), "import numpy as np");
        assert!(
            tail.source().starts_with("import pandas"),
            "{}",
            tail.source()
        );
        assert_eq!(tail.kind(), Kind::Code);
        assert!(tail.outputs().is_empty() && tail.count().is_none());
        nb.join(id).unwrap();
        assert_eq!(nb.cells()[1].source(), before);
        assert_eq!(
            nb.to_text(),
            ANALYSIS,
            "joined back, the cell is as it was read"
        );
    }

    #[test]
    fn a_marimo_notebook_has_no_raw_cells() {
        let mut nb = read("explore.py", EXPLORE).unwrap();
        assert!(nb.new_cell(Kind::Raw, "").is_none());
        let id = nb.cells()[0].id();
        assert!(
            nb.apply(Edit::Convert {
                id,
                kind: Kind::Raw
            })
            .is_none()
        );
    }

    #[test]
    fn the_sources_locate_their_cells() {
        let nb = read("explore.py", EXPLORE).unwrap();
        let sources = nb.sources();
        let at = sources.find("best_amplitude = passing.max()").unwrap();
        let (cell, offset) = nb.locate(at).unwrap();
        assert!(nb.cells()[cell].source()[offset..].starts_with("best_amplitude ="));
        assert_eq!(nb.locate(0), Some((0, 0)));
        assert_eq!(nb.locate(sources.len()), Some((nb.cells().len() - 1, 0)));
        assert_eq!(nb.locate(sources.len() + 1), None);
    }

    #[test]
    fn what_does_not_read_as_a_notebook_says_why() {
        let conflicted = ANALYSIS.replacen(
            "   \"cell_type\": \"markdown\",",
            "<<<<<<< HEAD\n   \"cell_type\": \"markdown\",\n=======\n   \"cell_type\": \"raw\",\n>>>>>>> theirs",
            1,
        );
        let why = |rel, text| match read(rel, text) {
            Err(Error::Invalid(why)) => why,
            other => panic!("{other:?}"),
        };
        assert_eq!(why("a.ipynb", &conflicted), "it holds conflict markers");
        let v3 = r#"{"metadata": {}, "nbformat": 3, "nbformat_minor": 0, "worksheets": []}"#;
        assert!(why("a.ipynb", v3).contains("nbformat 3"));
        assert!(why("a.ipynb", "{").starts_with("it is not JSON"));
        assert!(why("a.py", "print(1)\n").contains("marimo"));
        let unindented = EXPLORE.replace("    amps = np", "amps = np");
        assert!(why("a.py", &unindented).contains("indented"));
    }

    #[test]
    fn crlf_lines_are_read_as_lf_and_written_back_as_crlf() {
        let crlf = EXPLORE.replace('\n', "\r\n");
        let mut nb = read("explore.py", &crlf).unwrap();
        assert!(!nb.cells()[0].source().contains('\r'));
        assert_eq!(nb.to_text(), crlf);
        let id = nb.cells()[0].id();
        nb.apply(Edit::Source {
            id,
            text: "import marimo as mo\nimport numpy as np\nimport polars as pl".into(),
        });
        let text = nb.to_text();
        assert!(text.contains("    import polars as pl\r\n"));
        assert!(!text.replace("\r\n", "").contains('\n'));
    }
}
