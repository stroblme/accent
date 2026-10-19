//! What a cell's outputs hold, and the form a window shows each in, decided here so that every
//! window decides alike: which of an output's types to show, a pandas or polars table read off
//! its HTML, a stream's carriage returns played out, and ANSI colours as spans.

use std::ops::Range;

use serde_json::Value;

/// One output of a code cell, as the notebook stored it.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Stream {
        stderr: bool,
        text: String,
    },
    /// A display or a cell's result: the same thing in several types, a window showing one.
    Data {
        count: Option<u32>,
        items: Vec<Item>,
    },
    Error {
        name: String,
        value: String,
        traceback: Vec<String>,
    },
}

/// One type of an output or an attachment: a picture's data is base64, an SVG's is its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub mime: String,
    pub data: Data,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Text(String),
    /// `application/json` and its kin, stored as JSON rather than as a string.
    Json(Value),
}

/// A picture a markdown cell carries in the notebook, named as its `attachment:` link names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub name: String,
    pub items: Vec<Item>,
}

/// The form an output is shown in.
#[derive(Debug, Clone, PartialEq)]
pub enum Form<'a> {
    /// A PNG or JPEG as checked base64, or an SVG's text.
    Picture {
        mime: &'a str,
        data: &'a str,
    },
    Table(Table),
    Markdown(&'a str),
    Latex(&'a str),
    /// Pretty-printed.
    Json(String),
    Text(&'a str),
}

/// The first form of `items` a window draws natively: a picture, a plain HTML table, markdown,
/// LaTeX, JSON, then plain text (user decision 2026-10-19). `None` means only a script could show
/// it, an interactive plot or a widget, and the window says what it was.
pub fn pick(items: &[Item]) -> Option<Form<'_>> {
    let item = |mime: &str| items.iter().find(|i| i.mime == mime);
    let text = |mime: &str| match item(mime).map(|i| &i.data) {
        Some(Data::Text(t)) => Some(t.as_str()),
        _ => None,
    };
    for mime in ["image/png", "image/jpeg"] {
        if let Some(data) = text(mime).filter(|d| is_base64(d)) {
            return Some(Form::Picture { mime, data });
        }
    }
    if let Some(data) = text("image/svg+xml") {
        let mime = "image/svg+xml";
        return Some(Form::Picture { mime, data });
    }
    if let Some(t) = text("text/html").and_then(table) {
        return Some(Form::Table(t));
    }
    if let Some(t) = text("text/markdown") {
        return Some(Form::Markdown(t));
    }
    if let Some(t) = text("text/latex") {
        return Some(Form::Latex(t));
    }
    if let Some(json) = item("application/json") {
        return Some(Form::Json(match &json.data {
            Data::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            Data::Text(t) => t.clone(),
        }));
    }
    text("text/plain").map(Form::Text)
}

/// Whether `data` is base64 as a notebook stores a picture: the standard alphabet, line breaks
/// anywhere, `=` padding only at the end. A decoder that skips what it does not know would make
/// a picture of anything.
pub fn is_base64(data: &str) -> bool {
    let bytes: Vec<u8> = data.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let body = bytes.iter().rposition(|&b| b != b'=').map_or(0, |i| i + 1);
    !bytes.is_empty()
        && bytes.len().is_multiple_of(4)
        && bytes.len() - body <= 2
        && bytes[..body]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/'))
}

/// A stream's text as a terminal leaves it: a carriage return goes back to the start of its
/// line, and what follows writes over it, as a progress bar redraws itself.
pub fn carriage(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = match line.strip_suffix('\n') {
            Some(body) => (body.strip_suffix('\r').unwrap_or(body), true),
            None => (line, false),
        };
        let mut shown: Vec<char> = Vec::new();
        let mut at = 0;
        for c in body.chars() {
            if c == '\r' {
                at = 0;
                continue;
            }
            match shown.get_mut(at) {
                Some(old) => *old = c,
                None => shown.push(c),
            }
            at += 1;
        }
        out.extend(shown);
        if newline {
            out.push('\n');
        }
    }
    out
}

/// A run of text an escape sequence styled: `colour` is one of the terminal's 16, from 0 (black)
/// to 15 (bright white).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub colour: Option<u8>,
    pub bold: bool,
}

/// `text` without its escape sequences, and the runs their SGR codes styled with the 16 colours
/// and bold. Everything else, 256 colours past the first 16, true colour, backgrounds and
/// underline among them, is dropped.
pub fn ansi(text: &str) -> (String, Vec<Span>) {
    let mut out = String::with_capacity(text.len());
    let mut spans: Vec<Span> = Vec::new();
    let (mut colour, mut bold) = (None, false);
    let mut rest = text;
    loop {
        let (plain, escape) = rest.split_once('\x1b').unwrap_or((rest, ""));
        if !plain.is_empty() && (colour.is_some() || bold) {
            let range = out.len()..out.len() + plain.len();
            match spans.last_mut() {
                Some(last)
                    if last.range.end == range.start
                        && (last.colour, last.bold) == (colour, bold) =>
                {
                    last.range.end = range.end
                }
                _ => spans.push(Span {
                    range,
                    colour,
                    bold,
                }),
            }
        }
        out.push_str(plain);
        if plain.len() == rest.len() {
            return (out, spans);
        }
        rest = match escape.strip_prefix('[') {
            // A control sequence: parameters, then a final byte from `@` to `~`.
            Some(seq) => {
                let end = seq.find(|c| ('@'..='~').contains(&c)).unwrap_or(seq.len());
                if seq[end..].starts_with('m') {
                    sgr(&seq[..end], &mut colour, &mut bold);
                }
                seq.get(end + 1..).unwrap_or("")
            }
            // A title or a link, ended by BEL or by ESC `\`.
            None if escape.starts_with(']') => match escape.find(['\x07', '\x1b']) {
                Some(end) if escape[end..].starts_with('\x07') => &escape[end + 1..],
                Some(end) => escape[end + 1..]
                    .strip_prefix('\\')
                    .unwrap_or(&escape[end + 1..]),
                None => "",
            },
            None => escape
                .chars()
                .next()
                .map_or("", |c| &escape[c.len_utf8()..]),
        };
    }
}

/// Apply one SGR sequence's parameters.
fn sgr(params: &str, colour: &mut Option<u8>, bold: &mut bool) {
    let mut codes = params
        .split([';', ':'])
        .map(|p| p.parse::<u16>().unwrap_or(0));
    while let Some(code) = codes.next() {
        match code {
            0 => (*colour, *bold) = (None, false),
            1 => *bold = true,
            22 => *bold = false,
            30..=37 => *colour = Some(code as u8 - 30),
            90..=97 => *colour = Some(code as u8 - 90 + 8),
            39 => *colour = None,
            // 256 colours, of which the first 16 are the terminal's, and true colour.
            38 => {
                *colour = match codes.next() {
                    Some(5) => codes.next().filter(|&n| n < 16).map(|n| n as u8),
                    Some(2) => {
                        codes.nth(2);
                        None
                    }
                    _ => None,
                }
            }
            48 => match codes.next() {
                Some(5) => {
                    codes.next();
                }
                Some(2) => {
                    codes.nth(2);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// A table read off the HTML pandas and polars write, laid out as a grid: each cell where its
/// row and column put it once the spans above and left of it are counted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    pub cells: Vec<TableCell>,
    pub rows: usize,
    pub cols: usize,
    /// Text around the table, such as polars' `shape: (3, 2)` or pandas' `5 rows × 3 columns`.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
    pub rows: usize,
    pub cols: usize,
    pub text: String,
    /// A `<th>`: a column's name or a row's index.
    pub header: bool,
}

/// The one plain table `html` holds, or `None` for anything else: a script, a link, a picture,
/// any markup past what a data frame's HTML is made of, or a second table.
pub fn table(html: &str) -> Option<Table> {
    let mut t = Table::default();
    let mut text = String::new();
    // The cell being read: whether it is a header, its spans.
    let mut cell: Option<(bool, usize, usize)> = None;
    let (mut row, mut col) = (None::<usize>, 0);
    // For each column, the first row a span from above leaves free.
    let mut taken: Vec<usize> = Vec::new();
    let mut seen = false;
    let mut rest = html;
    loop {
        let Some(lt) = rest.find('<') else {
            text.push_str(rest);
            break;
        };
        text.push_str(&rest[..lt]);
        rest = &rest[lt..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = &comment[comment.find("-->")? + 3..];
            continue;
        }
        let end = tag_end(rest)?;
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        // A cell ends at its own end tag, or at whatever opens or closes around it.
        if let Some((header, rows, cols)) = cell.take() {
            let r = row?;
            while taken.get(col).is_some_and(|&free| free > r) {
                col += 1;
            }
            if taken.len() < col + cols {
                taken.resize(col + cols, 0);
            }
            taken[col..col + cols].fill(r + rows);
            t.cells.push(TableCell {
                row: r,
                col,
                rows,
                cols,
                text: words(&decode(&text)),
                header,
            });
            (t.rows, t.cols) = (t.rows.max(r + rows), t.cols.max(col + cols));
            col += cols;
            text.clear();
        }
        let note = words(&decode(&text));
        if !note.is_empty() {
            t.notes.push(note);
        }
        text.clear();
        match (name.as_str(), closing) {
            ("style", false) => rest = &rest[find_ascii_ci(rest, "</style")?..],
            ("table", false) if seen => return None,
            ("table", false) => seen = true,
            ("tr", false) if seen => (row, col) = (Some(row.map_or(0, |r| r + 1)), 0),
            ("th" | "td", false) if row.is_some() => {
                let span = |key| attr(tag, key).unwrap_or(1).clamp(1, 1000);
                cell = Some((name == "th", span("rowspan"), span("colspan")));
            }
            ("div" | "style" | "small" | "p" | "table" | "thead" | "tbody" | "tfoot", _) => {}
            ("tr" | "th" | "td", true) => {}
            _ => return None,
        }
    }
    let note = words(&decode(&text));
    if !note.is_empty() {
        t.notes.push(note);
    }
    seen.then_some(t)
}

/// Where the tag `s` starts with ends: its `>`, past any in a quoted attribute.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices().skip(1) {
        match (quote, c) {
            (None, '>') => return Some(i),
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            _ => {}
        }
    }
    None
}

/// The number an attribute such as `rowspan="2"` gives.
fn attr(tag: &str, key: &str) -> Option<usize> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{key}="))? + key.len() + 1;
    let value = lower[at..].trim_start_matches(['"', '\'']);
    let digits = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    value[..digits].parse().ok()
}

fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// `text` on one line, its whitespace collapsed as HTML collapses it.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` with its character references read: the five XML names, `&nbsp;` and numbers.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let reference = rest.get(1..rest.len().min(12)).and_then(|r| {
            let semi = r.find(';')?;
            let c = match &r[..semi] {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => '\u{a0}',
                name => {
                    let n = name.strip_prefix('#')?;
                    let n = match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(n)?
                }
            };
            Some((c, semi + 2))
        });
        match reference {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(mime: &str, text: &str) -> Item {
        Item {
            mime: mime.into(),
            data: Data::Text(text.into()),
        }
    }

    #[test]
    fn the_first_form_a_window_draws_is_picked() {
        let png = item("image/png", "iVBORw0KGgo=\n");
        let html = item("text/html", "<table><tr><td>1</td></tr></table>");
        let script = item("text/html", "<div></div><script>plot()</script>");
        let latex = item("text/latex", "$x^2$");
        let plain = item("text/plain", "x**2");
        let json = Item {
            mime: "application/json".into(),
            data: Data::Json(serde_json::json!({"a": 1})),
        };
        let picked = |items: &[Item]| match pick(items) {
            Some(Form::Picture { mime, .. }) => mime.to_string(),
            Some(Form::Table(_)) => "table".into(),
            Some(Form::Latex(_)) => "latex".into(),
            Some(Form::Json(text)) => text,
            Some(Form::Text(t)) => t.into(),
            other => format!("{other:?}"),
        };
        assert_eq!(
            picked(&[plain.clone(), html.clone(), png.clone()]),
            "image/png"
        );
        assert_eq!(picked(&[plain.clone(), latex.clone(), html]), "table");
        assert_eq!(picked(&[plain.clone(), script.clone(), latex]), "latex");
        assert_eq!(picked(&[plain.clone(), json]), "{\n  \"a\": 1\n}");
        assert_eq!(picked(&[plain.clone(), script.clone()]), "x**2");
        // A picture whose data is not base64 is not one.
        let bad = item("image/png", "iVBOR<w0KGgo=");
        assert_eq!(picked(&[bad, plain]), "x**2");
        assert_eq!(pick(&[script]), None);
    }

    #[test]
    fn base64_is_its_alphabet_padded_at_the_end() {
        assert!(is_base64("iVBORw0KGgo=\n"));
        assert!(is_base64("iVBO\nRw0K\nGgo="));
        assert!(is_base64("AA=="));
        assert!(!is_base64(""));
        assert!(!is_base64("A=A="), "padding inside");
        assert!(!is_base64("A==="));
        assert!(!is_base64("AAA"), "not a multiple of four");
        assert!(!is_base64("AA-_"), "the URL-safe alphabet");
    }

    #[test]
    fn a_carriage_return_writes_over_its_line() {
        assert_eq!(
            carriage("\r  0%|   | 0/4\r 50%|█  | 2/4\r100%|███| 4/4\nLoaded\n"),
            "100%|███| 4/4\nLoaded\n"
        );
        assert_eq!(carriage("abcdef\rXY"), "XYcdef");
        assert_eq!(
            carriage("done\r\nnext"),
            "done\nnext",
            "CRLF is a line break"
        );
    }

    #[test]
    fn ansi_codes_become_spans_of_the_16_colours_and_bold() {
        let (text, spans) = ansi(
            "\x1b[0;31mZeroDivisionError\x1b[0m: x \x1b[38;5;241m=\x1b[39m \x1b[1;92mok\x1b[0m\x1b]8;;http://a\x1b\\",
        );
        assert_eq!(text, "ZeroDivisionError: x = ok");
        let span = |range: Range<usize>, colour, bold| Span {
            range,
            colour,
            bold,
        };
        assert_eq!(
            spans,
            [span(0..17, Some(1), false), span(23..25, Some(10), true)],
            "256 colours past the 16 are dropped"
        );
        assert_eq!(
            ansi("\x1b[38;5;9mx\x1b[48;2;1;2;3my").1,
            [span(0..2, Some(9), false)]
        );
    }

    /// pandas' HTML for a frame with a MultiIndex on both axes, trimmed of its rows.
    const MULTI: &str = r#"<div>
<style scoped>
    .dataframe thead tr th { text-align: left; }
</style>
<table border="1" class="dataframe">
  <thead>
    <tr>
      <th></th>
      <th></th>
      <th colspan="2" halign="left">fidelity</th>
    </tr>
    <tr>
      <th>qubit</th>
      <th>basis</th>
      <th>mean</th>
      <th>std</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <th rowspan="2" valign="top">0</th>
      <th>X</th>
      <td>0.931</td>
      <td>&lt;NA&gt;</td>
    </tr>
    <tr>
      <th>Z&amp;Y</th>
      <td>0.957</td>
      <td>0.008</td>
    </tr>
  </tbody>
</table>
<p>2 rows × 2 columns</p>
</div>"#;

    #[test]
    fn a_multiindex_frame_reads_as_a_grid_its_spans_counted() {
        let t = table(MULTI).unwrap();
        assert_eq!((t.rows, t.cols), (4, 4));
        let at = |row, col| {
            let c = t
                .cells
                .iter()
                .find(|c| (c.row, c.col) == (row, col))
                .unwrap();
            (c.text.as_str(), c.rows, c.cols, c.header)
        };
        assert_eq!(at(0, 2), ("fidelity", 1, 2, true));
        assert_eq!(at(2, 0), ("0", 2, 1, true));
        assert_eq!(at(2, 3), ("<NA>", 1, 1, false));
        // The index cell spanning two rows pushes the next row's cells right.
        assert_eq!(at(3, 1), ("Z&Y", 1, 1, true));
        assert_eq!(at(3, 3), ("0.008", 1, 1, false));
        assert_eq!(t.notes, ["2 rows × 2 columns"]);

        let polars = "<div><style>\n.dataframe > thead > tr { text-align: right; }\n</style>\n<small>shape: (1, 1)</small><table border=\"1\" class=\"dataframe\"><thead><tr><th>a</th></tr></thead><tbody><tr><td>&quot;x&quot;</td></tr></tbody></table></div>";
        let t = table(polars).unwrap();
        assert_eq!(t.notes, ["shape: (1, 1)"]);
        assert_eq!(t.cells[1].text, "\"x\"");
    }

    #[test]
    fn a_table_with_any_other_markup_is_refused() {
        for html in [
            "<table><tr><td>1</td></tr></table><script>alert(1)</script>",
            "<table><tr><td><a href=\"x\">1</a></td></tr></table>",
            "<table><tr><td><img src=x onerror=alert(1)></td></tr></table>",
            "<table><tr><td>1</td></tr></table><table></table>",
            "<div>no table</div>",
        ] {
            assert_eq!(table(html), None, "{html}");
        }
    }
}
