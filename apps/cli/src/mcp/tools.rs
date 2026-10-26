//! The tools, each a façade call or two run on the blocking pool, and the arguments they take.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use accent_api::{Error, Etag, FileKind, Read, fs};
use accent_core::note::Syntax;
use accent_core::path::linked_path;
use accent_core::{markdown, notebook, path};
use base64::Engine;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    MAX_OUT, Server, Shared, answer, etag_string, explore, fail, image_type, line_at, locked,
};

#[derive(Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// Search terms in any order. The final term also matches word prefixes; if no words match,
    /// retry as substrings.
    query: String,
    /// Maximum rows: 20 by default, up to 100. Each row is one occurrence.
    limit: Option<usize>,
    /// Include files ignored by Git.
    include_ignored: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ReadArgs {
    /// The file, relative to the vault root.
    path: String,
    /// Read this heading and its subsections by heading text or slug (e.g. `notes-1`).
    heading: Option<String>,
    /// An `.ipynb`'s indexed cell sources (default) or `raw` editable JSON; other files are raw.
    view: Option<ReadView>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ReadView {
    Raw,
    Sources,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DiagramArgs {
    /// The draw.io diagram, relative to the vault root.
    path: String,
    /// The page, by its number counted from 0 or by its name as the page list gives it; the
    /// first when left out.
    page: Option<PageArg>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum PageArg {
    Number(usize),
    Name(String),
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
    /// Maximum rows: 20 by default, up to 500.
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct LinkArgs {
    /// Link target as written in a note, such as `Note#Heading` or `[[Note#Heading|alias]]`.
    target: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PatchArgs {
    /// The note, relative to the vault root.
    path: String,
    /// Section accepted by `read_note`. Omit for the whole note with `append` or `prepend`;
    /// use `write_note` to replace a whole note.
    heading: Option<String>,
    /// Patch mode: replace section text, append after it, or prepend below its heading
    /// (or below frontmatter for the whole note).
    mode: Mode,
    /// Text to insert.
    content: String,
    /// Etag from `read_note`; omit to patch the current version.
    etag: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WriteArgs {
    /// File path relative to the vault root; missing parent folders are created.
    path: String,
    /// Complete file text.
    content: String,
    /// Etag from `read_note`, required to overwrite an unchanged file. Omit for a new file.
    etag: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TemplateArgs {
    /// Template name or path in the vault's templates folder.
    template: String,
    /// New note path relative to the vault root; `.md` is added if no extension is given.
    /// Omit to use the template's `accent-target:`.
    path: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct StartJobArgs {
    /// Existing directory relative to the vault root; the root when omitted.
    cwd: Option<String>,
    /// Program and arguments. To use shell syntax, explicitly pass ["sh", "-lc", "..."].
    argv: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct JobIdArgs {
    id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ReadJobArgs {
    id: String,
    /// Byte offset returned by the previous read, starting at zero.
    offset: Option<u64>,
    /// Maximum bytes to read, up to 256 KiB.
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct StartTerminalArgs {
    /// Existing directory relative to the vault root; the root when omitted.
    cwd: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct TerminalIdArgs {
    id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ReadTerminalArgs {
    id: String,
    /// Absolute byte cursor returned by the previous read; zero for all retained output.
    offset: Option<u64>,
    /// Maximum recent output bytes to return, up to 64 KiB.
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SendTerminalArgs {
    id: String,
    /// Literal terminal input. Include `\n` to press Enter.
    input: String,
}

/// Tools omitted by `--read-only`.
pub(super) const WRITE_TOOLS: [&str; 3] = ["patch_note", "write_note", "create_note_from_template"];
pub(super) const JOB_TOOLS: [&str; 4] = ["start_job", "list_jobs", "read_job", "stop_job"];
pub(super) const MUTATING_JOB_TOOLS: [&str; 2] = ["start_job", "stop_job"];
pub(super) const TERMINAL_TOOLS: [&str; 5] = [
    "start_terminal",
    "list_terminals",
    "read_terminal",
    "send_terminal",
    "stop_terminal",
];
pub(super) const MUTATING_TERMINAL_TOOLS: [&str; 3] =
    ["start_terminal", "send_terminal", "stop_terminal"];

#[tool_router(vis = "pub(super)")]
impl Server {
    /// Start a held interactive terminal on this host; it survives MCP disconnection.
    #[tool]
    async fn start_terminal(
        &self,
        Parameters(a): Parameters<StartTerminalArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let terminal = crate::hold::mcp::start(&s.vault.root(), a.cwd.as_deref().unwrap_or(""))
                .map_err(fail)?;
            Ok(answer(terminal, None))
        })
        .await
    }

    /// List this vault's held terminals and GTK terminals explicitly shared with it.
    #[tool(annotations(read_only_hint = true))]
    async fn list_terminals(&self) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let terminals = crate::hold::mcp::list(&s.vault.root()).map_err(fail)?;
            Ok(answer(json!(terminals), None))
        })
        .await
    }

    /// Read a held terminal's recent output and current screen without taking its attachment.
    #[tool(annotations(read_only_hint = true))]
    async fn read_terminal(
        &self,
        Parameters(a): Parameters<ReadTerminalArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let output =
                crate::hold::mcp::read(&s.vault.root(), &a.id, a.offset.unwrap_or(0), a.limit)
                    .map_err(fail)?;
            Ok(answer(output, None))
        })
        .await
    }

    /// Send literal keystrokes to a held terminal; include a newline to execute a command.
    #[tool]
    async fn send_terminal(
        &self,
        Parameters(a): Parameters<SendTerminalArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            crate::hold::mcp::send(&s.vault.root(), &a.id, &a.input).map_err(fail)?;
            Ok(answer(json!({"id": a.id, "sent": true}), None))
        })
        .await
    }

    /// Stop a held terminal and its foreground job.
    #[tool]
    async fn stop_terminal(
        &self,
        Parameters(a): Parameters<TerminalIdArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            crate::hold::mcp::stop(&s.vault.root(), &a.id).map_err(fail)?;
            Ok(answer(json!({"id": a.id, "stopped": true}), None))
        })
        .await
    }

    /// Start a detached command on this host. It continues after MCP or SSH disconnects.
    #[tool]
    async fn start_job(
        &self,
        Parameters(a): Parameters<StartJobArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let job = crate::jobs::start(&s.vault.root(), a.cwd.as_deref().unwrap_or(""), &a.argv)
                .map_err(fail)?;
            Ok(answer(serde_json::to_value(job).map_err(fail)?, None))
        })
        .await
    }

    /// List recent jobs in this vault, including their exit state and truncated-output flag.
    #[tool(annotations(read_only_hint = true))]
    async fn list_jobs(&self) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let jobs = crate::jobs::list(&s.vault.root()).map_err(fail)?;
            Ok(answer(serde_json::to_value(jobs).map_err(fail)?, None))
        })
        .await
    }

    /// Read a bounded output chunk by byte offset and get the next offset.
    #[tool(annotations(read_only_hint = true))]
    async fn read_job(
        &self,
        Parameters(a): Parameters<ReadJobArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let output = crate::jobs::read(&s.vault.root(), &a.id, a.offset.unwrap_or(0), a.limit)
                .map_err(fail)?;
            Ok(answer(serde_json::to_value(output).map_err(fail)?, None))
        })
        .await
    }

    /// Stop a live job's process group, with a brief TERM grace before KILL.
    #[tool]
    async fn stop_job(
        &self,
        Parameters(a): Parameters<JobIdArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let job = crate::jobs::stop(&s.vault.root(), &a.id).map_err(fail)?;
            Ok(answer(serde_json::to_value(job).map_err(fail)?, None))
        })
        .await
    }

    /// Full-text search over the vault's indexed files. Each row is one occurrence: the file,
    /// its title, the 1-based line, and a snippet with the match between « and ». A PDF's rows are
    /// its pages instead, one a page holding the match, with the 1-based `page` and no line.
    #[tool(annotations(read_only_hint = true))]
    async fn search_notes(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let partial = s.ready.wait();
            let limit = a.limit.unwrap_or(20).min(100);
            let ignored = a.include_ignored.unwrap_or(false);
            let hits = {
                let _turn = locked(&s.search);
                s.first_shown(
                    limit,
                    |n| s.vault.search(&a.query, n, ignored).map_err(fail),
                    |h| &h.rel_path,
                )?
            };
            let rows: Vec<Value> = hits
                .into_iter()
                .map(|h| {
                    let mut row = json!({"path": h.rel_path, "title": h.title, "line": h.line, "snippet": h.snippet});
                    if let Some(page) = h.page {
                        row["page"] = json!(page);
                    }
                    row
                })
                .collect();
            Ok(answer(json!(rows), partial))
        })
        .await
    }

    /// Read file text or one heading section, with an etag for writing. An `.ipynb` defaults to
    /// indexed cell sources without a write etag; `view: "raw"` returns editable JSON and its
    /// etag. A PDF returns its indexed text, each page under a `--- page N ---` line, without
    /// an etag. Images return image data and other binaries return their size.
    #[tool(annotations(read_only_hint = true))]
    async fn read_note(
        &self,
        Parameters(a): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let rel = s.inside(&a.path)?;
            if path::file_type(&rel) == path::FileType::Pdf {
                if a.heading.is_some() {
                    return Err("heading is unavailable in a PDF's text".into());
                }
                return read_pdf(s, rel);
            }
            match s.vault.read_text(&rel).map_err(fail)? {
                Read::Text(t) => {
                    let default = match path::is_notebook(&rel) {
                        true => ReadView::Sources,
                        false => ReadView::Raw,
                    };
                    let (text, sources) = match a.view.unwrap_or(default) {
                        ReadView::Raw => (Cow::Borrowed(t.text.as_str()), false),
                        ReadView::Sources => {
                            if !path::is_notebook(&rel) {
                                return Err(format!("{rel} is not an .ipynb notebook"));
                            }
                            let text = notebook::search_text(&rel, &t.text)
                                .ok_or_else(|| format!("{rel} is not a readable notebook"))?;
                            (Cow::Owned(text), true)
                        }
                    };
                    let text = match &a.heading {
                        Some(_) if sources => {
                            return Err("heading is unavailable in notebook sources".into());
                        }
                        Some(heading) => &text[section(&rel, &text, heading)?],
                        None => text.as_ref(),
                    };
                    if text.len() as u64 > MAX_OUT {
                        let how = if sources {
                            ""
                        } else {
                            ": read it a section at a time with `heading`"
                        };
                        return Err(format!(
                            "{rel} holds {} bytes, more than a read returns ({MAX_OUT}){how}",
                            text.len()
                        ));
                    }
                    let mut head = match sources {
                        true => json!({"path": rel, "view": "sources"}),
                        false => json!({"path": rel, "etag": etag_string(t.etag)}),
                    };
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

    /// Draw one page of a draw.io diagram as a PNG, as the app's Export as PNG draws it (formulas
    /// typeset, pictures from the web as dashed boxes), to see an edit made with
    /// `write_note`. Also returns `{path, etag}` and every page's number, name and size. Needs
    /// the accent app and a display where this server runs, so not on a remote vault's host.
    #[tool(annotations(read_only_hint = true))]
    async fn render_diagram(
        &self,
        Parameters(a): Parameters<DiagramArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let rel = s.inside(&a.path)?;
            let not_one = || format!("{rel} is not a draw.io diagram");
            let Read::Text(t) = s.vault.read_text(&rel).map_err(fail)? else {
                return Err(not_one());
            };
            if !accent_core::path::is_diagram(&rel) && !accent_drawio::text::sniff(&t.text) {
                return Err(not_one());
            }
            let file = accent_drawio::File::from_bytes(t.text.as_bytes())
                .map_err(|e| format!("{}: {e}", not_one()))?;
            let names = file.page_names();
            let page = match a.page {
                None => 0,
                Some(PageArg::Number(n)) if n < names.len() => n,
                Some(PageArg::Number(n)) => return Err(format!("{rel} has no page {n}")),
                Some(PageArg::Name(name)) => names
                    .iter()
                    .position(|n| *n == name.trim())
                    .ok_or_else(|| {
                        format!("{rel} has no page {name:?}; its pages are {names:?}")
                    })?,
            };
            let png = super::render::png(&t.text, page)?;
            let pages: Vec<Value> = file
                .pages
                .iter()
                .zip(&names)
                .enumerate()
                .map(|(i, (p, name))| {
                    let (width, height) = p.size();
                    json!({"page": i, "name": name, "width": width, "height": height})
                })
                .collect();
            let head =
                json!({"path": rel, "etag": etag_string(t.etag), "page": page, "pages": pages});
            let data = base64::engine::general_purpose::STANDARD.encode(png);
            Ok(CallToolResult::success(vec![
                ContentBlock::text(head.to_string()),
                ContentBlock::image(data, "image/png"),
            ]))
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

    /// Resolve a link as Accent would open it, including the 1-based line of its heading or block.
    /// `path` is null if the vault has no target.
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
                let at = Syntax::of(&rel).anchor_range(&text, anchor)?;
                Some(line_at(&text, at.start).0)
            });
            Ok(answer(json!({"path": rel, "line": line}), partial))
        })
        .await
    }

    /// Explore words, names, paths, `[[links]]`, and `#tags`. Return ranked Markdown cards
    /// with line-numbered excerpts and omitted file names. A path alone returns its whole
    /// card. `precise` requests slower language-server call hierarchy.
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

    /// List folder entries, folders first, with path, kind, size, UTC modification time, note title,
    /// and whether the index includes each entry.
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
                    // An org file's `#+TITLE:` is a title as a note's is.
                    let titled = f.kind == FileKind::Markdown || path::is_org(&f.rel_path);
                    if let Some(title) = f.title.filter(|_| titled) {
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

    /// List non-ignored files by UTC modification time, newest first.
    #[tool(annotations(read_only_hint = true))]
    async fn recent_changes(
        &self,
        Parameters(a): Parameters<LimitArgs>,
    ) -> Result<CallToolResult, String> {
        self.blocking(move |s| {
            let partial = s.ready.wait();
            let limit = a.limit.unwrap_or(20).min(500);
            let rows: Vec<Value> = s
                .first_shown(
                    limit,
                    |n| s.vault.recent_files(n).map_err(fail),
                    String::as_str,
                )?
                .into_iter()
                .filter_map(|rel| {
                    let etag = s.vault.stat(&rel).ok()??;
                    Some(json!({"path": rel, "modified": modified(etag.mtime_ns)}))
                })
                .collect();
            Ok(answer(json!(rows), partial))
        })
        .await
    }

    /// List unresolved link targets and up to five linking notes and lines per target.
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

    /// List PDF highlights stored as note links, with page, quoted text, note, and line.
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
                (Some(heading), _) => section(&rel, &t.text, heading)?,
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

    /// Create a note from a template. Without `path`, use its `accent-target:`. Leave an
    /// existing target unchanged and return its path and creation status.
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

/// A PDF as `read_note` answers it: the text the index holds, by page, without an etag, as
/// there is no writing it back.
fn read_pdf(s: &Shared, rel: String) -> Result<CallToolResult, String> {
    let Some(etag) = s.vault.stat(&rel).map_err(fail)? else {
        return Err(format!("{rel}: no such file"));
    };
    let Some((text, pages)) = s.pdf_text(&rel)? else {
        let mut out = answer(json!({"path": rel, "kind": "pdf", "size": etag.size}), None);
        out.content.push(ContentBlock::text(format!(
            "The index holds no text of {rel}: it is scanned, or its text is not read yet."
        )));
        return Ok(out);
    };
    if text.len() as u64 > MAX_OUT {
        return Err(format!(
            "{rel}'s text holds {} bytes, more than a read returns ({MAX_OUT}): explore or \
             search_notes find its pages",
            text.len()
        ));
    }
    let head = json!({"path": rel, "kind": "pdf", "pages": pages});
    Ok(CallToolResult::success(vec![
        ContentBlock::text(head.to_string()),
        ContentBlock::text(text),
    ]))
}

/// File kind used by `list_dir`.
fn kind_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Dir => "dir",
        FileKind::Markdown => "note",
        FileKind::Pdf => "pdf",
        FileKind::Other => "file",
        FileKind::Conflict => "conflict",
    }
}

/// Format modification time as RFC 3339 UTC, to the second.
fn modified(mtime_ns: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(mtime_ns)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Patch placement: replace, append, or prepend.
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

/// Offset after frontmatter, which prepend must preserve.
fn body_start(text: &str) -> usize {
    markdown::analyze(text)
        .spans
        .iter()
        .find(|s| matches!(s.style, markdown::Style::Frontmatter))
        .map_or(0, |s| {
            s.range.end + usize::from(text[s.range.end..].starts_with('\n'))
        })
}

/// Parse an etag formatted by [`etag_string`].
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

/// Represent a write rejected because the file changed since reading.
fn changed(rel: &str, now: Etag) -> String {
    format!(
        "{rel} changed since it was read: read it again (its etag is now {})",
        etag_string(now)
    )
}

fn unsaved(rel: &str, e: Error) -> String {
    match e {
        Error::Core(accent_core::Error::ChangedOnDisk { current }) => changed(rel, current),
        e => fail(e),
    }
}

/// Find the section byte range under `heading` in `rel`, read as its syntax writes headings, or
/// the headings it has.
fn section(rel: &str, text: &str, heading: &str) -> Result<Range<usize>, String> {
    let headings = Syntax::of(rel).analyze(text).headings;
    markdown::section_in(text, &headings, heading).ok_or_else(|| {
        let names: Vec<&str> = headings.iter().map(|h| h.text.as_str()).collect();
        format!("no heading {heading:?}; the headings are {names:?}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(text: &str, heading: Option<&str>, mode: Mode, content: &str) -> String {
        let at = match heading {
            Some(h) => section("note.md", text, h).unwrap(),
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
