//! `#tag`s in prose and the YAML frontmatter block's `title:`, `tags:` and `aliases:`.

use super::{Analysis, Span, Style, Tag, sp};
use std::ops::Range;

/// Characters a tag may follow (Obsidian: start of line, whitespace, or opening punctuation).
const TAG_PREFIX: &str = "([{<,;:'\"*_~|";

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// Scan a text run (never code, never a link target) for `#tag`s.
pub(super) fn scan_tags(text: &str, r: &Range<usize>, tags: &mut Vec<Tag>, spans: &mut Vec<Span>) {
    let b = text.as_bytes();
    let mut i = r.start;
    while i < r.end {
        if b[i] != b'#' {
            i += 1;
            continue;
        }
        let ok = match text[..i].chars().next_back() {
            None => true,
            Some(c) => c.is_whitespace() || TAG_PREFIX.contains(c),
        };
        if !ok {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < r.end {
            let c = text[j..].chars().next().unwrap();
            if is_tag_char(c) {
                j += c.len_utf8();
            } else {
                break;
            }
        }
        let name = &text[i + 1..j];
        // `#123` is a heading-ish number, not a tag.
        if !name.is_empty() && !name.bytes().all(|c| c.is_ascii_digit()) {
            tags.push(Tag {
                range: i..j,
                name: name.to_string(),
            });
            spans.push(sp(i..j, Style::Tag));
        }
        i = j.max(i + 1);
    }
}

// ponytail: hand-rolled scan for `title:`, `tags:` and `aliases:` only; swap in serde_yaml (or
// yaml-rust2) the day we need nested frontmatter, anchors, or multi-line scalars.

/// What one item of a frontmatter list becomes: `raw` is the item as written, `base` its byte
/// offset in the document.
type Push = fn(&str, usize, &mut Analysis);

pub(super) fn frontmatter(text: &str, r: &Range<usize>, a: &mut Analysis) -> Option<String> {
    let block = &text[r.clone()];
    let start = block.find('\n').map(|i| r.start + i + 1)?;
    let end = block
        .rfind('\n')
        .map(|i| r.start + i)
        .unwrap_or(r.end)
        .max(start);
    a.frontmatter = Some(text[start..end].to_string());

    let mut title = None;
    // The list whose `- item` lines follow, when the key above them opened one.
    let mut list: Option<Push> = None;
    let mut off = start;
    for chunk in text[start..end].split_inclusive('\n') {
        let line_start = off;
        off += chunk.len();
        let line = chunk.trim_end_matches(['\n', '\r']);
        // A list's items may sit flush with its key, as a YAML dumper writes them: no key starts
        // with `-`, so such a line is an item too.
        if line.starts_with([' ', '\t', '-']) {
            let t = line.trim_start();
            if let Some(push) = list
                && t.starts_with('-')
            {
                let base = line_start + (line.len() - t.len()) + 1;
                push(&t[1..], base, a);
            }
            continue;
        }
        list = None;
        // Obsidian reads the singular `alias:` too, and a single string as a list of one.
        let keyed = [
            ("tags:", push_fm_tag as Push),
            ("aliases:", push_alias),
            ("alias:", push_alias),
        ]
        .into_iter()
        .find_map(|(key, push)| Some((line.strip_prefix(key)?, key.len(), push)));
        if let Some((rest, key_len, push)) = keyed {
            let base = line_start + key_len;
            let t = rest.trim();
            if t.is_empty() {
                list = Some(push);
            } else if let Some(inner) = t.strip_prefix('[') {
                let inner = inner.strip_suffix(']').unwrap_or(inner);
                let mut p = base + (rest.len() - rest.trim_start().len()) + 1;
                for item in inner.split(',') {
                    push(item, p, a);
                    p += item.len() + 1;
                }
            } else {
                push(rest, base, a);
            }
        } else if let Some(rest) = line.strip_prefix("title:") {
            let t = unquote(rest.trim());
            if !t.is_empty() {
                title = Some(t.to_string());
            }
        }
    }
    title
}

fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// One alias, by the name it gives the note. Where it sits is not kept: an alias is a name to find
/// the note by, not a span to style.
fn push_alias(raw: &str, _base: usize, a: &mut Analysis) {
    let name = unquote(raw.trim());
    if !name.is_empty() {
        a.aliases.push(name.to_string());
    }
}

/// `raw` is one frontmatter tag token; `base` is its byte offset in the document.
fn push_fm_tag(raw: &str, base: usize, a: &mut Analysis) {
    let lead = raw.len() - raw.trim_start().len();
    let tok = raw.trim();
    if tok.is_empty() {
        return;
    }
    let quoted = tok != unquote(tok);
    let tok = unquote(tok);
    let start = base + lead + usize::from(quoted);
    let name = tok.strip_prefix('#').unwrap_or(tok);
    if name.is_empty() {
        return;
    }
    let range = start..start + tok.len();
    a.tags.push(Tag {
        range: range.clone(),
        name: name.to_string(),
    });
    a.spans.push(sp(range, Style::Tag));
}

#[cfg(test)]
mod tests {
    use crate::markdown::analyze;
    use crate::markdown::testing::names;

    #[test]
    fn tags_obsidian_rules() {
        assert_eq!(
            names("#tag #nested/tag #tag_with-dash (#paren)"),
            ["tag", "nested/tag", "tag_with-dash", "paren"]
        );
        assert!(names("#123").is_empty(), "purely numeric is not a tag");
        assert!(
            names("# Heading\n").is_empty(),
            "heading marker is not a tag"
        );
        assert!(names("`#incode`").is_empty(), "code span is not a tag");
        assert!(
            names("```\n#inblock\n```\n").is_empty(),
            "code block is not a tag"
        );
        assert!(
            names("https://x.com/#frag").is_empty(),
            "URL fragment is not a tag"
        );
        assert!(
            names("[#notatag](x.md)").is_empty(),
            "link text is scanned as a link"
        );
    }

    #[test]
    fn tag_ranges_point_at_the_tag() {
        let t = "lead #alpha/beta trail";
        let a = analyze(t);
        assert_eq!(a.tags.len(), 1);
        assert_eq!(&t[a.tags[0].range.clone()], "#alpha/beta");
        assert_eq!(a.tags[0].name, "alpha/beta");
    }

    #[test]
    fn frontmatter_inline_and_block_tags() {
        let t = "---\ntitle: From Meta\ntags: [alpha, beta/gamma]\n---\n\nbody\n";
        let a = analyze(t);
        assert_eq!(names(t), ["alpha", "beta/gamma"]);
        assert_eq!(&t[a.tags[0].range.clone()], "alpha");
        assert_eq!(&t[a.tags[1].range.clone()], "beta/gamma");
        assert_eq!(a.title.as_deref(), Some("From Meta"));
        assert_eq!(
            a.frontmatter.as_deref(),
            Some("title: From Meta\ntags: [alpha, beta/gamma]")
        );

        let t = "---\ntags:\n  - alpha\n  - \"#beta\"\nauthor: me\n---\nbody\n";
        let a = analyze(t);
        assert_eq!(names(t), ["alpha", "beta"]);
        assert_eq!(&t[a.tags[1].range.clone()], "#beta");
        assert!(a.title.is_none());

        assert_eq!(names("---\ntags: single\n---\nx\n"), ["single"]);
    }
}
