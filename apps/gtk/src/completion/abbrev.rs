//! Lean's backslash abbreviations: `\alpha` offers `α` and `\to` offers `→`, from vscode-lean4's
//! own table (`data/lean/abbreviations.json`, with its NOTICE).
//!
//! Answered here in the window rather than by the language server, which has nothing to say after
//! a backslash: an input method needs no server, so it works before `lake serve` answers, or
//! without one, and puts nothing on an ssh link.

use std::collections::HashMap;
use std::sync::LazyLock;

use accent_api::{Completion, Completions, Kind, Range};

use super::MOST;

/// The GtkSourceView language whose tabs have them.
pub(super) const LANGUAGE: &str = "lean";

/// Every abbreviation's name, the text after its `\`, and what it writes, `$CURSOR` marking where
/// the caret goes. Shortest name first, then by name: the order the rows are listed in, so `\a`
/// shows `α` ahead of every longer name that starts the same.
static TABLE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    let json = include_str!("../../data/lean/abbreviations.json");
    let table: HashMap<String, String> = serde_json::from_str(json).expect("the table is JSON");
    let mut table: Vec<_> = table.into_iter().collect();
    table.sort_unstable_by(|(a, _), (b, _)| (a.chars().count(), a).cmp(&(b.chars().count(), b)));
    table
});

/// The rows for `typed`, an abbreviation from its `\` to the caret ([`typed`]), each replacing
/// `replace`: every entry whose name starts with what follows the `\`, labelled as it is typed
/// beside what it writes. Cut at [`MOST`], and asked for again as the name grows.
pub(super) fn rows(typed: &str, replace: Range) -> Completions {
    let name = typed.strip_prefix('\\').unwrap_or(typed);
    let mut matching = TABLE.iter().filter(|(key, _)| key.starts_with(name));
    let items = matching
        .by_ref()
        .take(MOST)
        .map(|(key, symbol)| row(key, symbol, replace))
        .collect();
    Completions {
        items,
        incomplete: matching.next().is_some(),
        ..Completions::default()
    }
}

fn row(key: &str, symbol: &str, replace: Range) -> Completion {
    let is_snippet = symbol.contains("$CURSOR");
    Completion {
        label: format!("\\{key}"),
        kind: Kind::Text,
        detail: Some(symbol.replace("$CURSOR", "")),
        doc: None,
        filter: None,
        insert: match is_snippet {
            true => snippet(symbol),
            false => symbol.to_string(),
        },
        is_snippet,
        replace,
        extra_edits: Vec::new(),
        resolve: None,
        preselect: true,
    }
}

/// `symbol` as a GtkSourceView snippet: `$CURSOR` becomes the final stop, `$0`, and the
/// characters GtkSourceView's expansion reads in plain text, `\` and `` ` ``, are escaped.
fn snippet(symbol: &str) -> String {
    symbol
        .split("$CURSOR")
        .map(|text| text.replace('\\', "\\\\").replace('`', "\\`"))
        .collect::<Vec<_>>()
        .join("$0")
}

/// The abbreviation being typed at the end of `before`, the line up to the caret: from its `\` on,
/// with no whitespace after it. `\\` is one, the abbreviation for a backslash.
pub(super) fn typed(before: &str) -> Option<&str> {
    let at = before.rfind('\\')?;
    let at = match before[..at].ends_with('\\') {
        true => at - 1,
        false => at,
    };
    let typed = &before[at..];
    (!typed.contains(char::is_whitespace)).then_some(typed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_api::Pos;

    #[test]
    fn a_backslash_offers_the_abbreviations_its_name_starts() {
        let at = |character| Pos { line: 0, character };
        let replace = Range {
            start: at(0),
            end: at(3),
        };
        let al = rows("\\al", replace);
        assert!(al.items.iter().all(|row| row.label.starts_with("\\al")));
        let alpha = al.items.iter().find(|row| row.label == "\\alpha");
        let alpha = alpha.expect("\\al lists \\alpha");
        assert_eq!((alpha.insert.as_str(), alpha.is_snippet), ("α", false));
        assert_eq!(alpha.replace, replace);
        // `$CURSOR` is where the caret goes: the snippet's final stop.
        let norm = &rows("\\norm", replace).items[0];
        assert_eq!(
            (norm.label.as_str(), norm.insert.as_str(), norm.is_snippet),
            ("\\norm", "‖$0‖", true)
        );
        // A bare `\` lists the first rows, and the rest as the name grows.
        assert!(rows("\\", replace).incomplete);
        assert_eq!(typed("x = \\al"), Some("\\al"));
        assert_eq!(typed("\\al x"), None);
        // `\\` is the abbreviation for a backslash.
        assert_eq!(typed("\"\\\\"), Some("\\\\"));
    }
}
