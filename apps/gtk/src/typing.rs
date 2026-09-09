//! What the editor does with Return, Backspace and a bracket: continue a list, close a fence,
//! pair a delimiter.
//!
//! The decisions are plain functions over the current line, so they are testable without a
//! display; [`install`] is the only part that needs a widget. Nothing here parses the note:
//! `markdown::analyze` works on the whole buffer and its spans are thrown away after the
//! highlighting pass, so a per-keystroke lookup would mean re-parsing. A list marker is visible in
//! the line itself, and "am I inside a code fence" is already on the buffer as a tag.
//!
//! Every edit runs inside one `begin_user_action`/`end_user_action`, so one Ctrl+Z undoes it.
//! Which presses reach [`on_key`] at all is `editor::keys`'s decision: a completion popup and a
//! column of carets both own Return, and neither wants a list marker inserted underneath them.

use crate::editor::{caret, line_prefix};
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;

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

/// The column a wrapped line lines up behind: the characters at the head of `line` that a reader
/// takes for its gutter — the indent, and the list, task or quote marker that opens the line
/// together with the space after it. 0 where the line opens neither, which is most prose.
///
/// The markers are [`continuation`]'s and are read the same way. What differs is that this
/// measures the marker the line already carries rather than writing the next one, so `9)` is the
/// three columns it takes and not the four `10)` would.
///
/// Indent, bullet, digits and `>` are all ASCII, so the byte count is the character count.
pub fn wrap_column(line: &str) -> usize {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    indent + marker_width(&line[indent..])
}

/// Whether `line` holds nothing but its indent and a list, task or quote marker: what Return on a
/// list item leaves behind, and the one place Tab means "indent this item".
///
/// Read through [`continuation`], which already decides what "a marker and nothing after it"
/// means — that is what it ends a list on — so the two cannot drift apart. A line of bare indent
/// counts too: there is nothing on it either, and Tab is its indent.
pub fn marker_only(line: &str) -> bool {
    match continuation(line) {
        Some(Continue::Unlist) => true,
        Some(Continue::Insert(_)) => false,
        None => !line.is_empty() && line.trim_start_matches([' ', '\t']).is_empty(),
    }
}

/// What Tab inserts in front of such a line: the marker's own width in spaces, or one tab where
/// the line is already indented with them. `None` where there is no marker to step past, which
/// leaves the key to the view.
pub fn list_indent(line: &str) -> Option<String> {
    if !marker_only(line) {
        return None;
    }
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let width = wrap_column(line) - indent;
    match (width > 0, line[..indent].contains('\t')) {
        (false, _) => None,
        (true, true) => Some("\t".to_string()),
        (true, false) => Some(" ".repeat(width)),
    }
}

/// The width of the list or quote marker `rest` opens with, the spaces after it included, or 0
/// where it opens with none. A task item's `[ ]` is content and not marker: the request is that a
/// wrap lines up under the line's first character, which is the bracket.
fn marker_width(rest: &str) -> usize {
    let head = if rest.starts_with(['>', '-', '*', '+']) {
        1
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || !rest[digits..].starts_with(['.', ')']) {
            return 0;
        }
        digits + 1
    };
    let spaces = rest[head..].len() - rest[head..].trim_start_matches(' ').len();
    // A marker is only a marker with a space behind it — `-no space` is a word. `>` is the
    // exception `continuation` also makes: `>quoted` is a quote all the same.
    match spaces > 0 || rest.starts_with('>') {
        true => head + spaces,
        false => 0,
    }
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

// ------------------------------------------------------------------------------- the keys

/// What a note does with a press `editor::keys` has offered to nobody else. Every accelerator
/// keeps its key: a modifier here is a chord, not typing.
pub fn on_key(
    view: &sourceview5::View,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> glib::Propagation {
    if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
        return glib::Propagation::Proceed;
    }
    match key {
        gdk::Key::Return | gdk::Key::KP_Enter => on_return(view),
        gdk::Key::BackSpace => on_backspace(view),
        gdk::Key::Tab | gdk::Key::KP_Tab => on_tab(view),
        _ => match key.to_unicode().filter(|c| !c.is_control()) {
            Some(ch) => on_char(view, ch),
            None => glib::Propagation::Proceed,
        },
    }
}

/// Tab on a line that is nothing but its indent and a marker indents the item by the width of
/// that marker, so a nested item starts where its parent's text does — the column
/// [`continuation`] writes the marker at, and the one `highlight::hang` wraps to. Anywhere else
/// the key is the view's.
fn on_tab(view: &sourceview5::View) -> glib::Propagation {
    let buffer = view.buffer();
    if buffer.has_selection() {
        return glib::Propagation::Proceed;
    }
    let at = caret(&buffer);
    let Some(indent) = list_indent(&line_prefix(&buffer, &at)) else {
        return glib::Propagation::Proceed;
    };
    let mut start = at;
    start.set_line_offset(0);
    buffer.begin_user_action();
    buffer.insert(&mut start, &indent);
    buffer.end_user_action();
    glib::Propagation::Stop
}

fn on_return(view: &sourceview5::View) -> glib::Propagation {
    let buffer = view.buffer();
    if buffer.has_selection() {
        return glib::Propagation::Proceed;
    }
    let caret = caret(&buffer);
    let mut start = caret;
    start.set_line_offset(0);
    let line = line_prefix(&buffer, &caret).to_string();

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
    let caret = caret(&buffer);
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
    let at = caret(&buffer);
    // Code and frontmatter are verbatim: a bracket there is data, not markup.
    if selection.is_none() && verbatim(&buffer, &at) {
        return glib::Propagation::Proceed;
    }

    match pair(ch, char_at(&at), selection.is_some()) {
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
            let mut at = at;
            buffer.begin_user_action();
            buffer.insert(&mut at, &format!("{open}{close}"));
            buffer.end_user_action();
            let mut back = caret(&buffer);
            back.backward_char();
            buffer.place_cursor(&back);
            // The pair lands as one two-character insert, and GtkSourceCompletion only asks
            // `is_trigger` about single characters, so the second `[` of a wikilink has to open
            // the note list by hand.
            let mut before = back;
            if open == '[' && before.backward_chars(2) && before.char() == '[' {
                view.completion().show();
            }
        }
        Pair::StepOver => {
            let mut at = at;
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
    [crate::highlight::CODEBLOCK, "frontmatter"]
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

    /// What a wrapped line hangs behind. The general rule and not a list-only one: an indented
    /// continuation line has no marker and still wraps under its own indent.
    #[test]
    fn a_wrapped_line_hangs_behind_its_own_gutter() {
        assert_eq!(wrap_column("- item"), 2);
        assert_eq!(wrap_column("* item"), 2);
        assert_eq!(wrap_column("1. first"), 3);
        assert_eq!(
            wrap_column("9) ninth"),
            3,
            "the marker it has, not the next one"
        );
        assert_eq!(wrap_column("12. twelfth"), 4);
        assert_eq!(wrap_column("  - nested"), 4, "the indent counts");
        assert_eq!(
            wrap_column("  - [ ] task"),
            4,
            "the box is content, not marker"
        );
        assert_eq!(wrap_column("> quoted"), 2);
        assert_eq!(wrap_column(">quoted"), 1, "a quote needs no space");
        assert_eq!(wrap_column("    continued"), 4, "a plain indent hangs too");
        for line in [
            "",
            "plain text",
            "# Head",
            "-no space",
            "*emphasis*",
            "1.no",
        ] {
            assert_eq!(wrap_column(line), 0, "{line:?}");
        }
    }

    /// What Return on a list item leaves behind, and where Tab therefore means "indent this".
    /// A line with any text of its own is not it: there Tab is the view's own key.
    #[test]
    fn a_marker_only_line_is_where_tab_indents() {
        for line in ["- ", "  - ", "1. ", "- [ ] ", "> ", "    "] {
            assert!(marker_only(line), "{line:?}");
        }
        for line in ["", "- item", "plain", "-no space"] {
            assert!(!marker_only(line), "{line:?}");
        }
    }

    /// One indent step is the marker's own width, so a nested item starts where its parent's
    /// text does. A line already indented with tabs keeps them.
    #[test]
    fn indenting_an_item_steps_by_its_own_marker() {
        assert_eq!(list_indent("- ").as_deref(), Some("  "));
        assert_eq!(list_indent("  - ").as_deref(), Some("  "));
        assert_eq!(list_indent("1. ").as_deref(), Some("   "));
        assert_eq!(list_indent("12. ").as_deref(), Some("    "));
        assert_eq!(
            list_indent("- [ ] ").as_deref(),
            Some("  "),
            "the box is content"
        );
        assert_eq!(list_indent("\t- ").as_deref(), Some("\t"), "tabs stay tabs");
        assert_eq!(list_indent("    "), None, "no marker to step past");
        assert_eq!(list_indent("- item"), None, "not on a line with text");
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
