//! The tools, each a façade call or two run on the blocking pool, and the arguments they take.

use std::ops::Range;

use accent_api::Read;
use accent_core::markdown;
use base64::Engine;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{MAX_OUT, Server, answer, etag_string, fail, image_type, line_at, locked};

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
struct LinkArgs {
    /// A link target as a note writes it: `Note`, `Note#Heading`, `folder/Note.md`, or a whole
    /// `[[Note#Heading|alias]]`.
    target: String,
}

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
