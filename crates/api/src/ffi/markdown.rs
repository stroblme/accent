//! Core Markdown analysis and rendering for Android. Convert output byte
//! ranges to Kotlin UTF-16 units. A note's path picks its syntax, an `.org` file's org.

use accent_core::markdown::{self, Click, Style};
use accent_core::note::Syntax;
use accent_core::{org, path};

use crate::ffi::convert::{Analysis, Heading, Link, PdfAnchor, Span, Tag, TextEdit, Utf16};

/// Parse PDF anchors such as `page=3&selection=0,4,1,9` into a zero-based
/// page and selection. `None` distinguishes heading and block anchors.
#[uniffi::export]
pub fn pdf_anchor(anchor: String) -> Option<PdfAnchor> {
    let (page, selection) = markdown::pdf_anchor(&anchor)?;
    Some(PdfAnchor {
        page: page as u32,
        selection: selection.map(|s| s.iter().map(|n| *n as u32).collect()),
    })
}

/// Resolve a heading or block anchor to the ID rendered in the HTML of the note at `rel`.
#[uniffi::export]
pub fn anchor_id(text: String, anchor: String, rel: String) -> Option<String> {
    Syntax::of(&rel).anchor_id(&text, &anchor)
}

/// Render the desktop-compatible HTML fragment: `accent://open/…` links,
/// `accent://tag/…` tags, `accent://file/…` embeds, MathML, block `data-line`
/// markers and heading slugs.
#[uniffi::export]
pub fn to_html(text: String, rel: String) -> String {
    Syntax::of(&rel).to_html(&text)
}

/// Where a click on a link in a rendered note goes, from the page loaded at `page`
/// ([`base_uri`]): the desktop preview's rules, so a link goes the same way on either.
#[uniffi::export]
pub fn click(uri: String, page: String) -> Click {
    markdown::click(&uri, &page)
}

/// What a tap on the box of the task on line `line` of the note at `rel` writes, as the desktop's
/// click does ([`Syntax::click_box`]): its tick's range and the tick it becomes.
#[uniffi::export]
pub fn click_box(text: String, rel: String, line: u32) -> Option<TextEdit> {
    let (at, tick) = Syntax::of(&rel).click_box(&text, line)?;
    Some(TextEdit {
        range: Utf16::new(&text).range(&(at..at + 1)),
        text: tick.to_string(),
    })
}

/// What a paste of `html`, copied beside its plain text by another app, writes at UTF-16 offset
/// `at` of `text`, the note at `rel`, as the desktop's formatted paste does: its Markdown, or org
/// in an org note. `None` for the plain text, which goes in where the HTML marks nothing worth
/// keeping, in a file that is no note, and in a code block, the front matter or a table row.
#[uniffi::export]
pub fn formatted_paste(text: String, rel: String, at: u32, html: String) -> Option<String> {
    let syntax = Syntax::of(&rel);
    if syntax == Syntax::Markdown && !path::is_note(&rel) {
        return None;
    }
    let at = Utf16::new(&text).byte(at);
    let verbatim = (syntax.analyze(&text).spans.iter())
        .any(|s| matches!(s.style, Style::CodeBlock | Style::Frontmatter) && s.range.contains(&at));
    let lines: Vec<&str> = text.split('\n').collect();
    let line = text[..at].matches('\n').count();
    let table = match syntax {
        Syntax::Markdown => markdown::in_table(&lines, line),
        Syntax::Org => lines[line].trim_start().starts_with('|'),
    };
    match syntax {
        _ if verbatim || table => None,
        Syntax::Markdown => markdown::from_html(&html),
        Syntax::Org => org::from_html(&html),
    }
}

/// The vault path an `accent://file/` request names, decoded; `None` for any other address.
#[uniffi::export]
pub fn accent_file(uri: String) -> Option<String> {
    markdown::accent_file(&uri)
}

/// Where relative links in `rel`'s markdown resolve from: the note's own folder, encoded.
#[uniffi::export]
pub fn base_uri(rel: String) -> String {
    markdown::base_uri(&rel)
}

/// The styling spans, links, tags and headings of the note at `rel`, with every range in UTF-16
/// code units.
#[uniffi::export]
pub fn analyze_utf16(text: String, rel: String) -> Analysis {
    let a = Syntax::of(&rel).analyze(&text);
    let map = Utf16::new(&text);
    Analysis {
        spans: a
            .spans
            .into_iter()
            .map(|s| Span {
                range: map.range(&s.range),
                style: s.style,
            })
            .collect(),
        links: a
            .links
            .into_iter()
            .map(|l| Link {
                range: map.range(&l.range),
                kind: l.kind,
                target: l.target,
                anchor: l.anchor,
                alias: l.alias,
            })
            .collect(),
        tags: a
            .tags
            .into_iter()
            .map(|t| Tag {
                range: map.range(&t.range),
                name: t.name,
            })
            .collect(),
        headings: a
            .headings
            .into_iter()
            .map(|h| Heading {
                range: map.range(&h.range),
                level: h.level,
                text: h.text,
            })
            .collect(),
        title: a.title,
        frontmatter: a.frontmatter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify UTF-16 styling spans land on the same headings as core byte spans.
    #[test]
    fn a_heading_after_an_emoji_is_measured_in_utf16_units() {
        let text = "é🙂\n\n# Heading\n";
        let a = analyze_utf16(text.to_string(), "n.md".into());
        let heading = a
            .spans
            .iter()
            .find(|s| matches!(s.style, Style::Heading(1)))
            .expect("the heading was not styled");
        // Bytes: "é" 2 + "🙂" 4 + "\n\n" 2 = 8. Units: 1 + 2 + 2 = 5.
        assert_eq!(heading.range.start, 5);
        assert_eq!(heading.range.end, 5 + "# Heading".len() as u32);
        assert_eq!(a.title.as_deref(), Some("Heading"));

        // And the same string with nothing above ASCII in it is unchanged by the conversion.
        let plain = analyze_utf16("# Heading\n".to_string(), "n.md".into());
        let h = plain
            .spans
            .iter()
            .find(|s| matches!(s.style, Style::Heading(1)))
            .unwrap();
        assert_eq!((h.range.start, h.range.end), (0, 9));
    }

    /// Links cross with their ranges converted too, since the editor underlines them where the
    /// spans say and the view opens what the target names.
    #[test]
    fn a_wikilink_after_an_emoji_keeps_its_target_and_moves_its_range() {
        let a = analyze_utf16("🙂 [[Other Note|alias]]\n".to_string(), "n.md".into());
        let link = a.links.first().expect("no link");
        assert_eq!(link.target, "Other Note");
        assert_eq!(link.alias.as_deref(), Some("alias"));
        assert_eq!(link.range.start, 3, "two units for the emoji and a space");
    }

    #[test]
    fn a_pdf_anchor_is_a_page_and_its_numbers() {
        let at = pdf_anchor("page=3&selection=1,2,3,4".to_string()).unwrap();
        assert_eq!((at.page, at.selection), (2, Some(vec![1, 2, 3, 4])));
        assert!(
            pdf_anchor("page=3".to_string())
                .unwrap()
                .selection
                .is_none()
        );
        assert!(pdf_anchor("A Heading".to_string()).is_none());
    }

    #[test]
    fn a_note_anchor_resolves_to_the_rendered_id() {
        let note = "# Intro\n# Intro\nParagraph ^Block\n".to_string();
        let html = to_html(note.clone(), "n.md".into());
        for (anchor, id) in [("intro-1", "intro-1"), ("^block", "^Block")] {
            let found = anchor_id(note.clone(), anchor.into(), "n.md".into());
            assert_eq!(found.as_deref(), Some(id));
            assert!(html.contains(&format!("id=\"{id}\"")), "{html}");
        }
    }

    /// The tick's range is in UTF-16 units, past an emoji too, and an org note ticks `X`, its
    /// `[-]` too.
    #[test]
    fn a_tapped_box_is_its_ticks_range_in_utf16() {
        let edit = click_box("🙂\n- [ ] a\n".to_string(), "n.md".into(), 2).unwrap();
        assert_eq!(
            (edit.range.start, edit.range.end, edit.text.as_str()),
            (6, 7, "x")
        );
        let edit = click_box("- [-] a\n".to_string(), "n.org".into(), 1).unwrap();
        assert_eq!((edit.range.start, edit.text.as_str()), (3, "X"));
        assert!(click_box("- a\n".to_string(), "n.md".into(), 1).is_none());
    }

    /// A formatted paste writes the note's own syntax, and leaves the plain text to a code block,
    /// the front matter, a table row and a file that is no note, its offset counted in UTF-16.
    #[test]
    fn a_formatted_paste_is_the_notes_syntax_outside_verbatim_text() {
        let paste = |text: &str, rel: &str, at: u32| {
            formatted_paste(
                text.into(),
                rel.into(),
                at,
                "<p><b>bold</b> text</p>".into(),
            )
        };
        assert_eq!(paste("", "n.md", 0).as_deref(), Some("**bold** text"));
        assert_eq!(paste("", "n.org", 0).as_deref(), Some("*bold* text"));
        assert_eq!(paste("", "n.py", 0), None);
        // Unit 13 is the code's first letter, byte 21: byte 13 would be among the emoji.
        let fenced = "🙂🙂🙂🙂\n```\ncode\n```\n";
        assert_eq!(paste(fenced, "n.md", 13), None);
        assert!(paste(fenced, "n.md", fenced.encode_utf16().count() as u32).is_some());
        assert_eq!(paste("---\ntitle: x\n---\n", "n.md", 5), None);
        assert_eq!(paste("| a | b |\n|---|---|\n| c | d |\n", "n.md", 2), None);
        assert_eq!(paste("| a |\n", "n.org", 2), None);
        let plain = formatted_paste(String::new(), "n.md".into(), 0, "plain".into());
        assert_eq!(plain, None);
    }

    /// An org file renders and reads as org, by its path.
    #[test]
    fn the_html_is_the_fragment_the_preview_gets() {
        let html = to_html("# Title\n\n[[Note]]\n".to_string(), "n.md".into());
        assert!(html.contains("accent://open/Note"), "{html}");
        assert!(html.contains("data-line="), "{html}");
        let org = "* Title\n".to_string();
        let html = to_html(org.clone(), "n.org".into());
        assert!(html.contains("<h1 id=\"title\">"), "{html}");
        assert_eq!(analyze_utf16(org.clone(), "n.org".into()).headings.len(), 1);
        let id = anchor_id(org, "Title".into(), "n.org".into());
        assert_eq!(id.as_deref(), Some("title"));
    }
}
