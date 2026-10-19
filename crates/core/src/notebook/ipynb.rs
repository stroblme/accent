//! The `.ipynb` backend: nbformat 4's JSON, kept as the file wrote it. Each cell keeps its text
//! and is written back verbatim until its source or its kind changes. An edited source replaces
//! the cell's `source` value alone, split into lines as nbformat splits it, in the file's indent;
//! a cell of a new kind is written whole, as nbformat writes one: keys sorted, the file's indent.

use std::collections::HashSet;
use std::ops::Range;

use serde::{Deserialize, Serialize};
use serde_json::ser::PrettyFormatter;
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::{Attachment, Cell, CellId, Data, Item, Kind, Notebook, Output, invalid};
use crate::Result;

#[derive(Debug, Clone)]
pub(super) struct Frame {
    /// The file up to and including the `[` of `cells`.
    open: String,
    /// The file from the `]` of `cells` on.
    close: String,
    /// What comes between `[` and the first cell, between two cells, and after the last.
    lead: String,
    sep: String,
    trail: String,
    /// One level of the file's indent; `None` for JSON on one line.
    unit: Option<String>,
    /// A cell's indent.
    depth: String,
    /// Whether cells carry ids, as they must from nbformat 4.5 on.
    ids: bool,
    /// What new cells' ids are drawn from, so that two sessions draw different ones.
    seed: blake3::Hash,
}

#[derive(Debug, Clone)]
pub(super) struct Kept {
    raw: String,
    kind: Kind,
    source: String,
    /// Where the `source` value sits in `raw`, and whether it is a list of lines or a string.
    span: Range<usize>,
    listed: bool,
    id: Option<String>,
}

#[derive(Deserialize)]
struct Doc<'a> {
    #[serde(borrow)]
    cells: Option<&'a RawValue>,
    #[serde(default)]
    metadata: Value,
    nbformat: u32,
    #[serde(default)]
    nbformat_minor: u32,
}

#[derive(Deserialize)]
struct RawCell<'a> {
    cell_type: String,
    #[serde(borrow)]
    source: &'a RawValue,
    id: Option<String>,
    execution_count: Option<u32>,
    #[serde(default)]
    outputs: Vec<Value>,
    #[serde(default)]
    attachments: Map<String, Value>,
}

pub(super) fn read(text: &str) -> Result<Notebook> {
    let doc: Doc =
        serde_json::from_str(text).map_err(|e| invalid(format!("it is not JSON: {e}")))?;
    if doc.nbformat != 4 {
        return Err(invalid(format!(
            "it is an nbformat {} notebook, and only nbformat 4 is read",
            doc.nbformat
        )));
    }
    let list = doc.cells.ok_or_else(|| invalid("it has no cells"))?;
    let raws: Vec<&RawValue> =
        serde_json::from_str(list.get()).map_err(|e| invalid(format!("its cells: {e}")))?;
    let at = |inner: &str| inner.as_ptr() as usize - text.as_ptr() as usize;
    let (start, end) = (at(list.get()), at(list.get()) + list.get().len());
    let spans: Vec<Range<usize>> = raws
        .iter()
        .map(|r| at(r.get())..at(r.get()) + r.get().len())
        .collect();
    let line = &text[text[..start].rfind('\n').map_or(0, |i| i + 1)..start];
    let unit = text[..start]
        .contains('\n')
        .then(|| line[..line.len() - line.trim_start().len()].to_string());
    let lead = spans.first().map(|s| &text[start + 1..s.start]);
    let depth = match lead.and_then(|l| l.rsplit_once('\n')) {
        Some((_, indent)) => indent.to_string(),
        None => unit.as_deref().unwrap_or("").repeat(2),
    };
    let lead = match (lead, &unit) {
        (Some(lead), _) => lead.to_string(),
        (None, Some(_)) => format!("\n{depth}"),
        (None, None) => String::new(),
    };
    let frame = Frame {
        open: text[..start + 1].to_string(),
        close: text[end - 1..].to_string(),
        sep: match &spans[..] {
            [a, b, ..] => text[a.end..b.start].to_string(),
            _ => format!(",{lead}"),
        },
        trail: match (spans.last(), &unit) {
            (Some(last), _) => text[last.end..end - 1].to_string(),
            (None, Some(unit)) => format!("\n{unit}"),
            (None, None) => String::new(),
        },
        lead,
        unit,
        depth,
        ids: doc.nbformat_minor >= 5,
        seed: blake3::hash(text.as_bytes()),
    };
    let cells = raws
        .iter()
        .enumerate()
        .map(|(n, raw)| {
            cell(raw.get(), n as u64).map_err(|why| invalid(format!("cell {}: {why}", n + 1)))
        })
        .collect::<Result<Vec<Cell>, _>>()?;
    let meta = |key: &str| doc.metadata.pointer(key).and_then(Value::as_str);
    let language = meta("/language_info/name")
        .or(meta("/kernelspec/language"))
        .unwrap_or("python");
    let extension = match meta("/language_info/file_extension") {
        Some(ext) => ext,
        None if language == "python" => ".py",
        None => "",
    };
    Ok(Notebook {
        frame: super::Frame::Ipynb(frame),
        next: cells.len() as u64,
        cells,
        language: language.to_string(),
        extension: extension.to_string(),
        crlf: false,
    })
}

/// The cell `raw` holds, as the `n`th read.
fn cell(raw: &str, n: u64) -> Result<Cell, String> {
    let c: RawCell = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let kind = match c.cell_type.as_str() {
        "code" => Kind::Code,
        "markdown" => Kind::Markdown,
        "raw" => Kind::Raw,
        other => return Err(format!("no cell is of type {other:?}")),
    };
    let value: Value = serde_json::from_str(c.source.get()).map_err(|e| e.to_string())?;
    let source = text(&value).ok_or("its source is not text")?;
    let start = c.source.get().as_ptr() as usize - raw.as_ptr() as usize;
    Ok(Cell {
        id: CellId(n),
        kind,
        count: c.execution_count,
        outputs: c.outputs.iter().filter_map(output).collect(),
        attachments: c
            .attachments
            .iter()
            .map(|(name, bundle)| Attachment {
                name: name.clone(),
                items: items(bundle),
            })
            .collect(),
        kept: Box::new(super::Kept::Ipynb(Kept {
            raw: raw.to_string(),
            kind,
            source: source.clone(),
            span: start..start + c.source.get().len(),
            listed: value.is_array(),
            id: c.id,
        })),
        source,
    })
}

/// A string, or the lines nbformat splits a string into, joined.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(lines) => lines.iter().map(Value::as_str).collect(),
        _ => None,
    }
}

fn items(bundle: &Value) -> Vec<Item> {
    let Some(bundle) = bundle.as_object() else {
        return Vec::new();
    };
    bundle
        .iter()
        .map(|(mime, data)| Item {
            mime: mime.clone(),
            data: text(data).map_or_else(|| Data::Json(data.clone()), Data::Text),
        })
        .collect()
}

/// One output; `None` for a type nbformat 4 does not have.
fn output(o: &Value) -> Option<Output> {
    let string = |key| o.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    Some(match o.get("output_type")?.as_str()? {
        "stream" => Output::Stream {
            stderr: o.get("name").and_then(Value::as_str) == Some("stderr"),
            text: text(o.get("text")?)?,
        },
        "display_data" | "execute_result" => Output::Data {
            count: o
                .get("execution_count")
                .and_then(Value::as_u64)
                .and_then(|n| n.try_into().ok()),
            items: items(o.get("data")?),
        },
        "error" => Output::Error {
            name: string("ename"),
            value: string("evalue"),
            traceback: o
                .get("traceback")
                .and_then(Value::as_array)
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default(),
        },
        _ => return None,
    })
}

fn kept(cell: &Cell) -> &Kept {
    match cell.kept.as_ref() {
        super::Kept::Ipynb(kept) => kept,
        // `Notebook::apply` takes no cell another format made.
        super::Kept::Marimo(_) => unreachable!("a marimo cell in an .ipynb"),
    }
}

impl Frame {
    pub(super) fn write(&self, cells: &[Cell]) -> String {
        let mut out = self.open.clone();
        for (i, cell) in cells.iter().enumerate() {
            out.push_str(if i == 0 { &self.lead } else { &self.sep });
            let kept = kept(cell);
            match (cell.kind == kept.kind, cell.source == kept.source) {
                (true, true) => out.push_str(&kept.raw),
                (true, false) => {
                    out.push_str(&kept.raw[..kept.span.start]);
                    out.push_str(&self.source(kept, &cell.source));
                    out.push_str(&kept.raw[kept.span.end..]);
                }
                (false, _) => out.push_str(&self.whole(cell.kind, &cell.source, &kept.raw)),
            }
        }
        if !cells.is_empty() {
            out.push_str(&self.trail);
        }
        out.push_str(&self.close);
        out
    }

    /// The `source` value for `source`, as nbformat writes it where `kept` had one.
    fn source(&self, kept: &Kept, source: &str) -> String {
        if !kept.listed {
            return json(&source);
        }
        let before = &kept.raw[..kept.span.start];
        let lines = split_lines(source);
        match (before.rsplit_once('\n'), &self.unit) {
            (Some((_, line)), Some(unit)) if !lines.is_empty() => {
                let indent = &line[..line.len() - line.trim_start().len()];
                let items: Vec<String> = lines
                    .iter()
                    .map(|l| format!("\n{indent}{unit}{}", json(l)))
                    .collect();
                format!("[{}\n{indent}]", items.join(","))
            }
            _ => json(&lines),
        }
    }

    /// A whole cell of `kind` holding `source`, written as nbformat writes one, with whatever
    /// else the cell held in `raw` that a cell of its kind has.
    fn whole(&self, kind: Kind, source: &str, raw: &str) -> String {
        let mut cell: Map<String, Value> = serde_json::from_str(raw).unwrap_or_default();
        let name = match kind {
            Kind::Code => "code",
            Kind::Markdown => "markdown",
            Kind::Raw => "raw",
        };
        cell.insert("cell_type".into(), name.into());
        cell.insert("source".into(), split_lines(source).into());
        cell.entry("metadata").or_insert_with(|| Map::new().into());
        match kind {
            Kind::Code => {
                cell.entry("execution_count").or_insert(Value::Null);
                cell.entry("outputs")
                    .or_insert_with(|| Vec::<Value>::new().into());
                cell.remove("attachments");
            }
            Kind::Markdown | Kind::Raw => {
                cell.remove("execution_count");
                cell.remove("outputs");
            }
        }
        let Some(unit) = &self.unit else {
            return json(&cell);
        };
        let mut out = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(
            &mut out,
            PrettyFormatter::with_indent(unit.as_bytes()),
        );
        // A map of JSON values always serialises, and to UTF-8.
        cell.serialize(&mut ser).expect("JSON serialises");
        let text = String::from_utf8(out).expect("JSON is UTF-8");
        text.replace('\n', &format!("\n{}", self.depth))
    }

    /// How a new cell of `kind` is kept: as nbformat writes an empty one, with an id no cell in
    /// `cells` has where the notebook's cells carry ids.
    pub(super) fn blank(&self, kind: Kind, cells: &[Cell], id: CellId) -> super::Kept {
        let mut raw = Map::new();
        if self.ids {
            let taken: HashSet<&str> = cells.iter().filter_map(|c| kept(c).id.as_deref()).collect();
            let fresh = (0u32..)
                .map(|n| {
                    let mut hash = blake3::Hasher::new();
                    hash.update(self.seed.as_bytes());
                    hash.update(&id.0.to_le_bytes());
                    hash.update(&n.to_le_bytes());
                    hash.finalize().to_hex()[..8].to_string()
                })
                .find(|fresh| !taken.contains(fresh.as_str()));
            raw.insert("id".into(), fresh.into());
        }
        let raw = self.whole(kind, "", &json(&raw));
        match cell(&raw, id.0) {
            Ok(cell) => *cell.kept,
            Err(why) => unreachable!("a new cell does not read back: {why}"),
        }
    }
}

/// `value` as compact JSON, which for a string is how nbformat writes it.
fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("JSON serialises")
}

/// `text` split as Python's `str.splitlines(True)` splits it, which is how nbformat splits a
/// source into its lines: at every line boundary Unicode names, each kept on its line.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = match c {
            '\r' if chars.peek().is_some_and(|&(_, next)| next == '\n') => {
                chars.next();
                i + 2
            }
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => i + c.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::{Edit, read};
    use super::*;

    const ANALYSIS: &str = include_str!("../../tests/fixtures/notebook/analysis.ipynb");

    /// What nbformat 5.10.4 wrote for a small notebook, whose second cell `EDITED` then edits.
    const SMALL: &str = r##"{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "aaaa0001",
   "metadata": {},
   "source": [
    "# Title"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "aaaa0002",
   "metadata": {},
   "outputs": [],
   "source": [
    "x = 1\n",
    "y = 2\n"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 2,
   "id": "aaaa0003",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "1\n"
     ]
    }
   ],
   "source": [
    "print(x)"
   ]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}
"##;

    #[test]
    fn serde_json_sorts_keys_as_nbformat_does() {
        // With serde_json's `preserve_order` on, a cell of a new kind would keep its keys in the
        // file's order rather than nbformat's sorted one.
        let v: Value = serde_json::from_str(r#"{"b": 1, "a": 2}"#).unwrap();
        assert_eq!(json(&v), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn an_nbformat_notebook_is_written_back_byte_for_byte() {
        let nb = read("analysis.ipynb", ANALYSIS).unwrap();
        assert_eq!(nb.to_text(), ANALYSIS);
        assert_eq!((nb.language(), nb.extension()), ("python", ".py"));
        let Notebook {
            frame: super::super::Frame::Ipynb(frame),
            cells,
            ..
        } = &nb
        else {
            unreachable!()
        };
        // Every cell written whole as nbformat writes one is the cell nbformat wrote.
        for cell in cells {
            let kept = kept(cell);
            assert_eq!(frame.whole(cell.kind, &cell.source, &kept.raw), kept.raw);
        }
        let kinds: Vec<Kind> = cells.iter().map(|c| c.kind).collect();
        assert_eq!(kinds[..2], [Kind::Markdown, Kind::Code]);
        assert_eq!(kinds[11], Kind::Raw);
        assert_eq!(cells[10].attachments[0].name, "readout.png");
        assert!(
            matches!(&cells[9].outputs[0], Output::Error { name, .. } if name == "ZeroDivisionError")
        );
        assert!(
            matches!(&cells[2].outputs[0], Output::Stream { stderr: true, text } if text.starts_with("\r  0%"))
        );
        assert_eq!(cells[13].count, None);
    }

    #[test]
    fn an_edited_source_is_written_as_nbformat_writes_it() {
        let mut nb = read("small.ipynb", SMALL).unwrap();
        let id = nb.cells()[1].id();
        let text = "x = \"é\"\ttab\u{2028}y\r\nz\\\n\n".to_string();
        nb.apply(Edit::Source { id, text });
        let edited = "    \"x = \\\"é\\\"\\ttab\u{2028}\",\n    \"y\\r\\n\",\n    \"z\\\\\\n\",\n    \"\\n\"\n";
        assert_eq!(
            nb.to_text(),
            SMALL.replace("    \"x = 1\\n\",\n    \"y = 2\\n\"\n", edited)
        );
        nb.apply(Edit::Source {
            id,
            text: String::new(),
        });
        let empty = SMALL.replace("[\n    \"x = 1\\n\",\n    \"y = 2\\n\"\n   ]", "[]");
        assert_eq!(nb.to_text(), empty);
    }

    #[test]
    fn cells_inserted_deleted_and_moved_leave_the_others_as_they_were() {
        let mut nb = read("analysis.ipynb", ANALYSIS).unwrap();
        let ids: Vec<CellId> = nb.cells().iter().map(Cell::id).collect();
        let code = nb.new_cell(Kind::Code, "print(1)").unwrap();
        let markdown = nb.new_cell(Kind::Markdown, "").unwrap();
        nb.apply(Edit::Insert { at: 0, cell: code });
        nb.apply(Edit::Insert {
            at: 5,
            cell: markdown,
        });
        nb.apply(Edit::Remove { id: ids[3] });
        nb.apply(Edit::Move { id: ids[0], to: 8 });
        nb.apply(Edit::Convert {
            id: ids[1],
            kind: Kind::Markdown,
        });
        let text = nb.to_text();
        let doc: Value = serde_json::from_str(&text).unwrap();
        let cells = doc["cells"].as_array().unwrap();
        assert_eq!(cells.len(), ANALYSIS.matches("\"cell_type\"").count() + 1);
        let unique: HashSet<&str> = cells.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(unique.len(), cells.len(), "ids are unique");
        assert_eq!(cells[0]["source"], serde_json::json!(["print(1)"]));
        assert_eq!(cells[0]["execution_count"], Value::Null);
        // The converted cell keeps its id and metadata but has no outputs.
        let converted = &cells[1];
        assert_eq!(converted["cell_type"], "markdown");
        assert_eq!(converted["id"], "c3b1e2f0");
        assert_eq!(converted["metadata"]["tags"][0], "setup");
        assert!(converted.get("outputs").is_none());

        let again = read("analysis.ipynb", &text).unwrap();
        let ours = read("analysis.ipynb", ANALYSIS).unwrap();
        for cell in again
            .cells()
            .iter()
            .filter(|c| kept(c).id.as_deref() != Some("c3b1e2f0"))
        {
            let raw = &kept(cell).raw;
            let was = ours.cells().iter().find(|c| kept(c).id == kept(cell).id);
            if let Some(was) = was {
                assert_eq!(raw, &kept(was).raw, "an untouched cell keeps its bytes");
            }
        }
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn an_empty_notebook_takes_its_first_cell_in_nbformat_style() {
        let empty = "{\n \"cells\": [],\n \"metadata\": {},\n \"nbformat\": 4,\n \"nbformat_minor\": 4\n}\n";
        let mut nb = read("e.ipynb", empty).unwrap();
        let cell = nb.new_cell(Kind::Code, "").unwrap();
        let id = cell.id();
        nb.apply(Edit::Insert { at: 0, cell });
        let one = "{\n \"cells\": [\n  {\n   \"cell_type\": \"code\",\n   \"execution_count\": null,\n   \"metadata\": {},\n   \"outputs\": [],\n   \"source\": []\n  }\n ],\n \"metadata\": {},\n \"nbformat\": 4,\n \"nbformat_minor\": 4\n}\n";
        assert_eq!(nb.to_text(), one, "no id before nbformat 4.5");
        nb.apply(Edit::Remove { id });
        assert_eq!(nb.to_text(), empty);
    }

    #[test]
    fn lines_split_where_python_splits_them() {
        assert_eq!(
            split_lines("a\nb\r\nc\rd\u{2028}e"),
            ["a\n", "b\r\n", "c\r", "d\u{2028}", "e"]
        );
        assert_eq!(split_lines(""), Vec::<&str>::new());
        assert_eq!(split_lines("a\n"), ["a\n"]);
    }
}
