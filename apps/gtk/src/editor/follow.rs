//! What a Ctrl+click would follow, underlined while Ctrl is held.
//!
//! Ctrl+click has always gone to a definition, but nothing on screen said *where* it would work:
//! the pointer turned into a hand over a `[[wikilink]]` and over nothing else, so on a source file
//! every word looked the same as every other and the only way to find out was to click.
//!
//! A link is answered from the table the analysis already filled, so it underlines on the motion
//! event itself. A word in code has to be asked about, and the only thing that truly knows is the
//! language server — underlining every identifier would say nothing, since the question is whether
//! *this* one leads anywhere. So the pointer resting on a word for [`PROBE`] asks for its
//! definition and the underline follows the answer. That is one request per word rested on, the
//! same shape as the hover beside it, and the answer is remembered for as long as the pointer
//! stays inside the word it was asked about.

use super::Tab;
use crate::lang;
use gtk::glib;
use gtk::prelude::*;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

/// How long the pointer rests on a word before the server is asked whether it leads anywhere.
/// Below what reads as a delay, and long enough that crossing a line of code does not ask about
/// every word on the way.
const PROBE: Duration = Duration::from_millis(120);

/// What the Ctrl+hover underline is doing right now.
#[derive(Default)]
pub(super) struct Follow {
    /// The character range the tag is on, so a pointer moving inside it re-tags nothing.
    shown: Option<Range<i32>>,
    /// The last word the server was asked about and what it said. A pointer wandering inside one
    /// word asks once; leaving it and coming back asks again, because the file may have changed.
    asked: Option<(Range<i32>, bool)>,
    /// The pending probe, cancelled whenever the pointer leaves what it was about to ask about.
    probe: Option<glib::SourceId>,
    /// Bumped whenever the question changes, so an answer that arrives late is dropped rather
    /// than underlining a word the pointer has already left.
    generation: u64,
    /// Where the pointer was last seen, in the view's coordinates: pressing Ctrl without moving
    /// has to underline what the pointer is already over.
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
        let Some(iter) = self.view.iter_at_location(bx, by) else {
            return self.clear_follow();
        };
        // A link is already known: the analysis stored its range in characters for exactly this.
        if let Some(range) = self.link_range_at(iter.offset()) {
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
                            .definition(&tab.rel(), pos)
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

    /// Take the underline off wherever it is. Over the whole buffer rather than the range that was
    /// stored, because an edit under a held Ctrl moves the tag with the text it is on.
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
    pub(crate) fn follow_shown(&self) -> Option<(i32, i32)> {
        self.follow.borrow().shown.clone().map(|r| (r.start, r.end))
    }

    /// Whether Ctrl is held, and where the pointer was last seen.
    pub(super) fn follow_pointer(&self) -> Option<(f64, f64)> {
        self.follow.borrow().pointer
    }
}

/// The identifier `at` sits in, as a range of buffer character offsets.
///
/// The line around the pointer is read out and scanned, so the boundary rule is one pure function
/// with a test rather than a walk over iterators that needs a display to run.
fn word_at(at: &gtk::TextIter) -> Option<Range<i32>> {
    let mut start = *at;
    start.set_line_offset(0);
    let mut end = *at;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    let line = start.slice(&end);
    let word = word_bounds(&line, at.line_offset() as usize)?;
    let base = start.offset();
    Some(base + word.start as i32..base + word.end as i32)
}

/// The identifier covering character `at` of `line`, in characters.
///
/// Not GTK's own word boundaries: Pango breaks `open_at` into two words, and half an underlined
/// identifier reads as a mistake. A word here is what a language calls one — letters, digits and
/// underscores — which is close enough in every language a server answers for.
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
    use super::word_bounds;

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
