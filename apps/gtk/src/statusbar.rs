//! The bar along the bottom of the editor column.
//!
//! It says what is happening (indexing, opening), what the file is, how long it is and how far
//! it is zoomed, the readout being the control that puts the zoom back. All of
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
    /// The branch readout, which is also the Sync control.
    branch: gtk::Button,
    branch_label: gtk::Label,
    /// Shown over the branch name while a sync runs. Over rather than beside it, and the name is
    /// faded rather than hidden, so the bar never changes width mid-sync.
    branch_spinner: adw::Spinner,
    kind: gtk::Label,
    words: gtk::Label,
    /// The zoom readout, which is also the control that resets it.
    zoom: gtk::Button,
    zoom_label: gtk::Label,
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
        // The branch is the Sync control as well as the readout: it names the repository the
        // document sits in, and clicking it pulls and pushes that one (DESIGN.md, Layout map).
        let branch_label = label(true);
        // The overlay does not measure the spinner (GtkOverlay's default), so the button is as
        // wide as the branch name alone whether or not a sync is running.
        let branch_spinner = adw::Spinner::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .can_target(false)
            .visible(false)
            .build();
        let branch_body = gtk::Overlay::builder().child(&branch_label).build();
        branch_body.add_overlay(&branch_spinner);
        let branch = bar_button(&branch_body, "win.git-sync", "Sync");
        let kind = label(false);
        let words = label(true);

        // The readout is the reset control: clicking it is Ctrl+0, which is 100 % for a document
        // and Fit Width for a PDF.
        let zoom_label = label(true);
        let zoom = bar_button(&zoom_label, "win.zoom-reset", "Reset Zoom");

        row.append(&progress);
        row.append(&branch);
        // The file's own facts sit at the far end, away from what the window is busy with.
        kind.set_hexpand(true);
        kind.set_halign(gtk::Align::End);
        row.append(&kind);
        row.append(&words);
        row.append(&zoom);

        Bar {
            row,
            progress,
            branch,
            branch_label,
            branch_spinner,
            kind,
            words,
            zoom,
            zoom_label,
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
        set(&self.branch_label, branch);
        self.branch.set_visible(branch.is_some());
    }

    /// Whether a sync is running. The branch name fades and a spinner turns in its place; the
    /// label keeps its allocation, so nothing in the bar moves.
    pub fn set_syncing(&self, on: bool) {
        self.branch_spinner.set_visible(on);
        self.branch_label.set_opacity(if on { 0.0 } else { 1.0 });
    }

    /// What the file is: "Markdown", "PDF", or "Rust · UTF-8 · LF" for code.
    pub fn set_kind(&self, text: Option<&str>) {
        set(&self.kind, text);
    }

    pub fn set_words(&self, count: Option<usize>) {
        set(&self.words, count.map(words_label).as_deref());
    }

    /// The zoom, while it is worth saying: "110 %", or a PDF's "Fit Width" / "Fit Page".
    pub fn set_zoom(&self, text: Option<&str>) {
        set(&self.zoom_label, text);
        self.zoom.set_visible(text.is_some());
    }

    /// The zoom control itself, which the window hangs its Fit Width / Fit Page menu off.
    pub fn zoom(&self) -> &gtk::Widget {
        self.zoom.upcast_ref()
    }
}

/// A readout that is also a control: flat and label-only, so it reads as the rest of the bar
/// rather than as a button parked in it.
///
/// `.accent-bar-button` is what keeps the bar one height. A button's minimum is 24 px plus 5 px of
/// padding either side, and a box is as tall as its tallest child whatever its alignment, so one
/// of these appearing pushed the bar from 29 px to 46 px. The class pins it to the caption's own
/// line height; it stays a button, so it keeps its focus, its role and its tooltip.
fn bar_button(child: &impl IsA<gtk::Widget>, action: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(child)
        .action_name(action)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    button.add_css_class("flat");
    button.add_css_class("accent-bar-button");
    button
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
