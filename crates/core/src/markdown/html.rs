//! The preview pane's HTML: wikilinks and tags become `accent://` anchors, math becomes MathML,
//! every block carries the source line it starts on, and a conflict block its sides in boxes.

use super::blocks::block_ids;
use super::frontmatter::scan_tags;
use super::links::{heading_named, is_image, percent_decode, percent_encode, slugs, split_anchor};
use super::options;
use crate::conflict::{self, Block};
use pulldown_cmark::{CowStr, Event, LinkType, Options, Parser, Tag as Cm, TagEnd};
use pulldown_latex::{Event as LatexEvent, ParserError, Storage};
use std::collections::HashMap;
use std::error::Error;
use std::ops::Range;

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

/// A formula's LaTeX as parser events, or why it does not parse: the one test of whether a
/// formula renders, which [`mathml`] and [`math_errors`] both ask.
fn latex<'a>(src: &'a str, storage: &'a Storage) -> Result<Vec<LatexEvent<'a>>, ParserError> {
    pulldown_latex::Parser::new(src, storage).collect()
}

/// A `$…$` or `$$…$$` formula as MathML, or `None` if the LaTeX does not parse.
///
/// The renderer itself never reports a failure — it writes `<merror>` and carries on — so the
/// parser events are collected first, and that is what decides between MathML and the caller's
/// raw-source fallback. WebKit draws MathML natively, so no stylesheet or script goes with it.
pub fn mathml(src: &str, display: bool) -> Option<String> {
    use pulldown_latex::RenderConfig;
    use pulldown_latex::config::DisplayMode;

    let storage = Storage::new();
    let events = latex(src, &storage).ok()?;
    let mut out = String::new();
    pulldown_latex::push_mathml(
        &mut out,
        events.into_iter().map(Ok::<_, ParserError>),
        RenderConfig {
            display_mode: if display {
                DisplayMode::Block
            } else {
                DisplayMode::Inline
            },
            ..Default::default()
        },
    )
    .ok()?;
    Some(out)
}

/// Every formula in `text` the preview shows as source instead of MathML: where it is written,
/// `$` delimiters included, and why its LaTeX does not parse.
///
/// Meant to run on every edit, so a note without a `$` is not parsed at all.
pub fn math_errors(text: &str) -> Vec<(Range<usize>, String)> {
    if !text.contains('$') {
        return Vec::new();
    }
    Parser::new_ext(text, options())
        .into_offset_iter()
        .filter_map(|(ev, r)| match ev {
            Event::InlineMath(src) | Event::DisplayMath(src) => {
                let err = latex(&src, &Storage::new()).err()?;
                // Its `Display` draws the offending text with carets under it; the source is the
                // reason alone, which is what fits on the end of an editor line.
                let why = err
                    .source()
                    .map_or_else(|| err.to_string(), ToString::to_string);
                Some((r, why))
            }
            _ => None,
        })
        .collect()
}

/// Render a note to an HTML fragment for the preview pane (wikilinks become `<a href="accent://…">`).
///
/// A `#tag` becomes `<a href="accent://tag/<tag>" class="tag">`, the tag percent-encoded, which the
/// app follows to the tag's notes; the class lets the page draw it as a tag rather than a link.
///
/// Each block opens with an empty `<span data-line="N">`, so the preview can scroll to the line
/// the editor's cursor is on, and each heading gets its [`slugs`] anchor as its `id`, so an
/// in-note `[text](#slug)` scrolls there; `[text](#My%20Section)`, naming the heading by its text,
/// is pointed at that `id`. A block with a [`block_ids`] id carries it on that
/// span, as `^id`, which is how `[text](#^id)` names it, and the `^id` itself is not shown.
///
/// A conflict block git left in the note ([`conflict::blocks`]) is shown as its sides, each in a
/// `<div class="conflict-current">` (`-base`, `-incoming`) captioned with its marker's label, and
/// with no marker shown: read as markdown, `=======` would make the current side a heading and
/// `>>>>>>>` a quote. The note is cut at the blocks and every stretch parsed on its own, so each
/// side is the markdown it says whatever the other leaves open, and every line marker is the
/// note's own. Each stretch is given the note's [`definitions`], so a reference link or a
/// footnote finds its definition across a block's edge.
pub fn to_html(text: &str) -> String {
    let mut page = Page {
        defs: definitions(text),
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

/// Every reference and footnote definition of a note with a conflict block, as written, each
/// after a blank line; empty for any other note. A stretch parsed on its own knows only the
/// definitions in it, so [`to_html`] parses each with these after it.
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

/// What [`to_html`] has gathered so far, a stretch of the note at a time.
#[derive(Default)]
struct Page<'a> {
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

    fn html(&mut self, html: String) {
        self.evts.push(Event::Html(html.into()));
    }

    /// A text run of `src` at `r`, its `#tag`s as `accent://tag/<tag>` links the app follows to
    /// the tag. They are found in the source by the rule the index reads tags with, so a run the
    /// parser rewrote (an escape, an entity) is left as it is; one whose block id was cut off its
    /// end is still the source up to there.
    fn tagged(&mut self, src: &'a str, r: Range<usize>, t: CowStr<'a>) {
        let shown = r.start..r.start + t.len();
        let mut tags = Vec::new();
        if src.get(shown.clone()) == Some(&*t) {
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
        let mut link_wiki: Vec<bool> = Vec::new();
        let mut image_wiki: Vec<bool> = Vec::new();
        let mut skip = 0usize;
        let mut in_heading = false;
        // Where a text run is never a tag, as `analyze` reads them: code and the front matter.
        let mut in_code = false;
        let mut in_meta = false;

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
        for (ev, r) in events {
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
            let marker = is_block_start(&ev).then(|| {
                let line = self.line_at(text, part.start + r.start);
                let id = ids
                    .get(&r.start)
                    .map_or(String::new(), |id| format!(" id=\"^{id}\""));
                Event::Html(format!("<span data-line=\"{line}\"{id}></span>").into())
            });
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
                    let (t, anchor) = split_anchor(dest_url);
                    self.html(format!(
                        "<a href=\"{}\" class=\"wikilink\">",
                        open_href(t, anchor)
                    ));
                    link_wiki.push(true);
                }
                Event::Start(Cm::Link { ref dest_url, .. }) => {
                    if dest_url.starts_with('#') {
                        self.anchors.push(self.evts.len());
                    }
                    link_wiki.push(false);
                    self.evts.push(ev);
                }
                Event::End(TagEnd::Link) => {
                    if link_wiki.pop().unwrap_or(false) {
                        self.html("</a>".into());
                    } else {
                        self.evts.push(ev);
                    }
                }
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
                        // the link a click follows to that page.
                        let page = anchor
                            .map_or_else(String::new, |a| format!("?page={}", percent_encode(a)));
                        self.html(format!(
                            "<a href=\"{}\" class=\"embed\"><img src=\"accent://file/{}{page}\"></a>",
                            open_href(t, anchor),
                            percent_encode(t)
                        ));
                        skip = 1;
                    } else {
                        self.html(format!(
                            "<a href=\"{}\" class=\"embed\">",
                            open_href(t, anchor)
                        ));
                        image_wiki.push(true);
                    }
                }
                Event::Start(Cm::Image { .. }) => {
                    image_wiki.push(false);
                    self.evts.push(ev);
                }
                Event::End(TagEnd::Image) => {
                    if image_wiki.pop().unwrap_or(false) {
                        self.html("</a>".into());
                    } else {
                        self.evts.push(ev);
                    }
                }
                Event::InlineMath(ref src) | Event::DisplayMath(ref src) => {
                    let html = mathml(src, matches!(ev, Event::DisplayMath(_)));
                    // A typo must never blank a formula: without MathML the original event goes
                    // on and pulldown-cmark's `.math` span shows the source as the author wrote it.
                    match html {
                        Some(html) => self.html(html),
                        None => self.evts.push(ev),
                    }
                }
                // The class GitHub uses, so the preview can draw the checkbox in place of the
                // bullet. The marker is the first thing in its item, so the nearest item start is
                // its own.
                Event::TaskListMarker(_) => {
                    if let Some(li) = self
                        .evts
                        .iter()
                        .rposition(|e| matches!(e, Event::Start(Cm::Item)))
                    {
                        self.evts[li] = Event::Html("<li class=\"task-list-item\">".into());
                    }
                    self.evts.push(ev);
                }
                Event::Text(t)
                    if !in_code && !in_meta && link_wiki.is_empty() && image_wiki.is_empty() =>
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
            let line = self.line_at(text, top);
            let label = esc_attr(conflict::label(text, named.clone()));
            self.html(format!(
                "<div class=\"conflict-{side}\"><div class=\"conflict-label\">\
                 <span data-line=\"{line}\"></span>{label}</div>\n"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_rewrites_wikilinks() {
        let h = bare("[[Note#Head|alias]]");
        assert_eq!(
            h.trim(),
            "<p><a href=\"accent://open/Note#Head\" class=\"wikilink\">alias</a></p>"
        );

        let h = bare("[[Other Note]]");
        assert!(
            h.contains("<a href=\"accent://open/Other%20Note\" class=\"wikilink\">Other Note</a>"),
            "{h}"
        );

        let h = bare("![[img.png]]");
        assert!(h.contains("<img src=\"accent://file/img.png\">"), "{h}");
        assert!(!h.contains("alt="), "embed alt text is dropped: {h}");

        // non-image embeds fall back to a link
        let h = bare("![[paper.pdf#page=3&selection=4,0,4,11]]");
        assert!(
            h.contains(
                "<a href=\"accent://open/paper.pdf#page=3&amp;selection=4,0,4,11\" class=\"embed\">"
            ),
            "{h}"
        );

        // ordinary markdown is untouched
        let h = bare("# Hi\n\n[x](y.md)\n");
        assert!(h.contains("<h1 id=\"hi\">Hi</h1>"), "{h}");
        assert!(h.contains("<a href=\"y.md\">x</a>"), "{h}");
    }

    /// A `#tag` is a link the app follows to the tag, by the rule the index reads tags with: not
    /// in code, a link's text or the front matter, not mid-word and not a bare number.
    #[test]
    fn html_links_a_tag_to_its_notes() {
        let h = bare("Read #inbox, then #area/work-1 and #café.\n");
        for (href, text) in [
            ("inbox", "inbox"),
            ("area/work-1", "area/work-1"),
            ("caf%C3%A9", "café"),
        ] {
            assert!(
                h.contains(&format!(
                    "<a href=\"accent://tag/{href}\" class=\"tag\">#{text}</a>"
                )),
                "{h}"
            );
        }
        assert!(h.starts_with("<p>Read <a "), "{h}");
        assert!(h.contains("</a>, then <a "), "{h}");

        // A heading keeps the anchor its whole text makes, and a block keeps its tag before its id.
        let h = bare("# Plan #draft\n\nNotes on #x ^para\n");
        assert!(
            h.contains("<h1 id=\"plan-draft\">Plan <a href=\"accent://tag/draft\""),
            "{h}"
        );
        assert!(
            h.contains("<p>Notes on <a href=\"accent://tag/x\" class=\"tag\">#x</a></p>"),
            "{h}"
        );

        let src = "---\ntags: [meta]\n---\n`#code` [#text](x.md) [[N|#alias]] a#b #123 \\#esc\n\n\
                   ```\n#fenced\n```\n";
        assert!(!bare(src).contains("accent://tag/"), "{}", bare(src));
    }

    /// A diagram shows as a picture of its page, inside the link that opens it there.
    #[test]
    fn html_shows_an_embedded_diagram_as_a_picture_of_its_page() {
        let h = bare("![[Figures/flow.drawio#Page 2]]");
        assert!(
            h.contains(
                "<a href=\"accent://open/Figures/flow.drawio#Page 2\" class=\"embed\">\
                 <img src=\"accent://file/Figures/flow.drawio?page=Page%202\"></a>"
            ),
            "{h}"
        );
        let h = bare("![[flow.drawio]]");
        assert!(
            h.contains("<img src=\"accent://file/flow.drawio\"></a>"),
            "{h}"
        );
    }

    #[test]
    fn html_renders_math_as_mathml() {
        let h = bare("$x^2$");
        assert!(h.contains("<math display=\"inline\">"), "{h}");
        assert!(h.contains("<msup>"), "{h}");

        let h = bare("$$\\frac{a}{b}$$");
        assert!(h.contains("<math display=\"block\">"), "{h}");
        assert!(h.contains("<mfrac>"), "{h}");

        // A formula the parser rejects keeps its source on screen rather than rendering as
        // nothing, so a typo is visible and fixable.
        let h = bare("$\\nosuchcommand$");
        assert!(h.contains("class=\"math math-inline\""), "{h}");
        assert!(h.contains("\\nosuchcommand"), "{h}");
    }

    /// Exactly the formulas the preview shows as source are reported, where they are written,
    /// dollars included; a `$` in code is not a formula.
    #[test]
    fn math_errors_are_the_formulas_the_preview_cannot_render() {
        let src = "ok $x^2$ and $\\left( x$\n\n$$\\frac{a}{b}$$\n\n$$\n\\nosuchcommand\n$$\n\n\
                   `$\\left( x$`\n\n```\n$\\left( x$\n```\n";
        let errors = math_errors(src);
        let written: Vec<&str> = errors.iter().map(|(r, _)| &src[r.clone()]).collect();
        assert_eq!(
            written,
            ["$\\left( x$", "$$\n\\nosuchcommand\n$$"],
            "{errors:?}"
        );
        assert_eq!(errors[0].0, 13..23);
        assert!(errors[0].1.starts_with("unbalanced group"), "{errors:?}");
        // The reason alone, not the parser's drawing of where it happened.
        assert!(!errors[1].1.contains('\n'), "{errors:?}");
        for (r, _) in &errors {
            assert!(bare(&src[r.clone()]).contains("class=\"math"), "{r:?}");
        }
        assert!(math_errors("no formulas, `$\\left( x$` in code").is_empty());
    }

    /// A task item carries the class the preview drops the bullet for, so it shows the checkbox
    /// alone; a plain item beside it, nested or not, keeps its bullet.
    #[test]
    fn html_marks_task_items() {
        let h = bare("- [ ] todo\n- plain\n  - [x] nested\n");
        assert_eq!(h.matches("<li class=\"task-list-item\">").count(), 2, "{h}");
        assert!(
            h.contains("<li class=\"task-list-item\"><input disabled=\"\" type=\"checkbox\"/>"),
            "{h}"
        );
        assert!(h.contains("<li>plain"), "{h}");

        // A loose list puts the checkbox inside a paragraph; the item still takes the class.
        let h = bare("1. [x] one\n\n2. [ ] two\n");
        assert_eq!(h.matches("<li class=\"task-list-item\">").count(), 2, "{h}");
    }

    /// Both embed forms have to end up as a URL the preview's `accent:` scheme can serve: a
    /// markdown image relative to the note's directory, a wikilink embed rooted at the vault.
    #[test]
    fn html_resolves_both_image_forms() {
        let h = bare("![alt](Attachments/img-0.png)");
        assert!(
            h.contains("<img src=\"Attachments/img-0.png\" alt=\"alt\" />"),
            "{h}"
        );

        let h = bare("![[Attachments/img 1.png]]");
        assert!(
            h.contains("<img src=\"accent://file/Attachments/img%201.png\">"),
            "{h}"
        );

        // A space needs the pointy-bracket form in markdown, and is escaped the same way.
        let h = bare("![alt](<Attachments/img 0.png>)");
        assert!(h.contains("src=\"Attachments/img%200.png\""), "{h}");
    }

    /// A heading's `id` is the anchor `[text](#…)` completes and the editor resolves, so a click
    /// on one lands where Ctrl+click does: a repeat takes its suffix, markup and embeds count.
    #[test]
    fn html_gives_each_heading_its_anchor() {
        let src = "# Notes\n## Notes\n## `C++` and ![[logo.png]]\n";
        let h = bare(src);
        let ids: Vec<&str> = h
            .match_indices(" id=\"")
            .map(|(i, m)| {
                let rest = &h[i + m.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        let headings = crate::markdown::analyze(src).headings;
        assert_eq!(ids, slugs(headings.iter().map(|h| h.text.as_str())), "{h}");
        assert_eq!(ids, ["notes", "notes-1", "c-and-logopng"]);
    }

    /// A same-page link written with a heading's text, as the editor resolves it, points at that
    /// heading's `id`, which is where WebKit scrolls; one naming no heading is left as written.
    #[test]
    fn html_points_a_text_anchor_at_its_heading() {
        let h = bare(
            "[a](#My%20Section) [b](<#my section>) [c](#MY-SECTION) [d](#nowhere)\n\n\
             ## My Section\n",
        );
        for text in ["a", "b", "c"] {
            assert!(
                h.contains(&format!("<a href=\"#my-section\">{text}</a>")),
                "{h}"
            );
        }
        assert!(h.contains("<a href=\"#nowhere\">d</a>"), "{h}");
    }

    /// A block with an id carries it on its line marker, so an in-note `[text](#^id)` scrolls to
    /// it, and the `^id` itself is not shown, as Obsidian's reading view shows none.
    #[test]
    fn html_gives_a_block_its_id_and_hides_it() {
        let h = to_html("Some prose. ^para\n\n- a\n- b\n\n^list\n");
        assert!(
            h.contains("<p><span data-line=\"1\" id=\"^para\"></span>Some prose.</p>"),
            "{h}"
        );
        assert!(
            h.contains("<li><span data-line=\"3\" id=\"^list\"></span>a</li>"),
            "{h}"
        );
        assert!(!h.contains("^list<") && !h.contains(" ^para"), "{h}");
    }

    /// The source lines of the markers in `h`, top to bottom.
    fn lines(h: &str) -> Vec<&str> {
        h.match_indices("<span data-line=\"")
            .map(|(i, m)| {
                let rest = &h[i + m.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect()
    }

    /// A conflict block is its sides in tinted boxes, each captioned with its marker's label and
    /// rendered as the markdown it is, with no marker on the page: read as markdown, `=======`
    /// would make the current side a heading and `>>>>>>>` a quote.
    #[test]
    fn html_shows_a_conflict_as_its_sides() {
        let src = "intro\n<<<<<<< HEAD\nours *here*\n=======\n# Theirs\n\n- item\n\
                   >>>>>>> feature/<x>\nafter\n";
        let h = to_html(src);
        assert_eq!(
            h,
            "<p><span data-line=\"1\"></span>intro</p>\n\
             <div class=\"conflict\">\n\
             <div class=\"conflict-current\">\
             <div class=\"conflict-label\"><span data-line=\"2\"></span>HEAD</div>\n\
             <p><span data-line=\"3\"></span>ours <em>here</em></p>\n\
             </div>\n\
             <div class=\"conflict-incoming\">\
             <div class=\"conflict-label\"><span data-line=\"4\"></span>feature/&lt;x></div>\n\
             <h1 id=\"theirs\"><span data-line=\"5\"></span>Theirs</h1>\n\
             <ul>\n<li><span data-line=\"7\"></span>item</li>\n</ul>\n\
             </div>\n\
             </div>\n\
             <p><span data-line=\"9\"></span>after</p>\n"
        );
    }

    /// A diff3 block's base sits between the two, and a side that deleted every line is an empty
    /// box under its label.
    #[test]
    fn html_shows_a_diff3_base_between_the_sides() {
        let h = to_html("<<<<<<< HEAD\nours\n||||||| base\n## Base\n=======\n>>>>>>> side\n");
        let order: Vec<usize> = ["conflict-current", "conflict-base", "conflict-incoming"]
            .iter()
            .map(|class| h.find(&format!("<div class=\"{class}\">")).expect(class))
            .collect();
        assert!(order.is_sorted(), "{h}");
        assert!(
            h.contains("<span data-line=\"3\"></span>base</div>\n<h2 id=\"base\">"),
            "{h}"
        );
        assert!(h.contains("side</div>\n</div>\n</div>"), "{h}");
        assert_eq!(lines(&h), ["1", "2", "3", "4", "5"]);
        for marker in ["<<<", "|||", "===", "&gt;&gt;", "blockquote"] {
            assert!(!h.contains(marker), "{marker} in {h}");
        }
    }

    /// Each stretch of a conflicted note is parsed with the whole note's definitions, so a
    /// reference link and a footnote find theirs across a block's edge, and the footnote is shown
    /// once, where it is written.
    #[test]
    fn html_resolves_definitions_across_a_conflict_block() {
        let src = "See [the docs][d] and a note[^n].\n\
                   <<<<<<< HEAD\n[^n]: The note.\n=======\n[d] too\n>>>>>>> side\n\n\
                   [d]: docs.md\n";
        let h = bare(src);
        assert_eq!(h.matches("<a href=\"docs.md\">").count(), 2, "{h}");
        assert!(
            h.contains("<sup class=\"footnote-reference\"><a href=\"#n\">1</a>"),
            "{h}"
        );
        assert_eq!(h.matches("class=\"footnote-definition\"").count(), 1, "{h}");
        assert!(!h.contains('['), "{h}");
    }

    /// The preview markers are tested on their own; strip them so the older assertions stay
    /// about the HTML the renderer produces.
    fn bare(text: &str) -> String {
        let mut out = to_html(text);
        while let Some(i) = out.find("<span data-line=\"") {
            let j = out[i..].find("></span>").unwrap() + i + "></span>".len();
            out.replace_range(i..j, "");
        }
        out
    }

    #[test]
    fn html_marks_block_source_lines() {
        let src = "# Title\n\nFirst para.\n\nSecond para.\n\n- item\n\n```rs\ncode\n```\n";
        let h = to_html(src);
        assert_eq!(lines(&h), ["1", "3", "5", "7", "9"], "{h}");
        assert!(
            h.contains("<h1 id=\"title\"><span data-line=\"1\"></span>Title</h1>"),
            "{h}"
        );
        assert!(
            h.contains("<p><span data-line=\"5\"></span>Second para.</p>"),
            "{h}"
        );
    }

    /// A note of block ids renders in time linear in its length: four times the paragraphs take
    /// about four times as long, where looking every id up afresh per parser event took sixteen.
    #[test]
    #[ignore = "timing-sensitive; run with --release"]
    fn html_of_block_ids_scales_linearly() {
        let ms = |paragraphs: usize| {
            let note: String = (0..paragraphs)
                .map(|i| format!("Paragraph {i} with some prose in it. ^block-{i}\n\n"))
                .collect();
            (0..3)
                .map(|_| {
                    let t = std::time::Instant::now();
                    to_html(&note);
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .fold(f64::MAX, f64::min)
        };
        let (small, large) = (ms(6_000), ms(24_000));
        assert!(large < 8.0 * small, "6k: {small:.1} ms, 24k: {large:.1} ms");
    }
}
