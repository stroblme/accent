//! The marimo backend: a notebook as the Python module marimo writes, each cell a block of it.
//!
//! A block is a function under `@app.cell`, its body the cell and its `return` marimo's; a
//! function or class under `@app.function` or `@app.class_definition`, which is the cell whole;
//! the `with app.setup:` block; or `app._unparsable_cell(…)`, the cell in a string. A cell that is
//! only `mo.md(…)` of a string is a markdown cell, the string its source. Each block keeps its
//! text and is written back verbatim until its source changes; then its decorator, `def` line and
//! `return` are kept as written (marimo reads neither the parameters nor the `return`), the body
//! indented as marimo indents it, and a markdown cell's call kept around its string.

use std::ops::Range;

use super::{Cell, CellId, Kind, Notebook, invalid};
use crate::Result;

#[derive(Debug, Clone)]
pub(super) struct Frame {
    /// The module up to its first cell: `import marimo`, `__generated_with`, `app = …`.
    header: String,
    /// The blank lines after the last cell, and `if __name__ == "__main__":`.
    footer: String,
    /// The blank lines between two cells, for a new one.
    gap: String,
    /// The file's first markdown cell, which a new one is written like.
    markdown: Option<Kept>,
    /// Whether the file ended with a line break; it is read as if it did.
    newline: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Kept {
    /// The blank lines before the block.
    gap: String,
    /// The block's lines before the cell: its decorator, and a function's `def` line.
    head: String,
    /// How far the cell is indented in the block.
    indent: &'static str,
    /// The string the cell is held in, for a markdown cell and an unparsable one.
    literal: Option<Literal>,
    /// The block's lines after the cell: a function's `return`.
    tail: String,
    kind: Kind,
    source: String,
    /// The block as written, without its gap.
    text: String,
}

/// A triple-quoted string holding a cell, and what is around it in the block's body.
#[derive(Debug, Clone)]
struct Literal {
    /// Before the string: `mo.md(`.
    open: String,
    /// After the opening quotes: a line break or nothing.
    lead: String,
    /// The indent of the source's lines inside the string.
    indent: String,
    /// Before the closing quotes: a line break and the closing line's indent, or nothing.
    trail: String,
    /// After the string: `)`.
    close: String,
}

/// How far marimo indents a cell in its function.
const CELL: &str = "    ";
const QUOTES: &str = "\"\"\"";

/// Whether `text` is a marimo notebook: it imports marimo and makes its `app`.
pub(super) fn sniff(text: &str) -> bool {
    text.lines().any(|l| l.trim_end() == "import marimo")
        && text.lines().any(|l| l.starts_with("app = marimo.App("))
}

/// Whether `line` opens a block.
fn opens(line: &str) -> bool {
    [
        "@app.",
        "with app.setup",
        "async with app.setup",
        "app._unparsable_cell(",
    ]
    .iter()
    .any(|p| line.starts_with(p))
}

fn blank(line: &str) -> bool {
    line.trim().is_empty()
}

pub(super) fn read(text: &str) -> Result<Notebook> {
    if !sniff(text) {
        return Err(invalid("it is not a marimo notebook"));
    }
    let newline = text.ends_with('\n');
    let text = match newline {
        true => text.to_string(),
        false => format!("{text}\n"),
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let end = lines
        .iter()
        .rposition(|l| l.starts_with("if __name__"))
        .unwrap_or(lines.len());
    let starts: Vec<usize> = (0..end).filter(|&i| opens(lines[i])).collect();
    // Where a region's text ends: its blank lines are the gap before what follows it.
    let stop = |mut r: Range<usize>| {
        let start = r.start;
        r.rfind(|&i| !blank(lines[i])).map_or(start, |i| i + 1)
    };
    let first = starts.first().copied().unwrap_or(end);
    let header_end = stop(0..first);
    let header = lines[..header_end].concat();
    let mut gap = lines[header_end..first].concat();
    let mut cells = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let next = starts.get(n + 1).copied().unwrap_or(end);
        let stop = stop(start..next);
        let kept = block(
            &lines[start..stop],
            std::mem::replace(&mut gap, lines[stop..next].concat()),
        )
        .map_err(|why| invalid(format!("cell {}: {why}", n + 1)))?;
        cells.push(Cell {
            id: CellId(n as u64),
            kind: kept.kind,
            source: kept.source.clone(),
            count: None,
            outputs: Vec::new(),
            attachments: Vec::new(),
            kept: Box::new(super::Kept::Marimo(kept)),
        });
    }
    let footer = gap + &lines[end..].concat();
    let kepts = || cells.iter().map(kept);
    let frame = Frame {
        header,
        footer,
        gap: kepts()
            .nth(1)
            .or(kepts().next())
            .map_or("\n\n".to_string(), |k| k.gap.clone()),
        markdown: kepts().find(|k| k.kind == Kind::Markdown).cloned(),
        newline,
    };
    Ok(Notebook {
        frame: super::Frame::Marimo(frame),
        next: cells.len() as u64,
        cells,
        language: "python".into(),
        extension: ".py".into(),
        crlf: false,
    })
}

/// The cell a block's `lines` hold, the blank lines before it being `gap`.
fn block(lines: &[&str], gap: String) -> Result<Kept, &'static str> {
    let first = lines[0];
    let cell = first.starts_with("@app.cell");
    let whole = first.starts_with("@app.function") || first.starts_with("@app.class_definition");
    let unparsable = first.starts_with("app._unparsable_cell(");
    let head = match (cell, whole) {
        (true, _) => signature(lines).ok_or("its `def` line is not where marimo writes it")?,
        (_, true) => decorator(lines),
        _ => 1,
    };
    let tail = match (cell, unparsable) {
        (true, _) => returns(lines, head),
        (_, true) if lines.len() > 1 && lines[lines.len() - 1].trim_end() == ")" => lines.len() - 1,
        (_, true) => return Err("it does not end as marimo ends an unparsable cell"),
        _ => lines.len(),
    };
    let indent = if whole { "" } else { CELL };
    let body = &lines[head..tail];
    if body.iter().any(|l| !blank(l) && !l.starts_with(indent)) {
        return Err("it is not indented as marimo indents a cell");
    }
    let code: String = body
        .iter()
        .map(|l| {
            l.strip_prefix(indent)
                .unwrap_or(l.trim_start_matches([' ', '\t']))
        })
        .collect();
    let code = code.strip_suffix('\n').unwrap_or(&code);
    let literal = match (cell, unparsable) {
        (true, _) => markdown(code),
        (_, true) => Some(
            literal(code, |open| open.trim().is_empty(), |_| true)
                .ok_or("its string is not one marimo writes")?,
        ),
        _ => None,
    };
    let kind = match (cell, &literal) {
        (true, Some(_)) => Kind::Markdown,
        _ => Kind::Code,
    };
    let (literal, source) = match literal {
        Some((literal, source)) => (Some(literal), source),
        None => (None, code.to_string()),
    };
    Ok(Kept {
        gap,
        head: lines[..head].concat(),
        indent,
        literal,
        tail: lines[tail..].concat(),
        kind,
        source,
        text: lines.concat(),
    })
}

/// How many of a cell function's lines are its decorator and its `def` line, however many lines
/// marimo broke the parameters over.
fn signature(lines: &[&str]) -> Option<usize> {
    let (mut depth, mut def) = (0, false);
    for (i, line) in lines.iter().enumerate() {
        def |= depth == 0 && (line.starts_with("def ") || line.starts_with("async def "));
        depth += brackets(line);
        if def && depth == 0 && line.trim_end().ends_with(':') {
            return Some(i + 1);
        }
    }
    None
}

/// How many lines the decorator `lines` open with takes.
fn decorator(lines: &[&str]) -> usize {
    let mut depth = 0;
    for (i, line) in lines.iter().enumerate() {
        depth += brackets(line);
        if depth <= 0 {
            return i + 1;
        }
    }
    lines.len()
}

fn brackets(line: &str) -> i32 {
    line.chars()
        .map(|c| match c {
            '(' | '[' | '{' => 1,
            ')' | ']' | '}' => -1,
            _ => 0,
        })
        .sum()
}

/// Where a cell function's closing `return` starts, the blank lines before it with it; past the
/// end when it has none.
fn returns(lines: &[&str], from: usize) -> usize {
    for i in (from..lines.len()).rev() {
        let line = lines[i];
        let rest = line.strip_prefix("    return");
        if rest.is_some_and(|r| blank(r) || r.starts_with([' ', '('])) {
            let blanks = lines[from..i].iter().rev().take_while(|l| blank(l)).count();
            return i - blanks;
        }
        // Below the `return` there are only its own lines, deeper or closing its brackets.
        let continued = blank(line)
            || line.starts_with("     ")
            || line
                .strip_prefix(CELL)
                .is_some_and(|r| r.starts_with([')', ']']));
        if !continued {
            break;
        }
    }
    lines.len()
}

/// The triple-quoted string `code` is, `open_ok` and `close_ok` saying whether what is before
/// and after it may be, and the source it holds: dedented, its line breaks at either end left
/// out.
fn literal(
    code: &str,
    open_ok: impl Fn(&str) -> bool,
    close_ok: impl Fn(&str) -> bool,
) -> Option<(Literal, String)> {
    let (start, end) = (code.find(QUOTES)?, code.rfind(QUOTES)?);
    if end < start + QUOTES.len() {
        return None;
    }
    let (open, raw) = match code[..start].strip_suffix(['r', 'R']) {
        Some(open) => (open, true),
        None => (&code[..start], false),
    };
    let (content, close) = (
        &code[start + QUOTES.len()..end],
        &code[end + QUOTES.len()..],
    );
    if !open_ok(open) || !close_ok(close) || content.contains(QUOTES) {
        return None;
    }
    let (lead, content) = match content.strip_prefix('\n') {
        Some(content) => ("\n", content),
        None => ("", content),
    };
    let (content, trail) = match content.rsplit_once('\n') {
        Some((before, last)) if blank(last) => (before, &content[before.len()..]),
        _ => (content, ""),
    };
    let lines = content.split('\n');
    let indent = lines
        .clone()
        .filter(|l| !blank(l))
        .map(|l| &l[..l.len() - l.trim_start().len()])
        .min_by_key(|indent| indent.len())
        .filter(|indent| {
            content
                .split('\n')
                .all(|l| blank(l) || l.starts_with(indent))
        })
        .unwrap_or("");
    let dedented: Vec<&str> = lines.map(|l| l.strip_prefix(indent).unwrap_or(l)).collect();
    let dedented = dedented.join("\n");
    let source = match raw {
        true => dedented,
        false => unescape(&dedented)?,
    };
    let literal = Literal {
        open: open.to_string(),
        lead: lead.to_string(),
        indent: indent.to_string(),
        trail: trail.to_string(),
        close: close.to_string(),
    };
    Some((literal, source))
}

/// The `mo.md(…)` call `code` is, and the markdown its string holds.
fn markdown(code: &str) -> Option<(Literal, String)> {
    literal(
        code,
        |open| open.trim() == "mo.md(",
        |close| close.trim() == ")",
    )
}

/// `mo.md(…)` as marimo writes it.
fn md_call() -> Literal {
    Literal {
        open: "mo.md(".into(),
        lead: "\n".into(),
        indent: String::new(),
        trail: "\n".into(),
        close: ")".into(),
    }
}

/// The code a markdown cell made a code cell is: the `mo.md(…)` call it was.
pub(super) fn to_code(markdown: &str) -> String {
    md_call().render(markdown)
}

/// The markdown a code cell made a markdown cell is: the string of the `mo.md(…)` call it is, or
/// else its code as markdown, as marimo converts one.
pub(super) fn to_markdown(code: &str) -> String {
    markdown(code).map_or_else(|| code.to_string(), |(_, source)| source)
}

/// The text a plain string written with no escapes but `\\` and `\"` holds, as marimo writes one
/// a raw string cannot hold; `None` for any other escape.
fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                c @ ('\\' | '"') => out.push(c),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

impl Literal {
    /// The string holding `source`: raw, as marimo writes it, unless a raw string cannot hold it.
    fn render(&self, source: &str) -> String {
        let raw =
            !source.contains(QUOTES) && !(self.trail.is_empty() && source.ends_with(['"', '\\']));
        let text = match raw {
            true => source.to_string(),
            false => source.replace('\\', "\\\\").replace('"', "\\\""),
        };
        let lines: Vec<String> = text
            .split('\n')
            .map(|l| match blank(l) {
                true => l.to_string(),
                false => format!("{}{l}", self.indent),
            })
            .collect();
        let prefix = if raw { "r" } else { "" };
        let (open, lead, trail, close) = (&self.open, &self.lead, &self.trail, &self.close);
        format!(
            "{open}{prefix}{QUOTES}{lead}{}{trail}{QUOTES}{close}",
            lines.join("\n")
        )
    }
}

impl Kept {
    /// The block holding `source`, set apart by `gap`, with this one's head and tail.
    fn render(&self, gap: &str, source: &str) -> String {
        let body = match &self.literal {
            Some(literal) => literal.render(source),
            None => source.to_string(),
        };
        let mut out = format!("{gap}{}", self.head);
        if !body.is_empty() {
            for line in body.split('\n') {
                if !blank(line) {
                    out.push_str(self.indent);
                }
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(&self.tail);
        out
    }
}

fn kept(cell: &Cell) -> &Kept {
    match cell.kept.as_ref() {
        super::Kept::Marimo(kept) => kept,
        // `Notebook::apply` takes no cell another format made.
        super::Kept::Ipynb(_) => unreachable!("an .ipynb cell in a marimo notebook"),
    }
}

impl Frame {
    pub(super) fn write(&self, cells: &[Cell]) -> String {
        let mut out = self.header.clone();
        for cell in cells {
            let kept = kept(cell);
            match (cell.kind == kept.kind, cell.source == kept.source) {
                (true, true) => {
                    out.push_str(&kept.gap);
                    out.push_str(&kept.text);
                }
                (true, false) => out.push_str(&kept.render(&kept.gap, &cell.source)),
                (false, _) => {
                    out.push_str(&self.template(cell.kind).render(&kept.gap, &cell.source))
                }
            }
        }
        out.push_str(&self.footer);
        if !self.newline {
            out.pop();
        }
        out
    }

    /// How a cell of `kind` that was not one is written: a code cell as marimo writes a new one,
    /// a markdown cell like the file's first, or as marimo writes one.
    fn template(&self, kind: Kind) -> Kept {
        let (head, literal) = match kind {
            Kind::Markdown => match &self.markdown {
                Some(kept) => return kept.clone(),
                None => ("@app.cell(hide_code=True)\ndef _(mo):\n", Some(md_call())),
            },
            Kind::Code | Kind::Raw => ("@app.cell\ndef _():\n", None),
        };
        Kept {
            gap: self.gap.clone(),
            head: head.into(),
            indent: CELL,
            literal,
            tail: format!("{CELL}return\n"),
            kind,
            source: String::new(),
            text: String::new(),
        }
    }

    /// How a new cell of `kind` is kept: written as [`template`](Self::template) has it, empty.
    pub(super) fn blank(&self, kind: Kind) -> super::Kept {
        let mut kept = self.template(kind);
        kept.gap = self.gap.clone();
        kept.source = String::new();
        kept.text = kept.render("", "");
        super::Kept::Marimo(kept)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Edit, read};
    use super::*;

    const EXPLORE: &str = include_str!("../../tests/fixtures/notebook/explore.py");

    #[test]
    fn a_marimo_notebook_is_written_back_byte_for_byte() {
        let nb = read("explore.py", EXPLORE).unwrap();
        assert_eq!(nb.to_text(), EXPLORE);
        let cells = nb.cells();
        let kinds: Vec<Kind> = cells.iter().map(Cell::kind).collect();
        use Kind::{Code, Markdown};
        let all = [
            Code, Markdown, Code, Code, Code, Code, Code, Code, Code, Code, Code,
        ];
        assert_eq!(kinds, all, "an f-string is code");
        assert_eq!(cells[0].source(), "import marimo as mo\nimport numpy as np");
        assert_eq!(
            cells[1].source(),
            "# Explore the sweep\n\n\
             Readout fidelity against drive amplitude, $F = 1 - \\frac{p_{01} + p_{10}}{2}$."
        );
        assert!(
            cells[2]
                .source()
                .starts_with("def fidelity(p01, p10):\n    \"\"\"Readout")
        );
        assert_eq!(
            cells[3].source(),
            "import polars as pl",
            "the blank line is the return's"
        );
        assert!(cells[6].source().starts_with("passing = amplitudes["));
        assert!(
            cells[6]
                .source()
                .ends_with("calibration_table_by_qubit.head()")
        );
        assert_eq!(cells[9].source(), "def unfinished(amps:");
        assert_eq!(cells[10].source(), "");
        // Without a final line break, the file is written without one.
        let bare = EXPLORE.trim_end();
        assert_eq!(read("explore.py", bare).unwrap().to_text(), bare);
    }

    /// The block of `nb`'s cell `n` once its source is `text`.
    fn edited(nb: &mut Notebook, n: usize, text: &str) -> String {
        let id = nb.cells()[n].id();
        let undo = nb
            .apply(Edit::Source {
                id,
                text: text.into(),
            })
            .unwrap();
        let out = nb.to_text();
        nb.apply(undo);
        out
    }

    #[test]
    fn an_edited_cell_keeps_its_def_line_and_its_return() {
        let mut nb = read("explore.py", EXPLORE).unwrap();
        let body = "passing = amplitudes[readout_errors < 0.1]\nbest_amplitude = passing.max()";
        let was = "    passing = amplitudes[fidelity(readout_errors, readout_errors) > fidelity_threshold]\n    best_amplitude = passing.max()\n    calibration_table_by_qubit.head()\n";
        let now =
            "    passing = amplitudes[readout_errors < 0.1]\n    best_amplitude = passing.max()\n";
        assert_eq!(edited(&mut nb, 6, body), EXPLORE.replace(was, now));

        let was = "    import polars as pl\n\n    return (pl,)";
        let now = "    import polars as pl\n\n    if True:\n\n        pass\n\n    return (pl,)";
        let body = "import polars as pl\n\nif True:\n\n    pass";
        assert_eq!(edited(&mut nb, 3, body), EXPLORE.replace(was, now));

        let was = "    def unfinished(amps:\n";
        let now = "    def unfinished(amps):\n        return amps\n";
        let body = "def unfinished(amps):\n    return amps";
        assert_eq!(edited(&mut nb, 9, body), EXPLORE.replace(was, now));
    }

    #[test]
    fn an_edited_markdown_cell_keeps_its_call() {
        let mut nb = read("explore.py", EXPLORE).unwrap();
        let start = EXPLORE.find("    mo.md(r\"\"\"\n    # Explore").unwrap();
        let end = start + EXPLORE[start..].find("\"\"\")\n").unwrap() + 5;
        let was = &EXPLORE[start..end];
        let md = |body| EXPLORE.replace(was, body);
        assert_eq!(
            edited(&mut nb, 1, "# Sweep\n\n$\\alpha$"),
            md("    mo.md(r\"\"\"\n    # Sweep\n\n    $\\alpha$\n    \"\"\")\n")
        );
        // What a raw string cannot hold is written escaped, and read back the same.
        let text = edited(&mut nb, 1, "Say \"\"\"hi\"\"\" \\o/");
        assert!(text.contains(
            "    mo.md(\"\"\"\n    Say \\\"\\\"\\\"hi\\\"\\\"\\\" \\\\o/\n    \"\"\")\n"
        ));
        let again = read("explore.py", &text).unwrap();
        assert_eq!(again.cells()[1].kind(), Kind::Markdown);
        assert_eq!(again.cells()[1].source(), "Say \"\"\"hi\"\"\" \\o/");
    }

    #[test]
    fn a_new_cell_is_written_as_marimo_writes_one() {
        let mut nb = read("explore.py", EXPLORE).unwrap();
        let at = nb.cells().len();
        let code = nb.new_cell(Kind::Code, "x = 1").unwrap();
        let empty = nb.new_cell(Kind::Code, "").unwrap();
        let markdown = nb.new_cell(Kind::Markdown, "# More").unwrap();
        nb.apply_all(vec![
            Edit::Insert { at, cell: code },
            Edit::Insert {
                at: at + 1,
                cell: empty,
            },
            Edit::Insert {
                at: at + 2,
                cell: markdown,
            },
        ]);
        let added = "\n\n@app.cell\ndef _():\n    x = 1\n    return\n\
            \n\n@app.cell\ndef _():\n    return\n\
            \n\n@app.cell(hide_code=True)\ndef _():\n    mo.md(r\"\"\"\n    # More\n    \"\"\")\n    return\n";
        let footer = "\n\nif __name__";
        assert_eq!(
            nb.to_text(),
            EXPLORE.replacen(footer, &format!("{added}{footer}"), 1)
        );

        // A code cell made markdown is written as the file's markdown cells are.
        let mut nb = read("explore.py", EXPLORE).unwrap();
        let id = nb.cells()[10].id();
        nb.apply(Edit::Source {
            id,
            text: "*fin*".into(),
        });
        nb.apply(Edit::Convert {
            id,
            kind: Kind::Markdown,
        });
        let was = "@app.cell\ndef _():\n    return\n\n\nif";
        let now = "@app.cell(hide_code=True)\ndef _():\n    mo.md(r\"\"\"\n    *fin*\n    \"\"\")\n    return\n\n\nif";
        assert_eq!(nb.to_text(), EXPLORE.replace(was, now));

        // A markdown cell made code is the `mo.md(…)` call it was; made markdown again, it is the
        // cell that was read.
        let mut nb = read("explore.py", EXPLORE).unwrap();
        let id = nb.cells()[1].id();
        nb.convert(id, Kind::Code).unwrap();
        let code = nb.cells()[1].source();
        assert!(
            code.starts_with("mo.md(r\"\"\"\n# Explore the sweep\n"),
            "{code}"
        );
        nb.convert(id, Kind::Markdown).unwrap();
        assert_eq!(nb.to_text(), EXPLORE);
    }

    #[test]
    fn an_older_markdown_call_keeps_its_layout() {
        let old = "import marimo\napp = marimo.App()\n\n\n@app.cell\ndef _(mo):\n    mo.md(\n        r\"\"\"\n        # Old\n        \"\"\"\n    )\n    return\n";
        let mut nb = read("old.py", old).unwrap();
        assert_eq!(nb.cells()[0].source(), "# Old");
        let new = "import marimo\napp = marimo.App()\n\n\n@app.cell\ndef _(mo):\n    mo.md(\n        r\"\"\"\n        # New\n\n        more\n        \"\"\"\n    )\n    return\n";
        assert_eq!(edited(&mut nb, 0, "# New\n\nmore"), new);
    }
}
