//! Link targets: what a `[[wikilink]]` or `[text](target)` points at, the keys a file answers
//! to, and rewriting the targets that name a renamed note.

use super::{LinkKind, Pending, analyze};
use pulldown_cmark::LinkType;
use std::ops::Range;

pub(super) fn pending(
    range: Range<usize>,
    dest: &str,
    link_type: LinkType,
    image: bool,
) -> Pending {
    match link_type {
        LinkType::WikiLink { has_pothole } => {
            let (target, anchor) = split_anchor(dest);
            Pending {
                range,
                kind: if image {
                    LinkKind::Embed
                } else {
                    LinkKind::Wiki
                },
                target: target.to_string(),
                anchor: anchor.map(str::to_string),
                aliased: has_pothole,
                text: String::new(),
            }
        }
        _ if is_external(dest) => Pending {
            range,
            kind: LinkKind::External,
            target: dest.to_string(),
            anchor: None,
            aliased: true,
            text: String::new(),
        },
        _ => {
            let (target, anchor) = split_anchor(dest);
            Pending {
                range,
                kind: LinkKind::Markdown,
                target: percent_decode(target),
                anchor: anchor.map(percent_decode),
                aliased: true,
                text: String::new(),
            }
        }
    }
}

pub(super) fn split_anchor(dest: &str) -> (&str, Option<&str>) {
    match dest.split_once('#') {
        Some((t, a)) => (t, Some(a)),
        None => (dest, None),
    }
}

/// Read a PDF anchor: `page=3` or `page=3&selection=4,0,4,11`, as Obsidian writes them.
///
/// The page comes back **zero-based**, the way [`crate::pdf::Selection`] counts, and the four
/// selection numbers in the order the link spells them. `None` for a heading or a block anchor,
/// which is what tells a link to a note apart from a link into a PDF.
///
/// Here rather than in `pdf.rs` because this is link syntax, not PDF geometry: the index parses
/// it without the `pdf` feature, and so does a build with no libpdfium at all.
pub fn pdf_anchor(anchor: &str) -> Option<(usize, Option<[usize; 4]>)> {
    let (mut page, mut selection) = (None, None);
    for part in anchor.split('&') {
        match part.split_once('=') {
            Some(("page", n)) => page = n.trim().parse::<usize>().ok()?.checked_sub(1),
            // Four numbers or none: a selection we cannot read is a link to the page, which is
            // still where the reader wanted to go.
            Some(("selection", list)) => {
                selection = list
                    .split(',')
                    .map(|n| n.trim().parse::<usize>().ok())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|nums| <[usize; 4]>::try_from(nums).ok());
            }
            _ => {}
        }
    }
    Some((page?, selection))
}

/// `scheme:` or `//host` — anything with an authority is not a vault path.
fn is_external(dest: &str) -> bool {
    if dest.starts_with("//") {
        return true;
    }
    match dest.find(':') {
        Some(i) if i > 0 => {
            let s = &dest[..i];
            s.starts_with(|c: char| c.is_ascii_alphabetic())
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
        }
        _ => false,
    }
}

pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(super) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

const IMAGE_EXT: [&str; 8] = ["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "avif"];

/// Whether a link target names an image, by extension: `![[x.png]]` embeds, `![[x.pdf]]` links.
pub fn is_image(target: &str) -> bool {
    target
        .rsplit_once('.')
        .is_some_and(|(_, e)| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
}

/// `"a/b/c.md"` -> `"a/b/c"`; a name without a real extension is returned unchanged.
pub fn strip_ext(s: &str) -> String {
    match s.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains('/') => stem.to_string(),
        _ => s.to_string(),
    }
}

/// Normalise a link target the way Obsidian compares them: trimmed, `./` dropped, `\` as `/`,
/// case-folded. Both sides of link resolution go through this, so it is the one place that
/// decides what "the same target" means.
pub fn link_key(target: &str) -> String {
    target
        .trim()
        .trim_start_matches("./")
        .replace('\\', "/")
        .to_lowercase()
}

/// Every key a file answers to: full path, path without extension, basename, basename without
/// extension. Duplicates are kept out so callers can insert blindly.
pub fn path_keys(rel: &str) -> Vec<String> {
    let base = rel.rsplit('/').next().unwrap_or(rel).to_string();
    let mut keys = Vec::with_capacity(4);
    for k in [
        rel.to_string(),
        strip_ext(rel),
        base.clone(),
        strip_ext(&base),
    ] {
        let k = link_key(&k);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

/// Rewrite every wikilink in `text` that could name `old_rel` so it points at `new_rel`, or
/// `None` when the note refers to it nowhere.
///
/// Every form of the name is in scope here, including the bare basename, which two notes in
/// different directories can share. Callers that know which of them really resolve to `old_rel`
/// (the index does) should use [`rewrite_targets`] instead.
pub fn rewrite_links(text: &str, old_rel: &str, new_rel: &str) -> Option<String> {
    rewrite_targets(text, &path_keys(old_rel), new_rel)
}

/// Rewrite every wikilink in `text` whose target is one of `targets` so it points at `new_rel`,
/// or `None` when the note holds none of them. Used when a note is renamed or moved.
///
/// `targets` are compared with [`link_key`], so they may be written any way a link may be.
/// Which spellings belong to the renamed note is the caller's decision: this is pure text.
///
/// ponytail: only `[[wiki]]` and `![[embeds]]` are rewritten. A markdown `[x](a.md)` link is
/// relative to the note holding it and percent-encoded, so it needs path arithmetic this does
/// not do; add it when a vault that writes markdown links shows up.
pub fn rewrite_targets(text: &str, targets: &[String], new_rel: &str) -> Option<String> {
    let keys: Vec<String> = targets.iter().map(|t| link_key(t)).collect();
    let mut out: Option<String> = None;

    // Wikilinks cannot nest, so `analyze` yields the matches in source order and applying them
    // back to front keeps the earlier offsets valid.
    for link in analyze(text).links.iter().rev() {
        let open = match link.kind {
            LinkKind::Wiki => 2,
            LinkKind::Embed => 3,
            _ => continue,
        };
        if !keys.contains(&link_key(&link.target)) {
            continue;
        }
        let at = link.range.start + open..link.range.start + open + link.target.len();
        // A parser surprise must never corrupt a note: only touch bytes that are the target.
        if text.get(at.clone()) != Some(link.target.as_str()) {
            continue;
        }
        out.get_or_insert_with(|| text.to_string())
            .replace_range(at, &as_written(&link.target, new_rel));
    }
    out
}

/// Spell `new_rel` the way `old_target` was spelled: a bare name stays bare, a path stays a
/// path, and an extension is only written back if the author wrote one.
fn as_written(old_target: &str, new_rel: &str) -> String {
    let full = if old_target.contains('/') {
        new_rel
    } else {
        new_rel.rsplit('/').next().unwrap_or(new_rel)
    };
    // Only the renamed file's own extension counts as one: `[[Rev 1.2 notes]]` is a name with a
    // dot in it, and inventing an extension the author never wrote is a corrupted link.
    match (ext(new_rel), ext(old_target)) {
        (Some(new), Some(old)) if old.eq_ignore_ascii_case(new) => full.to_string(),
        _ => strip_ext(full),
    }
}

/// The extension of the file `rel` names, if it has one.
fn ext(rel: &str) -> Option<&str> {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    base.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, ext)| ext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::testing::link;

    #[test]
    fn wikilink_variants() {
        let t = "[[Note]] [[Note#Heading]] [[Note#^block]] [[Note|alias]] [[Note#Heading|alias]]";
        let a = analyze(t);
        assert_eq!(a.links.len(), 5);
        let expect = [
            ("Note", None, None),
            ("Note", Some("Heading"), None),
            ("Note", Some("^block"), None),
            ("Note", None, Some("alias")),
            ("Note", Some("Heading"), Some("alias")),
        ];
        for (l, (target, anchor, alias)) in a.links.iter().zip(expect) {
            assert_eq!(l.kind, LinkKind::Wiki);
            assert_eq!(l.target, target);
            assert_eq!(l.anchor.as_deref(), anchor);
            assert_eq!(l.alias.as_deref(), alias);
        }
        assert_eq!(&t[a.links[0].range.clone()], "[[Note]]");
        assert_eq!(&t[a.links[4].range.clone()], "[[Note#Heading|alias]]");
    }

    #[test]
    fn pdf_anchor_reads_page_and_selection() {
        assert_eq!(
            pdf_anchor("page=3&selection=4,0,4,11"),
            Some((2, Some([4, 0, 4, 11])))
        );
        assert_eq!(pdf_anchor("page=3"), Some((2, None)));
        // A heading is not a PDF anchor, which is what keeps `[[Note#Heading]]` on the note path.
        assert_eq!(pdf_anchor("Heading"), None);
        // Page numbers are one-based in the link and zero-based here, so page 0 is not a page.
        assert_eq!(pdf_anchor("page=0"), None);
        // A selection that is not four numbers is no selection, but the page still counts.
        assert_eq!(pdf_anchor("page=2&selection=1,2,3"), Some((1, None)));
        assert_eq!(pdf_anchor("page=2&selection=1,2,3,4,5"), Some((1, None)));
    }

    #[test]
    fn embeds_and_pdf_selection() {
        let t = "![[image.png]] ![[paper.pdf#page=3&selection=4,0,4,11]]";
        let a = analyze(t);
        assert_eq!(a.links.len(), 2);
        assert_eq!(a.links[0].kind, LinkKind::Embed);
        assert_eq!(a.links[0].target, "image.png");
        assert_eq!(a.links[0].anchor, None);
        assert_eq!(a.links[1].target, "paper.pdf");
        // the PDF anchor is kept verbatim, commas and all
        assert_eq!(
            a.links[1].anchor.as_deref(),
            Some("page=3&selection=4,0,4,11")
        );
        assert_eq!(
            &t[a.links[1].range.clone()],
            "![[paper.pdf#page=3&selection=4,0,4,11]]"
        );
    }

    #[test]
    fn markdown_link_percent_decoded() {
        let l = link("[text](Other%20Note.md)", 0);
        assert_eq!(l.kind, LinkKind::Markdown);
        assert_eq!(l.target, "Other Note.md");
        assert_eq!(l.alias.as_deref(), Some("text"));
        let l = link("[s](dir/A%20B.md#Some%20Heading)", 0);
        assert_eq!(l.target, "dir/A B.md");
        assert_eq!(l.anchor.as_deref(), Some("Some Heading"));
    }

    #[test]
    fn external_links_classified() {
        let a = analyze("[x](https://e.com/a#frag) [m](mailto:a@b.c) [r](../rel.md)");
        assert_eq!(a.links[0].kind, LinkKind::External);
        // a URL fragment stays part of the URL
        assert_eq!(a.links[0].target, "https://e.com/a#frag");
        assert_eq!(a.links[0].anchor, None);
        assert_eq!(a.links[1].kind, LinkKind::External);
        assert_eq!(a.links[1].target, "mailto:a@b.c");
        assert_eq!(a.links[2].kind, LinkKind::Markdown);
    }

    #[test]
    fn wikilink_in_code_is_not_a_link() {
        assert!(analyze("`[[NotALink]]`").links.is_empty());
        assert!(analyze("```\n[[NotALink]]\n```\n").links.is_empty());
    }

    #[test]
    fn link_key_normalises_target_spelling() {
        assert_eq!(link_key("  ./Notes\\Deep Work.md "), "notes/deep work.md");
        assert_eq!(link_key("Deep Work"), "deep work");
    }

    #[test]
    fn path_keys_cover_path_and_basename_forms() {
        assert_eq!(
            path_keys("Notes-PHD/Deep Work.md"),
            [
                "notes-phd/deep work.md",
                "notes-phd/deep work",
                "deep work.md",
                "deep work"
            ]
        );
        // A root-level file yields only two distinct keys.
        assert_eq!(path_keys("Index.md"), ["index.md", "index"]);
    }

    #[test]
    fn rewrite_links_handles_every_wikilink_shape() {
        let src = concat!(
            "[[Old]] and [[Old|alias]] and [[Old#Heading]]\n\n",
            "![[Old]]\n\n",
            "[[Dir/Old]] and [[dir/old.md]]\n\n",
            "```\n[[Old]]\n```\n"
        );
        let want = concat!(
            "[[New]] and [[New|alias]] and [[New#Heading]]\n\n",
            "![[New]]\n\n",
            "[[Notes/New]] and [[Notes/New.md]]\n\n",
            "```\n[[Old]]\n```\n"
        );
        assert_eq!(
            rewrite_links(src, "Dir/Old.md", "Notes/New.md").unwrap(),
            want
        );
    }

    #[test]
    fn rewrite_links_returns_none_when_nothing_matches() {
        assert_eq!(rewrite_links("[[Other]]", "Old.md", "New.md"), None);
        // Markdown links are not rewritten.
        assert_eq!(rewrite_links("[x](Old.md)", "Old.md", "New.md"), None);
    }

    #[test]
    fn rewrite_targets_only_touches_the_targets_it_was_given() {
        // `[[Old]]` belongs to a different note; renaming `Dir/Old.md` must not hijack it.
        let src = "deep: [[Dir/Old]]\nshallow: [[Old]]\n";
        assert_eq!(
            rewrite_targets(
                src,
                &["Dir/Old.md".to_string(), "Dir/Old".to_string()],
                "Dir/Renamed.md"
            )
            .unwrap(),
            "deep: [[Dir/Renamed]]\nshallow: [[Old]]\n"
        );
    }

    /// A dot in a name is not an extension: `[[Rev 1.2 notes]]` must not gain a `.md`.
    #[test]
    fn rewrite_links_only_writes_back_a_real_extension() {
        assert_eq!(
            rewrite_links(
                "[[Rev 1.2 notes]]\n",
                "Rev 1.2 notes.md",
                "Rev 1.3 notes.md"
            )
            .unwrap(),
            "[[Rev 1.3 notes]]\n"
        );
    }

    #[test]
    fn rewrite_links_preserves_alias_and_anchor() {
        assert_eq!(
            rewrite_links("see [[Old#Deep Work|this one]].", "Old.md", "Dir/New.md").unwrap(),
            "see [[New#Deep Work|this one]]."
        );
    }
}
