//! A code file's declarations as the index keeps them (`code::symbols`), and what reads them: an
//! outline, the declarations a name finds, and where a name is written — a caller, by name.

use super::Index;
use crate::code::{self, SymbolKind};
use anyhow::Result;
use regex::Regex;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// One declaration the index holds, and the file it is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub rel_path: String,
    pub kind: SymbolKind,
    pub name: String,
    /// The type a method belongs to.
    pub container: Option<String>,
    pub byte_start: i64,
    pub byte_end: i64,
    /// 1-based.
    pub line: u32,
    pub end_line: u32,
    pub signature: String,
    pub test: bool,
}

/// Where [`Index::mentions`] found a name: the line, and the declaration around it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    pub rel_path: String,
    /// 1-based.
    pub line: u32,
    /// The line, trimmed, at most 200 characters of it.
    pub text: String,
    /// The innermost declaration the name is written in, `None` at a file's top level.
    pub symbol: Option<CodeSymbol>,
}

const COLUMNS: &str = "f.rel_path, s.kind, s.name, s.container, s.byte_start, s.byte_end, \
                       s.line, s.end_line, s.signature, s.test";

fn symbol(r: &rusqlite::Row<'_>) -> rusqlite::Result<CodeSymbol> {
    Ok(CodeSymbol {
        rel_path: r.get(0)?,
        kind: SymbolKind::from_i64(r.get(1)?),
        name: r.get(2)?,
        container: r.get(3)?,
        byte_start: r.get(4)?,
        byte_end: r.get(5)?,
        line: r.get(6)?,
        end_line: r.get(7)?,
        signature: r.get(8)?,
        test: r.get(9)?,
    })
}

/// A name's words, lower-cased: `settle_index` and `SettleIndex` are `settle`, `index`.
fn name_parts(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut part = String::new();
    let mut lower = false;
    for c in name.chars() {
        if (!c.is_alphanumeric() || c.is_uppercase() && lower) && !part.is_empty() {
            out.push(std::mem::take(&mut part));
        }
        if c.is_alphanumeric() {
            part.extend(c.to_lowercase());
        }
        lower = c.is_lowercase() || c.is_ascii_digit();
    }
    if !part.is_empty() {
        out.push(part);
    }
    out
}

/// `text` for a `LIKE` that takes it literally: identifiers are full of `_`.
fn like_literal(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl Index {
    /// A file's declarations, in the order they start.
    pub fn file_symbols(&self, rel: &str) -> Result<Vec<CodeSymbol>> {
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.rel_path = ?1 ORDER BY s.byte_start"
        ))?;
        let rows = st.query_map([rel], symbol)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The declarations `query` names — `name`, `Container::name` or `Container.name` — best
    /// first: the name itself, case aside, then the names it starts, then those holding it, a
    /// test's after the rest at each step; at most `limit`.
    pub fn find_symbols(&self, query: &str, limit: usize) -> Result<Vec<CodeSymbol>> {
        let query = query.trim();
        let (container, name) = match query.rsplit_once("::").or_else(|| query.rsplit_once('.')) {
            Some((c, n)) if !c.is_empty() && !n.is_empty() => {
                (Some(c.rsplit(['.', ':']).next().unwrap_or(c)), n)
            }
            _ => (None, query),
        };
        if name.is_empty() {
            return Ok(Vec::new());
        }
        let like = like_literal(name);
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE (?2 IS NULL OR s.container = ?2 COLLATE NOCASE)
               AND CASE ?4
                   WHEN 0 THEN s.name = ?1 COLLATE NOCASE
                   WHEN 1 THEN s.name LIKE ?3 || '%' ESCAPE '\\' AND s.name <> ?1 COLLATE NOCASE
                   ELSE s.name LIKE '%' || ?3 || '%' ESCAPE '\\'
                        AND NOT s.name LIKE ?3 || '%' ESCAPE '\\'
                   END
             ORDER BY s.test, s.name <> ?1, length(s.name), length(f.rel_path), f.rel_path,
                      s.byte_start
             LIMIT ?5"
        ))?;
        let mut out = Vec::new();
        for tier in 0..3 {
            let room = limit.saturating_sub(out.len());
            if room == 0 {
                break;
            }
            let rows = st.query_map(params![name, container, like, tier, room as i64], symbol)?;
            out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        Ok(out)
    }

    /// The declarations of each of `names`, at most `per` a name, those in the file `near` first:
    /// what a call by one of those names may reach.
    pub fn symbols_named(
        &self,
        names: &[String],
        near: &str,
        per: usize,
    ) -> Result<Vec<CodeSymbol>> {
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name = ?1 COLLATE NOCASE AND s.name = ?1
             ORDER BY f.rel_path <> ?2, s.test, length(f.rel_path), f.rel_path, s.byte_start
             LIMIT ?3"
        ))?;
        let mut out = Vec::new();
        for name in names {
            let rows = st.query_map(params![name, near, per as i64], symbol)?;
            out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        Ok(out)
    }

    /// The declarations whose names are made of a question's `words` — `settle_index` for
    /// "settle the index", `Worker` for "the worker" — each with how many of its name's parts
    /// the words hold; best first, at most `limit`. A name of one part counts when it is a
    /// type's: a function of one common word would answer every question.
    pub fn symbols_by_words(
        &self,
        words: &[String],
        limit: usize,
    ) -> Result<Vec<(CodeSymbol, usize)>> {
        let words: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
        let holds = |part: &str| {
            words.iter().any(|w| {
                part == w
                    || w.len() >= 4 && part.starts_with(w.as_str())
                    || part.len() >= 4 && w.starts_with(part)
            })
        };
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM symbols s JOIN files f ON f.id = s.file_id"
        ))?;
        let mut out = Vec::new();
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let name: String = r.get(2)?;
            let parts = name_parts(&name);
            let held = parts.iter().filter(|p| holds(p)).count();
            let kind = SymbolKind::from_i64(r.get(1)?);
            let typed = matches!(
                kind,
                SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Trait | SymbolKind::Class
            );
            if held >= 2 || held == 1 && parts.len() == 1 && typed {
                out.push((symbol(r)?, held, held == parts.len()));
            }
        }
        out.sort_by(|a, b| {
            (b.1, b.2, !b.0.test)
                .cmp(&(a.1, a.2, !a.0.test))
                .then_with(|| a.0.rel_path.len().cmp(&b.0.rel_path.len()))
                .then_with(|| (&a.0.rel_path, a.0.byte_start).cmp(&(&b.0.rel_path, b.0.byte_start)))
        });
        out.truncate(limit);
        Ok(out.into_iter().map(|(s, held, _)| (s, held)).collect())
    }

    /// Where `name` is written in the code the index holds, as a whole word outside comments and
    /// strings, each with the innermost declaration around it, by file; the declarations of
    /// `name` themselves left out. At most `limit`.
    ///
    /// The full-text index narrows the files to those holding the name's words, and those are
    /// read for it: a name is known by its spelling, not by what it resolves to, so a `new` finds
    /// every `new`.
    pub fn mentions(&self, name: &str, limit: usize) -> Result<Vec<Mention>> {
        let words: Vec<&str> = name
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let re = Regex::new(&format!(r"\b{}\b", regex::escape(name)))?;
        let mut st = self.conn.prepare_cached(
            "SELECT f.rel_path, n.body FROM notes_fts
             JOIN notes n ON n.file_id = notes_fts.rowid JOIN files f ON f.id = n.file_id
             WHERE notes_fts MATCH ?1 AND EXISTS (SELECT 1 FROM symbols s WHERE s.file_id = f.id)
             ORDER BY f.rel_path",
        )?;
        let phrase = format!("\"{}\"", words.join(" "));
        let files = st
            .query_map([phrase], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::new();
        for (rel, body) in files {
            let Some(lang) = code::lang_of(&rel) else {
                continue;
            };
            let masked = code::mask(lang, &body);
            let at: Vec<usize> = re.find_iter(&masked).map(|m| m.start()).collect();
            if at.is_empty() {
                continue;
            }
            let symbols = self.file_symbols(&rel)?;
            let starts: Vec<usize> = std::iter::once(0)
                .chain(body.match_indices('\n').map(|(i, _)| i + 1))
                .collect();
            for byte in at {
                let line = starts.partition_point(|&s| s <= byte);
                let inner = symbols
                    .iter()
                    .filter(|s| (s.byte_start as usize) <= byte && byte < s.byte_end as usize)
                    .min_by_key(|s| s.byte_end - s.byte_start);
                if inner.is_some_and(|s| s.name == name && s.line as usize == line) {
                    continue;
                }
                let start = starts[line - 1];
                let end = body[start..].find('\n').map_or(body.len(), |n| start + n);
                out.push(Mention {
                    rel_path: rel.clone(),
                    line: line as u32,
                    text: body[start..end].trim().chars().take(200).collect(),
                    symbol: inner.cloned(),
                });
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::code::SymbolKind;
    use crate::index::testing::{fixture, open};
    use std::fs;

    const LIB: &str = "pub struct Index;\n\nimpl Index {\n    pub fn backlinks(&self) {}\n    \
                       pub fn links_from(&self) {\n        self.backlinks();\n    }\n}\n";
    const USE: &str =
        "// backlinks in a comment\nfn caller(ix: &Index) {\n    ix.backlinks();\n}\n";

    /// A code file's declarations go into the index with it, and change and go with it.
    #[test]
    fn a_code_files_declarations_follow_it() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("lib.rs"), LIB).unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let names = |ix: &crate::index::Index| -> Vec<(SymbolKind, String, Option<String>, u32)> {
            ix.file_symbols("lib.rs")
                .unwrap()
                .into_iter()
                .map(|s| (s.kind, s.name, s.container, s.line))
                .collect()
        };
        assert_eq!(
            names(&ix),
            [
                (SymbolKind::Struct, "Index".into(), None, 1),
                (
                    SymbolKind::Method,
                    "backlinks".into(),
                    Some("Index".into()),
                    4
                ),
                (
                    SymbolKind::Method,
                    "links_from".into(),
                    Some("Index".into()),
                    5
                ),
            ]
        );
        // A note is no code, whatever it says.
        assert!(ix.file_symbols("a.md").unwrap().is_empty());

        fs::write(vault.path().join("lib.rs"), "fn only() {}\n").unwrap();
        ix.update_file(vault.path(), "lib.rs").unwrap();
        assert_eq!(names(&ix), [(SymbolKind::Function, "only".into(), None, 1)]);
        fs::remove_file(vault.path().join("lib.rs")).unwrap();
        ix.update_file(vault.path(), "lib.rs").unwrap();
        assert!(names(&ix).is_empty());
    }

    /// A name finds its declarations exactly first, then as a prefix, then inside; a container
    /// narrows them; a test file's come last.
    #[test]
    fn find_symbols_by_name_and_container() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("lib.rs"), LIB).unwrap();
        fs::create_dir(vault.path().join("tests")).unwrap();
        fs::write(vault.path().join("tests/t.rs"), "fn backlinks() {}\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let found = |q: &str| -> Vec<(String, String)> {
            ix.find_symbols(q, 10)
                .unwrap()
                .into_iter()
                .map(|s| (s.rel_path, s.name))
                .collect()
        };
        assert_eq!(
            found("BACKLINKS"),
            [
                ("lib.rs".into(), "backlinks".into()),
                ("tests/t.rs".into(), "backlinks".into())
            ]
        );
        assert_eq!(
            found("Index::backlinks"),
            [("lib.rs".into(), "backlinks".into())]
        );
        assert_eq!(
            found("links"),
            [
                ("lib.rs".into(), "links_from".into()),
                ("lib.rs".into(), "backlinks".into()),
                ("tests/t.rs".into(), "backlinks".into())
            ]
        );
        // `_` is a letter of the name, not a `LIKE` wildcard.
        assert!(found("links_x").is_empty());
    }

    /// A question's words find the declarations named by them, the type named by one alone.
    #[test]
    fn symbols_by_words_of_the_question() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("lib.rs"),
            "struct Worker;\nfn settle_index() {}\nfn SettleAll() {}\nfn settle() {}\n",
        )
        .unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let words: Vec<String> = ["worker", "settles", "index"].map(String::from).into();
        let found: Vec<(String, usize)> = ix
            .symbols_by_words(&words, 10)
            .unwrap()
            .into_iter()
            .map(|(s, n)| (s.name, n))
            .collect();
        assert_eq!(found, [("settle_index".into(), 2), ("Worker".into(), 1)]);
        assert_eq!(super::name_parts("SettleAll_x2"), ["settle", "all", "x2"]);
    }

    /// Where a name is written, in code and outside comments, with the declaration around it;
    /// its own declaration is not one of them.
    #[test]
    fn mentions_name_the_declaration_they_are_in() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("lib.rs"), LIB).unwrap();
        fs::write(vault.path().join("use.rs"), USE).unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let found: Vec<(String, u32, Option<String>)> = ix
            .mentions("backlinks", 10)
            .unwrap()
            .into_iter()
            .map(|m| (m.rel_path, m.line, m.symbol.map(|s| s.name)))
            .collect();
        assert_eq!(
            found,
            [
                ("lib.rs".into(), 6, Some("links_from".into())),
                ("use.rs".into(), 3, Some("caller".into())),
            ]
        );
        assert_eq!(ix.mentions("backlinks", 1).unwrap().len(), 1);

        let named = ix
            .symbols_named(&["backlinks".into(), "caller".into()], "use.rs", 5)
            .unwrap();
        let named: Vec<&str> = named.iter().map(|s| s.rel_path.as_str()).collect();
        assert_eq!(named, ["lib.rs", "use.rs"]);
    }
}
