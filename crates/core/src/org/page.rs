//! An org file as the blocks its page shows, the same [`Block`]s a markdown note's page is
//! ([`crate::markdown::page`]), so the preview, an export and Android write it alike. Read line
//! by line as [`super::analyze`] reads the file, the subset accent shows: headings with their
//! TODO keyword and tags, paragraphs, lists with their boxes, description lists, tables, blocks
//! of code, examples, quotes, verse and centred text, footnotes (an inline one defined after the
//! page, as org's export defines every one), LaTeX environments as formulas, and in prose
//! emphasis, links, pictures, formulas, `<<targets>>`, line breaks and sub- and superscripts in
//! braces (`H_{2}O`, as org's `^:{}` has them, so `snake_case` stays as written).
//!
//! What org's own export leaves out by default is not shown: `#+KEY:` lines (`#+TITLE` too: a
//! markdown note's front matter is no heading either), drawers, comments, comment blocks and
//! export blocks but HTML ones (HTML of the page, as a note's is), a heading's priority and
//! planning line, and a heading marked `COMMENT` with its section.

use super::links::OrgLink;
use super::table::{aligns, cookie};
use super::{Search, closed, emphasis, heading, is_heading, keywords, lines, target};
use crate::markdown::{
    Align, Block, Cell, LinkKind, Mode, Node, Part, Style, cut, is_image, math, parts,
    percent_encode, split, write,
};
use crate::path::parent_dir;
use pulldown_cmark::{
    Alignment, CodeBlockKind, CowStr, Event, HeadingLevel, LinkType, Tag as Cm, TagEnd,
};
use std::ops::Range;

/// The page of an org file ([`crate::markdown::page`]'s for a note): a heading's `id` its title's
/// slug, a link to a heading or a `<<target>>` of the file pointed at its `id`.
pub fn page(text: &str) -> Vec<Block<'_>> {
    let all = lines(text);
    let mut page = Page {
        text,
        keywords: keywords(text, &all),
        nodes: Vec::new(),
        headings: Vec::new(),
        named: Vec::new(),
        targets: Vec::new(),
        notes: Vec::new(),
        verse: false,
    };
    for part in parts(text) {
        match part {
            Part::Stretch(r) => {
                let inside: Vec<_> = all
                    .iter()
                    .filter(|l| l.start >= r.start && l.end <= r.end)
                    .cloned()
                    .collect();
                page.blocks(&inside, false);
            }
            Part::Node(node, bytes) => page.nodes.push((node, bytes)),
        }
    }
    page.finish()
}

/// The preview's HTML of an org file ([`crate::markdown::to_html`]'s rules).
pub fn to_html(text: &str) -> String {
    write(&page(text), Mode::Preview)
}

/// The org file at `rel`, a vault path, as an exported file holds it
/// ([`crate::markdown::to_html_file`]'s rules).
pub fn to_html_file(text: &str, rel: &str) -> String {
    write(
        &page(text),
        Mode::File {
            dir: parent_dir(rel),
        },
    )
}

/// What [`page`] has gathered so far.
struct Page<'a> {
    text: &'a str,
    keywords: Vec<Vec<(&'a str, bool)>>,
    nodes: Vec<(Node<'a>, Range<usize>)>,
    /// Each heading's place in `nodes` and its title, as the index reads it.
    headings: Vec<(usize, String)>,
    /// Each link into the file by name, its place in `nodes` and the name: a `<<target>>` of that
    /// name first, as org searches, else a heading.
    named: Vec<(usize, String)>,
    targets: Vec<String>,
    /// Each inline footnote's label and its text, defined after the page.
    notes: Vec<(CowStr<'a>, Range<usize>)>,
    /// Whether the text being read is a verse block's, every line break of it a hard one.
    verse: bool,
}

/// What a line opens.
enum Kind {
    Blank,
    Heading,
    /// A block of code, an example, an export or a comment, or a drawer, to its last line.
    Closed(usize, Style),
    /// A quote block, to its last line.
    Quote(usize),
    /// A verse block, to its last line.
    Verse(usize),
    /// A centre block, to its last line.
    Center(usize),
    /// A LaTeX environment, `\begin{equation}` to `\end{equation}`, to its last line.
    Latex(usize),
    /// A `#+KEY:` line, the fence of any other block, or a comment.
    Hidden,
    Fixed,
    Table,
    Rule,
    /// A footnote's definition, `[fn:label]`, and where its label ends.
    Footnote(Range<usize>),
    Item,
    Text,
}

impl<'a> Page<'a> {
    fn push(&mut self, node: Node<'a>, bytes: Range<usize>) {
        self.nodes.push((node, bytes));
    }

    /// A start event and the line marker after it, both over `bytes`, which `cut` turns into the
    /// block's and the marker's lines.
    fn open(&mut self, tag: Cm<'a>, bytes: Range<usize>) {
        self.push(Node::Event(Event::Start(tag)), bytes.clone());
        self.line(bytes, None);
    }

    fn line(&mut self, bytes: Range<usize>, id: Option<String>) {
        self.push(Node::Line { at: 0, id }, bytes);
    }

    fn close(&mut self, tag: TagEnd, bytes: Range<usize>) {
        self.push(Node::Event(Event::End(tag)), bytes);
    }

    fn finish(mut self) -> Vec<Block<'a>> {
        // An inline footnote is defined after the page, where org's export puts every footnote;
        // its text may hold one more. The definition is written nowhere, so stands at the end.
        let end = self.text.len()..self.text.len();
        let mut i = 0;
        while let Some((label, body)) = self.notes.get(i).cloned() {
            self.push(
                Node::Event(Event::Start(Cm::FootnoteDefinition(label))),
                end.clone(),
            );
            self.push(Node::Event(Event::Start(Cm::Paragraph)), end.clone());
            self.inline(body);
            self.close(TagEnd::Paragraph, end.clone());
            self.close(TagEnd::FootnoteDefinition, end.clone());
            i += 1;
        }
        let mut anchors = Vec::new();
        for (at, name) in &self.named {
            let target = self.targets.iter().find(|t| t.eq_ignore_ascii_case(name));
            match (target, &mut self.nodes[*at].0) {
                (Some(t), Node::Event(Event::Start(Cm::Link { dest_url, .. }))) => {
                    *dest_url = format!("#{}", percent_encode(t)).into()
                }
                _ => anchors.push(*at),
            }
        }
        cut(self.text, self.nodes, &self.headings, &anchors)
    }

    fn kind(&self, lines: &[Range<usize>], i: usize) -> Kind {
        let text = self.text;
        let line = lines[i].clone();
        let s = text[line.clone()].trim_start();
        let word = |m: &str| {
            s.strip_prefix(m)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
        };
        // The first line of a list item starts past its bullet, where nothing opens a heading.
        let column0 = line.start == 0 || text.as_bytes()[line.start - 1] == b'\n';
        if s.is_empty() {
            Kind::Blank
        } else if column0 && is_heading(&text[line.clone()]) {
            Kind::Heading
        } else if let Some((last, style)) = closed(text, lines, i) {
            Kind::Closed(last, style)
        } else if let Some(kind) = prose_block(text, lines, i) {
            kind
        } else if let Some(last) = environment(text, lines, i) {
            Kind::Latex(last)
        } else if s.starts_with("#+") || word("#") {
            Kind::Hidden
        } else if word(":") {
            Kind::Fixed
        } else if s.starts_with('|') {
            Kind::Table
        } else if s.trim_end().len() >= 5 && s.trim_end().bytes().all(|c| c == b'-') {
            Kind::Rule
        } else if let Some(label) =
            footnote(text, line.start).filter(|_| column0 && s.len() == line.len())
        {
            Kind::Footnote(label)
        } else if bullet(text, line).is_some() {
            Kind::Item
        } else {
            Kind::Text
        }
    }

    /// The elements of `lines`, each read whole. In a list item (`tight`) a first paragraph is
    /// the item's text, as a tight markdown list's is.
    fn blocks(&mut self, lines: &[Range<usize>], mut tight: bool) {
        let mut i = 0;
        while i < lines.len() {
            let blank = matches!(self.kind(lines, i), Kind::Blank | Kind::Hidden);
            i = self.block(lines, i, tight);
            tight &= blank;
        }
    }

    /// The element `lines[i]` opens, and the index of the line after it.
    fn block(&mut self, lines: &[Range<usize>], i: usize, tight: bool) -> usize {
        let text = self.text;
        let span = |a: usize, b: usize| lines[a].start..lines[b].end;
        match self.kind(lines, i) {
            Kind::Blank | Kind::Hidden => i + 1,
            Kind::Heading => self.heading(lines, i),
            Kind::Closed(last, Style::CodeBlock) => {
                let words: Vec<&str> = text[lines[i].clone()].trim()[8..]
                    .split_whitespace()
                    .take(2)
                    .collect();
                let tag = match words[..] {
                    // The words after the language as a fence's info string has them, so a
                    // `search` block's `limit=` reaches it (`markdown::page`).
                    [name, _, ..] if name.eq_ignore_ascii_case("src") => {
                        let info =
                            text[lines[i].clone()].trim()[8..].trim_start()[name.len()..].trim();
                        Cm::CodeBlock(CodeBlockKind::Fenced(info.into()))
                    }
                    [name, ..]
                        if ["src", "example"]
                            .iter()
                            .any(|v| name.eq_ignore_ascii_case(v)) =>
                    {
                        Cm::CodeBlock(CodeBlockKind::Indented)
                    }
                    // An HTML export block is HTML of the page, as a note's HTML is; any other
                    // export block or a comment block shows nothing.
                    [name, backend, ..]
                        if name.eq_ignore_ascii_case("export")
                            && backend.eq_ignore_ascii_case("html") =>
                    {
                        Cm::HtmlBlock
                    }
                    _ => return last + 1,
                };
                self.verbatim(tag, &lines[i + 1..last], span(i, last));
                last + 1
            }
            Kind::Closed(last, _) => last + 1,
            Kind::Quote(last) => {
                let bytes = span(i, last);
                self.open(Cm::BlockQuote(None), bytes.clone());
                self.blocks(&lines[i + 1..last], false);
                self.close(TagEnd::BlockQuote(None), bytes);
                last + 1
            }
            // Each element centred on its own, a block of the page, spaced from the next as any
            // two are; one that shows nothing is left out.
            Kind::Center(last) => {
                let inner = &lines[i + 1..last];
                let mut j = 0;
                while j < inner.len() {
                    let at = self.nodes.len();
                    self.push(Node::Center, inner[j].clone());
                    let next = self.block(inner, j, false);
                    if self.nodes.len() == at + 1 {
                        self.nodes.pop();
                    } else {
                        let bytes = inner[j].start..inner[next - 1].end;
                        self.nodes[at].1 = bytes.clone();
                        self.push(Node::CenterEnd, bytes);
                    }
                    j = next;
                }
                last + 1
            }
            // One paragraph of the lines between the fences, each ending in a break, as org's
            // export writes a verse.
            Kind::Verse(last) => {
                let bytes = span(i, last);
                self.open(Cm::Paragraph, bytes.clone());
                if last > i + 1 {
                    let body = span(i + 1, last - 1);
                    let s = &text[body.clone()];
                    let lead = body.start + s.len() - s.trim_start().len();
                    self.verse = true;
                    self.inline(lead..lead + s.trim().len());
                    self.verse = false;
                }
                self.close(TagEnd::Paragraph, bytes);
                last + 1
            }
            // The whole environment as a formula on a line of its own, as `\[…\]` is.
            Kind::Latex(last) => {
                let bytes = span(i, last);
                let s = &text[bytes.clone()];
                let lead = bytes.start + s.len() - s.trim_start().len();
                self.open(Cm::Paragraph, bytes.clone());
                self.push(
                    math(text[lead..lead + s.trim().len()].into(), true),
                    bytes.clone(),
                );
                self.close(TagEnd::Paragraph, bytes);
                last + 1
            }
            Kind::Fixed => {
                let end = (i..lines.len())
                    .find(|&j| !matches!(self.kind(lines, j), Kind::Fixed))
                    .unwrap_or(lines.len());
                // What follows each line's `:` and the space after it.
                let body: Vec<Range<usize>> = lines[i..end]
                    .iter()
                    .map(|l| {
                        let s = &text[l.clone()];
                        let colon = l.start + s.len() - s.trim_start().len() + 1;
                        (colon + 1).min(l.end)..l.end
                    })
                    .collect();
                let tag = Cm::CodeBlock(CodeBlockKind::Indented);
                self.verbatim(tag, &body, span(i, end - 1));
                end
            }
            Kind::Table => {
                let end = (i..lines.len())
                    .find(|&j| !matches!(self.kind(lines, j), Kind::Table))
                    .unwrap_or(lines.len());
                self.table(&lines[i..end]);
                end
            }
            Kind::Rule => {
                self.push(Node::Event(Event::Rule), lines[i].clone());
                i + 1
            }
            Kind::Footnote(label) => {
                let end = self.paragraph_end(lines, i);
                let bytes = span(i, end - 1);
                let name: CowStr = text[label.start + 4..label.end - 1].into();
                self.push(
                    Node::Event(Event::Start(Cm::FootnoteDefinition(name))),
                    bytes.clone(),
                );
                let body = label.end + text[label.end..bytes.end].len()
                    - text[label.end..bytes.end].trim_start().len();
                self.paragraph(body..bytes.end, false);
                self.close(TagEnd::FootnoteDefinition, bytes);
                end
            }
            Kind::Item => self.list(lines, i),
            Kind::Text => {
                let end = self.paragraph_end(lines, i);
                self.paragraph(span(i, end - 1), tight);
                end
            }
        }
    }

    /// The line after the paragraph starting at `lines[i]`: the next that is blank or opens
    /// something else.
    fn paragraph_end(&self, lines: &[Range<usize>], i: usize) -> usize {
        (i + 1..lines.len())
            .find(|&j| !matches!(self.kind(lines, j), Kind::Text))
            .unwrap_or(lines.len())
    }

    fn paragraph(&mut self, bytes: Range<usize>, tight: bool) {
        if tight {
            return self.inline(bytes);
        }
        self.open(Cm::Paragraph, bytes.clone());
        self.inline(bytes.clone());
        self.close(TagEnd::Paragraph, bytes);
    }

    /// A heading: its TODO keyword, its title and its tags, as links to the tag's notes; a
    /// property drawer under it gives it its `CUSTOM_ID`. The index of the line after it and its
    /// planning line, which org's export leaves out (`org-export-with-planning`), or after its
    /// whole section for one marked `COMMENT`.
    fn heading(&mut self, lines: &[Range<usize>], i: usize) -> usize {
        let text = self.text;
        let line = lines[i].clone();
        let Some(head) = heading(text, line.clone(), &self.keywords) else {
            return i + 1;
        };
        if head.marks.iter().any(|m| &text[m.clone()] == "COMMENT") {
            let stars = |j: usize| {
                text[lines[j].clone()]
                    .bytes()
                    .take_while(|&c| c == b'*')
                    .count()
            };
            return (i + 1..lines.len())
                .find(|&j| is_heading(&text[lines[j].clone()]) && stars(j) <= stars(i))
                .unwrap_or(lines.len());
        }
        let level =
            HeadingLevel::try_from(usize::from(head.level).min(6)).unwrap_or(HeadingLevel::H6);
        self.headings
            .push((self.nodes.len(), text[head.title.clone()].to_string()));
        let start = Cm::Heading {
            level,
            id: None,
            classes: Vec::new(),
            attrs: Vec::new(),
        };
        self.push(Node::Event(Event::Start(start)), line.clone());
        let next =
            i + 1 + usize::from(lines.get(i + 1).is_some_and(|l| planning(&text[l.clone()])));
        let id = custom_id(text, lines, next).map(str::to_string);
        self.line(line.clone(), id);
        if let Some((at, done)) = head.keyword {
            let keyword = Node::Keyword {
                text: text[at.clone()].into(),
                done,
            };
            self.push(keyword, at);
            if !head.title.is_empty() {
                self.push(Node::Event(Event::Text(" ".into())), line.clone());
            }
        }
        self.inline(head.title);
        // The tags as written, `:a:b:`, as a note's `#tag` is: each a link, the colons text.
        if let Some(block) = head.tag_block {
            let mut at = block.start;
            self.push(Node::Event(Event::Text(" ".into())), line.clone());
            for tag in head.tags {
                let colons = text[at..tag.range.start].into();
                self.push(Node::Event(Event::Text(colons)), line.clone());
                let shown = text[tag.range.clone()].into();
                self.push(
                    Node::Tag {
                        name: tag.name,
                        text: shown,
                    },
                    tag.range.clone(),
                );
                at = tag.range.end;
            }
            let last = text[at..block.end].into();
            self.push(Node::Event(Event::Text(last)), line.clone());
        }
        self.close(TagEnd::Heading(level), line);
        next
    }

    /// A block of code, or of HTML for an `HtmlBlock` `tag`: each line of `body` less the
    /// indentation they share, and less the comma org escapes a `*` or a `#+` at the start of a
    /// line with.
    fn verbatim(&mut self, tag: Cm<'a>, body: &[Range<usize>], bytes: Range<usize>) {
        let text = self.text;
        let html = matches!(tag, Cm::HtmlBlock);
        let end = tag.to_end();
        let shown = |s: CowStr<'a>| match html {
            true => Node::Event(Event::Html(s)),
            false => Node::Event(Event::Text(s)),
        };
        let indent = body
            .iter()
            .map(|l| &text[l.clone()])
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.len() - s.trim_start().len())
            .min()
            .unwrap_or(0);
        self.open(tag, bytes.clone());
        for l in body {
            let mut at = (l.start + indent).min(l.end);
            let s = &text[at..l.end];
            let lead = s.len() - s.trim_start().len();
            if s[lead..].starts_with(",*") || s[lead..].starts_with(",#+") {
                if lead > 0 {
                    self.push(shown(text[at..at + lead].into()), l.clone());
                }
                at += lead + 1;
            }
            self.push(shown(format!("{}\n", &text[at..l.end]).into()), l.clone());
        }
        self.close(end, bytes);
    }

    /// A table: rows of cells split on `|`, its first hline closing the header, of every row above
    /// it, where rows follow it, as org exports one; an hline and a row of alignment cookies show
    /// nothing, the cookies
    /// aligning their columns as a column of numbers is aligned (`super::table`).
    fn table(&mut self, rows: &[Range<usize>]) {
        let text = self.text;
        let bytes = rows[0].start..rows[rows.len() - 1].end;
        let hline = |r: &Range<usize>| text[r.clone()].trim_start().starts_with("|-");
        let read: Vec<(Vec<Cell>, bool)> = rows
            .iter()
            .map(|r| (split(&text[r.clone()], false).unwrap_or_default(), hline(r)))
            .collect();
        let aligns: Vec<Alignment> = aligns(&read)
            .into_iter()
            .map(|a| match a {
                Align::None => Alignment::None,
                Align::Left => Alignment::Left,
                Align::Center => Alignment::Center,
                Align::Right => Alignment::Right,
            })
            .collect();
        let shown: Vec<Option<Vec<Range<usize>>>> = rows
            .iter()
            .map(|r| {
                let cells = cells(text, r.clone());
                let special = cells
                    .iter()
                    .all(|c| c.is_empty() || is_cookie(&text[c.clone()]));
                (!hline(r) && !special).then_some(cells)
            })
            .collect();
        // A header is the rows above the first hline that has rows below it.
        let rule = shown
            .iter()
            .position(Option::is_some)
            .and_then(|first| (first..rows.len()).find(|&j| hline(&rows[j])))
            .filter(|&j| shown[j..].iter().any(Option::is_some));
        let columns = aligns.len();
        self.open(Cm::Table(aligns), bytes.clone());
        // A table without a header has an empty one, after which the writer opens its body.
        self.push(Node::Event(Event::Start(Cm::TableHead)), bytes.clone());
        if rule.is_none() {
            self.close(TagEnd::TableHead, bytes.clone());
        }
        // The head opens its first row and closes its last. A row between closes and opens as a
        // body row does, which pulldown-cmark's writer writes as one more row of `th`s.
        let first = shown
            .iter()
            .position(Option::is_some)
            .filter(|_| rule.is_some());
        let last = rule.and_then(|rule| (0..rule).rev().find(|&j| shown[j].is_some()));
        for (j, cells) in shown.iter().enumerate() {
            let Some(cells) = cells else { continue };
            if Some(j) != first {
                self.push(Node::Event(Event::Start(Cm::TableRow)), rows[j].clone());
            }
            for k in 0..columns {
                self.push(Node::Event(Event::Start(Cm::TableCell)), rows[j].clone());
                if let Some(cell) = cells.get(k) {
                    self.inline(cell.clone());
                }
                self.close(TagEnd::TableCell, rows[j].clone());
            }
            match Some(j) == last {
                true => self.close(TagEnd::TableHead, rows[j].clone()),
                false => self.close(TagEnd::TableRow, rows[j].clone()),
            }
        }
        self.close(TagEnd::Table, bytes);
    }

    /// The list whose first item is `lines[i]`, and the index of the line after it: its items are
    /// at that item's indentation, each running on over the lines indented more, a list ending at
    /// a line indented no more that is no item, or at two blank lines. Its first item decides
    /// whether it is ordered, or a description list (`- term :: description`).
    fn list(&mut self, lines: &[Range<usize>], i: usize) -> usize {
        let text = self.text;
        let indent = |j: usize| indentation(text, lines[j].clone());
        let ind = indent(i);
        let mut items = Vec::new();
        let mut j = i;
        let end = loop {
            let (mut k, mut blanks) = (j + 1, 0);
            while k < lines.len() {
                if text[lines[k].clone()].trim().is_empty() {
                    blanks += 1;
                    if blanks == 2 {
                        break;
                    }
                } else if indent(k) <= ind {
                    break;
                } else {
                    blanks = 0;
                }
                k += 1;
            }
            let last = (j..k)
                .rev()
                .find(|&l| !text[lines[l].clone()].trim().is_empty())
                .unwrap_or(j);
            items.push(j..last + 1);
            let sibling = k < lines.len()
                && blanks < 2
                && indent(k) == ind
                && matches!(self.kind(lines, k), Kind::Item);
            match sibling {
                true => j = k,
                false => break last + 1,
            }
        };
        let bytes = lines[i].start..lines[end - 1].end;
        let (ordered, body) = bullet(text, lines[i].clone()).unwrap_or_default();
        let described = ordered.is_none() && term(text, body..lines[i].end).is_some();
        let list = match described {
            true => Cm::DefinitionList,
            false => Cm::List(ordered),
        };
        self.push(Node::Event(Event::Start(list)), bytes.clone());
        for item in items {
            self.item(&lines[item], described);
        }
        let end_tag = match described {
            true => TagEnd::DefinitionList,
            false => TagEnd::List(ordered.is_some()),
        };
        self.close(end_tag, bytes);
        end
    }

    /// One item of a list: its box, as a task's, or its term, then what it holds.
    fn item(&mut self, lines: &[Range<usize>], described: bool) {
        let text = self.text;
        let first = lines[0].clone();
        let bytes = first.start..lines[lines.len() - 1].end;
        let Some((_, mut body)) = bullet(text, first.clone()) else {
            return;
        };
        let rest = |body: usize| -> Vec<Range<usize>> {
            std::iter::once(body..first.end)
                .chain(lines[1..].iter().cloned())
                .collect()
        };
        if described {
            if let Some((term, after)) = term(text, body..first.end) {
                self.open(Cm::DefinitionListTitle, bytes.clone());
                self.inline(term);
                self.close(TagEnd::DefinitionListTitle, bytes.clone());
                body = after;
                self.push(
                    Node::Event(Event::Start(Cm::DefinitionListDefinition)),
                    bytes.clone(),
                );
            } else {
                self.open(Cm::DefinitionListDefinition, bytes.clone());
            }
            self.blocks(&rest(body), true);
            return self.close(TagEnd::DefinitionListDefinition, bytes);
        }
        let tick = checkbox(text, body..first.end);
        match tick {
            Some(_) => self.push(Node::TaskItem, bytes.clone()),
            None => self.push(Node::Event(Event::Start(Cm::Item)), bytes.clone()),
        }
        self.line(bytes.clone(), None);
        if let Some((tick, after)) = tick {
            let node = match tick {
                '-' => Node::TaskDoing,
                c => Node::Event(Event::TaskListMarker(c != ' ')),
            };
            self.push(node, body..after);
            body = after;
        }
        self.blocks(&rest(body), true);
        self.close(TagEnd::Item, bytes);
    }

    /// The objects of the prose at `at`, over one line or several: emphasis, code, links,
    /// pictures, formulas, footnote references, `<<targets>>`, sub- and superscripts in braces,
    /// and the text between them.
    fn inline(&mut self, at: Range<usize>) {
        let text = self.text;
        let b = text.as_bytes();
        let (mut run, mut i, mut eol) = (at.start, at.start, at.start);
        while i < at.end {
            // Where the line ends: a link is on one.
            if i >= eol {
                eol = b[i..at.end]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(at.end, |n| i + n);
            }
            let before = run..i;
            let found = match b[i] {
                b'[' => super::links::bracket(text, i, eol)
                    .map(Found::Link)
                    .or_else(|| footnote(text, i).map(Found::Footnote))
                    .or_else(|| inline_footnote(text, i, at.end).map(Found::Note)),
                b'<' if b.get(i + 1) == Some(&b'<') => target(text, i, eol).map(Found::Target),
                b'<' => super::links::angle(text, i, eol).map(Found::Link),
                b'\\' => latex(text, i, at.end).map(Found::Math),
                b'$' => dollars(text, i, at.end).map(Found::Math),
                c @ (b'_' | b'^') if b.get(i + 1) == Some(&b'{') && i > at.start => {
                    script(text, i, eol).map(|r| Found::Script(c == b'^', r))
                }
                _ => None,
            };
            let found = found.or_else(|| match b[i] {
                c @ (b'=' | b'~' | b'*' | b'/' | b'_' | b'+') => {
                    emphasis(text, at.start, i, at.end).map(|end| Found::Emphasis(c, i..end))
                }
                c if c.is_ascii_alphabetic()
                    && (i == at.start || !b[i - 1].is_ascii_alphanumeric()) =>
                {
                    super::links::plain(text, i, eol).map(Found::Link)
                }
                _ => None,
            });
            let Some(found) = found else {
                i += 1;
                continue;
            };
            self.plain(before);
            i = self.object(found);
            run = i;
        }
        self.plain(run..at.end);
    }

    /// The nodes of an object [`Page::inline`] found, and where it ends.
    fn object(&mut self, found: Found) -> usize {
        let text = self.text;
        match found {
            Found::Link(link) => {
                let end = link.range.end;
                self.link(&link);
                end
            }
            Found::Footnote(r) => {
                let name: CowStr = text[r.start + 4..r.end - 1].into();
                self.push(Node::Event(Event::FootnoteReference(name)), r.clone());
                r.end
            }
            // One without a label is named by its place among them, after a `:` no label holds.
            Found::Note((r, label, body)) => {
                let label: CowStr = match label.is_empty() {
                    true => format!(":{}", self.notes.len() + 1).into(),
                    false => text[label].into(),
                };
                self.notes.push((label.clone(), body));
                self.push(Node::Event(Event::FootnoteReference(label)), r.clone());
                r.end
            }
            Found::Target((r, name)) => {
                self.targets.push(name.clone());
                self.push(Node::Anchor(name), r.clone());
                r.end
            }
            Found::Math((r, tex, display)) => {
                self.push(math(text[tex].into(), display), r.clone());
                r.end
            }
            Found::Script(up, r) => {
                let (start, end) = match up {
                    true => (Cm::Superscript, TagEnd::Superscript),
                    false => (Cm::Subscript, TagEnd::Subscript),
                };
                self.push(Node::Event(Event::Start(start)), r.clone());
                self.inline(r.start + 2..r.end - 1);
                self.close(end, r.clone());
                r.end
            }
            Found::Emphasis(c, r) => {
                let inner = r.start + 1..r.end - 1;
                let (start, end) = match c {
                    b'=' | b'~' => {
                        self.push(Node::Event(Event::Code(text[inner].into())), r.clone());
                        return r.end;
                    }
                    b'*' => (
                        Node::Event(Event::Start(Cm::Strong)),
                        Node::Event(Event::End(TagEnd::Strong)),
                    ),
                    b'/' => (
                        Node::Event(Event::Start(Cm::Emphasis)),
                        Node::Event(Event::End(TagEnd::Emphasis)),
                    ),
                    b'_' => (Node::Underline, Node::UnderlineEnd),
                    _ => (
                        Node::Event(Event::Start(Cm::Strikethrough)),
                        Node::Event(Event::End(TagEnd::Strikethrough)),
                    ),
                };
                self.push(start, r.clone());
                self.inline(inner);
                self.push(end, r.clone());
                r.end
            }
        }
    }

    /// A link as org shows it: a web address and a file as links, the file by its path from the
    /// file's folder as a markdown link's is, a picture with no description in place, a heading,
    /// a custom ID or a `<<target>>` of the file as a link into the page; anything else (an `id:`
    /// link, `elisp:`, an absolute path) its text alone. Without a description a link shows its
    /// address, path or name.
    fn link(&mut self, link: &OrgLink) {
        let text = self.text;
        let r = link.range.clone();
        let Some(read) = link.to_link(text) else {
            return match link.desc.clone() {
                Some(desc) => self.inline(desc),
                None => self.plain(link.link.clone()),
            };
        };
        let anchor = read.anchor.as_deref();
        let dest = match read.kind {
            LinkKind::External => read.target.clone(),
            LinkKind::Markdown
                if link.desc.is_none() && anchor.is_none() && is_image(&read.target) =>
            {
                let image = Cm::Image {
                    link_type: LinkType::Inline,
                    dest_url: percent_encode(&read.target).into(),
                    title: "".into(),
                    id: "".into(),
                };
                self.push(Node::Event(Event::Start(image)), r.clone());
                return self.close(TagEnd::Image, r);
            }
            LinkKind::Markdown => match anchor {
                Some(a) => format!("{}#{}", percent_encode(&read.target), percent_encode(a)),
                None => percent_encode(&read.target),
            },
            _ => match anchor.map(Search::of) {
                Some(Search::CustomId(id)) => format!("#{}", percent_encode(id)),
                Some(Search::Name(name)) => {
                    self.named.push((self.nodes.len(), name.to_string()));
                    format!("#{}", percent_encode(name))
                }
                _ => {
                    return match link.desc.clone() {
                        Some(desc) => self.inline(desc),
                        None => self.plain(link.link.clone()),
                    };
                }
            },
        };
        let start = Cm::Link {
            link_type: LinkType::Inline,
            dest_url: dest.into(),
            title: "".into(),
            id: "".into(),
        };
        self.push(Node::Event(Event::Start(start)), r.clone());
        match link.desc.clone() {
            Some(desc) => self.inline(desc),
            None => {
                let shown = match read.kind {
                    LinkKind::Wiki => anchor.unwrap_or_default().to_string(),
                    _ => read.target,
                };
                self.push(Node::Event(Event::Text(shown.into())), r.clone());
            }
        }
        self.close(TagEnd::Link, r);
    }

    /// Text as written, a line break a soft one and the next line's indentation gone; a hard one
    /// after a `\\` ending the line, and every one in a verse.
    fn plain(&mut self, at: Range<usize>) {
        let text = self.text;
        let mut start = at.start;
        let mut hard = false;
        for (n, piece) in text[at.clone()].split('\n').enumerate() {
            let r = start..start + piece.len();
            start = r.end + 1;
            let mut shown = &text[r.clone()];
            if n > 0 {
                let brk = match hard || self.verse {
                    true => Event::HardBreak,
                    false => Event::SoftBreak,
                };
                self.push(Node::Event(brk), r.clone());
                shown = shown.trim_start();
            }
            // Org's line break, `\\` and blanks to the end of the line, after no third `\`.
            let eol = (text.as_bytes().get(r.end)).is_none_or(|&c| c == b'\n' || c == b'\r');
            let cut = (shown.trim_end().strip_suffix("\\\\")).filter(|s| eol && !s.ends_with('\\'));
            hard = cut.is_some();
            if let Some(cut) = cut {
                shown = cut.trim_end();
            }
            if r.end < at.end {
                shown = shown.trim_end();
            }
            if !shown.is_empty() {
                self.push(Node::Event(Event::Text(shown.into())), r);
            }
        }
    }
}

/// An object [`Page::inline`] found.
enum Found {
    Link(OrgLink),
    /// `[fn:label]`.
    Footnote(Range<usize>),
    /// An inline footnote, `[fn::text]` or `[fn:label:text]`: its label and its text.
    Note((Range<usize>, Range<usize>, Range<usize>)),
    Target((Range<usize>, String)),
    /// The whole formula, its LaTeX, and whether it is shown on a line of its own.
    Math((Range<usize>, Range<usize>, bool)),
    /// `^{…}` or `_{…}`, and whether it is a superscript.
    Script(bool, Range<usize>),
    Emphasis(u8, Range<usize>),
}

/// The quote, verse or centre block opening on `lines[i]`, to its last line before the next
/// heading.
fn prose_block(text: &str, lines: &[Range<usize>], i: usize) -> Option<Kind> {
    let at = |j: usize| text[lines[j].clone()].trim();
    let name = (at(i).get(..8))
        .filter(|begin| begin.eq_ignore_ascii_case("#+begin_"))
        .and_then(|_| at(i)[8..].split_whitespace().next())?;
    let kind = match name.to_ascii_lowercase().as_str() {
        "quote" => Kind::Quote,
        "verse" => Kind::Verse,
        "center" => Kind::Center,
        _ => return None,
    };
    let end = format!("#+end_{name}");
    (i + 1..lines.len())
        .take_while(|&j| !is_heading(&text[lines[j].clone()]))
        .find(|&j| at(j).eq_ignore_ascii_case(&end))
        .map(kind)
}

/// The last line of the LaTeX environment opening on `lines[i]`, `\begin{name}` to a line of
/// `\end{name}` alone, before the next heading.
fn environment(text: &str, lines: &[Range<usize>], i: usize) -> Option<usize> {
    let at = |j: usize| text[lines[j].clone()].trim();
    let (name, _) = at(i).strip_prefix("\\begin{")?.split_once('}')?;
    if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'*') {
        return None;
    }
    let end = format!("\\end{{{name}}}");
    (i + 1..lines.len())
        .take_while(|&j| !is_heading(&text[lines[j].clone()]))
        .find(|&j| at(j) == end)
}

/// Whether `line`, right under a heading, is its planning line: `SCHEDULED:`, `DEADLINE:` or
/// `CLOSED:` and their times.
fn planning(line: &str) -> bool {
    let s = line.trim_start();
    ["SCHEDULED:", "DEADLINE:", "CLOSED:"]
        .iter()
        .any(|k| s.starts_with(k))
}

/// The value of the `CUSTOM_ID` in the property drawer opening on `lines[i]`.
fn custom_id<'a>(text: &'a str, lines: &[Range<usize>], i: usize) -> Option<&'a str> {
    let opens = text[lines.get(i)?.clone()]
        .trim()
        .eq_ignore_ascii_case(":PROPERTIES:");
    let (last, _) = closed(text, lines, i).filter(|_| opens)?;
    lines[i + 1..last].iter().find_map(|l| {
        let s = text[l.clone()].trim();
        s.get(..11)
            .filter(|key| key.eq_ignore_ascii_case(":CUSTOM_ID:"))
            .map(|_| s[11..].trim())
            .filter(|id| !id.is_empty())
    })
}

/// The number an ordered list item on `line` counts from, and where its text starts past its
/// bullet or counter; `None` for a line that is no item. A `*` at the start of a line opens a
/// heading, so a `*` bullet is indented, as the editor reads one.
fn bullet(text: &str, line: Range<usize>) -> Option<(Option<u64>, usize)> {
    let s = &text[line.clone()];
    let lead = s.len() - s.trim_start_matches([' ', '\t']).len();
    let rest = &s[lead..];
    let (len, number) = match rest.bytes().next()? {
        b'-' | b'+' => (1, None),
        b'*' if indentation(text, line.clone()) > 0 => (1, None),
        _ => {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            match rest.as_bytes().get(digits) {
                Some(b'.' | b')') if digits > 0 => (digits + 1, rest[..digits].parse().ok()),
                _ => return None,
            }
        }
    };
    let after = &rest[len..];
    if !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let at = line.start + lead + len;
    Some((number, at + after.len() - after.trim_start().len()))
}

/// How far `line` is indented, from the start of the line it is on.
fn indentation(text: &str, line: Range<usize>) -> usize {
    let start = text[..line.start].rfind('\n').map_or(0, |n| n + 1);
    let s = &text[start..line.end];
    s.len() - s.trim_start().len()
}

/// An item's checkbox at the start of `at`, `[ ]`, `[-]` or `[X]`: its tick and where the text
/// after it starts.
fn checkbox(text: &str, at: Range<usize>) -> Option<(char, usize)> {
    let s = &text[at.clone()];
    let tick = s.strip_prefix('[')?.chars().next()?;
    let after = s.get(3..)?;
    (matches!(tick, ' ' | '-' | 'X' | 'x')
        && s.get(2..3) == Some("]")
        && after.chars().next().is_none_or(char::is_whitespace))
    .then(|| (tick, at.start + 3 + after.len() - after.trim_start().len()))
}

/// A description item's term, `term :: description` at `at`, and where its description starts.
fn term(text: &str, at: Range<usize>) -> Option<(Range<usize>, usize)> {
    let s = &text[at.clone()];
    let sep = s
        .match_indices(" ::")
        .map(|(i, _)| i)
        .find(|&i| s[i + 3..].chars().next().is_none_or(char::is_whitespace))?;
    let after = &s[sep + 3..];
    Some((
        at.start..at.start + s[..sep].trim_end().len(),
        at.start + sep + 3 + after.len() - after.trim_start().len(),
    ))
}

/// `[fn:label]` at `i`: a footnote's reference, or at the start of a line its definition.
fn footnote(text: &str, i: usize) -> Option<Range<usize>> {
    let rest = text[i..].strip_prefix("[fn:")?;
    let len = rest
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        .count();
    (len > 0 && rest[len..].starts_with(']')).then(|| i..i + 4 + len + 1)
}

/// An inline footnote at `i`, `[fn::text]` or `[fn:label:text]`, its brackets balanced within
/// `end`: the whole, its label (empty for none) and its text.
fn inline_footnote(
    text: &str,
    i: usize,
    end: usize,
) -> Option<(Range<usize>, Range<usize>, Range<usize>)> {
    let rest = text[i..end].strip_prefix("[fn:")?;
    let len = rest
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        .count();
    if !rest[len..].starts_with(':') {
        return None;
    }
    let body = i + 4 + len + 1;
    let mut depth = 0usize;
    for (k, c) in text[body..end].bytes().enumerate() {
        match c {
            b'[' => depth += 1,
            b']' if depth == 0 => {
                return Some((i..body + k + 1, i + 4..i + 4 + len, body..body + k));
            }
            b']' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The cells of the table row at `row`, each without the blanks around it.
fn cells(text: &str, row: Range<usize>) -> Vec<Range<usize>> {
    let s = &text[row.clone()];
    let start = row.start + s.len() - s.trim_start().len() + 1;
    let end = row.start + s.trim_end().len();
    let end = end - usize::from(end > start && text.as_bytes()[end - 1] == b'|');
    let mut out = Vec::new();
    let mut from = start;
    for piece in text[start..end.max(start)].split('|') {
        let lead = piece.len() - piece.trim_start().len();
        out.push(from + lead..from + piece.trim_end().len());
        from += piece.len() + 1;
    }
    out
}

/// Whether a cell is an alignment or a width cookie, `<r>`, `<l10>`, `<10>`: a row of them is
/// org's own and not shown.
fn is_cookie(cell: &str) -> bool {
    cookie(cell).is_some()
        || cell
            .strip_prefix('<')
            .and_then(|c| c.strip_suffix('>'))
            .is_some_and(|w| !w.is_empty() && w.bytes().all(|c| c.is_ascii_digit()))
}

/// `\(…\)` or `\[…\]` at `i`: the formula, its LaTeX, and whether it is on a line of its own.
fn latex(text: &str, i: usize, end: usize) -> Option<(Range<usize>, Range<usize>, bool)> {
    let rest = &text[i..end];
    let (close, display) = match rest.get(..2)? {
        "\\(" => ("\\)", false),
        "\\[" => ("\\]", true),
        _ => return None,
    };
    let len = rest[2..].find(close)?;
    (len > 0).then(|| (i..i + len + 4, i + 2..i + 2 + len, display))
}

/// `$$…$$` or `$…$` at `i`, by org's rules for the single one: not after a `$`, its first
/// character no blank, `.`, `,`, `;` or `$`, its last no blank, `.`, `,` or `$`, and a blank,
/// punctuation or the end after it, so `$5 and $10` stays text.
fn dollars(text: &str, i: usize, end: usize) -> Option<(Range<usize>, Range<usize>, bool)> {
    let b = text.as_bytes();
    if i > 0 && b[i - 1] == b'$' {
        return None;
    }
    if text[i..end].starts_with("$$") {
        let len = text[i + 2..end].find("$$")?;
        return (len > 0).then(|| (i..i + len + 4, i + 2..i + 2 + len, true));
    }
    let first = *b.get(i + 1).filter(|_| i + 1 < end)?;
    if first.is_ascii_whitespace() || b".,;$".contains(&first) {
        return None;
    }
    let close = i + 1 + text[i + 1..end].find('$')?;
    let last = b[close - 1];
    let after = b.get(close + 1).filter(|_| close + 1 < end);
    let ends = after.is_none_or(|c| c.is_ascii_whitespace() || c.is_ascii_punctuation());
    (!last.is_ascii_whitespace() && !b".,$".contains(&last) && ends)
        .then(|| (i..close + 1, i + 1..close, false))
}

/// `^{…}` or `_{…}` at `i`, after a character that is no blank, within the line ending at `eol`.
fn script(text: &str, i: usize, eol: usize) -> Option<Range<usize>> {
    if text.as_bytes()[i - 1].is_ascii_whitespace() {
        return None;
    }
    let len = text[i + 2..eol].find('}')?;
    (len > 0).then(|| i..i + 2 + len + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An org `search` src block is a saved search as a note's fence is, its header's `limit=`
    /// read; any other src block keeps its language first.
    #[test]
    fn a_search_src_block_is_a_saved_search() {
        let blocks = page("#+begin_src search limit=3\nkind:pdf\n#+end_src\n");
        assert!(
            blocks[0].nodes.contains(&Node::SavedSearch {
                query: "kind:pdf".into(),
                limit: 3
            }),
            "{:?}",
            blocks[0].nodes
        );
        let html = to_html("#+begin_src python :results output\nx = 1\n#+end_src\n");
        assert!(html.contains("<code class=\"language-python\">"), "{html}");
    }

    const ORG: &str = "#+TITLE: Fixture
#+FILETAGS: :f:
#+TODO: TODO NEXT | DONE
* NEXT [#A] Plan :work:urgent:
:PROPERTIES:
:CUSTOM_ID: plan-id
:END:
Some *bold*, /it/, _under_, +gone+, =verb=, ~code~, x^{2} and H_{2}O.
See [[file:sub/a.org::*Intro][the intro]], [[https://orgmode.org][Org]], <https://x.org>,
[[*Plan]], [[here]], [[#plan-id]] and [[id:1234][by id]].
<<here>> \\(a+b\\) and $c$ cost $5 and $10.
\\[
x^2
\\]
[[file:fig.png]]
# a comment
#+begin_src python :results output
  ,* not a heading
  x = 1
#+end_src
#+begin_example
raw *text*
#+end_example
#+begin_quote
Quoted /words/.
#+end_quote
| <r>  |     |
| Name | Qty |
|------+-----|
| tea  | 12  |

- [X] done
- [-] doing
- [ ] open
  + nested

Then:
1. first
2. second

Terms:
- term :: meaning
Note.[fn:1]

[fn:1] The footnote.
* COMMENT Draft
Left out.
** Under it
* DONE Shipped
";

    /// The page's HTML without the preview's line markers, which are tested on their own.
    fn bare(text: &str) -> String {
        let mut out = to_html(text);
        while let Some(i) = out.find("<span data-line=\"") {
            let j = out[i..].find("></span>").unwrap() + i + "></span>".len();
            let id = out[i..j]
                .find(" id=")
                .map(|k| format!("<span{}", &out[i + k..j]));
            out.replace_range(i..j, &id.unwrap_or_default());
        }
        out
    }

    #[test]
    fn an_org_file_shows_as_its_page() {
        let h = bare(ORG);
        for want in [
            "<h1 id=\"plan\"><span id=\"plan-id\"></span><span class=\"todo\">NEXT</span> Plan \
             :<a href=\"accent://tag/work\" class=\"tag\">work</a>:\
             <a href=\"accent://tag/urgent\" class=\"tag\">urgent</a>:</h1>",
            "<p>Some <strong>bold</strong>, <em>it</em>, <u>under</u>, <del>gone</del>, \
             <code>verb</code>, <code>code</code>, x<sup>2</sup> and H<sub>2</sub>O.\n",
            "<a href=\"sub/a.org#Intro\">the intro</a>",
            "<a href=\"https://orgmode.org\">Org</a>",
            "<a href=\"https://x.org\">https://x.org</a>",
            "<a href=\"#plan\">Plan</a>, <a href=\"#here\">here</a>, \
             <a href=\"#plan-id\">#plan-id</a> and by id.",
            "<span id=\"here\"></span> <math",
            " cost $5 and $10.",
            "<math display=\"block\">",
            "<img src=\"fig.png\" alt=\"\" />",
            "<pre><code class=\"language-python\">* not a heading\nx = 1\n</code></pre>",
            "<pre><code>raw *text*\n</code></pre>",
            "<p>Quoted <em>words</em>.</p>\n</blockquote>",
            "<table><thead><tr><th style=\"text-align: right\">Name</th>\
             <th style=\"text-align: right\">Qty</th></tr></thead><tbody>\n\
             <tr><td style=\"text-align: right\">tea</td>",
            "<li class=\"task-list-item\"><input disabled=\"\" type=\"checkbox\" checked=\"\"/>\ndone</li>",
            "<input disabled=\"\" type=\"checkbox\" class=\"in-progress\"/>\ndoing</li>",
            "<input disabled=\"\" type=\"checkbox\"/>\nopen\n<ul>\n<li>nested</li>",
            "<ol>\n<li>first</li>\n<li>second</li>\n</ol>",
            "<dl>\n<dt>term</dt>\n<dd>meaning</dd>\n</dl>",
            "<p>Note.<sup class=\"footnote-reference\"><a href=\"#1\">1</a></sup></p>",
            "<div class=\"footnote-definition\" id=\"1\"><sup class=\"footnote-definition-label\">1</sup>\n\
             <p>The footnote.</p>",
            "<h1 id=\"shipped\"><span class=\"done\">DONE</span> Shipped</h1>",
        ] {
            assert!(h.contains(want), "{want}\nin\n{h}");
        }
        for hidden in [
            "Fixture",
            "Draft",
            "Left out",
            "Under it",
            "FILETAGS",
            "PROPERTIES",
            "[#A]",
            "COMMENT",
            "a comment",
            "results",
            "&lt;r&gt;",
            "c$",
        ] {
            assert!(!h.contains(hidden), "{hidden} in\n{h}");
        }
        // A link leaves the vault behind in an exported file, its text staying.
        let file = to_html_file(ORG, "Notes/x.org");
        assert!(
            file.contains("the intro") && !file.contains("sub/a.org"),
            "{file}"
        );
        assert!(
            file.contains("src=\"accent://file/Notes/fig.png\""),
            "{file}"
        );
    }

    /// A heading's planning line is left out, as org's export leaves it, and a drawer under it
    /// still gives the heading its custom ID; anywhere else such a line is text.
    #[test]
    fn a_planning_line_is_hidden() {
        let h = bare(
            "* TODO Call\nSCHEDULED: <2026-10-30 Fri> DEADLINE: <2026-11-02 Mon>\n\
             :PROPERTIES:\n:CUSTOM_ID: call\n:END:\nBody.\n\nCLOSED: as text\n",
        );
        assert!(h.contains("<span id=\"call\"></span>"), "{h}");
        assert!(!h.contains("SCHEDULED") && !h.contains("2026"), "{h}");
        assert!(h.contains("<p>Body.</p>\n<p>CLOSED: as text</p>"), "{h}");
    }

    /// An inline footnote is numbered as a reference is, its text defined after the page; one
    /// with a label is the footnote a reference by that label names.
    #[test]
    fn an_inline_footnote_is_defined_after_the_page() {
        let h = bare("A[fn::one /it/] B[fn:n:two [x]] C[fn:n].\n\nLast.\n");
        let note = |label: &str, n: u32| {
            format!("<sup class=\"footnote-reference\"><a href=\"#{label}\">{n}</a></sup>")
        };
        assert!(
            h.starts_with(&format!(
                "<p>A{} B{} C{}.</p>\n<p>Last.</p>\n",
                note(":1", 1),
                note("n", 2),
                note("n", 2)
            )),
            "{h}"
        );
        assert!(
            h.contains(
                "<div class=\"footnote-definition\" id=\":1\">\
                 <sup class=\"footnote-definition-label\">1</sup>\n<p>one <em>it</em></p>\n</div>"
            ),
            "{h}"
        );
        assert!(h.ends_with("2</sup>\n<p>two [x]</p>\n</div>\n"), "{h}");
        let last = page("x[fn::y]\n").pop().unwrap();
        assert_eq!(last.bytes, 9..9);
    }

    /// `\\` ending a line breaks it there, gone from the text; anywhere else it is text.
    #[test]
    fn a_backslash_pair_ending_a_line_breaks_it() {
        let h = bare("One\\\\\n*two* \\\\  \nthree \\\\ four\\\\\\\nfive\\\\\n");
        assert_eq!(
            h,
            "<p>One<br />\n<strong>two</strong><br />\nthree \\\\ four\\\\\\\nfive</p>\n"
        );
    }

    /// A verse block keeps its line breaks, a blank line among them.
    #[test]
    fn a_verse_keeps_its_line_breaks() {
        let h =
            bare("#+begin_verse\n  Roses are /red/,\n    violets blue.\n\nThe end.\n#+end_verse\n");
        assert_eq!(
            h,
            "<p>Roses are <em>red</em>,<br />\nviolets blue.<br />\n<br />\nThe end.</p>\n"
        );
    }

    /// Each paragraph of a centre block is a block of the page, shown centred.
    #[test]
    fn a_centre_block_is_centred() {
        let src = "#+begin_center\nMiddle\nline.\n\nNext *one*.\n#+end_center\nAfter.\n";
        let div = "<div style=\"text-align: center\">\n";
        assert_eq!(
            bare(src),
            format!(
                "{div}<p>Middle\nline.</p>\n</div>\n\
                 {div}<p>Next <strong>one</strong>.</p>\n</div>\n<p>After.</p>\n"
            )
        );
        let lines: Vec<_> = page(src).into_iter().map(|b| b.lines).collect();
        assert_eq!(lines, [2..4, 5..6, 7..8]);
    }

    /// A LaTeX environment is a formula on its own, the environment whole; one that does not
    /// close is text.
    #[test]
    fn a_latex_environment_is_a_formula() {
        let src = "Before\n\\begin{align*}\na &= b \\\\\nc &= d\n\\end{align*}\n\\begin{x}\nopen\n";
        let blocks = page(src);
        let Some(Node::Math { tex, display, .. }) = blocks[1].nodes.get(2) else {
            panic!("{:?}", blocks[1].nodes);
        };
        assert_eq!(
            (&**tex, *display),
            ("\\begin{align*}\na &= b \\\\\nc &= d\n\\end{align*}", true)
        );
        assert_eq!(blocks[1].lines, 2..6);
        let h = bare(src);
        assert!(
            h.starts_with("<p>Before</p>\n<p><math display=\"block\">"),
            "{h}"
        );
        assert!(h.ends_with("</math></p>\n<p>\\begin{x}\nopen</p>\n"), "{h}");
    }

    /// Every row above a table's first hline is a head row, an hline under the head the end of
    /// the head alone.
    #[test]
    fn every_row_above_the_first_hline_is_a_head_row() {
        let h = bare("| A | B |\n| a | b |\n|---+---|\n| c | d |\n|---+---|\n| e | f |\n");
        assert_eq!(
            h,
            "<table><thead><tr><th>A</th><th>B</th></tr>\n<tr><th>a</th><th>b</th></tr>\
             </thead><tbody>\n<tr><td>c</td><td>d</td></tr>\n<tr><td>e</td><td>f</td></tr>\n\
             </tbody></table>\n"
        );
    }

    /// An HTML export block is HTML of the page, line marker first, as a note's HTML block is;
    /// one for another backend shows nothing.
    #[test]
    fn an_html_export_block_is_html() {
        let src = "#+begin_export html\n<b>bold</b> &amp;\n,#+not a keyword\n#+end_export\n\
                   #+BEGIN_EXPORT latex\n\\newpage\n#+END_EXPORT\n";
        assert_eq!(
            to_html(src),
            "<span data-line=\"1\"></span><b>bold</b> &amp;\n#+not a keyword\n"
        );
    }

    /// An anchor names the `id` its element has on the page, a target's before a heading's.
    #[test]
    fn an_anchor_is_its_id_on_the_page() {
        let h = to_html(ORG);
        for (anchor, id) in [("Plan", "plan"), ("#plan-id", "plan-id"), ("HERE", "here")] {
            assert_eq!(super::super::anchor_id(ORG, anchor).as_deref(), Some(id));
            assert!(h.contains(&format!("id=\"{id}\"")), "{id}");
        }
        assert_eq!(super::super::anchor_id(ORG, "missing"), None);
    }

    /// Every block is the bytes and lines of an element, on character boundaries whatever the
    /// text, with each line marker on its element's first line.
    #[test]
    fn every_block_is_on_its_lines() {
        let blocks = page(ORG);
        let first = |b: &Block| {
            ORG[b.bytes.clone()]
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        };
        let heads: Vec<String> = blocks.iter().map(first).collect();
        assert_eq!(heads[0], "* NEXT [#A] Plan :work:urgent:");
        assert_eq!(blocks[0].lines, 4..5);
        assert!(heads.contains(&"| <r>  |     |".to_string()), "{heads:?}");
        for (s, sample) in [("é", 0), ("中", 1), ("🎉", 2)] {
            let doc = ORG.replace(['a', 'e', 'o'], s);
            for b in page(&doc) {
                assert!(
                    doc.is_char_boundary(b.bytes.start) && doc.is_char_boundary(b.bytes.end),
                    "{sample}: {:?}",
                    b.bytes
                );
            }
        }
    }
}
