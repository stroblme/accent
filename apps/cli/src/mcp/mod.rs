//! Serve vault tools over stdio MCP using the app's façade, config and shared index, including
//! while the app is closed. Restrict reads and results to the real vault root, excluding
//! external symlink targets (DESIGN.md, MCP).

use std::collections::HashMap;
use std::path::{Component, Path};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use accent_api::{Etag, Event, Read, Vault};
use accent_core::config::Config;
use accent_core::template;
use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool_handler};
use serde_json::Value;

mod explore;
mod graph;
mod precise;
mod render;
mod tools;

/// Maximum read size. Larger notes require section reads; larger images are omitted.
const MAX_OUT: u64 = 1024 * 1024;

/// Maximum wait for an initial index walk before returning partial results.
const WAIT: Duration = Duration::from_secs(20);

/// Server instructions shown to the agent before its first request.
const INSTRUCTIONS: &str = "The notes of one accent vault: markdown files linked by [[wikilinks]] \
    and tagged with #tags, beside PDFs, code and other files, indexed for search. Start with \
    explore: one call answers a question, a name, a path, a [[link]] or a #tag with the files \
    it is about, their outlines, links in and out, and the matching sections verbatim with line \
    numbers; a path alone answers with that file's card. Every path is relative to the vault \
    root. A write takes the etag the last read gave, and is refused when the file changed \
    since.";

/// Serve the vault at `root`, its index at `db`, until stdin closes; `read_only` leaves out the
/// tools that write.
pub fn run(root: &Path, db: &Path, read_only: bool, allow_jobs: bool) -> Result<()> {
    let cfg = Config::load().local_vault(root);
    let (vault, events) = Vault::open_at(root, db, cfg)?;
    let ready = Arc::new(Ready::default());
    let drain = ready.clone();
    std::thread::Builder::new()
        .name("accent-mcp-events".to_string())
        .spawn(move || drain.follow(events))?;
    let mut tool_router = Server::tool_router();
    if read_only {
        for tool in tools::WRITE_TOOLS {
            tool_router.remove_route(tool);
        }
        for tool in tools::MUTATING_JOB_TOOLS {
            tool_router.remove_route(tool);
        }
        for tool in tools::MUTATING_TERMINAL_TOOLS {
            tool_router.remove_route(tool);
        }
    }
    if !allow_jobs {
        for tool in tools::JOB_TOOLS {
            tool_router.remove_route(tool);
        }
        for tool in tools::TERMINAL_TOOLS {
            tool_router.remove_route(tool);
        }
    } else {
        crate::jobs::prune(root)?;
    }
    let shared = Arc::new(Shared {
        vault,
        ready,
        search: Mutex::new(()),
    });
    let server = Server {
        shared: shared.clone(),
        tool_router,
    };
    let served = accent_lsp::runtime().block_on(async move {
        server
            .serve(rmcp::transport::stdio())
            .await?
            .waiting()
            .await?;
        Ok(())
    });
    // The vault last, here: closing it stops the language servers `precise` started, which
    // blocks on the runtime and so must not happen on one of its threads.
    drop(shared);
    served
}

/// Whether the initial index walk has completed.
#[derive(Default)]
struct Ready {
    state: Mutex<Indexing>,
    changed: Condvar,
}

#[derive(Default)]
struct Indexing {
    done: bool,
    /// Indexed and total file counts during the first walk.
    progress: (usize, usize),
}

impl Ready {
    /// Consume vault events until close to prevent unbounded channel growth; track progress and errors.
    fn follow(&self, events: Receiver<Event>) {
        for event in events {
            match event {
                Event::Progress(p) => locked(&self.state).progress = (p.done, p.total),
                // A failed walk has ended as well, as has a gone worker's; the error, if new, is the
                // line below.
                Event::Reconciled(_) | Event::WalkFailed | Event::WorkerGone => {
                    locked(&self.state).done = true;
                    self.changed.notify_all();
                }
                Event::Error(e) => tracing::warn!("{e}"),
                _ => {}
            }
        }
    }

    /// Wait up to [`WAIT`] for the first walk; return `None` if complete or its progress otherwise.
    fn wait(&self) -> Option<String> {
        let state = locked(&self.state);
        let (state, _) = self
            .changed
            .wait_timeout_while(state, WAIT, |s| !s.done)
            .unwrap_or_else(PoisonError::into_inner);
        (!state.done).then(|| format!("{}/{}", state.progress.0, state.progress.1))
    }
}

/// State shared by tool calls on the blocking pool.
struct Shared {
    vault: Vault,
    ready: Arc<Ready>,
    /// Serialize agent searches because the vault cancels a preceding search.
    search: Mutex<()>,
}

impl Shared {
    /// `rel` if it names a place inside the vault's real root, or why not: no absolute path, no
    /// `..`, nothing in `.git`, and nothing a symlink takes outside the root. A file not there
    /// yet is held to where its nearest folder is.
    fn inside(&self, rel: &str) -> Result<String, String> {
        let plain = !rel.is_empty()
            && Path::new(rel)
                .components()
                .all(|c| matches!(c, Component::Normal(n) if n != ".git"));
        let outside = || format!("{rel}: not a path inside the vault");
        if !plain {
            return Err(outside());
        }
        let path = self.vault.resolve(rel).map_err(|_| outside())?;
        let real = path
            .ancestors()
            .find_map(|p| p.canonicalize().ok())
            .ok_or_else(outside)?;
        match real.starts_with(self.vault.root()) {
            true => Ok(rel.to_string()),
            false => Err(outside()),
        }
    }

    /// Check index results with [`inside`](Self::inside) to exclude external symlink targets.
    fn shown(&self, rel: &str) -> bool {
        self.inside(rel).is_ok()
    }

    /// Return up to `limit` safe rows from `fetch`, requesting more when external symlinks
    /// occupy top results.
    fn first_shown<T>(
        &self,
        limit: usize,
        fetch: impl Fn(usize) -> Result<Vec<T>, String>,
        rel: impl Fn(&T) -> &str,
    ) -> Result<Vec<T>, String> {
        let mut asked = limit;
        loop {
            let rows = fetch(asked)?;
            let all = rows.len() < asked;
            let shown: Vec<T> = rows
                .into_iter()
                .filter(|r| self.shown(rel(r)))
                .take(limit)
                .collect();
            if shown.len() == limit || all {
                return Ok(shown);
            }
            asked *= 2;
        }
    }

    /// Read the first matching template, refusing symlinks outside the vault.
    fn template(&self, name: &str) -> Result<(), String> {
        let dir = self.vault.config().templates_dir;
        match template::candidates(&dir, name)
            .into_iter()
            .find(|rel| self.vault.exists(rel))
        {
            Some(rel) => self.inside(&rel).map(drop),
            None => Ok(()),
        }
    }

    /// Wait for the index to take in what was just written, so that what the agent asks next
    /// finds it. Not while the first walk runs, which holds the write back until it ends: the
    /// index reads say they are partial meanwhile.
    fn settle(&self) {
        if locked(&self.ready.state).done {
            self.vault.settle_index();
        }
    }

    /// Read text using the encoding assumed by indexed offsets.
    fn text(&self, rel: &str) -> Result<accent_api::Text, String> {
        match self.vault.read_text(rel).map_err(fail)? {
            Read::Text(t) => Ok(t),
            _ => Err(format!("{rel} is not a text file")),
        }
    }

    /// The 1-based line `byte` is on in each source note, and that line, each note read once:
    /// what a backlink or a highlight is quoted with.
    fn lines(&self, at: impl IntoIterator<Item = (String, usize)>) -> Vec<(String, usize, String)> {
        let mut read: HashMap<String, String> = HashMap::new();
        let mut out = Vec::new();
        for (src, byte) in at {
            if !self.shown(&src) {
                continue;
            }
            let text = read
                .entry(src.clone())
                .or_insert_with(|| self.text(&src).map(|t| t.text).unwrap_or_default());
            let (line, quote) = line_at(text, byte);
            out.push((src, line, quote));
        }
        out
    }
}

/// MCP tool routes and their shared state.
#[derive(Clone)]
struct Server {
    shared: Arc<Shared>,
    tool_router: ToolRouter<Self>,
}

impl Server {
    /// Run synchronous façade calls on the blocking pool, leaving runtime workers for language servers.
    async fn blocking(
        &self,
        body: impl FnOnce(&Shared) -> Result<CallToolResult, String> + Send + 'static,
    ) -> Result<CallToolResult, String> {
        let shared = self.shared.clone();
        tokio::task::spawn_blocking(move || body(&shared))
            .await
            .map_err(fail)?
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("accent", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Return compact JSON and initial-index progress when applicable.
fn answer(value: Value, partial: Option<String>) -> CallToolResult {
    let mut content = vec![ContentBlock::text(value.to_string())];
    if let Some(at) = partial {
        content.push(ContentBlock::text(format!(
            "The index is still being built ({at} files), so this may be incomplete."
        )));
    }
    CallToolResult::success(content)
}

/// The 1-based line `byte` is on, and that line, cut to what a list row needs. The offset is the
/// index's, which a note edited since may no longer hold.
fn line_at(text: &str, byte: usize) -> (usize, String) {
    let byte = text.floor_char_boundary(byte);
    let start = text[..byte].rfind('\n').map_or(0, |n| n + 1);
    let end = text[byte..].find('\n').map_or(text.len(), |n| byte + n);
    let line = text[..start].matches('\n').count() + 1;
    (line, text[start..end].trim().chars().take(200).collect())
}

/// Encode etags as strings to preserve nanosecond timestamps in JavaScript clients.
fn etag_string(e: Etag) -> String {
    format!("{}-{}-{}", e.mtime_ns, e.size, e.ino)
}

/// Image extensions supported by clients.
fn image_type(rel: &str) -> Option<&'static str> {
    let ext = Path::new(rel).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

fn fail(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
