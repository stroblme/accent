//! Typst, read as far as its outline and the files it brings in: its `=` headings, each a
//! section running to the next heading of its level or above, and the paths a `#include` or
//! `#import` names.
//!
//! One pass tells markup from code as Typst's own lexer does: a `#` starts an expression (a
//! name with its fields, calls and trailing content) or a statement (`#let`, `#show`, … to the
//! end of its line), a `[…]` inside code is markup again, and `$…$` is math. Comments, raw text
//! and strings are passed over, so a `= ` inside one of them, in code or in math is no heading.

use super::{Decl, SymbolKind};
use regex::Regex;
use std::sync::LazyLock;

/// What a statement keyword after `#` runs to: the end of its line, or a `;`.
const STATEMENTS: [&str; 10] = [
    "let", "set", "show", "import", "include", "if", "for", "while", "context", "return",
];

/// What the lexer is inside of.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Frame {
    /// Text, at the top or in a content block (`[…]` inside code), with the plain brackets it has
    /// opened since; a content block ends at the `]` that closes none of them.
    Markup {
        block: bool,
        depth: usize,
    },
    Math,
    /// A `#` expression: a name, then its `.field`s, calls and trailing content.
    Expr(Last),
    /// A `#let`, `#show`, … statement, which ends with its line or a `;`.
    Statement,
    /// Code in `(…)` or `{…}`, ending at its closer.
    Group(u8),
}

/// What a `#` expression read last, which decides what may go on with it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Last {
    Start,
    Name,
    Dot,
    /// A call's arguments, a trailing content block, a string.
    Closed,
}

/// `text` read once: its markup with everything else blanked, and the paths it includes.
struct Read {
    markup: Vec<u8>,
    includes: Vec<String>,
}

/// `text` with code, math, comments, raw text and strings blanked to spaces, newlines and byte
/// offsets kept: only markup is left, which is where a heading is.
pub(super) fn mask(text: &str) -> String {
    let markup = read(text).markup;
    String::from_utf8(markup).expect("whole characters were replaced by ASCII spaces")
}

/// The paths `text` includes or imports (`#include "ch.typ"`, `#import "lib.typ": f`), as
/// written, in order; a package's (`"@preview/…"`) too, which the caller tells apart.
pub fn includes(text: &str) -> Vec<String> {
    read(text).includes
}

/// The headings of `text`, `masked` as [`mask`] leaves it, each spanning its section: a line
/// opening with `=`s and a space in markup, its title whatever follows them but a label, so
/// `= #title` is one and `#a = 1` is not. A heading opening a content block (`[= Title]`), which
/// tinymist lists, is not read, since only code generating a document writes one.
pub(super) fn headings(masked: &str, text: &str) -> Vec<Decl> {
    static HEADING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[ \t]*(=+)(?:[ \t]|$)").expect("a valid heading"));
    static LABEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<[\w.:-]+>[ \t]*$").expect("a valid label"));
    // Where each heading's line starts and ends, its level and its title.
    let mut found: Vec<(usize, usize, usize, &str)> = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        if let Some(marker) = HEADING.captures(line).and_then(|c| c.get(1))
            && masked.as_bytes()[start + marker.start()] == b'='
        {
            let rest = line[marker.end()..].trim_end();
            let title = LABEL
                .find(rest)
                .map_or(rest, |label| &rest[..label.start()]);
            if !title.trim().is_empty() {
                found.push((start, end, marker.len(), title.trim()));
            }
        }
        start = end;
    }
    let mut out = Vec::new();
    for (i, &(start, end, level, title)) in found.iter().enumerate() {
        let section_end = found[i + 1..]
            .iter()
            .find(|(_, _, other, _)| *other <= level)
            .map_or(text.len(), |(next, ..)| *next);
        out.push(Decl {
            // A namespace, as tinymist's outline has a heading.
            kind: SymbolKind::Module,
            name: title.to_string(),
            container: None,
            range: start..section_end,
            line: 0,
            end_line: 0,
            signature: text[start..end].trim().chars().take(200).collect(),
            test: false,
        });
    }
    out
}

fn read(text: &str) -> Read {
    let mut lexer = Lexer {
        b: text.as_bytes(),
        out: text.as_bytes().to_vec(),
        stack: vec![Frame::Markup {
            block: false,
            depth: 0,
        }],
        includes: Vec::new(),
    };
    let mut i = 0;
    while i < lexer.b.len() {
        i = lexer.step(i);
    }
    Read {
        markup: lexer.out,
        includes: lexer.includes,
    }
}

struct Lexer<'a> {
    b: &'a [u8],
    out: Vec<u8>,
    stack: Vec<Frame>,
    includes: Vec<String>,
}

impl Lexer<'_> {
    fn at(&self, i: usize) -> u8 {
        self.b.get(i).copied().unwrap_or(0)
    }

    fn blank(&mut self, from: usize, to: usize) {
        for o in &mut self.out[from..to.min(self.b.len())] {
            if *o != b'\n' {
                *o = b' ';
            }
        }
    }

    fn push(&mut self, frame: Frame) {
        self.stack.push(frame);
    }

    /// End the innermost frame, and with it the part of a `#` expression it was.
    fn pop(&mut self) {
        self.stack.pop();
        if let Some(Frame::Expr(last)) = self.stack.last_mut() {
            *last = Last::Closed;
        }
        if self.stack.is_empty() {
            self.stack.push(Frame::Markup {
                block: false,
                depth: 0,
            });
        }
    }

    /// Read from `i` and return where to go on from.
    fn step(&mut self, i: usize) -> usize {
        let (c, next) = (self.b[i], self.at(i + 1));
        let frame = *self.stack.last().expect("the stack is never empty");
        // What every mode passes over whole.
        let skip = match (c, frame) {
            (b'/', Frame::Markup { .. }) if next == b'/' && self.after_scheme(i) => None,
            (b'/', _) if next == b'/' => Some(line_end(self.b, i)),
            (b'/', _) if next == b'*' => Some(block_comment_end(self.b, i)),
            (b'`', _) => Some(raw_end(self.b, i)),
            (b'"', Frame::Math | Frame::Statement | Frame::Group(_)) => Some(string_end(self.b, i)),
            _ => None,
        };
        if let Some(end) = skip {
            self.blank(i, end);
            return end;
        }
        match frame {
            Frame::Markup { block, depth } => self.markup(i, block, depth),
            Frame::Math => match c {
                b'\\' => self.escape(i, true),
                b'$' => {
                    self.blank(i, i + 1);
                    self.pop();
                    i + 1
                }
                b'#' if starts_expr(next) => self.hash(i),
                _ => self.code_char(i),
            },
            Frame::Expr(last) => self.expr(i, last),
            Frame::Statement => match c {
                b'\n' => {
                    self.pop();
                    i
                }
                b';' => {
                    self.blank(i, i + 1);
                    self.pop();
                    i + 1
                }
                _ => self.code(i),
            },
            Frame::Group(closer) if c == closer => {
                self.blank(i, i + 1);
                self.pop();
                i + 1
            }
            Frame::Group(_) => self.code(i),
        }
    }

    /// Whether the `//` at `i` is a web address's, which markup reads as a link.
    fn after_scheme(&self, i: usize) -> bool {
        let before = &self.b[..i];
        before.ends_with(b"http:") || before.ends_with(b"https:")
    }

    fn markup(&mut self, i: usize, block: bool, depth: usize) -> usize {
        let set = |lexer: &mut Self, depth| {
            if let Some(Frame::Markup { depth: d, .. }) = lexer.stack.last_mut() {
                *d = depth;
            }
        };
        match self.b[i] {
            b'\\' => self.escape(i, false),
            b'$' => {
                self.blank(i, i + 1);
                self.push(Frame::Math);
                i + 1
            }
            b'#' if starts_expr(self.at(i + 1)) => self.hash(i),
            b'[' => {
                set(self, depth + 1);
                i + 1
            }
            b']' if depth > 0 => {
                set(self, depth - 1);
                i + 1
            }
            b']' if block => {
                self.blank(i, i + 1);
                self.pop();
                i + 1
            }
            _ => i + 1,
        }
    }

    /// A backslash and the character it escapes, kept in markup and blanked in math.
    fn escape(&mut self, i: usize, blank: bool) -> usize {
        let end = i + 1 + char_len(self.b, i + 1);
        if blank {
            self.blank(i, end);
        }
        end
    }

    /// The `#` at `i`, which starts an expression or a statement.
    fn hash(&mut self, i: usize) -> usize {
        self.blank(i, i + 1);
        self.push(Frame::Expr(Last::Start));
        i + 1
    }

    fn expr(&mut self, i: usize, last: Last) -> usize {
        let c = self.b[i];
        let set = |lexer: &mut Self, to| {
            if let Some(Frame::Expr(last)) = lexer.stack.last_mut() {
                *last = to;
            }
        };
        match (c, last) {
            (c, Last::Start | Last::Dot) if is_name_start(c) => {
                let (end, word) = self.word(i);
                match last == Last::Start && STATEMENTS.contains(&word) {
                    true => {
                        *self.stack.last_mut().expect("the expression") = Frame::Statement;
                        self.after_word(word, end)
                    }
                    false => {
                        set(self, Last::Name);
                        end
                    }
                }
            }
            (b'.', Last::Name | Last::Closed) if is_name_start(self.at(i + 1)) => {
                self.blank(i, i + 1);
                set(self, Last::Dot);
                i + 1
            }
            (b'(', Last::Start | Last::Name | Last::Closed) | (b'{', Last::Start) => {
                self.blank(i, i + 1);
                self.push(Frame::Group(if c == b'(' { b')' } else { b'}' }));
                i + 1
            }
            (b'[', Last::Name | Last::Closed) => {
                self.blank(i, i + 1);
                self.push(Frame::Markup {
                    block: true,
                    depth: 0,
                });
                i + 1
            }
            (b'"', Last::Start) => {
                let end = string_end(self.b, i);
                self.blank(i, end);
                set(self, Last::Closed);
                end
            }
            // Anything else is past the expression, read again by what it is in.
            _ => {
                self.stack.pop();
                i
            }
        }
    }

    /// One step of code in a statement or a group.
    fn code(&mut self, i: usize) -> usize {
        match self.b[i] {
            b'$' => {
                self.blank(i, i + 1);
                self.push(Frame::Math);
                i + 1
            }
            b'(' | b'{' => {
                let closer = if self.b[i] == b'(' { b')' } else { b'}' };
                self.blank(i, i + 1);
                self.push(Frame::Group(closer));
                i + 1
            }
            b'[' => {
                self.blank(i, i + 1);
                self.push(Frame::Markup {
                    block: true,
                    depth: 0,
                });
                i + 1
            }
            c if is_name_start(c) => {
                let (end, word) = self.word(i);
                self.after_word(word, end)
            }
            _ => self.code_char(i),
        }
    }

    /// One character of code or math, blanked.
    fn code_char(&mut self, i: usize) -> usize {
        let end = i + char_len(self.b, i);
        self.blank(i, end);
        end
    }

    /// The name at `i`, blanked: where it ends, and the keyword it is, if it is one.
    fn word(&mut self, i: usize) -> (usize, &'static str) {
        let mut end = i;
        while end < self.b.len() && is_name(self.b[end]) {
            end += 1;
        }
        self.blank(i, end);
        let word = STATEMENTS.iter().find(|s| s.as_bytes() == &self.b[i..end]);
        (end, word.copied().unwrap_or(""))
    }

    /// After `include` or `import`, the path its string names, read and blanked.
    fn after_word(&mut self, word: &str, end: usize) -> usize {
        if !matches!(word, "include" | "import") {
            return end;
        }
        let mut at = end;
        while matches!(self.at(at), b' ' | b'\t') {
            at += 1;
        }
        if self.at(at) != b'"' {
            return end;
        }
        let close = string_end(self.b, at);
        let inner = &self.b[at + 1..close.saturating_sub(1).max(at + 1)];
        if let Ok(path) = std::str::from_utf8(inner) {
            self.includes.push(path.to_string());
        }
        self.blank(at, close);
        close
    }
}

/// Whether `c` can follow a `#` as the start of an expression.
fn starts_expr(c: u8) -> bool {
    is_name_start(c) || matches!(c, b'(' | b'{' | b'"')
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

/// Typst's names take `-` too: `my-function`.
fn is_name(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-') || c >= 0x80
}

/// The length of the UTF-8 character starting at `i`, 1 past the end.
fn char_len(b: &[u8], i: usize) -> usize {
    match b.get(i) {
        Some(&c) if c >= 0xF0 => 4,
        Some(&c) if c >= 0xE0 => 3,
        Some(&c) if c >= 0xC0 => 2,
        _ => 1,
    }
}

fn line_end(b: &[u8], i: usize) -> usize {
    b[i..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(b.len(), |p| i + p)
}

/// The end of the `/* … */` at `i`, nested ones counted as Typst counts them.
fn block_comment_end(b: &[u8], i: usize) -> usize {
    let (mut depth, mut j) = (0, i);
    while j + 1 < b.len() {
        match (b[j], b[j + 1]) {
            (b'/', b'*') => {
                depth += 1;
                j += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    return j;
                }
            }
            _ => j += 1,
        }
    }
    b.len()
}

/// The end of the raw text at `i`: as many backticks close it as opened it, `` ` `` inline and
/// three or more a block; two are an empty one.
fn raw_end(b: &[u8], i: usize) -> usize {
    let ticks = b[i..].iter().take_while(|&&c| c == b'`').count();
    if ticks == 2 {
        return i + 2;
    }
    let mut j = i + ticks;
    while j < b.len() {
        let run = b[j..].iter().take_while(|&&c| c == b'`').count();
        if run >= ticks {
            return j + ticks;
        }
        j += run.max(1);
    }
    b.len()
}

/// The end of the string literal at `i`, a backslash escaping what follows it.
fn string_end(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::super::{Lang, symbols};
    use super::*;

    /// Each heading's name, level by its `=`s, and first and last line.
    fn outline(text: &str) -> Vec<(String, u32, u32)> {
        symbols(Lang::Typst, text)
            .into_iter()
            .map(|d| (d.name, d.line, d.end_line))
            .collect()
    }

    fn row(name: &str, first: u32, last: u32) -> (String, u32, u32) {
        (name.to_string(), first, last)
    }

    #[test]
    fn a_heading_spans_its_section_and_holds_the_deeper_ones() {
        let text = "#set page(width: 10cm)\n= Intro <intro>\nText.\n== Detail\nMore.\n= Next\n";
        assert_eq!(
            outline(text),
            [row("Intro", 2, 5), row("Detail", 4, 5), row("Next", 6, 6)]
        );
    }

    #[test]
    fn no_heading_in_code_math_raw_text_or_comments() {
        let text = r#"// = commented
/* = block
   /* = nested */
= still in it */
```typst
= raw
```
$ a
  = b $
#let f(x) = {
  = not markup
}
#set text(font: "
= in a string")
#figure(caption: [
= In content
])[
= Trailing
]
`= inline raw` and #x.y = z
=not a heading
= Last
"#;
        let names: Vec<String> = outline(text).into_iter().map(|(n, ..)| n).collect();
        assert_eq!(names, ["In content", "Trailing", "Last"]);
    }

    #[test]
    fn an_expression_ends_where_markup_goes_on() {
        // Math closing after an expression in it, a web address, an escaped dollar, code before
        // an `=`, a heading of a label alone (tinymist lists none) and one titled by code.
        let text = "Value $#x + 1$ here.\nhttps://typst.app\n= After\n\\$ = 5\n#a = 1\n= <only>\n= #title\n= End\n";
        let names: Vec<String> = outline(text).into_iter().map(|(n, ..)| n).collect();
        assert_eq!(names, ["After", "#title", "End"]);
    }

    #[test]
    fn includes_and_imports_are_read_from_code_only() {
        let text = r#"#import "@preview/cetz:0.3.0": canvas
#import "lib.typ": *
// #include "commented.typ"
We include "prose.typ" here.
#include "chapters/one.typ"
#let two = include "chapters/two.typ"
```
#include "raw.typ"
```
#{
  include "/abs.typ"
}
"#;
        assert_eq!(
            includes(text),
            [
                "@preview/cetz:0.3.0",
                "lib.typ",
                "chapters/one.typ",
                "chapters/two.typ",
                "/abs.typ"
            ]
        );
    }
}
