//! What a ranked search query says (user decisions 2026-11-03): free words, found in any order,
//! `"quoted phrases"`, and qualifiers that narrow the files those are looked for in.
//!
//! A `-` in front of any token leaves out what it names. Qualifier names are read in any case, and
//! a name this does not know is text (`std::vec`, `TODO:`, `http://…`), while one it knows with
//! nothing after it yet (`tag:` as it is typed) is passed over. A known name with a value it cannot
//! read is an error that says what it takes; a quoted token is always text (`"#include"`).

use crate::markdown::is_tag_char;
use crate::path::{FileType, file_type, is_notebook, is_org};
use crate::{Error, Result};
use chrono::{DateTime, Duration, NaiveDate, TimeZone};

/// A query once read: what the text has to hold, and what the files holding it have to be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// The words and phrases the text holds, every one of them in any order, as typed.
    pub text: Vec<Term>,
    pub filters: Vec<Filter>,
}

/// A free word, or a quoted phrase with its quotes taken off and its spaces made single.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    pub text: String,
    /// Matched as the start of a word: the query's last token, which is still being typed, unless
    /// a closing quote ended it.
    pub prefix: bool,
}

/// One qualifier, or one word left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Written with a `-`: the files it names are the ones left out.
    pub not: bool,
    pub on: On,
}

/// What a [`Filter`] narrows by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum On {
    /// A word or phrase in the text, as `-word` names one.
    Term(String),
    /// Any of these tags, as written without their `#`, or a tag nested under one (`a` takes `a/b`).
    Tag(Vec<String>),
    /// A piece of the file's vault path.
    Path(String),
    Kind(Vec<Doc>),
    /// The files linking to the one this link target names, or waiting for it if nothing does.
    Links(String),
    /// The files the one this link target names links to.
    LinkedFrom(String),
    /// A note holding a link to a PDF's highlight, or a PDF one points into.
    Highlights,
    /// Modified in `[lo, hi)`, in nanoseconds since the epoch as the index counts a file's time.
    Modified {
        lo: i64,
        hi: i64,
    },
}

/// What `kind:` tells apart, and what the index keeps for every file (`files.doc`, 0 for a folder,
/// a conflict copy or a name none of these is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doc {
    /// Markdown and org.
    Note = 1,
    Pdf = 2,
    /// A draw.io diagram, named as one or an `.xml` whose bytes say so.
    Diagram = 3,
    /// A Jupyter notebook, or a `.py` marimo wrote.
    Notebook = 4,
    Code = 5,
    /// Plain text, and the tables and configuration files that are text to read.
    Text = 6,
    Image = 7,
}

impl Doc {
    pub const ALL: [Doc; 7] = [
        Doc::Note,
        Doc::Pdf,
        Doc::Diagram,
        Doc::Notebook,
        Doc::Code,
        Doc::Text,
        Doc::Image,
    ];

    /// The word `kind:` takes for it.
    pub fn name(self) -> &'static str {
        match self {
            Doc::Note => "note",
            Doc::Pdf => "pdf",
            Doc::Diagram => "diagram",
            Doc::Notebook => "notebook",
            Doc::Code => "code",
            Doc::Text => "text",
            Doc::Image => "image",
        }
    }

    pub fn as_i64(self) -> i64 {
        self as i64
    }

    /// What `rel` is by its name alone: a `.py` is code and an `.xml` text until the index has
    /// read them ([`crate::path::holds_notebook`], [`crate::path::holds_diagram`]).
    pub fn named(rel: &str) -> Option<Doc> {
        if is_org(rel) {
            return Some(Doc::Note);
        }
        if is_notebook(rel) {
            return Some(Doc::Notebook);
        }
        Some(match file_type(rel) {
            FileType::Note => Doc::Note,
            FileType::Pdf => Doc::Pdf,
            FileType::Diagram => Doc::Diagram,
            FileType::Image => Doc::Image,
            FileType::Code => Doc::Code,
            FileType::Table | FileType::Config | FileType::Text => Doc::Text,
            FileType::Other => return None,
        })
    }
}

/// The qualifier names, as a query writes them.
const NAMES: [&str; 7] = [
    "tag",
    "path",
    "kind",
    "links",
    "linked-from",
    "has",
    "modified",
];

/// Read `q`, telling the dates `modified:` names by `now` and its zone.
pub fn parse<Tz: TimeZone>(q: &str, now: &DateTime<Tz>) -> Result<Query> {
    let mut out = Query::default();
    for token in tokens(q) {
        let class = class(&token);
        // Punctuation alone is no word the index holds, so it is neither looked for nor left out.
        if let Class::Text(text) = &class
            && !text.chars().any(char::is_alphanumeric)
        {
            continue;
        }
        // Anything after a word says it is typed in full.
        if let Some(term) = out.text.last_mut() {
            term.prefix = false;
        }
        let on = match class {
            Class::Text(text) if token.not => On::Term(text),
            Class::Text(text) => {
                let prefix = !token.closed;
                out.text.push(Term { text, prefix });
                continue;
            }
            Class::Tag(tag) => On::Tag(vec![tag]),
            Class::Named(name, value) => match qualifier(name, &value, now)? {
                Some(on) => on,
                None => continue,
            },
        };
        out.filters.push(Filter { not: token.not, on });
    }
    Ok(out)
}

/// The words and phrases `q` looks for, as typed, whatever its qualifiers say and whether or not
/// they can be read: what a row's further matches are found by.
pub fn free_text(q: &str) -> Vec<String> {
    tokens(q)
        .iter()
        .filter(|token| !token.not)
        .filter_map(|token| match class(token) {
            Class::Text(text) if text.chars().any(char::is_alphanumeric) => Some(text),
            _ => None,
        })
        .collect()
}

/// One whitespace-separated token of a query, its quotes taken off.
#[derive(Debug, Default)]
struct Token {
    not: bool,
    text: String,
    /// It opened with a quote, so it is text whatever it says.
    quoted: bool,
    /// It ended with a closing quote, so its last word is whole.
    closed: bool,
}

/// Cut `q` at whitespace, a quote holding what it encloses together up to its closing quote or the
/// end of the query, so `path:"My Folder"` and `"a b` are one token each; a wikilink's brackets
/// hold theirs together too, so `links:[[My Note]]` is one.
fn tokens(q: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = q.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            return out;
        };
        let mut token = Token::default();
        if first == '-' {
            chars.next();
            match chars.peek() {
                Some(c) if !c.is_whitespace() => token.not = true,
                _ => token.text.push('-'),
            }
        }
        token.quoted = chars.peek() == Some(&'"');
        while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
            token.closed = false;
            if c == '[' && chars.peek() == Some(&'[') {
                token.text.push(c);
                for c in chars.by_ref() {
                    token.text.push(c);
                    if token.text.ends_with("]]") {
                        break;
                    }
                }
                continue;
            }
            if c != '"' {
                token.text.push(c);
                continue;
            }
            let mut inner = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    token.closed = true;
                    break;
                }
                inner.push(c);
            }
            // A phrase's words are matched across any run of whitespace, so one space stands for it.
            token
                .text
                .push_str(&inner.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        out.push(token);
    }
}

/// What a token is before its value is read.
enum Class {
    Text(String),
    Tag(String),
    /// A known qualifier's name, lower-cased, and its value.
    Named(&'static str, String),
}

fn class(token: &Token) -> Class {
    let text = &token.text;
    if !token.quoted {
        if let Some((name, value)) = text.split_once(':')
            && let Some(name) = NAMES.iter().find(|n| n.eq_ignore_ascii_case(name))
        {
            return Class::Named(name, value.to_string());
        }
        if let Some(tag) = text.strip_prefix('#')
            && is_tag(tag)
        {
            return Class::Tag(tag.to_string());
        }
    }
    Class::Text(text.clone())
}

/// Whether `tag` is a tag's name as a note writes one after its `#` (`markdown::frontmatter`).
fn is_tag(tag: &str) -> bool {
    !tag.is_empty() && tag.chars().all(is_tag_char) && !tag.bytes().all(|b| b.is_ascii_digit())
}

/// The filter a known qualifier names, `None` while its value is still empty.
fn qualifier<Tz: TimeZone>(name: &str, value: &str, now: &DateTime<Tz>) -> Result<Option<On>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let wrong = |what: &str| Err(Error::Invalid(format!("{name}:{value} {what}")));
    let list = || value.split(',').map(str::trim).filter(|v| !v.is_empty());
    Ok(Some(match name {
        "tag" => {
            let tags: Vec<String> = list()
                .map(|t| t.trim_start_matches('#').to_string())
                .filter(|t| !t.is_empty())
                .collect();
            if !tags.iter().all(|t| is_tag(t)) {
                return wrong("is not a tag. A tag is letters, digits, _, - and /.");
            }
            match tags.is_empty() {
                true => return Ok(None),
                false => On::Tag(tags),
            }
        }
        "path" => On::Path(value.to_string()),
        "kind" => match list().map(doc).collect::<Option<Vec<Doc>>>() {
            Some(docs) if !docs.is_empty() => On::Kind(docs),
            Some(_) => return Ok(None),
            None => {
                return wrong(
                    "is not a kind. Use note, pdf, diagram, notebook, code, text or image.",
                );
            }
        },
        "links" | "linked-from" => {
            let target = link_target(value);
            if target.is_empty() {
                return Ok(None);
            }
            match name {
                "links" => On::Links(target),
                _ => On::LinkedFrom(target),
            }
        }
        "has" => match "highlights".starts_with(&value.to_ascii_lowercase()) {
            true => On::Highlights,
            false => return wrong("is not something a file has. Use has:highlights."),
        },
        "modified" => match modified(value, now) {
            Some((lo, hi)) => On::Modified { lo, hi },
            None => return wrong("is not a date. Use 7d, 2w, 12h or 2026-10-01."),
        },
        _ => unreachable!("{name} is in NAMES"),
    }))
}

/// The kind `v` names, whole or by a beginning only it has (`pd`).
fn doc(v: &str) -> Option<Doc> {
    let v = v.to_ascii_lowercase();
    if let Some(doc) = Doc::ALL.into_iter().find(|d| d.name() == v) {
        return Some(doc);
    }
    let mut starting = Doc::ALL.into_iter().filter(|d| d.name().starts_with(&v));
    match (starting.next(), starting.next()) {
        (Some(doc), None) => Some(doc),
        _ => None,
    }
}

/// The file a `links:` value names, as a link writes it: `Note`, `[[My Note]]`, `[[n|alias]]`,
/// `dir/n.md`, a `#heading` after it read past.
fn link_target(v: &str) -> String {
    let v = v.trim_start_matches("[[").trim_end_matches("]]");
    let v = v.split('|').next().unwrap_or(v);
    let v = v.split('#').next().unwrap_or(v);
    v.trim().to_string()
}

/// `modified:`'s value as nanoseconds `[lo, hi)`. A date is the local day; `7d` (also `h` and `w`)
/// is that long before `now`. Later is greater, so `>7d` and a bare `7d` are the last seven days,
/// `<7d` before them, `<2026-10-01` before that day, `2026-10-01..2026-10-15` both days and those
/// between.
fn modified<Tz: TimeZone>(v: &str, now: &DateTime<Tz>) -> Option<(i64, i64)> {
    let (op, rest) = [">=", "<=", ">", "<"]
        .into_iter()
        .find_map(|op| v.strip_prefix(op).map(|rest| (op, rest)))
        .unwrap_or(("", v));
    if op.is_empty()
        && let Some((from, to)) = rest.split_once("..")
    {
        return Some((day(from, now)?.0, day(to, now)?.1));
    }
    if let Some(at) = ago(rest, now) {
        return Some(match op {
            "<" | "<=" => (i64::MIN, at),
            _ => (at, i64::MAX),
        });
    }
    let (start, end) = day(rest, now)?;
    Some(match op {
        "" => (start, end),
        ">" => (end, i64::MAX),
        ">=" => (start, i64::MAX),
        "<" => (i64::MIN, start),
        _ => (i64::MIN, end),
    })
}

/// `12h`, `7d` or `2w` before `now`.
fn ago<Tz: TimeZone>(v: &str, now: &DateTime<Tz>) -> Option<i64> {
    let unit = v.chars().last()?;
    let n: i64 = v[..v.len() - unit.len_utf8()].parse().ok()?;
    let span = match unit.to_ascii_lowercase() {
        'h' => Duration::try_hours(n)?,
        'd' => Duration::try_days(n)?,
        'w' => Duration::try_weeks(n)?,
        _ => return None,
    };
    Some(nanos(&now.clone().checked_sub_signed(span)?))
}

/// Where the day `v` (`2026-10-01`) starts and the next one does, in `now`'s zone.
fn day<Tz: TimeZone>(v: &str, now: &DateTime<Tz>) -> Option<(i64, i64)> {
    let date = NaiveDate::parse_from_str(v.trim(), "%Y-%m-%d").ok()?;
    let midnight = |date: NaiveDate| {
        let start = date.and_hms_opt(0, 0, 0)?;
        // A zone whose clocks skip midnight starts that day an hour later.
        let local = now.timezone().from_local_datetime(&start).earliest();
        let local = local.or_else(|| {
            let later = start + Duration::hours(1);
            now.timezone().from_local_datetime(&later).earliest()
        })?;
        Some(nanos(&local))
    };
    Some((midnight(date)?, midnight(date.succ_opt()?)?))
}

fn nanos<Tz: TimeZone>(at: &DateTime<Tz>) -> i64 {
    at.timestamp_nanos_opt().unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    /// 2026-11-03 12:00 at UTC+1.
    fn now() -> DateTime<FixedOffset> {
        let zone = FixedOffset::east_opt(3600).unwrap();
        zone.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap()
    }

    fn read(q: &str) -> Query {
        parse(q, &now()).unwrap()
    }

    fn words(q: &Query) -> Vec<(&str, bool)> {
        q.text.iter().map(|t| (t.text.as_str(), t.prefix)).collect()
    }

    fn only(q: &str) -> (bool, On) {
        let q = read(q);
        assert!(q.text.is_empty(), "{q:?}");
        assert_eq!(q.filters.len(), 1, "{q:?}");
        let f = q.filters.into_iter().next().unwrap();
        (f.not, f.on)
    }

    fn error(q: &str) -> String {
        match parse(q, &now()) {
            Err(Error::Invalid(why)) => why,
            other => panic!("{q}: {other:?}"),
        }
    }

    /// Local midnight of a day in November 2026, in nanoseconds.
    fn midnight(day: u32) -> i64 {
        let zone = FixedOffset::east_opt(3600).unwrap();
        let at = zone.with_ymd_and_hms(2026, 11, day, 0, 0, 0).unwrap();
        at.timestamp_nanos_opt().unwrap()
    }

    #[test]
    fn free_words_are_any_order_and_the_last_one_is_a_prefix() {
        assert_eq!(words(&read("foo bar")), [("foo", false), ("bar", true)]);
        assert_eq!(words(&read("  foo  ")), [("foo", true)]);
        // A closed quote is a whole phrase; one still open runs to the end and is still typed.
        assert_eq!(words(&read("\"a  b\" c")), [("a b", false), ("c", true)]);
        assert_eq!(words(&read("x \"a b\"")), [("x", false), ("a b", false)]);
        assert_eq!(words(&read("x \"a b")), [("x", false), ("a b", true)]);
        // A qualifier after the words means the last word is done.
        let q = read("foo kind:pdf");
        assert_eq!(words(&q), [("foo", false)]);
        assert!(read("").text.is_empty() && read("").filters.is_empty());
    }

    #[test]
    fn tags_are_hashes_or_tag_qualifiers() {
        let tag = |q: &str| only(q);
        assert_eq!(tag("#a"), (false, On::Tag(vec!["a".into()])));
        assert_eq!(
            tag("tag:#a,b"),
            (false, On::Tag(vec!["a".into(), "b".into()]))
        );
        assert_eq!(tag("TAG:x/y"), (false, On::Tag(vec!["x/y".into()])));
        assert_eq!(tag("-#a"), (true, On::Tag(vec!["a".into()])));
        let q = read("#a foo");
        assert_eq!(words(&q), [("foo", true)]);
        assert_eq!(q.filters[0].on, On::Tag(vec!["a".into()]));
        // Not a tag a note could write, so a word to look for.
        assert_eq!(words(&read("#123")), [("#123", true)]);
        assert_eq!(read("#"), Query::default());
        assert!(error("tag:c++").starts_with("tag:c++ is not a tag"));
    }

    #[test]
    fn path_kind_and_links_read_their_values() {
        assert_eq!(
            only("path:\"My Folder\""),
            (false, On::Path("My Folder".into()))
        );
        assert_eq!(only("kind:pd"), (false, On::Kind(vec![Doc::Pdf])));
        assert_eq!(
            only("kind:note,NOTEBOOK"),
            (false, On::Kind(vec![Doc::Note, Doc::Notebook]))
        );
        assert!(error("kind:pfd").starts_with("kind:pfd is not a kind. Use note, pdf"));
        // Note and notebook both start so.
        assert!(error("kind:no").contains("is not a kind"));
        assert_eq!(only("links:[[Note]]"), (false, On::Links("Note".into())));
        assert_eq!(
            only("links:[[My Note]]"),
            (false, On::Links("My Note".into()))
        );
        assert_eq!(
            only("links:\"My Note#Part|see\""),
            (false, On::Links("My Note".into()))
        );
        assert_eq!(
            only("linked-from:Note"),
            (false, On::LinkedFrom("Note".into()))
        );
        assert_eq!(only("has:highlights"), (false, On::Highlights));
        assert_eq!(only("-has:high"), (true, On::Highlights));
        assert!(error("has:pages").contains("has:highlights"));
    }

    #[test]
    fn modified_takes_ages_days_and_ranges() {
        let span = |q: &str| match only(q) {
            (false, On::Modified { lo, hi }) => (lo, hi),
            other => panic!("{q}: {other:?}"),
        };
        let week = now().timestamp_nanos_opt().unwrap() - 7 * 86_400 * 1_000_000_000;
        assert_eq!(span("modified:>7d"), (week, i64::MAX));
        assert_eq!(span("modified:7d"), (week, i64::MAX));
        assert_eq!(span("modified:1w"), (week, i64::MAX));
        assert_eq!(span("modified:<7d"), (i64::MIN, week));
        assert_eq!(span("modified:2026-11-02"), (midnight(2), midnight(3)));
        assert_eq!(span("modified:<2026-11-02"), (i64::MIN, midnight(2)));
        assert_eq!(span("modified:<=2026-11-02"), (i64::MIN, midnight(3)));
        assert_eq!(span("modified:>2026-11-02"), (midnight(3), i64::MAX));
        assert_eq!(span("modified:>=2026-11-02"), (midnight(2), i64::MAX));
        assert_eq!(
            span("modified:2026-11-01..2026-11-04"),
            (midnight(1), midnight(5))
        );
        assert!(error("modified:>7x").starts_with("modified:>7x is not a date"));
        assert!(error("modified:2026-13-01").contains("is not a date"));
    }

    #[test]
    fn negation_and_what_stays_text() {
        assert_eq!(only("-word"), (true, On::Term("word".into())));
        assert_eq!(only("-\"a b\""), (true, On::Term("a b".into())));
        // A qualifier still being typed narrows nothing yet.
        assert_eq!(read("tag:"), Query::default());
        assert_eq!(read("kind:pdf,"), read("kind:pdf"));
        for q in [
            "\"tag:x\"",
            "foo:bar",
            "http://x",
            "a-b",
            "std::vec",
            "TODO:",
        ] {
            let read = read(q);
            assert!(read.filters.is_empty(), "{q}: {read:?}");
            assert_eq!(read.text.len(), 1, "{q}: {read:?}");
        }
        assert_eq!(words(&read("\"tag:x\"")), [("tag:x", false)]);
        // Punctuation alone is no word, wherever it stands, so the word before it is still typed.
        assert_eq!(words(&read("a - b")), [("a", false), ("b", true)]);
        assert_eq!(words(&read("a -")), [("a", true)]);
        assert_eq!(read("-# \"\""), Query::default());
    }

    #[test]
    fn free_text_is_the_words_whatever_the_qualifiers_say() {
        assert_eq!(free_text("kind:pfd foo -bar \"a b\" #t"), ["foo", "a b"]);
    }

    #[test]
    fn a_file_is_one_kind_by_its_name() {
        let kind = |rel: &str| Doc::named(rel).map(Doc::name);
        assert_eq!(kind("a/n.md"), Some("note"));
        assert_eq!(kind("n.org"), Some("note"));
        assert_eq!(kind("p.PDF"), Some("pdf"));
        assert_eq!(kind("f.drawio"), Some("diagram"));
        assert_eq!(kind("a.ipynb"), Some("notebook"));
        assert_eq!(kind("m.py"), Some("code"));
        assert_eq!(kind("t.csv"), Some("text"));
        assert_eq!(kind("c.toml"), Some("text"));
        assert_eq!(kind("plot.xml"), Some("text"));
        assert_eq!(kind("README"), Some("text"));
        assert_eq!(kind("i.png"), Some("image"));
        assert_eq!(kind("a.zip"), None);
    }
}
