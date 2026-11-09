//! An open document's text on a remote vault's host, kept as the window's by sending what changed
//! rather than the whole text, as LSP's incremental sync does. Each message gives the document a
//! version of the window's, one it never gave before, and an edit names the version it applies
//! to, so a host holding any other (a change lost on the way, one that came late, a reconnect)
//! refuses it and is sent the whole text.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{Range, TextEdit, byte_of, range_of};
use crate::locked;

/// What changed in an open document, as LSP's `TextDocumentContentChangeEvent` says it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum Change {
    /// `edit` made to the text at version `from`.
    Edit { from: u64, edit: TextEdit },
    /// The whole text.
    Whole(String),
}

/// The one edit that turns `old` into `new`: what lies between their common start and their
/// common end, the start taken first.
pub(crate) fn edit_between(old: &str, new: &str) -> TextEdit {
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut start = common(a.iter(), b.iter());
    // A shared lead byte is not a shared character.
    while !(old.is_char_boundary(start) && new.is_char_boundary(start)) {
        start -= 1;
    }
    let mut end = common(a[start..].iter().rev(), b[start..].iter().rev());
    while !(old.is_char_boundary(a.len() - end) && new.is_char_boundary(b.len() - end)) {
        end -= 1;
    }
    TextEdit {
        range: range_of(old, &(start..a.len() - end)),
        text: new[start..b.len() - end].to_string(),
    }
}

/// How many bytes `a` and `b` share from their start.
fn common<'a>(a: impl Iterator<Item = &'a u8>, b: impl Iterator<Item = &'a u8>) -> usize {
    a.zip(b).take_while(|(x, y)| x == y).count()
}

/// The text of each document a remote vault's window has open on this host, at the version the
/// window gave it last; empty on a local vault, whose window sends the whole text.
#[derive(Default)]
pub(crate) struct Held(Mutex<HashMap<String, (u64, String)>>);

impl Held {
    pub(crate) fn open(&self, rel: &str, version: u64, text: String) {
        locked(&self.0).insert(rel.to_string(), (version, text));
    }

    pub(crate) fn close(&self, rel: &str) {
        locked(&self.0).remove(rel);
    }

    /// `change` made to `rel`, which is `version` from then on: the text it leaves, or `None` for
    /// an edit of a version not held here, or of a place past the text's end, which changes
    /// nothing. A document not open here takes nothing either.
    pub(crate) fn change(&self, rel: &str, version: u64, change: Change) -> Option<String> {
        let mut held = locked(&self.0);
        let (at, text) = held.get_mut(rel)?;
        match change {
            Change::Whole(whole) => *text = whole,
            Change::Edit { from, edit } if from == *at => {
                let span = bytes_of(text, edit.range)?;
                text.replace_range(span, &edit.text);
            }
            Change::Edit { .. } => return None,
        }
        *at = version;
        Some(text.clone())
    }
}

/// `range` of `text` as byte offsets, or `None` where it starts past the text's end.
fn bytes_of(text: &str, range: Range) -> Option<std::ops::Range<usize>> {
    let start = byte_of(text, range.start)?;
    Some(start..byte_of(text, range.end)?.max(start))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An edit found between two texts turns the first into the second, wherever it is and
    /// whatever characters it borders: a two-byte character sharing its lead byte with the one
    /// typed over it, a line break taken out, the start, the end, and nothing at all.
    #[test]
    fn the_edit_between_two_texts_makes_one_the_other() {
        let held = Held::default();
        for (old, new) in [
            ("a\nbé\nc", "a\nbè\nc"),
            ("one\ntwo\n", "one two\n"),
            ("x", "yx"),
            ("x", "xy"),
            ("", "new"),
            ("gone", ""),
            ("same\n", "same\n"),
            ("ab ab ab", "ab ab"),
        ] {
            held.open("a.md", 1, old.to_string());
            let edit = edit_between(old, new);
            let change = Change::Edit { from: 1, edit };
            assert_eq!(
                held.change("a.md", 2, change).as_deref(),
                Some(new),
                "{old:?}"
            );
        }
        let typed = edit_between("one\ntwo\nthree", "one\ntwo!\nthree");
        assert_eq!(typed.text, "!");
    }

    /// A host that holds another version than an edit was made to, a change having been lost or
    /// come late, takes nothing and holds what it held; the whole text it takes at any version.
    #[test]
    fn an_edit_of_another_version_is_refused() {
        let held = Held::default();
        held.open("a.md", 1, "abc".to_string());
        let edit = edit_between("abc", "abXc");
        let stale = Change::Edit { from: 7, edit };
        assert_eq!(held.change("a.md", 8, stale.clone()), None);
        assert_eq!(
            held.change("a.md", 9, Change::Whole("abc".into())),
            Some("abc".into())
        );
        // Version 9 now, so an edit made to 7 is still refused, as one to 9 is not.
        assert_eq!(held.change("a.md", 10, stale), None);
        let edit = edit_between("abc", "abXc");
        let fresh = Change::Edit { from: 9, edit };
        assert_eq!(held.change("a.md", 10, fresh), Some("abXc".into()));
        held.close("a.md");
        assert_eq!(held.change("a.md", 11, Change::Whole("x".into())), None);
    }
}
