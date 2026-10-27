//! Inline values: while a program is paused, the value of each variable of the call selected,
//! dimmed after the lines from the top of its file to the paused one that name it, as VS Code's
//! `debug.inlineValues` paints them for a language with no provider of its own (debugpy gives its
//! scopes no lines, so every line up to the paused one is looked at).

use super::*;
use sourceview5::AnnotationStyle;
use sourceview5::prelude::*;
use std::collections::{HashMap, HashSet};

/// At most this many variables a scope, as VS Code takes (`MAX_NUM_INLINE_VALUES`): a module's
/// globals can be hundreds.
pub(super) const MOST: usize = 100;
/// A line's values cut to this many characters, as VS Code cuts them
/// (`MAX_INLINE_DECORATOR_LENGTH`), and then to the room the line leaves.
const LONGEST: usize = 150;

/// The values painted, on the one tab with the paused line, through an annotation provider of
/// their own beside the diagnostics'.
#[derive(Default)]
pub(crate) struct Values {
    painted: RefCell<Option<(sourceview5::View, sourceview5::AnnotationProvider)>>,
    /// Each line painted and what it says, for the drills.
    #[cfg(feature = "bench")]
    shown: RefCell<Vec<(i32, String)>>,
}

impl Values {
    /// Paint `values`, name and value, the first of a name winning, after the lines of `tab` from
    /// the first to `paused` that name them. A line a diagnostic's message ends keeps it: two
    /// annotations on one line draw over each other.
    pub(super) fn show(&self, tab: &Tab, paused: u32, values: &[(String, String)]) {
        self.clear();
        let buffer = &tab.buffer;
        let end = buffer
            .iter_at_line(paused as i32 + 1)
            .unwrap_or_else(|| buffer.end_iter());
        // Highlighting runs in idle time: what is a comment or a string is known only once it
        // has.
        buffer.ensure_highlight(&buffer.start_iter(), &end);
        let mut known = HashMap::new();
        for (name, value) in values {
            known.entry(name.as_str()).or_insert(value.as_str());
        }
        let provider = sourceview5::AnnotationProvider::new();
        #[cfg(feature = "bench")]
        let mut shown = Vec::new();
        for line in 0..=paused as i32 {
            let Some(start) = buffer.iter_at_line(line) else {
                break;
            };
            if tab.has_message(line) || crate::diagnostics::out_of_sight(&start) {
                continue;
            }
            let mut stop = start;
            // From an empty line's end, this would go on to the next line's.
            if !stop.ends_line() {
                stop.forward_to_line_end();
            }
            let text = buffer.text(&start, &stop, true);
            let code = |at: usize| {
                let iter = buffer.iter_at_offset(start.offset() + at as i32);
                !["comment", "string"]
                    .iter()
                    .any(|class| buffer.iter_has_context_class(&iter, class))
            };
            let named = named(&text, &known, code);
            if named.is_empty() {
                continue;
            }
            let said = cut(&named.join(", "), LONGEST);
            let fits = crate::diagnostics::fit(&tab.view, line, said);
            let annotation = sourceview5::Annotation::new(
                Some(&fits),
                None::<gtk::gio::Icon>,
                line,
                AnnotationStyle::None,
            );
            provider.add_annotation(&annotation);
            #[cfg(feature = "bench")]
            shown.push((line, fits));
        }
        tab.view.annotations().add_provider(&provider);
        self.painted.replace(Some((tab.view.clone(), provider)));
        #[cfg(feature = "bench")]
        self.shown.replace(shown);
    }

    /// Take the values away: the program moved on, or another call is selected.
    pub(super) fn clear(&self) {
        if let Some((view, provider)) = self.painted.take() {
            view.annotations().remove_provider(&provider);
        }
        #[cfg(feature = "bench")]
        self.shown.take();
    }

    /// Each line painted, from 1, and what it says.
    #[cfg(feature = "bench")]
    pub(crate) fn shown(&self) -> Vec<String> {
        let shown = self.shown.borrow();
        shown
            .iter()
            .map(|(line, said)| format!("{}:{said}", line + 1))
            .collect()
    }
}

/// `name = value` for each name of `known` that `line` holds as a word where `code` says the
/// character at that index is code, not a comment or a string: each once, in the order the line
/// first names them, the value on one line.
fn named(line: &str, known: &HashMap<&str, &str>, code: impl Fn(usize) -> bool) -> Vec<String> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut seen = HashSet::new();
    let mut said = Vec::new();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut at = 0;
    while at < chars.len() {
        if !word(chars[at].1) {
            at += 1;
            continue;
        }
        let first = at;
        while at < chars.len() && word(chars[at].1) {
            at += 1;
        }
        let end = chars.get(at).map_or(line.len(), |(byte, _)| *byte);
        let name = &line[chars[first].0..end];
        if let Some(value) = known.get(name)
            && !seen.contains(name)
            && code(first)
        {
            seen.insert(name);
            said.push(format!(
                "{name} = {}",
                value.replace(['\n', '\r', '\t'], " ")
            ));
        }
    }
    said
}

/// `text` cut to `most` characters, an ellipsis saying it was.
fn cut(text: &str, most: usize) -> String {
    match text.char_indices().nth(most) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_says_the_values_it_names_once_in_its_order_and_not_in_comments() {
        let known = HashMap::from([("x", "21"), ("y", "42"), ("total", "'a\nb'")]);
        let line = "y = x * 2 + x  # total";
        let comment = line.find('#').unwrap();
        assert_eq!(named(line, &known, |at| at < comment), ["y = 42", "x = 21"]);
        assert_eq!(named("totals = total", &known, |_| true), ["total = 'a b'"]);
        assert_eq!(cut("abcdef", 3), "abc…");
        assert_eq!(cut("abc", 3), "abc");
    }
}
