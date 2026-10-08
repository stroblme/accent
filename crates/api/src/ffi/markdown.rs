//! Markdown for the editor and the rendered view.
//!
//! Both call straight into the core. The only thing this layer adds is the offset conversion:
//! the core reports byte ranges into the Rust string, and Kotlin indexes UTF-16 units.

use accent_core::markdown;

use crate::ffi::convert::{Analysis, Heading, Link, PdfAnchor, Span, Tag, Utf16};

/// What a link's anchor names inside a PDF — `page=3&selection=0,4,1,9` — or `None` for a
/// heading or a block, which is what tells a link into a PDF apart. The page is zero-based.
#[uniffi::export]
pub fn pdf_anchor(anchor: String) -> Option<PdfAnchor> {
    let (page, selection) = markdown::pdf_anchor(&anchor)?;
    Some(PdfAnchor {
        page: page as u32,
        selection: selection.map(|s| s.iter().map(|n| *n as u32).collect()),
    })
}

/// The note rendered as an HTML fragment, exactly as the desktop preview gets it: wikilinks as
/// `accent://open/…`, tags as `accent://tag/…`, embeds as `accent://file/…`, maths as MathML, a
/// `data-line` marker before every block and a slug on every heading.
#[uniffi::export]
pub fn to_html(text: String) -> String {
    markdown::to_html(&text)
}

/// The note's styling spans, links, tags and headings, with every range in UTF-16 code units.
#[uniffi::export]
pub fn analyze_utf16(text: String) -> Analysis {
    let a = markdown::analyze(&text);
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
    use accent_core::markdown::Style;

    /// The whole point of `analyze_utf16`: the same heading measured in the units the other side
    /// counts, so a span applied to the Kotlin string lands on the heading and not beside it.
    #[test]
    fn a_heading_after_an_emoji_is_measured_in_utf16_units() {
        let text = "é🙂\n\n# Heading\n";
        let a = analyze_utf16(text.to_string());
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
        let plain = analyze_utf16("# Heading\n".to_string());
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
        let a = analyze_utf16("🙂 [[Other Note|alias]]\n".to_string());
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
    fn the_html_is_the_fragment_the_preview_gets() {
        let html = to_html("# Title\n\n[[Note]]\n".to_string());
        assert!(html.contains("accent://open/Note"), "{html}");
        assert!(html.contains("data-line="), "{html}");
    }
}
