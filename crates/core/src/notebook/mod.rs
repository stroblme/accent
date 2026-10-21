//! Notebooks: a Jupyter notebook (`.ipynb`, nbformat 4) or a marimo notebook (a `.py` marimo
//! wrote) as one model of cells, read and written by a backend per format. Nothing is run
//! here: what a kernel's run leaves in an `.ipynb`'s cells comes in as a [`Run`].
//!
//! A save writes back only what changed: each cell keeps the text it was read from, written as
//! it was until its source or its kind changes, and then only its source is rewritten, in the
//! format's own style. Every change goes through [`Notebook::apply`], which answers the edit that
//! undoes it, so one history can hold the cell operations and the typing in the cells alike.

mod ipynb;
mod marimo;
pub mod output;
mod program;

use std::borrow::Cow;

use serde_json::Value;

pub use ipynb::EMPTY as EMPTY_IPYNB;
pub use output::{Attachment, Data, Item, Output};
pub use program::Program;

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
    /// What a kernel left in the cell; `None` while it shows what the file stored.
    run: Option<Box<Run>>,
}

/// What a kernel left in a code cell: its count, and its outputs as nbformat 4 JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    count: Option<u32>,
    outputs: Vec<Value>,
    /// Kernel-only IDs, one per output; nbformat does not store `transient`.
    display_ids: Vec<Option<String>>,
    /// `outputs` read, each one that reads.
    shown: Vec<Output>,
}

impl Run {
    pub fn new(count: Option<u32>, outputs: Vec<Value>) -> Run {
        let shown = outputs.iter().filter_map(output::read).collect();
        Run {
            count,
            display_ids: vec![None; outputs.len()],
            outputs,
            shown,
        }
    }

    /// Add `output`, a stream running on into the last output when that is a stream of the same
    /// name, as Jupyter's output area joins them: `true` when the last output grew.
    fn push(&mut self, output: Value, display_id: Option<String>) -> bool {
        let stream = |o: &Value| match o["output_type"] == "stream" {
            true => o["name"].as_str().map(String::from),
            false => None,
        };
        if let (Some(last), Some(more)) = (self.outputs.last_mut(), output["text"].as_str())
            && stream(last).is_some()
            && stream(last) == stream(&output)
            && let Some(Value::String(text)) = last.get_mut("text")
        {
            text.push_str(more);
            // A stream with text reads, so the last output shown is this one.
            if let Some(Output::Stream { text, .. }) = self.shown.last_mut() {
                text.push_str(more);
            }
            return true;
        }
        self.shown.extend(output::read(&output));
        self.outputs.push(output);
        self.display_ids.push(display_id);
        false
    }

    /// Replace every output the kernel named `display_id`; return the first slot changed.
    fn update_display(&mut self, display_id: &str, output: &Value) -> Option<usize> {
        let mut first = None;
        for (at, (stored, id)) in self.outputs.iter_mut().zip(&self.display_ids).enumerate() {
            if id.as_deref() == Some(display_id) {
                *stored = output.clone();
                first.get_or_insert(at);
            }
        }
        if first.is_some() {
            self.shown = self.outputs.iter().filter_map(output::read).collect();
        }
        first
    }
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

    /// The execution count, shown as `[n]`: its run's, else the one the file stored.
    pub fn count(&self) -> Option<u32> {
        self.run.as_ref().map_or(self.count, |run| run.count)
    }

    /// The outputs: its run's, else the ones the file stored, which a save writes back as they
    /// were.
    pub fn outputs(&self) -> &[Output] {
        self.run.as_ref().map_or(&self.outputs, |run| &run.shown)
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
    /// Set what a kernel left in a code cell, `None` for what the file stored; an `.ipynb`'s.
    Outputs {
        id: CellId,
        run: Option<Run>,
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
            run: None,
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
            Edit::Outputs { id, run } => {
                let at = self.position(id).filter(|&at| self.runs(at))?;
                let run = std::mem::replace(&mut self.cells[at].run, run.map(Box::new));
                Edit::Outputs {
                    id,
                    run: run.map(|run| *run),
                }
            }
        })
    }

    /// Whether the `at`th cell holds what a kernel leaves: an `.ipynb`'s code cell. A marimo
    /// notebook stores no outputs.
    fn runs(&self, at: usize) -> bool {
        self.format() == Format::Ipynb && self.cells[at].kind == Kind::Code
    }

    /// Add `output`, nbformat 4 JSON, to cell `id`'s run, a stream running on into the stream
    /// before it of the same name: `Some(true)` when the last output grew, `Some(false)` when
    /// one was added, `None` for a cell without a run. Outside the history: a run is the
    /// kernel's, not an edit; only [`Edit::Outputs`] answers its undo.
    pub fn push_output(
        &mut self,
        id: CellId,
        output: Value,
        display_id: Option<String>,
    ) -> Option<bool> {
        let at = self.position(id)?;
        Some(self.cells[at].run.as_mut()?.push(output, display_id))
    }

    /// Replace every live display with this ID, returning each cell's first changed slot.
    pub fn update_display(&mut self, display_id: &str, output: Value) -> Vec<(CellId, usize)> {
        self.cells
            .iter_mut()
            .filter_map(|cell| {
                let at = cell.run.as_mut()?.update_display(display_id, &output)?;
                Some((cell.id, at))
            })
            .collect()
    }

    /// Set cell `id`'s run's count; `None` for a cell without a run. Outside the history: a run
    /// is the kernel's, not an edit; only [`Edit::Outputs`] answers its undo.
    pub fn set_count(&mut self, id: CellId, count: Option<u32>) -> Option<()> {
        let at = self.position(id)?;
        self.cells[at].run.as_mut()?.count = count;
        Some(())
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

    /// Where each cell's source starts in [`sources`](Self::sources).
    pub fn starts(&self) -> Vec<usize> {
        let mut start = 0;
        let mut starts = Vec::with_capacity(self.cells.len());
        for cell in &self.cells {
            starts.push(start);
            start += cell.source.len() + 1;
        }
        starts
    }

    /// The cell whose source in [`sources`](Self::sources) holds byte `at`, by its place, and
    /// where in its source that byte is.
    pub fn locate(&self, at: usize) -> Option<(usize, usize)> {
        let starts = self.starts();
        let cell = starts.partition_point(|&s| s <= at).checked_sub(1)?;
        let offset = at - starts[cell];
        (offset <= self.cells[cell].source.len()).then_some((cell, offset))
    }

    /// The cell holding byte `at` of what the index holds of this notebook, by its place, and the
    /// byte of its source that is: an `.ipynb`'s [`sources`](Self::sources), and a marimo
    /// notebook's file as [`to_text`](Self::to_text) writes it with LF line breaks.
    pub fn place(&self, at: usize) -> Option<(usize, usize)> {
        match &self.frame {
            Frame::Ipynb(_) => self.locate(at),
            Frame::Marimo(frame) => frame.place(&self.cells, at),
        }
    }

    /// The JSON bytes spelling an indexed `.ipynb` source hit, for Open as Text.
    pub fn json_hit(&self, hit: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        let Frame::Ipynb(_) = &self.frame else {
            return None;
        };
        let (cell, from) = self.locate(hit.start)?;
        let (last, to) = self.locate(hit.end)?;
        if cell != last || from > to {
            return None;
        }
        ipynb::json_hit(&self.cells[cell], from..to)
    }
}

/// What the index holds of the file `rel` holding `text` when it is an `.ipynb`: its cells'
/// [`sources`](Notebook::sources), as neither an output's base64 nor a metadata key is worth
/// finding. `None` for any other file, a marimo notebook being the Python it is, and for an
/// `.ipynb` that does not read, which is indexed as its text.
pub fn search_text(rel: &str, text: &str) -> Option<String> {
    match path::is_notebook(rel) {
        true => read(rel, text).ok().map(|nb| nb.sources()),
        false => None,
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
            let mut groups = vec![
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
            if nb.format() == Format::Ipynb {
                let output =
                    serde_json::json!({"output_type": "stream", "name": "stdout", "text": "x"});
                let run = Some(Run::new(Some(7), vec![output]));
                groups.push(vec![Edit::Outputs { id: ids[2], run }]);
            }
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
    fn an_indexed_hit_locates_its_json_source_when_opened_as_text() {
        let text = r#"{"nbformat":4,"cells":[{"cell_type":"code","source":["first\n","h\u00e9llo \"world\""]},{"cell_type":"code","source":"later"},  {"cell_type":"code","source":"final"}]}"#;
        let nb = read("sample.ipynb", text).unwrap();
        for (source, encoded) in [
            ("héllo", r"h\u00e9llo"),
            ("world", "world"),
            ("later", "later"),
            ("final", "final"),
        ] {
            let start = nb.sources().find(source).unwrap();
            let hit = nb.json_hit(start..start + source.len()).unwrap();
            assert_eq!(&text[hit], encoded);
        }
        let source = nb.sources();
        for (needle, encoded) in [("first\n", r"first\n"), ("\"world\"", r#"\"world\""#)] {
            let start = source.find(needle).unwrap();
            let hit = nb.json_hit(start..start + needle.len()).unwrap();
            assert_eq!(&text[hit], encoded);
        }
    }

    #[test]
    fn streams_run_on_as_jupyter_joins_them() {
        let mut nb = read("analysis.ipynb", ANALYSIS).unwrap();
        let id = nb.cells()[2].id();
        let run = Some(Run::new(None, Vec::new()));
        nb.apply(Edit::Outputs { id, run });
        let stream =
            |name, text| serde_json::json!({"output_type": "stream", "name": name, "text": text});
        let pushed: Vec<Option<bool>> = [
            ("stdout", "a"),
            ("stdout", "b\n"),
            ("stderr", "x"),
            ("stdout", "c"),
        ]
        .into_iter()
        .map(|(name, text)| nb.push_output(id, stream(name, text), None))
        .collect();
        assert_eq!(pushed, [Some(false), Some(true), Some(false), Some(false)]);
        let out = |stderr, text: &str| Output::Stream {
            stderr,
            text: text.into(),
        };
        let outputs = [out(false, "ab\n"), out(true, "x"), out(false, "c")];
        assert_eq!(nb.cells()[2].outputs(), outputs);
        nb.set_count(id, Some(4));
        assert_eq!(nb.cells()[2].count(), Some(4));
    }

    #[test]
    fn a_display_update_replaces_every_copy_without_saving_its_id() {
        let mut nb = read("empty.ipynb", EMPTY_IPYNB).unwrap();
        let ids: Vec<CellId> = (0..2)
            .map(|at| {
                let cell = nb.new_cell(Kind::Code, "display(value)").unwrap();
                let id = cell.id();
                nb.apply(Edit::Insert { at, cell });
                nb.apply(Edit::Outputs {
                    id,
                    run: Some(Run::new(None, Vec::new())),
                });
                id
            })
            .collect();
        let display = |text| {
            serde_json::json!({
                "output_type": "display_data", "data": {"text/plain": text}, "metadata": {}
            })
        };
        for &id in &ids {
            nb.push_output(id, display("before"), Some("shared".into()));
        }
        nb.push_output(ids[0], display("before"), Some("shared".into()));
        let changed = nb.update_display("shared", display("after"));
        assert_eq!(changed, [(ids[0], 0), (ids[1], 0)]);
        let expected = Output::Data {
            count: None,
            items: vec![Item {
                mime: "text/plain".into(),
                data: Data::Text("after".into()),
            }],
        };
        assert_eq!(
            nb.cells()[0].outputs(),
            [expected.clone(), expected.clone()]
        );
        assert_eq!(nb.cells()[1].outputs(), [expected]);
        let saved = nb.to_text();
        assert!(saved.contains("after"));
        assert!(!saved.contains("before"));
        assert!(!saved.contains("transient"));
        assert!(!saved.contains("display_id"));
    }

    #[test]
    fn a_byte_the_index_holds_is_placed_in_its_cell() {
        let nb = read("analysis.ipynb", ANALYSIS).unwrap();
        let at = nb.sources().find("binomial").unwrap();
        assert_eq!(nb.place(at), nb.locate(at));
        assert_eq!(nb.place(at).map(|(cell, _)| cell), Some(2));

        let nb = read("explore.py", EXPLORE).unwrap();
        let place = |text: &str| nb.place(EXPLORE.find(text).unwrap());
        for (text, cell) in [
            ("best_amplitude = passing.max()", 6),
            ("Readout fidelity against", 1),
        ] {
            let (at, offset) = place(text).unwrap();
            let source = nb.cells()[at].source();
            assert!(at == cell && source[offset..].starts_with(text), "{text}");
        }
        // An `@app.function`'s `def` line is its source; above and below the source are its ends.
        assert_eq!(place("def fidelity(p01"), Some((2, 0)));
        assert_eq!(place("@app.cell\ndef _(\n"), Some((6, 0)));
        let end = nb.cells()[6].source().len();
        assert_eq!(place("return (best_amplitude,)"), Some((6, end)));
        assert_eq!(place("app = marimo.App("), None, "the header is no cell's");
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
