//! Underline Ctrl+click targets while Ctrl is held.
//!
//! Analysed note links and line-local URLs provide immediate hints; clicks query current
//! text because analysis may lag typing. Code definitions require a language-server probe
//! after [`PROBE`], avoiding false hints on identifiers with no destination. The result is
//! cached while the pointer stays in the word.

use super::Tab;
use crate::lang;
use gtk::glib;
use gtk::prelude::*;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

/// Pointer-rest delay before probing definitions, avoiding requests for
/// every word crossed without a noticeable hover delay.
const PROBE: Duration = Duration::from_millis(120);

/// What the Ctrl+hover underline is doing right now.
#[derive(Default)]
pub(super) struct Follow {
    /// Tagged character range, avoiding retagging when the pointer stays inside it.
    shown: Option<Range<i32>>,
    /// Last probed word and result. Reuse within the word, but reprobe after
    /// leaving because the file may have changed.
    asked: Option<(Range<i32>, bool)>,
    /// The pending probe, cancelled whenever the pointer leaves what it was about to ask about.
    probe: Option<glib::SourceId>,
    /// Probe generation, used to discard answers after the pointer changes target.
    generation: u64,
    /// Last view coordinates, allowing Ctrl presses to underline without pointer motion.
    pointer: Option<(f64, f64)>,
}

impl Tab {
    /// The pointer moved, or the modifiers changed under a pointer that did not.
    pub(crate) fn follow_hint(self: &Rc<Self>, x: f64, y: f64, ctrl: bool) {
        self.follow.borrow_mut().pointer = Some((x, y));
        if !ctrl {
            return self.clear_follow();
        }
        let (bx, by) =
            self.view
                .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let Some(iter) = crate::fold::iter_at_location(&self.view, bx, by) else {
            return self.clear_follow();
        };
        // A link is already known: the analysis stored its range in characters for exactly this.
        // A bare URL is in the text itself, in any file.
        if let Some(range) = self
            .link_range_at(iter.offset())
            .or_else(|| url_under(&iter).map(|(range, _)| range))
        {
            return self.set_follow(range);
        }
        let Some(vault) = self.lang.vault() else {
            return self.clear_follow();
        };
        // A file whose server was never installed answers nothing, and Ctrl+click on it toasts
        // rather than jumps. Underlining there would promise a jump that cannot happen.
        if self
            .lang
            .support()
            .and_then(|s| s.missing.clone())
            .is_some()
        {
            return self.clear_follow();
        }
        let Some(word) = word_at(&iter) else {
            return self.clear_follow();
        };
        // Still inside the word the answer is about: nothing to ask and nothing to change. Read
        // out of the cell in its own statement, because both arms below borrow it again.
        let asked = self.follow.borrow().asked.clone();
        if let Some((asked, found)) = asked
            && asked == word
        {
            return if found {
                self.set_follow(word)
            } else {
                self.clear_follow()
            };
        }
        // Nothing is known about this word yet. Take the underline off while the server is asked,
        // or the one belonging to the link the pointer just left lingers over the word it moved
        // to, which is the opposite of what the underline is for.
        self.clear_follow();
        self.probe(vault, iter, word);
    }

    /// Ask the server whether `word` leads anywhere, once the pointer has rested on it.
    fn probe(
        self: &Rc<Self>,
        vault: std::sync::Arc<accent_api::Vault>,
        iter: gtk::TextIter,
        word: Range<i32>,
    ) {
        let pos = lang::pos_of(&iter);
        let generation = {
            let mut follow = self.follow.borrow_mut();
            follow.generation += 1;
            if let Some(id) = follow.probe.take() {
                id.remove();
            }
            follow.generation
        };
        let id = glib::timeout_add_local_once(
            PROBE,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || {
                    tab.follow.borrow_mut().probe = None;
                    glib::spawn_future_local(async move {
                        lang::flush(tab.clone()).await;
                        let found = !vault
                            .definition(&tab.language_key(), pos)
                            .await
                            .unwrap_or_default()
                            .is_empty();
                        // The pointer may have moved on while the server was thinking.
                        if tab.follow.borrow().generation != generation {
                            return;
                        }
                        tab.follow.borrow_mut().asked = Some((word.clone(), found));
                        if found {
                            tab.set_follow(word);
                        } else {
                            tab.clear_follow();
                        }
                    });
                }
            ),
        );
        self.follow.borrow_mut().probe = Some(id);
    }

    /// Underline `range` and turn the pointer into a hand.
    fn set_follow(self: &Rc<Self>, range: Range<i32>) {
        if self.follow.borrow().shown.as_ref() == Some(&range) {
            return;
        }
        self.untag();
        let (from, to) = (
            self.buffer.iter_at_offset(range.start),
            self.buffer.iter_at_offset(range.end),
        );
        self.buffer.apply_tag(&self.follow_tag, &from, &to);
        self.follow.borrow_mut().shown = Some(range);
        self.view.set_cursor_from_name(Some("pointer"));
    }

    /// Ctrl came up, the pointer left the view, or what it is over leads nowhere.
    pub(super) fn clear_follow(&self) {
        {
            let mut follow = self.follow.borrow_mut();
            follow.generation += 1;
            if let Some(id) = follow.probe.take() {
                id.remove();
            }
        }
        if self.follow.borrow().shown.is_none() {
            return;
        }
        self.untag();
        self.follow.borrow_mut().shown = None;
        self.view.set_cursor_from_name(Some("text"));
    }

    /// Clear the underline across the buffer because edits can move tagged text
    /// beyond the stored range.
    fn untag(&self) {
        let (start, end) = self.buffer.bounds();
        self.buffer.remove_tag(&self.follow_tag, &start, &end);
    }

    /// The character range of the link covering `at`, if there is one.
    fn link_range_at(&self, at: i32) -> Option<Range<i32>> {
        self.links
            .borrow()
            .iter()
            .find(|(range, _)| range.contains(&at))
            .map(|(range, _)| range.clone())
    }

    /// What the underline covers right now, for the drill that checks it.
    #[cfg(feature = "bench")]
    pub(crate) fn follow_shown(&self) -> Option<(i32, i32)> {
        self.follow.borrow().shown.clone().map(|r| (r.start, r.end))
    }

    /// Whether Ctrl is held, and where the pointer was last seen.
    pub(super) fn follow_pointer(&self) -> Option<(f64, f64)> {
        self.follow.borrow().pointer
    }
}

/// Find the identifier at `at` as buffer character offsets. Scan its line
/// with a pure, testable boundary rule instead of display-dependent iterators.
pub(super) fn word_at(at: &gtk::TextIter) -> Option<Range<i32>> {
    let (base, line) = line_around(at);
    let word = word_bounds(&line, at.line_offset() as usize)?;
    Some(base + word.start as i32..base + word.end as i32)
}

/// Find the bare URL at `at` in any text, returning buffer character offsets
/// and the URL.
pub(crate) fn url_under(at: &gtk::TextIter) -> Option<(Range<i32>, String)> {
    let (base, line) = line_around(at);
    let byte = line.char_indices().nth(at.line_offset() as usize)?.0;
    let url = url_at(&line, byte)?;
    let chars = |byte: usize| base + line[..byte].chars().count() as i32;
    Some((chars(url.start)..chars(url.end), line[url].to_string()))
}

/// The buffer offset the line holding `at` starts at, and its text.
fn line_around(at: &gtk::TextIter) -> (i32, String) {
    let mut start = *at;
    start.set_line_offset(0);
    let mut end = *at;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    (start.offset(), start.slice(&end).to_string())
}

/// Find the `http://`, `https://` or `mailto:` URL covering byte `at`, with the terminal's
/// boundaries (`markdown::bare_urls`), which the spell checker passes over too. Exclude `file:`
/// paths from Ctrl-click desktop opening.
fn url_at(line: &str, at: usize) -> Option<Range<usize>> {
    accent_core::markdown::bare_urls(line).find(|url| url.contains(&at))
}

/// Find the identifier covering character `at`, using letters, digits and
/// underscores. Pango word boundaries split `open_at`, producing partial
/// identifier underlines.
fn word_bounds(line: &str, at: usize) -> Option<Range<usize>> {
    let part = |c: &char| c.is_alphanumeric() || *c == '_';
    let chars: Vec<char> = line.chars().collect();
    if !chars.get(at).is_some_and(part) {
        return None;
    }
    let start = chars[..at]
        .iter()
        .rposition(|c| !part(c))
        .map_or(0, |i| i + 1);
    let end = chars[at..]
        .iter()
        .position(|c| !part(c))
        .map_or(chars.len(), |i| at + i);
    Some(start..end)
}

#[cfg(test)]
mod tests {
    use super::{url_at, word_bounds};

    /// Verify URL detection from any interior byte, including in code comments,
    /// excludes surrounding sentence punctuation.
    #[test]
    fn a_url_is_found_from_any_byte_in_it() {
        let line = "x = 1  # see https://e.org/a_(b)?q=1#f, then";
        let url = 13..line.find(',').unwrap();
        assert_eq!(&line[url.clone()], "https://e.org/a_(b)?q=1#f");
        for at in url.clone() {
            assert_eq!(url_at(line, at), Some(url.clone()), "at {at}");
        }
        assert_eq!(url_at(line, url.start - 1), None, "the space before it");
        assert_eq!(url_at(line, url.end), None, "the comma after it");
        assert_eq!(url_at("(mailto:a@b.org).", 3), Some(1..15));
        assert_eq!(
            url_at("<http://é.fr>", 1),
            Some(1..13),
            "past a multibyte character"
        );
    }

    #[test]
    fn a_scheme_alone_or_another_scheme_is_not_a_url() {
        assert_eq!(url_at("https:// and", 2), None);
        assert_eq!(url_at("file:///etc/passwd", 3), None);
        assert_eq!(url_at("ftp://e.org", 3), None);
    }

    #[test]
    fn a_word_is_the_whole_identifier_underscores_and_all() {
        // The pointer anywhere in `open_at` covers all of it, first character to last.
        for at in 4..11 {
            assert_eq!(word_bounds("let open_at = 1;", at), Some(4..11), "at {at}");
        }
    }

    #[test]
    fn punctuation_and_space_are_not_part_of_one() {
        assert_eq!(word_bounds("let open_at = 1;", 3), None, "the space");
        assert_eq!(word_bounds("a.b", 1), None, "the dot");
        assert_eq!(word_bounds("a.b", 2), Some(2..3), "the field after it");
    }

    #[test]
    fn a_word_at_either_end_of_the_line_keeps_its_edge() {
        assert_eq!(word_bounds("tab", 0), Some(0..3));
        assert_eq!(word_bounds("tab", 2), Some(0..3));
        assert_eq!(word_bounds("", 0), None);
    }
}
