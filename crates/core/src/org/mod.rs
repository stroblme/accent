//! Org mode as the index and the notes layer read it, line by line: headings (a level, and a
//! title without its TODO keyword, priority, `COMMENT` and tags), heading tags, `#+TITLE`,
//! `#+FILETAGS` and links, into the [`Analysis`] a note has, so whatever reads a note's links,
//! tags and outline reads an org file's too. A block of code or examples, a drawer, a comment
//! line and a fixed-width line hold none of them, nor does `=verbatim=` or `~code~` hold a link.
//!
//! A subset of <https://orgmode.org/worg/org-syntax.html>, not a parser of all of it. Ranges are
//! byte offsets of the text as written; the only spans are the blocks and drawers a fold hides.

mod links;

pub use links::{path_link_keys, rewrite_moved};

use crate::markdown::{Analysis, Heading, Span, Style, Tag, heading_for};
use links::OrgLink;
use std::ops::Range;

/// What one pass over a file finds: the analysis, the links as org writes them, which a move's
/// rewrite reads, and the `<<targets>>` an internal link may name.
struct Scan {
    analysis: Analysis,
    links: Vec<OrgLink>,
    targets: Vec<(Range<usize>, String)>,
}

/// Analyse an org file. Two passes, one for the TODO keywords a heading may open with, which
/// any line of the file may set, and one for the rest: fast enough for every keystroke.
pub fn analyze(text: &str) -> Analysis {
    scan(text).analysis
}

/// Where an anchor lands: on the `<<target>>` it names, else on the heading, by its title as
/// `[[*Title]]` and `[[file:x.org::*Title]]` write it or by its slug, as a note's
/// `[t](x.org#title)` does.
pub fn anchor_range(text: &str, anchor: &str) -> Option<Range<usize>> {
    let scan = scan(text);
    let anchor = anchor.trim();
    scan.targets
        .iter()
        .find(|(_, name)| name.eq_ignore_ascii_case(anchor))
        .map(|(at, _)| at.clone())
        .or_else(|| heading_for(&scan.analysis.headings, anchor).map(|h| h.range.clone()))
}

fn scan(text: &str) -> Scan {
    let lines = lines(text);
    let keywords = keywords(text, &lines);
    let mut s = Scan {
        analysis: Analysis::default(),
        links: Vec::new(),
        targets: Vec::new(),
    };
    let mut title: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].clone();
        if let Some((last, style)) = closed(text, &lines, i) {
            s.analysis.spans.push(Span {
                range: line.start..lines[last].end,
                style,
            });
            i = last + 1;
            continue;
        }
        let trimmed = text[line.clone()].trim_start();
        if let Some((level, at, tags)) = heading(text, line.clone(), &keywords) {
            s.analysis.headings.push(Heading {
                range: line,
                level,
                text: text[at.clone()].to_string(),
            });
            s.analysis.tags.extend(tags);
            inline(text, at, &mut s);
        } else if let Some((key, value)) = keyword(text, line.clone()) {
            if key.eq_ignore_ascii_case("title") && !value.is_empty() {
                title.push(&text[value]);
            } else if key.eq_ignore_ascii_case("filetags") {
                s.analysis
                    .tags
                    .extend(tags_in(text, value, |c| c == ':' || c.is_whitespace()));
            }
        // A comment (`# …`) and a fixed-width line (`: …`).
        } else if !["#", ":"].iter().any(|m| {
            trimmed
                .strip_prefix(m)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
        }) {
            inline(text, line, &mut s);
        }
        i += 1;
    }
    s.analysis.links = s.links.iter().filter_map(|l| l.to_link(text)).collect();
    // Org joins the lines of a title written over several.
    s.analysis.title = (!title.is_empty()).then(|| title.join(" "));
    s
}

/// Every line's range, without its line break.
fn lines(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        out.push(start..start + line.trim_end_matches(['\n', '\r']).len());
        start += line.len();
    }
    out
}

/// The TODO keywords a heading may open with: every word of the file's `#+TODO`, `#+SEQ_TODO`
/// and `#+TYP_TODO` lines, but the `|` between the open and the done ones, a fast-access key
/// such as `(t)` dropped; org's own TODO and DONE when the file sets none.
fn keywords<'a>(text: &'a str, lines: &[Range<usize>]) -> Vec<&'a str> {
    let mut out = Vec::new();
    for line in lines {
        let Some((key, value)) = keyword(text, line.clone()) else {
            continue;
        };
        if ["todo", "seq_todo", "typ_todo"]
            .iter()
            .any(|k| key.eq_ignore_ascii_case(k))
        {
            out.extend(
                text[value]
                    .split_whitespace()
                    .filter_map(|w| w.split('(').next())
                    .filter(|w| !w.is_empty() && *w != "|"),
            );
        }
    }
    if out.is_empty() {
        out = vec!["TODO", "DONE"];
    }
    out
}

/// `#+KEY: value` as its key and where its value is, trimmed.
fn keyword(text: &str, line: Range<usize>) -> Option<(&str, Range<usize>)> {
    let s = &text[line.clone()];
    let lead = s.len() - s.trim_start().len();
    let rest = s[lead..].strip_prefix("#+")?;
    let (key, value) = rest.split_once(':')?;
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    let start = line.start + lead + 2 + key.len() + 1;
    let start = start + value.len() - value.trim_start().len();
    Some((key, start..start + value.trim().len()))
}

/// The last line of the block or drawer opening on `lines[i]`, and the span it gets: a block of
/// code, an example, an export or a comment (`#+begin_src` … `#+end_src`), whose lines are
/// verbatim, or a drawer (`:PROPERTIES:` … `:END:`). `None` where none opens, and for one that
/// does not close before the next heading, which org reads as plain lines.
fn closed(text: &str, lines: &[Range<usize>], i: usize) -> Option<(usize, Style)> {
    let at = |j: usize| text[lines[j].clone()].trim();
    let first = at(i);
    let (end, style) = match first.get(..8) {
        Some(begin) if begin.eq_ignore_ascii_case("#+begin_") => {
            let name = first[8..].split_whitespace().next()?;
            ["src", "example", "export", "comment"]
                .iter()
                .any(|v| name.eq_ignore_ascii_case(v))
                .then(|| (format!("#+end_{name}"), Style::CodeBlock))?
        }
        _ => {
            let name = first.strip_prefix(':')?.strip_suffix(':')?;
            let named = !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'));
            (named && !name.eq_ignore_ascii_case("end"))
                .then(|| (":END:".to_string(), Style::Frontmatter))?
        }
    };
    (i + 1..lines.len())
        .take_while(|&j| !is_heading(&text[lines[j].clone()]))
        .find(|&j| at(j).eq_ignore_ascii_case(&end))
        .map(|j| (j, style))
}

/// Whether a line is a heading: stars at its start, then a space.
fn is_heading(line: &str) -> bool {
    let stars = line.bytes().take_while(|&c| c == b'*').count();
    stars > 0 && matches!(line.as_bytes().get(stars), Some(b' ' | b'\t'))
}

/// `* TODO [#A] COMMENT Title :a:b:` read into its level, where its title is, and its tags.
fn heading(
    text: &str,
    line: Range<usize>,
    keywords: &[&str],
) -> Option<(u8, Range<usize>, Vec<Tag>)> {
    let s = &text[line.clone()];
    if !is_heading(s) {
        return None;
    }
    let stars = s.bytes().take_while(|&c| c == b'*').count();
    let mut start = line.start + stars;
    let mut end = line.start + s.trim_end().len();
    // The tags close the line: `:a:b:` after a space.
    let head = &text[start..end];
    let run = head.len()
        - head
            .trim_end_matches(|c: char| c == ':' || is_tag_char(c))
            .len();
    let block = &head[head.len() - run..];
    let mut tags = Vec::new();
    if run > 1 && block.starts_with(':') && block.ends_with(':') {
        tags = tags_in(text, end - run..end, |c| c == ':');
        end -= run;
    }
    // Then what opens the title, each a word of its own: a keyword, a priority, `COMMENT`.
    let word = |start: usize| {
        let rest = &text[start..end];
        let from = start + rest.len() - rest.trim_start().len();
        let len = text[from..end]
            .find(char::is_whitespace)
            .unwrap_or(end - from);
        from..from + len
    };
    start = word(start).start;
    if keywords.contains(&&text[word(start)]) {
        start = word(word(start).end).start;
    }
    let w = &text[word(start)];
    if w.len() > 3
        && w.starts_with("[#")
        && w.ends_with(']')
        && w[2..w.len() - 1].bytes().all(|c| c.is_ascii_alphanumeric())
    {
        start = word(word(start).end).start;
    }
    if &text[word(start)] == "COMMENT" {
        start = word(word(start).end).start;
    }
    let title = start..start + text[start..end].trim_end().len();
    Some((stars.min(u8::MAX as usize) as u8, title, tags))
}

/// A character a tag is written with: a letter, a digit, `_`, `@`, `#` or `%`.
fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '%')
}

/// The tags written at `at`, each the run between two separators. `sep` takes `:`, which also
/// ends the last one.
fn tags_in(text: &str, at: Range<usize>, sep: impl Fn(char) -> bool) -> Vec<Tag> {
    let mut out = Vec::new();
    let mut from = None;
    for (i, c) in text[at.clone()].char_indices().chain([(at.len(), ':')]) {
        match (sep(c), from) {
            (true, Some(f)) => {
                let range = at.start + f..at.start + i;
                out.push(Tag {
                    name: text[range.clone()].to_string(),
                    range,
                });
                from = None;
            }
            (false, None) => from = Some(i),
            _ => {}
        }
    }
    out
}

/// The links and `<<targets>>` of the prose at `at`, read left to right as org reads its
/// objects: a `=verbatim=` or `~code~` run hides the links in it, as a link hides its markers.
fn inline(text: &str, at: Range<usize>, s: &mut Scan) {
    let b = text.as_bytes();
    let push = |link: OrgLink, s: &mut Scan| {
        let end = link.range.end;
        s.links.push(link);
        end
    };
    let mut i = at.start;
    while i < at.end {
        let after = match b[i] {
            b'[' => links::bracket(text, i, at.end).map(|l| push(l, s)),
            b'<' if b.get(i + 1) == Some(&b'<') => target(text, i, at.end).map(|(range, name)| {
                s.targets.push((range.clone(), name));
                range.end
            }),
            b'<' => links::angle(text, i, at.end).map(|l| push(l, s)),
            b'=' | b'~' => verbatim(text, at.start, i, at.end),
            c if c.is_ascii_alphabetic()
                && (i == at.start || !b[i - 1].is_ascii_alphanumeric()) =>
            {
                links::plain(text, i, at.end).map(|l| push(l, s))
            }
            _ => None,
        };
        i = after.unwrap_or(i + 1);
    }
}

/// The `<<target>>` opening at `i`, and its name; never a radio target's `<<<…>>>`.
fn target(text: &str, i: usize, end: usize) -> Option<(Range<usize>, String)> {
    let rest = &text[i + 2..end];
    let name = &rest[..rest.find(">>")?];
    (!name.is_empty() && name.trim() == name && !name.contains(['<', '>']))
        .then(|| (i..i + name.len() + 4, name.to_string()))
}

/// Where the `=verbatim=` or `~code~` run opening at `i` ends, by org's rules for its markers:
/// the opening one at the start of the line, after a space or after one of `-({'"`, and not
/// before a space; the closing one the same character, not after a space, and before a space,
/// the end of the line or one of `-.,;:!?')}["\`.
fn verbatim(text: &str, line: usize, i: usize, end: usize) -> Option<usize> {
    let b = text.as_bytes();
    let pre = i == line || b[i - 1].is_ascii_whitespace() || b"-({'\"".contains(&b[i - 1]);
    if !pre || i + 2 >= end || b[i + 1].is_ascii_whitespace() {
        return None;
    }
    (i + 2..end)
        .find(|&j| {
            b[j] == b[i]
                && !b[j - 1].is_ascii_whitespace()
                && (j + 1 == end
                    || b[j + 1].is_ascii_whitespace()
                    || b"-.,;:!?')}[\"\\".contains(&b[j + 1]))
        })
        .map(|j| j + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::LinkKind;

    fn headings(text: &str) -> Vec<(u8, String)> {
        analyze(text)
            .headings
            .into_iter()
            .map(|h| (h.level, h.text))
            .collect()
    }

    fn targets(text: &str) -> Vec<(LinkKind, String, Option<String>, Option<String>)> {
        analyze(text)
            .links
            .into_iter()
            .map(|l| (l.kind, l.target, l.anchor, l.alias))
            .collect()
    }

    #[test]
    fn a_heading_is_its_title_without_keyword_priority_comment_or_tags() {
        let text = "* TODO [#A] COMMENT Write the paper :work:urgent:\n** Notes\n*** DONE\n*not a heading*\n";
        let a = analyze(text);
        assert_eq!(
            headings(text),
            [
                (1, "Write the paper".to_string()),
                (2, "Notes".to_string()),
                (3, String::new())
            ]
        );
        assert_eq!(
            &text[a.headings[0].range.clone()],
            &text[..text.find('\n').unwrap()]
        );
        let tags: Vec<&str> = a.tags.iter().map(|t| &text[t.range.clone()]).collect();
        assert_eq!(tags, ["work", "urgent"]);
        assert_eq!(a.tags[0].name, "work");
        // A word that only looks like one stays in the title.
        assert_eq!(
            headings("* TODOS today 10:30:\n")[0].1,
            "TODOS today 10:30:"
        );
    }

    #[test]
    fn todo_lines_set_the_keywords_a_heading_opens_with() {
        let text =
            "#+TODO: NEXT(n) | DONE CANCELLED(c@)\n* NEXT Call\n* CANCELLED Trip\n* TODO Stays\n";
        assert_eq!(
            headings(text)
                .into_iter()
                .map(|(_, t)| t)
                .collect::<Vec<_>>(),
            ["Call", "Trip", "TODO Stays"]
        );
    }

    #[test]
    fn the_title_and_file_tags_come_from_their_keywords() {
        let a = analyze("#+title: Field notes\n#+FILETAGS: :lab:2024:\n\n* Field notes\n");
        assert_eq!(a.title.as_deref(), Some("Field notes"));
        let tags: Vec<&str> = a.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(tags, ["lab", "2024"]);
        assert_eq!(analyze("* Only a heading\n").title, None);
    }

    #[test]
    fn every_link_form_is_read_as_the_index_stores_it() {
        use LinkKind::*;
        let text = "See [[file:sub/a.org::*Intro][the intro]], [[./b.md]], [[../c.pdf]], \
             [[*Plan]], [[Target]], [[https://orgmode.org][Org]], <https://example.com/x>, \
             https://plain.org/p. and [[id:1234]] [[elisp:(foo)]] [[/etc/hosts]] [[~/x.org]]\n";
        let one = |kind, target: &str, anchor: Option<&str>, alias: Option<&str>| {
            (
                kind,
                target.to_string(),
                anchor.map(str::to_string),
                alias.map(str::to_string),
            )
        };
        assert_eq!(
            targets(text),
            [
                one(Markdown, "sub/a.org", Some("Intro"), Some("the intro")),
                one(Markdown, "./b.md", None, None),
                one(Markdown, "../c.pdf", None, None),
                one(Wiki, "", Some("Plan"), None),
                one(Wiki, "", Some("Target"), None),
                one(External, "https://orgmode.org", None, Some("Org")),
                one(External, "https://example.com/x", None, None),
                one(External, "https://plain.org/p", None, None),
            ]
        );
        let a = analyze(text);
        assert_eq!(
            &text[a.links[0].range.clone()],
            "[[file:sub/a.org::*Intro][the intro]]"
        );
        assert_eq!(
            targets(r"[[file:a\[1\].org]]"),
            [one(Markdown, "a[1].org", None, None)]
        );
    }

    #[test]
    fn blocks_drawers_comments_and_code_hold_nothing() {
        let text = "* Real :tag:\n\
             #+begin_src python :results output\n\
             [[file:a.org]]\n\
             ,* Escaped heading :no:\n\
             #+end_src\n\
             #+BEGIN_EXAMPLE\n[[file:b.org]]\n#+END_EXAMPLE\n\
             :PROPERTIES:\n:LINK: [[file:c.org]]\n:END:\n\
             # [[file:d.org]]\n\
             : [[file:e.org]]\n\
             ~[[file:f.org]]~ and =[[file:g.org]]= but [[file:h.org]]\n";
        let a = analyze(text);
        assert_eq!(headings(text), [(1, "Real".to_string())]);
        assert_eq!(a.tags.len(), 1);
        assert_eq!(
            targets(text).into_iter().map(|t| t.1).collect::<Vec<_>>(),
            ["h.org"]
        );
        let spans: Vec<(&str, Style)> = a
            .spans
            .iter()
            .map(|s| (&text[s.range.clone()], s.style))
            .collect();
        assert_eq!(spans.len(), 3);
        assert!(spans[0].0.starts_with("#+begin_src") && spans[0].0.ends_with("#+end_src"));
        assert_eq!(
            spans[2],
            (
                ":PROPERTIES:\n:LINK: [[file:c.org]]\n:END:",
                Style::Frontmatter
            )
        );
        // One that never closes is plain lines, and a heading ends the search for its end.
        let open = "#+begin_src\n[[file:a.org]]\n* Next\n#+end_src\n";
        assert_eq!(targets(open).len(), 1);
        assert!(analyze(open).spans.is_empty());
    }

    #[test]
    fn every_range_survives_a_multibyte_file() {
        let samples = ["é", "中", "🎉", "aé中🎉"];
        let templates = [
            "* X :X:X:\n",
            "* TODO [#A] X\n",
            "#+TITLE: X\n#+FILETAGS: :X:\n",
            "[[file:X.org::*X][X]] [[X]] <https://X> https://X.org/X\n",
            "X =X= ~X~ <<X>> X\n",
            "#+begin_src X\nX\n#+end_src\n:X:\nX\n:END:\n",
        ];
        for t in templates {
            for s in samples {
                let doc = t.replace('X', s);
                let a = analyze(&doc);
                let ranges = a
                    .spans
                    .iter()
                    .map(|s| &s.range)
                    .chain(a.links.iter().map(|l| &l.range))
                    .chain(a.tags.iter().map(|t| &t.range))
                    .chain(a.headings.iter().map(|h| &h.range));
                for r in ranges {
                    assert!(
                        r.start <= r.end
                            && r.end <= doc.len()
                            && doc.is_char_boundary(r.start)
                            && doc.is_char_boundary(r.end),
                        "bad range {r:?} in {doc:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_anchor_lands_on_a_target_before_a_heading() {
        let text = "* Plan\nSee <<plan>> here.\n* Other\n";
        assert_eq!(
            anchor_range(text, "plan").map(|r| &text[r]),
            Some("<<plan>>")
        );
        assert_eq!(
            anchor_range(text, "Other").map(|r| &text[r]),
            Some("* Other")
        );
        assert_eq!(anchor_range(text, "missing"), None);
    }
}
