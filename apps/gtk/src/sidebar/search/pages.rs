//! The Search pane's pages around its rows — what to type, nothing found, a query that cannot be
//! read — and the count that says what the rows add up to.

use crate::widgets::{scroller, status_page};
use gtk::glib;

/// What the prompt says: the query language at a glance, before anything is typed.
const PROMPT: &str = "Words are found in any order. Narrow them with #tag, path:, kind:pdf, \
                      links:, has:highlights, modified:>7d or -word, and turn on All to include \
                      ignored files.";

/// The body under the boxes: a page each for nothing typed (`prompt`), nothing found (`empty`), a
/// pattern that does not compile (`invalid`) and a query whose qualifier cannot be read
/// (`invalid-search`), and the rows (`results`). The second is that last page, which says why.
pub(super) fn body(view: &gtk::ListView) -> (gtk::Stack, adw::StatusPage) {
    let body = gtk::Stack::builder().vexpand(true).build();
    let prompt = glib::markup_escape_text(PROMPT);
    body.add_named(
        &status_page("system-search-symbolic", "Search Files", &prompt),
        Some("prompt"),
    );
    body.add_named(
        &status_page(
            "system-search-symbolic",
            "No Results",
            "Try a different search.",
        ),
        Some("empty"),
    );
    body.add_named(
        &status_page(
            "dialog-warning-symbolic",
            "Invalid Pattern",
            "This is not a valid regular expression.",
        ),
        Some("invalid"),
    );
    let unread = status_page("dialog-warning-symbolic", "Invalid Search", "");
    body.add_named(&unread, Some("invalid-search"));
    body.add_named(&scroller(view), Some("results"));
    body.set_visible_child_name("prompt");
    (body, unread)
}

/// Show why a query cannot be read, in its own words (`accent_core::query`), which name the
/// qualifier as typed and what it takes.
pub(super) fn say_unread(page: &adw::StatusPage, why: &str) {
    let why = glib::markup_escape_text(why);
    page.set_description(Some(&why));
}

/// What an answer holds, where the progress bar is drawn between queries: "12 results in 3
/// files". A number the list's cap may have cut short is a floor, and says so with a `+`.
pub(super) fn count_label(
    found: usize,
    files: usize,
    more_found: bool,
    more_files: bool,
) -> String {
    format!(
        "{} in {}",
        counted(found, more_found, "result"),
        counted(files, more_files, "file")
    )
}

/// What a query of qualifiers alone lists: files, one row each, "12 files".
pub(super) fn files_label(files: usize, more: bool) -> String {
    counted(files, more, "file")
}

fn counted(n: usize, more: bool, noun: &str) -> String {
    match (n, more) {
        (1, false) => format!("1 {noun}"),
        (n, false) => format!("{n} {noun}s"),
        (n, true) => format!("{n}+ {noun}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_says_one_in_the_singular_and_a_cut_number_as_a_floor() {
        assert_eq!(count_label(1, 1, false, false), "1 result in 1 file");
        assert_eq!(count_label(12, 3, false, false), "12 results in 3 files");
        // An exact scan counts every match the index holds past the cap, but not the files.
        assert_eq!(
            count_label(340, 20, false, true),
            "340 results in 20+ files"
        );
        assert_eq!(
            count_label(100, 20, true, true),
            "100+ results in 20+ files"
        );
        assert_eq!(files_label(1, false), "1 file");
        assert_eq!(files_label(100, true), "100+ files");
    }

    #[test]
    fn the_prompt_is_markup_that_parses() {
        let prompt = glib::markup_escape_text(PROMPT);
        assert!(gtk::pango::parse_markup(&prompt, '\u{0}').is_ok());
    }
}
