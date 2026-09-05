//! The bar along the bottom of the editor column.
//!
//! It says what is happening (indexing, opening), what the file is and how long it is. All of
//! that used to be spread across the header bar, where it competed with the vault name and the
//! note path; a document's own facts belong under it, not beside its title. The branch is the
//! repository the document sits in, which is not always the vault's own.
//!
//! Nothing here fades with the chrome: the bar is one line of text the reader glances at, and a
//! word count that disappears while you type is a word count nobody can use.

use gtk::prelude::*;

/// The bar itself. Every label hides when it has nothing to say, so an empty bar is an empty
/// line rather than a row of dashes.
pub struct Bar {
    row: gtk::Box,
    progress: gtk::Label,
    branch: gtk::Label,
    kind: gtk::Label,
    words: gtk::Label,
}

impl Bar {
    pub fn new() -> Bar {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("accent-flat");
        row.set_margin_start(12);
        row.set_margin_end(12);
        row.set_margin_top(6);
        row.set_margin_bottom(6);

        let progress = label(false);
        let branch = label(false);
        let kind = label(false);
        let words = label(true);

        row.append(&progress);
        row.append(&branch);
        // The file's own facts sit at the far end, away from what the window is busy with.
        kind.set_hexpand(true);
        kind.set_halign(gtk::Align::End);
        row.append(&kind);
        row.append(&words);

        Bar {
            row,
            progress,
            branch,
            kind,
            words,
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.row.upcast_ref()
    }

    /// What the window is busy with: "Indexing… 1200/42700 files", "Opening the document…".
    pub fn set_progress(&self, text: Option<&str>) {
        set(&self.progress, text);
    }

    /// The branch of the repository holding the active document, "main ↑1 ↓2".
    pub fn set_branch(&self, branch: Option<&str>) {
        set(&self.branch, branch);
    }

    /// What the file is: "Markdown", "PDF", or "Rust · UTF-8 · LF" for code.
    pub fn set_kind(&self, text: Option<&str>) {
        set(&self.kind, text);
    }

    pub fn set_words(&self, count: Option<usize>) {
        set(&self.words, count.map(words_label).as_deref());
    }
}

fn label(numeric: bool) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    if numeric {
        label.add_css_class("numeric");
    }
    label.set_visible(false);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label
}

fn set(label: &gtk::Label, text: Option<&str>) {
    match text {
        Some(text) => {
            label.set_label(text);
            label.set_visible(true);
        }
        None => label.set_visible(false),
    }
}

/// Words as a reader counts them, which is whitespace-separated runs. Markdown markers ride along
/// on the words they mark, so a heading's `#` is not a word of its own but `- item` counts the
/// dash; close enough for a readout that exists to show a draft growing.
pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

pub fn words_label(count: usize) -> String {
    match count {
        1 => "1 word".to_string(),
        n => format!("{n} words"),
    }
}

/// The readout for a code tab: what the language is, then how the bytes are encoded and how the
/// lines end. Only code says the last two — a note is UTF-8 with LF endings or it would not be a
/// note, and a readout that never changes is chrome for nothing.
pub fn code_label(language: Option<&str>, encoding: &str) -> String {
    format!("{} · {encoding}", language.unwrap_or("Plain Text"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_count_splits_on_whitespace() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("   \n\t "), 0);
        assert_eq!(word_count("a  b\n\nc"), 3);
        assert_eq!(word_count("héllo wörld"), 2);
        // Markers count with the word they mark, except a list dash, which stands alone.
        assert_eq!(word_count("# Title\n- item"), 4);
    }

    #[test]
    fn words_label_says_one_word_in_the_singular() {
        assert_eq!(words_label(0), "0 words");
        assert_eq!(words_label(1), "1 word");
        assert_eq!(words_label(42), "42 words");
    }

    #[test]
    fn code_label_names_plain_text_when_there_is_no_language() {
        assert_eq!(code_label(Some("Rust"), "UTF-8 · LF"), "Rust · UTF-8 · LF");
        assert_eq!(
            code_label(None, "Not UTF-8 · CRLF"),
            "Plain Text · Not UTF-8 · CRLF"
        );
    }
}
