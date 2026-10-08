//! What the popup lists and what a key does to it, decided without a widget.

use accent_core::fuzzy::{Corpus, Query};
use gtk::gdk;

/// How many rows the popup shows before it scrolls.
pub(super) const MAX_ROWS: u32 = 8;

/// How many rows Page Up and Page Down move by: a screenful, keeping one row in sight.
const PAGE: u32 = MAX_ROWS - 1;

/// One item to rank: what it is matched by, and what was typed since where it starts.
pub(super) struct Candidate<'a> {
    pub filter: &'a str,
    pub typed: &'a str,
    pub corpus: Corpus,
}

/// The candidates that match what was typed, best first, by index.
///
/// Those whose filter starts with what was typed lead, as the word being finished; then the
/// better match; then the provider's own order, which is a server's ranking or a note's shortest
/// path. The matching is [`Query::as_typed`]: in the order typed, any case, `cafe` finding `café`.
pub(super) fn rank(candidates: &[Candidate]) -> Vec<usize> {
    // One query per distinct text typed, which is one or two however many items there are: the
    // items of an answer mostly start where the word does.
    let mut queries: Vec<(&str, Corpus, Query)> = Vec::new();
    let mut hits: Vec<(bool, std::cmp::Reverse<u32>, usize)> = Vec::new();
    for (i, c) in candidates.iter().enumerate() {
        let q = match queries
            .iter()
            .position(|(typed, corpus, _)| *typed == c.typed && *corpus == c.corpus)
        {
            Some(q) => q,
            None => {
                queries.push((c.typed, c.corpus, Query::as_typed(c.typed, c.corpus)));
                queries.len() - 1
            }
        };
        if let Some(score) = queries[q].2.score(c.filter) {
            hits.push((!starts_with(c.filter, c.typed), std::cmp::Reverse(score), i));
        }
    }
    hits.sort_unstable();
    hits.into_iter().map(|(_, _, i)| i).collect()
}

/// Whether `text` starts with `prefix`, case aside.
fn starts_with(text: &str, prefix: &str) -> bool {
    let mut text = text.chars().flat_map(char::to_lowercase);
    prefix
        .chars()
        .flat_map(char::to_lowercase)
        .all(|p| text.next() == Some(p))
}

/// Which characters of `label` a row emboldens: where what was typed matches it, from after the
/// last character that opens a link, a heading, a tag or a path segment (`[[Notes/De` against
/// `Deep`), since the label leaves those out.
pub(super) fn highlight(label: &str, typed: &str) -> Vec<u32> {
    let needle = typed.rsplit(['[', '#', '(', '/']).next().unwrap_or(typed);
    if needle.is_empty() {
        return Vec::new();
    }
    Query::as_typed(needle, Corpus::Words)
        .indices(label)
        .map(|(_, at)| at)
        .unwrap_or_default()
}

/// Whether a character continues a word being completed: a code identifier, or a word of prose.
fn is_identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether what was just typed opens the popup, and with which trigger character: `before` is the
/// line up to the caret.
///
/// The character just typed is one the provider asked to be told about (`.`, `[`); or a word has
/// grown to `min_word` characters, one in code and two in prose, where a single letter matches
/// half the dictionary; or a word was started straight after a trigger, the `t` of `#t`, which a
/// note answers at once although the `#` alone was a heading.
pub(super) fn opens(before: &str, triggers: &[char], min_word: usize) -> Option<Option<char>> {
    let last = before.chars().next_back()?;
    if triggers.contains(&last) {
        return Some(Some(last));
    }
    let word = before
        .chars()
        .rev()
        .take_while(|c| is_identifier(*c))
        .count();
    let lead = before.chars().rev().nth(word);
    let after_trigger = lead.is_some_and(|c| triggers.contains(&c));
    (word > 0 && (word >= min_word || after_trigger)).then_some(None)
}

/// What a key pressed with the popup up asks of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Select this row, or none.
    Select(Option<u32>),
    /// Apply this row.
    Accept(u32),
    /// Put the popup away, and the key with it: Escape.
    Dismiss,
    /// Put the popup away and let the key go on to the editor: Return or Tab with no row
    /// selected, which are a newline and an indent then.
    Close,
    /// Not the popup's: typing, the caret keys, every chord.
    Pass,
}

/// Which [`Step`] a press asks for, from the popup's selection and its number of rows.
///
/// Nothing is selected until an arrow picks a row; Up from the first row goes back to none.
/// Return and Tab accept a selected row and are the editor's otherwise.
pub(super) fn step(
    key: gdk::Key,
    state: gdk::ModifierType,
    selected: Option<u32>,
    rows: u32,
) -> Step {
    let held = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::SHIFT_MASK;
    if rows == 0 || state.intersects(held) {
        return Step::Pass;
    }
    let last = rows - 1;
    match key {
        gdk::Key::Down | gdk::Key::KP_Down => {
            Step::Select(Some(selected.map_or(0, |i| (i + 1).min(last))))
        }
        gdk::Key::Up | gdk::Key::KP_Up => Step::Select(selected.and_then(|i| i.checked_sub(1))),
        gdk::Key::Page_Down | gdk::Key::KP_Page_Down => {
            Step::Select(Some(selected.map_or(0, |i| (i + PAGE).min(last))))
        }
        gdk::Key::Page_Up | gdk::Key::KP_Page_Up => {
            Step::Select(Some(selected.map_or(0, |i| i.saturating_sub(PAGE))))
        }
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab | gdk::Key::KP_Tab => {
            selected.map_or(Step::Close, Step::Accept)
        }
        gdk::Key::Escape => Step::Dismiss,
        _ => Step::Pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked(items: &[(&str, &str)]) -> Vec<usize> {
        let candidates: Vec<Candidate> = items
            .iter()
            .map(|(filter, typed)| Candidate {
                filter,
                typed,
                corpus: Corpus::Words,
            })
            .collect();
        rank(&candidates)
    }

    #[test]
    fn what_starts_with_the_typed_text_leads_then_the_score_then_the_provider() {
        // `xget` matches `get` scattered, `get_func` and `getter` start with it.
        assert_eq!(
            ranked(&[("xget", "get"), ("getter", "get"), ("get_func", "get")]),
            [1, 2, 0]
        );
        // Equal in both: the provider's order stands.
        assert_eq!(ranked(&[("beta", ""), ("alpha", "")]), [0, 1]);
        // Case is no matter, a non-match is gone, and `cafe` finds `café`.
        assert_eq!(ranked(&[("Get", "get"), ("set", "get")]), [0]);
        assert_eq!(ranked(&[("café", "cafe")]), [0]);
    }

    /// Each item is matched against what was typed since its own start: a word starting at the
    /// word, a link row at its `[[`.
    #[test]
    fn each_item_is_matched_by_what_was_typed_since_its_own_start() {
        assert_eq!(
            ranked(&[
                ("[[Deep.md", "[[De"),
                ("Debate", "De"),
                ("[[Other.md", "[[De")
            ]),
            [0, 1]
        );
    }

    #[test]
    fn the_highlight_is_the_typed_word_in_the_label() {
        assert_eq!(highlight("get_func", "gf"), [0, 4]);
        assert_eq!(highlight("Deep", "[[Notes/De"), [0, 1]);
        assert_eq!(highlight("Heading", "[[Note#Hea"), [0, 1, 2]);
        assert!(highlight("anything", "[[").is_empty());
        assert!(highlight("other", "zz").is_empty());
    }

    #[test]
    fn a_trigger_or_a_long_enough_word_opens_the_popup() {
        let code = ['.'];
        let note = ['[', '#', '('];
        assert_eq!(opens("self.", &code, 1), Some(Some('.')));
        assert_eq!(opens("let x = f", &code, 1), Some(None));
        assert_eq!(opens("x = ", &code, 1), None);
        assert_eq!(opens("", &code, 1), None);
        // Prose waits for a second letter, unless the word follows a trigger.
        assert_eq!(opens("the t", &note, 2), None);
        assert_eq!(opens("the th", &note, 2), Some(None));
        assert_eq!(opens("a #t", &note, 2), Some(None));
        assert_eq!(opens("see [[", &note, 2), Some(Some('[')));
        assert_eq!(opens("x_1", &code, 2), Some(None));
    }

    #[test]
    fn keys_walk_the_rows_and_accept_only_a_selected_one() {
        let none = gdk::ModifierType::empty();
        let key = |k, sel| step(k, none, sel, 10);
        assert_eq!(key(gdk::Key::Down, None), Step::Select(Some(0)));
        assert_eq!(key(gdk::Key::Down, Some(9)), Step::Select(Some(9)));
        assert_eq!(key(gdk::Key::Up, Some(0)), Step::Select(None));
        assert_eq!(key(gdk::Key::Up, None), Step::Select(None));
        assert_eq!(key(gdk::Key::Page_Down, Some(0)), Step::Select(Some(7)));
        assert_eq!(key(gdk::Key::Page_Down, Some(7)), Step::Select(Some(9)));
        assert_eq!(key(gdk::Key::Page_Up, Some(9)), Step::Select(Some(2)));
        assert_eq!(key(gdk::Key::Page_Up, Some(2)), Step::Select(Some(0)));
        assert_eq!(key(gdk::Key::Return, Some(3)), Step::Accept(3));
        assert_eq!(key(gdk::Key::Tab, Some(3)), Step::Accept(3));
        assert_eq!(key(gdk::Key::Return, None), Step::Close);
        assert_eq!(key(gdk::Key::Tab, None), Step::Close);
        assert_eq!(key(gdk::Key::Escape, None), Step::Dismiss);
        assert_eq!(key(gdk::Key::a, Some(1)), Step::Pass);
        assert_eq!(key(gdk::Key::Left, Some(1)), Step::Pass);
        assert_eq!(key(gdk::Key::BackSpace, Some(1)), Step::Pass);
        // A chord, Shift+Tab included, is somebody else's.
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        assert_eq!(step(gdk::Key::Down, ctrl, None, 10), Step::Pass);
        let shift = gdk::ModifierType::SHIFT_MASK;
        assert_eq!(step(gdk::Key::ISO_Left_Tab, shift, Some(1), 10), Step::Pass);
        assert_eq!(step(gdk::Key::Down, none, None, 0), Step::Pass);
    }
}
