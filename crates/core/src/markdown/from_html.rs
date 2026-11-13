//! Clipboard HTML as the Markdown a note is written in: what `Ctrl+V` puts into a markdown note
//! when another application copied formatted text (DESIGN.md, Editing).
//!
//! The HTML is read with the tolerant tag reader labels use (`accent_drawio::html`) into a small
//! tree, which is written block by block: headings, paragraphs, lists, quotes, fenced code, GFM
//! tables and rules, with emphasis, strikethrough, code, links and pictures inline. What Markdown
//! has no syntax for is kept as the raw HTML a note may hold (`<u>`, `<sub>`, `<sup>`) or left out
//! (colours, fonts, sizes). Text is escaped only where it would read as markup.

use accent_drawio::Marks;
use accent_drawio::html::{Tag, attributes, html_char};

/// The Markdown `html` reads as, or `None` where it carries no formatting worth keeping (see
/// [`formatted`]), so the paste takes the clipboard's plain text instead.
pub fn from_html(html: &str) -> Option<String> {
    let root = parse(copied(html));
    if !carries_formatting(&root) {
        return None;
    }
    let md = join(&Writer::default().blocks(&root.children, &root), false);
    // A link alone that shows its own address, as a browser's Copy Link copies one, is that
    // address, which the plain text is.
    let address = (md.strip_prefix('<').and_then(|m| m.strip_suffix('>')))
        .is_some_and(|a| !a.contains(['<', '>', ' ']));
    (!md.is_empty() && !address).then_some(md)
}

/// The part of clipboard HTML that holds what was copied, without the document around it, where
/// it carries formatting: a heading, a list, a quote, code, a table, a rule, a link or a picture,
/// or text styled bold, italic, struck, underlined, sub- or superscript, by tag or inline style.
/// `None` for text alone, and for code copied from an editor, all of whose text keeps its
/// whitespace (`<pre>`, `white-space: pre`): its plain text is the better paste.
pub fn formatted(html: &str) -> Option<&str> {
    let body = copied(html);
    carries_formatting(&parse(body)).then_some(body)
}

/// A link's destination as written: literally where it reads back whole, otherwise between `<`
/// and `>` with those escaped, for an address with a space or a parenthesis left open or shut
/// once too often.
pub fn link_destination(url: &str) -> String {
    let mut depth = 0i32;
    let balanced = url.chars().all(|c| {
        depth += match c {
            '(' => 1,
            ')' => -1,
            _ => 0,
        };
        depth >= 0
    }) && depth == 0;
    match balanced && !url.contains([' ', '<', '>']) {
        true => url.to_string(),
        false => format!("<{}>", url.replace('<', "\\<").replace('>', "\\>")),
    }
}

/// How text looks, as far as Markdown can say it, and whether its whitespace is kept.
#[derive(Clone, Copy, Default, PartialEq)]
struct Look {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    sub: bool,
    sup: bool,
    pre: bool,
}

/// Each mark's opening and closing Markdown, in the order they nest, the outermost first.
const MARKS: [(&str, &str); 6] = [
    ("<u>", "</u>"),
    ("<sup>", "</sup>"),
    ("<sub>", "</sub>"),
    ("~~", "~~"),
    ("**", "**"),
    ("*", "*"),
];

impl Look {
    /// Which of [`MARKS`] this look has where `base` does not: a heading is bold already, and a
    /// link underlined.
    fn marks(self, base: Look) -> [bool; 6] {
        let mark = |on: bool, had: bool| on && !had;
        [
            mark(self.underline, base.underline),
            mark(self.sup, base.sup),
            mark(self.sub, base.sub),
            mark(self.strike, base.strike),
            mark(self.bold, base.bold),
            mark(self.italic, base.italic),
        ]
    }

    /// The look inside `tag`, an element of `parent`'s: its tag and inline style, a style
    /// winning over the tag as in a browser.
    fn of(tag: &Tag, attrs: &[(String, String)], parent: Look) -> Look {
        let mut marks = Marks {
            bold: parent.bold,
            italic: parent.italic,
            underline: parent.underline,
            ..Marks::default()
        };
        tag.apply(&mut marks);
        let mut look = Look {
            bold: marks.bold,
            italic: marks.italic,
            underline: marks.underline,
            ..parent
        };
        match tag.name.as_str() {
            "s" | "strike" | "del" => look.strike = true,
            "sub" => look.sub = true,
            "sup" => look.sup = true,
            "pre" => look.pre = true,
            _ => {}
        }
        for declaration in attr(attrs, "style").unwrap_or_default().split(';') {
            let Some((property, value)) = declaration.split_once(':') else {
                continue;
            };
            let value = value.trim().to_ascii_lowercase();
            match property.trim().to_ascii_lowercase().as_str() {
                "text-decoration" | "text-decoration-line" => {
                    look.strike |= value.contains("line-through");
                }
                "vertical-align" if value == "sub" => look.sub = true,
                "vertical-align" if value == "super" => look.sup = true,
                // Google Docs sets `pre` and then `pre-wrap` on every run of text: the last wins.
                "white-space" => look.pre = value == "pre",
                _ => {}
            }
        }
        look
    }
}

/// An element read off the HTML, with the look of the text inside it.
struct Element {
    /// Lowercased; empty for the root.
    name: String,
    attrs: Vec<(String, String)>,
    look: Look,
    /// Whether a block element is among its descendants.
    holds_block: bool,
    children: Vec<Node>,
}

enum Node {
    /// Text with its character references decoded and its whitespace as written.
    Text(String),
    Element(Element),
}

/// The elements whose content is never text: skipped whole.
const SKIPPED: [&str; 9] = [
    "head", "title", "style", "script", "template", "noscript", "svg", "math", "iframe",
];

/// The elements that have no end tag.
const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// The elements that start a block of their own.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "blockquote"
            | "pre"
            | "table"
            | "caption"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "hr"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "figure"
            | "figcaption"
            | "address"
            | "details"
            | "summary"
            | "center"
            | "form"
            | "fieldset"
            | "body"
            | "html"
    )
}

/// The value of attribute `name`.
fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    accent_drawio::model::attr(attrs, name)
}

/// The part of clipboard HTML that holds what was copied: between the `StartFragment` and
/// `EndFragment` comments Windows' clipboard format marks it with, else inside `<body>`, else all
/// of it. A document's doctype, head and title stay out, which a label would read as text.
fn copied(html: &str) -> &str {
    let (start, end) = ("<!--StartFragment-->", "<!--EndFragment-->");
    if let (Some(from), Some(to)) = (html.find(start), html.find(end))
        && from < to
    {
        return &html[from + start.len()..to];
    }
    let Some(open) = find_ignoring_case(html, "<body") else {
        return html;
    };
    let from = html[open..]
        .find('>')
        .map_or(html.len(), |end| open + end + 1);
    let to = find_ignoring_case(&html[from..], "</body").map_or(html.len(), |end| from + end);
    &html[from..to]
}

/// Where `needle`, ASCII, is in `s`, letters matching in either case.
fn find_ignoring_case(s: &str, needle: &str) -> Option<usize> {
    (s.as_bytes().windows(needle.len())).position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// The HTML as a tree under a nameless root. Comments, declarations and the skipped elements go;
/// an end tag closes the nearest element of its name and all inside it, and one matching none is
/// ignored, as a browser does; a list item, a cell or a row closes the one still open before it.
fn parse(html: &str) -> Element {
    let mut stack = vec![Element {
        name: String::new(),
        attrs: Vec::new(),
        look: Look::default(),
        holds_block: false,
        children: Vec::new(),
    }];
    let mut rest = html;
    while let Some((c, len)) = html_char(rest) {
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
        } else if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |end| &rest[end + 1..]);
        } else if let Some((tag, after)) = Tag::read(rest) {
            rest = after;
            let name = tag.name.as_str();
            if tag.close {
                if let Some(at) = stack.iter().rposition(|e| e.name == name)
                    && at > 0
                {
                    close(&mut stack, at);
                }
            } else if SKIPPED.contains(&name) && !tag.attrs.trim_end().ends_with('/') {
                // Left to be read as an end tag that matches nothing.
                let end = find_ignoring_case(rest, &format!("</{name}"));
                rest = &rest[end.unwrap_or(rest.len())..];
            } else {
                let (item, scope): (&[&str], &[&str]) = match name {
                    "li" => (&["li"], &["ul", "ol"]),
                    "dt" | "dd" => (&["dt", "dd"], &["dl"]),
                    "td" | "th" => (&["td", "th"], &["tr", "table"]),
                    "tr" => (&["tr"], &["table", "thead", "tbody", "tfoot"]),
                    "p" => (&["p"], &["div", "li", "td", "th", "blockquote", "table"]),
                    _ => (&[], &[]),
                };
                let open = stack.iter().rposition(|e| {
                    item.contains(&e.name.as_str()) || scope.contains(&e.name.as_str())
                });
                if let Some(at) =
                    open.filter(|&at| at > 0 && item.contains(&stack[at].name.as_str()))
                {
                    close(&mut stack, at);
                }
                let attrs = attributes(tag.attrs);
                let parent = stack.last().map_or(Look::default(), |e| e.look);
                let element = Element {
                    look: Look::of(&tag, &attrs, parent),
                    name: tag.name.clone(),
                    attrs,
                    holds_block: false,
                    children: Vec::new(),
                };
                let void = VOID.contains(&name) || tag.attrs.trim_end().ends_with('/');
                stack.push(element);
                if void {
                    let at = stack.len() - 1;
                    close(&mut stack, at);
                }
            }
        } else {
            let children = &mut stack.last_mut().expect("the root stays").children;
            match children.last_mut() {
                Some(Node::Text(text)) => text.push(c),
                _ => children.push(Node::Text(c.to_string())),
            }
            rest = &rest[len..];
        }
    }
    close(&mut stack, 1);
    stack.pop().expect("the root stays")
}

/// Close the open elements from `at` on, each into the one it is in.
fn close(stack: &mut Vec<Element>, at: usize) {
    while stack.len() > at.max(1) {
        let element = stack.pop().expect("longer than at");
        let parent = stack.last_mut().expect("the root stays");
        parent.holds_block |= element.holds_block || is_block(&element.name);
        parent.children.push(Node::Element(element));
    }
}

/// See [`formatted`].
fn carries_formatting(root: &Element) -> bool {
    /// Whether any text or picture is not in kept whitespace, and whether anything is formatted.
    #[derive(Default)]
    struct Found {
        free: bool,
        formatted: bool,
    }
    fn scan(element: &Element, found: &mut Found) {
        for node in &element.children {
            match node {
                Node::Text(text) if !text.trim().is_empty() => {
                    let look = element.look;
                    found.free |= !look.pre;
                    found.formatted |= look.marks(Look::default()).contains(&true);
                }
                Node::Text(_) => {}
                Node::Element(child) => {
                    let picture = child.name == "img" && image(child).is_some();
                    found.free |= picture && !child.look.pre;
                    found.formatted |= picture
                        || matches!(
                            child.name.as_str(),
                            "h1" | "h2"
                                | "h3"
                                | "h4"
                                | "h5"
                                | "h6"
                                | "ul"
                                | "ol"
                                | "table"
                                | "pre"
                                | "code"
                                | "kbd"
                                | "samp"
                                | "tt"
                                | "blockquote"
                                | "hr"
                        )
                        || (child.name == "a" && attr(&child.attrs, "href").is_some_and(linkable));
                    scan(child, found);
                }
            }
        }
    }
    let mut found = Found::default();
    scan(root, &mut found);
    found.free && found.formatted
}

/// Whether a link to `href` is worth keeping: an address with a scheme, not a script's.
fn linkable(href: &str) -> bool {
    let Some((scheme, _)) = href.trim().split_once(':') else {
        return false;
    };
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        && !["javascript", "vbscript", "data"].contains(&scheme.to_ascii_lowercase().as_str())
}

/// A picture as Markdown, where its address is on the web; one held in the HTML (`data:`) or in
/// a file the copying application made has none a note could keep.
fn image(img: &Element) -> Option<String> {
    let src = attr(&img.attrs, "src")?.trim();
    let web = ["http://", "https://"].iter().any(|scheme| {
        src.get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    });
    web.then(|| {
        let mut alt = String::new();
        escape(
            &mut alt,
            &collapse(attr(&img.attrs, "alt").unwrap_or_default()),
        );
        format!("![{}]({})", after_markup(&alt), link_destination(src))
    })
}

/// Whitespace as HTML shows it outside `<pre>`: each stretch of it one space.
fn collapse(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text inside `element` as written, `<br>` a newline, for `<pre>` and inline code.
fn raw_text(element: &Element, out: &mut String) {
    for node in &element.children {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Element(br) if br.name == "br" => out.push('\n'),
            Node::Element(child) => raw_text(child, out),
        }
    }
}

/// A piece of a paragraph, in reading order.
enum Piece {
    /// Text, its whitespace collapsed, to be escaped.
    Text(String, Look),
    /// Finished Markdown: a link, a picture, inline code.
    Markdown(String, Look),
    Break,
}

/// A finished block, and whether it is a list, which follows a list item's own text without the
/// blank line other blocks keep.
struct Block {
    md: String,
    list: bool,
}

/// Blocks as a note holds them, a blank line between two; `tight` writes a list straight under
/// the text before it, as a list item's nested list.
fn join(blocks: &[Block], tight: bool) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str(if tight && block.list { "\n" } else { "\n\n" });
        }
        out.push_str(&block.md);
    }
    out
}

/// Writes the tree as Markdown.
#[derive(Default)]
struct Writer {
    /// In a table's cell, where a line break is `<br>` and every block shares the cell's line.
    cell: bool,
    /// In a link's text, where another link can only be text.
    link: bool,
}

impl Writer {
    /// `nodes`, children of `parent`, as blocks: runs of inline content as paragraphs, each block
    /// element as what it is.
    fn blocks(&mut self, nodes: &[Node], parent: &Element) -> Vec<Block> {
        let mut out = Vec::new();
        let mut line = Vec::new();
        for node in nodes {
            match node {
                Node::Text(text) => push_text(&mut line, text, parent.look),
                Node::Element(e) if is_block(&e.name) || e.holds_block => {
                    self.paragraphs(&mut line, &mut out);
                    self.block(e, parent, &mut out);
                }
                Node::Element(e) => self.inline(e, &mut line),
            }
        }
        self.paragraphs(&mut line, &mut out);
        out
    }

    /// A block element, a child of `parent`.
    fn block(&mut self, e: &Element, parent: &Element, out: &mut Vec<Block>) {
        let paragraph = |md: String| Block { md, list: false };
        match e.name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" if !self.cell => {
                let mut line = Vec::new();
                self.contents(e, &mut line);
                let text = self.render(&line, e.look, " ");
                if !text.is_empty() {
                    let level = usize::from(e.name.as_bytes()[1] - b'0');
                    out.push(paragraph(format!("{} {text}", "#".repeat(level))));
                }
            }
            "ul" | "ol" | "li" if !self.cell => {
                let list = match e.name.as_str() {
                    // An item outside any list.
                    "li" => items(vec![self.blocks(&e.children, e)], None),
                    _ => self.list(e),
                };
                if !list.is_empty() {
                    out.push(Block {
                        md: list,
                        list: true,
                    });
                }
            }
            "blockquote" if !self.cell => {
                let inner = join(&self.blocks(&e.children, e), false);
                if !inner.is_empty() {
                    let quoted: Vec<String> = (inner.lines())
                        .map(|line| match line.is_empty() {
                            true => ">".to_string(),
                            false => format!("> {line}"),
                        })
                        .collect();
                    out.push(paragraph(quoted.join("\n")));
                }
            }
            "pre" => out.push(paragraph(self.code_block(e, parent))),
            "table" if !self.cell => out.extend(self.table(e)),
            "hr" if !self.cell => out.push(paragraph("---".into())),
            _ => out.extend(self.blocks(&e.children, e)),
        }
    }

    /// The items of `list`, each item's lines after its first under its text, as deep as its
    /// marker is wide. A list placed straight in another, as Google Docs and LibreOffice nest
    /// one, goes under the item before it.
    fn list(&mut self, list: &Element) -> String {
        let mut found = Vec::new();
        for node in &list.children {
            match node {
                Node::Element(li) if li.name == "li" => found.push(self.blocks(&li.children, li)),
                Node::Element(nested) if matches!(nested.name.as_str(), "ul" | "ol") => {
                    let block = Block {
                        md: self.list(nested),
                        list: true,
                    };
                    match found.last_mut() {
                        Some(item) => item.push(block),
                        None => found.push(vec![block]),
                    }
                }
                Node::Text(text) if text.trim().is_empty() => {}
                other => found.push(self.blocks(std::slice::from_ref(other), list)),
            }
        }
        let start = attr(&list.attrs, "start").and_then(|s| s.trim().parse().ok());
        items(found, (list.name == "ol").then_some(start.unwrap_or(1)))
    }

    /// `<pre>` as a fenced block, named for its language where a class says it (`language-rust`,
    /// GitHub's `highlight-source-rust` on the block around it), its fence longer than any run of
    /// backticks inside. In a table's cell, each of its lines as inline code.
    fn code_block(&mut self, pre: &Element, parent: &Element) -> String {
        let mut code = String::new();
        raw_text(pre, &mut code);
        let code = code.replace('\r', "");
        // HTML drops a newline straight after `<pre>`; the last line's ends nothing.
        let code = code.strip_prefix('\n').unwrap_or(&code).trim_end();
        if self.cell {
            let lines: Vec<String> = (code.lines())
                .map(|line| match line.trim().is_empty() {
                    true => String::new(),
                    false => code_span(line),
                })
                .collect();
            return lines.join("<br>");
        }
        let code_child = pre.children.iter().find_map(|node| match node {
            Node::Element(code) if code.name == "code" => Some(code),
            _ => None,
        });
        let classes = [Some(pre), code_child, Some(parent)]
            .into_iter()
            .flatten()
            .filter_map(|e| attr(&e.attrs, "class"));
        let lang = attr(&pre.attrs, "lang").or_else(|| {
            classes.flat_map(str::split_whitespace).find_map(|class| {
                ["language-", "lang-", "highlight-source-"]
                    .iter()
                    .find_map(|prefix| class.strip_prefix(prefix))
            })
        });
        let fence = "`".repeat(longest_run(code, '`').max(2) + 1);
        format!("{fence}{}\n{code}\n{fence}", lang.unwrap_or_default())
    }

    /// A table as a GFM pipe table, its first row the header, a cell's lines and blocks joined by
    /// `<br>` and its pipes escaped; a cell spanning columns is followed by empty ones, so the
    /// columns stay in line. A table of one cell is that cell's content.
    fn table(&mut self, table: &Element) -> Vec<Block> {
        let mut rows: Vec<Vec<&Element>> = Vec::new();
        let mut caption = Vec::new();
        collect_rows(table, &mut rows, &mut caption);
        if let [row] = rows.as_slice()
            && let [cell] = row.as_slice()
        {
            return self.blocks(&cell.children, cell);
        }
        let mut out = Vec::new();
        for caption in caption {
            out.extend(self.blocks(&caption.children, caption));
        }
        let mut written: Vec<Vec<String>> = Vec::new();
        for row in rows {
            let mut cells = Vec::new();
            for cell in row {
                self.cell = true;
                let blocks = self.blocks(&cell.children, cell);
                self.cell = false;
                let md: Vec<String> = (blocks.iter().enumerate())
                    .map(|(i, b)| match i {
                        0 => b.md.replace('\n', "<br>"),
                        _ => after_markup(&b.md.replace('\n', "<br>")),
                    })
                    .collect();
                cells.push(escape_pipes(&md.join("<br>")));
                let span: usize = attr(&cell.attrs, "colspan")
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(1);
                cells.extend(std::iter::repeat_n(String::new(), span.clamp(1, 100) - 1));
            }
            written.push(cells);
        }
        let width = written.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return out;
        }
        let line = |cells: &[String]| {
            let mut padded = cells.to_vec();
            padded.resize(width, String::new());
            format!("| {} |", padded.join(" | "))
        };
        let mut md = vec![line(&written[0]), line(&vec!["---".to_string(); width])];
        md.extend(written[1..].iter().map(|row| line(row)));
        out.push(Block {
            md: md.join("\n"),
            list: false,
        });
        out
    }

    /// The inline content of `e` added to `line`.
    fn inline(&mut self, e: &Element, line: &mut Vec<Piece>) {
        match e.name.as_str() {
            "br" => line.push(Piece::Break),
            "img" => match image(e) {
                Some(md) => line.push(Piece::Markdown(md, e.look)),
                None => push_text(line, attr(&e.attrs, "alt").unwrap_or_default(), e.look),
            },
            "code" | "kbd" | "samp" | "tt" => {
                let mut text = String::new();
                raw_text(e, &mut text);
                let text = collapse(&text);
                if !text.is_empty() {
                    line.push(Piece::Markdown(code_span(&text), e.look));
                }
            }
            "input"
                if attr(&e.attrs, "type").is_some_and(|t| t.eq_ignore_ascii_case("checkbox")) =>
            {
                let tick = match attr(&e.attrs, "checked") {
                    Some(_) => "[x] ",
                    None => "[ ] ",
                };
                line.push(Piece::Markdown(tick.into(), Look::default()));
            }
            "a" => {
                let href = attr(&e.attrs, "href")
                    .map(str::trim)
                    .filter(|h| linkable(h));
                let Some(href) = href.filter(|_| !self.link) else {
                    return self.contents(e, line);
                };
                let mut inner = Vec::new();
                self.link = true;
                self.contents(e, &mut inner);
                self.link = false;
                // Underlined is how a link looks, not a mark of its text.
                let base = Look {
                    underline: true,
                    ..e.look
                };
                let text = after_markup(&self.render(&inner, base, " "));
                let plain = match inner.as_slice() {
                    [Piece::Text(plain, _)] => Some(plain.trim()),
                    _ => None,
                };
                let md = match plain {
                    _ if text.is_empty() => return,
                    Some(plain) if plain == href && !href.contains([' ', '<', '>']) => {
                        format!("<{href}>")
                    }
                    _ => format!("[{text}]({})", link_destination(href)),
                };
                line.push(Piece::Markdown(md, e.look));
            }
            _ => self.contents(e, line),
        }
    }

    /// Everything inside `e` as inline content, a block in it included.
    fn contents(&mut self, e: &Element, line: &mut Vec<Piece>) {
        for node in &e.children {
            match node {
                Node::Text(text) => push_text(line, text, e.look),
                Node::Element(child) => self.inline(child, line),
            }
        }
    }

    /// `line` as paragraphs: each run between two line breaks or more is one, a single break in
    /// it a hard line break, `<br>` in a cell.
    fn paragraphs(&mut self, line: &mut Vec<Piece>, out: &mut Vec<Block>) {
        let pieces = std::mem::take(line);
        let breaks = if self.cell { "<br>" } else { "\\\n" };
        let mut rest = pieces.as_slice();
        while !rest.is_empty() {
            let end = (0..rest.len())
                .find(|&i| matches!(rest[i..], [Piece::Break, Piece::Break, ..]))
                .unwrap_or(rest.len());
            let md = self.render(&rest[..end], Look::default(), breaks);
            if !md.is_empty() {
                out.push(Block { md, list: false });
            }
            let skip = rest[end..]
                .iter()
                .take_while(|p| matches!(p, Piece::Break))
                .count();
            rest = &rest[end + skip..];
        }
    }

    /// Pieces as Markdown, their marks where they differ from `base`'s, a break written as
    /// `breaks`; trimmed, with no break at either end.
    fn render(&self, pieces: &[Piece], base: Look, breaks: &str) -> String {
        let mut out = String::new();
        let mut open = [false; 6];
        for piece in pieces {
            match piece {
                Piece::Break => {
                    mark(&mut out, &mut open, [false; 6]);
                    let kept = out.trim_end_matches(' ').len();
                    out.truncate(kept);
                    if !out.is_empty() && !out.ends_with(breaks) {
                        out.push_str(breaks);
                    }
                }
                Piece::Text(text, _) if text.trim().is_empty() => {
                    if !out.is_empty() && !out.ends_with([' ', '\n']) {
                        out.push(' ');
                    }
                }
                Piece::Text(text, look) | Piece::Markdown(text, look) => {
                    // A space before the text goes before the marks that open on it.
                    let text = match text.strip_prefix(' ') {
                        Some(rest) => {
                            if !out.is_empty() && !out.ends_with([' ', '\n']) {
                                out.push(' ');
                            }
                            rest
                        }
                        None => text,
                    };
                    mark(&mut out, &mut open, look.marks(base));
                    match piece {
                        Piece::Text(..) => escape(&mut out, text),
                        _ => out.push_str(text),
                    }
                }
            }
        }
        mark(&mut out, &mut open, [false; 6]);
        out.trim_end_matches(breaks).trim().to_string()
    }
}

/// List items as Markdown: `- `, or numbered from `start`, each item's blocks joined tight.
fn items(items: Vec<Vec<Block>>, start: Option<u64>) -> String {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let marker = match start {
            Some(start) => format!("{}. ", start + i as u64),
            None => "- ".to_string(),
        };
        let pad = " ".repeat(marker.len());
        let body = join(item, true);
        let mut lines = body.lines();
        let mut md = format!("{marker}{}", lines.next().unwrap_or_default());
        for line in lines {
            md.push('\n');
            if !line.is_empty() {
                md.push_str(&pad);
                md.push_str(line);
            }
        }
        out.push(md.trim_end().to_string());
    }
    out.join("\n")
}

/// Close the marks `open` has that `want` has not, from the innermost, and open the ones it
/// lacks. A closing mark goes before the spaces it follows, so that it still closes.
// ponytail: a mark between punctuation and a letter (`**Note:**text`) neither opens nor closes
// in CommonMark and shows its stars; CommonMark's flanking rules decide where `<b>` would do.
fn mark(out: &mut String, open: &mut [bool; 6], want: [bool; 6]) {
    // Everything inside a mark that closes closes with it, and opens again after it.
    if let Some(first) = (0..6).find(|&i| open[i] && !want[i]) {
        let kept = out.trim_end_matches(' ').len();
        let spaces = out.split_off(kept);
        for i in (first..6).rev() {
            if open[i] {
                out.push_str(MARKS[i].1);
                open[i] = false;
            }
        }
        out.push_str(&spaces);
    }
    for i in 0..6 {
        if want[i] && !open[i] {
            out.push_str(MARKS[i].0);
            open[i] = true;
        }
    }
}

/// Add HTML text to `line`, its whitespace collapsed as a browser shows it: no space at the start
/// of a line, nor two in a row.
fn push_text(line: &mut Vec<Piece>, text: &str, look: Look) {
    let at_start = match line.last() {
        None | Some(Piece::Break) => true,
        Some(Piece::Text(last, _)) => last.ends_with(' '),
        Some(Piece::Markdown(..)) => false,
    };
    let mut collapsed = String::with_capacity(text.len());
    let mut space = at_start;
    for c in text.chars() {
        // A non-breaking space reads as one, as the text a browser copies has it.
        if c.is_ascii_whitespace() || c == '\u{a0}' {
            if !space {
                collapsed.push(' ');
            }
            space = true;
        } else {
            collapsed.push(c);
            space = false;
        }
    }
    if collapsed.is_empty() {
        return;
    }
    match line.last_mut() {
        Some(Piece::Text(last, last_look)) if *last_look == look => last.push_str(&collapsed),
        _ => line.push(Piece::Text(collapsed, look)),
    }
}

/// The rows of `table`, in its head, bodies and foot or straight in it, and its caption.
fn collect_rows<'a>(
    table: &'a Element,
    rows: &mut Vec<Vec<&'a Element>>,
    caption: &mut Vec<&'a Element>,
) {
    for node in &table.children {
        let Node::Element(e) = node else { continue };
        match e.name.as_str() {
            "tr" => rows.push(
                (e.children.iter())
                    .filter_map(|n| match n {
                        Node::Element(cell) if matches!(cell.name.as_str(), "td" | "th") => {
                            Some(cell)
                        }
                        _ => None,
                    })
                    .collect(),
            ),
            "thead" | "tbody" | "tfoot" => collect_rows(e, rows, caption),
            "caption" => caption.push(e),
            _ => {}
        }
    }
}

/// `code` as inline code, between backticks one more than its longest run, with a space inside
/// each where it starts or ends with one, or with a space at both ends, which would be taken off.
fn code_span(code: &str) -> String {
    let ticks = "`".repeat(longest_run(code, '`') + 1);
    let spaced = code.starts_with(' ') && code.ends_with(' ') && !code.trim().is_empty();
    let pad = match code.starts_with('`') || code.ends_with('`') || spaced {
        true => " ",
        false => "",
    };
    format!("{ticks}{pad}{code}{pad}{ticks}")
}

/// Markdown rendered on its own and put straight after markup that is no space (a link's `[`, a
/// picture's `![`, a cell's `<br>`), where a `$` it starts with could close a formula.
fn after_markup(md: &str) -> String {
    match md.starts_with('$') {
        true => format!("\\{md}"),
        false => md.to_string(),
    }
}

/// The most `c` in a row in `s`.
fn longest_run(s: &str, c: char) -> usize {
    let (mut longest, mut run) = (0, 0);
    for x in s.chars() {
        run = if x == c { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// A cell's Markdown with its pipes escaped, a table splitting its row at every one that is not,
/// inside code too.
fn escape_pipes(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut slashes = 0;
    for c in md.chars() {
        if c == '|' && slashes % 2 == 0 {
            out.push('\\');
        }
        slashes = if c == '\\' { slashes + 1 } else { 0 };
        out.push(c);
    }
    out
}

/// Add `text` to `out` escaped where it would read as markup: by itself (`` ` ``, `[`, `]`), by
/// its neighbours (`*`, `_` and `~` that could open or close emphasis or a strikethrough, a `$`
/// that could close a formula, `<` before a tag, `&` before a reference, `\` before punctuation),
/// or at the start of a line (a heading's `#`, a quote's `>`, a list's marker, a rule).
fn escape(out: &mut String, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    let mut line_start = out.is_empty() || out.ends_with('\n');
    // `1.` or `1)` and a space start a numbered list.
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    let number = line_start
        && (1..=9).contains(&digits)
        && matches!(chars.get(digits), Some('.' | ')'))
        && chars.get(digits + 1).is_none_or(|c| c.is_whitespace());
    for (i, &c) in chars.iter().enumerate() {
        if number && i == digits {
            out.push('\\');
        }
        let prev = match i {
            0 => out.chars().next_back(),
            _ => Some(chars[i - 1]),
        };
        let next = chars.get(i + 1).copied();
        // At the start of the text there is nothing before, and past its end there may be
        // anything: what follows is judged as the worst it could be.
        let space_before = prev.is_none_or(char::is_whitespace);
        let space_after = next.is_some_and(char::is_whitespace);
        let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        let escaped = match c {
            '`' | '[' | ']' => true,
            '\\' => next.is_none_or(|n| n.is_ascii_punctuation()),
            '*' | '~' => !(space_before && space_after),
            '_' => !(space_before && space_after) && !(word(prev) && word(next)),
            // pulldown-cmark closes a formula at a `$` after anything but a space, so with every
            // such `$` escaped none opens either, and a price (`$5 and $10`) keeps its own.
            '$' => !space_before,
            '<' => next.is_none_or(|n| n.is_ascii_alphabetic() || "/!?".contains(n)),
            '&' => next.is_some_and(|n| n.is_ascii_alphanumeric() || n == '#'),
            '#' if line_start => next.is_none_or(|n| n == ' ' || n == '#'),
            '>' if line_start => true,
            '-' | '+' | '=' if line_start => next.is_none_or(|n| n.is_whitespace() || n == c),
            _ => false,
        };
        if escaped {
            out.push('\\');
        }
        out.push(c);
        line_start = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::to_html;

    /// The text an HTML page shows, without its whitespace: what a paste keeps, however it is
    /// marked up.
    fn shown(html: &str) -> String {
        fn walk(e: &Element, out: &mut String) {
            for node in &e.children {
                match node {
                    Node::Text(text) => out.extend(text.chars().filter(|c| !c.is_whitespace())),
                    Node::Element(child) => walk(child, out),
                }
            }
        }
        let mut out = String::new();
        walk(&parse(html), &mut out);
        out
    }

    /// `html` converts to `md`, which a note shows as the text `html` showed.
    fn converts(html: &str, md: &str) {
        let got = from_html(html).unwrap_or_else(|| panic!("no Markdown for {html}"));
        assert_eq!(got, md, "{html}");
        assert_eq!(shown(&to_html(&got)), shown(html), "{got}");
    }

    /// Captured from Firefox 153 under Xvfb (Playwright 1.62), all of a test page selected.
    const FIREFOX_PAGE: &str = r##"<meta http-equiv="content-type" content="text/html; charset=utf-8">
<h1>Release notes</h1>
<p>Plain, <b>bold</b>, <em>italic</em>, <strong><em>both</em></strong>, <s>struck</s>, <u>under</u>, H<sub>2</sub>O, x<sup>2</sup>, <code>a_b*c</code> and a <a href="https://example.org/a_(b)?q=1&amp;r=2">link [1]</a>.</p>
<p>Prices: $5 and $10, a #tag, 2 * 3 * 4, snake_case_name, &lt;script&gt; and [[not a wikilink]] [^1].<br>Second line after a break.</p>
<h2 id="lists">Lists</h2>
<ul><li>one</li><li>two<ul><li>nested <i>deep</i></li></ul></li><li><p>para item</p></li></ul>
<ol start="3"><li>three</li><li>four</li></ol>
<blockquote><p>Quoted <b>text</b>.</p><p>Second quoted paragraph.</p></blockquote>
<pre><code class="language-rust">fn main() {
    println!("```");
}
</code></pre>
<table><thead><tr><th>Name</th><th>Value | pipe</th></tr></thead>
<tbody><tr><td>alpha</td><td>1<br>2</td></tr><tr><td><b>beta</b></td><td><a href="https://example.org/b">b</a></td></tr></tbody></table>
<p><img src="https://example.org/cat.png" alt="A cat"> <img src="data:image/png;base64,iVBORw0KGgo=" alt="inline"></p>
<hr>
<p>1. Not a list</p>
<p>The end.</p>
"##;

    const PAGE_MD: &str = r##"# Release notes

Plain, **bold**, *italic*, ***both***, ~~struck~~, <u>under</u>, H<sub>2</sub>O, x<sup>2</sup>, `a_b*c` and a [link \[1\]](https://example.org/a_(b)?q=1&r=2).

Prices: $5 and $10, a #tag, 2 * 3 * 4, snake_case_name, \<script> and \[\[not a wikilink\]\] \[^1\].\
Second line after a break.

## Lists

- one
- two
  - nested *deep*
- para item

3. three
4. four

> Quoted **text**.
>
> Second quoted paragraph.

````rust
fn main() {
    println!("```");
}
````

| Name | Value \| pipe |
| --- | --- |
| alpha | 1<br>2 |
| **beta** | [b](https://example.org/b) |

![A cat](https://example.org/cat.png) inline

---

1\. Not a list

The end."##;

    #[test]
    fn a_page_converts_block_by_block() {
        assert_eq!(from_html(FIREFOX_PAGE).as_deref(), Some(PAGE_MD));
        // Read back, it shows the page's text, the `data:` picture's alt standing in for it.
        let md = to_html(PAGE_MD);
        let data = r#"<img src="data:image/png;base64,iVBORw0KGgo=" alt="inline">"#;
        assert_eq!(shown(&md), shown(&FIREFOX_PAGE.replace(data, "inline")));
        assert!(md.contains("<hr"), "{md}");
    }

    /// Chrome for Testing 151 copies inline styles on every element, the computed
    /// `font-weight: 400` of the text around a `<b>` included.
    #[test]
    fn chromium_styles_mark_what_they_set() {
        let partial = r##"<span style="color: rgb(0, 0, 0); font-family: &quot;Times New Roman&quot;; font-size: medium; font-style: normal; font-variant-ligatures: normal; font-variant-caps: normal; font-weight: 400; letter-spacing: normal; orphans: 2; text-align: start; text-indent: 0px; text-transform: none; widows: 2; word-spacing: 0px; -webkit-text-stroke-width: 0px; white-space: normal; text-decoration-thickness: initial; text-decoration-style: initial; text-decoration-color: initial; display: inline !important; float: none;">me<span> </span></span><b id="b1" style="color: rgb(0, 0, 0); font-family: &quot;Times New Roman&quot;; font-size: medium; font-style: normal; font-variant-ligatures: normal; font-variant-caps: normal; letter-spacing: normal; orphans: 2; text-align: start; text-indent: 0px; text-transform: none; widows: 2; word-spacing: 0px; -webkit-text-stroke-width: 0px; white-space: normal; text-decoration-thickness: initial; text-decoration-style: initial; text-decoration-color: initial;">emphasised</b><span style="color: rgb(0, 0, 0); font-family: &quot;Times New Roman&quot;; font-size: medium; font-style: normal; font-variant-ligatures: normal; font-variant-caps: normal; font-weight: 400; letter-spacing: normal; orphans: 2; text-align: start; text-indent: 0px; text-transform: none; widows: 2; word-spacing: 0px; -webkit-text-stroke-width: 0px; white-space: normal; text-decoration-thickness: initial; text-decoration-style: initial; text-decoration-color: initial; display: inline !important; float: none;"><span> </span>wo</span>"##;
        converts(partial, "me **emphasised** wo");
        let link = r##"<span style="color: rgb(0, 0, 0); font-weight: 400; white-space: normal; display: inline !important; float: none;">Go to<span> </span></span><a id="a1" href="https://example.org/docs" style="font-family: &quot;Times New Roman&quot;; font-size: medium; font-style: normal; font-weight: 400; white-space: normal;">the docs</a><span style="color: rgb(0, 0, 0); font-weight: 400; white-space: normal; display: inline !important; float: none;"><span> </span>now.</span>"##;
        converts(link, "Go to [the docs](https://example.org/docs) now.");
        // Firefox's copy of the same.
        converts(
            r##"<meta http-equiv="content-type" content="text/html; charset=utf-8">me <b id="b1">emphasised</b> wo"##,
            "me **emphasised** wo",
        );
    }

    /// Wikipedia as Firefox copies it: links made absolute, a citation a superscript link.
    #[test]
    fn wikipedia_keeps_its_links_and_citations() {
        let para = r##"<meta http-equiv="content-type" content="text/html; charset=utf-8"><b id="mwMA">Markdown</b><sup about="#mwt53" class="mw-ref reference" id="cite_ref-philosophy_9-0" rel="dc:references" typeof="mw:Extension/ref" data-mw="{&quot;name&quot;:&quot;ref&quot;,&quot;attrs&quot;:{&quot;name&quot;:&quot;philosophy&quot;}}"><a href="https://en.wikipedia.org/wiki/Markdown#cite_note-philosophy-9" id="mwMQ"><span class="mw-reflink-text" id="mwMg"><span class="cite-bracket" id="mwMw">[</span>9<span class="cite-bracket" id="mwNA">]</span></span></a></sup> is a <a rel="mw:WikiLink" href="https://en.wikipedia.org/wiki/Lightweight_markup_language" title="Lightweight markup language" id="mwNQ">lightweight markup language</a> for creating <a rel="mw:WikiLink" href="https://en.wikipedia.org/wiki/Formatted_text" title="Formatted text" id="mwNg">formatted text</a>."##;
        converts(
            para,
            "**Markdown**<sup>[\\[9\\]](https://en.wikipedia.org/wiki/Markdown#cite_note-philosophy-9)</sup> is a [lightweight markup language](https://en.wikipedia.org/wiki/Lightweight_markup_language) for creating [formatted text](https://en.wikipedia.org/wiki/Formatted_text).",
        );
        let infobox = r##"<meta http-equiv="content-type" content="text/html; charset=utf-8"><table class="infobox" about="#mwt7" id="mwCg"><tbody><tr><th colspan="2" class="infobox-above" style="padding-bottom: 0.15em;background-color:#e0e0e0;color:inherit;">Markdown</th></tr><tr><td colspan="2" class="infobox-image"><span class="skin-invert" typeof="mw:File"><a href="https://en.wikipedia.org/wiki/File:Markdown-mark.svg" class="mw-file-description"><img resource="https://en.wikipedia.org/wiki/File:Markdown-mark.svg" src="https://thumb.wikimedia.org/wikipedia/commons/thumb/4/48/Markdown-mark.svg/120px-Markdown-mark.svg.png?utm_source=en.wikipedia.org&amp;utm_campaign=parser&amp;utm_content=thumbnail" decoding="async" loading="lazy" height="49" width="80" class="mw-file-element"></a></span></td></tr><tr><th scope="row" class="infobox-label" style="line-height: 1.2; padding-right: 0.65em;"><a rel="mw:WikiLink" href="https://en.wikipedia.org/wiki/Filename_extension" title="Filename extension">Filename extensions</a></th><td class="infobox-data" style="line-height: 1.35;"><code>.md</code>, <code>.markdown</code></td></tr><tr><th scope="row" class="infobox-label" style="line-height: 1.2; padding-right: 0.65em;">Developed<span typeof="mw:Entity"> </span>by</th><td class="infobox-data" style="line-height: 1.35;"><div class="plainlist ">
<ul><li><a rel="mw:WikiLink" href="https://en.wikipedia.org/wiki/John_Gruber" title="John Gruber">John Gruber</a></li></ul>

</div></td></tr><tr><th scope="row" class="infobox-label" style="line-height: 1.2; padding-right: 0.65em;">Website</th><td class="infobox-data" style="line-height: 1.35;"><span class="url"><a rel="mw:ExtLink nofollow" href="https://daringfireball.net/projects/markdown/" class="external text">daringfireball<wbr>.net<wbr>/projects<wbr>/markdown<wbr>/</a></span></td></tr></tbody></table>"##;
        converts(
            infobox,
            "| Markdown |  |
| --- | --- |
| [![](https://thumb.wikimedia.org/wikipedia/commons/thumb/4/48/Markdown-mark.svg/120px-Markdown-mark.svg.png?utm_source=en.wikipedia.org&utm_campaign=parser&utm_content=thumbnail)](https://en.wikipedia.org/wiki/File:Markdown-mark.svg) |  |
| [Filename extensions](https://en.wikipedia.org/wiki/Filename_extension) | `.md`, `.markdown` |
| Developed by | [John Gruber](https://en.wikipedia.org/wiki/John_Gruber) |
| Website | [daringfireball.net/projects/markdown/](https://daringfireball.net/projects/markdown/) |",
        );
    }

    /// A GitHub README as Firefox copies it: a heading's anchor (an empty link), badges, code
    /// blocks with their copy buttons, a table.
    #[test]
    fn a_github_readme_reads_as_its_markdown() {
        let readme = r##"<meta http-equiv="content-type" content="text/html; charset=utf-8"><div class="markdown-heading" dir="auto"><h2 tabindex="-1" class="heading-element" dir="auto">ripgrep (rg)</h2><a id="user-content-ripgrep-rg" class="anchor" aria-label="Permalink: ripgrep (rg)" href="https://github.com/BurntSushi/ripgrep#ripgrep-rg"></a></div>
<p dir="auto"><a href="https://github.com/BurntSushi/ripgrep/actions"><img src="https://github.com/BurntSushi/ripgrep/workflows/ci/badge.svg" alt="Build status" style="max-width: 100%;"></a>
<a href="https://crates.io/crates/ripgrep" rel="nofollow"><img src="https://camo.githubusercontent.com/2fd0b64d/68747470" alt="Crates.io" style="max-width: 100%;"></a></p>
<p dir="auto">If you're a <strong>macOS Homebrew</strong> or a <strong>Linuxbrew</strong> user, then you can install
ripgrep from homebrew-core:</p>
<div class="snippet-clipboard-content notranslate position-relative overflow-auto"><pre class="notranslate"><code>$ brew install ripgrep
</code></pre><div class="zeroclipboard-container">
    
      
    

      
    
  </div></div>
<table>
<thead>
<tr>
<th>Tool</th>
<th>Command</th>
<th>Time</th>
</tr>
</thead>
<tbody>
<tr>
<td>ripgrep (Unicode)</td>
<td><code>rg -n -w '[A-Z]+_SUSPEND'</code></td>
<td><strong>0.082s</strong> (1.00x)</td>
</tr>
<tr>
<td><a href="https://github.com/p-ranav/hypergrep">hypergrep</a></td>
<td><code>hgrep -n -w '[A-Z]+_SUSPEND'</code></td>
<td>0.167s (2.04x)</td>
</tr>
</tbody>
</table>
<ul dir="auto">
<li><a href="https://github.com/BurntSushi/ripgrep#installation">Installation</a></li>
<li><a href="https://github.com/BurntSushi/ripgrep/blob/master/GUIDE.md">User Guide</a></li>
</ul>"##;
        converts(
            readme,
            "## ripgrep (rg)

[![Build status](https://github.com/BurntSushi/ripgrep/workflows/ci/badge.svg)](https://github.com/BurntSushi/ripgrep/actions) [![Crates.io](https://camo.githubusercontent.com/2fd0b64d/68747470)](https://crates.io/crates/ripgrep)

If you're a **macOS Homebrew** or a **Linuxbrew** user, then you can install ripgrep from homebrew-core:

```
$ brew install ripgrep
```

| Tool | Command | Time |
| --- | --- | --- |
| ripgrep (Unicode) | `rg -n -w '[A-Z]+_SUSPEND'` | **0.082s** (1.00x) |
| [hypergrep](https://github.com/p-ranav/hypergrep) | `hgrep -n -w '[A-Z]+_SUSPEND'` | 0.167s (2.04x) |

- [Installation](https://github.com/BurntSushi/ripgrep#installation)
- [User Guide](https://github.com/BurntSushi/ripgrep/blob/master/GUIDE.md)",
        );
    }

    /// Google Docs wraps its copy in `<b style="font-weight:normal">` and styles every run of text
    /// inline, `white-space:pre-wrap` included; it nests a list straight in its parent list.
    /// Written from its known format (no Google account here).
    #[test]
    fn google_docs_styles_are_read_not_its_wrapper() {
        let run = |style: &str, text: &str| {
            format!(
                r##"<span style="font-size:11pt;font-family:Arial,sans-serif;color:#000000;background-color:transparent;{style}font-variant:normal;vertical-align:baseline;white-space:pre;white-space:pre-wrap;">{text}</span>"##
            )
        };
        let plain = "font-weight:400;font-style:normal;text-decoration:none;";
        let p = |runs: String| {
            format!(
                r##"<p dir="ltr" style="line-height:1.38;margin-top:0pt;margin-bottom:0pt;">{runs}</p>"##
            )
        };
        let item = |level: u8, text: &str| {
            format!(
                r##"<li dir="ltr" style="list-style-type:disc;font-size:11pt;font-family:Arial,sans-serif;" aria-level="{level}"><p dir="ltr" style="line-height:1.38;margin-top:0pt;margin-bottom:0pt;" role="presentation">{}</p></li>"##,
                run(plain, text)
            )
        };
        let doc = format!(
            r##"<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-6a4e1f3c-7fff-3b2a-9c1d-0e5f8a7b6c5d"><h1 dir="ltr" style="line-height:1.38;margin-top:20pt;margin-bottom:6pt;">{}</h1>{}<br><ul style="margin-top:0;margin-bottom:0;padding-inline-start:48px;">{}<ul style="margin-top:0;margin-bottom:0;padding-inline-start:48px;">{}</ul></ul><br>{}</b>"##,
            run(
                "font-size:20pt;font-weight:400;font-style:normal;text-decoration:none;",
                "Title"
            ),
            p(run(
                "font-weight:700;font-style:normal;text-decoration:none;",
                "Bold"
            ) + &run(plain, ", ")
                + &run(
                    "font-weight:400;font-style:italic;text-decoration:none;",
                    "italic"
                )
                + &run(plain, ", ")
                + &run(
                    "font-weight:400;font-style:normal;text-decoration:line-through;",
                    "struck"
                )
                + &run(plain, ", E=mc")
                + &run(
                    "font-size:0.6em;font-weight:400;font-style:normal;text-decoration:none;vertical-align:super;",
                    "2"
                )
                + &run(plain, " and ")
                + r##"<a href="https://example.org/" style="text-decoration:none;">"##
                + &run(
                    "color:#1155cc;font-weight:400;font-style:normal;text-decoration:underline;-webkit-text-decoration-skip:none;text-decoration-skip-ink:none;",
                    "a link"
                )
                + "</a>"),
            item(1, "First"),
            item(2, "Nested"),
            p(run(plain, "After")),
        );
        converts(
            &doc,
            "# Title

**Bold**, *italic*, ~~struck~~, E=mc<sup>2</sup> and [a link](https://example.org/)

- First
  - Nested

After",
        );
        // One word copied is one run in the wrapper: still bold.
        let word = format!(
            r##"<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-6a4e1f3c-7fff-3b2a-9c1d-0e5f8a7b6c5d">{}</b>"##,
            run(
                "font-weight:700;font-style:normal;text-decoration:none;",
                "Bold"
            )
        );
        converts(&word, "**Bold**");
        // Plain text from it is plain.
        let none = format!(
            r##"<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-6a4e1f3c-7fff-3b2a-9c1d-0e5f8a7b6c5d">{}</b>"##,
            run(plain, "Plain words")
        );
        assert_eq!(from_html(&none), None);
    }

    /// LibreOffice Writer and Calc copy a whole document, its doctype, head and title included.
    /// Written from their known format (no LibreOffice on the build host).
    #[test]
    fn libreoffice_documents_convert_without_their_head() {
        let writer = r##"<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.0 Transitional//EN">
<html><head><meta http-equiv="content-type" content="text/html; charset=utf-8"/><title>Report</title><meta name="generator" content="LibreOffice 25.2.3.2 (Linux)"/><style type="text/css">@page { size: 21cm 29.7cm; margin: 2cm }
		p { line-height: 115%; margin-bottom: 0.25cm; background: transparent }
		h1.western { font-family: "Liberation Sans", sans-serif; font-size: 18pt }</style></head><body lang="en-US" link="#000080" vlink="#800000" dir="ltr"><h1 class="western">Heading</h1>
<p style="line-height: 100%; margin-bottom: 0cm">Text with <b>bold</b>, <i>italic</i> and <u>underlined</u> words.</p>
<ul>
	<li><p style="line-height: 100%; margin-bottom: 0cm">One</p>
	<ul>
		<li><p style="line-height: 100%; margin-bottom: 0cm">Two</p></li>
	</ul>
</ul>
<p style="line-height: 100%; margin-bottom: 0cm"><img src="file:///tmp/lu12345abc.tmp/lu12345abc_html_6f1e2d3c4b5a6978.png" name="Image1" align="bottom" width="96" height="64" border="0"/>
</p>
<p style="line-height: 100%; margin-bottom: 0cm"><br/>

</p>
</body></html>"##;
        assert_eq!(
            from_html(writer).as_deref(),
            Some(
                "# Heading\n\nText with **bold**, *italic* and <u>underlined</u> words.\n\n- One\n  - Two"
            )
        );
        let calc = r##"<!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.0 Transitional//EN">
<html><head><meta http-equiv="content-type" content="text/html; charset=utf-8"/><title></title><style type="text/css">body,div,table,thead,tbody,tfoot,tr,th,td,p { font-family:"Liberation Sans"; font-size:x-small }</style></head>
<body>
<table cellspacing="0" border="0">
	<colgroup width="85"></colgroup>
	<colgroup span="2" width="85"></colgroup>
	<tr>
		<td height="17" align="left">Name</td>
		<td align="left">Count</td>
		<td align="left">Note</td>
	</tr>
	<tr>
		<td height="17" align="left">apples</td>
		<td align="right" sdval="3" sdnum="1033;">3</td>
		<td align="left"><b>ripe</b></td>
	</tr>
</table>
</body>
</html>"##;
        converts(
            calc,
            "| Name | Count | Note |\n| --- | --- | --- |\n| apples | 3 | **ripe** |",
        );
        // One cell is its text.
        let cell = r##"<html><head><title></title></head><body><table cellspacing="0" border="0"><colgroup width="85"></colgroup><tr><td height="17" align="left"><b>alone</b></td></tr></table></body></html>"##;
        converts(cell, "**alone**");
    }

    /// Code copied from an editor or a code block keeps its whitespace in HTML: its plain text is
    /// the paste, however its keywords are styled.
    #[test]
    fn code_from_an_editor_is_no_formatting() {
        // VS Code (written from its known format).
        let vscode = r##"<meta charset='utf-8'><div style="color: #cccccc;background-color: #1f1f1f;font-family: 'Droid Sans Mono', 'monospace', monospace;font-weight: normal;font-size: 14px;line-height: 19px;white-space: pre;"><div><span style="color: #569cd6;font-weight: bold;">fn</span><span style="color: #cccccc;"> </span><span style="color: #dcdcaa;">main</span><span style="color: #cccccc;">() {}</span></div></div>"##;
        // JetBrains (written from its known format).
        let jetbrains = r##"<html><head><style type="text/css">.s0 { color: #cf8e6d; font-weight: bold;}</style></head><body><pre style="background-color:#1e1f22;color:#bcbec4;font-family:'JetBrains Mono',monospace;font-size:9.8pt;"><span class="s0">fn </span>main() {}</pre></body></html>"##;
        // Chromium and Firefox, a `<pre>` selected inside (captured).
        let chromium = r##"<pre id="codeonly" style="color: rgb(0, 0, 0); font-style: normal; font-variant-ligatures: normal; font-variant-caps: normal; font-weight: 400; letter-spacing: normal; orphans: 2; text-align: start; text-indent: 0px; text-transform: none; widows: 2; word-spacing: 0px; -webkit-text-stroke-width: 0px; text-decoration-thickness: initial; text-decoration-style: initial; text-decoration-color: initial;">let x = 1;
    indented();</pre>"##;
        let firefox = "<meta http-equiv=\"content-type\" content=\"text/html; charset=utf-8\"><pre id=\"codeonly\">let x = 1;\n    indented();</pre>";
        for html in [vscode, jetbrains, chromium, firefox] {
            assert_eq!(from_html(html), None, "{html}");
            assert_eq!(formatted(html), None, "{html}");
        }
        // Plain prose has nothing to convert either.
        assert_eq!(from_html("<p>One</p><p>Two <span>words</span></p>"), None);
    }

    /// Text is escaped where it would read as markup in a note, and only there.
    #[test]
    fn text_is_escaped_where_it_would_be_markup() {
        for (html, md) in [
            ("<b>b</b> $5 and $10", "**b** $5 and $10"),
            ("<b>b</b> $x$ and 10$", r"**b** $x\$ and 10\$"),
            ("<b>b</b> $5-$10, a$5 b$6", r"**b** $5-\$10, a\$5 b\$6"),
            (
                "<b>b</b> was $20, now <a href='https://a.org'>$15 today</a>",
                r"**b** was $20, now [\$15 today](https://a.org)",
            ),
            (
                "$5 <img src='https://a.org/x.png' alt='$4 off'>",
                r"$5 ![\$4 off](https://a.org/x.png)",
            ),
            (
                "<table><tr><th>a</th><th>b</th></tr><tr><td><div>$5</div><div>$6</div></td><td>x</td></tr></table>",
                "| a | b |\n| --- | --- |\n| $5<br>\\$6 | x |",
            ),
            (
                "<b>b</b> [[x]] #tag [^1] ![a]",
                r"**b** \[\[x\]\] #tag \[^1\] !\[a\]",
            ),
            (
                "<b>b</b> &lt;script&gt; &amp;copy; a &lt; b",
                r"**b** \<script> \&copy; a < b",
            ),
            (
                "<b>b</b> *x* _y_ ~z~ `c` a\\b \\*",
                r"**b** \*x\* \_y\_ \~z\~ \`c\` a\b \\\*",
            ),
            (
                "<p># a</p><p>&gt; b</p><p>- c</p><p>+ d</p><p>2) e</p><p>===</p><hr>",
                "\\# a\n\n\\> b\n\n\\- c\n\n\\+ d\n\n2\\) e\n\n\\===\n\n---",
            ),
            ("<hr><p>after</p><hr>", "---\n\nafter\n\n---"),
        ] {
            converts(html, md);
        }
    }

    /// Emphasis keeps the spaces at its edges outside its markers, where they still close it.
    #[test]
    fn emphasis_hugs_its_text() {
        converts(
            "<p>a<b> bold </b>b <i>it</i>, <b>x<i>y</i></b></p>",
            "a **bold** b *it*, **x*y***",
        );
        converts("<p><b>a<br>b</b></p>", "**a**\\\n**b**");
        converts("<p><i>a</i><br><br>b<br></p>", "*a*\n\nb");
        converts("<h2><b>Bold</b> <i>title</i></h2>", "## Bold *title*");
    }

    #[test]
    fn links_and_pictures() {
        for (html, md) in [
            (
                "see <a href='https://a.org/x'>https://a.org/x</a>",
                "see <https://a.org/x>",
            ),
            (
                "<a href='https://a.org/a b'>sp</a>",
                "[sp](<https://a.org/a b>)",
            ),
            ("<a href='javascript:alert(1)'>js</a> <b>b</b>", "js **b**"),
            ("<a href='/relative'>rel</a> <b>b</b>", "rel **b**"),
            (
                "<a href='https://a.org'><b>bold</b> <code>c</code></a>",
                "[**bold** `c`](https://a.org)",
            ),
            (
                "<b><a href='https://a.org'>all</a></b>",
                "**[all](https://a.org)**",
            ),
            (
                "<img src='file:///tmp/x.png' alt='kept'> <b>b</b>",
                "kept **b**",
            ),
            ("<code>a`b</code>", "``a`b``"),
            ("<code>`a</code>", "`` `a ``"),
            (
                "<ul><li><input type=checkbox checked disabled> done</li><li><input type=checkbox> open</li></ul>",
                "- [x] done\n- [ ] open",
            ),
        ] {
            assert_eq!(from_html(html).as_deref(), Some(md), "{html}");
        }
    }

    /// What a browser's Copy Link puts beside the address: the address is the paste.
    #[test]
    fn a_link_alone_showing_its_address_is_that_address() {
        let link = r#"<meta charset="utf-8"><a href="https://a.org/x">https://a.org/x</a>"#;
        assert_eq!(from_html(link), None);
        assert_eq!(
            from_html(r#"<a href="https://a.org/x">the page</a>"#).as_deref(),
            Some("[the page](https://a.org/x)")
        );
    }

    #[test]
    fn the_copied_part_leaves_the_document_out() {
        let marked = "<html><head><title>T</title></head><body>x<!--StartFragment--><b>in</b><!--EndFragment-->y</body></html>";
        assert_eq!(formatted(marked), Some("<b>in</b>"));
        assert_eq!(from_html(marked).as_deref(), Some("**in**"));
        let body = "<!DOCTYPE html><html><head><title>T</title></head><BODY class=x><i>it</i></BODY></html>";
        assert_eq!(formatted(body), Some("<i>it</i>"));
    }
}
