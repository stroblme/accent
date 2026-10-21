//! Completion, navigation, symbols, folds, and diagnostics for text documents. Code uses
//! `accent-lsp`; notes use the index through the same interface. Positions use zero-based lines
//! and Unicode scalar columns, matching `GtkTextIter`. Server encodings are converted beside the
//! text. Requests run as [`Task`]s on the shared runtime and cancel when dropped.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::{Error, Result};
use accent_core::index::Index;
use accent_core::markdown;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::local::Ignored;
use crate::remote::Remote;
use crate::vault::Backend;
use crate::{Event, FileEdits, Local, LspConfig, Vault, VaultConfig, locked};

pub(crate) mod external;
mod latex;
pub(crate) mod notes;
pub(crate) mod words;

use notes::Notes;

/// A line and a column, both zero-based; the column counts characters from the line start.
/// Ordered as the document is, the line first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct Pos {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Range {
    pub start: Pos,
    pub end: Pos,
}

/// A navigation target: `path` is vault-relative inside the vault, absolute
/// outside it, or a URL for external links.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub range: Range,
    /// A `#anchor` not represented by `range`, such as a PDF selection or a missing
    /// heading/block. Missing text anchors leave `range` at the file start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// Nothing is there yet: `path` is the file following the link would create.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub missing: bool,
}

impl Location {
    /// Somewhere the system opens rather than the vault: see [`markdown::is_url`], which a click
    /// in the preview asks as well.
    pub fn is_url(&self) -> bool {
        markdown::is_url(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextEdit {
    pub range: Range,
    pub text: String,
}

/// Completion glyph kinds: supported LSP kinds plus `Tag` for note tags.
/// Unsupported server kinds map to `Text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Enum,
    Keyword,
    Snippet,
    File,
    Folder,
    EnumMember,
    Constant,
    Struct,
    TypeParameter,
    Tag,
}

impl Kind {
    /// The LSP `CompletionItemKind` number, or `Text` for one accent has no glyph for.
    pub fn from_lsp(n: u32) -> Kind {
        match n {
            2 => Kind::Method,
            3 => Kind::Function,
            4 => Kind::Constructor,
            5 => Kind::Field,
            6 => Kind::Variable,
            7 => Kind::Class,
            8 => Kind::Interface,
            9 => Kind::Module,
            10 => Kind::Property,
            13 => Kind::Enum,
            14 => Kind::Keyword,
            15 => Kind::Snippet,
            17 => Kind::File,
            19 => Kind::Folder,
            20 => Kind::EnumMember,
            21 => Kind::Constant,
            22 => Kind::Struct,
            25 => Kind::TypeParameter,
            _ => Kind::Text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub label: String,
    pub kind: Kind,
    /// A type or a path, shown after the label.
    pub detail: Option<String>,
    /// Markdown, shown in the details panel; may only arrive after `resolve`.
    pub doc: Option<String>,
    /// What typed text is matched against, when it is not the label.
    pub filter: Option<String>,
    pub insert: String,
    /// `insert` is a snippet (`$1`, `${2:name}`), not literal text.
    pub is_snippet: bool,
    /// Range replaced by `insert`, including the prefix before the caret
    /// and any suffix the completion replaces.
    pub replace: Range,
    /// Applied with the insert, in one undo step: an import, typically.
    pub extra_edits: Vec<TextEdit>,
    /// The item as the server sent it, when it can still be resolved for more; opaque to the UI.
    pub resolve: Option<serde_json::Value>,
    /// Preselect an exact abbreviation match, so Tab accepts a fully typed Lean
    /// abbreviation such as `\to`. Only window abbreviations set this; server
    /// `preselect` is ignored to preserve Return as a newline in code.
    #[serde(default)]
    pub preselect: bool,
    /// Characters that, typed with this row selected, accept it and are then typed after it: a
    /// server's `commitCharacters`, or its `allCommitCharacters`. No other source has any.
    #[serde(default)]
    pub commit: Vec<char>,
}

/// Completion results. When `incomplete` is true, query again as the prefix grows;
/// filtering this capped list alone could miss omitted items.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Completions {
    pub items: Vec<Completion>,
    pub incomplete: bool,
    /// PDF bookmarks deferred by a host without a PDF reader. The window lists
    /// them from its local copy ([`Vault::completion`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<PdfPages>,
}

/// Deferred `[[paper.pdf#` completion for the side holding a readable PDF:
/// the resolved file and the typed target retained in each page row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfPages {
    /// The PDF the link resolved to, vault-relative.
    pub rel: String,
    /// The link's target as it was typed, which every row spells the same way.
    pub note: String,
    pub replace: Range,
}

/// The signature of the call containing the caret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signature {
    pub label: String,
    pub doc: Option<String>,
    /// Character ranges into `label`, one per parameter.
    pub params: Vec<(u32, u32)>,
    /// Which of `params` the caret is on.
    pub active: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hover {
    /// Markdown.
    pub text: String,
    pub range: Option<Range>,
}

/// Proof-assistant state at a position, displayed as plain text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goals {
    /// The open goals of the tactic proof there, each as its hypotheses then `⊢ target`;
    /// `None` outside a tactic proof, empty once the proof is finished.
    pub tactic: Option<Vec<String>>,
    /// The type the term there must have.
    pub expected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// The whole of it: the function body, the section under the heading.
    pub range: Range,
    /// The name alone, where a jump lands.
    pub selection: Range,
    pub children: Vec<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    /// The server's name for the rule it reports, a number as its text: pyright's
    /// `reportUnusedExpression`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// A language-server call-hierarchy edge relative to the queried declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// The caller for incoming calls or callee for outgoing calls, located by its
    /// name range inside the vault.
    pub decl: Location,
    pub name: String,
    /// The lines the calls are written on, in the caller's file, 0-based.
    pub lines: Vec<u32>,
}

/// Lines that can be hidden behind their first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fold {
    pub start_line: u32,
    pub end_line: u32,
}

/// Whether a document with this language id is prose: what gets the words in its completion.
pub fn is_prose(language_id: &str) -> bool {
    words::PROSE.contains(&language_id)
}

/// What attached to an opened document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Support {
    /// Characters that open the completion popup as they are typed.
    pub completion_triggers: Vec<char>,
    /// Characters that ask for a signature as they are typed.
    pub signature_triggers: Vec<char>,
    /// The language server accent looked for and did not find, so the UI can say so when asked.
    pub missing: Option<String>,
    /// Whether prose ghost text is available (`merl-rt` is installed), allowing
    /// the UI to enable inline suggestions when the preference is on.
    pub inline: bool,
    /// The provider answers [`Vault::goals`]: a proof assistant's server.
    pub goals: bool,
    /// The document is outside the vault, answered by the running server whose answer named
    /// it: a dependency of the code.
    pub dependency: bool,
}

// -------------------------------------------------------------------- tasks

/// A request on the shared runtime. Dropping it aborts the request and sends
/// `$/cancelRequest` to its language server. Its `JoinHandle` can be awaited
/// from any executor, including the GTK main loop.
pub struct Task<T> {
    handle: tokio::task::JoinHandle<Result<T>>,
    /// Cancellation callback for requests whose wait cannot be aborted. Remote
    /// round trips use blocking threads, so cancellation must notify the server.
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl<T> Future for Task<T> {
    type Output = Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.handle).poll(cx) {
            Poll::Ready(Ok(r)) => Poll::Ready(r),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e.into())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.handle.abort();
        if let Some(cancel) = self.on_drop.take() {
            cancel();
        }
    }
}

impl<T> Task<T> {
    /// What aborts the work this task is waiting on, for a server holding it on someone's behalf.
    pub(crate) fn abort_handle(&self) -> tokio::task::AbortHandle {
        self.handle.abort_handle()
    }

    /// Run `f` when this task is dropped, for a request whose cancellation is a message rather
    /// than an abort.
    pub(crate) fn cancelled_by(mut self, f: impl FnOnce() + Send + 'static) -> Task<T> {
        self.on_drop = Some(Box::new(f));
        self
    }
}

impl<T: Send + 'static> Task<T> {
    pub(crate) fn spawn(f: impl Future<Output = Result<T>> + Send + 'static) -> Task<T> {
        Task {
            handle: accent_lsp::runtime().spawn(f),
            on_drop: None,
        }
    }

    /// For a call that blocks: a remote round trip over the ssh client.
    pub(crate) fn blocking(f: impl FnOnce() -> Result<T> + Send + 'static) -> Task<T> {
        Task {
            handle: accent_lsp::runtime().spawn_blocking(f),
            on_drop: None,
        }
    }
}

// ----------------------------------------------------------------- providers

pub(crate) type Fut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// A provider for open documents: a language server or the index.
/// Boxed futures allow the registry to store `dyn Language`. `open`, `change`
/// and `close` are synchronous notifications or local analysis and do not wait.
pub(crate) trait Language: Send + Sync {
    fn open(&self, rel: &str, language_id: &str, text: String) -> Result<Support>;
    fn change(&self, rel: &str, text: String) -> Result<()>;
    /// Notify that the document reached disk, triggering save-only work such as
    /// rust-analyzer's `cargo check` diagnostics.
    fn saved(&self, _rel: &str) -> Result<()> {
        Ok(())
    }
    /// Notify that the user left the document. Expensive save work, such as merl
    /// rebuilding its index, runs here instead of on each autosave.
    fn settle(&self, _rel: &str) -> Result<()> {
        Ok(())
    }
    /// Refresh diagnostics when external state changes without a text edit, such
    /// as a missing link target appearing in the index. Language servers publish
    /// their own updates and ignore this.
    fn rediagnose(&self, _rel: &str) -> Result<()> {
        Ok(())
    }
    fn close(&self, rel: &str);
    fn completion(&self, rel: &str, pos: Pos, trigger: Option<char>) -> Fut<'_, Completions>;
    /// Inline ghost text from a model or vault index; `None` for providers
    /// without this capability.
    fn inline_completion(&self, _rel: &str, _pos: Pos) -> Fut<'_, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    /// Resolve deferred completion details. `rel` identifies the document whose
    /// text defines returned edit ranges.
    fn resolve(&self, rel: &str, item: Completion) -> Fut<'_, Completion>;
    fn signature_help(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Signature>>;
    fn hover(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Hover>>;
    fn definition(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>>;
    fn symbols(&self, rel: &str) -> Fut<'_, Vec<Symbol>>;
    fn references(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>>;
    fn folds(&self, rel: &str) -> Fut<'_, Vec<Fold>>;
    /// What holds at `pos` in a proof; `None` from anything that is not a proof assistant.
    fn goals(&self, _rel: &str, _pos: Pos) -> Fut<'_, Option<Goals>> {
        Box::pin(async { Ok(None) })
    }
    /// Whether the provider's `willRename` filters match the file or folder at `abs`.
    fn renames(&self, _abs: &Path, _is_dir: bool) -> bool {
        false
    }
    /// Request edits for `(old, new)` vault-relative moves, such as import updates.
    /// Ask before moving files because the server may read them to answer.
    fn will_rename(&self, _moves: Vec<(String, String)>) -> Fut<'_, Vec<FileEdits>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    /// Incoming or outgoing calls for the declaration named at `pos`. Use the
    /// open buffer when available, otherwise disk. `None` means no declaration;
    /// [`Error::NotYet`] means the provider cannot answer yet.
    fn calls(
        &self,
        _rel: &str,
        _language_id: &str,
        _pos: Pos,
        _incoming: bool,
    ) -> Fut<'_, Option<Vec<Call>>> {
        Box::pin(async { Ok(None) })
    }
    /// Whether [`Self::calls`] has returned a declaration; false while
    /// the server is still loading its project.
    fn has_answered(&self) -> bool {
        true
    }
    /// The provider stopped answering (the server exited); the registry starts a fresh one.
    fn is_dead(&self) -> bool {
        false
    }
    /// Whether an answer of this provider named `path`, a file outside the vault: a dependency
    /// of its project, which it then answers for ([`Languages::naming`]).
    fn named(&self, _path: &str) -> bool {
        false
    }
    fn shutdown(&self) -> Fut<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A session that may still be starting: concurrent openers await the same cell.
pub(crate) type Session = Arc<tokio::sync::OnceCell<Arc<dyn Language>>>;

/// What a session is keyed by: the server's name and the root it was started in.
pub(crate) type Key = (String, PathBuf);

/// The open documents: each one's provider, the session it came from, and whether it is prose.
pub(crate) type Open = HashMap<String, (Option<Key>, Arc<dyn Language>, bool)>;

/// The provider, protocol language id, missing server and session selected
/// when opening a document.
type Opened = (
    Option<Arc<dyn Language>>,
    String,
    Option<String>,
    Option<Key>,
);

/// Vault providers and their open documents. Notes and external providers
/// populate the registry; diagnostics travel through its `Event` channel.
pub(crate) struct Languages {
    pub(crate) root: PathBuf,
    pub(crate) db: PathBuf,
    pub(crate) events: Sender<Event>,
    /// One session per (server, root): the cell makes concurrent openers await one start.
    pub(crate) sessions: Mutex<HashMap<Key, Session>>,
    /// Refuse a new session after window close, including one whose start is still in flight.
    closed: AtomicBool,
    /// Each open document's provider, session and prose flag. Closing the last
    /// document stops its session. A `None` key denotes prose with word
    /// suggestions but no session.
    pub(crate) docs: Mutex<Open>,
    /// Whether the vault runs a ghost-text session; the preference behind it is global, so a
    /// vault is told it once and every document follows.
    pub(crate) ghost: AtomicBool,
    /// Serializes ghost progress with toggles and replacement sessions.
    ghost_busy: Arc<Mutex<u64>>,
    /// When the ghost session was started within the last [`GHOST_WINDOW`]; `None` once the
    /// vault has given up on it.
    ghost_starts: Mutex<Option<Vec<Instant>>>,
    /// Whether prose documents are offered words (the Word Suggestions preference, global like
    /// Ghost Text), shared with each of them.
    words: Arc<AtomicBool>,
    /// The folders a LaTeX document's `\input{` lists.
    listing: Arc<Listing>,
    /// The files in the folders git ignores, which the notes provider and the vault both ask.
    pub(crate) ignored: Arc<Ignored>,
    /// When each session [`Self::calls`] used was last asked, for its idle stop.
    asked: Mutex<HashMap<Key, tokio::time::Instant>>,
}

/// Folder listings for path completion, using [`crate::local::list_dir`] and
/// a lazily opened index connection to avoid waiting on the file tree's connection.
pub(crate) struct Listing {
    root: PathBuf,
    db: PathBuf,
    index: Mutex<Option<Index>>,
}

impl Listing {
    pub(crate) fn list_dir(&self, rel: &str) -> Result<Vec<crate::FileRow>> {
        let mut index = locked(&self.index);
        let index = match &mut *index {
            Some(index) => index,
            none => none.insert(Index::open(&self.db)?),
        };
        crate::local::list_dir(index, &self.root, rel)
    }
}

impl Languages {
    pub(crate) fn new(root: PathBuf, db: PathBuf, events: Sender<Event>) -> Arc<Languages> {
        let listing = Arc::new(Listing {
            root: root.clone(),
            db: db.clone(),
            index: Mutex::new(None),
        });
        let ignored = Arc::new(Ignored::new(root.clone()));
        Arc::new(Languages {
            root,
            db,
            events,
            sessions: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            docs: Mutex::new(HashMap::new()),
            ghost: AtomicBool::new(true),
            ghost_busy: Arc::new(Mutex::new(0)),
            ghost_starts: Mutex::new(Some(Vec::new())),
            words: Arc::new(AtomicBool::new(true)),
            listing,
            ignored,
            asked: Mutex::new(HashMap::new()),
        })
    }

    /// Toggle ghost text. Off stops the session and frees merl's index; on clears
    /// give-up state ([`Self::ghost_may_start`]) and restarts if prose is open.
    /// Documents observe the change on their next suggestion ([`words::Layered`]).
    pub(crate) fn set_ghost(self: &Arc<Self>, on: bool) {
        if self.ghost.swap(on, Ordering::Relaxed) == on {
            return;
        }
        if on {
            *locked(&self.ghost_starts) = Some(Vec::new());
            if locked(&self.docs).values().any(|(_, _, prose)| *prose) {
                let me = self.clone();
                accent_lsp::runtime().spawn(async move { me.ghost_session().await });
            }
            return;
        }
        {
            let mut generation = locked(&self.ghost_busy);
            *generation += 1;
            let _ = self.events.send(Event::Busy {
                what: "suggestions".to_string(),
                busy: false,
                message: None,
            });
        }
        let key = (GHOST.to_string(), self.root.clone());
        let running = locked(&self.sessions).remove(&key);
        if let Some(session) = running.and_then(|cell| cell.get().cloned()) {
            accent_lsp::runtime().spawn(async move { session.shutdown().await });
        }
    }

    /// Toggle prose word suggestions. Off releases the dictionary; the first
    /// request after enabling reloads it.
    pub(crate) fn set_words(&self, on: bool) {
        self.words.store(on, Ordering::Relaxed);
        if !on {
            words::forget_dictionary();
        }
    }

    /// Get or start the session under `key`. Concurrent openers share one start;
    /// failed starts are not cached, so the next opener retries.
    pub(crate) async fn session(
        self: &Arc<Self>,
        key: Key,
        start: impl Future<Output = Result<Arc<dyn Language>>>,
    ) -> Result<Arc<dyn Language>> {
        let cell = {
            let mut sessions = locked(&self.sessions);
            if self.closed.load(Ordering::SeqCst) {
                return Err(accent_core::Error::Cancelled.into());
            }
            // A server that has exited answers nothing; the next open gets a fresh one.
            if sessions
                .get(&key)
                .and_then(|cell| cell.get())
                .is_some_and(|p| p.is_dead())
            {
                sessions.remove(&key);
            }
            sessions.entry(key).or_default().clone()
        };
        let provider = cell
            .get_or_try_init(|| async {
                if self.closed.load(Ordering::SeqCst) {
                    return Err(accent_core::Error::Cancelled.into());
                }
                start.await
            })
            .await?
            .clone();
        if self.closed.load(Ordering::SeqCst) {
            let _ = provider.shutdown().await;
            return Err(accent_core::Error::Cancelled.into());
        }
        Ok(provider)
    }

    /// Start one ghost-text session on the first prose document that needs it.
    /// Key by vault root because merl indexes the whole vault, avoiding duplicate
    /// indexes for multiple projects. Pass only the three supported argv words.
    async fn ghost_session(self: &Arc<Self>) -> Option<Arc<dyn Language>> {
        let root = self.root.clone();
        let key = (GHOST.to_string(), root.clone());
        let argv = vec![
            GHOST.to_string(),
            "--vault".to_string(),
            root.to_string_lossy().into_owned(),
        ];
        let start = async {
            if !self.ghost.load(Ordering::Relaxed) {
                return Err(Error::Language("Ghost Text is off".to_string()));
            }
            if !self.ghost_may_start(Instant::now()) {
                return Err(Error::Language(format!("{GHOST} keeps exiting")));
            }
            let busy = {
                let mut generation = locked(&self.ghost_busy);
                *generation += 1;
                let _ = self.events.send(Event::Busy {
                    what: "suggestions".to_string(),
                    busy: false,
                    message: None,
                });
                external::Busy {
                    what: "suggestions",
                    generation: *generation,
                    current: self.ghost_busy.clone(),
                }
            };
            external::start(argv, root.clone(), root, self.events.clone(), Some(busy)).await
        };
        match self.session(key, start).await {
            // Switched off while it started, before `set_ghost` had a session to shut down.
            Ok(session) if !self.ghost.load(Ordering::Relaxed) => {
                let _ = session.shutdown().await;
                None
            }
            Ok(session) => Some(session),
            Err(e) => {
                tracing::debug!("no ghost text: {e:#}");
                None
            }
        }
    }

    /// Weak callback supplying a live or restarted ghost session. Return none
    /// when disabled or after give-up. Weak ownership avoids a cycle with the
    /// registry-held document provider.
    fn respawn(self: &Arc<Self>) -> words::Respawn {
        let me = Arc::downgrade(self);
        Box::new(move || {
            let me = me.upgrade();
            Box::pin(async move {
                let me = me.filter(|me| {
                    me.ghost.load(Ordering::Relaxed) && locked(&me.ghost_starts).is_some()
                })?;
                me.ghost_session().await
            })
        })
    }

    /// Allow a ghost-session start at `now` unless [`GHOST_STARTS`] within
    /// [`GHOST_WINDOW`] have exhausted the retry budget. This prevents restart
    /// loops on each typing pause; give up until reopening and report once.
    fn ghost_may_start(&self, now: Instant) -> bool {
        let mut starts = locked(&self.ghost_starts);
        let Some(recent) = starts.as_mut() else {
            return false;
        };
        recent.retain(|t| now.duration_since(*t) < GHOST_WINDOW);
        if recent.len() < GHOST_STARTS {
            recent.push(now);
            return true;
        }
        *starts = None;
        let message = format!("Ghost text stopped: {GHOST} keeps exiting");
        let _ = self.events.send(Event::Error(message));
        false
    }

    /// Who answers for an open document.
    pub(crate) fn provider(&self, rel: &str) -> Result<Arc<dyn Language>> {
        locked(&self.docs)
            .get(rel)
            .map(|(_, provider, _)| provider.clone())
            .ok_or_else(|| Error::Language(format!("{rel} is not open")))
    }

    /// Stop all providers and clear documents. Blocks on the runtime; never call
    /// from a runtime worker. Window close and `Local::drop` call this on the closing thread.
    pub(crate) fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let sessions: Vec<Session> = locked(&self.sessions)
            .drain()
            .map(|(_, cell)| cell)
            .collect();
        locked(&self.docs).clear();
        let stopping: Vec<_> = sessions
            .iter()
            .filter_map(|cell| cell.get().cloned())
            .map(|p| accent_lsp::runtime().spawn(async move { p.shutdown().await }))
            .collect();
        // No provider ever started, so there is nothing to wait for — and asking for the runtime
        // here would start one just to close a vault. That is what a phone would pay on every
        // vault it opens and closes, having no language servers at all.
        if stopping.is_empty() {
            return;
        }
        accent_lsp::runtime().block_on(async {
            for handle in stopping {
                let _ = handle.await;
            }
        });
    }

    /// Before moving files, request edits from running servers whose projects
    /// and filters match the paths. Return edits and the moves covered. Do not
    /// start servers for a rename; give each 5 s. Blocks on the runtime, so call
    /// from a rename worker or host request thread, never a runtime worker.
    pub(crate) fn will_rename(
        &self,
        moves: &[(String, String, bool)],
    ) -> (Vec<FileEdits>, Vec<String>) {
        let running: Vec<(PathBuf, Arc<dyn Language>)> = locked(&self.sessions)
            .iter()
            .filter_map(|((_, root), cell)| Some((root.clone(), cell.get()?.clone())))
            .filter(|(_, provider)| !provider.is_dead())
            .collect();
        let (mut edits, mut asked) = (Vec::new(), Vec::new());
        for (root, provider) in running {
            let mine: Vec<(String, String)> = moves
                .iter()
                .filter(|(from, _, is_dir)| {
                    Local::join(&self.root, from)
                        .is_ok_and(|abs| abs.starts_with(&root) && provider.renames(&abs, *is_dir))
                })
                .map(|(from, to, _)| (from.clone(), to.clone()))
                .collect();
            if mine.is_empty() {
                continue;
            }
            asked.extend(mine.iter().map(|(from, _)| from.clone()));
            // The timer is made inside the runtime, which is the only place it has a clock.
            let answer = accent_lsp::runtime().block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    provider.will_rename(mine),
                )
                .await
            });
            match answer {
                Ok(Ok(found)) => edits.extend(found),
                Ok(Err(e)) => tracing::warn!("asking about a rename: {e:#}"),
                Err(_) => tracing::warn!("asking about a rename: no answer within 5 s"),
            }
        }
        (edits, asked)
    }

    /// Request [`Language::calls`] for `rel`, starting its configured server if
    /// needed. Allow `waits.starting` until the first answer, then `waits.asking`.
    /// Stop after `waits.idle` without requests unless an open tab retains it.
    /// Errors explain why no answer is available.
    pub(crate) fn calls(
        self: &Arc<Self>,
        rel: String,
        pos: Pos,
        incoming: bool,
        cfg: &LspConfig,
        waits: Waits,
    ) -> Task<Option<Vec<Call>>> {
        let me = self.clone();
        let language = language_of(&rel);
        let which = language.and_then(|language| server(cfg, language));
        let markers = language.map_or(&[][..], markers);
        Task::spawn(async move {
            let (language_id, argv) = match which {
                Some(Server::External { language_id, argv }) => (language_id, argv),
                Some(Server::Missing(name)) => {
                    return Err(Error::Language(format!("{name} is not installed")));
                }
                _ => return Err(Error::Language(format!("no language server for {rel}"))),
            };
            let name = Path::new(&argv[0])
                .file_name()
                .map_or_else(|| argv[0].clone(), |n| n.to_string_lossy().into_owned());
            let asked_at = tokio::time::Instant::now();
            let root = session_root(&me.root, &Local::join(&me.root, &rel)?, markers);
            let key = (argv[0].clone(), root.clone());
            me.asked_now(&key, waits.idle);
            let start = external::start(argv, root, me.root.clone(), me.events.clone(), None);
            // A task of its own, so a call that stops waiting leaves the server starting.
            let starting = accent_lsp::runtime().spawn({
                let me = me.clone();
                async move { me.session(key, start).await }
            });
            let Ok(session) = tokio::time::timeout_at(asked_at + waits.starting, starting).await
            else {
                return Err(Error::NotYet(format!("{name} not ready")));
            };
            let session = session??;
            // Whether it said it cannot tell yet, which a wait that runs out is then put down to.
            let mut loading = !session.has_answered();
            loop {
                let wait = match session.has_answered() {
                    true => waits.asking,
                    false => waits.starting,
                };
                let deadline = asked_at + wait;
                let asked = session.calls(&rel, &language_id, pos, incoming);
                match tokio::time::timeout_at(deadline, asked).await {
                    Ok(Ok(found)) => return Ok(found),
                    Ok(Err(e)) if !matches!(e, Error::NotYet(_)) => {
                        return Err(Error::Language(format!("{name}: {e}")));
                    }
                    Ok(Err(e)) => {
                        tracing::debug!("{name} is not ready for {rel}: {e}");
                        loading = true;
                    }
                    Err(_) => {}
                }
                if tokio::time::Instant::now() + AGAIN >= deadline {
                    return Err(match loading {
                        true => Error::NotYet(format!("{name} not ready")),
                        false => Error::Language(format!(
                            "{name} did not answer within {}s",
                            wait.as_secs()
                        )),
                    });
                }
                tokio::time::sleep(AGAIN).await;
            }
        })
    }

    /// Note a question to the session under `key`, and on its first watch it for `idle`.
    fn asked_now(self: &Arc<Self>, key: &Key, idle: Duration) {
        let now = tokio::time::Instant::now();
        if locked(&self.asked).insert(key.clone(), now).is_none() {
            let me = Arc::downgrade(self);
            accent_lsp::runtime().spawn(stop_when_idle(me, key.clone(), idle));
        }
    }

    pub(crate) fn open_document(
        self: &Arc<Self>,
        rel: String,
        language: String,
        text: String,
        cfg: &VaultConfig,
    ) -> Task<Support> {
        let me = self.clone();
        let which = server(&cfg.lsp, &language);
        let configured_python = cfg.debug.python.clone();
        Task::spawn(async move {
            // Outside the vault: a dependency a running server's answer led to is that server's
            // to answer for, and nothing is started for any other file there.
            if Path::new(&rel).is_absolute() {
                let Some((key, session)) = me.naming(&rel) else {
                    return Ok(Support::default());
                };
                let language_id = match which {
                    Some(Server::External { language_id, .. }) => language_id,
                    _ => language,
                };
                let support = Support {
                    dependency: true,
                    ..session.open(&rel, &language_id, text)?
                };
                locked(&me.docs).insert(rel, (Some(key), session, false));
                return Ok(support);
            }
            let python = if language == "python3" && matches!(&which, Some(Server::External { .. }))
            {
                let root = me.root.clone();
                let command =
                    crate::debug::interpreter(&root, &root.join(&rel), &configured_python);
                let resolved =
                    tokio::task::spawn_blocking(move || crate::python::executable(&root, &command))
                        .await?;
                if configured_python.len() > 1 && resolved.is_none() {
                    return Err(Error::Language(
                        "cannot resolve the selected Python command for language analysis".into(),
                    ));
                }
                resolved
            } else {
                None
            };
            // What speaks the file's structure, if anything does: the index for a note, a
            // language server for code, nothing for a `.txt`.
            let (primary, language_id, missing, key): Opened = match which {
                Some(Server::Notes) => {
                    // One notes provider per vault, whatever the note: they share the index.
                    let key = ("accent".to_string(), me.root.clone());
                    let (root, db, events) = (me.root.clone(), me.db.clone(), me.events.clone());
                    let ignored = me.ignored.clone();
                    let start = async move {
                        Ok(Arc::new(Notes::open_at(root, &db, events, ignored)?) as _)
                    };
                    (
                        Some(me.session(key.clone(), start).await?),
                        "markdown".to_string(),
                        None,
                        Some(key),
                    )
                }
                Some(Server::External { language_id, argv }) => {
                    // One session per (server, project): two crates in one vault get one
                    // server each, and two files in one crate share it.
                    let (root, events) = (me.root.clone(), me.events.clone());
                    let abs = Local::join(&root, &rel)?;
                    let session_root = session_root(&root, &abs, markers(&language));
                    let key = (
                        match &python {
                            Some(python) => format!("{}|{python}", argv[0]),
                            None => argv[0].clone(),
                        },
                        session_root.clone(),
                    );
                    let start =
                        external::start_configured(argv, session_root, root, events, None, python);
                    (
                        Some(me.session(key.clone(), start).await?),
                        language_id,
                        None,
                        Some(key),
                    )
                }
                Some(Server::Missing(name)) => {
                    tracing::debug!("no language server for {language}: {name} is not installed");
                    (None, language.clone(), Some(name), None)
                }
                None => (None, language.clone(), None, None),
            };
            let prose = words::PROSE.contains(&language.as_str());
            // Ghost text rides beside the primary for prose: one merl for the whole vault,
            // started when it is installed and wanted. A binary that is not there is a debug
            // line and never a `missing`, because this is optional where a language server is
            // expected: nothing about the tab stops working without it.
            let respawn = (prose && in_path(GHOST)).then(|| me.respawn());
            let ghost = match respawn.is_some() && me.ghost.load(Ordering::Relaxed) {
                true => me.ghost_session().await,
                false => None,
            };
            // Prose gets its words layered under whatever the primary answers, and gets them
            // even with no primary at all.
            let provider: Arc<dyn Language> = match (primary, prose) {
                (Some(primary), false) => primary,
                (primary, true) => {
                    let listing = (language == "latex").then(|| me.listing.clone());
                    Arc::new(words::Layered::new(
                        primary,
                        ghost,
                        respawn,
                        me.words.clone(),
                        listing,
                    ))
                }
                (None, false) => {
                    return Ok(Support {
                        missing,
                        ..Support::default()
                    });
                }
            };
            let mut support = provider.open(&rel, &language_id, text)?;
            support.missing = support.missing.or(missing);
            locked(&me.docs).insert(rel, (key, provider, prose));
            Ok(support)
        })
    }

    /// The running session whose answers named `path`, a file outside the vault, under its key.
    fn naming(&self, path: &str) -> Option<(Key, Arc<dyn Language>)> {
        locked(&self.sessions).iter().find_map(|(key, cell)| {
            let session = cell.get().filter(|s| !s.is_dead() && s.named(path))?;
            Some((key.clone(), session.clone()))
        })
    }

    pub(crate) fn named(&self, path: &str) -> bool {
        self.naming(path).is_some()
    }

    /// Close the document and stop its session if no documents remain. Reopening
    /// pays a cold start rather than retaining an unused server. The vault-wide
    /// ghost session is separate and ends with the vault or Ghost Text preference.
    pub(crate) fn close_document(&self, rel: String) -> Task<()> {
        let (key, provider) = match locked(&self.docs).remove(&rel) {
            Some((key, provider, _)) => (key, Some(provider)),
            None => (None, None),
        };
        let idle = key.filter(|key| {
            !locked(&self.docs)
                .values()
                .any(|(open, _, _)| open.as_ref() == Some(key))
        });
        let session = idle.and_then(|key| locked(&self.sessions).remove(&key));
        Task::spawn(async move {
            if let Some(provider) = provider {
                provider.close(&rel);
            }
            // After the close, so the server hears `didClose` before it is asked to exit.
            if let Some(session) = session.as_ref().and_then(|cell| cell.get()) {
                let _ = session.shutdown().await;
            }
            Ok(())
        })
    }
}

/// One vault-wide ghost-text process serving `textDocument/inlineCompletion`
/// from notes. Separate from `SERVERS` so it supplements a language provider.
const GHOST: &str = "merl-rt";

/// How long a server still loading its project is left before it is asked again.
const AGAIN: Duration = Duration::from_millis(250);

/// Timeouts for [`Vault::calls`]: `starting` before the first answer, `asking`
/// afterwards, and `idle` for sessions started by requests. Idle time starts
/// with the request and must exceed the request waits.
#[derive(Debug, Clone, Copy)]
pub struct Waits {
    pub starting: Duration,
    pub asking: Duration,
    pub idle: Duration,
}

/// Stop the session under `key` once it has gone `idle` without a question to
/// [`Languages::calls`], unless a tab's document holds it, whose close stops it then.
async fn stop_when_idle(me: std::sync::Weak<Languages>, key: Key, idle: Duration) {
    loop {
        let Some(last) = me
            .upgrade()
            .and_then(|me| locked(&me.asked).get(&key).copied())
        else {
            return;
        };
        tokio::time::sleep_until(last + idle).await;
        let Some(me) = me.upgrade() else { return };
        // Under the one lock a question takes first, so none is asked of a session going away.
        let session = {
            let mut asked = locked(&me.asked);
            if asked.get(&key).is_some_and(|t| t.elapsed() < idle) {
                continue;
            }
            asked.remove(&key);
            let held = locked(&me.docs)
                .values()
                .any(|(open, _, _)| open.as_ref() == Some(&key));
            match held {
                true => None,
                false => locked(&me.sessions).remove(&key),
            }
        };
        if let Some(session) = session.as_ref().and_then(|cell| cell.get()) {
            tracing::debug!("stopping {} after {idle:?} without a question", key.0);
            let _ = session.shutdown().await;
        }
        return;
    }
}

/// Starts of the ghost session a vault allows within [`GHOST_WINDOW`]: the first, and two more
/// after it exits.
const GHOST_STARTS: usize = 3;
const GHOST_WINDOW: Duration = Duration::from_secs(60);

/// What answers for a language.
pub(crate) enum Server {
    /// The index, for a note.
    Notes,
    /// A language server to run, named by the command line that starts it.
    External {
        language_id: String,
        argv: Vec<String>,
    },
    /// A language accent knows a server for, which is not installed. The name is what to install.
    Missing(String),
}

/// Built-in server choices: GtkSourceView id, protocol id and commands in
/// preference order. Keep the list to tested languages; configure others
/// in the vault instead of guessing defaults.
const SERVERS: &[(&str, &str, &[&[&str]])] = &[
    ("rust", "rust", &[&["rust-analyzer"]]),
    ("c", "c", &[&["clangd"]]),
    ("cpp", "cpp", &[&["clangd"]]),
    // A header is its language's, as GtkSourceView tells a `.h` from a `.hpp`; clangd reads the
    // dialect off the compile command whatever the `languageId` says.
    ("chdr", "c", &[&["clangd"]]),
    ("cpphdr", "cpp", &[&["clangd"]]),
    ("python", "python", PYTHON),
    ("python3", "python", PYTHON),
    ("js", "javascript", TYPESCRIPT),
    ("jsx", "javascriptreact", TYPESCRIPT),
    ("typescript", "typescript", TYPESCRIPT),
    ("typescript-jsx", "typescriptreact", TYPESCRIPT),
    ("latex", "latex", &[&["texlab"]]),
    ("toml", "toml", &[&["taplo", "lsp", "stdio"]]),
    ("go", "go", &[&["gopls"]]),
    // elan installs `lake` under `~/.elan/bin`, which a desktop session's PATH does not hold.
    (
        "lean",
        "lean4",
        &[&["lake", "serve"], &["~/.elan/bin/lake", "serve"]],
    ),
];

/// The files that mark a project's root for a language's server, besides `.git`
/// ([`session_root`]): a Lean project's lakefile often sits below the checkout holding it.
const ROOTS: &[(&str, &[&str])] = &[(
    "lean",
    &["lakefile.toml", "lakefile.lean", "lean-toolchain"],
)];

/// The root markers of a GtkSourceView language id, none for most.
fn markers(language: &str) -> &'static [&'static str] {
    ROOTS
        .iter()
        .find(|(id, _)| *id == language)
        .map_or(&[], |(_, markers)| markers)
}

const PYTHON: &[&[&str]] = &[&["pyright-langserver", "--stdio"], &["pylsp"]];
const TYPESCRIPT: &[&[&str]] = &[&["typescript-language-server", "--stdio"]];

/// Which provider a GtkSourceView language id gets. A configured command line wins over the
/// built-in choices, which is how a vault picks `pylsp` over `pyright`.
pub(crate) fn server(cfg: &LspConfig, language: &str) -> Option<Server> {
    let row = SERVERS.iter().find(|(id, ..)| *id == language);
    let configured = cfg.servers.get(language).filter(|argv| !argv.is_empty());
    if configured.is_none() && language == "markdown" {
        return Some(Server::Notes);
    }
    // A configured command line replaces the built-in alternatives rather than joining them.
    let mut alternatives: Vec<Vec<String>> = match configured {
        Some(argv) => vec![argv.clone()],
        None => row?
            .2
            .iter()
            .map(|argv| argv.iter().map(|&a| a.to_string()).collect())
            .collect(),
    };
    // Read where the server runs, so a remote vault's `~/` is the host's home.
    if let Some(home) = std::env::var_os("HOME") {
        for argv in &mut alternatives {
            argv[0] = home_relative(&argv[0], Path::new(&home))
                .to_string_lossy()
                .into_owned();
        }
    }
    // The protocol's name for the language, which is not always GtkSourceView's (`js`).
    let language_id = row.map_or(language, |(_, id, _)| id).to_string();
    match alternatives.iter().find(|argv| in_path(&argv[0])) {
        Some(argv) => Some(Server::External {
            language_id,
            argv: argv.clone(),
        }),
        // Known but not installed: the first choice is the one worth naming to the user.
        None => Some(Server::Missing(alternatives.into_iter().next()?.remove(0))),
    }
}

/// Server program name for a status line (for example, "lake"), from config
/// without consulting the local PATH, which may differ from the host's.
/// `None` for notes, unsupported languages or ambiguous built-in choices
/// such as Python's pyright/pylsp.
fn server_name(cfg: &LspConfig, language: &str) -> Option<String> {
    let name = |program: &str| {
        Path::new(program)
            .file_name()
            .map_or_else(|| program.to_string(), |n| n.to_string_lossy().into_owned())
    };
    let mut names: Vec<String> = match cfg.servers.get(language).filter(|argv| !argv.is_empty()) {
        Some(argv) => vec![name(&argv[0])],
        None => SERVERS
            .iter()
            .find(|(id, ..)| *id == language)?
            .2
            .iter()
            .map(|argv| name(argv[0]))
            .collect(),
    };
    names.dedup();
    match names.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// The GtkSourceView language id the app gives a code file, which a server is chosen by: the
/// languages the index reads declarations in.
fn language_of(rel: &str) -> Option<&'static str> {
    let ext = rel.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python3",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescript-jsx",
        "go" => "go",
        "c" => "c",
        "h" => "chdr",
        "cc" | "cpp" | "cxx" | "c++" => "cpp",
        "hh" | "hpp" | "h++" => "cpphdr",
        "kt" | "kts" => "kotlin",
        "java" => "java",
        _ => return None,
    })
}

/// A command named from the home directory (`~/…`) as the path it names there.
fn home_relative(arg: &str, home: &Path) -> PathBuf {
    match arg.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(arg),
    }
}

/// Whether an executable can be started: an absolute name is the file itself, a bare one is
/// looked for the way a shell would.
pub(crate) fn in_path(exe: &str) -> bool {
    let path = Path::new(exe);
    if path.is_absolute() {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(exe).is_file()))
}

/// Choose the nearest checkout or language root marker inside the vault,
/// falling back to the vault root. This gives each project its own server.
/// Test `.git` existence because worktrees use a file.
/// For files under `.lake`, search from outside the outermost `.lake` so
/// dependency packages use the project's server rather than their own roots.
pub(crate) fn session_root(vault_root: &Path, abs_file: &Path, markers: &[&str]) -> PathBuf {
    let lake = abs_file
        .ancestors()
        .filter(|d| d.starts_with(vault_root) && d.ends_with(".lake"))
        .last();
    let mut dir = lake.unwrap_or(abs_file).parent();
    while let Some(here) = dir.filter(|d| d.starts_with(vault_root)) {
        if here.join(".git").exists() || markers.iter().any(|m| here.join(m).exists()) {
            return here.to_path_buf();
        }
        dir = here.parent();
    }
    vault_root.to_path_buf()
}

// ------------------------------------------------------------------ the façade

/// Document operations as [`Task`]s, awaited by the UI and dropped when stale.
/// Remote requests execute beside the files and servers; their round trips
/// run on the runtime's blocking pool.
impl Vault {
    /// Open `rel` and return provider capabilities before any other operation.
    /// Defined separately because it reads the vault's language-server config.
    pub fn open_document(&self, rel: &str, language_id: &str, text: String) -> Task<Support> {
        match &self.backend {
            Backend::Local(v) => v.open_document(rel, language_id, text),
            // The host may be starting the file's server, which has `STARTING` to answer.
            Backend::Remote(r) => remote_task_then(
                r.clone(),
                "open_document",
                json!([rel, language_id, text]),
                accent_lsp::STARTING + crate::rpc::DEADLINE,
                |_, answer| Ok(answer),
            ),
        }
    }

    /// The language server a document in `language` waits on while it opens, by the name of
    /// its program ("lake"), where the config says which one that is.
    pub fn server_name(&self, language: &str) -> Option<String> {
        server_name(&self.config().lsp, language)
    }

    /// Close the tab and remove its document from the registry, beyond notifying
    /// the provider.
    pub fn close_document(&self, rel: &str) -> Task<()> {
        match &self.backend {
            Backend::Local(v) => v.close_document(rel),
            Backend::Remote(r) => remote_task(r.clone(), "close_document", json!([rel])),
        }
    }

    /// Incoming or outgoing calls for the declaration named at `pos` in `rel`.
    /// No open tab is required: the configured server can read from disk.
    /// `None` means no declaration. `waits` controls startup, request and idle waits.
    /// Local vaults only, for `accent-cli mcp` running beside the files.
    pub fn calls(
        &self,
        rel: &str,
        pos: Pos,
        incoming: bool,
        waits: Waits,
    ) -> Task<Option<Vec<Call>>> {
        match &self.backend {
            Backend::Local(v) => {
                v.lang
                    .calls(rel.to_string(), pos, incoming, &v.config().lsp, waits)
            }
            Backend::Remote(_) => {
                Task::spawn(async { Err(Error::Language("not on a remote vault".to_string())) })
            }
        }
    }
}

/// Generate document operations for [`Languages`], [`Vault`] and host RPC.
/// Rows map `façade => trait method`: requests await a future, with `then f`
/// finishing host answers locally; notifications finish when the provider
/// accepts them. Providers run on the shared runtime beside the files.
macro_rules! requests {
    (@remote $r:ident $name:ident $params:expr) => {
        remote_task($r.clone(), stringify!($name), $params)
    };
    (@remote $r:ident $name:ident $params:expr, $then:path) => {
        remote_task_then($r.clone(), stringify!($name), $params, crate::rpc::DEADLINE, $then)
    };
    (@ret) => { () };
    (@ret $ret:ty) => { $ret };
    (@answer $call:expr;) => { $call };
    (@answer $call:expr; $ret:ty) => { $call.await };
    ($(
        $(#[$doc:meta])*
        $name:ident => $inner:ident ( $($arg:ident : $ty:ty),* ) $(-> $ret:ty)?
            $(, then $then:path)?;
    )*) => {
        impl Languages { $(
            pub(crate) fn $name(&self, rel: String, $($arg: $ty),*)
                -> Task<requests!(@ret $($ret)?)>
            {
                let provider = self.provider(&rel);
                Task::spawn(async move {
                    requests!(@answer provider?.$inner(&rel, $($arg),*); $($ret)?)
                })
            }
        )* }

        impl Vault { $(
            $(#[$doc])*
            pub fn $name(&self, rel: &str, $($arg: $ty),*) -> Task<requests!(@ret $($ret)?)> {
                match &self.backend {
                    Backend::Local(v) => v.$name(rel, $($arg),*),
                    Backend::Remote(r) => {
                        requests!(@remote r $name json!([rel, $($arg),*]) $(, $then)?)
                    }
                }
            }
        )* }

        impl Local { $(
            pub fn $name(&self, rel: &str, $($arg: $ty),*) -> Task<requests!(@ret $($ret)?)> {
                self.lang.$name(rel.to_string(), $($arg),*)
            }
        )* }

        /// Dispatch these operations on host request threads in
        /// [`crate::rpc::serve_local`], allowing a single, non-nested runtime wait.
        pub(crate) fn dispatch(
            vault: &Local,
            method: &str,
            p: &Value,
        ) -> Option<Result<Value>> {
            $( if method == stringify!($name) {
                crate::rpc::args!(p; rel: String $(, $arg: $ty)*);
                return Some(crate::rpc::answer(crate::rpc::block(
                    vault.$name(&rel, $($arg),*),
                )));
            } )*
            None
        }
    };
}

requests! {
    /// On a remote vault the host answers all but a PDF's bookmarks, which are read here.
    completion => completion(pos: Pos, trigger: Option<char>) -> Completions, then list_pages;
    /// Fill in what the popup left out until a row was looked at.
    resolve_completion => resolve(item: Completion) -> Completion;
    signature_help => signature_help(pos: Pos) -> Option<Signature>;
    hover => hover(pos: Pos) -> Option<Hover>;
    definition => definition(pos: Pos) -> Vec<Location>;
    symbols => symbols() -> Vec<Symbol>;
    references => references(pos: Pos) -> Vec<Location>;
    folds => folds() -> Vec<Fold>;
    /// The proof goals at a position, from a proof assistant's server ([`Support::goals`]).
    goals => goals(pos: Pos) -> Option<Goals>;
    /// Inline ghost text, or none. Queried on typing pauses, so dropping stale
    /// tasks is the normal cancellation path.
    inline_completion => inline_completion(pos: Pos) -> Option<String>;

    // What the editor did to the document: nothing to answer, and nothing to await inside.
    change_document => change(text: String);
    /// The buffer was written; a server that checks on save is told.
    save_document => saved();
    /// Notify that the user left the document. The ghost provider rereads the
    /// vault here rather than on every autosave.
    settle => settle();
    /// Republish hints after index changes, such as a dangling link's target
    /// being created without an edit to the source note.
    rediagnose => rediagnose();
}

/// Wrap a remote request as a task. The round trip blocks, so dropping sends
/// `cancel` to the host instead of relying on thread abort. This cancels stale
/// server work and releases the waiting thread without waiting for an answer.
fn remote_task<T: DeserializeOwned + Send + 'static>(
    r: Arc<Remote>,
    method: &'static str,
    params: Value,
) -> Task<T> {
    remote_task_then(r, method, params, crate::rpc::DEADLINE, |_, answer| {
        Ok(answer)
    })
}

/// [`remote_task`], waiting up to `deadline`, with the answer finished by `then` on the same
/// blocking thread.
pub(crate) fn remote_task_then<T: DeserializeOwned + Send + 'static>(
    r: Arc<Remote>,
    method: &'static str,
    params: Value,
    deadline: Duration,
    then: fn(&Remote, T) -> Result<T>,
) -> Task<T> {
    let asked: Arc<crate::remote::Asked> = Arc::default();
    Task::blocking({
        let (r, asked) = (r.clone(), asked.clone());
        move || {
            let answer = r.call_tracked(method, params, &asked, deadline)?;
            then(&r, answer)
        }
    })
    .cancelled_by(move || r.cancel(&asked))
}

/// Complete deferred `[[paper.pdf#` bookmarks from the locally fetched PDF;
/// host `serve` has no PDF reader. Remote bytes travel as files, never RPC data.
fn list_pages(r: &Remote, mut answer: Completions) -> Result<Completions> {
    match answer.pages.take() {
        Some(pages) => {
            let outline = notes::pdf_outline(&r.fetch(&pages.rel)?)?;
            Ok(pages.answer(outline))
        }
        None => Ok(answer),
    }
}

/// The two lifecycle calls the macros do not write, where the vault is.
impl Local {
    pub fn open_document(&self, rel: &str, language_id: &str, text: String) -> Task<Support> {
        self.lang.open_document(
            rel.to_string(),
            language_id.to_string(),
            text,
            &self.config(),
        )
    }

    pub fn close_document(&self, rel: &str) -> Task<()> {
        self.lang.close_document(rel.to_string())
    }
}

/// Compute headings and folds from note text for documents outside a vault,
/// where no notes provider is available.
pub fn note_outline(text: &str) -> (Vec<Symbol>, Vec<Fold>) {
    let analysis = markdown::analyze(text);
    (
        notes::symbols_of(text, &analysis.headings),
        notes::folds_of(text, &analysis),
    )
}

/// Convert a byte offset to a position in `text`, in linear time.
pub fn pos_of(text: &str, byte: usize) -> Pos {
    let byte = byte.min(text.len());
    let head = &text[..byte];
    let line = head.matches('\n').count() as u32;
    let line_start = head.rfind('\n').map_or(0, |i| i + 1);
    Pos {
        line,
        character: head[line_start..].chars().count() as u32,
    }
}

/// Position → byte offset in `text`; `None` past the last line. A column past the line's end
/// lands on its end.
pub fn byte_of(text: &str, pos: Pos) -> Option<usize> {
    let mut start = 0;
    for _ in 0..pos.line {
        start = text[start..].find('\n').map(|i| start + i + 1)?;
    }
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let line = &text[start..line_end];
    let off = line
        .char_indices()
        .nth(pos.character as usize)
        .map_or(line.len(), |(i, _)| i);
    Some(start + off)
}

pub fn range_of(text: &str, r: &std::ops::Range<usize>) -> Range {
    Range {
        start: pos_of(text, r.start),
        end: pos_of(text, r.end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A note outside every vault has no provider to ask, and gets from its text what the notes
    /// provider would answer: its headings nested, and the folds of their sections.
    #[test]
    fn a_note_is_outlined_from_its_text_alone() {
        let (symbols, folds) = note_outline("# One\ntext\n## Two\n");
        assert_eq!(symbols[0].name, "One");
        assert_eq!(symbols[0].children[0].name, "Two");
        assert_eq!(
            folds,
            [Fold {
                start_line: 0,
                end_line: 2
            }]
        );
    }

    #[test]
    fn positions_count_chars_not_bytes() {
        let text = "héllo\n[[wörld]]";
        assert_eq!(
            pos_of(text, 7),
            Pos {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            pos_of(text, 17),
            Pos {
                line: 1,
                character: 9
            }
        );
        assert_eq!(
            byte_of(
                text,
                Pos {
                    line: 1,
                    character: 9
                }
            ),
            Some(17)
        );
        assert_eq!(
            byte_of(
                text,
                Pos {
                    line: 0,
                    character: 99
                }
            ),
            Some(6)
        );
        assert_eq!(
            byte_of(
                text,
                Pos {
                    line: 5,
                    character: 0
                }
            ),
            None
        );
    }

    /// A vault whose config names `argv` for `language`.
    fn configured(language: &str, argv: &[&str]) -> LspConfig {
        LspConfig {
            servers: [(
                language.to_string(),
                argv.iter().map(|a| a.to_string()).collect(),
            )]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn a_language_gets_the_server_it_has() {
        // Something certain to be executable on any machine running this test.
        let me = std::env::current_exe().unwrap();
        let me = me.to_str().unwrap();

        let Some(Server::External { language_id, argv }) =
            server(&configured("rust", &[me, "--stdio"]), "rust")
        else {
            panic!("a configured command line that exists is the server to run")
        };
        assert_eq!(language_id, "rust", "the protocol's name for the language");
        assert_eq!(argv, [me, "--stdio"]);

        // A language the table has no row for takes its own id as the protocol's.
        let Some(Server::External { language_id, .. }) = server(&configured("nim", &[me]), "nim")
        else {
            panic!("a configured server answers for any language")
        };
        assert_eq!(language_id, "nim");

        for (header, id) in [("chdr", "c"), ("cpphdr", "cpp")] {
            let Some(Server::External { language_id, .. }) =
                server(&configured(header, &[me]), header)
            else {
                panic!("{header} has a server")
            };
            assert_eq!(language_id, id, "a header is its language's");
        }
        // JSX is its own language to the protocol, and typescript-language-server's to answer.
        for (jsx, id) in [
            ("typescript-jsx", "typescriptreact"),
            ("jsx", "javascriptreact"),
        ] {
            let Some(Server::External { language_id, .. }) = server(&configured(jsx, &[me]), jsx)
            else {
                panic!("{jsx} has a server")
            };
            assert_eq!(language_id, id);
            assert_eq!(
                server_name(&LspConfig::default(), jsx).as_deref(),
                Some("typescript-language-server")
            );
        }

        assert!(
            matches!(server(&configured("rust", &["/no/such/server"]), "rust"),
                Some(Server::Missing(name)) if name == "/no/such/server"),
            "a named server that is not there is what the UI reports"
        );
        assert!(matches!(
            server(&LspConfig::default(), "markdown"),
            Some(Server::Notes)
        ));
        assert!(server(&LspConfig::default(), "brainfuck").is_none());
    }

    /// What a status line calls the server a document waits on.
    #[test]
    fn a_server_is_named_by_its_program() {
        let none = LspConfig::default();
        assert_eq!(server_name(&none, "lean").as_deref(), Some("lake"));
        assert_eq!(server_name(&none, "python3"), None, "pyright or pylsp");
        assert_eq!(server_name(&none, "markdown"), None);
        let mine = configured("python3", &["/opt/bin/pylsp", "-v"]);
        assert_eq!(server_name(&mine, "python3").as_deref(), Some("pylsp"));
    }

    #[test]
    fn a_command_under_home_is_looked_for_there() {
        let home = Path::new("/home/u");
        assert_eq!(
            home_relative("~/.elan/bin/lake", home),
            Path::new("/home/u/.elan/bin/lake")
        );
        assert_eq!(home_relative("lake", home), Path::new("lake"));
    }

    #[test]
    fn a_session_is_rooted_at_the_nearest_checkout() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let crate_dir = root.join("proj/src");
        std::fs::create_dir_all(&crate_dir).unwrap();
        assert_eq!(
            session_root(root, &crate_dir.join("main.rs"), &[]),
            root,
            "no checkout anywhere: the vault is the project"
        );

        // A worktree's `.git` is a file, and it counts the same as a directory.
        std::fs::write(root.join("proj/.git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            session_root(root, &crate_dir.join("main.rs"), &[]),
            root.join("proj")
        );
        assert_eq!(
            session_root(root, &root.join("loose.rs"), &[]),
            root,
            "a file beside the checkout is not inside it"
        );

        // A Lean project below the checkout is rooted at its lakefile, and a dependency's file
        // under its `.lake`, a checkout with a lakefile of its own, at the project.
        let lean = root.join("proj/lean");
        let mathlib = lean.join(".lake/packages/mathlib");
        std::fs::create_dir_all(mathlib.join("Mathlib")).unwrap();
        std::fs::create_dir(mathlib.join(".git")).unwrap();
        std::fs::write(lean.join("lakefile.toml"), "").unwrap();
        std::fs::write(mathlib.join("lakefile.lean"), "").unwrap();
        let markers = ROOTS[0].1;
        assert_eq!(session_root(root, &lean.join("A.lean"), markers), lean);
        assert_eq!(
            session_root(root, &mathlib.join("Mathlib/X.lean"), markers),
            lean
        );
    }

    /// The notes provider needs no external binary, so it verifies that a session
    /// lives exactly while it holds an open document.
    #[test]
    fn the_last_document_to_close_takes_its_session_with_it() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.md"), "# A\n").unwrap();
        std::fs::write(root.path().join("b.md"), "# B\n").unwrap();
        let (vault, _events) = Local::open_at(
            root.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();
        // The ghost session is the vault's, not a document's, and it is not counted here.
        vault.set_ghost(false);

        accent_lsp::runtime().block_on(async {
            for rel in ["a.md", "b.md"] {
                vault
                    .open_document(rel, "markdown", "# note\n".to_string())
                    .await
                    .unwrap();
            }
            assert_eq!(locked(&vault.lang.sessions).len(), 1, "one notes provider");

            vault.close_document("a.md").await.unwrap();
            assert_eq!(
                locked(&vault.lang.sessions).len(),
                1,
                "b.md still has the session open"
            );
            vault.close_document("b.md").await.unwrap();
            assert!(
                locked(&vault.lang.sessions).is_empty(),
                "nothing is open, so nothing is kept"
            );
        });
    }

    /// A server whose definition answer named `/dep/lib.rs`, as rust-analyzer's names a crate
    /// in `~/.cargo/registry`, and answers nothing else.
    struct Naming {
        stopped: Option<Arc<AtomicBool>>,
        named: String,
    }

    impl Language for Naming {
        fn open(&self, _: &str, _: &str, _: String) -> Result<Support> {
            Ok(Support::default())
        }
        fn change(&self, _: &str, _: String) -> Result<()> {
            Ok(())
        }
        fn close(&self, _: &str) {}
        fn completion(&self, _: &str, _: Pos, _: Option<char>) -> Fut<'_, Completions> {
            Box::pin(async { Ok(Completions::default()) })
        }
        fn resolve(&self, _: &str, item: Completion) -> Fut<'_, Completion> {
            Box::pin(async { Ok(item) })
        }
        fn signature_help(&self, _: &str, _: Pos) -> Fut<'_, Option<Signature>> {
            Box::pin(async { Ok(None) })
        }
        fn hover(&self, _: &str, _: Pos) -> Fut<'_, Option<Hover>> {
            Box::pin(async { Ok(None) })
        }
        fn definition(&self, _: &str, _: Pos) -> Fut<'_, Vec<Location>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn symbols(&self, _: &str) -> Fut<'_, Vec<Symbol>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn references(&self, _: &str, _: Pos) -> Fut<'_, Vec<Location>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn folds(&self, _: &str) -> Fut<'_, Vec<Fold>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn named(&self, path: &str) -> bool {
            path == self.named
        }
        fn shutdown(&self) -> Fut<'_, ()> {
            Box::pin(async move {
                if let Some(stopped) = &self.stopped {
                    stopped.store(true, Ordering::SeqCst);
                }
                Ok(())
            })
        }
    }

    #[test]
    fn a_session_starting_during_shutdown_is_stopped() {
        let (events, _said) = std::sync::mpsc::channel();
        let langs = Languages::new("/vault".into(), "/index.db".into(), events);
        let stopped = Arc::new(AtomicBool::new(false));
        let (began, seen) = std::sync::mpsc::channel();
        let (resume, waiting) = tokio::sync::oneshot::channel::<()>();
        let starting = accent_lsp::runtime().spawn({
            let (langs, stopped) = (langs.clone(), stopped.clone());
            async move {
                langs
                    .session(("server".to_string(), "/vault".into()), async move {
                        began.send(()).unwrap();
                        waiting.await.unwrap();
                        Ok(Arc::new(Naming {
                            stopped: Some(stopped),
                            named: "/dep/lib.rs".into(),
                        }) as Arc<dyn Language>)
                    })
                    .await
            }
        });
        seen.recv_timeout(crate::tests::BUDGET).unwrap();
        langs.shutdown();
        resume.send(()).unwrap();
        let started = accent_lsp::runtime().block_on(async { starting.await.unwrap() });

        assert!(started.is_err(), "a closed vault accepted a new provider");
        assert!(stopped.load(Ordering::SeqCst));
        assert!(locked(&langs.sessions).is_empty());
    }

    /// A file outside the vault joins the session that named it, holds it while open and starts
    /// nothing; one no session named gets no provider.
    #[test]
    fn a_dependency_joins_the_session_that_named_it() {
        let (events, _said) = std::sync::mpsc::channel();
        let langs = Languages::new("/vault".into(), "/index.db".into(), events);
        let key = ("rust-analyzer".to_string(), PathBuf::from("/vault"));
        let running = tokio::sync::OnceCell::new_with(Some(Arc::new(Naming {
            stopped: None,
            named: "/dep/lib.rs".into(),
        }) as Arc<dyn Language>));
        locked(&langs.sessions).insert(key.clone(), Arc::new(running));
        let open = |rel: &str| {
            let cfg = VaultConfig::default();
            langs.open_document(rel.into(), "rust".into(), String::new(), &cfg)
        };
        accent_lsp::runtime().block_on(async {
            assert!(open("/dep/lib.rs").await.unwrap().dependency);
            assert!(langs.provider("/dep/lib.rs").is_ok());
            assert_eq!(locked(&langs.docs)["/dep/lib.rs"].0, Some(key));

            assert!(!open("/elsewhere/main.rs").await.unwrap().dependency);
            assert!(langs.provider("/elsewhere/main.rs").is_err());
            assert_eq!(locked(&langs.sessions).len(), 1, "nothing was started");

            langs.close_document("/dep/lib.rs".into()).await.unwrap();
            assert!(
                locked(&langs.sessions).is_empty(),
                "the dependency was the last document holding the session"
            );
        });
    }

    #[test]
    fn a_host_read_requires_an_absolute_file_named_by_a_running_session() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let dep = cache.path().join("lib.rs");
        std::fs::write(&dep, "pub fn helper() {}\n").unwrap();
        let (vault, _events) = Local::open_at(
            root.path(),
            &cache.path().join("index.db"),
            VaultConfig::default(),
        )
        .unwrap();
        let key = ("rust-analyzer".to_string(), root.path().to_path_buf());
        let running = tokio::sync::OnceCell::new_with(Some(Arc::new(Naming {
            stopped: None,
            named: dep.to_str().unwrap().into(),
        }) as Arc<dyn Language>));
        locked(&vault.lang.sessions).insert(key, Arc::new(running));

        let read = vault.read_named(dep.to_str().unwrap()).unwrap();
        assert!(
            matches!(read, accent_core::fs::Read::Text(text) if text.text == "pub fn helper() {}\n")
        );
        let wire = crate::vault::dispatch(&vault, "read_named", &json!([dep.to_str().unwrap()]))
            .unwrap()
            .unwrap();
        let read: accent_core::fs::Read = serde_json::from_value(wire).unwrap();
        assert!(
            matches!(read, accent_core::fs::Read::Text(text) if text.text == "pub fn helper() {}\n")
        );
        assert!(
            vault.read_named("lib.rs").is_err(),
            "relative paths are not host files"
        );
        assert!(
            vault.read_named(root.path().to_str().unwrap()).is_err(),
            "the session did not name this path"
        );
        locked(&vault.lang.sessions).clear();
        assert!(
            vault.read_named(dep.to_str().unwrap()).is_err(),
            "a closed session no longer authorizes a host read"
        );
    }

    /// Verify longer startup waits until the server first answers, then stop it
    /// after `idle` without requests. The fake server models project loading.
    #[test]
    fn a_server_started_for_calls_is_waited_for_then_stopped_when_idle() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("lib.rs"),
            "fn helper() {}\n\nfn main() {\n    helper();\n}\n",
        )
        .unwrap();
        let calls = cache.path().join("calls.json");
        let called = r#"{"lib.rs:0": {"incoming": [["lib.rs", 2, "main", [3]]]}}"#;
        std::fs::write(&calls, called).unwrap();
        let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/../../build-aux/fake-lsp.py");
        let calls = calls.to_str().unwrap();
        let argv = ["python3", fake, "--calls", calls, "--loading", "4"];
        let cfg = crate::VaultConfig {
            lsp: configured("rust", &argv),
            ..Default::default()
        };
        let (vault, _events) =
            Local::open_at(root.path(), &cache.path().join("index.db"), cfg).unwrap();
        let waits = Waits {
            starting: Duration::from_secs(5),
            asking: Duration::from_millis(500),
            idle: Duration::from_secs(2),
        };

        // Refused four times, a second at `AGAIN` apiece.
        let at = Pos {
            line: 0,
            character: 3,
        };
        let asked = vault
            .lang
            .calls("lib.rs".to_string(), at, true, &vault.config().lsp, waits);
        let found = accent_lsp::runtime().block_on(asked).unwrap().unwrap();
        assert_eq!(
            (found[0].name.as_str(), found[0].lines.as_slice()),
            ("main", &[3][..])
        );
        let session = locked(&vault.lang.sessions)
            .values()
            .find_map(|cell| cell.get().cloned())
            .expect("the server is kept");

        std::thread::sleep(Duration::from_millis(2_500));
        assert!(locked(&vault.lang.sessions).is_empty(), "idle, so stopped");
        let stopped = Instant::now() + Duration::from_secs(3);
        while !session.is_dead() && Instant::now() < stopped {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(session.is_dead(), "the server exited");
    }

    /// A note created after a link to it was flagged: the hint goes when the walk that found it
    /// reports, not at the next keystroke.
    #[test]
    fn an_indexed_note_clears_the_link_that_was_waiting_for_it() {
        let indexed = |e: &Event| matches!(e, Event::DirsChanged(_) | Event::Reconciled(_));
        let f = crate::tests::Fixture::open(crate::VaultConfig::default());
        f.write("a.md", "[[Beta]]\n");
        // Drained here, so that the wait for Beta's walk below cannot match this one's.
        assert!(f.wait(indexed).is_some(), "a.md reaches the index");
        let rt = accent_lsp::runtime();
        rt.block_on(async {
            f.vault
                .open_document("a.md", "markdown", "[[Beta]]\n".to_string())
                .await
                .unwrap();
        });
        assert!(
            f.wait(|e| matches!(e, Event::Diagnostics { rel, items } if rel == "a.md" && items.len() == 1))
                .is_some(),
            "the link is dangling while Beta does not exist"
        );

        f.write("Beta.md", "# Beta\n");
        assert!(f.wait(indexed).is_some(), "the new note reaches the index");
        rt.block_on(async { f.vault.rediagnose("a.md").await.unwrap() });
        assert!(
            f.wait(|e| matches!(e, Event::Diagnostics { rel, items } if rel == "a.md" && items.is_empty()))
                .is_some(),
            "the link resolves now, so the hint goes"
        );
    }

    #[test]
    fn a_diagnostics_event_survives_the_wire() {
        let event = Event::Diagnostics {
            rel: "a.md".into(),
            items: vec![Diagnostic {
                range: Range::default(),
                severity: Severity::Hint,
                message: "No note named b".into(),
                source: Some("accent".into()),
                code: None,
            }],
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Event::Diagnostics { items, .. } if items.len() == 1));
    }

    /// Ghost Text off ends `merl-rt` under an open note, and on starts it again, re-indexed,
    /// without the note being opened again.
    #[test]
    fn ghost_text_off_ends_merl_and_on_starts_it_again() {
        if !in_path(GHOST) {
            eprintln!("merl-rt is not installed: skipping the ghost on/off test");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let seen = "The kettle was already boiling.\n";
        std::fs::write(root.path().join("a.md"), format!("{seen}{seen}")).unwrap();
        let (vault, _events) = Local::open_at(
            root.path(),
            &cache.path().join("index.db"),
            crate::VaultConfig::default(),
        )
        .unwrap();
        let typed = "The kettle was";
        let at = Pos {
            line: 0,
            character: typed.chars().count() as u32,
        };
        let key = (GHOST.to_string(), vault.lang.root.clone());
        let running = || {
            locked(&vault.lang.sessions)
                .get(&key)
                .and_then(|cell| cell.get().cloned())
        };
        accent_lsp::runtime().block_on(async {
            let support = vault
                .open_document("b.md", "markdown", typed.to_string())
                .await
                .unwrap();
            assert!(support.inline);
            let suggest = || async { vault.inline_completion("b.md", at).await.unwrap() };
            let mut first = None;
            for _ in 0..100 {
                first = suggest().await;
                if first.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert_eq!(first, Some(" already boiling.".to_string()));
            let merl = running().expect("merl runs");

            vault.set_ghost(false);
            for _ in 0..100 {
                if merl.is_dead() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(merl.is_dead(), "merl-rt has exited");
            assert!(running().is_none());
            assert_eq!(suggest().await, None, "nothing while it is off");

            vault.set_ghost(true);
            let mut got = None;
            for _ in 0..100 {
                got = suggest().await;
                if got.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert_eq!(
                got,
                Some(" already boiling.".to_string()),
                "a fresh merl answers"
            );
            assert!(running().is_some_and(|fresh| !Arc::ptr_eq(&fresh, &merl)));
        });
    }

    /// Retry spaced-out exits; give up after a burst and report it once.
    #[test]
    fn a_ghost_session_that_keeps_exiting_is_given_up_on_once() {
        let (events, said) = std::sync::mpsc::channel();
        let langs = Languages::new("/vault".into(), "/index.db".into(), events);
        let t0 = Instant::now();
        let at = |s| t0 + Duration::from_secs(s);
        for s in [0, 30, 50, 61] {
            assert!(langs.ghost_may_start(at(s)), "start at {s} s");
        }
        assert!(
            !langs.ghost_may_start(at(70)),
            "a fourth start within the minute"
        );
        assert!(!langs.ghost_may_start(at(500)), "given up for good");
        let said: Vec<String> = said
            .try_iter()
            .filter_map(|e| match e {
                Event::Error(message) => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(said, ["Ghost text stopped: merl-rt keeps exiting"]);
    }
}
