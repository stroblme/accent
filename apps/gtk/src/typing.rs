//! Markdown typing helpers for lists, fences, pairs and tables, and a code tab's brackets.
//!
//! Pure functions inspect the current line so decisions can be tested without a display;
//! `install` supplies widget integration. Re-parsing the whole note per key is unnecessary:
//! list markers are local, code fences are tagged, and `markdown::table_key` inspects nearby
//! pipe-delimited lines.
//!
//! Each edit is one user action for Undo. `editor::keys` decides which presses reach
//! [`on_key`], giving completion and multiple carets priority.

use crate::editor::{caret, line_end, line_prefix};
use accent_core::markdown::{TableKey, table_key};
use gtk::prelude::*;
use gtk::{gdk, glib};

/// Auto-paired delimiters. Omit Markdown emphasis because inserted partners often obstruct typing.
const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// Selection-only wrapping delimiters.
/// Backticks must not auto-pair because stepping over partners prevents typing triple-backtick fences.
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

/// Continue `line` before the caret on Return, preserving indentation.
///
/// Recognise bullet, numbered, task and quote markers. Tasks continue unchecked; numbers increment.
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

/// Continue YAML frontmatter sequence items with their indentation; empty items end the sequence.
pub fn sequence_item(line: &str) -> Option<Continue> {
    let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
    let body = line[indent.len()..].strip_prefix("- ")?;
    Some(match body.is_empty() {
        true => Continue::Unlist,
        false => Continue::Insert(format!("{indent}- ")),
    })
}

/// Return the starting column for wrapped rows of `line`.
///
/// With `markers`, note lists, tasks and quotes wrap behind their existing marker. Other
/// indented lines wrap one `level` deeper to distinguish continuations; unindented lines
/// start at 0. [`continuation`] shares marker parsing, but this measures the current marker,
/// not the next number. Tabs advance to multiples of `tab_width`; marker syntax is ASCII.
pub fn wrap_column(line: &str, tab_width: usize, level: usize, markers: bool) -> usize {
    match wrap_head(line, tab_width, markers) {
        (0, "") => 0,
        (columns, "") => columns + level,
        (columns, marker) => columns + marker.len(),
    }
}

/// Return indent column and following marker text for wrapped rows.
/// [`wrap_column`] counts columns; wrap::measure uses styled marker widths for proportional fonts.
pub fn wrap_head(line: &str, tab_width: usize, markers: bool) -> (usize, &str) {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let columns = line[..indent].bytes().fold(0, |column, byte| match byte {
        b'\t' => (column / tab_width + 1) * tab_width,
        _ => column + 1,
    });
    let marker = match markers {
        true => marker_width(&line[indent..]),
        false => 0,
    };
    (columns, &line[indent..indent + marker])
}

/// Whether `line` contains only indent and a list, task or quote marker, or bare indent.
///
/// Ghost text is suppressed here because there is no content to complete. [`continuation`]
/// shares the same empty-marker rule used to end lists.
pub fn marker_only(line: &str) -> bool {
    match continuation(line) {
        Some(Continue::Unlist) => true,
        Some(Continue::Insert(_)) => false,
        None => !line.is_empty() && line.trim_start_matches([' ', '\t']).is_empty(),
    }
}

/// Return Tab's indent for an item on `line`, or `None` to leave prose and blank lines to the view.
///
/// Use the marker's width in spaces, or one tab for tab-indented lines, so nested text aligns
/// with the parent's text and wrapped rows ([`wrap_column`]). Caret position does not matter.
pub fn list_indent(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let width = marker_width(&line[indent..]);
    match (width > 0, line[..indent].contains('\t')) {
        (false, _) => None,
        (true, true) => Some("\t".to_string()),
        (true, false) => Some(" ".repeat(width)),
    }
}

/// Return the opening list/quote marker width including spaces, or 0.
/// Task brackets are content, so wrapping aligns beneath the opening bracket.
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

/// Choose the effect of typing `ch`; `next` is the character after the caret, and `selection` allows wrapping.
///
/// Open pairs only before whitespace, closing delimiters or line ends, avoiding unwanted
/// closing characters when correcting text inside a word.
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

/// Delete an empty delimiter pair with one Backspace rather than leaving its closing partner.
pub fn deletes_pair(prev: char, next: Option<char>) -> bool {
    PAIRS
        .iter()
        .any(|(open, close)| *open == prev && next == Some(*close))
}

/// Whether Return should close a lone fence opener before `rest`.
/// An odd number of later fences means a closer already exists; only an even count needs insertion.
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
    if state.intersects(
        gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::META_MASK,
    ) {
        return glib::Propagation::Proceed;
    }
    match key {
        gdk::Key::Return | gdk::Key::KP_Enter => on_return(view),
        gdk::Key::BackSpace => on_backspace(view),
        gdk::Key::Tab | gdk::Key::KP_Tab => on_tab(view),
        gdk::Key::ISO_Left_Tab if in_table(view, TableKey::BackTab) => glib::Propagation::Stop,
        _ => match key.to_unicode().filter(|c| !c.is_control()) {
            Some(ch) => on_char(view, ch),
            None => glib::Propagation::Proceed,
        },
    }
}

/// Indent a list, task, enumeration or quote with Tab from anywhere on its line.
///
/// The marker's width aligns nested content with the parent's text and wraps. Insert at
/// line start to preserve the caret's text position; other lines leave Tab to the view.
fn on_tab(view: &sourceview5::View) -> glib::Propagation {
    if in_table(view, TableKey::Tab) {
        return glib::Propagation::Stop;
    }
    let buffer = view.buffer();
    if buffer.has_selection() {
        return glib::Propagation::Proceed;
    }
    let at = caret(&buffer);
    let mut start = at;
    start.set_line_offset(0);
    let line = buffer.text(&start, &line_end(&buffer, at.line()), true);
    let Some(indent) = list_indent(&line) else {
        return glib::Propagation::Proceed;
    };
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
    let yaml = tagged(&buffer, &start, "frontmatter");
    if verbatim(&buffer, &start) && !yaml {
        return glib::Propagation::Proceed;
    }
    if in_table(view, TableKey::Enter) {
        return glib::Propagation::Stop;
    }

    let next = match yaml {
        true => sequence_item(&line),
        false => continuation(&line),
    };
    match next {
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
    apply(view, pair(ch, char_at(&at), selection.is_some()))
}

/// A bracket or Backspace in a code tab with a language: paired, stepped over, put around the
/// selection and taken back as a pair as in a note, but not in a comment or a string. Brackets
/// only: a quote cannot tell an opening one from a closing one, nor an apostrophe from either.
pub fn on_code_key(
    view: &sourceview5::View,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> glib::Propagation {
    use sourceview5::prelude::BufferExt as _;
    let language = view
        .buffer()
        .downcast_ref::<sourceview5::Buffer>()
        .and_then(|source| source.language());
    if state.intersects(
        gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::META_MASK,
    ) || !view.is_editable()
        || language.is_none()
    {
        return glib::Propagation::Proceed;
    }
    if key == gdk::Key::BackSpace {
        return on_backspace(view);
    }
    let Some(ch) = key
        .to_unicode()
        .filter(|ch| PAIRS.iter().any(|(open, close)| open == ch || close == ch))
    else {
        return glib::Propagation::Proceed;
    };
    let buffer = view.buffer();
    let at = caret(&buffer);
    let pair = pair(ch, char_at(&at), buffer.has_selection());
    // The closer stepped over is the code's own, and nothing pairs with the caret in a comment or
    // a string.
    let skip = match pair {
        Pair::StepOver => quoted(&buffer, &at),
        Pair::Insert(..) => inside(&buffer, &at),
        _ => false,
    };
    if skip {
        return glib::Propagation::Proceed;
    }
    apply(view, pair)
}

/// Carry out what a typed delimiter does, as one user action.
fn apply(view: &sourceview5::View, pair: Pair) -> glib::Propagation {
    let buffer = view.buffer();
    let selection = buffer.selection_bounds();
    let at = caret(&buffer);
    match pair {
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

/// Handle Tab, Shift+Tab or Return in an unselected table through markdown::table_key.
/// Realignment and row changes form one undo step; return whether handled.
/// Multi-caret dispatch takes these keys first.
fn in_table(view: &sourceview5::View, key: TableKey) -> bool {
    let buffer = view.buffer();
    let at = caret(&buffer);
    let mut start = at;
    start.set_line_offset(0);
    if buffer.has_selection() || verbatim(&buffer, &start) {
        return false;
    }
    let line_text = |line: i32| match buffer.iter_at_line(line) {
        Some(start) => buffer
            .text(&start, &line_end(&buffer, line), true)
            .to_string(),
        None => String::new(),
    };
    // The run of lines holding a pipe around the caret's: as far as any table here can reach.
    let piped = |line: i32| line_text(line).contains('|');
    let (mut first, mut last) = (at.line(), at.line());
    while first > 0 && piped(first - 1) {
        first -= 1;
    }
    while last + 1 < buffer.line_count() && piped(last + 1) {
        last += 1;
    }
    let lines: Vec<String> = (first..=last).map(line_text).collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let row = (at.line() - first) as usize;
    let Some(edit) = table_key(&lines, row, at.line_offset() as usize, key) else {
        return false;
    };
    let top = first + edit.lines.start as i32;
    let text = edit.text.join("\n");
    let mut from = buffer.iter_at_line(top).unwrap_or(start);
    let mut to = line_end(&buffer, first + edit.lines.end as i32 - 1);
    if buffer.text(&from, &to, true) != text {
        buffer.begin_user_action();
        buffer.delete(&mut from, &mut to);
        buffer.insert(&mut from, &text);
        buffer.end_user_action();
    }
    let (line, column) = edit.caret;
    let mut landing = line_end(&buffer, top + line as i32);
    landing.set_line_offset(column as i32);
    buffer.place_cursor(&landing);
    view.scroll_mark_onscreen(&buffer.get_insert());
    true
}

/// The character the iter sits in front of, or `None` at the end of the buffer.
fn char_at(iter: &gtk::TextIter) -> Option<char> {
    (!iter.is_end()).then(|| iter.char())
}

/// Whether the character at `iter` is in a comment or a string, by the language's highlighting.
fn quoted(buffer: &gtk::TextBuffer, iter: &gtk::TextIter) -> bool {
    use sourceview5::prelude::BufferExt as _;
    let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() else {
        return false;
    };
    let mut start = *iter;
    start.set_line_offset(0);
    // Highlighting runs in idle time, and the line was typed a moment ago.
    source.ensure_highlight(&start, &line_end(buffer, iter.line()));
    ["comment", "string"]
        .iter()
        .any(|class| source.iter_has_context_class(iter, class))
}

/// Whether a caret at `at` is inside a comment or a string: the character before it is, and so
/// is the one after it unless the line ends there.
fn inside(buffer: &gtk::TextBuffer, at: &gtk::TextIter) -> bool {
    let mut before = *at;
    !at.starts_line()
        && before.backward_char()
        && quoted(buffer, &before)
        && (at.ends_line() || quoted(buffer, at))
}

/// Whether `iter` is inside a code block or the frontmatter, where none of this applies but
/// front matter's own lists ([`sequence_item`]).
fn verbatim(buffer: &gtk::TextBuffer, iter: &gtk::TextIter) -> bool {
    [crate::highlight::CODEBLOCK, "frontmatter"]
        .iter()
        .any(|name| tagged(buffer, iter, name))
}

/// Whether the styling pass has tagged `iter` with the tag called `name`.
fn tagged(buffer: &gtk::TextBuffer, iter: &gtk::TextIter, name: &str) -> bool {
    buffer
        .tag_table()
        .lookup(name)
        .is_some_and(|tag| iter.has_tag(&tag))
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

    /// Front matter is YAML: a `- ` item carries on at its indent and an empty one ends the
    /// sequence, while what the body would continue, a quote or a task's brackets, does not.
    #[test]
    fn a_front_matter_sequence_carries_on() {
        assert_eq!(
            sequence_item("  - draft"),
            Some(Continue::Insert("  - ".into()))
        );
        assert_eq!(
            sequence_item("- [x] y"),
            Some(Continue::Insert("- ".into()))
        );
        assert_eq!(sequence_item("  - "), Some(Continue::Unlist));
        assert_eq!(sequence_item("tags: [a, b]"), None);
        assert_eq!(sequence_item("> quoted"), None);
        assert_eq!(sequence_item("1. first"), None);
    }

    #[test]
    fn an_enumeration_counts_on() {
        assert_eq!(inserts("1. first"), "2. ");
        assert_eq!(inserts("9) ninth"), "10) ");
        assert_eq!(inserts("  12. twelfth"), "  13. ");
    }

    /// What a wrapped line hangs behind: a note's list or quote marker, and otherwise one level
    /// deeper than the line's own indent, in a note and in code alike.
    #[test]
    fn a_wrapped_line_hangs_behind_its_own_gutter() {
        // A note's rule, markers on, here with 8-column tabs and levels.
        let note = |line| wrap_column(line, 8, 8, true);
        assert_eq!(note("- item"), 2);
        assert_eq!(note("* item"), 2);
        assert_eq!(note("1. first"), 3);
        assert_eq!(note("9) ninth"), 3, "the marker it has, not the next one");
        assert_eq!(note("12. twelfth"), 4);
        assert_eq!(note("  - nested"), 4, "the indent counts");
        assert_eq!(note("  - [ ] task"), 4, "the box is content, not marker");
        assert_eq!(note("\t- tabbed"), 10, "a tab reaches the next tab stop");
        assert_eq!(note("> quoted"), 2);
        assert_eq!(note(">quoted"), 1, "a quote needs no space");
        assert_eq!(
            note("    continued"),
            12,
            "a plain indent hangs a level deeper"
        );
        for line in [
            "",
            "plain text",
            "# Head",
            "-no space",
            "*emphasis*",
            "1.no",
        ] {
            assert_eq!(note(line), 0, "{line:?}");
        }
        // Code: four-column tabs and levels, and no markers.
        let code = |line| wrap_column(line, 4, 4, false);
        assert_eq!(code("    body();"), 8);
        assert_eq!(code("\t\tbody();"), 12);
        assert_eq!(code("  \tbody();"), 8, "a tab after spaces stops at 4");
        assert_eq!(code(" * a comment"), 5, "a star in code is not a bullet");
        assert_eq!(code("fn main() {"), 0);
    }

    /// The marker a wrap hangs behind is handed over as text, for the view to measure in its own
    /// font, with the column its indent reaches.
    #[test]
    fn a_wrap_hangs_behind_the_marker_text_after_its_indent() {
        let head = |line| wrap_head(line, 8, true);
        assert_eq!(head("- item"), (0, "- "));
        assert_eq!(head("  10.  item"), (2, "10.  "));
        assert_eq!(head("\t> quoted"), (8, "> "));
        assert_eq!(head("  - [ ] task"), (2, "- "));
        assert_eq!(head("    continued"), (4, ""));
        assert_eq!(
            wrap_head("- item", 8, false),
            (0, ""),
            "code has no markers"
        );
    }

    /// What Return on a list item leaves behind, and so the line ghost text stays off.
    #[test]
    fn a_marker_only_line_is_what_return_leaves_behind() {
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
    }

    /// An item is indented from anywhere on its line, whatever is written after the marker;
    /// prose is still the view's own key.
    #[test]
    fn a_line_with_text_on_it_indents_all_the_same() {
        assert_eq!(list_indent("- item").as_deref(), Some("  "));
        assert_eq!(list_indent("1. first").as_deref(), Some("   "));
        assert_eq!(list_indent("- [x] done").as_deref(), Some("  "));
        assert_eq!(list_indent("> quoted").as_deref(), Some("  "));
        assert_eq!(
            list_indent("  - nested").as_deref(),
            Some("  "),
            "its parent's width, not its own indent"
        );
        for line in ["", "    ", "plain text", "-no space", "# Head"] {
            assert_eq!(list_indent(line), None, "{line:?}");
        }
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
