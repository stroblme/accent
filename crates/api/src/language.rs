//! What a text tab asks about its own content: completion, hover, definition, symbols,
//! references, folds, and the diagnostics that arrive unasked.
//!
//! One shape for every text file. A code file is answered by a language server over
//! `accent-lsp`; a note is answered by the index (`notes.rs`). The UI never learns which. Every
//! position here is a line and a column in *characters* (Unicode scalars), which is what a
//! `GtkTextIter` counts in; the UTF-16 arithmetic a server wants happens next to the text, on
//! the host that holds it.
//!
//! The requests are futures on the shared runtime, wrapped in a [`Task`] that aborts when
//! dropped: a completion the user has typed past is cancelled at the server rather than answered
//! into the void.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use accent_core::index::Index;
use accent_core::markdown;
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::remote::Remote;
use crate::vault::{Backend, remote_err};
use crate::{Event, FileEdits, Local, LspConfig, Vault, locked};

pub(crate) mod external;
mod latex;
pub(crate) mod notes;
pub(crate) mod words;

use notes::Notes;

/// A line and a column, both zero-based; the column counts characters from the line start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Pos {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Range {
    pub start: Pos,
    pub end: Pos,
}

/// Somewhere to go. `path` is vault-relative inside the vault, absolute outside it, and a URL
/// when the target is not a file at all (an external link in a note).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub range: Range,
    /// A link's `#anchor` where `range` cannot place it: a PDF's `page=3&selection=…`, or a
    /// heading or a `^block` the file does not have, `range` then being the top of the file.
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

/// What a completion item is, for the glyph beside it. The LSP kinds accent shows, plus `Tag`
/// for a note's `#tag`; anything else a server sends is `Text`.
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
    /// What `insert` replaces: the word or trigger before the caret, and whatever the item wants
    /// eaten after it.
    pub replace: Range,
    /// Applied with the insert, in one undo step: an import, typically.
    pub extra_edits: Vec<TextEdit>,
    /// The item as the server sent it, when it can still be resolved for more; opaque to the UI.
    pub resolve: Option<serde_json::Value>,
}

/// A completion answer. `incomplete` is the server saying it stopped at a cap: the list has to
/// be asked for again as the word grows, because narrowing what it sent would miss items it
/// left out.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Completions {
    pub items: Vec<Completion>,
    pub incomplete: bool,
    /// A PDF's bookmarks the side that answered could not list, having no PDF reader: a host's
    /// `serve`. The window lists them from its own copy of the file ([`Vault::completion`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<PdfPages>,
}

/// `[[paper.pdf#` left for the side holding a copy of the PDF to answer: which file, and what
/// each `[[paper.pdf#page=N]]` row keeps of the link as typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfPages {
    /// The PDF the link resolved to, vault-relative.
    pub rel: String,
    /// The link's target as it was typed, which every row spells the same way.
    pub note: String,
    pub replace: Range,
}

/// The signature the caret is inside a call of.
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
}

/// Lines that can be hidden behind their first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fold {
    pub start_line: u32,
    pub end_line: u32,
}

/// What attached to an opened document.
///
/// `default` on the struct, because this crosses the rpc wire: a client newer than the host's
/// `accent-cli serve` must read what an older one sends rather than fail on a missing field.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Support {
    /// Characters that open the completion popup as they are typed.
    pub completion_triggers: Vec<char>,
    /// Characters that ask for a signature as they are typed.
    pub signature_triggers: Vec<char>,
    /// The language server accent looked for and did not find, so the UI can say so when asked.
    pub missing: Option<String>,
    /// Something answers for ghost text, so the UI arms the inline path for this tab.
    pub inline: bool,
}

// -------------------------------------------------------------------- tasks

/// A request in flight on the shared runtime. Dropping it aborts the request, which is what
/// sends `$/cancelRequest` to a language server.
///
/// A `JoinHandle` can be polled from any executor, so the GTK main loop awaits one directly.
pub struct Task<T> {
    handle: tokio::task::JoinHandle<Result<T>>,
    /// What to do when the answer stops mattering, for a request that cannot be aborted where it
    /// waits. A remote round trip runs on a blocking thread, where `abort` does nothing at all,
    /// so it is cancelled by telling the server instead.
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl<T> Future for Task<T> {
    type Output = Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.handle).poll(cx) {
            Poll::Ready(Ok(r)) => Poll::Ready(r),
            Poll::Ready(Err(e)) => Poll::Ready(Err(anyhow::anyhow!("{e}"))),
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

/// One thing that answers for a set of open documents: a language server, or the index.
///
/// Boxed futures rather than `async fn`, because the registry holds these as `dyn Language`.
/// `open`, `change` and `close` are synchronous: each is a notification or a local analysis,
/// and neither provider waits on anything to answer them.
pub(crate) trait Language: Send + Sync {
    fn open(&self, rel: &str, language_id: &str, text: String) -> Result<Support>;
    fn change(&self, rel: &str, text: String) -> Result<()>;
    /// The document reached the disk. What a server does on a save it does not do on a change:
    /// rust-analyzer's `cargo check` diagnostics, for one.
    fn saved(&self, _rel: &str) -> Result<()> {
        Ok(())
    }
    /// The user left the document. A provider too expensive to tell about every save is told
    /// here instead: merl rebuilds its whole index on a save, which is not a per-keystroke cost.
    fn settle(&self, _rel: &str) -> Result<()> {
        Ok(())
    }
    /// Say again what is wrong with the document, the text being what it was. For a provider
    /// whose answer depends on more than the document — the index, which the note a link names
    /// may have just appeared in — where a language server publishes on its own and ignores this.
    fn rediagnose(&self, _rel: &str) -> Result<()> {
        Ok(())
    }
    fn close(&self, rel: &str);
    fn completion(&self, rel: &str, pos: Pos, trigger: Option<char>) -> Fut<'_, Completions>;
    /// The rest of the line, from a model or an index of the vault: ghost text, painted where
    /// the caret is rather than listed in the popup. `None` from anything that does not offer it.
    fn inline_completion(&self, _rel: &str, _pos: Pos) -> Fut<'_, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    /// Fill in what the item was too expensive to send: `rel` says which document's text the
    /// edits it comes back with are measured against.
    fn resolve(&self, rel: &str, item: Completion) -> Fut<'_, Completion>;
    fn signature_help(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Signature>>;
    fn hover(&self, rel: &str, pos: Pos) -> Fut<'_, Option<Hover>>;
    fn definition(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>>;
    fn symbols(&self, rel: &str) -> Fut<'_, Vec<Symbol>>;
    fn references(&self, rel: &str, pos: Pos) -> Fut<'_, Vec<Location>>;
    fn folds(&self, rel: &str) -> Fut<'_, Vec<Fold>>;
    /// Whether the provider wants to be asked before the file or folder at `abs` moves: a
    /// language server's `willRename` filters.
    fn renames(&self, _abs: &Path, _is_dir: bool) -> bool {
        false
    }
    /// What has to change because these files move, `(old, new)` vault-relative: an import naming
    /// a moved module. Asked before they move, because a server may look at the disk to answer.
    fn will_rename(&self, _moves: Vec<(String, String)>) -> Fut<'_, Vec<FileEdits>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    /// The provider stopped answering (the server exited); the registry starts a fresh one.
    fn is_dead(&self) -> bool {
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

/// The open documents: each one's provider, and the session it came from.
pub(crate) type Open = HashMap<String, (Option<Key>, Arc<dyn Language>)>;

/// What opening a document settled on: the provider that answers for it, if anything does, the
/// protocol's name for its language, a server that should have been installed and was not, and
/// the session to count the document against.
type Opened = (
    Option<Arc<dyn Language>>,
    String,
    Option<String>,
    Option<Key>,
);

/// The providers a vault has started, and which document each one holds.
///
/// Filled in by the notes and external providers; the registry itself is the shape of the
/// session map and of the `Event` channel the diagnostics travel on.
pub(crate) struct Languages {
    pub(crate) root: PathBuf,
    pub(crate) db: PathBuf,
    pub(crate) events: Sender<Event>,
    /// One session per (server, root): the cell makes concurrent openers await one start.
    pub(crate) sessions: Mutex<HashMap<Key, Session>>,
    /// Open document → the provider holding it, and the session it came from: the last document
    /// to close takes that session down with it. The key is `None` for a document no session
    /// answers for, which is a prose file with its words and nothing else.
    pub(crate) docs: Mutex<Open>,
    /// Whether a prose document gets a ghost-text session; the preference behind it is global,
    /// so a vault reads it once and every document opened after that follows.
    pub(crate) ghost: AtomicBool,
    /// When the ghost session was started within the last [`GHOST_WINDOW`]; `None` once the
    /// vault has given up on it.
    ghost_starts: Mutex<Option<Vec<Instant>>>,
    /// The folders a LaTeX document's `\input{` lists.
    listing: Arc<Listing>,
}

/// The vault's listing of a folder, the file tree's own ([`crate::local::list_dir`]), for a
/// completion that offers files. It reads the index on a connection of its own, opened by the
/// first such completion, so it never waits on the tree's.
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
        Arc::new(Languages {
            root,
            db,
            events,
            sessions: Mutex::new(HashMap::new()),
            docs: Mutex::new(HashMap::new()),
            ghost: AtomicBool::new(true),
            ghost_starts: Mutex::new(Some(Vec::new())),
            listing,
        })
    }

    /// Turn ghost text on or off for documents opened from here on.
    ///
    /// ponytail: a session already running is left alone; it ends with the vault. Stopping one
    /// means shutting a server down under documents that are still open.
    pub(crate) fn set_ghost(&self, on: bool) {
        self.ghost.store(on, Ordering::Relaxed);
    }

    /// The session under `key`, started with `start` if it is not there yet. Concurrent openers
    /// of the same server await one start, and a start that failed is not remembered, so the
    /// next document to open tries again.
    pub(crate) async fn session(
        self: &Arc<Self>,
        key: Key,
        start: impl Future<Output = Result<Arc<dyn Language>>>,
    ) -> Result<Arc<dyn Language>> {
        let cell = {
            let mut sessions = locked(&self.sessions);
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
        cell.get_or_try_init(|| start).await.cloned()
    }

    /// The vault's one ghost-text session, started on the first prose document that wants it.
    ///
    /// Keyed by the vault root rather than by `session_root`: merl indexes the whole vault, and
    /// a multi-project vault would otherwise get one index per `.git` in it. The argv is exactly
    /// these three words — merl-rt exits with a usage message on any other flag.
    async fn ghost_session(self: &Arc<Self>) -> Option<Arc<dyn Language>> {
        let root = self.root.clone();
        let key = (GHOST.to_string(), root.clone());
        let argv = vec![
            GHOST.to_string(),
            "--vault".to_string(),
            root.to_string_lossy().into_owned(),
        ];
        let start = async {
            anyhow::ensure!(
                self.ghost_may_start(Instant::now()),
                "{GHOST} keeps exiting"
            );
            external::start(
                argv,
                root.clone(),
                root,
                self.events.clone(),
                Some("suggestions"),
            )
            .await
        };
        match self.session(key, start).await {
            Ok(session) => Some(session),
            Err(e) => {
                tracing::debug!("no ghost text: {e:#}");
                None
            }
        }
    }

    /// How a document gets the ghost session again once the one it was opened on has exited:
    /// the running one, or a fresh start. Nothing while Ghost Text is off. Weak, because the
    /// registry holds the document's provider.
    fn respawn(self: &Arc<Self>) -> words::Respawn {
        let me = Arc::downgrade(self);
        Box::new(move || {
            let me = me.upgrade();
            Box::pin(async move {
                let me = me.filter(|me| me.ghost.load(Ordering::Relaxed))?;
                me.ghost_session().await
            })
        })
    }

    /// Count a start of the ghost session at `now`, or refuse it: a `merl-rt` that exits as soon
    /// as it starts must not be started again on every pause in the typing. Past [`GHOST_STARTS`]
    /// within [`GHOST_WINDOW`] the vault gives up on it until it is opened again, and says so once.
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
            .map(|(_, provider)| provider.clone())
            .ok_or_else(|| anyhow::anyhow!("{rel} is not open"))
    }

    /// Stop every provider and forget every document.
    ///
    /// Blocks on the runtime, so it must never be called from a runtime worker thread; its one
    /// caller is `Local::drop`, which runs on the thread that closed the window.
    pub(crate) fn shutdown(&self) {
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

    /// Ask every language server already running, whose project holds a moved path and whose
    /// filters take it, what the moves mean for the code naming them — before anything moves.
    /// Returns those edits and the moves some server was asked about.
    ///
    /// Only running sessions: a rename is no reason to start rust-analyzer, and a server that
    /// was not running has nothing open that could be wrong. Each is given 5 s. Blocks on the
    /// runtime, so it must not be called from one of its workers; the callers are a rename's
    /// worker thread and the host's request threads.
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

    pub(crate) fn open_document(
        self: &Arc<Self>,
        rel: String,
        language: String,
        text: String,
        cfg: &LspConfig,
    ) -> Task<Support> {
        let me = self.clone();
        let which = server(cfg, &language);
        Task::spawn(async move {
            // What speaks the file's structure, if anything does: the index for a note, a
            // language server for code, nothing for a `.txt`.
            let (primary, language_id, missing, key): Opened = match which {
                Some(Server::Notes) => {
                    // One notes provider per vault, whatever the note: they share the index.
                    let key = ("accent".to_string(), me.root.clone());
                    let (root, db, events) = (me.root.clone(), me.db.clone(), me.events.clone());
                    let start =
                        async move { Ok(Arc::new(Notes::open_at(root, &db, events)?) as _) };
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
                    let session_root = session_root(&root, &Local::join(&root, &rel)?);
                    let key = (argv[0].clone(), session_root.clone());
                    let start = external::start(argv, session_root, root, events, None);
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
            let ghost = match prose && me.ghost.load(Ordering::Relaxed) && in_path(GHOST) {
                true => me.ghost_session().await.map(|g| (g, me.respawn())),
                false => None,
            };
            // Prose gets its words layered under whatever the primary answers, and gets them
            // even with no primary at all.
            let provider: Arc<dyn Language> = match (primary, prose) {
                (Some(primary), false) => primary,
                (primary, true) => {
                    let listing = (language == "latex").then(|| me.listing.clone());
                    Arc::new(words::Layered::new(primary, ghost, listing))
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
            locked(&me.docs).insert(rel, (key, provider));
            Ok(support)
        })
    }

    /// The user closed the tab: the provider is told, and a session left with no open document
    /// is shut down rather than kept for the life of the vault.
    ///
    /// ponytail: the last close is the rule, not a wall clock. Reopening the file then pays a
    /// cold start, which is the price of not holding a rust-analyzer for a file nobody is
    /// reading. The ghost session is not counted here — it is one process for the whole vault
    /// and its start is measured in seconds; it still ends with the vault.
    pub(crate) fn close_document(&self, rel: String) -> Task<()> {
        let (key, provider) = match locked(&self.docs).remove(&rel) {
            Some((key, provider)) => (key, Some(provider)),
            None => (None, None),
        };
        let idle = key.filter(|key| {
            !locked(&self.docs)
                .values()
                .any(|(open, _)| open.as_ref() == Some(key))
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

/// The ghost-text server: one process per vault, answering `textDocument/inlineCompletion` from
/// an index of the vault's own notes. Not in `SERVERS`, because it answers beside a language's
/// own provider rather than instead of one.
const GHOST: &str = "merl-rt";

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

/// The servers accent starts by itself: GtkSourceView language id, the id the protocol calls the
/// same language, and the command lines to try in order.
///
/// Deliberately short. A language nobody here has run is better served by a line in the vault's
/// config than by a guess in this table.
const SERVERS: &[(&str, &str, &[&[&str]])] = &[
    ("rust", "rust", &[&["rust-analyzer"]]),
    ("c", "c", &[&["clangd"]]),
    ("cpp", "cpp", &[&["clangd"]]),
    ("python", "python", PYTHON),
    ("python3", "python", PYTHON),
    ("js", "javascript", TYPESCRIPT),
    ("typescript", "typescript", TYPESCRIPT),
    ("latex", "latex", &[&["texlab"]]),
    ("toml", "toml", &[&["taplo", "lsp", "stdio"]]),
    ("go", "go", &[&["gopls"]]),
];

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
    let alternatives: Vec<Vec<String>> = match configured {
        Some(argv) => vec![argv.clone()],
        None => row?
            .2
            .iter()
            .map(|argv| argv.iter().map(|&a| a.to_string()).collect())
            .collect(),
    };
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

/// Whether an executable can be started: an absolute name is the file itself, a bare one is
/// looked for the way a shell would.
fn in_path(exe: &str) -> bool {
    let path = Path::new(exe);
    if path.is_absolute() {
        return path.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(exe).is_file()))
}

/// Where a server should be rooted for a file: the nearest checkout at or above it that is still
/// inside the vault, else the vault itself.
///
/// A vault of several projects gets one server per project, which is what makes `rust-analyzer`
/// see a crate rather than a directory of unrelated ones. A worktree's `.git` is a file, so
/// existence is the test, not directory-ness.
pub(crate) fn session_root(vault_root: &Path, abs_file: &Path) -> PathBuf {
    let mut dir = abs_file.parent();
    while let Some(here) = dir.filter(|d| d.starts_with(vault_root)) {
        if here.join(".git").exists() {
            return here.to_path_buf();
        }
        dir = here.parent();
    }
    vault_root.to_path_buf()
}

// ------------------------------------------------------------------ the façade

/// What a text tab asks about the document it holds. Every one of these is a [`Task`]: the UI
/// awaits it on its own loop and drops it when the answer stops mattering.
///
/// A remote vault asks the host, where the files and the language servers are; the call itself
/// blocks, so it runs on a blocking thread of the runtime rather than being cancelled.
impl Vault {
    /// Start answering for `rel`, and say what the provider can do. Nothing else here works
    /// before this has finished.
    ///
    /// Spelled out rather than tabled: it is the one call that reads the vault's config, for the
    /// language server the file's language names.
    pub fn open_document(&self, rel: &str, language_id: &str, text: String) -> Task<Support> {
        match &self.backend {
            Backend::Local(v) => v.open_document(rel, language_id, text),
            Backend::Remote(r) => {
                remote_task(r.clone(), "open_document", json!([rel, language_id, text]))
            }
        }
    }

    /// The user closed the tab. Spelled out because it forgets the document rather than telling
    /// a provider about it.
    pub fn close_document(&self, rel: &str) -> Task<()> {
        match &self.backend {
            Backend::Local(v) => v.close_document(rel),
            Backend::Remote(r) => remote_task(r.clone(), "close_document", json!([rel])),
        }
    }
}

/// What a document is asked and told, written once for the three layers that carry it:
/// [`Languages`] finds the provider that holds the document and spawns the call on the runtime,
/// [`Vault`] makes it here or on the host that has the files, and the host answers it through the
/// same rpc dispatch every other method goes through. Each line reads `façade => trait method`. A
/// line with an answer is a request, whose trait method is a future to await, and `then f`
/// finishes a host's answer here with `f`; one without is a notification, which the provider has
/// taken in by the time its trait method returns.
macro_rules! requests {
    (@remote $r:ident $name:ident $params:expr) => {
        remote_task($r.clone(), stringify!($name), $params)
    };
    (@remote $r:ident $name:ident $params:expr, $then:path) => {
        remote_task_then($r.clone(), stringify!($name), $params, $then)
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

        /// These methods' half of the rpc dispatch. Every request runs on its own thread in
        /// [`crate::rpc::serve_local`], so blocking on the runtime here is a wait for one answer
        /// and never a nested one.
        pub(crate) fn dispatch(
            vault: &Local,
            method: &str,
            p: &Value,
        ) -> Option<std::result::Result<Value, crate::rpc::RpcError>> {
            $( if method == stringify!($name) {
                crate::rpc::args!(p; rel: String $(, $arg: $ty)*);
                return Some(crate::rpc::any(crate::rpc::block(
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
    /// The rest of the line as ghost text, or nothing. Asked on every pause in the typing, so
    /// dropping the task is the normal end of one.
    inline_completion => inline_completion(pos: Pos) -> Option<String>;

    // What the editor did to the document: nothing to answer, and nothing to await inside.
    change_document => change(text: String);
    /// The buffer was written; a server that checks on save is told.
    save_document => saved();
    /// The user left the document. Cheap for every provider but the ghost one, which re-reads
    /// the vault here rather than on every autosave.
    settle => settle();
    /// The index moved under the document: its hints are worked out again and published. What a
    /// note's dangling link needs, the note it names having just been created.
    rediagnose => rediagnose();
}

/// One remote request as a task.
///
/// The round trip runs on a blocking thread, where `abort` does nothing, so dropping the task
/// sends the server a `cancel` for that request id instead: a completion the user has typed past
/// is dropped where the language server is, rather than computed into a pipe nobody reads.
fn remote_task<T: DeserializeOwned + Send + 'static>(
    r: Arc<Remote>,
    method: &'static str,
    params: Value,
) -> Task<T> {
    remote_task_then(r, method, params, |_, answer| Ok(answer))
}

/// [`remote_task`], with the answer finished by `then` on the same blocking thread.
fn remote_task_then<T: DeserializeOwned + Send + 'static>(
    r: Arc<Remote>,
    method: &'static str,
    params: Value,
    then: fn(&Remote, T) -> Result<T>,
) -> Task<T> {
    let asked: Arc<crate::remote::Asked> = Arc::default();
    Task::blocking({
        let (r, asked) = (r.clone(), asked.clone());
        move || {
            let answer = r
                .call_tracked(method, params, &asked, crate::rpc::DEADLINE)
                .map_err(remote_err)?;
            then(&r, answer)
        }
    })
    .cancelled_by(move || r.cancel(&asked))
}

/// List the bookmarks a host left unlisted in its answer to `[[paper.pdf#`, its `serve` having no
/// PDF reader: from the copy this machine keeps to show the PDF, fetched as opening it would be.
/// Remote bytes travel as files, never in the protocol (DESIGN.md, Architecture).
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
            &self.config().lsp,
        )
    }

    pub fn close_document(&self, rel: &str) -> Task<()> {
        self.lang.close_document(rel.to_string())
    }
}

/// A note's headings as symbols and the folds of its sections, fences and frontmatter, read from
/// its text alone: what the notes provider answers, for a note outside every vault, which has no
/// provider to ask.
pub fn note_outline(text: &str) -> (Vec<Symbol>, Vec<Fold>) {
    let analysis = markdown::analyze(text);
    (
        notes::symbols_of(text, &analysis.headings),
        notes::folds_of(text, &analysis),
    )
}

/// Byte offset → position in `text`. Linear in the offset; a line table if a note ever makes it show.
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

    #[test]
    fn a_session_is_rooted_at_the_nearest_checkout() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let crate_dir = root.join("proj/src");
        std::fs::create_dir_all(&crate_dir).unwrap();
        assert_eq!(
            session_root(root, &crate_dir.join("main.rs")),
            root,
            "no checkout anywhere: the vault is the project"
        );

        // A worktree's `.git` is a file, and it counts the same as a directory.
        std::fs::write(root.join("proj/.git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            session_root(root, &crate_dir.join("main.rs")),
            root.join("proj")
        );
        assert_eq!(
            session_root(root, &root.join("loose.rs")),
            root,
            "a file beside the checkout is not inside it"
        );
    }

    /// The notes provider is the session every checkout has — no binary to install — so it is
    /// what proves the rule: a session lives exactly as long as a document it holds is open.
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
            }],
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Event::Diagnostics { items, .. } if items.len() == 1));
    }

    /// Exits spread out are started again every time; a burst of them is given up on for good,
    /// and said so once.
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
