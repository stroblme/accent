//! Citation keys follow fixbib's author/title/year convention, without renaming existing keys.
use super::*;

pub const DEFAULT_PATTERN: &str = "{author}_{title}_{year}";

pub(super) fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Library {
    /// Supported placeholders are deliberately small and shared by every entry type.
    pub fn generate_key(&self, index: usize, pattern: &str) -> Result<String> {
        let e = self
            .entries
            .get(index)
            .ok_or_else(|| invalid("entry no longer exists"))?;
        let authors = self.value(e, "author");
        let authors = if authors.is_empty() {
            self.value(e, "editor")
        } else {
            authors
        };
        let first = authors.split(" and ").next().unwrap_or("");
        let family = first
            .split_once(',')
            .map(|(s, _)| s)
            .unwrap_or_else(|| first.split_whitespace().last().unwrap_or("anon"));
        let author = plain(family).replace(' ', "");
        let title = plain(&self.value(e, "title"));
        let title = title
            .split_whitespace()
            .find(|s| {
                ![
                    "a", "an", "the", "on", "of", "for", "and", "or", "in", "to", "with", "by",
                    "from",
                ]
                .contains(s)
            })
            .unwrap_or("untitled");
        let year = self.year(e);
        let base = pattern
            .replace("{author}", if author.is_empty() { "anon" } else { &author })
            .replace("{title}", title)
            .replace("{year}", if year.is_empty() { "0000" } else { &year });
        if !valid_key(&base) {
            return Err(invalid(
                "key pattern must use {author}, {title}, {year} and citation-key characters",
            ));
        }
        let mut key = base.clone();
        let mut n = 2;
        while self
            .entries
            .iter()
            .enumerate()
            .any(|(i, e)| i != index && e.key == key)
        {
            key = format!("{base}_{n}");
            n += 1;
        }
        Ok(key)
    }
}

pub use DEFAULT_PATTERN as PATTERN;

/// How a document cites `keys`: LaTeX's `\cite{a,b}`, or Typst's `@a @b`, a key its references
/// cannot spell (a `/` in it, a `.` or `:` at its end) as `#cite(label("key"))`.
pub fn citation(keys: &[&str], typst: bool) -> String {
    if !typst {
        return format!("\\cite{{{}}}", keys.join(","));
    }
    let cite = |key: &&str| match referable(key) {
        true => format!("@{key}"),
        false => {
            let quoted = key.replace('\\', "\\\\").replace('"', "\\\"");
            format!("#cite(label(\"{quoted}\"))")
        }
    };
    keys.iter().map(cite).collect::<Vec<_>>().join(" ")
}

/// Whether `@key` reads as a reference to `key`: Typst takes letters, digits, `_`, `-`, `.` and
/// `:` after an `@`, and leaves a `.` or `:` at the end to the sentence.
fn referable(key: &str) -> bool {
    let allowed = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':');
    !key.is_empty() && key.chars().all(allowed) && !key.ends_with(['.', ':'])
}
