//! The preview pane's HTML: wikilinks become `accent://` anchors, math becomes MathML, and every
//! block carries the source line it starts on.

use super::links::{is_image, percent_encode, slugs, split_anchor};
use super::options;
use pulldown_cmark::{Event, LinkType, Parser, Tag as Cm, TagEnd};

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

/// A `$…$` or `$$…$$` formula as MathML, or `None` if the LaTeX does not parse.
///
/// The renderer itself never reports a failure — it writes `<merror>` and carries on — so the
/// parser events are collected first, and that is what decides between MathML and the caller's
/// raw-source fallback. WebKit draws MathML natively, so no stylesheet or script goes with it.
fn mathml(src: &str, display: bool) -> Option<String> {
    use pulldown_latex::config::DisplayMode;
    use pulldown_latex::{ParserError, RenderConfig, Storage};

    let storage = Storage::new();
    let events = pulldown_latex::Parser::new(src, &storage)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
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

/// Render a note to an HTML fragment for the preview pane (wikilinks become `<a href="accent://…">`).
///
/// Each block opens with an empty `<span data-line="N">`, so the preview can scroll to the line
/// the editor's cursor is on, and each heading gets its [`slugs`] anchor as its `id`, so an
/// in-note `[text](#slug)` scrolls there.
pub fn to_html(text: &str) -> String {
    let mut evts: Vec<Event> = Vec::new();
    let mut link_wiki: Vec<bool> = Vec::new();
    let mut image_wiki: Vec<bool> = Vec::new();
    let mut skip = 0usize;
    // Block starts arrive in source order, so one forward pass over the newlines suffices.
    let mut counted = 0usize;
    let mut line = 1usize;
    // Each heading's place in `evts` and its text, gathered the way `analyze` gathers
    // `Heading::text` — every text and code event inside, an embed's too — so the `id` is the
    // anchor the editor resolves.
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut in_heading = false;

    for (ev, r) in Parser::new_ext(text, options()).into_offset_iter() {
        match &ev {
            Event::Start(Cm::Heading { .. }) => {
                in_heading = true;
                headings.push((evts.len(), String::new()));
            }
            Event::End(TagEnd::Heading(_)) => in_heading = false,
            Event::Text(t) | Event::Code(t) if in_heading => {
                if let Some((_, h)) = headings.last_mut() {
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
            line += text[counted..r.start]
                .bytes()
                .filter(|b| *b == b'\n')
                .count();
            counted = r.start;
            Event::Html(format!("<span data-line=\"{line}\"></span>").into())
        });
        match ev {
            Event::Start(Cm::Link {
                link_type: LinkType::WikiLink { .. },
                ref dest_url,
                ..
            }) => {
                let (t, anchor) = split_anchor(dest_url);
                evts.push(Event::Html(
                    format!("<a href=\"{}\" class=\"wikilink\">", open_href(t, anchor)).into(),
                ));
                link_wiki.push(true);
            }
            Event::Start(Cm::Link { .. }) => {
                link_wiki.push(false);
                evts.push(ev);
            }
            Event::End(TagEnd::Link) => {
                if link_wiki.pop().unwrap_or(false) {
                    evts.push(Event::Html("</a>".into()));
                } else {
                    evts.push(ev);
                }
            }
            Event::Start(Cm::Image {
                link_type: LinkType::WikiLink { .. },
                ref dest_url,
                ..
            }) => {
                let (t, anchor) = split_anchor(dest_url);
                if is_image(t) {
                    evts.push(Event::Html(
                        format!("<img src=\"accent://file/{}\">", percent_encode(t)).into(),
                    ));
                    skip = 1;
                } else {
                    evts.push(Event::Html(
                        format!("<a href=\"{}\" class=\"embed\">", open_href(t, anchor)).into(),
                    ));
                    image_wiki.push(true);
                }
            }
            Event::Start(Cm::Image { .. }) => {
                image_wiki.push(false);
                evts.push(ev);
            }
            Event::End(TagEnd::Image) => {
                if image_wiki.pop().unwrap_or(false) {
                    evts.push(Event::Html("</a>".into()));
                } else {
                    evts.push(ev);
                }
            }
            Event::InlineMath(ref src) | Event::DisplayMath(ref src) => {
                let html = mathml(src, matches!(ev, Event::DisplayMath(_)));
                // A typo must never blank a formula: without MathML the original event goes on and
                // pulldown-cmark's `.math` span shows the source as the author wrote it.
                match html {
                    Some(html) => evts.push(Event::Html(html.into())),
                    None => evts.push(ev),
                }
            }
            // The class GitHub uses, so the preview can draw the checkbox in place of the bullet.
            // The marker is the first thing in its item, so the nearest item start is its own.
            Event::TaskListMarker(_) => {
                if let Some(li) = evts
                    .iter()
                    .rposition(|e| matches!(e, Event::Start(Cm::Item)))
                {
                    evts[li] = Event::Html("<li class=\"task-list-item\">".into());
                }
                evts.push(ev);
            }
            _ => evts.push(ev),
        }
        evts.extend(marker);
    }

    let ids = slugs(headings.iter().map(|(_, h)| h.as_str()));
    for ((at, _), slug) in headings.iter().zip(ids) {
        if let Event::Start(Cm::Heading { id, .. }) = &mut evts[*at] {
            *id = Some(slug.into());
        }
    }

    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, evts.into_iter());
    out
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
        let lines: Vec<&str> = h
            .match_indices("<span data-line=\"")
            .map(|(i, m)| {
                let rest = &h[i + m.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        assert_eq!(lines, ["1", "3", "5", "7", "9"], "{h}");
        assert!(
            h.contains("<h1 id=\"title\"><span data-line=\"1\"></span>Title</h1>"),
            "{h}"
        );
        assert!(
            h.contains("<p><span data-line=\"5\"></span>Second para.</p>"),
            "{h}"
        );
    }
}
