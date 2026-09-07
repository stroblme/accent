//! What a language server says about the symbol under the pointer.
//!
//! Only the markdown-to-Pango conversion lives here so far: the `HoverProvider` that shows it
//! lands beside it.
//!
//! Servers answer in markdown and GtkSourceView's hover shows a `GtkLabel`, which speaks Pango
//! markup and nothing else. The two overlap in bold, italic and monospace and in nothing more, so
//! everything else is flattened rather than approximated: a heading is a bold line, a link is its
//! own text, a list loses its bullets. A hover is three lines of a signature and a sentence about
//! it, not a document.

use gtk::glib;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// `md` as Pango markup. Every run of text is escaped, so a C++ signature full of `<` and `&`
/// cannot turn the label into a parse error and blank the hover.
// The hover provider is the only caller and arrives with the rest of this module.
#[allow(dead_code)]
pub fn markup_of(md: &str) -> String {
    let mut out = String::new();
    let escaped = |out: &mut String, text: &str| out.push_str(&glib::markup_escape_text(text));

    for event in Parser::new(md) {
        match event {
            Event::Text(text) => escaped(&mut out, &text),
            // Inline code and a fenced block are the same thing to a label: monospace. The block
            // keeps its own newlines, which is what makes it a block.
            Event::Code(text) => {
                out.push_str("<tt>");
                escaped(&mut out, &text);
                out.push_str("</tt>");
            }
            Event::Start(Tag::CodeBlock(_)) => out.push_str("<tt>"),
            Event::End(TagEnd::CodeBlock) => out.push_str("</tt>\n"),
            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            // A heading is a bold line: the label has one text size and nothing to grade.
            Event::Start(Tag::Heading { .. }) => out.push_str("<b>"),
            Event::End(TagEnd::Heading(_)) => out.push_str("</b>\n"),
            // The blank line between paragraphs is the only structure a label can show.
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),
            Event::SoftBreak | Event::HardBreak | Event::Rule => out.push('\n'),
            // Links keep their text and lose their target; lists lose their bullets; raw HTML,
            // images, tables and footnotes are dropped whole.
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_markup_becomes_pango_and_the_rest_is_escaped() {
        assert_eq!(
            markup_of("**fn** `add<T>(a: T)` *twice*"),
            "<b>fn</b> <tt>add&lt;T&gt;(a: T)</tt> <i>twice</i>"
        );
    }

    #[test]
    fn a_fenced_block_keeps_its_lines_and_a_heading_is_one_bold_one() {
        assert_eq!(
            markup_of("# Title\n\n```rust\nfn a() {}\nfn b() {}\n```"),
            "<b>Title</b>\n<tt>fn a() {}\nfn b() {}\n</tt>"
        );
    }

    #[test]
    fn a_link_is_its_text_and_paragraphs_keep_one_blank_line() {
        assert_eq!(
            markup_of("See [the docs](https://example.com).\n\nSecond & last."),
            "See the docs.\n\nSecond &amp; last."
        );
    }
}
