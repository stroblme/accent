//! Paste a single address over selected note text as `[selection](address)`, or org's
//! `[[address][selection]]` in an org note, and formatted text another application copied as the
//! Markdown it reads as. `Ctrl+Shift+V` bypasses both (DESIGN.md, Editing).

use super::{Flavour, Syntax, Tab};
use crate::multicaret;
use gtk::glib;
use gtk::prelude::*;

/// What a formatted paste makes of clipboard HTML: the Markdown it writes, or `None` for the
/// clipboard's plain text.
pub(crate) type Convert = fn(&str) -> Option<String>;

/// What VS Code copies beside its HTML, whose styled spans are code, not formatting.
const VSCODE: &str = "vscode-editor-data";

/// How a view of `flavour` converts a formatted paste: a markdown note or cell into its Markdown.
/// Org, code and tables take the plain text.
pub(crate) fn converter(flavour: Flavour) -> Option<Convert> {
    (flavour == Flavour::Note(Syntax::Markdown))
        .then_some(accent_core::markdown::from_html as Convert)
}

/// A diagram label's formatted paste: the Markdown its editor keeps (`accent_drawio::label`),
/// bold, italic, underline, bullets and line breaks. `None`, for the plain text, where the copy
/// marks none of those, or holds a table or code, whose cells and lines a label runs together.
pub(crate) fn label_markdown(html: &str) -> Option<String> {
    use accent_drawio::label::{Run, html_to_runs, runs_to_markdown};
    let copied = accent_core::markdown::formatted(html)?;
    let lower = copied.to_ascii_lowercase();
    if lower.contains("<table") || lower.contains("<pre") {
        return None;
    }
    let runs = html_to_runs(copied, true);
    let shown = runs.iter().any(|run| match run {
        Run::Bullet => true,
        Run::Text { text, marks } => {
            !text.trim().is_empty() && (marks.bold || marks.italic || marks.underline)
        }
        _ => false,
    });
    shown.then(|| runs_to_markdown(&runs))
}

/// Return a link for an eligible `selection` and single trimmed clipboard
/// address, or `None` for ordinary paste in this `flavour`.
pub(super) fn link(clipboard: &str, selection: &str, flavour: Flavour) -> Option<String> {
    let url = clipboard.trim();
    (could_link(selection, flavour) && is_url(url)).then(|| match flavour {
        Flavour::Note(Syntax::Org) => accent_core::org::link(url, Some(selection)),
        _ => format!(
            "[{}]({})",
            link_text(selection),
            accent_core::markdown::link_destination(url)
        ),
    })
}

/// Escape link-text brackets and trailing backslashes that could close
/// the link incorrectly. Preserve existing Markdown escapes and code spans.
fn link_text(selection: &str) -> String {
    let mut out = String::with_capacity(selection.len());
    let mut rest = selection;
    // How many backslashes run up to here: an odd count escapes the next character.
    let mut slashes = 0;
    while let Some(c) = rest.chars().next() {
        if c == '`' && slashes % 2 == 0 {
            let taken = code_span(rest);
            out.push_str(&rest[..taken]);
            rest = &rest[taken..];
            slashes = 0;
            continue;
        }
        if matches!(c, '[' | ']') && slashes % 2 == 0 {
            out.push('\\');
        }
        slashes = if c == '\\' { slashes + 1 } else { 0 };
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if slashes % 2 == 1 {
        out.push('\\');
    }
    out
}

/// Find the matching backtick run closing `rest`'s code span. Without one,
/// return the opening run length because it is plain text.
fn code_span(rest: &str) -> usize {
    let run = rest.len() - rest.trim_start_matches('`').len();
    let mut at = run;
    while let Some(found) = rest[at..].find('`') {
        let start = at + found;
        let len = rest[start..].len() - rest[start..].trim_start_matches('`').len();
        if len == run {
            return start + len;
        }
        at = start + len;
    }
    run
}

/// Check clipboard-independent eligibility: prose with a non-address,
/// nonempty single-line selection.
fn could_link(selection: &str, flavour: Flavour) -> bool {
    flavour.is_note()
        && !selection.contains('\n')
        && !selection.trim().is_empty()
        && !is_url(selection.trim())
}

/// One web or mail address and nothing else. Balanced parentheses in it are fine: CommonMark
/// allows them in a link destination, and they are what an address has in practice.
fn is_url(text: &str) -> bool {
    !text.contains(char::is_whitespace)
        && ["http://", "https://", "mailto:"].iter().any(|scheme| {
            text.len() > scheme.len()
                && text
                    .get(..scheme.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
        })
}

/// Take the paste a note, a cell or a label editor does its own way: a URL over a selection
/// [`link`]s it, and formatted text `convert` reads becomes its Markdown, outside a code block,
/// the front matter and a table, where the plain text goes in. Each is one undo step.
///
/// Anything else is GTK's paste: a column of carets, a view not editable, and a clipboard with no
/// text, whose image is filed as an attachment (`App::wire_attachments`) by a handler connected
/// after this one. Answers the handler's id, which [`Tab::paste_verbatim`] holds back.
pub(super) fn formatted_paste(
    view: &sourceview5::View,
    flavour: Flavour,
    convert: Option<Convert>,
) -> glib::SignalHandlerId {
    view.connect_paste_clipboard(move |view| {
        let buffer = view.buffer();
        let column = view
            .downcast_ref::<multicaret::View>()
            .is_some_and(|v| v.has_carets());
        let clipboard = view.clipboard();
        let formats = clipboard.formats();
        if column || !view.is_editable() || !formats.contains_type(glib::Type::STRING) {
            return;
        }
        let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
            let caret = buffer.iter_at_mark(&buffer.get_insert());
            (caret, caret)
        });
        let selection = buffer.text(&start, &end, true);
        let linking = could_link(&selection, flavour);
        let convert = convert.filter(|_| {
            formats.contain_mime_type(crate::clipboard::HTML)
                && !formats.contain_mime_type(VSCODE)
                && !verbatim(&buffer, &start)
        });
        if !linking && convert.is_none() {
            return;
        }
        view.stop_signal_emission_by_name("paste-clipboard");
        // The reads are asynchronous even from this process; the marks ride out an edit meanwhile.
        let (from, to) = (
            buffer.create_mark(None, &start, true),
            buffer.create_mark(None, &end, false),
        );
        let view = view.downgrade();
        glib::spawn_future_local(async move {
            let plain = clipboard.read_text_future().await.ok().flatten();
            let linked = (plain.as_deref())
                .filter(|_| linking)
                .and_then(|plain| link(plain, &selection, flavour));
            let converted = match (&linked, convert) {
                (None, Some(convert)) => crate::clipboard::html(&clipboard)
                    .await
                    .as_deref()
                    .and_then(convert),
                _ => None,
            };
            let Some(view) = view.upgrade() else { return };
            let buffer = view.buffer();
            let (mut start, mut end) = (buffer.iter_at_mark(&from), buffer.iter_at_mark(&to));
            buffer.delete_mark(&from);
            buffer.delete_mark(&to);
            let Some(text) = linked.or(converted).or(plain.map(String::from)) else {
                return;
            };
            // One undo step, the link, the Markdown or the paste.
            buffer.begin_user_action();
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &text);
            buffer.end_user_action();
            view.scroll_mark_onscreen(&buffer.get_insert());
        });
    })
}

/// Whether `at` is in a code block, the front matter or a table row, where Markdown is text.
fn verbatim(buffer: &gtk::TextBuffer, at: &gtk::TextIter) -> bool {
    if crate::typing::verbatim(buffer, at) {
        return true;
    }
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    let lines: Vec<&str> = text.split('\n').collect();
    accent_core::markdown::in_table(&lines, at.line() as usize)
}

/// Paste verbatim at all of `view`'s carets using GTK, suppressing its [`formatted_paste`]
/// handler `link` for this emission (`Ctrl+Shift+V`).
pub(crate) fn paste_verbatim(view: &sourceview5::View, link: &glib::SignalHandlerId) {
    view.block_signal(link);
    view.emit_paste_clipboard();
    view.unblock_signal(link);
}

impl Tab {
    /// [`paste_verbatim`] in this tab.
    pub fn paste_verbatim(&self) {
        paste_verbatim(&self.view, &self.paste_link);
    }
}

#[cfg(test)]
mod tests {
    use super::{Flavour, Syntax, label_markdown, link};

    /// A label keeps bold, italic, underline, bullets and lines, and pastes the plain text of a
    /// copy that marks none of them or holds a table, whose cells it would run together.
    #[test]
    fn a_label_takes_what_it_can_show() {
        assert_eq!(
            label_markdown("<h2>Title</h2><ul><li>one <i>it</i></li></ul>").as_deref(),
            Some("**Title**\n- one *it*")
        );
        for html in [
            "<table><tr><th>Tool</th><th>Time</th></tr><tr><td><b>rg</b></td><td>1s</td></tr></table>",
            "<p><img src='https://a.org/x.png' alt=''></p>",
            "<p>see <a href='https://a.org/'>the page</a></p>",
            "<p>text <code>x</code></p>",
        ] {
            assert_eq!(label_markdown(html), None, "{html}");
        }
    }

    #[test]
    fn a_url_pasted_over_prose_links_it() {
        let note = Flavour::Note(Syntax::Markdown);
        let linked = |clip, sel| link(clip, sel, note);
        assert_eq!(
            linked(" https://example.org/a_(b)\n", "the page").as_deref(),
            Some("[the page](https://example.org/a_(b))"),
            "trimmed, parentheses kept"
        );
        assert_eq!(
            linked("mailto:me@example.org", "me").as_deref(),
            Some("[me](mailto:me@example.org)")
        );
        assert!(linked("HTTP://example.org", "x").is_some(), "any case");
        // Everything else is the ordinary paste.
        assert_eq!(
            link("https://example.org", "x", Flavour::Code),
            None,
            "code"
        );
        assert_eq!(linked("https://a.org https://b.org", "x"), None, "two");
        assert_eq!(linked("see https://a.org", "x"), None, "text");
        assert_eq!(linked("ftp://a.org", "x"), None, "scheme");
        assert_eq!(linked("https://", "x"), None, "no address");
        assert_eq!(linked("https://a.org", "two\nlines"), None, "lines");
        assert_eq!(linked("https://a.org", "https://b.org"), None, "a url");
        assert_eq!(linked("https://a.org", "  "), None, "blank");
        assert_eq!(
            link("https://a.org/[x]", "the page", Flavour::Note(Syntax::Org)).as_deref(),
            Some(r"[[https://a.org/\[x\]][the page]]"),
            "org writes its own link"
        );
    }

    /// A bracket in the selection or a parenthesis the address leaves open would end the link
    /// early: the text's are escaped, and such an address goes between `<` and `>`.
    #[test]
    fn a_link_is_written_so_it_reads_back_whole() {
        let linked = |clip, sel| link(clip, sel, Flavour::Note(Syntax::Markdown)).unwrap();
        let cases = [
            (
                "https://a.org/x",
                "see [1]",
                r"[see \[1\]](https://a.org/x)",
                "see [1]",
            ),
            ("https://a.org/x", r"a\", r"[a\\](https://a.org/x)", r"a\"),
            (
                "https://a.org/x",
                r"kept \]",
                r"[kept \]](https://a.org/x)",
                "kept ]",
            ),
            (
                "https://a.org/x",
                "`a[0]` b",
                "[`a[0]` b](https://a.org/x)",
                "<code>a[0]</code> b",
            ),
            (
                "https://a.org/f(x",
                "open",
                "[open](<https://a.org/f(x>)",
                "open",
            ),
            (
                "https://a.org/x)",
                "shut",
                "[shut](<https://a.org/x)>)",
                "shut",
            ),
            (
                "https://a.org/<b>",
                "angle",
                r"[angle](<https://a.org/\<b\>>)",
                "angle",
            ),
            (
                "https://a.org/f(x)",
                "plain",
                "[plain](https://a.org/f(x))",
                "plain",
            ),
        ];
        for (url, selection, written, shown) in cases {
            assert_eq!(linked(url, selection), written);
            // What the preview makes of it: one link, to the address, around the selection.
            let html = accent_core::markdown::to_html(&linked(url, selection));
            assert_eq!(html.matches("<a ").count(), 1, "{html}");
            let href = url.replace('<', "%3C").replace('>', "%3E");
            assert!(html.contains(&format!("href=\"{href}\"")), "{html}");
            assert!(html.contains(&format!(">{shown}</a>")), "{html}");
        }
    }
}
