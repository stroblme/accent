//! Org's links — `[[link]]`, `[[link][description]]`, `<https://…>` and a plain `https://…` —
//! read into the [`Link`] the index stores, and rewritten where a move leaves a file link naming
//! the wrong place. A link of one line only.

use crate::markdown::{Link, LinkKind, Moved, is_url, path_key};
use crate::path::parent_dir;
use std::collections::HashMap;
use std::iter::repeat_n;
use std::ops::Range;

/// One link as org writes it.
pub(super) struct OrgLink {
    /// The whole link, brackets included.
    pub range: Range<usize>,
    /// What it points at as written, escapes and all: `file:a.org::*H`, `https://…`, `Name`.
    link: Range<usize>,
    /// The description of `[[link][description]]`.
    desc: Option<Range<usize>>,
}

/// The `[[link]]` or `[[link][description]]` opening at `i`, on a line ending at `end`. A bracket
/// in the link is escaped by a backslash; an unescaped `[` is no link.
pub(super) fn bracket(text: &str, i: usize, end: usize) -> Option<OrgLink> {
    let b = text.as_bytes();
    if b.get(i + 1) != Some(&b'[') {
        return None;
    }
    let start = i + 2;
    let (mut j, mut slashes) = (start, 0);
    while j < end {
        match b[j] {
            b'\\' => slashes += 1,
            b'[' | b']' if slashes % 2 == 1 => slashes = 0,
            b']' => break,
            b'[' => return None,
            _ => slashes = 0,
        }
        j += 1;
    }
    let link = start..j;
    let rest = &text[j..end];
    if link.is_empty() {
        return None;
    }
    if rest.starts_with("]]") {
        return Some(OrgLink {
            range: i..j + 2,
            link,
            desc: None,
        });
    }
    let from = j + 2;
    let len = rest.strip_prefix("][")?.find("]]")?;
    (len > 0).then(|| OrgLink {
        range: i..from + len + 2,
        link,
        desc: Some(from..from + len),
    })
}

/// `<https://…>`: an address the system opens, in angle brackets.
pub(super) fn angle(text: &str, i: usize, end: usize) -> Option<OrgLink> {
    let len = text[i + 1..end].find('>')?;
    let link = i + 1..i + 1 + len;
    is_url(&text[link.clone()]).then(|| OrgLink {
        range: i..link.end + 1,
        link,
        desc: None,
    })
}

/// A plain `scheme://…` address at `i`: up to the next space, bracket or quote, a mark closing a
/// sentence left to the sentence.
pub(super) fn plain(text: &str, i: usize, end: usize) -> Option<OrgLink> {
    let rest = &text[i..end];
    let scheme = rest
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || b"+.-".contains(c))
        .count();
    if !rest[scheme..].starts_with("://") {
        return None;
    }
    let len = rest
        .find(|c: char| c.is_whitespace() || "()<>[]{}\"'".contains(c))
        .unwrap_or(rest.len());
    let len = rest[..len]
        .trim_end_matches(['.', ',', ';', ':', '!', '?'])
        .len();
    let link = i..i + len;
    is_url(&text[link.clone()]).then(|| OrgLink {
        range: link.clone(),
        link,
        desc: None,
    })
}

impl OrgLink {
    /// The link as the index stores it: a web address as an external link, a file link as a
    /// relative path (a markdown link's kind), and a link to a heading or a `<<target>>` of this
    /// file as a wikilink with no target, as a note's `[[#Heading]]`. `None` for what the index
    /// does not follow: an `id:` link (deferred, user decision 2026-10-24), any other link type
    /// (`elisp:`, `doi:`, …), a `(coderef)`, and an absolute or home path, which no vault path is.
    pub(super) fn to_link(&self, text: &str) -> Option<Link> {
        let written = unescape(&text[self.link.clone()]);
        let anchor = |s: &str| unescape(s.strip_prefix('*').unwrap_or(s));
        let (kind, target, anchor) = if is_url(&written) {
            (LinkKind::External, written, None)
        } else if let Some((path, _, search)) = self.file(text) {
            (
                LinkKind::Markdown,
                unescape(&text[path]),
                search.map(anchor),
            )
        } else if typed(&written) || written.starts_with(['(', '/', '~']) {
            return None;
        } else {
            (LinkKind::Wiki, String::new(), Some(anchor(&written)))
        };
        Some(Link {
            range: self.range.clone(),
            kind,
            target,
            anchor,
            alias: self.desc.clone().map(|d| text[d].to_string()),
        })
    }

    /// A file link's path as written, after `file:` and before `::search`, whether `file:` is
    /// written, and the search. Org reads a path as one without `file:` when it starts with `./`
    /// or `../`; `None` for any other link, and for an absolute or a home path.
    fn file<'a>(&self, text: &'a str) -> Option<(Range<usize>, bool, Option<&'a str>)> {
        let written = &text[self.link.clone()];
        let (start, prefixed) = match written.strip_prefix("file:") {
            Some(_) => (self.link.start + "file:".len(), true),
            None if written.starts_with("./") || written.starts_with("../") => {
                (self.link.start, false)
            }
            None => return None,
        };
        let rest = &text[start..self.link.end];
        let (path, search) = match rest.split_once("::") {
            Some((path, search)) => (path, Some(search)),
            None => (rest, None),
        };
        (!path.is_empty() && !path.starts_with(['/', '~'])).then_some((
            start..start + path.len(),
            prefixed,
            search,
        ))
    }
}

/// Whether a link names its type, `id:` or `elisp:`, as a lowercase scheme: `[[Note: intro]]`
/// is a heading's name.
fn typed(link: &str) -> bool {
    link.split_once(':').is_some_and(|(scheme, rest)| {
        scheme.starts_with(|c: char| c.is_ascii_lowercase())
            && scheme
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"+.-".contains(&c))
            && !rest.starts_with(' ')
    })
}

/// A link as org means it (`org-link-unescape`): a backslash before a bracket escapes it, and a
/// run of them before one, or at the end, is written doubled.
fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut run = 0;
    for c in s.chars() {
        if c == '\\' {
            run += 1;
            continue;
        }
        out.extend(repeat_n(
            '\\',
            if matches!(c, '[' | ']') { run / 2 } else { run },
        ));
        out.push(c);
        run = 0;
    }
    out.extend(repeat_n('\\', run / 2));
    out
}

/// [`unescape`]'s inverse, for a path written back.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0;
    for c in s.chars() {
        if c == '\\' {
            run += 1;
            continue;
        }
        let bracket = matches!(c, '[' | ']');
        out.extend(repeat_n('\\', if bracket { 2 * run + 1 } else { run }));
        out.push(c);
        run = 0;
    }
    out.extend(repeat_n('\\', 2 * run));
    out
}

/// [`crate::markdown::rewrite_moved`] for an org file: a file link that would name another file
/// from where the file is after the move names the one it named again, as the relative path
/// from there, its `file:`, `::search` and description as they were. One written without
/// `file:` keeps `./` or `../` in front, without which org reads the path as a heading's name.
/// Org's other links name no vault path, so nothing else changes.
pub fn rewrite_moved(
    text: &str,
    src_old: &str,
    src_new: &str,
    targets: &HashMap<String, String>,
    moves: &HashMap<String, String>,
) -> Option<String> {
    let m = Moved {
        dir_old: parent_dir(src_old),
        dir_new: parent_dir(src_new),
        targets,
        moves,
    };
    let mut out = text.to_string();
    // Back to front, so the earlier offsets stay valid.
    for link in super::scan(text).links.iter().rev() {
        let Some((at, prefixed, _)) = link.file(text) else {
            continue;
        };
        let Some(to) = m.repoint(&unescape(&text[at.clone()])) else {
            continue;
        };
        let to = match prefixed || to.starts_with("../") {
            true => to,
            false => format!("./{to}"),
        };
        out.replace_range(at, &escape(&to));
    }
    (out != text).then_some(out)
}

/// The key of every file link's path in `text`, as [`rewrite_moved`] looks them up in its
/// `targets`, for a caller resolving them itself: Save As, whose text the index has not seen.
pub fn path_link_keys(text: &str, src: &str) -> Vec<String> {
    let dir = parent_dir(src);
    super::scan(text)
        .links
        .iter()
        .filter_map(|l| l.file(text))
        .filter_map(|(at, ..)| path_key(dir, &unescape(&text[at])))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn a_move_repoints_file_links_and_keeps_what_surrounds_their_paths() {
        let text = "[[file:a.md::*Intro][the intro]] [[./a.md]] [[*Plan]] [[https://a.md]]\n";
        let targets = map(&[("a.md", "a.md")]);
        // The target moved into a folder.
        let moved = rewrite_moved(
            text,
            "b.org",
            "b.org",
            &targets,
            &map(&[("a.md", "sub/a[1].md")]),
        );
        assert_eq!(
            moved.as_deref(),
            Some(
                "[[file:sub/a\\[1\\].md::*Intro][the intro]] [[./sub/a\\[1\\].md]] [[*Plan]] \
                 [[https://a.md]]\n"
            )
        );
        // The org file moved instead, and its links climb out to the file they named.
        let moved = rewrite_moved(text, "b.org", "deep/b.org", &targets, &HashMap::new());
        assert_eq!(
            moved.as_deref(),
            Some("[[file:../a.md::*Intro][the intro]] [[../a.md]] [[*Plan]] [[https://a.md]]\n")
        );
        assert_eq!(
            rewrite_moved(text, "b.org", "c.org", &targets, &HashMap::new()),
            None
        );
        assert_eq!(path_link_keys(text, "dir/b.org"), ["dir/a.md", "dir/a.md"]);
    }

    #[test]
    fn escaping_round_trips() {
        for s in [r"a[1]", r"c:\x\", r"\[", "plain"] {
            assert_eq!(unescape(&escape(s)), s);
        }
        assert_eq!(escape("a[1]"), r"a\[1\]");
    }
}
