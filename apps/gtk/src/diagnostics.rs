//! What a language server says is wrong with the open file, painted on the tab.
//!
//! Four channels, because one diagnostic is not like another. An error and a warning are worth
//! interrupting for: a wavy underline in the text, an icon in the gutter and the message itself
//! at the end of the line. Information and hints are not — a dangling `[[wikilink]]` is the
//! ordinary state of a vault — so they get a thin dim underline and say nothing until the pointer
//! asks. The same rule decides the status bar: it counts errors and warnings and nothing else.
//!
//! The colours are derived the way `diff.rs` derives its own: a fixed hue mixed with the resolved
//! theme foreground, so they read on a light theme and on a dark one without a hex colour outside
//! `theme.rs`.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::lang;
use accent_api::{Diagnostic, Pos, Severity};
use gtk::prelude::*;
use gtk::{gdk, pango};
use sourceview5::AnnotationStyle;
use sourceview5::prelude::*;

/// One tag per severity, so a re-render can lift exactly what it painted.
const ERROR: &str = "diag-error";
const WARNING: &str = "diag-warning";
const INFO: &str = "diag-info";
const HINT: &str = "diag-hint";
const TAGS: [&str; 4] = [ERROR, WARNING, INFO, HINT];

/// Gutter mark categories. Only the two severities that carry an icon have one.
pub const MARK_ERROR: &str = "error";
pub const MARK_WARNING: &str = "warning";

/// How loud the two quiet severities are, as an alpha over the view's background. A hint is the
/// dimmest thing the editor draws: it marks something that is not yet a problem.
const INFO_ALPHA: f32 = 0.4;
const HINT_ALPHA: f32 = 0.25;

fn tag_of(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => ERROR,
        Severity::Warning => WARNING,
        Severity::Info => INFO,
        Severity::Hint => HINT,
    }
}

/// Install the four tags on a buffer. Their colours arrive with the first [`restyle`]; the shapes
/// are fixed here, because a squiggle under an error and a straight line under a hint is the
/// distinction, not the colour.
pub fn install_tags(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    for name in TAGS {
        let underline = match name {
            ERROR | WARNING => pango::Underline::Error,
            _ => pango::Underline::Single,
        };
        let tag = gtk::TextTag::builder()
            .name(name)
            .underline(underline)
            .build();
        table.add(&tag);
    }
}

/// Re-derive the underline colours from the resolved theme foreground. Called from `Tab::restyle`,
/// which runs on the first map and again on every theme change.
pub fn restyle(buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    let fg = view.color();
    let table = buffer.tag_table();
    let set = |name: &str, colour: gdk::RGBA| {
        if let Some(tag) = table.lookup(name) {
            tag.set_underline_rgba(Some(&colour));
        }
    };
    set(ERROR, crate::diff::tint(crate::diff::REMOVED_HUE, fg, 1.0));
    set(
        WARNING,
        crate::diff::tint(crate::diff::WARNING_HUE, fg, 1.0),
    );
    set(INFO, crate::highlight::with_alpha(fg, INFO_ALPHA));
    set(HINT, crate::highlight::with_alpha(fg, HINT_ALPHA));
}

/// Paint `items` over the buffer, replacing whatever was there.
///
/// Everything is removed first rather than diffed: a publish is the server's whole answer for the
/// file, and a buffer-wide tag lift is one pass over a text nobody has scrolled through yet.
pub fn render(
    buffer: &sourceview5::Buffer,
    provider: &sourceview5::AnnotationProvider,
    items: &[Diagnostic],
) {
    let (start, end) = buffer.bounds();
    for name in TAGS {
        buffer.remove_tag_by_name(name, &start, &end);
    }
    for category in [MARK_ERROR, MARK_WARNING] {
        buffer.remove_source_marks(&start, &end, Some(category));
    }
    provider.remove_all();

    let mut lines: BTreeMap<i32, (AnnotationStyle, String, usize)> = BTreeMap::new();
    for item in items {
        let (from, mut to) = (
            lang::iter_at(buffer, item.range.start),
            lang::iter_at(buffer, item.range.end),
        );
        // A zero-width range is what a server sends for "here", and a tag over no characters
        // draws nothing. One character is the smallest thing the eye can be pointed at.
        if from == to {
            to.forward_char();
        }
        buffer.apply_tag_by_name(tag_of(item.severity), &from, &to);

        let (category, style) = match item.severity {
            Severity::Error => (MARK_ERROR, AnnotationStyle::Error),
            Severity::Warning => (MARK_WARNING, AnnotationStyle::Warning),
            // Information and hints stay in the text: no gutter icon, no line-end message.
            Severity::Info | Severity::Hint => continue,
        };
        let mut line_start = from;
        line_start.set_line_offset(0);
        buffer.create_source_mark(None, category, &line_start);
        // One annotation a line, or the messages draw over each other at the line end: the most
        // severe one is shown and the rest are counted, and the hover lists them all.
        match lines.entry(from.line()) {
            Entry::Vacant(slot) => {
                slot.insert((style, item.message.clone(), 0));
            }
            Entry::Occupied(mut slot) => {
                let shown = slot.get_mut();
                if style == AnnotationStyle::Error && shown.0 != AnnotationStyle::Error {
                    (shown.0, shown.1) = (style, item.message.clone());
                }
                shown.2 += 1;
            }
        }
    }
    for (line, (style, message, more)) in lines {
        let text = match more {
            0 => message,
            n => format!("{message} (+{n} more)"),
        };
        provider.add_annotation(&sourceview5::Annotation::new(
            Some(&text),
            None::<gtk::gio::Icon>,
            line,
            style,
        ));
    }
}

/// Reading order, which is what a range comparison means. `Pos` is not `Ord` — it crosses the
/// wire and orderings are not part of that contract — so the tuple does it here.
fn key(pos: Pos) -> (u32, u32) {
    (pos.line, pos.character)
}

/// Every diagnostic covering `pos`. What the hover appends to the server's own answer, all four
/// severities included: a hint that says nothing on screen has to say it here.
pub fn at(items: &[Diagnostic], pos: Pos) -> Vec<&Diagnostic> {
    items
        .iter()
        .filter(|d| match d.range.start == d.range.end {
            // An empty range is what a server sends for "insert something here", and it covers
            // only the one position it sits on.
            true => pos == d.range.start,
            // Half-open, as everywhere: the character after the range is outside it.
            false => key(pos) >= key(d.range.start) && key(pos) < key(d.range.end),
        })
        .collect()
}

/// What the status bar says in the word-count slot: "2 errors, 1 warning". Errors and warnings
/// only, matching the gutter; `None` when the file has neither, so the slot goes away rather than
/// standing there saying zero.
pub fn counts(items: &[Diagnostic]) -> Option<String> {
    let count = |want: Severity| items.iter().filter(|d| d.severity == want).count();
    let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
    let plural = |n: usize, one: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {one}s"),
    };
    match (errors, warnings) {
        (0, 0) => None,
        (e, 0) => Some(plural(e, "error")),
        (0, w) => Some(plural(w, "warning")),
        (e, w) => Some(format!("{}, {}", plural(e, "error"), plural(w, "warning"))),
    }
}

/// Every message on `line`, one per line: what the gutter icon says when the pointer rests on it.
pub fn messages_on(items: &[Diagnostic], line: u32) -> String {
    items
        .iter()
        .filter(|d| {
            matches!(d.severity, Severity::Error | Severity::Warning) && d.range.start.line == line
        })
        .map(|d| d.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_api::Range;

    fn diag(severity: Severity, line: u32, from: u32, to: u32) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Pos {
                    line,
                    character: from,
                },
                end: Pos {
                    line,
                    character: to,
                },
            },
            severity,
            message: format!("{severity:?} at {line}:{from}"),
            source: None,
        }
    }

    #[test]
    fn counts_names_errors_and_warnings_and_nothing_else() {
        assert_eq!(counts(&[]), None);
        assert_eq!(counts(&[diag(Severity::Hint, 0, 0, 1)]), None);
        assert_eq!(
            counts(&[diag(Severity::Error, 0, 0, 1)]),
            Some("1 error".to_string())
        );
        assert_eq!(
            counts(&[
                diag(Severity::Error, 0, 0, 1),
                diag(Severity::Error, 1, 0, 1),
                diag(Severity::Warning, 2, 0, 1),
                diag(Severity::Info, 3, 0, 1),
            ]),
            Some("2 errors, 1 warning".to_string())
        );
    }

    #[test]
    fn at_takes_the_ranges_that_cover_the_position() {
        let items = [
            diag(Severity::Error, 1, 2, 6),
            diag(Severity::Hint, 1, 4, 4),
            diag(Severity::Warning, 2, 0, 3),
        ];
        let found = at(
            &items,
            Pos {
                line: 1,
                character: 4,
            },
        );
        assert_eq!(found.len(), 2, "the range and the empty one on the caret");
        // The end is exclusive, so the character after the range is outside it.
        assert!(
            at(
                &items,
                Pos {
                    line: 1,
                    character: 6
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn a_gutter_icon_says_every_loud_message_on_its_line() {
        let items = [
            diag(Severity::Error, 3, 0, 1),
            diag(Severity::Warning, 3, 4, 5),
            diag(Severity::Hint, 3, 8, 9),
            diag(Severity::Error, 4, 0, 1),
        ];
        assert_eq!(
            messages_on(&items, 3),
            "Error at 3:0\nWarning at 3:4",
            "the hint is not on the icon, which is not drawn for it"
        );
    }
}
