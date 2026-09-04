//! What the editor does with Return, Backspace and a bracket: continue a list, close a fence,
//! pair a delimiter.
//!
//! The decisions are plain functions over the current line, so they are testable without a
//! display; [`install`] is the only part that needs a widget. Nothing here parses the note:
//! `markdown::analyze` works on the whole buffer and its spans are thrown away after the
//! highlighting pass, so a per-keystroke lookup would mean re-parsing. A list marker is visible in
//! the line itself, and "am I inside a code fence" is already on the buffer as a tag.
//!
//! Every edit runs inside one `begin_user_action`/`end_user_action`, so one Ctrl+Z undoes it,
//! and the whole controller stands down while there are secondary carets or a completion popup:
//! both own Return, and neither wants a list marker inserted underneath them.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::Cell;
use std::rc::Rc;

/// Delimiters that close themselves when typed. Markdown emphasis is deliberately absent: a `*`
/// that grew a second `*` under the caret would be in the way far more often than it helped.
const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// Delimiters that only wrap an existing selection. Backtick is here rather than in [`PAIRS`]
/// because auto-closing it makes ```` ``` ```` impossible to type: the second backtick would step
/// over the first one's partner and the third would open a new pair, and the fence rule below
/// would never see a line it recognises.
const WRAPPERS: [char; 4] = ['*', '_', '~', '`'];

/// What Return does to the line it was pressed on.
#[derive(Debug, PartialEq, Eq)]
pub enum Continue {
    /// The new line starts with this, so the list, enumeration or quote carries on.
    Insert(String),
    /// The line holds a marker and nothing else: the marker goes and the list ends, the way
    /// every editor with list continuation ends one.
    Unlist,
}

/// What a typed delimiter does.
#[derive(Debug, PartialEq, Eq)]
pub enum Pair {
    /// Insert both, caret between them.
    Insert(char, char),
    /// Put the two around the selection.
    Wrap(char, char),
    /// The closer typed is already sitting under the caret: move over it instead of doubling it.
    StepOver,
    /// Nothing to do; the key is the user's.
    Plain,
}

/// How Return should carry on from `line`, which is the current line up to the caret.
///
/// Recognised: `-`/`*`/`+` bullets, `N.` and `N)` enumerations, `- [ ]` and `- [x]` task items,
/// and `>` quotes, each keeping its indent. A task item always continues unchecked; an
/// enumeration counts on.
pub fn continuation(line: &str) -> Option<Continue> {
    let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
    let rest = &line[indent.len()..];

    if let Some(body) = rest.strip_prefix('>') {
        let body = body.strip_prefix(' ').unwrap_or(body);
        return Some(match body.is_empty() {
            true => Continue::Unlist,
            false => Continue::Insert(format!("{indent}> ")),
        });
    }

    if let Some(bullet) = rest.chars().next().filter(|c| "-*+".contains(*c))
        && let Some(body) = rest[bullet.len_utf8()..].strip_prefix(' ')
    {
        // A task item keeps its brackets but never its tick.
        if let Some(after) = task_body(body) {
            return Some(match after.is_empty() {
                true => Continue::Unlist,
                false => Continue::Insert(format!("{indent}{bullet} [ ] ")),
            });
        }
        return Some(match body.is_empty() {
            true => Continue::Unlist,
            false => Continue::Insert(format!("{indent}{bullet} ")),
        });
    }

    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[digits.len()..];
    if let Some(delimiter) = after.chars().next().filter(|c| *c == '.' || *c == ')')
        && let Some(body) = after[1..].strip_prefix(' ')
        // A number too long to count on is not an enumeration anyone is writing.
        && let Ok(n) = digits.parse::<u32>()
    {
        return Some(match body.is_empty() {
            true => Continue::Unlist,
            false => Continue::Insert(format!("{indent}{}{delimiter} ", n + 1)),
        });
    }
    None
}

/// The text of a task item after its `[ ]`, or `None` when `body` does not start with one.
fn task_body(body: &str) -> Option<&str> {
    let rest = body.strip_prefix('[')?;
    let tick = rest
        .chars()
        .next()
        .filter(|c| matches!(c, ' ' | 'x' | 'X'))?;
    let rest = rest[tick.len_utf8()..].strip_prefix(']')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

/// What typing `ch` should do. `next` is the character the caret sits in front of, `selection`
/// whether there is one to wrap.
///
/// A pair is only opened in front of whitespace, a closing delimiter or the end of the line:
/// typing `(` right before a word is far more often the start of a correction than of a
/// parenthesis, and a `)` appearing mid-word has to be deleted again.
pub fn pair(ch: char, next: Option<char>, selection: bool) -> Pair {
    if selection {
        return match surround(ch) {
            Some((open, close)) => Pair::Wrap(open, close),
            None => Pair::Plain,
        };
    }
    if PAIRS.iter().any(|(_, close)| *close == ch) && next == Some(ch) {
        return Pair::StepOver;
    }
    match PAIRS.iter().find(|(open, _)| *open == ch) {
        Some((open, close))
            if next.is_none_or(|n| n.is_whitespace() || PAIRS.iter().any(|(_, c)| *c == n)) =>
        {
            Pair::Insert(*open, *close)
        }
        _ => Pair::Plain,
    }
}

/// The two delimiters `ch` puts around a selection, or `None` if it puts none.
fn surround(ch: char) -> Option<(char, char)> {
    PAIRS
        .iter()
        .find(|(open, close)| *open == ch || *close == ch)
        .copied()
        .or_else(|| WRAPPERS.contains(&ch).then_some((ch, ch)))
}

/// Whether Backspace between `prev` and `next` should take both: an empty pair this module put
/// there is deleted as one, so undoing a mistyped bracket is one key rather than two.
pub fn deletes_pair(prev: char, next: Option<char>) -> bool {
    PAIRS
        .iter()
        .any(|(open, close)| *open == prev && next == Some(*close))
}

/// Whether Return at the end of `line` should write the fence that closes the block it opens.
/// `rest` is the buffer text after the caret.
///
/// The line has to be a lone opener (`` ``` `` plus an info string), and the fences below it have
/// to be even in number: an odd count means one of them already closes this block.
pub fn closes_fence(line: &str, rest: &str) -> bool {
    if !opens_fence(line) {
        return false;
    }
    rest.lines().filter(|l| is_fence(l)).count() % 2 == 0
}

fn is_fence(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn opens_fence(line: &str) -> bool {
    let Some(info) = line.trim_start().strip_prefix("```") else {
        return false;
    };
    // `\w*`: an info string is a language name, and anything else on the line means the fence is
    // being edited rather than opened.
    info.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '+'))
}

// ------------------------------------------------------------------------------- the controller

/// Give `view` the typing helpers. Called from `editor::open` after `completion::install`.
pub fn install(view: &sourceview5::View) {
    // The completion popup answers Return itself, and both its key controller and ours run in the
    // capture phase on the same widget, so which of them GTK reaches first is not something to
    // rely on. Its visibility is, and it is two signals away.
    let popup = Rc::new(Cell::new(false));
    let completion = sourceview5::prelude::ViewExt::completion(view);
    completion.connect_show(glib::clone!(
        #[strong]
        popup,
        move |_| popup.set(true)
    ));
    completion.connect_hide(glib::clone!(
        #[strong]
        popup,
        move |_| popup.set(false)
    ));

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        view,
        #[strong]
        popup,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, state| match popup.get() {
            true => glib::Propagation::Proceed,
            false => on_key(&view, key, state),
        }
    ));
    view.add_controller(keys);
}

fn on_key(view: &sourceview5::View, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    // Every accelerator, and every multi-caret replay, keeps the key. The caret check is the
    // reliable half of that: `multicaret`'s controller stops propagation when it acts, but it is
    // on the same widget and the same phase, so this asks the view directly instead.
    if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK)
        || view
            .downcast_ref::<crate::multicaret::View>()
            .is_some_and(|v| v.has_carets())
    {
        return glib::Propagation::Proceed;
    }
    match key {
        gdk::Key::Return | gdk::Key::KP_Enter => on_return(view),
        gdk::Key::BackSpace => on_backspace(view),
        _ => match key.to_unicode().filter(|c| !c.is_control()) {
            Some(ch) => on_char(view, ch),
            None => glib::Propagation::Proceed,
        },
    }
}

fn on_return(view: &sourceview5::View) -> glib::Propagation {
    let buffer = view.buffer();
    if buffer.has_selection() {
        return glib::Propagation::Proceed;
    }
    let caret = buffer.iter_at_mark(&buffer.get_insert());
    let mut start = caret;
    start.set_line_offset(0);
    let line = buffer.text(&start, &caret, true).to_string();

    // The fence rule runs before the code-block guard on purpose: by the time Return arrives the
    // opening line is usually already tagged as the block it opened.
    if caret.ends_line() {
        let rest = buffer.text(&caret, &buffer.end_iter(), true);
        if closes_fence(&line, &rest) {
            let indent: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            let at_line = caret.line();
            let mut at = caret;
            buffer.begin_user_action();
            buffer.insert(&mut at, &format!("\n\n{indent}```"));
            buffer.end_user_action();
            // The blank line between the two fences, which is where the code goes.
            let inside = buffer
                .iter_at_line(at_line + 1)
                .unwrap_or_else(|| buffer.end_iter());
            buffer.place_cursor(&inside);
            view.scroll_mark_onscreen(&buffer.get_insert());
            return glib::Propagation::Stop;
        }
    }
    if verbatim(&buffer, &start) {
        return glib::Propagation::Proceed;
    }

    match continuation(&line) {
        Some(Continue::Insert(prefix)) => {
            let mut at = caret;
            buffer.begin_user_action();
            buffer.insert(&mut at, &format!("\n{prefix}"));
            buffer.end_user_action();
        }
        // Only at the end of the line: in the middle of one, Return is a split and the marker
        // still belongs to the text that stays behind.
        Some(Continue::Unlist) if caret.ends_line() => {
            let (mut s, mut e) = (start, caret);
            buffer.begin_user_action();
            buffer.delete(&mut s, &mut e);
            buffer.end_user_action();
        }
        _ => return glib::Propagation::Proceed,
    }
    view.scroll_mark_onscreen(&buffer.get_insert());
    glib::Propagation::Stop
}

fn on_backspace(view: &sourceview5::View) -> glib::Propagation {
    let buffer = view.buffer();
    if buffer.has_selection() {
        return glib::Propagation::Proceed;
    }
    let caret = buffer.iter_at_mark(&buffer.get_insert());
    let mut before = caret;
    if !before.backward_char() || !deletes_pair(before.char(), char_at(&caret)) {
        return glib::Propagation::Proceed;
    }
    let mut after = caret;
    after.forward_char();
    buffer.begin_user_action();
    buffer.delete(&mut before, &mut after);
    buffer.end_user_action();
    glib::Propagation::Stop
}

fn on_char(view: &sourceview5::View, ch: char) -> glib::Propagation {
    if surround(ch).is_none() {
        return glib::Propagation::Proceed;
    }
    let buffer = view.buffer();
    let selection = buffer.selection_bounds();
    let caret = buffer.iter_at_mark(&buffer.get_insert());
    // Code and frontmatter are verbatim: a bracket there is data, not markup.
    if selection.is_none() && verbatim(&buffer, &caret) {
        return glib::Propagation::Proceed;
    }

    match pair(ch, char_at(&caret), selection.is_some()) {
        Pair::Wrap(open, close) => {
            let Some((s, e)) = selection else {
                return glib::Propagation::Proceed;
            };
            // Marks rather than iters: the first insert invalidates both. Gravities are chosen so
            // the selection ends up around the original text, delimiters outside it, which is
            // what makes wrapping twice work.
            let from = buffer.create_mark(None, &s, false);
            let to = buffer.create_mark(None, &e, true);
            buffer.begin_user_action();
            let mut at = buffer.iter_at_mark(&to);
            buffer.insert(&mut at, &close.to_string());
            let mut at = buffer.iter_at_mark(&from);
            buffer.insert(&mut at, &open.to_string());
            buffer.end_user_action();
            // Cursor at the end of the wrapped text, the way a selection normally ends up.
            buffer.select_range(&buffer.iter_at_mark(&to), &buffer.iter_at_mark(&from));
            buffer.delete_mark(&from);
            buffer.delete_mark(&to);
        }
        Pair::Insert(open, close) => {
            let mut at = caret;
            buffer.begin_user_action();
            buffer.insert(&mut at, &format!("{open}{close}"));
            buffer.end_user_action();
            let mut back = buffer.iter_at_mark(&buffer.get_insert());
            back.backward_char();
            buffer.place_cursor(&back);
        }
        Pair::StepOver => {
            let mut at = caret;
            at.forward_char();
            buffer.place_cursor(&at);
        }
        Pair::Plain => return glib::Propagation::Proceed,
    }
    view.scroll_mark_onscreen(&buffer.get_insert());
    glib::Propagation::Stop
}

/// The character the iter sits in front of, or `None` at the end of the buffer.
fn char_at(iter: &gtk::TextIter) -> Option<char> {
    (!iter.is_end()).then(|| iter.char())
}

/// Whether `iter` is inside a code block or the frontmatter, where none of this applies.
fn verbatim(buffer: &gtk::TextBuffer, iter: &gtk::TextIter) -> bool {
    ["codeblock", "frontmatter"]
        .iter()
        .filter_map(|name| buffer.tag_table().lookup(name))
        .any(|tag| iter.has_tag(&tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inserts(line: &str) -> String {
        match continuation(line) {
            Some(Continue::Insert(s)) => s,
            other => panic!("{line:?} gave {other:?}"),
        }
    }

    #[test]
    fn a_list_item_carries_its_marker_to_the_next_line() {
        assert_eq!(inserts("- item"), "- ");
        assert_eq!(inserts("* item"), "* ");
        assert_eq!(inserts("+ item"), "+ ");
        assert_eq!(inserts("    - nested"), "    - ", "the indent comes along");
        assert_eq!(inserts("> quoted"), "> ");
    }

    #[test]
    fn an_enumeration_counts_on() {
        assert_eq!(inserts("1. first"), "2. ");
        assert_eq!(inserts("9) ninth"), "10) ");
        assert_eq!(inserts("  12. twelfth"), "  13. ");
    }

    #[test]
    fn a_task_item_starts_unchecked() {
        assert_eq!(inserts("- [ ] todo"), "- [ ] ");
        assert_eq!(inserts("- [x] done"), "- [ ] ");
        assert_eq!(inserts("- [X] done"), "- [ ] ");
    }

    #[test]
    fn an_empty_item_ends_the_list() {
        for line in ["- ", "1. ", "- [ ] ", "- [x]", "> ", "  * "] {
            assert_eq!(continuation(line), Some(Continue::Unlist), "{line:?}");
        }
    }

    #[test]
    fn prose_is_left_alone() {
        for line in [
            "",
            "plain text",
            "-no space",
            "1.no space",
            "#tag",
            "# Head",
        ] {
            assert_eq!(continuation(line), None, "{line:?}");
        }
    }

    #[test]
    fn a_bracket_closes_itself_only_where_it_makes_sense() {
        assert_eq!(pair('(', None, false), Pair::Insert('(', ')'));
        assert_eq!(pair('[', Some(' '), false), Pair::Insert('[', ']'));
        assert_eq!(pair('{', Some(')'), false), Pair::Insert('{', '}'));
        assert_eq!(
            pair('(', Some('a'), false),
            Pair::Plain,
            "not before a word"
        );
        assert_eq!(pair(')', Some(')'), false), Pair::StepOver);
        assert_eq!(pair(')', Some('a'), false), Pair::Plain);
        assert_eq!(pair('*', None, false), Pair::Plain, "emphasis never pairs");
    }

    #[test]
    fn a_selection_gets_wrapped() {
        assert_eq!(pair('(', None, true), Pair::Wrap('(', ')'));
        assert_eq!(pair(')', None, true), Pair::Wrap('(', ')'));
        assert_eq!(pair('*', None, true), Pair::Wrap('*', '*'));
        assert_eq!(pair('`', None, true), Pair::Wrap('`', '`'));
        assert_eq!(pair('q', None, true), Pair::Plain);
    }

    #[test]
    fn backspace_takes_an_empty_pair_whole() {
        assert!(deletes_pair('(', Some(')')));
        assert!(!deletes_pair('(', Some('x')));
        assert!(!deletes_pair('(', None));
        assert!(!deletes_pair('x', Some(')')));
    }

    #[test]
    fn a_fence_closes_itself_only_while_it_is_unmatched() {
        assert!(closes_fence("```", "\nprose\n"));
        assert!(closes_fence("```rust", "\nprose\n"));
        assert!(closes_fence("  ```", "\n"), "an indented fence too");
        assert!(
            !closes_fence("```", "\ncode\n```\n"),
            "a closer below already matches it"
        );
        assert!(
            closes_fence("```", "\ncode\n```\nmore\n```py\n"),
            "two below means the next one is this block's opener again"
        );
        assert!(!closes_fence("prose", "\n"));
        assert!(!closes_fence("``", "\n"));
        assert!(!closes_fence("```rust extra", "\n"), "not an info string");
    }
}
