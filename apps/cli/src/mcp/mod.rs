//! `accent-cli mcp`: a vault served to an agent over the Model Context Protocol, on stdio.
//!
//! The same façade, index and vault config as the app (DESIGN.md, MCP), so it answers with the
//! app closed and shares the app's index while it is open. What it hands out is held to the
//! vault's real root: a path a symlink takes outside it is neither read nor listed.

use std::collections::HashMap;
use std::path::{Component, Path};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use accent_api::{Etag, Event, Read, Vault};
use accent_core::config::Config;
use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool_handler};
use serde_json::Value;

mod tools;

/// Most a read hands back: a note past it is read a section at a time, an image past it not at
/// all. A client holds the whole answer in its context.
const MAX_OUT: u64 = 1024 * 1024;

/// How long an index read waits for the first walk of a cold index before it answers from what
/// is there, saying so.
const WAIT: Duration = Duration::from_secs(20);

/// What the agent is told about the server before it asks anything.
const INSTRUCTIONS: &str = "The notes of one accent vault: markdown files linked by [[wikilinks]] \
    and tagged with #tags, beside PDFs and other files, indexed for search. Every path is \
    relative to the vault root.";

/// Serve the vault at `root`, its index at `db`, until stdin closes.
pub fn run(root: &Path, db: &Path) -> Result<()> {
    let cfg = Config::load().vault(root);
    let (vault, events) = Vault::open_at(root, db, cfg)?;
    let ready = Arc::new(Ready::default());
    let drain = ready.clone();
    std::thread::Builder::new()
        .name("accent-mcp-events".to_string())
        .spawn(move || drain.follow(events))?;
    let server = Server {
        shared: Arc::new(Shared {
            vault,
            ready,
            search: Mutex::new(()),
        }),
        tool_router: Server::tool_router(),
    };
    accent_lsp::runtime().block_on(async move {
        server
            .serve(rmcp::transport::stdio())
            .await?
            .waiting()
            .await?;
        Ok(())
    })
}

/// Whether the index has been brought up to date since the vault was opened.
#[derive(Default)]
struct Ready {
    state: Mutex<Indexing>,
    changed: Condvar,
}

#[derive(Default)]
struct Indexing {
    done: bool,
    /// Files indexed of how many, while the first walk runs.
    progress: (usize, usize),
}

impl Ready {
    /// Keep up with the vault's events until it closes. They have to be read, the channel being
    /// unbounded; all that matters here is how far the index is, and what went wrong.
    fn follow(&self, events: Receiver<Event>) {
        for event in events {
            match event {
                Event::Progress(p) => locked(&self.state).progress = (p.done, p.total),
                Event::Reconciled(_) => {
                    locked(&self.state).done = true;
                    self.changed.notify_all();
                }
                Event::Error(e) => tracing::warn!("{e}"),
                _ => {}
            }
        }
    }

    /// Wait for the first walk, at most [`WAIT`]: `None` once it has ended, or how far it got.
    fn wait(&self) -> Option<String> {
        let state = locked(&self.state);
        let (state, _) = self
            .changed
            .wait_timeout_while(state, WAIT, |s| !s.done)
            .unwrap_or_else(PoisonError::into_inner);
        (!state.done).then(|| format!("{}/{}", state.progress.0, state.progress.1))
    }
}

/// What every tool call shares, handed to the blocking pool whole.
struct Shared {
    vault: Vault,
    ready: Arc<Ready>,
    /// Searches take turns: the vault's search stops the one before it, as the Search pane wants
    /// and two calls of an agent's do not.
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

    /// Whether a path the index answered with may be shown: [`inside`](Self::inside) holds for
    /// results as for arguments, or a search would quote what a symlink brings in from outside.
    fn shown(&self, rel: &str) -> bool {
        self.inside(rel).is_ok()
    }

    /// A text file's text, as the index counted its offsets.
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

/// The protocol's handler: the tools' routes, and what they share.
#[derive(Clone)]
struct Server {
    shared: Arc<Shared>,
    tool_router: ToolRouter<Self>,
}

impl Server {
    /// Run a tool's body on the blocking pool: the façade is synchronous, and the runtime's two
    /// workers are the language servers' as well.
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

/// A tool's answer: compact JSON, and a word on the index while it is still being built.
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

/// The etag as the wire carries it: a string, its nanosecond mtime being past the 2^53 a
/// JavaScript client's number holds exactly.
fn etag_string(e: Etag) -> String {
    format!("{}-{}-{}", e.mtime_ns, e.size, e.ino)
}

/// The image types a client shows, by extension.
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
