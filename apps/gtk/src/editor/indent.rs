//! A code tab's indentation as it is typed, one rule for every language, with no server: Return
//! after a line whose code ends in an opener indents one level deeper, and between the opener and
//! its closer puts the closer on a line of its own; a closer typed on a blank line goes back out to
//! the level its block opened from. Comments and strings are the language spec's own context
//! classes, so `def f():  # x` opens a block and `x = "("` does not. Any other Return is
//! GtkSourceView's, the line's indent copied. Insert Line Below follows the same rule. Shift+Tab
//! takes each line it covers back to the indent stop before it, in every code tab.
//!
//! A key handler rather than a `GtkSourceIndenter`: sourceview5-rs frees the iter GtkSourceView
//! hands its `indent` (ISSUES.md).

use super::lines::leading_indent;
use super::{caret, line_end, line_prefix};
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;

/// The characters that open a level at the end of a line's code, each with the closer that ends
/// it: none for `:`, which opens Python's blocks and a `case` label's.
const OPENERS: [(char, Option<char>); 4] = [
    ('{', Some('}')),
    ('(', Some(')')),
    ('[', Some(']')),
    (':', None),
];

/// The languages in which a `:` opens nothing: Typst's blocks are brackets and LaTeX's are
/// environments, and the prose of both ends many a line with a colon.
const NO_COLON: [&str; 2] = ["typst", "latex"];

/// Shift+Tab in any code tab, and Return or a closer in one with a language; the rest is left to
/// the view.
pub(crate) fn on_key(
    view: &sourceview5::View,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> glib::Propagation {
    let Ok(buffer) = view.buffer().downcast::<sourceview5::Buffer>() else {
        return glib::Propagation::Proceed;
    };
    if state.intersects(
        gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::META_MASK,
    ) || !view.is_editable()
    {
        return glib::Propagation::Proceed;
    }
    if key == gdk::Key::ISO_Left_Tab {
        let (tab, level) = (view.tab_width() as usize, width(view));
        reindent(view, |line| {
            Some(stop_before(leading_indent(line), tab, level))
        });
        view.scroll_mark_onscreen(&buffer.get_insert());
        return glib::Propagation::Stop;
    }
    if buffer.language().is_none() || buffer.has_selection() {
        return glib::Propagation::Proceed;
    }
    let done = match key {
        gdk::Key::Return | gdk::Key::KP_Enter => on_return(view, &buffer),
        _ => match key.to_unicode() {
            Some(ch) if OPENERS.iter().any(|(_, close)| *close == Some(ch)) => {
                on_closer(view, &buffer, ch)
            }
            _ => false,
        },
    };
    if !done {
        return glib::Propagation::Proceed;
    }
    view.scroll_mark_onscreen(&buffer.get_insert());
    glib::Propagation::Stop
}

/// The indent of a line opened below the one `end` ends (Insert Line Below): that line's own, one
/// level deeper where its code ends in an opener and the view has a language.
pub(super) fn below(view: &sourceview5::View, end: &gtk::TextIter) -> String {
    let buffer = end.buffer();
    let indent = leading_indent(&line_prefix(&buffer, end)).to_string();
    let opens = buffer
        .downcast_ref::<sourceview5::Buffer>()
        .filter(|source| source.language().is_some())
        .is_some_and(|source| opened(source, end).is_some());
    match opens {
        true => indent + &unit(view),
        false => indent,
    }
}

/// One level deeper after an opener, the closer right after the caret going to a line of its own.
fn on_return(view: &sourceview5::View, buffer: &sourceview5::Buffer) -> bool {
    let at = caret(buffer);
    let Some((_, close)) = opened(buffer, &at) else {
        return false;
    };
    let indent = leading_indent(&line_prefix(buffer, &at)).to_string();
    let deeper = format!("{indent}{}", unit(view));
    let mut end = at;
    buffer.begin_user_action();
    if close == Some(at.char()) {
        buffer.insert(&mut end, &format!("\n{deeper}\n{indent}"));
        // Back over the closer's line break and indent, onto the line between.
        end.backward_chars(1 + indent.chars().count() as i32);
        buffer.place_cursor(&end);
    } else {
        buffer.insert(&mut end, &format!("\n{deeper}"));
    }
    buffer.end_user_action();
    true
}

/// A closer typed on a blank line goes to the indent of the line above with code on it, or one
/// level out from there when that line opened nothing. Never deeper than where it was typed.
fn on_closer(view: &sourceview5::View, buffer: &sourceview5::Buffer, ch: char) -> bool {
    let at = caret(buffer);
    let before = line_prefix(buffer, &at);
    if before.is_empty() || leading_indent(&before).len() < before.len() || !at.ends_line() {
        return false;
    }
    let Some(above) = (0..at.line())
        .rev()
        .map(|line| line_end(buffer, line))
        .find(|end| !line_prefix(buffer, end).trim().is_empty())
    else {
        return false;
    };
    let line = line_prefix(buffer, &above);
    let indent = leading_indent(&line);
    let target = match opened(buffer, &above) {
        Some(_) => indent,
        None => outdent(indent, width(view)),
    };
    if before.len() <= target.len() || !before.starts_with(target) {
        return false;
    }
    let (mut start, mut end) = (at, at);
    start.set_line_offset(0);
    buffer.begin_user_action();
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &format!("{target}{ch}"));
    buffer.end_user_action();
    true
}

/// The opener the code before `end` on its line ends in, with its closer: spaces and comments
/// passed over, and nothing inside a string.
fn opened(buffer: &sourceview5::Buffer, end: &gtk::TextIter) -> Option<(char, Option<char>)> {
    let mut start = *end;
    start.set_line_offset(0);
    // Highlighting runs in idle time, and the line was typed a moment ago.
    buffer.ensure_highlight(&start, end);
    let mut iter = *end;
    while iter > start {
        iter.backward_char();
        if iter.char().is_whitespace() || buffer.iter_has_context_class(&iter, "comment") {
            continue;
        }
        if buffer.iter_has_context_class(&iter, "string") {
            return None;
        }
        let colon = || {
            let language = buffer.language();
            !language.is_some_and(|l| NO_COLON.contains(&l.id().as_str()))
        };
        return OPENERS
            .into_iter()
            .find(|(open, _)| *open == iter.char() && (*open != ':' || colon()));
    }
    None
}

/// Give each line the caret or the selection touches the indent `new` makes of the line, as one
/// undo step, the selection still holding the whole lines it held: Tab and Shift+Tab over a
/// block. A selection ending at the start of a line leaves that line out, as GtkSourceView's does.
pub(crate) fn reindent(view: &sourceview5::View, new: impl Fn(&str) -> Option<String>) {
    let buffer = view.buffer();
    let at = caret(&buffer);
    let (start, end) = buffer.selection_bounds().unwrap_or((at, at));
    let last = match end.line() > start.line() && end.starts_line() {
        true => end.line() - 1,
        false => end.line(),
    };
    // The buffer's own selection marks would slide past an indent put in at the first line's
    // start, so the selection is held by marks of ours, the start's staying in front of it.
    let backwards = at == start && start != end;
    let from = buffer.create_mark(None, &start, true);
    let to = buffer.create_mark(None, &end, false);
    buffer.begin_user_action();
    for line in start.line()..=last {
        let text = line_prefix(&buffer, &line_end(&buffer, line));
        let old = leading_indent(&text);
        let Some(indent) = new(&text).filter(|indent| indent != old) else {
            continue;
        };
        // Only past what the two share, so a caret in the text keeps its place in it.
        let same = old
            .bytes()
            .zip(indent.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let Some(mut cut) = buffer.iter_at_line_offset(line, same as i32) else {
            continue;
        };
        let mut cut_end = cut;
        cut_end.forward_chars((old.len() - same) as i32);
        buffer.delete(&mut cut, &mut cut_end);
        buffer.insert(&mut cut, &indent[same..]);
    }
    buffer.end_user_action();
    if start != end {
        let (from_at, to_at) = (buffer.iter_at_mark(&from), buffer.iter_at_mark(&to));
        match backwards {
            true => buffer.select_range(&from_at, &to_at),
            false => buffer.select_range(&to_at, &from_at),
        }
    }
    buffer.delete_mark(&from);
    buffer.delete_mark(&to);
}

/// `indent` back to the indent stop before it, every `width` columns, as VS Code's Outdent takes
/// a line: GtkSourceView's own took up to `width` columns off the start, so `\t  ` (six columns)
/// went to two and five spaces to one. It comes off the end, so a makefile's leading tab stays,
/// with spaces making up for a tab that took it past the stop.
pub(crate) fn stop_before(indent: &str, tab_width: usize, width: usize) -> String {
    let stop = columns(indent, tab_width).saturating_sub(1) / width * width;
    let mut kept = indent;
    while columns(kept, tab_width) > stop {
        kept = &kept[..kept.len() - 1];
    }
    format!("{kept}{}", " ".repeat(stop - columns(kept, tab_width)))
}

/// The columns `indent`, spaces and tabs, reaches with a tab stop every `tab_width`.
fn columns(indent: &str, tab_width: usize) -> usize {
    indent.chars().fold(0, |column, c| match c {
        '\t' => (column / tab_width + 1) * tab_width,
        _ => column + 1,
    })
}

/// One indent level in columns: the Indent Width preference, which is the view's tab width.
pub(crate) fn width(view: &sourceview5::View) -> usize {
    match view.indent_width() {
        width if width > 0 => width as usize,
        _ => view.tab_width() as usize,
    }
}

/// One indent level as text: spaces, or a tab where the file wants tabs (a makefile).
fn unit(view: &sourceview5::View) -> String {
    match view.is_insert_spaces_instead_of_tabs() {
        true => " ".repeat(width(view)),
        false => "\t".to_string(),
    }
}

/// `indent` one level out: without its last tab, or without up to `width` of its trailing spaces.
fn outdent(indent: &str, width: usize) -> &str {
    if let Some(rest) = indent.strip_suffix('\t') {
        return rest;
    }
    let spaces = indent.len() - indent.trim_end_matches(' ').len();
    &indent[..indent.len() - spaces.min(width)]
}

#[cfg(test)]
mod tests {
    use super::{outdent, stop_before};

    #[test]
    fn outdent_takes_one_level_off_the_end() {
        assert_eq!(outdent("        ", 4), "    ");
        assert_eq!(outdent("  ", 4), "");
        assert_eq!(outdent("\t\t", 4), "\t");
        assert_eq!(outdent("\t  ", 4), "\t");
        assert_eq!(outdent("", 4), "");
    }

    /// Shift+Tab lands on the stop before the line's indent, never past it: from a stop it is one
    /// level, from between two the nearer, however tabs and spaces are mixed.
    #[test]
    fn back_tab_goes_to_the_stop_before() {
        assert_eq!(stop_before("        ", 4, 4), "    ");
        assert_eq!(stop_before("    ", 4, 4), "");
        assert_eq!(stop_before("", 4, 4), "");
        // Off a stop: GtkSourceView took a whole level off the start, two columns and one.
        assert_eq!(stop_before("      ", 4, 4), "    ");
        assert_eq!(stop_before("     ", 4, 4), "    ");
        // A tab and spaces (six columns) keep the tab, where GtkSourceView kept the spaces.
        assert_eq!(stop_before("\t  ", 4, 4), "\t");
        assert_eq!(stop_before("\t\t", 4, 4), "\t");
        assert_eq!(stop_before("  \t", 4, 4), "");
        // A tab wider than the level is made up with spaces to the stop.
        assert_eq!(stop_before("\t", 8, 4), "    ");
        assert_eq!(stop_before("      ", 2, 2), "    ");
    }
}
