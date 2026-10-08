//! The tools, each a façade call or two run on the blocking pool, and the arguments they take.

use std::collections::HashMap;
use std::ops::Range;

use accent_api::{Etag, FileKind, Read, SaveError, fs};
use accent_core::markdown;
use accent_core::path::linked_path;
use base64::Engine;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{MAX_OUT, Server, answer, etag_string, explore, fail, image_type, line_at, locked};

#[derive(Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// Words to find, in any order; the last one also matches as the start of a word, and a
    /// query found in no word is looked for inside words.
    query: String,
    /// Most rows to return, 20 when left out, 100 at most. A row is one occurrence.
    limit: Option<usize>,
    /// Also search the files git ignores, left out otherwise.
    include_ignored: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ReadArgs {
    /// The file, relative to the vault root.
    path: String,
    /// Only the section under this heading, its subsections included: the heading's text or its
    /// slug, `notes-1` naming the second of two `Notes`.
    heading: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PathArgs {
    /// The file, relative to the vault root.
    path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TagArgs {
    /// List the files holding this tag, without its `#`, instead of every tag.
    tag: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DirArgs {
    /// The folder, relative to the vault root; the root when left out.
    path: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct LimitArgs {
    /// Most rows to return, 20 when left out, 500 at most.
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct LinkArgs {
    /// A link target as a note writes it: `Note`, `Note#Heading`, `folder/Note.md`, or a whole
    /// `[[Note#Heading|alias]]`.
    target: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PatchArgs {
    /// The note, relative to the vault root.
    path: String,
    /// The section to change, as `read_note` takes it. Left out, the whole note, which `append`
    /// and `prepend` change and `replace` does not: `write_note` writes a whole note.
    heading: Option<String>,
    /// `replace` the section's text, its heading kept; or `append` it after the section's last
    /// line, or `prepend` it under the heading (under the frontmatter for the whole note).
    mode: Mode,
    /// The markdown to put there.
    content: String,
    /// The etag `read_note` gave, so that a note changed since is not patched. Left out, the
    /// note is patched as it is now.
    etag: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WriteArgs {
    /// The file, relative to the vault root; its folders are made as needed.
    path: String,
    /// The whole text.
    content: String,
    /// The etag `read_note` gave: required to overwrite a file that exists, which is then
    /// refused if it changed since. Left out, only a new file is written.
    etag: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TemplateArgs {
    /// The template: its name in the vault's templates folder, or its path.
    template: String,
    /// The note to make, relative to the vault root, `.md` added when it names no extension.
    /// Left out, the template's own `accent-target:` says where.
    path: Option<String>,
}

/// The tools that write, which `--read-only` leaves out.
pub(super) const WRITE_TOOLS: [&str; 3] = ["patch_note", "write_note", "create_note_from_template"];

#[tool_router(vis = "pub(super)")]
impl Server {
    /// Full-text search over the vault's indexed files. Each row is one occurrence: the file,
    /// its title, the 1-based line, and a snippet with the match between « and ».
    #[tool(annotations(read_only_hint = true))]
    async fn search_notes(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let partial = s.ready.wait();
            let limit = a.limit.unwrap_or(20).min(100);
            let hits = {
                let _turn = locked(&s.search);
                s.vault
                    .search(&a.query, limit, a.include_ignored.unwrap_or(false))
                    .map_err(fail)?
            };
            let rows: Vec<Value> = hits
                .into_iter()
                .filter(|h| s.shown(&h.rel_path))
                .map(|h| {
                    json!({"path": h.rel_path, "title": h.title, "line": h.line, "snippet": h.snippet})
                })
                .collect();
            Ok(answer(json!(rows), partial))
        })
        .await
    }

    /// Read a file: a note or any other text file whole, or the section under one heading. The
    /// first block is `{path, etag}`, the etag being what a write of the file is checked against;
    /// the second is the text. An image comes back as an image, another binary file as its size.
    #[tool(annotations(read_only_hint = true))]
    async fn read_note(
        &self,
        Parameters(a): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let rel = s.inside(&a.path)?;
            match s.vault.read_text(&rel).map_err(fail)? {
                Read::Text(t) => {
                    let text = match &a.heading {
                        Some(heading) => &t.text[section(&t.text, heading)?],
                        None => &t.text,
                    };
                    if text.len() as u64 > MAX_OUT {
                        return Err(format!(
                            "{rel} holds {} bytes, more than a read returns ({MAX_OUT}): read it a \
                             section at a time with `heading`",
                            text.len()
                        ));
                    }
                    let mut head = json!({"path": rel, "etag": etag_string(t.etag)});
                    if t.lossy {
                        // Written back, the replacement characters would replace the bytes.
                        head["lossy"] = json!(true);
                    }
                    Ok(CallToolResult::success(vec![
                        ContentBlock::text(head.to_string()),
                        ContentBlock::text(text),
                    ]))
                }
                Read::Binary { size } => match image_type(&rel) {
                    Some(mime) if size <= MAX_OUT => {
                        let path = s.vault.resolve(&rel).map_err(fail)?;
                        let bytes = std::fs::read(path).map_err(fail)?;
                        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                        Ok(CallToolResult::success(vec![ContentBlock::image(
                            data, mime,
                        )]))
                    }
                    _ => Ok(answer(
                        json!({"path": rel, "kind": "binary", "size": size}),
                        None,
                    )),
                },
                Read::TooLarge { size } => {
                    Err(format!("{rel} holds {size} bytes, too many to read"))
                }
            }
        })
        .await
    }

    /// The notes linking to a file: each link's note, its 1-based line, and that line.
    #[tool(annotations(read_only_hint = true))]
    async fn list_backlinks(
        &self,
        Parameters(a): Parameters<PathArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let rel = s.inside(&a.path)?;
            let partial = s.ready.wait();
            let links = s.vault.backlinks(&rel).map_err(fail)?;
            let rows: Vec<Value> = s
                .lines(
                    links
                        .into_iter()
                        .map(|b| (b.src_rel_path, b.byte_start as usize)),
                )
                .into_iter()
                .map(|(path, line, text)| json!({"path": path, "line": line, "text": text}))
                .collect();
            Ok(answer(json!(rows), partial))
        })
        .await
    }

    /// Every tag in the vault with the number of times it is used, or with `tag` the files
    /// holding that one.
    #[tool(annotations(read_only_hint = true))]
    async fn list_tags(
        &self,
        Parameters(a): Parameters<TagArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let partial = s.ready.wait();
            let rows: Value = match &a.tag {
                None => s
                    .vault
                    .tags()
                    .map_err(fail)?
                    .into_iter()
                    .map(|(tag, count)| json!({"tag": tag, "count": count}))
                    .collect(),
                Some(tag) => s
                    .vault
                    .files_with_tag(tag.trim_start_matches('#'))
                    .map_err(fail)?
                    .into_iter()
                    .filter(|f| s.shown(&f.rel_path))
                    .map(|f| json!(f.rel_path))
                    .collect(),
            };
            Ok(answer(rows, partial))
        })
        .await
    }

    /// The file a link leads to, as following it in accent opens it, and the 1-based line its
    /// `#heading` or `#^block` is on. `path` is null when nothing in the vault answers to it.
    #[tool(annotations(read_only_hint = true))]
    async fn resolve_link(
        &self,
        Parameters(a): Parameters<LinkArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let partial = s.ready.wait();
            let link = a.target.trim().trim_start_matches('!');
            let link = link.strip_prefix("[[").unwrap_or(link);
            let link = link.strip_suffix("]]").unwrap_or(link);
            let link = link.split('|').next().unwrap_or_default();
            let (target, anchor) = match link.split_once('#') {
                Some((target, anchor)) => (target, Some(anchor)),
                None => (link, None),
            };
            let found = s.vault.follow(target).map_err(fail)?;
            let Some(rel) = found.filter(|rel| s.shown(rel)) else {
                return Ok(answer(json!({"path": null}), partial));
            };
            let line = anchor.and_then(|anchor| {
                let text = s.text(&rel).ok()?.text;
                let at = markdown::anchor_range(&text, anchor)?;
                Some(line_at(&text, at.start).0)
            });
            Ok(answer(json!({"path": rel, "line": line}), partial))
        })
        .await
    }

    /// A question about the vault answered in one call: start here. `query` takes plain words,
    /// names, vault paths, `[[links]]` and `#tags` in any mix; the answer is markdown, the files
    /// it is about best first, each with its outline, the links going out of it and coming in,
    /// and the sections holding the words verbatim as `<line>\t<text>`, the rest named below
    /// them. A query that is one path or one link answers with that file's whole card.
    #[tool(annotations(read_only_hint = true))]
    async fn explore(
        &self,
        Parameters(a): Parameters<explore::ExploreArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let text = explore::explore(s, &a)?;
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        })
        .await
    }

    /// The entries of one folder, folders first: each one's path, kind (`dir`, `note`, `pdf`,
    /// `file`), a file's size in bytes, when it was last modified (UTC), a note's title, and
    /// whether the index holds it, which a dependency or build tree it never walks is listed
    /// without.
    #[tool(annotations(read_only_hint = true))]
    async fn list_dir(&self, Parameters(a): Parameters<DirArgs>) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let rel = match a.path.as_deref().map(|p| p.trim_matches('/')) {
                Some(p) if !p.is_empty() => s.inside(p)?,
                _ => String::new(),
            };
            let partial = s.ready.wait();
            let rows: Vec<Value> = s
                .vault
                .list_dir(&rel)
                .map_err(fail)?
                .into_iter()
                .filter(|f| s.shown(&f.rel_path))
                .map(|f| {
                    let indexed = f.id != 0;
                    let mut row = json!({"path": f.rel_path, "kind": kind_name(f.kind)});
                    if indexed && f.kind != FileKind::Dir {
                        row["size"] = json!(f.size);
                    }
                    if indexed {
                        row["modified"] = json!(modified(f.mtime_ns));
                    }
                    if let Some(title) = f.title.filter(|_| f.kind == FileKind::Markdown) {
                        row["title"] = json!(title);
                    }
                    row["indexed"] = json!(indexed);
                    row
                })
                .collect();
            Ok(answer(json!(rows), partial))
        })
        .await
    }

    /// The files modified last, newest first: notes, PDFs and other files alike, leaving out
    /// what git ignores. Each row is the path and when it was modified (UTC).
    #[tool(annotations(read_only_hint = true))]
    async fn recent_changes(
        &self,
        Parameters(a): Parameters<LimitArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let partial = s.ready.wait();
            let limit = a.limit.unwrap_or(20).min(500);
            let rows: Vec<Value> = s
                .vault
                .recent_files(limit)
                .map_err(fail)?
                .into_iter()
                .filter(|rel| s.shown(rel))
                .filter_map(|rel| {
                    let etag = s.vault.stat(&rel).ok()??;
                    Some(json!({"path": rel, "modified": modified(etag.mtime_ns)}))
                })
                .collect();
            Ok(answer(json!(rows), partial))
        })
        .await
    }

    /// The notes links point at that are not written yet, by the path a new note would take
    /// for them, each with up to five of the notes and lines linking to it.
    #[tool(annotations(read_only_hint = true))]
    async fn missing_notes(
        &self,
        Parameters(a): Parameters<LimitArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let partial = s.ready.wait();
            let missing = s.vault.missing_notes().map_err(fail)?;
            let mut sources: HashMap<String, Vec<(String, usize)>> = HashMap::new();
            for (src, target, at) in s.vault.unresolved_links().map_err(fail)? {
                let held = sources
                    .entry(markdown::link_key(&linked_path(&target)))
                    .or_default();
                if held.len() < 5 {
                    held.push((src, at as usize));
                }
            }
            let limit = a.limit.unwrap_or(20).min(500);
            let rows: Vec<Value> = missing
                .iter()
                .filter(|rel| s.shown(rel))
                .take(limit)
                .map(|rel| {
                    let from = sources.remove(&markdown::link_key(rel)).unwrap_or_default();
                    let from: Vec<Value> = s
                        .lines(from)
                        .into_iter()
                        .map(|(path, line, _)| json!({"path": path, "line": line}))
                        .collect();
                    json!({"path": rel, "linked_from": from})
                })
                .collect();
            let mut out = answer(json!(rows), partial);
            if missing.len() > rows.len() {
                out.content.push(ContentBlock::text(format!(
                    "{} of {} missing notes listed: ask with a higher limit for more.",
                    rows.len(),
                    missing.len()
                )));
            }
            Ok(out)
        })
        .await
    }

    /// The highlights made on a PDF, which accent keeps as links in the notes: each one's
    /// 1-based page, the text it quotes, and the note and line holding it.
    #[tool(annotations(read_only_hint = true))]
    async fn pdf_annotations(
        &self,
        Parameters(a): Parameters<PathArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let rel = s.inside(&a.path)?;
            let partial = s.ready.wait();
            let links = s.vault.pdf_links(&rel).map_err(fail)?;
            let quotes: Vec<(usize, Option<String>)> = links
                .iter()
                .map(|l| (l.page + 1, l.alias.clone()))
                .collect();
            let at = links
                .into_iter()
                .map(|l| (l.src_rel_path, l.byte_start as usize));
            let rows: Vec<Value> = s
                .lines(at)
                .into_iter()
                .zip(quotes)
                .map(|((note, line, _), (page, quote))| {
                    json!({"page": page, "quote": quote, "note": note, "line": line})
                })
                .collect();
            Ok(answer(json!(rows), partial))
        })
        .await
    }

    /// Change one section of a note: replace the text under a heading, the heading kept, or
    /// append or prepend to it, or to the whole note when no heading is given. Returns the
    /// note's new etag.
    #[tool]
    async fn patch_note(
        &self,
        Parameters(a): Parameters<PatchArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let rel = s.inside(&a.path)?;
            let t = s.text(&rel)?;
            if t.lossy {
                return Err(format!(
                    "{rel} is not UTF-8 throughout, and patching it would replace its bytes"
                ));
            }
            if let Some(etag) = &a.etag
                && parse_etag(etag)? != t.etag
            {
                return Err(changed(&rel, t.etag));
            }
            let at = match (&a.heading, a.mode) {
                (Some(heading), _) => section(&t.text, heading)?,
                (None, Mode::Replace) => {
                    return Err("replace takes a heading: write_note writes a whole note".into());
                }
                (None, _) => body_start(&t.text)..t.text.len(),
            };
            let text = patched(&t.text, at, a.mode, &a.content);
            let etag = s
                .vault
                .save(&rel, &fs::for_disk(&text, t.crlf, false), Some(t.etag))
                .map_err(|e| unsaved(&rel, e))?;
            s.settle();
            Ok(answer(
                json!({"path": rel, "etag": etag_string(etag)}),
                None,
            ))
        })
        .await
    }

    /// Write a whole file: a new one, its folders made as needed, or with the etag `read_note`
    /// gave one that exists. Returns its etag, and whether it is new.
    #[tool]
    async fn write_note(
        &self,
        Parameters(a): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let rel = s.inside(&a.path)?;
            let (expected, crlf) = match (s.vault.stat(&rel).map_err(fail)?, &a.etag) {
                (None, None) => {
                    // Made empty first, so a file made meanwhile is refused, not overwritten.
                    s.vault.create_note(&rel, None).map_err(fail)?;
                    (s.vault.stat(&rel).map_err(fail)?, false)
                }
                (Some(_), Some(etag)) => (Some(parse_etag(etag)?), s.text(&rel)?.crlf),
                (Some(_), None) => {
                    return Err(format!(
                        "{rel} exists: pass the etag read_note gave to overwrite it"
                    ));
                }
                (None, Some(_)) => return Err(format!("{rel} is gone since it was read")),
            };
            let etag = s
                .vault
                .save(&rel, &fs::for_disk(&a.content, crlf, false), expected)
                .map_err(|e| unsaved(&rel, e))?;
            s.settle();
            let created = a.etag.is_none();
            Ok(answer(
                json!({"path": rel, "etag": etag_string(etag), "created": created}),
                None,
            ))
        })
        .await
    }

    /// Make a note from one of the vault's templates, its placeholders filled in. Without
    /// `path` it goes where the template's `accent-target:` says, and a note already there, such
    /// as today's daily note, is left as it is. Returns its path, and whether it is new.
    #[tool]
    async fn create_note_from_template(
        &self,
        Parameters(a): Parameters<TemplateArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            s.template(&a.template)?;
            let (rel, created) = match &a.path {
                Some(path) => {
                    let rel = s.inside(&linked_path(path))?;
                    s.vault.create_note(&rel, Some(&a.template)).map_err(fail)?;
                    (rel, true)
                }
                None => {
                    let Some(target) = s.vault.template_target(&a.template).map_err(fail)? else {
                        let templates = s.vault.templates().map_err(fail)?;
                        return Err(format!(
                            "{} says nowhere its notes go: give a path. The templates are \
                             {templates:?}",
                            a.template
                        ));
                    };
                    let rel = s.inside(&target)?;
                    let created = !s.vault.exists(&rel);
                    s.vault.note_from_template(&a.template).map_err(fail)?;
                    (rel, created)
                }
            };
            s.settle();
            Ok(answer(json!({"path": rel, "created": created}), None))
        })
        .await
    }
}

/// A file's kind as `list_dir` names it.
fn kind_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Dir => "dir",
        FileKind::Markdown => "note",
        FileKind::Pdf => "pdf",
        FileKind::Other => "file",
        FileKind::Conflict => "conflict",
    }
}

/// A modification time as the wire carries it: RFC 3339, UTC, to the second.
fn modified(mtime_ns: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(mtime_ns)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Where a patch puts its text in the section: in place of it, after it, or before it.
#[derive(Deserialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Replace,
    Append,
    Prepend,
}

/// `text` with `content` put into the section at `at` as `mode` says: lines in, lines out, the
/// last one ended, and the blank lines that ended the section kept after it.
fn patched(text: &str, at: Range<usize>, mode: Mode, content: &str) -> String {
    let section = &text[at.clone()];
    let body = section.trim_end_matches('\n');
    let blank = (section.len() - body.len()).saturating_sub(usize::from(!body.is_empty()));
    let content = content.trim_end_matches('\n');
    let parts = match mode {
        Mode::Replace => [content, ""],
        Mode::Append => [body, content],
        Mode::Prepend => [content, body],
    };
    let new: Vec<&str> = parts.into_iter().filter(|p| !p.is_empty()).collect();
    let new = match new.is_empty() {
        true => String::new(),
        false => new.join("\n") + "\n",
    };
    // A heading on the note's last line has no newline for the section to start after.
    let lead = match at.start > 0 && !text[..at.start].ends_with('\n') {
        true => "\n",
        false => "",
    };
    let gap = "\n".repeat(blank);
    format!("{}{lead}{new}{gap}{}", &text[..at.start], &text[at.end..])
}

/// Where a note's own text starts: past its frontmatter, which a prepend leaves first.
fn body_start(text: &str) -> usize {
    markdown::analyze(text)
        .spans
        .iter()
        .find(|s| matches!(s.style, markdown::Style::Frontmatter))
        .map_or(0, |s| {
            s.range.end + usize::from(text[s.range.end..].starts_with('\n'))
        })
}

/// An etag as [`etag_string`] spells it.
fn parse_etag(etag: &str) -> Result<Etag, String> {
    let bad = || format!("{etag:?} is not an etag read_note gave");
    // From the right: the mtime of a file from before 1970 is negative.
    let mut parts = etag.rsplitn(3, '-');
    let mut next = || parts.next().ok_or_else(bad);
    let ino = next()?.parse().map_err(|_| bad())?;
    let size = next()?.parse().map_err(|_| bad())?;
    let mtime_ns = next()?.parse().map_err(|_| bad())?;
    Ok(Etag {
        mtime_ns,
        size,
        ino,
    })
}

/// A write refused because the file moved on since it was read.
fn changed(rel: &str, now: Etag) -> String {
    format!(
        "{rel} changed since it was read: read it again (its etag is now {})",
        etag_string(now)
    )
}

fn unsaved(rel: &str, e: SaveError) -> String {
    match e {
        SaveError::ChangedOnDisk { current } => changed(rel, current),
        e => fail(e),
    }
}

/// The byte range of the section under `heading`, or the headings there are.
fn section(text: &str, heading: &str) -> Result<Range<usize>, String> {
    markdown::section(text, heading).ok_or_else(|| {
        let headings: Vec<String> = markdown::analyze(text)
            .headings
            .into_iter()
            .map(|h| h.text)
            .collect();
        format!("no heading {heading:?}; the headings are {headings:?}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(text: &str, heading: Option<&str>, mode: Mode, content: &str) -> String {
        let at = match heading {
            Some(h) => section(text, h).unwrap(),
            None => body_start(text)..text.len(),
        };
        patched(text, at, mode, content)
    }

    /// A patch changes the section's lines and nothing around them: the heading, the blank
    /// line before the next one, the frontmatter, a note ending without a newline.
    #[test]
    fn a_patch_keeps_what_is_around_the_section() {
        let note = "# A\nold\n\n# B\nb\n";
        assert_eq!(
            patch(note, Some("a"), Mode::Replace, "new"),
            "# A\nnew\n\n# B\nb\n"
        );
        assert_eq!(patch(note, Some("a"), Mode::Replace, ""), "# A\n\n# B\nb\n");
        assert_eq!(
            patch(note, Some("a"), Mode::Append, "more\n"),
            "# A\nold\nmore\n\n# B\nb\n"
        );
        assert_eq!(
            patch(note, Some("b"), Mode::Prepend, "first"),
            "# A\nold\n\n# B\nfirst\nb\n"
        );
        assert_eq!(
            patch("# A\n# B\n", Some("a"), Mode::Replace, "x"),
            "# A\nx\n# B\n"
        );
        assert_eq!(patch("# A", Some("a"), Mode::Append, "x"), "# A\nx\n");

        let fm = "---\nk: v\n---\n# T\nbody\n";
        assert_eq!(
            patch(fm, None, Mode::Prepend, "top"),
            "---\nk: v\n---\ntop\n# T\nbody\n"
        );
        assert_eq!(patch("body", None, Mode::Append, "more"), "body\nmore\n");
    }
}
