//! The preview's HTML as `html.rs` wrote it before the block model (`page.rs`), frozen: an event
//! rewriter writing accent's own HTML into the parser's stream. The tests at the end hold
//! `to_html` and `to_html_file` to it byte for byte. Test-only; it goes with WebKit (stage 2, P9).

use super::blocks::block_ids;
use super::frontmatter::scan_tags;
use super::html::mathml;
use super::links::{
    heading_named, is_external, is_image, is_url, percent_decode, percent_encode, slugs,
    split_anchor,
};
use super::options;
use super::spans::list_marker;
use super::task::in_progress;
use crate::conflict::{self, Block};
use crate::path::{parent_dir, resolve};
use pulldown_cmark::{CowStr, Event, LinkType, Options, Parser, Tag as Cm, TagEnd};
use std::collections::HashMap;
use std::ops::Range;

/// An in-progress task's box: a checkbox has no such state without a script, so the preview's
/// stylesheet draws this one itself. The newline is pulldown-cmark's after a task's own box.
const IN_PROGRESS: &str = "<input disabled=\"\" type=\"checkbox\" class=\"in-progress\"/>\n";

fn esc_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

fn open_href(target: &str, anchor: Option<&str>) -> String {
    let mut h = format!("accent://open/{}", percent_encode(target));
    if let Some(a) = anchor {
        h.push('#');
        h.push_str(&esc_attr(a));
    }
    h
}

/// Blocks that carry a source-line marker. Everything the editor can put a cursor in starts at
/// one of these, which is all the preview needs to scroll along.
fn is_block_start(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Start(
            Cm::Paragraph
                | Cm::Heading { .. }
                | Cm::Item
                | Cm::BlockQuote(_)
                | Cm::CodeBlock(_)
                | Cm::Table(_)
                | Cm::HtmlBlock
        )
    )
}

/// Render preview HTML with `accent://` wikilinks and percent-encoded `accent://tag/<tag>`
/// links. Tag classes distinguish their styling.
///
/// Block `data-line` markers synchronise preview scrolling with the editor. Heading IDs use
/// [`slugs`]; links naming heading text are rewritten to those IDs. [`block_ids`] appear as
/// `^id` anchors on markers and are hidden from rendered text.
///
/// Render conflict sides in labelled `conflict-current`, `-base` and `-incoming` boxes without
/// git markers, which markdown would otherwise misinterpret. Each stretch is parsed
/// independently with the whole note's [`definitions`], retaining source line numbers and
/// cross-stretch reference links and footnotes.
pub fn to_html(text: &str) -> String {
    render(text, None)
}

/// The note at `note`, a vault path, as an exported file holds it, read outside the app: no line
/// markers, a link into the vault as its text, a relative picture as an `accent://file/` address
/// the app inlines, and no `<script>`. A mermaid fence stays its code block.
pub fn to_html_file(text: &str, note: &str) -> String {
    without_scripts(&render(text, Some(parent_dir(note))))
}

/// `html` without its `<script>` elements, found in any case. The writer escapes every `<` of
/// text and code, so each one is the note's own HTML; one left open runs to the end, as it does
/// in a browser.
fn without_scripts(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let (mut kept, mut from) = (0, 0);
    while let Some(i) = lower[from..].find("<script").map(|i| from + i) {
        from = i + "<script".len();
        if !lower[from..].starts_with(|c: char| c == '>' || c == '/' || c.is_ascii_whitespace()) {
            continue;
        }
        out.push_str(&html[kept..i]);
        kept = lower[from..]
            .find("</script")
            .and_then(|j| lower[from + j..].find('>').map(|k| from + j + k + 1))
            .unwrap_or(html.len());
        from = kept;
    }
    out.push_str(&html[kept..]);
    out
}

/// The preview's page of `text`, or with `export`, the note's directory, an exported file's.
fn render<'a>(text: &'a str, export: Option<&'a str>) -> String {
    let mut page = Page {
        defs: definitions(text),
        export,
        ..Page::default()
    };
    let mut at = 0;
    for block in conflict::blocks(text) {
        page.markdown(text, at..block.range.start);
        page.conflict(text, &block);
        at = block.range.end;
    }
    page.markdown(text, at..text.len());
    page.finish()
}

/// The callout a quote starting at `src[at..]` opens: its type lowercased, its title, and the
/// bytes of its `[!type]` line, newline included. `None` for a plain quote. A `+` or `-` fold
/// marker is read past: a callout always shows open.
fn callout(src: &str, at: usize) -> Option<(String, String, Range<usize>)> {
    let line = src[at..].split('\n').next().unwrap_or_default();
    let head = line.strip_prefix('>')?.trim_start_matches([' ', '\t']);
    let (kind, rest) = head.strip_prefix("[!")?.split_once(']')?;
    let named = kind.starts_with(|c: char| c.is_ascii_alphabetic())
        && kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !named {
        return None;
    }
    let kind = kind.to_ascii_lowercase();
    let title = match rest.strip_prefix(['+', '-']).unwrap_or(rest).trim() {
        "" => {
            let mut t = kind.clone();
            t[..1].make_ascii_uppercase();
            t
        }
        t => t.to_string(),
    };
    let start = at + line.len() - head.len();
    Some((kind, title, start..(at + line.len() + 1).min(src.len())))
}

/// Every reference and footnote definition of a note with a conflict block, as written, each
/// after a blank line; empty for any other note. A stretch parsed on its own knows only the
/// definitions in it, so [`render`] parses each with these after it.
fn definitions(text: &str) -> String {
    let Some(blank) = conflict::blank_markers(text) else {
        return String::new();
    };
    let parser = Parser::new_ext(&blank, options());
    let mut defs: Vec<Range<usize>> = parser
        .reference_definitions()
        .iter()
        .map(|(_, d)| d.span.clone())
        .collect();
    defs.extend(
        parser.into_offset_iter().filter_map(|(ev, r)| {
            matches!(ev, Event::Start(Cm::FootnoteDefinition(_))).then_some(r)
        }),
    );
    defs.into_iter()
        .map(|r| format!("\n\n{}", &text[r]))
        .collect()
}

/// What [`render`] has gathered so far, a stretch of the note at a time.
#[derive(Default)]
struct Page<'a> {
    /// The directory of the note being exported ([`to_html_file`]); `None` for the preview.
    export: Option<&'a str>,
    evts: Vec<Event<'a>>,
    /// Each heading's place in `evts` and its text, gathered the way `analyze` gathers
    /// `Heading::text` — every text and code event inside, an embed's too — so the `id` is the
    /// anchor the editor resolves.
    headings: Vec<(usize, String)>,
    /// The place in `evts` of each link to `#…` on the page itself.
    anchors: Vec<usize>,
    /// The note's [`definitions`], parsed after every stretch.
    defs: String,
    /// How far into the note the lines are counted, and the newlines up to there: block starts
    /// arrive in source order, so one forward pass counts every marker's line.
    counted: usize,
    newlines: usize,
}

impl<'a> Page<'a> {
    /// The 1-based line the byte `at` of the note is on.
    fn line_at(&mut self, text: &str, at: usize) -> usize {
        self.newlines += text[self.counted..at]
            .bytes()
            .filter(|b| *b == b'\n')
            .count();
        self.counted = at;
        self.newlines + 1
    }

    /// The marker a block starting at byte `at` of the note opens with: its source line for the
    /// preview to scroll by, and its block id, which an exported file keeps alone.
    fn marker(&mut self, text: &str, at: usize, id: Option<&str>) -> Option<String> {
        let id = id.map_or(String::new(), |id| format!(" id=\"^{id}\""));
        match self.export {
            None => Some(format!(
                "<span data-line=\"{}\"{id}></span>",
                self.line_at(text, at)
            )),
            Some(_) if id.is_empty() => None,
            Some(_) => Some(format!("<span{id}></span>")),
        }
    }

    fn html(&mut self, html: String) {
        self.evts.push(Event::Html(html.into()));
    }

    /// Give the item being read the class GitHub gives a task, so the preview draws its box in
    /// place of the bullet. The box is the first thing in its item, so the nearest item start is
    /// its own.
    fn task_item(&mut self) {
        let item = |e: &Event| matches!(e, Event::Start(Cm::Item));
        if let Some(li) = self.evts.iter().rposition(item) {
            self.evts[li] = Event::Html("<li class=\"task-list-item\">".into());
        }
    }

    /// A text run of `src` at `r`, its `#tag`s as `accent://tag/<tag>` links the app follows to
    /// the tag. They are found in the source by the rule the index reads tags with, so a run the
    /// parser rewrote (an escape, an entity) is left as it is; one whose block id was cut off its
    /// end is still the source up to there.
    fn tagged(&mut self, src: &'a str, r: Range<usize>, t: CowStr<'a>) {
        let shown = r.start..r.start + t.len();
        let mut tags = Vec::new();
        // An exported file has no tag to go to.
        if self.export.is_none() && src.get(shown.clone()) == Some(&*t) {
            scan_tags(src, &shown, &mut tags, &mut Vec::new());
        }
        if tags.is_empty() {
            return self.evts.push(Event::Text(t));
        }
        let mut at = shown.start;
        for tag in tags {
            if at < tag.range.start {
                self.evts.push(Event::Text(src[at..tag.range.start].into()));
            }
            self.html(format!(
                "<a href=\"accent://tag/{}\" class=\"tag\">",
                percent_encode(&tag.name)
            ));
            self.evts.push(Event::Text(src[tag.range.clone()].into()));
            self.html("</a>".into());
            at = tag.range.end;
        }
        if at < shown.end {
            self.evts.push(Event::Text(src[at..shown.end].into()));
        }
    }

    /// The markdown of `text[part]`, parsed as a note of its own.
    fn markdown(&mut self, text: &'a str, part: Range<usize>) {
        let src = &text[part.clone()];
        let mut opts = options();
        // Front matter opens the note, never a stretch after a conflict block.
        if part.start > 0 {
            opts.remove(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
        }
        let blocks = block_ids(src);
        // Asked of every event, so indexed once: a block start's id (the first, if two name it),
        // and the marker inside a text, found by halving, as `block_ids` gives them in order.
        let mut ids = HashMap::new();
        for b in &blocks {
            ids.entry(b.start).or_insert(b.id.as_str());
        }
        let marker_in = |r: &Range<usize>| {
            let at = blocks.partition_point(|b| b.marker.start < r.start);
            blocks.get(at).filter(|b| r.contains(&b.marker.start))
        };
        // Whether each open link and embed is ours to close rather than the parser's: with our
        // `</a>` in the preview, with nothing in an exported file, which keeps only its text.
        let mut link_ours: Vec<bool> = Vec::new();
        let mut image_ours: Vec<bool> = Vec::new();
        let mut skip = 0usize;
        // The `[/]` the item being read opens with, and once its box is in, the bytes it covered.
        let mut doing: Option<Range<usize>> = None;
        let mut dropped = 0..0;
        let mut in_heading = false;
        // Where a text run is never a tag, as `analyze` reads them: code and the front matter.
        let mut in_code = false;
        let mut in_meta = false;
        // The `[!type]` line of the callout being read, its newline included: its events go.
        let mut header = 0..0;

        // The definitions go on after the stretch, where the parser finds them; nothing of them
        // is shown, their events all starting past its end.
        let joined;
        let events: Box<dyn Iterator<Item = (Event<'a>, Range<usize>)>> = if self.defs.is_empty() {
            Box::new(Parser::new_ext(src, opts).into_offset_iter())
        } else {
            joined = format!("{src}{}", self.defs);
            Box::new(
                Parser::new_ext(&joined, opts)
                    .into_offset_iter()
                    .filter(|(_, r)| r.start < src.len())
                    .map(|(ev, r)| (ev.into_static(), r)),
            )
        };
        let mut events = events.peekable();
        while let Some((ev, r)) = events.next() {
            if header.contains(&r.start) && r.end <= header.end {
                continue;
            }
            match &ev {
                Event::Start(Cm::Heading { .. }) => {
                    in_heading = true;
                    self.headings.push((self.evts.len(), String::new()));
                }
                Event::End(TagEnd::Heading(_)) => in_heading = false,
                Event::Start(Cm::CodeBlock(_)) => in_code = true,
                Event::End(TagEnd::CodeBlock) => in_code = false,
                Event::Start(Cm::MetadataBlock(_)) => in_meta = true,
                Event::End(TagEnd::MetadataBlock(_)) => in_meta = false,
                Event::Text(t) | Event::Code(t) if in_heading => {
                    if let Some((_, h)) = self.headings.last_mut() {
                        h.push_str(t);
                    }
                }
                _ => {}
            }
            if skip > 0 {
                match ev {
                    Event::Start(Cm::Image { .. }) => skip += 1,
                    Event::End(TagEnd::Image) => skip -= 1,
                    _ => {}
                }
                continue;
            }
            let marker = is_block_start(&ev)
                .then(|| self.marker(text, part.start + r.start, ids.get(&r.start).copied()))
                .flatten()
                .map(|m| Event::Html(m.into()));
            // A callout's header is its quote's first paragraph line, which the title replaces,
            // after the quote's marker.
            if matches!(ev, Event::Start(Cm::BlockQuote(_)))
                && let Some((kind, title, line)) = callout(src, r.start)
                && let Some((Event::Start(Cm::Paragraph), p)) = events.peek()
                && p.start == line.start
            {
                header = line;
                self.html(format!(
                    "<blockquote class=\"callout\" data-callout=\"{kind}\">\n"
                ));
                self.evts.extend(marker);
                self.html(format!(
                    "<p class=\"callout-title\">{}</p>\n",
                    esc_attr(&title)
                ));
                continue;
            }
            // The text a block's id ends goes on without it.
            let ev = match (ev, marker_in(&r)) {
                (Event::Text(t), Some(b)) => {
                    let shown = t.strip_suffix(&src[b.marker.clone()]).unwrap_or(&t);
                    Event::Text(shown.trim_end().to_string().into())
                }
                (ev, _) => ev,
            };
            match ev {
                Event::Start(Cm::Link {
                    link_type: LinkType::WikiLink { .. },
                    ref dest_url,
                    ..
                }) => {
                    if self.export.is_none() {
                        let (t, anchor) = split_anchor(dest_url);
                        self.html(format!(
                            "<a href=\"{}\" class=\"wikilink\">",
                            open_href(t, anchor)
                        ));
                    }
                    link_ours.push(true);
                }
                // A link into the vault goes nowhere from an exported file: its text is left.
                Event::Start(Cm::Link {
                    ref dest_url,
                    link_type,
                    ..
                }) if self.export.is_some()
                    && !(dest_url.starts_with('#')
                        || is_url(dest_url)
                        || link_type == LinkType::Email) =>
                {
                    link_ours.push(true);
                }
                Event::Start(Cm::Link { ref dest_url, .. }) => {
                    if dest_url.starts_with('#') {
                        self.anchors.push(self.evts.len());
                    }
                    link_ours.push(false);
                    self.evts.push(ev);
                }
                Event::End(TagEnd::Link) => match link_ours.pop() {
                    Some(true) if self.export.is_some() => {}
                    Some(true) => self.html("</a>".into()),
                    _ => self.evts.push(ev),
                },
                Event::Start(Cm::Image {
                    link_type: LinkType::WikiLink { .. },
                    ref dest_url,
                    ..
                }) => {
                    let (t, anchor) = split_anchor(dest_url);
                    if is_image(t) {
                        self.html(format!("<img src=\"accent://file/{}\">", percent_encode(t)));
                        skip = 1;
                    } else if crate::path::is_diagram(t) {
                        // A picture of the page the anchor names, which the app draws, inside
                        // the link a click follows to that page; an exported file has the
                        // picture alone.
                        let page = anchor
                            .map_or_else(String::new, |a| format!("?page={}", percent_encode(a)));
                        let img =
                            format!("<img src=\"accent://file/{}{page}\">", percent_encode(t));
                        self.html(match self.export {
                            None => format!(
                                "<a href=\"{}\" class=\"embed\">{img}</a>",
                                open_href(t, anchor)
                            ),
                            Some(_) => img,
                        });
                        skip = 1;
                    } else {
                        if self.export.is_none() {
                            self.html(format!(
                                "<a href=\"{}\" class=\"embed\">",
                                open_href(t, anchor)
                            ));
                        }
                        image_ours.push(true);
                    }
                }
                // An exported file is read away from the note's folder, so a picture beside it
                // is fetched by its vault path.
                Event::Start(Cm::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => {
                    let dest_url = match self.export {
                        Some(dir) if !is_external(&dest_url) => {
                            let rel = resolve(dir, &percent_decode(&dest_url));
                            format!("accent://file/{}", percent_encode(&rel)).into()
                        }
                        _ => dest_url,
                    };
                    image_ours.push(false);
                    self.evts.push(Event::Start(Cm::Image {
                        link_type,
                        dest_url,
                        title,
                        id,
                    }));
                }
                Event::End(TagEnd::Image) => match image_ours.pop() {
                    Some(true) if self.export.is_some() => {}
                    Some(true) => self.html("</a>".into()),
                    _ => self.evts.push(ev),
                },
                Event::InlineMath(ref src) | Event::DisplayMath(ref src) => {
                    let html = mathml(src, matches!(ev, Event::DisplayMath(_)));
                    // A typo must never blank a formula: without MathML the original event goes
                    // on and pulldown-cmark's `.math` span shows the source as the author wrote it.
                    match html {
                        Some(html) => self.html(html),
                        None => self.evts.push(ev),
                    }
                }
                Event::TaskListMarker(_) => {
                    self.task_item();
                    self.evts.push(ev);
                }
                Event::Start(Cm::Item) => {
                    doing = list_marker(src, &r).and_then(|m| in_progress(src, m.end));
                    self.evts.push(ev);
                }
                // `[/]` is three runs of text to pulldown-cmark: a box of its own takes the first
                // one's place, and the other two and the space after them go, as the parser drops
                // a task's own brackets.
                Event::Text(_) if doing.as_ref().is_some_and(|d| d.start == r.start) => {
                    dropped = doing.take().unwrap_or_default();
                    self.task_item();
                    self.html(IN_PROGRESS.into());
                }
                Event::Text(_) if dropped.contains(&r.start) => {}
                Event::Text(t) if !dropped.is_empty() && dropped.end == r.start => {
                    dropped = 0..0;
                    let shown = t.trim_start().to_string();
                    self.tagged(src, r.end - shown.len()..r.end, shown.into());
                }
                Event::Text(t)
                    if !in_code && !in_meta && link_ours.is_empty() && image_ours.is_empty() =>
                {
                    self.tagged(src, r, t)
                }
                _ => self.evts.push(ev),
            }
            self.evts.extend(marker);
        }
    }

    /// A conflict block's sides top to bottom, each in its box under its marker's label. A box
    /// starts where the marker above it is, so that is its caption's line; the incoming side is
    /// named by the marker below it, the `>>>>>>>` line.
    fn conflict(&mut self, text: &'a str, block: &Block) {
        let m = block.markers();
        let (split, end) = (&m[m.len() - 2], &m[m.len() - 1]);
        let sides = [
            Some(("current", &m[0], m[0].start, block.ours.clone())),
            block
                .base
                .clone()
                .map(|base| ("base", &m[1], m[1].start, base)),
            Some(("incoming", end, split.start, block.theirs.clone())),
        ];
        self.html("<div class=\"conflict\">\n".into());
        for (side, named, top, lines) in sides.into_iter().flatten() {
            let marker = self.marker(text, top, None).unwrap_or_default();
            let label = esc_attr(conflict::label(text, named.clone()));
            self.html(format!(
                "<div class=\"conflict-{side}\"><div class=\"conflict-label\">\
                 {marker}{label}</div>\n"
            ));
            self.markdown(text, lines);
            self.html("</div>\n".into());
        }
        self.html("</div>\n".into());
    }

    fn finish(mut self) -> String {
        let texts: Vec<&str> = self.headings.iter().map(|(_, h)| h.as_str()).collect();
        let ids = slugs(texts.iter().copied());
        // A link to a heading of the page goes to its `id`, which is all WebKit scrolls to, even
        // when it names the heading by its text, as the editor lets it.
        for at in &self.anchors {
            if let Event::Start(Cm::Link { dest_url, .. }) = &mut self.evts[*at]
                && let Some(i) = heading_named(&texts, &percent_decode(&dest_url[1..]))
            {
                *dest_url = format!("#{}", ids[i]).into();
            }
        }
        for ((at, _), slug) in self.headings.iter().zip(ids) {
            if let Event::Start(Cm::Heading { id, .. }) = &mut self.evts[*at] {
                *id = Some(slug.into());
            }
        }
        let mut out = String::new();
        pulldown_cmark::html::push_html(&mut out, self.evts.into_iter());
        out
    }
}

/// The block model's writer against this one, over every input the html tests use, two fixtures
/// of everything accent writes for itself (the generated vault has no callout, `[/]`, raw HTML
/// block or comment), and on demand a generated vault.
mod tests {
    use crate::markdown::{to_html, to_html_file};

    const NOTE: &str = r##"---
title: Fixture
tags: [meta, "#quoted"]
---

# Fixture #draft

Intro with **bold**, *em*, ~~struck~~, `#code`, \#escaped, a#b, #123, &amp; &copy; and #tag/nested.\
A [[Note]], [[Note#Head|alias #x]], [[#Fixture]], [[Folder/Other Note#^blk]] and #café. ^intro

## Links

[vault](Other%20Note.md), [web](https://example.org/a?b=1&c=2 "Title"), <https://auto.example>,
<me@example.org>, [mail](mailto:me@example.org), [top](#Fixture), [text](#Links),
[spaced](<#links>), [none](#nowhere), [ref][r], [collapsed][], [blk](#^intro), [up](../up.md#Head).

[collapsed]: https://example.org/collapsed
[r]: Folder/Ref.md "Ref"

## Pictures

![rel](Attachments/a%201.png "t") ![abs](/abs.png) ![web](https://example.org/w.png)
![data](data:image/png;base64,AAAA) ![amp](./b&c.png) ![up](../../out.png)
![[Attachments/img 1.png]] ![[img.png|300]] ![[flow.drawio]] ![[Figures/flow.drawio#Page 2]]
![[paper.pdf#page=3&selection=4,0,4,11]] ![[Note#Head|an *alias* #t]] ![[Note]]

[![linked](img.png)](https://example.org) [![[inner.png]]](Note.md) [x ![[deep.pdf]]](y.md)

## Tasks

- [ ] todo #t
- [x] done
- [/] doing #x
- [/]
- plain
  - [X] nested
  - [/] nested doing
* [ ] star
+ [/] plus

1. [/] loose

2. [ ] two

3. three

not [/] here, - [/] nor here

## Callouts

> [!Note]
> Body with #tag and [[Note]].

> [!tip]- Read *this* & that
>
> Body. ^tipid

> [!WARNING]+
> > nested quote
> - list in callout

> [!my-type] Title "quoted" <b>
> line

- > [!info] in a list
  > continued

> plain quote
> > deeper #deep

> [!x y]
> not a callout

## Math

Inline $x^2$, $a < b \& c$, bad $\left( x$, and $\text{<&>}$.

$$\frac{a}{b}$$

$$
\nosuchcommand
$$

## Code

```rust
fn main() { let x = "#notag"; }
```

```mermaid
graph TD; A-->B
```

    indented #code
    block

~~~
tilde
~~~

## Tables

| Left | Center | Right |
|:-----|:------:|------:|
| #tag | [[Note]] | $x$ |
| a \| b | `c` | ![[img.png]] |

^table

## HTML

<div class="box">

**markdown inside** #divtag

</div>

<!-- a comment
over lines -->

Inline <!-- comment --> and <kbd>Ctrl</kbd> and <span style="color:red">red</span>.

<details>
<summary>More #summ</summary>

Hidden *markdown* with [[Note]].

</details>

<script>alert(1)</script>
<SCRIPT src=x></Script>

<img src="Attachments/raw.png" alt="raw">

## Footnotes

A note[^1] and another[^long].

[^1]: The first, with #fntag.
[^long]: The second,
    over lines.

## Headings

Setext Heading
==============

Another
-------

### `C++` and ![[logo.png]]

### Fixture

#### Links

***

- a
- b

^list

1) one
2) two

Term #end"##;

    const CONFLICTS: &str = r##"Before[^n] and [ref], see [Ours](#ours-o).
<<<<<<< HEAD
## Ours #o

- [ ] ours task
> [!note]
> ours callout
||||||| base
base text $x$ ^b
=======
[^n]: Footnote in theirs.

Theirs with [[Note]] ^cid
>>>>>>> feature/<x>&"y"
<<<<<<< HEAD
=======
only theirs
>>>>>>> other

[ref]: https://example.org
"##;

    /// Every input the tests of `html.rs` and `markdown/mod.rs` render, and the fixtures.
    const INPUTS: &[&str] = &[
        NOTE,
        CONFLICTS,
        "[[Note#Head|alias]]",
        "[[Other Note]]",
        "![[img.png]]",
        "![[paper.pdf#page=3&selection=4,0,4,11]]",
        "# Hi\n\n[x](y.md)\n",
        "Read #inbox, then #area/work-1 and #café.\n",
        "# Plan #draft\n\nNotes on #x ^para\n",
        "---\ntags: [meta]\n---\n`#code` [#text](x.md) [[N|#alias]] a#b #123 \\#esc\n\n\
         ```\n#fenced\n```\n",
        "![[Figures/flow.drawio#Page 2]]",
        "![[flow.drawio]]",
        "$x^2$",
        "$$\\frac{a}{b}$$",
        "$\\nosuchcommand$",
        "ok $x^2$ and $\\left( x$\n\n$$\\frac{a}{b}$$\n\n$$\n\\nosuchcommand\n$$\n\n\
         `$\\left( x$`\n\n```\n$\\left( x$\n```\n",
        "- [ ] todo\n- plain\n  - [x] nested\n",
        "1. [x] one\n\n2. [ ] two\n",
        "- [/] doing #x\n\n1. [/] loose\n\n2. two\n\nnot [/] here\n",
        "![alt](Attachments/img-0.png)",
        "![[Attachments/img 1.png]]",
        "![alt](<Attachments/img 0.png>)",
        "# Notes\n## Notes\n## `C++` and ![[logo.png]]\n",
        "[a](#My%20Section) [b](<#my section>) [c](#MY-SECTION) [d](#nowhere)\n\n\
         ## My Section\n",
        "Some prose. ^para\n\n- a\n- b\n\n^list\n",
        "intro\n<<<<<<< HEAD\nours *here*\n=======\n# Theirs\n\n- item\n\
         >>>>>>> feature/<x>\nafter\n",
        "<<<<<<< HEAD\nours\n||||||| base\n## Base\n=======\n>>>>>>> side\n",
        "See [the docs][d] and a note[^n].\n\
         <<<<<<< HEAD\n[^n]: The note.\n=======\n[d] too\n>>>>>>> side\n\n\
         [d]: docs.md\n",
        "# Head\n\nSee [[N]], #tag, [x](y.md), ![[paper.pdf]], [h](#Head) and \
         [w](https://example.org). ^p\n\n\
         ![a](../img%201.png) ![b](https://example.org/b.png) ![[c.png]] \
         ![[d.drawio#P 2]]\n\n\
         <script>alert(1)</script>\n<div>kept</div>\n\n\
         In <SCRIPT src=x></Script>line, `<script>` shown\n\n\
         <<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> side\n",
        "> [!Note]\n> Body.\n",
        "> [!tip]- Read *this* & that\n>\n> Body.\n",
        "- > > [!my-type]\n",
        "> a [!x]\n",
        "> a\n> [!x]\n",
        "[!x]\n",
        ">     [!x]\n",
        "# Title\n\nFirst para.\n\nSecond para.\n\n- item\n\n```rs\ncode\n```\n",
        "- [ ] \\\n\t-",
        "",
    ];

    #[test]
    fn to_html_matches_before() {
        for text in INPUTS {
            assert_eq!(to_html(text), super::to_html(text), "{text:?}");
        }
        // The fixtures hold what they are here for.
        let html = to_html(&format!("{NOTE}\n\n{CONFLICTS}"));
        for shown in [
            "class=\"callout\"",
            "class=\"in-progress\"",
            "class=\"tag\"",
            "class=\"embed\"",
            "<math",
            "class=\"math math-inline\"",
            "<div class=\"box\">",
            "<!--",
            "<details>",
            "footnote-definition",
            "conflict-base",
        ] {
            assert!(html.contains(shown), "{shown}");
        }
    }

    #[test]
    fn to_html_file_matches_before() {
        for text in INPUTS {
            let (now, before) = (
                to_html_file(text, "a/b.md"),
                super::to_html_file(text, "a/b.md"),
            );
            assert_eq!(now, before, "{text:?}");
        }
    }

    /// Both over every note of a generated vault: `make vault VAULT=<dir>`, then
    /// `ACCENT_VAULT=<dir> cargo test -p accent-core --release -- --ignored matches_before`.
    #[test]
    #[ignore = "needs a generated vault in ACCENT_VAULT"]
    fn the_vault_matches_before() {
        let vault = std::env::var("ACCENT_VAULT").expect("ACCENT_VAULT names a generated vault");
        let mut notes = 0;
        for entry in ignore::WalkBuilder::new(&vault)
            .standard_filters(false)
            .build()
        {
            let path = entry.unwrap().into_path();
            let Some(rel) = path.strip_prefix(&vault).ok().and_then(|p| p.to_str()) else {
                continue;
            };
            if !rel.ends_with(".md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(to_html(&text) == super::to_html(&text), "{rel}");
            assert!(
                to_html_file(&text, rel) == super::to_html_file(&text, rel),
                "{rel}"
            );
            notes += 1;
        }
        eprintln!("{notes} notes match");
        assert!(notes > 0, "no note in {vault}");
    }
}
