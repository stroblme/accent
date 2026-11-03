//! A query's qualifiers ([`crate::query::Filter`]) as SQL over the `files` row a statement names,
//! each one a condition ANDed onto the statement's own.

use super::search::{fold, fts_term};
use crate::markdown::link_key;
use crate::query::{Filter, On};
use rusqlite::types::Value;

/// The file one link key names, by [`super::links`]'s rule (`BEST_FILE`): the shortest path that
/// answers to it, the older row breaking a tie. `{}` is the key's parameter.
const BEST_ID: &str = "SELECT k.file_id FROM file_keys k JOIN files b ON b.id = k.file_id
     WHERE k.key = {} ORDER BY length(b.rel_path), b.id LIMIT 1";

/// The links that quote a PDF's highlight (`Index::pdf_links`).
const HIGHLIGHT: &str = "anchor LIKE 'page=%selection=%'";

/// `filters` as ` AND (…)` conditions on the row `alias` names, their values numbered from `?first`
/// on, in the order the returned values hold them.
pub(super) fn sql(filters: &[Filter], alias: &str, first: usize) -> (String, Vec<Value>) {
    let (mut sql, mut values) = (String::new(), Vec::new());
    for filter in filters {
        let mut param = |value: Value| {
            values.push(value);
            format!("?{}", first + values.len() - 1)
        };
        let id = format!("{alias}.id");
        let cond = match &filter.on {
            On::Term(text) => format!(
                "{id} IN (SELECT rowid FROM notes_fts WHERE notes_fts MATCH {})",
                param(Value::Text(fts_term(text, false)))
            ),
            On::Tag(tags) => {
                let names: Vec<String> = tags
                    .iter()
                    .map(|tag| {
                        let p = param(Value::Text(fold(tag)));
                        format!("fold(name) = {p} OR substr(fold(name), 1, length({p}) + 1) = {p} || '/'")
                    })
                    .collect();
                format!(
                    "{id} IN (SELECT file_id FROM tags WHERE {})",
                    names.join(" OR ")
                )
            }
            On::Path(path) => format!(
                "phrase_start({alias}.rel_path, {}) IS NOT NULL",
                param(Value::Text(fold(path)))
            ),
            On::Kind(docs) => {
                let docs: Vec<String> = docs.iter().map(|d| d.as_i64().to_string()).collect();
                format!("{alias}.doc IN ({})", docs.join(", "))
            }
            On::Links(target) => {
                let p = param(Value::Text(link_key(target)));
                format!(
                    "{id} IN (SELECT src_file FROM links WHERE resolved_file = ({}) \
                     OR (resolved_file IS NULL AND key = {p}))",
                    BEST_ID.replace("{}", &p)
                )
            }
            On::LinkedFrom(target) => {
                let p = param(Value::Text(link_key(target)));
                format!(
                    "{id} IN (SELECT resolved_file FROM links WHERE src_file = ({}) \
                     AND resolved_file IS NOT NULL)",
                    BEST_ID.replace("{}", &p)
                )
            }
            // `IS NOT NULL`, because a `NULL` in the list would make every `NOT IN` unknown.
            On::Highlights => format!(
                "{id} IN (SELECT src_file FROM links WHERE {HIGHLIGHT} UNION \
                 SELECT resolved_file FROM links WHERE {HIGHLIGHT} AND resolved_file IS NOT NULL)"
            ),
            On::Modified { lo, hi } => format!(
                "{alias}.mtime_ns >= {} AND {alias}.mtime_ns < {}",
                param(Value::Integer(*lo)),
                param(Value::Integer(*hi))
            ),
        };
        let not = if filter.not { "NOT " } else { "" };
        sql.push_str(&format!(" AND {not}({cond})"));
    }
    (sql, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Doc;

    #[test]
    fn each_filter_is_one_condition_numbered_on() {
        let filters = [
            Filter {
                not: false,
                on: On::Tag(vec!["Paper".into(), "a/b".into()]),
            },
            Filter {
                not: true,
                on: On::Kind(vec![Doc::Pdf, Doc::Note]),
            },
            Filter {
                not: false,
                on: On::Modified { lo: 1, hi: 2 },
            },
        ];
        let (sql, values) = sql(&filters, "g", 7);
        assert!(sql.starts_with(" AND (g.id IN (SELECT file_id FROM tags WHERE fold(name) = ?7"));
        assert!(sql.contains("= ?8 || '/'"), "{sql}");
        assert!(sql.contains(" AND NOT (g.doc IN (2, 1))"), "{sql}");
        assert!(
            sql.ends_with(" AND (g.mtime_ns >= ?9 AND g.mtime_ns < ?10)"),
            "{sql}"
        );
        assert_eq!(
            values,
            [
                Value::Text("paper".into()),
                Value::Text("a/b".into()),
                Value::Integer(1),
                Value::Integer(2)
            ]
        );
        assert_eq!(super::sql(&[], "g", 1), (String::new(), Vec::new()));
    }
}
