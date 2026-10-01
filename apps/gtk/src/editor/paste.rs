//! Paste over a selection in a note, when the clipboard is one address: the selection becomes a
//! link to it, `[selection](address)`, the way Obsidian and GitHub do it. `Ctrl+Shift+V` is the
//! way around it, the clipboard as it is (DESIGN.md, Editing).

use super::{Flavour, Tab};
use crate::multicaret;
use gtk::prelude::*;
use gtk::{gio, glib};

/// What a paste of `clipboard` over `selection` in a file of `flavour` puts there instead of the
/// clipboard, or `None` for an ordinary paste: the link, when the clipboard, trimmed, is a single
/// address.
pub(super) fn link(clipboard: &str, selection: &str, flavour: Flavour) -> Option<String> {
    let url = clipboard.trim();
    (could_link(selection, flavour) && is_url(url))
        .then(|| format!("[{}]({})", link_text(selection), destination(url)))
}

/// The selection as a link's text: a bracket it would end the text at escaped, and a backslash
/// at its end, which would escape the closing one. The selection is markdown already, so a
/// bracket it escapes itself stays as it is, and so does all of a code span, where a backslash is
/// a backslash.
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

/// How much of `rest`, which starts with a backtick, a code span takes: up to the next run of
/// exactly as many backticks, or the run alone where none closes it, which is then text.
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

/// The address as a link's destination: as it is, unless a space, a parenthesis left open or
/// shut too often, or an angle bracket would end it or read as something else; then between `<`
/// and `>`, CommonMark's other form, with the angle brackets in it escaped.
fn destination(url: &str) -> String {
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

/// What the file and the selection decide on their own, before the clipboard is read: prose,
/// where `[x](y)` is a link and not code, and some text on one line that is not an address itself.
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

/// Answer a paste over a selection that [`could_link`] with the link when the clipboard is an
/// address, and with the paste GTK would have made when it is not. Every other paste is left to
/// GTK and to a column's own: no selection, a column of carets, a read-only view, code.
///
/// The signal is what `Ctrl+V`, `Shift+Insert` and the context menu's Paste all emit, and this
/// runs before the inherited handler, which it stops. Returns the handler, which
/// [`Tab::paste_verbatim`] holds back.
pub(super) fn link_paste(view: &sourceview5::View, flavour: Flavour) -> glib::SignalHandlerId {
    view.connect_paste_clipboard(move |view| {
        let buffer = view.buffer();
        let column = view
            .downcast_ref::<multicaret::View>()
            .is_some_and(|v| v.has_carets());
        let Some((start, end)) = buffer.selection_bounds() else {
            return;
        };
        // A clipboard with no text on it holds no address, and an image on its own is pasted as
        // an attachment (`App::wire_attachments`), whose handler runs after this one.
        let text = view.clipboard().formats().contains_type(glib::Type::STRING);
        if column
            || !text
            || !view.is_editable()
            || !could_link(&buffer.text(&start, &end, true), flavour)
        {
            return;
        }
        view.stop_signal_emission_by_name("paste-clipboard");
        // The read is asynchronous even from this process; the marks ride out an edit meanwhile.
        let (from, to) = (
            buffer.create_mark(None, &start, true),
            buffer.create_mark(None, &end, false),
        );
        view.clipboard().read_text_async(
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak]
                view,
                move |read| {
                    let buffer = view.buffer();
                    let (mut start, mut end) =
                        (buffer.iter_at_mark(&from), buffer.iter_at_mark(&to));
                    buffer.delete_mark(&from);
                    buffer.delete_mark(&to);
                    let Ok(Some(clipboard)) = read else {
                        return;
                    };
                    let selection = buffer.text(&start, &end, true);
                    let text = link(&clipboard, &selection, flavour)
                        .unwrap_or_else(|| clipboard.to_string());
                    // One undo step, the link or the paste.
                    buffer.begin_user_action();
                    buffer.delete(&mut start, &mut end);
                    buffer.insert(&mut start, &text);
                    buffer.end_user_action();
                    view.scroll_mark_onscreen(&buffer.get_insert());
                }
            ),
        );
    })
}

impl Tab {
    /// `Ctrl+Shift+V`: the clipboard as it is, never made into a link. GTK's own paste, at every
    /// caret of a column as `Ctrl+V` is, with [`link_paste`] held back for the one emission.
    pub fn paste_verbatim(&self) {
        self.view.block_signal(&self.paste_link);
        self.view.emit_paste_clipboard();
        self.view.unblock_signal(&self.paste_link);
    }
}

#[cfg(test)]
mod tests {
    use super::{Flavour, link};

    #[test]
    fn a_url_pasted_over_prose_links_it() {
        let note = Flavour::Note;
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
    }

    /// A bracket in the selection or a parenthesis the address leaves open would end the link
    /// early: the text's are escaped, and such an address goes between `<` and `>`.
    #[test]
    fn a_link_is_written_so_it_reads_back_whole() {
        let linked = |clip, sel| link(clip, sel, Flavour::Note).unwrap();
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
