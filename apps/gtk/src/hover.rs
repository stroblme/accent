//! What a language server says about the symbol under the pointer, and what is wrong with it.
//!
//! One label: the server's own answer, then every diagnostic covering that position. All four
//! severities, because the two quiet ones say nothing on screen — a dangling `[[wikilink]]` draws
//! a dim underline and nothing else, and the hover is where its message is read.
//!
//! Servers answer in markdown and GtkSourceView's hover shows a `GtkLabel`, which speaks Pango
//! markup and nothing else. The two overlap in bold, italic and monospace and in nothing more, so
//! everything else is flattened rather than approximated: a heading is a bold line, a link is its
//! own text, a list loses its bullets. A hover is three lines of a signature and a sentence about
//! it, not a document.

use crate::editor::Tab;
use gtk::glib;
use gtk::subclass::prelude::*;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use sourceview5::prelude::*;
use std::rc::Rc;

/// How wide a hover or a signature is let grow before it wraps. A signature is the longest thing
/// in either and 80 characters is where one stops being read left to right.
pub const WIDTH: i32 = 80;

/// `md` as Pango markup. Every run of text is escaped, so a C++ signature full of `<` and `&`
/// cannot turn the label into a parse error and blank the hover.
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

/// The severities as the hover names them, which is the only place three of the four are named
/// at all.
fn severity_label(severity: accent_api::Severity) -> &'static str {
    match severity {
        accent_api::Severity::Error => "Error",
        accent_api::Severity::Warning => "Warning",
        accent_api::Severity::Info => "Information",
        accent_api::Severity::Hint => "Hint",
    }
}

/// One diagnostic as a line of markup: what it is, what it says, and who said it.
fn diagnostic_markup(item: &accent_api::Diagnostic) -> String {
    let mut line = format!(
        "<b>{}</b>: {}",
        severity_label(item.severity),
        glib::markup_escape_text(&item.message)
    );
    if let Some(source) = &item.source {
        line.push_str(&format!(
            " <span alpha=\"60%\">({})</span>",
            glib::markup_escape_text(source)
        ));
    }
    line
}

// ------------------------------------------------------------------------------------ provider

mod provider_imp {
    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Weak;

    use gtk::subclass::prelude::*;
    use gtk::{glib, pango};

    use sourceview5::subclass::prelude::*;
    use sourceview5::{HoverContext, HoverDisplay};

    use super::{WIDTH, diagnostic_markup, markup_of};
    use crate::editor::Tab;
    use crate::{diagnostics, lang};

    #[derive(Default)]
    pub struct Provider {
        /// Weak for the reason the completion provider's is: the view holds the provider and the
        /// tab holds the view.
        pub tab: RefCell<Weak<Tab>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Provider {
        const NAME: &'static str = "AccentHoverProvider";
        type Type = super::Provider;
        type Interfaces = (sourceview5::HoverProvider,);
    }

    impl ObjectImpl for Provider {}

    impl HoverProviderImpl for Provider {
        fn populate_future(
            &self,
            context: &HoverContext,
            display: &HoverDisplay,
        ) -> Pin<Box<dyn Future<Output = Result<(), glib::Error>>>> {
            let tab = self.tab.borrow().upgrade();
            let (context, display) = (context.clone(), display.clone());
            Box::pin(async move {
                let Some(tab) = tab else { return Ok(()) };
                let Some((start, _)) = context.bounds() else {
                    return Ok(());
                };
                let pos = lang::pos_of(&start);
                let Some(vault) = tab.lang.vault() else {
                    return Ok(());
                };
                lang::flush(tab.clone()).await;
                let answer = vault.hover(&tab.rel(), pos).await.ok().flatten();
                let items = tab.diagnostics().clone();
                let here = diagnostics::at(&items, pos);
                tracing::debug!(
                    "hover for {} at {pos:?}: answer {}, {} diagnostics",
                    tab.rel(),
                    answer.is_some(),
                    here.len()
                );
                if answer.is_none() && here.is_empty() {
                    // Nothing appended: an empty display is what stops the assistant showing at
                    // all, which is not the same as showing an empty box.
                    return Ok(());
                }
                let mut markup = answer.map(|h| markup_of(&h.text)).unwrap_or_default();
                for item in here {
                    if !markup.is_empty() {
                        markup.push('\n');
                    }
                    markup.push_str(&diagnostic_markup(item));
                }
                let label = gtk::Label::builder()
                    .use_markup(true)
                    .wrap(true)
                    .wrap_mode(pango::WrapMode::WordChar)
                    .max_width_chars(WIDTH)
                    .xalign(0.0)
                    .build();
                label.set_markup(&markup);
                display.append(&label);
                Ok(())
            })
        }
    }
}

glib::wrapper! {
    pub struct Provider(ObjectSubclass<provider_imp::Provider>)
        @implements sourceview5::HoverProvider;
}

/// Attach the provider to a tab's view. Called by [`crate::lang::attach`].
pub fn install(tab: &Rc<Tab>) {
    let provider: Provider = glib::Object::new();
    *provider.imp().tab.borrow_mut() = Rc::downgrade(tab);
    tab.view.hover().add_provider(&provider);
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
