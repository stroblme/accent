//! Label text. A draw.io label is either plain text or, with `html=1`, a small HTML fragment;
//! both become [`Run`]s for drawing, and both go to and from Markdown for editing.
//!
//! The HTML is read tolerantly, the way a browser draws it: nothing here fails, and markup that
//! makes no sense is kept as text. The Markdown is the small subset the label editor offers
//! (bold, italic, underline, bullets and formulas), and [`markdown_to_html`] writes only tags
//! that [`html_to_runs`] reads back.

use crate::model::attr;
use crate::style::Color;

/// How a run of text looks where it differs from the label's font. `None` keeps the font's.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub color: Option<Color>,
    /// In page units (CSS pixels).
    pub size: Option<f64>,
}

/// One piece of a label, in reading order.
#[derive(Debug, Clone, PartialEq)]
pub enum Run {
    Text {
        text: String,
        marks: Marks,
    },
    /// TeX between `\(`…`\)` (inline) or `$$`…`$$` (display), delimiters stripped. Only produced
    /// when the page has math switched on.
    Math {
        tex: String,
        display: bool,
    },
    /// A line break: `<br>`, the edge of a block, or `\n` in a plain label.
    Break,
    /// The start of a list item; its text follows, then a `Break`.
    Bullet,
}

/// No marks: the label's own font.
const PLAIN: Marks = Marks {
    bold: false,
    italic: false,
    underline: false,
    color: None,
    size: None,
};

/// An HTML label (`html=1`) as runs. `math` is the page's `math` switch.
///
/// Bold, italic, underline, colour and size come from the tags and inline styles that set them;
/// `<br>` and the edges of blocks break lines, and a list item starts with a bullet. Everything
/// else (alignment, font family, links, sub- and superscript) is ignored and its text kept.
/// Whitespace collapses as CSS `white-space: normal` has it, and there is no break at either end.
pub fn html_to_runs(html: &str, math: bool) -> Vec<Run> {
    let mut runs = Vec::new();
    // The open elements, innermost last, each with the marks of the text inside it.
    let mut open: Vec<(String, Marks)> = Vec::new();
    let mut rest = html;
    while let Some((c, len)) = html_char(rest) {
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
        } else if let Some((tag, after)) = Tag::read(rest) {
            rest = after;
            let name = tag.name.as_str();
            if tag.close {
                // The nearest open element of that name ends, and whatever was opened inside
                // it; a close tag that matches none is ignored.
                if let Some(i) = open.iter().rposition(|(n, _)| n == name) {
                    open.truncate(i);
                    // A block ends its line, even an empty list item's.
                    if is_block(name) && matches!(runs.last(), Some(Run::Text { .. } | Run::Bullet))
                    {
                        line_break(&mut runs);
                    }
                }
            } else if name == "br" {
                line_break(&mut runs);
            } else if name == "script" || name == "style" {
                // Code, not text: skip to the close tag, which is then ignored as unmatched.
                let end = rest.to_ascii_lowercase().find(&format!("</{name}"));
                rest = &rest[end.unwrap_or(rest.len())..];
            } else {
                // A block starts on a line of its own.
                if is_block(name) && matches!(runs.last(), Some(Run::Text { .. })) {
                    line_break(&mut runs);
                }
                if name == "li" {
                    runs.push(Run::Bullet);
                }
                let mut marks = open.last().map_or(PLAIN, |(_, m)| m.clone());
                tag.apply(&mut marks);
                open.push((tag.name, marks));
            }
        } else {
            collapse(&mut runs, c, open.last().map_or(&PLAIN, |(_, m)| m));
            rest = &rest[len..];
        }
    }
    trim_line_end(&mut runs);
    while runs.last() == Some(&Run::Break) {
        runs.pop();
    }
    let leading = runs.iter().take_while(|run| **run == Run::Break).count();
    runs.drain(..leading);
    if math { find_math(runs) } else { runs }
}

/// A plain label as runs: one text run per line.
pub fn plain_to_runs(text: &str, math: bool) -> Vec<Run> {
    let mut runs = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            runs.push(Run::Break);
        }
        if !line.is_empty() {
            runs.push(Run::Text {
                text: line.to_string(),
                marks: Marks::default(),
            });
        }
    }
    if math { find_math(runs) } else { runs }
}

/// The text alone, one line per break.
pub fn runs_to_plain(runs: &[Run]) -> String {
    let mut out = String::new();
    for run in runs {
        match run {
            Run::Text { text, .. } => out.push_str(text),
            Run::Math {
                tex,
                display: false,
            } => out.push_str(&format!("\\({tex}\\)")),
            Run::Math { tex, display: true } => out.push_str(&format!("$${tex}$$")),
            Run::Break => out.push('\n'),
            Run::Bullet => out.push_str("• "),
        }
    }
    out
}

/// Runs as the Markdown the label editor shows: `**bold**`, `*italic*`, `<u>underline</u>`,
/// `- ` bullets and formulas as written. Colour and size have no Markdown and are left out, so
/// a label edited as Markdown loses them.
pub fn runs_to_markdown(runs: &[Run]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while let Some(run) = runs.get(i) {
        i += 1;
        match run {
            Run::Text { text, marks } => {
                // Text that differs only in colour or size is one span here.
                let look = |m: &Marks| (m.bold, m.italic, m.underline);
                let mut text = text.clone();
                while let Some(Run::Text {
                    text: more,
                    marks: m,
                }) = runs.get(i)
                    && look(m) == look(marks)
                {
                    text.push_str(more);
                    i += 1;
                }
                emphasize(&mut out, &text, marks);
            }
            Run::Math {
                tex,
                display: false,
            } => out.push_str(&format!("\\({tex}\\)")),
            Run::Math { tex, display: true } => out.push_str(&format!("$${tex}$$")),
            Run::Break => out.push('\n'),
            Run::Bullet => out.push_str("- "),
        }
    }
    out
}

/// A cell's label as Markdown for editing, whichever form it is stored in.
pub fn to_markdown(label: &str, html: bool) -> String {
    if html {
        runs_to_markdown(&html_to_runs(label, true))
    } else {
        runs_to_markdown(&plain_to_runs(label, true))
    }
}

/// Markdown from the label editor as the HTML a label is stored in (`html=1`). Lines are joined
/// with `<br>`, lines starting `- ` or `* ` become a list, and emphasis becomes `<b>` and `<i>`;
/// `<u>` passes through, formulas are kept as written and everything else is text. Only `<b>`,
/// `<i>`, `<u>`, `<br>`, `<ul>` and `<li>` are written, so [`html_to_runs`] reads back what the
/// editor showed.
pub fn markdown_to_html(md: &str) -> String {
    let lines = md_lines(md);
    let mut html = String::new();
    for (i, (bullet, line)) in lines.iter().enumerate() {
        let before = i.checked_sub(1).map(|j| &lines[j]);
        match (before.is_some_and(|(b, _)| *b), *bullet) {
            (true, true) => {}
            (true, false) => html.push_str("</ul>"),
            (false, true) => {
                // A list starts on its own line, but an empty line before it needs its break.
                if before.is_some_and(|(_, text)| text.is_empty()) {
                    html.push_str("<br>");
                }
                html.push_str("<ul>");
            }
            (false, false) if i > 0 => html.push_str("<br>"),
            (false, false) => {}
        }
        if *bullet {
            html.push_str(&format!("<li>{line}</li>"));
        } else {
            html.push_str(line);
        }
    }
    if lines.last().is_some_and(|(bullet, _)| *bullet) {
        html.push_str("</ul>");
    }
    html
}

/// A start or end tag.
struct Tag<'a> {
    /// Lowercased.
    name: String,
    close: bool,
    /// Everything between the name and the `>`.
    attrs: &'a str,
}

impl<'a> Tag<'a> {
    /// The tag `s` starts with and the text after it; `None` when there is none, so that a `<`
    /// that opens no tag stays text.
    fn read(s: &'a str) -> Option<(Tag<'a>, &'a str)> {
        let s = s.strip_prefix('<')?;
        let close = s.starts_with('/');
        let s = if close { &s[1..] } else { s };
        if !s.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        let name = s.find(|c: char| !c.is_ascii_alphanumeric())?;
        // The `>` that ends the tag, not one inside a quoted attribute value.
        let mut quote = None;
        let (end, _) = s.char_indices().find(|&(_, c)| {
            if quote.is_none() && (c == '"' || c == '\'') {
                quote = Some(c);
            } else if quote == Some(c) {
                quote = None;
            }
            quote.is_none() && c == '>'
        })?;
        let tag = Tag {
            name: s[..name].to_ascii_lowercase(),
            close,
            attrs: &s[name..end],
        };
        Some((tag, &s[end + 1..]))
    }

    /// Change `marks` as the element changes the look of its text.
    fn apply(&self, marks: &mut Marks) {
        let attrs = attributes(self.attrs);
        match self.name.as_str() {
            "b" | "strong" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => marks.bold = true,
            "i" | "em" => marks.italic = true,
            "u" => marks.underline = true,
            "font" => {
                if let Some(color) = attr(&attrs, "color").and_then(Color::parse) {
                    marks.color = Some(color);
                }
                if let Some(size) = attr(&attrs, "size").and_then(font_size) {
                    marks.size = Some(size);
                }
            }
            _ => {}
        }
        // An inline style wins over the tag, as in a browser.
        if let Some(style) = attr(&attrs, "style") {
            css(marks, style);
        }
    }
}

/// The elements that sit on lines of their own.
// ponytail: lines are flat. Numbered lists get bullets, nested lists lose their indent, `pre`
// collapses its whitespace like any text and the cells of a table row run together. A run
// carrying the list depth and number, and a cell gap, would draw those as draw.io does.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "div" | "p" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "tr" | "blockquote" | "pre"
    )
}

/// A tag's `name=value` pairs, names lowercased, values unquoted and their references decoded.
fn attributes(mut s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '/');
        let end = s
            .find(|c: char| c.is_ascii_whitespace() || c == '=' || c == '/')
            .unwrap_or(s.len());
        if end == 0 {
            return out;
        }
        let name = s[..end].to_ascii_lowercase();
        s = s[end..].trim_start();
        let mut value = "";
        if let Some(v) = s.strip_prefix('=') {
            let v = v.trim_start();
            (value, s) = match v.chars().next() {
                Some(q @ ('"' | '\'')) => v[1..].split_once(q).unwrap_or((&v[1..], "")),
                _ => v.split_at(v.find(|c: char| c.is_ascii_whitespace()).unwrap_or(v.len())),
            };
        }
        out.push((name, decode(value)));
    }
}

/// The declarations of an inline `style` that change how text looks; the rest (background,
/// font family, alignment) are ignored.
fn css(marks: &mut Marks, style: &str) {
    for declaration in style.split(';') {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        match property.trim().to_ascii_lowercase().as_str() {
            "font-size" => marks.size = css_px(&value).or(marks.size),
            "color" => marks.color = Color::parse(&value).or(marks.color),
            "font-weight" => {
                marks.bold = match value.as_str() {
                    "bold" | "bolder" => true,
                    "normal" | "lighter" => false,
                    n => n
                        .parse::<f64>()
                        .map_or(marks.bold, |weight| weight >= 600.0),
                }
            }
            "font-style" => {
                marks.italic = match value.as_str() {
                    "italic" | "oblique" => true,
                    "normal" => false,
                    _ => marks.italic,
                }
            }
            // An underline is drawn across the children; they cannot take it back.
            "text-decoration" | "text-decoration-line" => {
                marks.underline |= value.contains("underline");
            }
            _ => {}
        }
    }
}

/// A CSS font size in pixels, points converted at 96 dpi.
// ponytail: relative sizes (`em`, `%`, `larger`) and keywords are ignored and keep the size
// around them; resolve them against that size if labels turn up that use them.
fn css_px(value: &str) -> Option<f64> {
    let (number, scale) = match value.strip_suffix("px") {
        Some(number) => (number, 1.0),
        None => (value.strip_suffix("pt")?, 4.0 / 3.0),
    };
    let n: f64 = number.trim().parse().ok()?;
    (n.is_finite() && n > 0.0).then_some(n * scale)
}

/// `<font size>`: 1 to 7, or with a sign a step from the default 3, as the pixel size browsers
/// give each step.
fn font_size(value: &str) -> Option<f64> {
    const PX: [f64; 7] = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];
    let value = value.trim();
    let n: i32 = value.parse().ok()?;
    let step = if value.starts_with(['+', '-']) {
        3 + n
    } else {
        n
    };
    Some(PX[step.clamp(1, 7) as usize - 1])
}

/// The first character of HTML text and the bytes it takes, a character reference decoded.
fn html_char(s: &str) -> Option<(char, usize)> {
    let c = s.chars().next()?;
    Some(match c {
        '&' => entity(s).unwrap_or((c, 1)),
        _ => (c, c.len_utf8()),
    })
}

/// The character reference `s` starts with, as its character and length: the named ones a
/// browser's `innerHTML` writes, and numeric ones. Others stay text.
fn entity(s: &str) -> Option<(char, usize)> {
    let (end, _) = s.char_indices().take(12).find(|&(_, c)| c == ';')?;
    let c = match &s[1..end] {
        "nbsp" => '\u{a0}',
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        name => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((c, end + 1))
}

/// HTML text with its character references decoded.
fn decode(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((c, len)) = html_char(s) {
        out.push(c);
        s = &s[len..];
    }
    out
}

/// Add text to the runs, into the last one when its marks are the same.
fn append(runs: &mut Vec<Run>, s: &str, marks: &Marks) {
    match runs.last_mut() {
        Some(Run::Text { text, marks: last }) if last == marks => text.push_str(s),
        _ => runs.push(Run::Text {
            text: s.to_string(),
            marks: marks.clone(),
        }),
    }
}

/// Add one character of HTML text. Whitespace collapses: a stretch of it is one space, and
/// there is none at the start of a line.
fn collapse(runs: &mut Vec<Run>, c: char, marks: &Marks) {
    if !c.is_ascii_whitespace() {
        append(runs, c.encode_utf8(&mut [0; 4]), marks);
        return;
    }
    match runs.last() {
        None | Some(Run::Break | Run::Bullet) => {}
        Some(Run::Text { text, .. }) if text.ends_with(' ') => {}
        _ => append(runs, " ", marks),
    }
}

/// End the line.
fn line_break(runs: &mut Vec<Run>) {
    trim_line_end(runs);
    runs.push(Run::Break);
}

/// Drop the space a line ends with, which a browser does not draw.
fn trim_line_end(runs: &mut Vec<Run>) {
    if let Some(Run::Text { text, .. }) = runs.last_mut()
        && text.ends_with(' ')
    {
        text.pop();
        if text.is_empty() {
            runs.pop();
        }
    }
}

/// One character of a label with its marks, or a run that is not text.
enum Item<'a> {
    Char(char, &'a Marks),
    Other(&'a Run),
}

/// `\(…\)` and `$$…$$` as [`Run::Math`], wherever the delimiters fall among the runs. The
/// marks inside a formula are dropped and a break in it is a space, as MathJax takes a `<br>`
/// there. An opener without a closer stays text.
fn find_math(runs: Vec<Run>) -> Vec<Run> {
    let mut items = Vec::new();
    for run in &runs {
        match run {
            Run::Text { text, marks } => items.extend(text.chars().map(|c| Item::Char(c, marks))),
            other => items.push(Item::Other(other)),
        }
    }
    let pair_at = |i: usize, a: char, b: char| match items.get(i..i + 2) {
        Some([Item::Char(x, _), Item::Char(y, _)]) => (*x, *y) == (a, b),
        _ => false,
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let closer = if pair_at(i, '\\', '(') {
            Some(('\\', ')'))
        } else if pair_at(i, '$', '$') {
            Some(('$', '$'))
        } else {
            None
        };
        if let Some((a, b)) = closer
            && let Some(end) = (i + 2..items.len()).find(|&j| pair_at(j, a, b))
        {
            let tex = items[i + 2..end]
                .iter()
                .map(|item| match item {
                    Item::Char(c, _) => *c,
                    Item::Other(_) => ' ',
                })
                .collect();
            out.push(Run::Math {
                tex,
                display: a == '$',
            });
            i = end + 2;
            continue;
        }
        match items[i] {
            Item::Char(c, marks) => append(&mut out, c.encode_utf8(&mut [0; 4]), marks),
            Item::Other(run) => out.push(run.clone()),
        }
        i += 1;
    }
    out
}

/// Text with its bold, italic and underline markers. Whitespace at its edges goes outside them,
/// where CommonMark needs it for the markers to count.
fn emphasize(out: &mut String, text: &str, marks: &Marks) {
    let stars = match (marks.bold, marks.italic) {
        (true, true) => "***",
        (true, false) => "**",
        (false, true) => "*",
        (false, false) => "",
    };
    let core = text.trim();
    if core.is_empty() || (stars.is_empty() && !marks.underline) {
        escape_markdown(out, text);
        return;
    }
    out.push_str(&text[..text.len() - text.trim_start().len()]);
    if marks.underline {
        out.push_str("<u>");
    }
    out.push_str(stars);
    escape_markdown(out, core);
    out.push_str(stars);
    if marks.underline {
        out.push_str("</u>");
    }
    out.push_str(&text[text.trim_end().len()..]);
}

/// Text as Markdown that [`markdown_to_html`] reads back as the same text: what it would take
/// for markup gets a backslash.
fn escape_markdown(out: &mut String, text: &str) {
    for (i, c) in text.char_indices() {
        let line_start = out.is_empty() || out.ends_with('\n');
        let bullet = c == '-' && line_start && text[i + 1..].starts_with(' ');
        if matches!(c, '\\' | '*' | '_' | '<') || bullet {
            out.push('\\');
        }
        out.push(c);
    }
}

/// A run of `*` or `_` in a line of Markdown, which may pair up into emphasis.
struct Delim {
    ch: char,
    /// How many of its characters are still unpaired.
    len: usize,
    open: bool,
    close: bool,
    /// The tags that end before what is left of the run, and those that start after it.
    closing: String,
    opening: String,
}

impl Delim {
    /// `len` × `ch` between the characters `before` and `after` (`None` at the ends of the
    /// text). It can open when a non-space follows and close when one precedes; `_` does
    /// neither inside a word, as in CommonMark.
    fn new(ch: char, len: usize, before: Option<char>, after: Option<char>) -> Delim {
        let space = |c: Option<char>| c.is_none_or(char::is_whitespace);
        let word = |c: Option<char>| ch == '_' && c.is_some_and(char::is_alphanumeric);
        Delim {
            ch,
            len,
            open: !space(after) && !word(before),
            close: !space(before) && !word(after),
            closing: String::new(),
            opening: String::new(),
        }
    }
}

/// A piece of a Markdown line: finished HTML, or a run that may become emphasis.
enum Piece {
    Html(String),
    Delim(Delim),
}

/// The editor's Markdown as lines of HTML, each with whether it is a list item. A formula is
/// kept whole, even across lines.
fn md_lines(md: &str) -> Vec<(bool, String)> {
    let mut lines = Vec::new();
    let (mut bullet, mut pieces) = (false, Vec::new());
    let mut i = 0;
    while let Some(c) = md[i..].chars().next() {
        let rest = &md[i..];
        let line_start = i == 0 || md[..i].ends_with('\n');
        let len = if line_start && (rest.starts_with("- ") || rest.starts_with("* ")) {
            bullet = true;
            2
        } else if c == '\n' {
            lines.push((bullet, emphasis(std::mem::take(&mut pieces))));
            bullet = false;
            1
        } else if let Some(len) = formula_len(rest) {
            pieces.push(Piece::Html(escape_html(&rest[..len])));
            len
        } else if c == '\\' && rest[1..].starts_with(|p: char| p.is_ascii_punctuation()) {
            pieces.push(Piece::Html(escape_html(&rest[1..2])));
            2
        } else if let Some(tag) = ["<u>", "</u>"].into_iter().find(|t| rest.starts_with(t)) {
            pieces.push(Piece::Html(tag.to_string()));
            tag.len()
        } else if c == '*' || c == '_' {
            let len = rest.len() - rest.trim_start_matches(c).len();
            let (before, after) = (md[..i].chars().next_back(), rest[len..].chars().next());
            pieces.push(Piece::Delim(Delim::new(c, len, before, after)));
            len
        } else {
            pieces.push(Piece::Html(escape_html(&rest[..c.len_utf8()])));
            c.len_utf8()
        };
        i += len;
    }
    lines.push((bullet, emphasis(pieces)));
    lines
}

/// The length of the formula `s` starts with, `\(…\)` or `$$…$$`, delimiters included.
fn formula_len(s: &str) -> Option<usize> {
    let close = if s.starts_with("\\(") {
        "\\)"
    } else if s.starts_with("$$") {
        "$$"
    } else {
        return None;
    };
    Some(s[2..].find(close)? + 4)
}

/// A line's pieces as HTML, its `*` and `_` runs paired into `<b>` and `<i>` as CommonMark
/// pairs them: each closer, left to right, takes the nearest opener of its kind, and the runs
/// left unpaired stay text.
// ponytail: opening and closing are judged by whitespace (and words, for `_`) alone; CommonMark's
// punctuation rules and its rule of three are left out, which only shows with oddly packed
// markers. Port them from the spec's "process emphasis" if that ever matters.
fn emphasis(mut pieces: Vec<Piece>) -> String {
    let mut delims: Vec<&mut Delim> = pieces
        .iter_mut()
        .filter_map(|piece| match piece {
            Piece::Delim(d) => Some(d),
            Piece::Html(_) => None,
        })
        .collect();
    for c in 0..delims.len() {
        while delims[c].close && delims[c].len > 0 {
            let ch = delims[c].ch;
            let Some(o) = (0..c)
                .rev()
                .find(|&o| delims[o].ch == ch && delims[o].open && delims[o].len > 0)
            else {
                break;
            };
            // Bold takes two characters from each side, italic one.
            let (n, tag) = if delims[o].len >= 2 && delims[c].len >= 2 {
                (2, "b")
            } else {
                (1, "i")
            };
            delims[o].len -= n;
            delims[o].opening.insert_str(0, &format!("<{tag}>"));
            delims[c].len -= n;
            delims[c].closing.push_str(&format!("</{tag}>"));
            // The runs in between can no longer open, so that the tags nest.
            for d in &mut delims[o + 1..c] {
                d.open = false;
            }
        }
    }
    pieces
        .into_iter()
        .map(|piece| match piece {
            Piece::Html(html) => html,
            Piece::Delim(d) => {
                let left = d.ch.to_string().repeat(d.len);
                format!("{}{left}{}", d.closing, d.opening)
            }
        })
        .collect()
}

/// Text as HTML: `&`, `<` and `>` escaped.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use Run::{Break, Bullet};

    fn t(s: &str) -> Run {
        m(s, "")
    }

    /// A text run whose marks are named by letter: `b`old, `i`talic, `u`nderline.
    fn m(s: &str, look: &str) -> Run {
        let marks = Marks {
            bold: look.contains('b'),
            italic: look.contains('i'),
            underline: look.contains('u'),
            ..PLAIN
        };
        Run::Text {
            text: s.into(),
            marks,
        }
    }

    fn sized(s: &str, look: &str, size: f64, color: Option<Color>) -> Run {
        let mut run = m(s, look);
        if let Run::Text { marks, .. } = &mut run {
            (marks.size, marks.color) = (Some(size), color);
        }
        run
    }

    fn math(tex: &str, display: bool) -> Run {
        Run::Math {
            tex: tex.into(),
            display,
        }
    }

    #[test]
    fn html_to_runs_table() {
        let teal = Some(Color::rgb(0, 150, 130));
        let cases = [
            ("a <b>b</b><strong>c</strong>", vec![t("a "), m("bc", "b")]),
            (
                "<i>a</i><em>b</em> <u>c</u>",
                vec![m("ab", "i"), t(" "), m("c", "u")],
            ),
            ("<b>a<i>b</i></b>c", vec![m("a", "b"), m("b", "bi"), t("c")]),
            (
                "<font color='#009682' size=5>a</font>",
                vec![sized("a", "", 24.0, teal)],
            ),
            (
                "<span style='font-size: 28px; color: #009682'>a</span>",
                vec![sized("a", "", 28.0, teal)],
            ),
            (
                "<p style='font-size:12pt;font-weight:700'>a</p>",
                vec![sized("a", "b", 16.0, None)],
            ),
            ("<b style='font-weight: normal'>a</b>", vec![t("a")]),
            (
                "a<br>b<br/><br>c",
                vec![t("a"), Break, t("b"), Break, Break, t("c")],
            ),
            ("<div>a</div><div>b</div>", vec![t("a"), Break, t("b")]),
            (
                "<div>a</div><div><br></div><div>b</div>",
                vec![t("a"), Break, Break, t("b")],
            ),
            (
                "<ul><li>a</li><li>b</li></ul>c",
                vec![Bullet, t("a"), Break, Bullet, t("b"), Break, t("c")],
            ),
            (
                "<h1>T</h1>a<p>b</p>",
                vec![m("T", "b"), Break, t("a"), Break, t("b")],
            ),
            (
                "&lt;&amp;&gt;&quot;&#39;&#x41;&nbsp;&foo; & x",
                vec![t("<&>\"'A\u{a0}&foo; & x")],
            ),
            ("  a \n\t b  <br>  c  ", vec![t("a b"), Break, t("c")]),
            (
                "<mark>a</mark><!-- note -->b<script>c</script>",
                vec![t("ab")],
            ),
            ("<b>a</i>b</b>c</b>", vec![m("ab", "b"), t("c")]),
            ("1 < 2 <b", vec![t("1 < 2 <b")]),
        ];
        for (html, runs) in cases {
            assert_eq!(html_to_runs(html, false), runs, "{html}");
        }
    }

    #[test]
    fn math_spans_runs_and_ignores_inner_breaks() {
        assert_eq!(
            html_to_runs("a \\(f(<b>x</b>,<br>y)\\) b", true),
            vec![t("a "), math("f(x, y)", false), t(" b")]
        );
        assert_eq!(
            html_to_runs("<i>$$E = mc^2$$</i><br>\\(open", true),
            vec![math("E = mc^2", true), Break, t("\\(open")]
        );
    }

    #[test]
    fn math_is_text_when_the_page_has_it_off() {
        assert_eq!(
            html_to_runs("\\(x\\) $$y$$", false),
            vec![t("\\(x\\) $$y$$")]
        );
        assert_eq!(plain_to_runs("\\(x\\)", false), vec![t("\\(x\\)")]);
    }

    #[test]
    fn runs_to_markdown_table() {
        let red = Some(Color::rgb(255, 0, 0));
        let cases = [
            (vec![m("a ", "b"), t("b")], "**a** b"),
            (vec![m(" a b ", "i"), m("c", "u")], " *a b* <u>c</u>"),
            (vec![m("x", "biu")], "<u>***x***</u>"),
            (
                vec![sized("a", "b", 20.0, red), sized("b", "b", 9.0, None)],
                "**ab**",
            ),
            (vec![m(" ", "b")], " "),
            (
                vec![t("1*2 <u> a_b \\"), Break, t("- x")],
                "1\\*2 \\<u> a\\_b \\\\\n\\- x",
            ),
            (
                vec![Bullet, t("a"), Break, Bullet, m("b", "b")],
                "- a\n- **b**",
            ),
            (
                vec![t("see "), math("a*b_<c", false), Break, math("x", true)],
                "see \\(a*b_<c\\)\n$$x$$",
            ),
        ];
        for (runs, md) in cases {
            assert_eq!(runs_to_markdown(&runs), md);
        }
    }

    #[test]
    fn markdown_round_trip_is_stable() {
        for md in [
            "plain text",
            "**bold**, *italic* and ***both***",
            "<u>under</u> and <u>**bold under**</u>",
            "**a***b* and *c***d**",
            "- one\n- **two**\nafter",
            "before\n\n- item\n\nafter",
            "inline \\(a * b < c_1\\) and $$\\sum_i x_i$$",
            "\\*not bold\\* a\\_b 1 \\< 2 back\\\\slash \\\\(x",
            "\\- not a list",
            "first\n\n\nsecond",
            "a & b > c",
        ] {
            let html = markdown_to_html(md);
            assert_eq!(runs_to_markdown(&html_to_runs(&html, true)), md, "{html}");
        }
    }

    #[test]
    fn markdown_writes_the_tags_labels_are_read_with() {
        for (md, html) in [
            (
                "***x*** __b__ _i_ a_b **c",
                "<i><b>x</b></i> <b>b</b> <i>i</i> a_b **c",
            ),
            ("a <b> & \\<u>", "a &lt;b&gt; &amp; &lt;u&gt;"),
            ("a\n- b\n* c\n\nd", "a<ul><li>b</li><li>c</li></ul><br>d"),
            ("\\(x<1\\)", "\\(x&lt;1\\)"),
        ] {
            assert_eq!(markdown_to_html(md), html);
        }
    }

    #[test]
    fn plain_labels_break_on_newlines() {
        assert_eq!(
            plain_to_runs("a  <b>\n\nc", false),
            vec![t("a  <b>"), Break, Break, t("c")]
        );
        assert_eq!(
            plain_to_runs("x\n\\(y\\)", true),
            vec![t("x"), Break, math("y", false)]
        );
    }
}
