//! accent-api: the UI-facing façade. Plain serde data types only; no GTK, no Android types.
//! Desktop links this directly; Android gets uniffi bindings of this crate; the CLI renders it as JSON-RPC over stdio.
//!
//! [`Vault`] owns the lifecycle of one open vault: three SQLite connections, the filesystem
//! watcher, and the batching that turns a Syncthing pull of 500 files into a single [`Event`].
//! The caller reads on its own connection and never waits for the worker, which is what keeps a
//! UI thread free while the vault is being indexed.

use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub mod language;
mod paths;
pub mod remote;
pub mod rpc;
pub mod ssh;
mod worker;

#[cfg(test)]
mod tests;

pub use paths::conflict_original_rel;
use paths::{accent_conflict_name, conflict_pairs, outside, stem_key, with_md};
use worker::Msg;

use accent_core::index::Index;
use accent_core::path::{basename, stem};
use accent_core::walk;
use accent_core::{diff, markdown, template};
// The module too: a tab matches on `fs::Read`, and the façade hands one back.
pub use accent_core::fs;

pub use accent_core::config::{Config, LspConfig, Session, VaultConfig};
pub use accent_core::diff::{DiffLine, Op};
pub use accent_core::fs::{Etag, Read, SaveError, Text};
// The module as well as its types: the git operations take a `Repo`, not a `Vault`, so callers
// reach them as `accent_api::git::status(&repo)` after asking the vault which repos there are.
pub use accent_core::git;
pub use accent_core::git::{Branch, Commit, Entry, LogRow, Repo, Status, Submodule};
pub use accent_core::index::{
    Backlink, FileRow, Match, PdfLink, Progress, ReconcileStats, SearchHit, Stats,
};
pub use accent_core::search::{self, Options, Regex};
pub use accent_core::walk::FileKind;
pub use language::{
    Completion, Completions, Diagnostic, Fold, Hover, Kind, Location, Pos, Range, Severity,
    Signature, Support, Symbol, Task, TextEdit,
};

// ---------------------------------------------------------------- public data

/// What happened in the vault, in vault-relative paths the UI can use directly.
///
/// No `PartialEq`: `index::Progress` has none, and an event type is matched, not compared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    Progress(Progress),
    Reconciled(ReconcileStats),
    /// The direct children of these directories changed ("" is the vault root). The tree refills
    /// exactly these levels instead of dropping its whole cache.
    DirsChanged(Vec<String>),
    /// Someone else changed this note's content. Never fires for our own saves.
    FileChanged(String),
    FileRemoved(String),
    FileRenamed {
        from: String,
        to: String,
    },
    Conflict {
        original: String,
        conflict: String,
    },
    /// Something under a repository's `.git` moved: a commit, a checkout, a stage. The git pane
    /// refreshes on it, which is how a `git commit` typed in a shell reaches the UI.
    GitChanged,
    /// A remote vault is still getting ready, and this is what it is doing. Shown where the
    /// indexing progress is shown, because to the reader it is the same wait.
    ///
    /// `fraction` is how far the step has got, 0 to 1, for the one step that can measure itself:
    /// uploading the server binary, which is most of a first connection's wait. The others are
    /// waits of unknown length, and say so with `None` rather than with a number nobody computed.
    Connecting {
        what: String,
        fraction: Option<f64>,
    },
    /// The remote vault is answering. A local vault never sends this: it is connected from the
    /// moment it opens.
    Connected,
    /// The remote vault is not answering, and why. Reads stay served from whatever the UI already
    /// has; writes fail until [`Vault::reconnect`] succeeds.
    Disconnected(String),
    Error(String),
    /// What a language provider has to say about an open document, whole: an empty list clears.
    Diagnostics {
        rel: String,
        items: Vec<Diagnostic>,
    },
    /// A language provider started or finished a background job worth waiting for — the
    /// ghost-text index rebuilding, and nothing else today. `what` is what to call it on screen.
    /// Shown where the vault's own indexing is shown, and yielding to it: this one is optional.
    Busy {
        what: String,
        busy: bool,
    },
}

/// What a rename would do, so the UI can confirm before anything is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenamePlan {
    pub from: String,
    pub to: String,
    pub rewrites: Vec<String>,
}

/// What a global replace wrote, in the shape [`RenameReport`] has: what worked is counted, what
/// failed is named, and a note the replace could not write never fails the whole call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplaceReport {
    pub rewritten: Vec<String>,
    pub matches: usize,
    pub failed: Vec<(String, String)>,
}

/// What it actually did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenameReport {
    pub rewritten: Vec<String>,
    pub failed: Vec<(String, String)>,
}

// --------------------------------------------------------------------- vault

/// One open vault, wherever it lives.
///
/// A window holds exactly one of these and cannot tell the two apart: every method below means
/// the same thing whether the files are on this machine or on the other end of an ssh connection.
/// That is the whole point of the split — the UI was written against a local vault and did not
/// have to learn anything to work on a remote one.
///
/// Reads and writes are synchronous here, as they always were. A remote call is a round trip, so
/// the desktop runs the ones that paint a list or open a document on a worker thread; the ones
/// that follow a click and write a file stay where they are, because a save that takes a
/// millisecond longer is not something anyone can feel.
pub struct Vault {
    backend: Backend,
    /// What this vault is called in the config, the recents and the session file: the root for a
    /// local vault, the `ssh://` address for a remote one. Never a path to open.
    key: PathBuf,
}

// ponytail: `Local` is the big variant, so every remote `Vault` carries its footprint too. One
// per window makes that a few hundred bytes in the whole process; box it if that ever stops being
// true.
#[allow(clippy::large_enum_variant)]
enum Backend {
    Local(Local),
    Remote(std::sync::Arc<remote::Remote>),
}

impl Vault {
    /// Open a vault on this machine.
    pub fn open(root: &Path, cfg: VaultConfig) -> Result<(Vault, Receiver<Event>)> {
        let (local, events) = Local::open(root, cfg)?;
        Ok((Vault::of(Backend::Local(local)), events))
    }

    /// [`open`](Self::open) with an explicit index file, for tests and tooling.
    pub fn open_at(root: &Path, db: &Path, cfg: VaultConfig) -> Result<(Vault, Receiver<Event>)> {
        let (local, events) = Local::open_at(root, db, cfg)?;
        Ok((Vault::of(Backend::Local(local)), events))
    }

    /// Open a vault on another machine, addressed as `ssh://[user@]host[:port]/path`.
    ///
    /// Returns before the connection exists. The window opens on the spot and the connection
    /// reports itself through the events: [`Event::Connecting`] while it works, then
    /// [`Event::Connected`] or [`Event::Disconnected`].
    pub fn open_remote(url: &str, cfg: VaultConfig) -> Result<(Vault, Receiver<Event>)> {
        let url = ssh::parse(url).map_err(|e| anyhow::anyhow!("{url}: {e}"))?;
        let (events, event_rx) = channel::<Event>();
        let key = PathBuf::from(url.to_string());
        let remote = remote::Remote::open(url, cfg, events);
        Ok((
            Vault {
                backend: Backend::Remote(remote),
                key,
            },
            event_rx,
        ))
    }

    fn of(backend: Backend) -> Vault {
        let key = match &backend {
            Backend::Local(v) => v.root().to_path_buf(),
            Backend::Remote(r) => PathBuf::from(r.url().to_string()),
        };
        Vault { backend, key }
    }

    /// The vault root: an absolute path *on the machine holding the files*. Every `rel` this API
    /// takes or returns is relative to it, and so is every path a [`Repo`] carries.
    pub fn root(&self) -> PathBuf {
        match &self.backend {
            Backend::Local(v) => v.root().to_path_buf(),
            Backend::Remote(r) => r.root(),
        }
    }

    /// What this vault is keyed by: the root locally, the `ssh://` address remotely. The recent
    /// list, the per-vault settings and the session file all use this, so a remote vault keeps
    /// its own history without ever being mistaken for a directory on this machine.
    pub fn key(&self) -> &Path {
        &self.key
    }

    /// The remote half, for the things only a remote vault has: a shell on the host, a port
    /// forward, an upload. `None` for a local vault, which is how the UI decides what to offer.
    pub fn remote(&self) -> Option<&std::sync::Arc<remote::Remote>> {
        match &self.backend {
            Backend::Remote(r) => Some(r),
            Backend::Local(_) => None,
        }
    }

    pub fn is_remote(&self) -> bool {
        self.remote().is_some()
    }

    /// Try the connection again after [`Event::Disconnected`]. Does nothing to a local vault.
    pub fn reconnect(&self) {
        if let Backend::Remote(r) = &self.backend {
            r.reconnect();
        }
    }

    pub fn config(&self) -> VaultConfig {
        match &self.backend {
            Backend::Local(v) => v.config(),
            Backend::Remote(r) => r.config(),
        }
    }

    pub fn set_config(&self, cfg: VaultConfig) {
        match &self.backend {
            Backend::Local(v) => v.set_config(cfg),
            Backend::Remote(r) => r.set_config(cfg),
        }
    }

    /// Whether a prose document opened from here on gets a ghost-text session. Global rather
    /// than per vault, which is why it does not travel in [`VaultConfig`].
    pub fn set_ghost(&self, on: bool) {
        match &self.backend {
            Backend::Local(v) => v.set_ghost(on),
            Backend::Remote(r) => r.set_ghost(on),
        }
    }

    pub fn rescan(&self) {
        match &self.backend {
            Backend::Local(v) => v.rescan(),
            Backend::Remote(r) => {
                let _ = r.call::<()>("rescan", json!([]));
            }
        }
    }

    /// Join `rel` to the vault root, refusing anything that would land outside it.
    ///
    /// For a remote vault the answer is a path on the *host*, so it is what to show and what to
    /// pass to a remote command — never something to open. Use [`fetch`](Self::fetch) for that.
    pub fn resolve(&self, rel: &str) -> io::Result<PathBuf> {
        match &self.backend {
            Backend::Local(v) => v.resolve(rel),
            Backend::Remote(_) => Local::join(&self.root(), rel),
        }
    }

    /// The session as it was left. Always this machine's: where the windows and tabs were is a
    /// fact about the desk, not about the files.
    pub fn session(&self) -> Session {
        Session::load(&self.key)
    }

    pub fn save_session(&self, s: &Session) -> Result<()> {
        s.save(&self.key)
    }
}

/// Turn an RPC failure into the `anyhow` error every caller of the façade already handles.
fn remote_err(e: rpc::RpcError) -> anyhow::Error {
    anyhow::anyhow!("{}", e.message)
}

macro_rules! ask {
    ($self:ident, $local:expr, $method:literal, $params:expr) => {
        match &$self.backend {
            Backend::Local(v) => $local(v),
            Backend::Remote(r) => r.call($method, $params).map_err(remote_err),
        }
    };
}

// Files. Every one of these is the same operation on either machine; what differs is only where
// the bytes are, and none of them carry any.
impl Vault {
    pub fn read(&self, rel: &str) -> io::Result<(String, Etag)> {
        match &self.backend {
            Backend::Local(v) => v.read(rel),
            Backend::Remote(r) => r
                .call("read", json!([rel]))
                .map_err(rpc::RpcError::io_error),
        }
    }

    pub fn read_text(&self, rel: &str) -> io::Result<fs::Read> {
        match &self.backend {
            Backend::Local(v) => v.read_text(rel),
            Backend::Remote(r) => r
                .call("read_text", json!([rel]))
                .map_err(rpc::RpcError::io_error),
        }
    }

    pub fn stat(&self, rel: &str) -> io::Result<Option<Etag>> {
        match &self.backend {
            Backend::Local(v) => v.stat(rel),
            Backend::Remote(r) => r
                .call("stat", json!([rel]))
                .map_err(rpc::RpcError::io_error),
        }
    }

    /// Whether there is anything at `rel`. One `stat`, and the answer the tree and the open path
    /// actually want.
    pub fn exists(&self, rel: &str) -> bool {
        matches!(self.stat(rel), Ok(Some(_)))
    }

    pub fn save(&self, rel: &str, text: &str, expected: Option<Etag>) -> Result<Etag, SaveError> {
        match &self.backend {
            Backend::Local(v) => v.save(rel, text, expected),
            Backend::Remote(r) => r
                .call("save", json!([rel, text, expected]))
                .map_err(rpc::RpcError::save_error),
        }
    }

    /// Delete a file or a directory. Local vaults go to the system trash through the desktop, so
    /// this is the remote path only — permanent, and confirmed as such by the UI.
    pub fn delete(&self, rel: &str) -> io::Result<()> {
        match &self.backend {
            Backend::Local(v) => v.delete(rel),
            Backend::Remote(r) => r
                .call("delete", json!([rel]))
                .map_err(rpc::RpcError::io_error),
        }
    }

    /// A path on *this* machine holding `rel`'s current bytes: the file itself when the vault is
    /// local, a cached copy fetched over ssh when it is not. For the readers that need a real
    /// file — the PDF viewer, an image, the preview's assets.
    pub fn fetch(&self, rel: &str) -> io::Result<PathBuf> {
        match &self.backend {
            Backend::Local(v) => v.resolve(rel),
            Backend::Remote(r) => r.fetch(rel),
        }
    }

    /// Copy a file from this machine into the vault.
    pub fn upload(&self, local: &Path, rel: &str) -> io::Result<()> {
        match &self.backend {
            Backend::Local(v) => std::fs::copy(local, v.resolve(rel)?).map(|_| ()),
            Backend::Remote(r) => r.upload(local, rel),
        }
    }

    /// Copy a file out of the vault to somewhere on this machine.
    pub fn download(&self, rel: &str, dest: &Path) -> io::Result<()> {
        match &self.backend {
            Backend::Local(v) => std::fs::copy(v.resolve(rel)?, dest).map(|_| ()),
            Backend::Remote(r) => r.download(rel, dest),
        }
    }

    pub fn create_note(&self, rel: &str, template: Option<&str>) -> Result<(String, Vec<usize>)> {
        ask!(
            self,
            |v: &Local| v.create_note(rel, template),
            "create_note",
            json!([rel, template])
        )
    }

    pub fn create_dir(&self, rel: &str) -> io::Result<()> {
        match &self.backend {
            Backend::Local(v) => v.create_dir(rel),
            Backend::Remote(r) => r
                .call("create_dir", json!([rel]))
                .map_err(rpc::RpcError::io_error),
        }
    }

    pub fn plan_rename(&self, from: &str, to: &str) -> Result<RenamePlan> {
        ask!(
            self,
            |v: &Local| v.plan_rename(from, to),
            "plan_rename",
            json!([from, to])
        )
    }

    pub fn rename(&self, plan: &RenamePlan, rewrite_links: bool) -> Result<RenameReport> {
        ask!(
            self,
            |v: &Local| v.rename(plan, rewrite_links),
            "rename",
            json!([plan, rewrite_links])
        )
    }

    /// Replace every match in every note that has one.
    ///
    /// The pattern crosses as what the user typed plus the three toggles, not as a compiled
    /// regex: a `Regex` cannot be serialised, and case-insensitivity lives in the builder rather
    /// than in the pattern string, so sending the string alone would quietly change the search.
    pub fn replace_all(
        &self,
        query: &str,
        options: Options,
        replacement: &str,
        literal: bool,
    ) -> Result<ReplaceReport> {
        match &self.backend {
            Backend::Local(v) => {
                v.replace_all(&search::pattern(query, options)?, replacement, literal)
            }
            Backend::Remote(r) => r
                .call("replace_all", json!([query, options, replacement, literal]))
                .map_err(remote_err),
        }
    }

    pub fn adopt_conflict(&self, original: &str, conflict: &str) -> Result<Etag> {
        ask!(
            self,
            |v: &Local| v.adopt_conflict(original, conflict),
            "adopt_conflict",
            json!([original, conflict])
        )
    }

    pub fn conflict_diff(&self, original: &str, conflict: &str) -> Result<Vec<DiffLine>> {
        ask!(
            self,
            |v: &Local| v.conflict_diff(original, conflict),
            "conflict_diff",
            json!([original, conflict])
        )
    }

    pub fn template_target(&self, template: &str) -> Result<Option<String>> {
        ask!(
            self,
            |v: &Local| v.template_target(template),
            "template_target",
            json!([template])
        )
    }

    pub fn note_from_template(&self, template: &str) -> Result<Option<(String, Vec<usize>)>> {
        ask!(
            self,
            |v: &Local| v.note_from_template(template),
            "note_from_template",
            json!([template])
        )
    }

    pub fn render_template(&self, template: &str, title: &str) -> Result<(String, Vec<usize>)> {
        ask!(
            self,
            |v: &Local| v.render_template(template, title),
            "render_template",
            json!([template, title])
        )
    }

    pub fn templates(&self) -> Result<Vec<String>> {
        ask!(self, |v: &Local| v.templates(), "templates", json!([]))
    }
}

// Index reads.
impl Vault {
    pub fn list_dir(&self, rel: &str) -> Result<Vec<FileRow>> {
        ask!(self, |v: &Local| v.list_dir(rel), "list_dir", json!([rel]))
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        include_ignored: bool,
    ) -> Result<Vec<SearchHit>> {
        ask!(
            self,
            |v: &Local| v.search(query, limit, include_ignored),
            "search",
            json!([query, limit, include_ignored])
        )
    }

    pub fn grep(
        &self,
        query: &str,
        options: Options,
        limit: usize,
        include_ignored: bool,
    ) -> Result<(Vec<Match>, usize)> {
        match &self.backend {
            Backend::Local(v) => v.grep(&search::pattern(query, options)?, limit, include_ignored),
            Backend::Remote(r) => r
                .call("grep", json!([query, options, limit, include_ignored]))
                .map_err(remote_err),
        }
    }

    /// [`Local::grep_unindexed`], and nothing at all when there is no room for a row.
    ///
    /// The Search pane fills its row budget from the index first and asks here for the remainder,
    /// so a query common enough to fill it asks for zero rows — which is most of what a query
    /// looks like while it is being typed. The walk costs 18 ms of the 55 ms this call takes on
    /// 10 000 dependency files, and a round trip on a remote vault, for an answer that can only
    /// be empty.
    pub fn grep_unindexed(
        &self,
        query: &str,
        options: Options,
        limit: usize,
    ) -> Result<Vec<Match>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        match &self.backend {
            Backend::Local(v) => v.grep_unindexed(&search::pattern(query, options)?, limit),
            Backend::Remote(r) => r
                .call("grep_unindexed", json!([query, options, limit]))
                .map_err(remote_err),
        }
    }

    pub fn tags(&self) -> Result<Vec<(String, i64)>> {
        ask!(self, |v: &Local| v.tags(), "tags", json!([]))
    }

    pub fn files_with_tag(&self, tag: &str) -> Result<Vec<FileRow>> {
        ask!(
            self,
            |v: &Local| v.files_with_tag(tag),
            "files_with_tag",
            json!([tag])
        )
    }

    pub fn backlinks(&self, rel: &str) -> Result<Vec<Backlink>> {
        ask!(
            self,
            |v: &Local| v.backlinks(rel),
            "backlinks",
            json!([rel])
        )
    }

    /// The note links that highlight a page of this PDF. Asked of the host on a remote vault,
    /// because that is where the notes and the index are.
    pub fn pdf_links(&self, rel: &str) -> Result<Vec<PdfLink>> {
        ask!(
            self,
            |v: &Local| v.pdf_links(rel),
            "pdf_links",
            json!([rel])
        )
    }

    pub fn note_paths(&self) -> Result<Vec<String>> {
        ask!(self, |v: &Local| v.note_paths(), "note_paths", json!([]))
    }

    pub fn file_paths(&self, include_ignored: bool) -> Result<Vec<String>> {
        ask!(
            self,
            |v: &Local| v.file_paths(include_ignored),
            "file_paths",
            json!([include_ignored])
        )
    }

    pub fn set_excluded(&self, entries: &[String]) -> Result<()> {
        ask!(
            self,
            |v: &Local| v.set_excluded(entries),
            "set_excluded",
            json!([entries])
        )
    }

    pub fn recent_notes(&self, limit: usize) -> Result<Vec<String>> {
        ask!(
            self,
            |v: &Local| v.recent_notes(limit),
            "recent_notes",
            json!([limit])
        )
    }

    pub fn resolve_link(&self, target: &str) -> Result<Option<String>> {
        ask!(
            self,
            |v: &Local| v.resolve_link(target),
            "resolve_link",
            json!([target])
        )
    }

    pub fn conflicts(&self) -> Result<Vec<(String, String)>> {
        ask!(self, |v: &Local| v.conflicts(), "conflicts", json!([]))
    }

    /// Which vault file an embed names, or `None` when nothing answers to it.
    ///
    /// `![[img.png]]` is written the way a wikilink is, so a basename on its own has to be found
    /// the way `[[note]]` is found — through the index, which knows the file lives in
    /// `Attachments/`. The path as written wins whenever it is really there, so an embed that
    /// spells the whole path never takes a second look. Here rather than in the preview because
    /// nothing about it is the desktop's: the same embeds render on Android.
    pub fn asset(&self, rel: &str) -> Option<String> {
        if self.exists(rel) {
            return Some(rel.to_string());
        }
        self.resolve_link(rel).ok().flatten()
    }

    pub fn conflicts_of(&self, rel: &str) -> Result<Vec<String>> {
        ask!(
            self,
            |v: &Local| v.conflicts_of(rel),
            "conflicts_of",
            json!([rel])
        )
    }
}

// Git. The repositories belong to the machine the files are on, so every one of these runs there
// — the `git` binary the user configured, with their hooks and their credential helper.
impl Vault {
    pub fn repos(&self) -> Vec<Repo> {
        match &self.backend {
            Backend::Local(v) => v.repos(),
            Backend::Remote(r) => r.call("repos", json!([])).unwrap_or_default(),
        }
    }

    pub fn git_status(&self, repo: &Repo) -> Result<Status> {
        ask!(
            self,
            |_: &Local| git::status(repo).map_err(anyhow::Error::from),
            "git_status",
            json!([repo])
        )
    }

    /// One page of history. The graph itself is computed where it is drawn: [`git::lanes`] is a
    /// forward pass over every commit so far, so the pane keeps the list and re-lanes it, and
    /// there is nothing in it for a remote host to do.
    pub fn git_log(&self, repo: &Repo, skip: usize, limit: usize) -> Result<Vec<Commit>> {
        ask!(
            self,
            |_: &Local| git::log(repo, skip, limit).map_err(anyhow::Error::from),
            "git_log",
            json!([repo, skip, limit])
        )
    }

    pub fn git_show(&self, repo: &Repo, rev: &str, path: &str) -> Result<Option<git::Blob>> {
        ask!(
            self,
            |_: &Local| git::show(repo, rev, path).map_err(anyhow::Error::from),
            "git_show",
            json!([repo, rev, path])
        )
    }

    pub fn git_changed_files(&self, repo: &Repo, oid: &str) -> Result<Vec<(char, String)>> {
        ask!(
            self,
            |_: &Local| git::changed_files(repo, oid).map_err(anyhow::Error::from),
            "git_changed_files",
            json!([repo, oid])
        )
    }

    pub fn git_submodules(&self, repo: &Repo) -> Result<Vec<Submodule>> {
        ask!(
            self,
            |_: &Local| git::submodules(repo).map_err(anyhow::Error::from),
            "git_submodules",
            json!([repo])
        )
    }

    pub fn git_branches(&self, repo: &Repo) -> Result<Vec<String>> {
        ask!(
            self,
            |_: &Local| git::branches(repo).map_err(anyhow::Error::from),
            "git_branches",
            json!([repo])
        )
    }

    pub fn git_checkout(&self, repo: &Repo, branch: &str) -> Result<()> {
        ask!(
            self,
            |_: &Local| git::checkout(repo, branch).map_err(anyhow::Error::from),
            "git_checkout",
            json!([repo, branch])
        )
    }

    pub fn git_checkout_commit(&self, repo: &Repo, oid: &str) -> Result<()> {
        ask!(
            self,
            |_: &Local| git::checkout_commit(repo, oid).map_err(anyhow::Error::from),
            "git_checkout_commit",
            json!([repo, oid])
        )
    }

    pub fn git_create_branch(&self, repo: &Repo, name: &str, checkout: bool) -> Result<()> {
        ask!(
            self,
            |_: &Local| git::create_branch(repo, name, checkout).map_err(anyhow::Error::from),
            "git_create_branch",
            json!([repo, name, checkout])
        )
    }

    pub fn git_delete_branch(&self, repo: &Repo, name: &str, force: bool) -> Result<()> {
        ask!(
            self,
            |_: &Local| git::delete_branch(repo, name, force).map_err(anyhow::Error::from),
            "git_delete_branch",
            json!([repo, name, force])
        )
    }

    pub fn git_commit(&self, repo: &Repo, message: &str, all: bool) -> Result<String> {
        ask!(
            self,
            |_: &Local| git::commit(repo, message, all).map_err(anyhow::Error::from),
            "git_commit",
            json!([repo, message, all])
        )
    }

    pub fn git_sync(&self, repo: &Repo) -> Result<String> {
        ask!(
            self,
            |_: &Local| git::sync(repo).map_err(anyhow::Error::from),
            "git_sync",
            json!([repo])
        )
    }

    /// Bring the remote-tracking refs up to date. Bounded by [`git::FETCH_TIMEOUT`], which is
    /// under [`rpc::DEADLINE`] so that a fetch on a remote vault answers rather than times out.
    pub fn git_fetch(&self, repo: &Repo) -> Result<String> {
        ask!(
            self,
            |_: &Local| git::fetch(repo).map_err(anyhow::Error::from),
            "git_fetch",
            json!([repo])
        )
    }

    /// The oids a pull would bring in. Asked only where [`git::Status`] says there are any.
    pub fn git_incoming(&self, repo: &Repo) -> Result<Vec<String>> {
        ask!(
            self,
            |_: &Local| git::incoming(repo).map_err(anyhow::Error::from),
            "git_incoming",
            json!([repo])
        )
    }

    pub fn git_stage(&self, repo: &Repo, paths: &[String]) -> Result<()> {
        let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
        ask!(
            self,
            |_: &Local| git::stage(repo, &borrowed).map_err(anyhow::Error::from),
            "git_stage",
            json!([repo, paths])
        )
    }

    pub fn git_unstage(&self, repo: &Repo, paths: &[String]) -> Result<()> {
        let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
        ask!(
            self,
            |_: &Local| git::unstage(repo, &borrowed).map_err(anyhow::Error::from),
            "git_unstage",
            json!([repo, paths])
        )
    }

    pub fn git_discard(&self, repo: &Repo, paths: &[String]) -> Result<()> {
        let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
        ask!(
            self,
            |_: &Local| git::discard(repo, &borrowed).map_err(anyhow::Error::from),
            "git_discard",
            json!([repo, paths])
        )
    }
}

/// Take a lock, ignoring poison.
///
/// A panic in one query must not take the whole vault down with it, so a poisoned lock is used
/// rather than propagated: everything behind one here is a cache the next query rebuilds.
pub(crate) fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One open vault: the index, the watcher, and the worker thread that owns both writers.
struct Local {
    root: PathBuf,
    /// The caller's connection. The mutex is not about contention (WAL readers never block):
    /// it is what makes `Local` `Send + Sync`, which uniffi will need in Phase 3.
    index: Mutex<Index>,
    /// A reader of its own for the sidebar's search, which runs on a worker thread. A regex scan
    /// of every note holds its connection for as long as it takes, and the main thread's
    /// `list_dir` and `backlinks` must never queue behind one on the same mutex.
    search: Mutex<Index>,
    cfg: Mutex<VaultConfig>,
    /// The language providers answering for the open documents.
    lang: std::sync::Arc<language::Languages>,
    tx: Sender<Msg>,
    worker: Option<JoinHandle<()>>,
}

// ---------------------------------------------------------------------- open

impl Local {
    /// Open `root` with its index in the shared cache directory.
    fn open(root: &Path, cfg: VaultConfig) -> Result<(Local, Receiver<Event>)> {
        let db = accent_core::index::default_db_path(root);
        Local::open_at(root, &db, cfg)
    }

    /// [`open`](Self::open) with an explicit index file, for tests and tooling.
    fn open_at(root: &Path, db: &Path, cfg: VaultConfig) -> Result<(Local, Receiver<Event>)> {
        // One spelling of the root for everything downstream: index paths, watcher events and
        // symlink targets are all compared against it.
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        // The reader opens first because it is the connection that may drop and recreate the
        // schema; the worker's must never see the database half-built.
        let index = Index::open(db)?;
        let search = Index::open(db)?;
        let writer = Index::open(db)?;

        let (tx, rx) = channel::<Msg>();
        let (events, event_rx) = channel::<Event>();
        // The providers send their diagnostics down the same channel the worker's events use.
        let lang = language::Languages::new(root.clone(), db.to_path_buf(), events.clone());
        let handle = worker::spawn(root.clone(), writer, rx, tx.clone(), events)?;

        Ok((
            Local {
                root,
                index: Mutex::new(index),
                search: Mutex::new(search),
                cfg: Mutex::new(cfg),
                lang,
                tx,
                worker: Some(handle),
            },
            event_rx,
        ))
    }

    /// The canonical vault root. Every `rel` this API takes or returns is relative to it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> VaultConfig {
        locked(&self.cfg).clone()
    }

    pub fn set_config(&self, cfg: VaultConfig) {
        *locked(&self.cfg) = cfg;
    }

    pub fn set_ghost(&self, on: bool) {
        self.lang.set_ghost(on);
    }

    /// Ask for a full walk: after a resume, or when the UI suspects it missed something.
    pub fn rescan(&self) {
        self.post(Msg::Rescan);
    }

    /// Join `rel` to the vault root, refusing anything that would land outside it. Every
    /// path-taking method goes through this: the GTK app sanitises its own input, but an
    /// `accent-target:` of `../Outside/x.md` arrives here straight from a template, and Phase 2's MCP
    /// server and Phase 3's Android bindings call the façade with whatever their caller said.
    ///
    /// The test is deliberately lexical and never `canonicalize`s: a vault links external
    /// directories in on purpose, so resolving would reject the very paths the walk indexed and
    /// a note reached through a directory symlink has to stay openable.
    pub fn resolve(&self, rel: &str) -> io::Result<PathBuf> {
        Local::join(&self.root, rel)
    }

    /// [`resolve`](Self::resolve) against any root, so a remote vault can do the same arithmetic
    /// with the root the server reported.
    pub fn join(root: &Path, rel: &str) -> io::Result<PathBuf> {
        let mut out = root.to_path_buf();
        for part in Path::new(rel).components() {
            match part {
                Component::Normal(name) => out.push(name),
                Component::CurDir => {}
                Component::ParentDir => {
                    // `..` may walk back down to the root, never past it.
                    out.pop();
                    if !out.starts_with(root) {
                        return Err(outside(rel));
                    }
                }
                // A root or a prefix component: that is not a vault-relative path at all.
                _ => return Err(outside(rel)),
            }
        }
        Ok(out)
    }

    fn index(&self) -> MutexGuard<'_, Index> {
        locked(&self.index)
    }

    fn searcher(&self) -> MutexGuard<'_, Index> {
        locked(&self.search)
    }

    fn post(&self, msg: Msg) {
        if self.tx.send(msg).is_err() {
            tracing::debug!("vault worker is gone; dropping an index update");
        }
    }
}

impl Drop for Local {
    /// Stop the worker before the vault goes away, so no thread outlives the window that opened it.
    ///
    /// ponytail: the join waits for whatever the worker is doing, and a cold reconcile of a large
    /// vault takes seconds. Give `reconcile` a cancellation flag if closing a window ever stalls.
    fn drop(&mut self) {
        // Before the worker, because a provider is still sending diagnostics down its channel.
        self.lang.shutdown();
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

// --------------------------------------------------------------------- files

impl Local {
    pub fn read(&self, rel: &str) -> io::Result<(String, Etag)> {
        fs::read_note(&self.resolve(rel)?)
    }

    /// Write a note, then tell the worker about it: the index is correct within a millisecond
    /// instead of a watcher debounce later, and the save never comes back as [`Event::FileChanged`].
    ///
    /// ponytail: the write runs on the calling thread, because an fsync of a note is well under a
    /// frame on an SSD. If a slow disk ever shows up, move the write to the worker and answer
    /// with an event.
    pub fn save(&self, rel: &str, text: &str, expected: Option<Etag>) -> Result<Etag, SaveError> {
        let etag = fs::write_note(&self.resolve(rel)?, text, expected)?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(etag)
    }

    /// Read any file as text, saying so when it is binary or too big to hold. What a tab opens
    /// with; [`read`](Self::read) is the note-shaped version the rename and conflict paths use.
    pub fn read_text(&self, rel: &str) -> io::Result<fs::Read> {
        fs::read_text(&self.resolve(rel)?)
    }

    /// The file's etag, or `None` when there is no file there. One `stat`, which is how a tab
    /// asks "did this change under me" and how the app asks "does this path exist".
    pub fn stat(&self, rel: &str) -> io::Result<Option<Etag>> {
        match Etag::of(&self.resolve(rel)?) {
            Ok(etag) => Ok(Some(etag)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Delete a file or a whole directory, permanently.
    ///
    /// The desktop trashes through `gio` instead and never calls this; it exists for a vault on
    /// another machine, where there is no session bus to ask and no trash to ask it about. The
    /// UI is what makes that difference visible, by confirming the way it already confirms a
    /// delete the trash could not take.
    pub fn delete(&self, rel: &str) -> io::Result<()> {
        let path = self.resolve(rel)?;
        match path.is_dir() {
            true => std::fs::remove_dir_all(&path)?,
            false => std::fs::remove_file(&path)?,
        }
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(())
    }

    /// A template's text, whether it is named by a vault-relative path or by a bare file name in
    /// the templates directory. Failing, it names both places it looked.
    fn read_template(&self, name: &str) -> Result<String> {
        // The config lock is taken before the filesystem, never inside the index lock.
        let tried = template::candidates(&self.config().templates_dir, name);
        for rel in &tried {
            let path = self.resolve(rel)?;
            if path.is_file() {
                let (text, _) =
                    fs::read_note(&path).with_context(|| format!("reading template {rel}"))?;
                return Ok(text);
            }
        }
        anyhow::bail!("no template {name}: looked at {}", tried.join(" and "))
    }

    /// A template's text as a note called `title` would get it: the body with every placeholder
    /// expanded, and the byte offsets of its `{{cursor}}` stops. What Insert Template puts at
    /// the caret, and what [`create_note`](Self::create_note) writes.
    pub fn render_template(&self, template: &str, title: &str) -> Result<(String, Vec<usize>)> {
        // Through `parse` first: `accent-target:` is accent's directive, not the note's text.
        Ok(template::render(
            &template::parse(&self.read_template(template)?).body,
            title,
            chrono::Local::now().naive_local(),
        ))
    }

    /// Create a file, optionally from a template. Returns the path it was created at and where
    /// the caret belongs. The name is taken as it is given: `notes` is a file called `notes`, not
    /// a note called `notes.md`. Callers that mean markdown say so (`note_from_template` does).
    pub fn create_note(&self, rel: &str, template: Option<&str>) -> Result<(String, Vec<usize>)> {
        let rel = rel.to_string();
        let (text, cursor) = match template {
            Some(t) => self.render_template(t, &stem(&rel))?,
            None => (String::new(), Vec::new()),
        };
        fs::create_note(&self.resolve(&rel)?, &text).with_context(|| format!("creating {rel}"))?;
        self.post(Msg::Update {
            rel: rel.clone(),
            own: true,
        });
        Ok((rel, cursor))
    }

    pub fn create_dir(&self, rel: &str) -> io::Result<()> {
        std::fs::create_dir_all(self.resolve(rel)?)?;
        // ponytail: only the deepest component is indexed straight away; intermediate levels of a
        // nested path wait for the watcher or the next reconcile. Post one update per component
        // if the tree ever looks wrong right after "New folder".
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(())
    }

    /// What a rename would touch, so the UI can show it before anything is written.
    ///
    /// `rewrites` is empty for a directory, and for a pure move: a wikilink resolves by basename,
    /// so a note that keeps its name keeps its links wherever it lands.
    pub fn plan_rename(&self, from: &str, to: &str) -> Result<RenamePlan> {
        let renamed = stem_key(from) != stem_key(to);
        let mut rewrites = Vec::new();
        if renamed && !self.resolve(from)?.is_dir() {
            rewrites = self
                .index()
                .backlinks(from)?
                .into_iter()
                .map(|b| b.src_rel_path)
                .collect();
            rewrites.sort();
            rewrites.dedup();
        }
        Ok(RenamePlan {
            from: from.to_string(),
            to: to.to_string(),
            rewrites,
        })
    }

    /// Apply a [`RenamePlan`]. The move happens first and is the only step that may fail the
    /// call: a note whose links could not be rewritten is reported instead, because a
    /// half-renamed vault is worse than a fully renamed one with a couple of stale links.
    pub fn rename(&self, plan: &RenamePlan, rewrite_links: bool) -> Result<RenameReport> {
        // Which spellings of the old name really mean this note is a question only the index can
        // answer, and only while it still describes the vault as it was: ask before the move.
        let targets = if rewrite_links {
            self.targets_of(&plan.from)?
        } else {
            Vec::new()
        };
        fs::rename(&self.resolve(&plan.from)?, &self.resolve(&plan.to)?)
            .with_context(|| format!("renaming {} to {}", plan.from, plan.to))?;
        // Both ends, so the index and the tree do not wait for inotify.
        self.post(Msg::Update {
            rel: plan.from.clone(),
            own: true,
        });
        self.post(Msg::Update {
            rel: plan.to.clone(),
            own: true,
        });

        let mut report = RenameReport::default();
        if rewrite_links {
            for rel in &plan.rewrites {
                match self.rewrite_one(rel, &targets, &plan.to) {
                    Ok(true) => report.rewritten.push(rel.clone()),
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!("rewriting links in {rel}: {e:#}");
                        report.failed.push((rel.clone(), format!("{e:#}")));
                    }
                }
            }
        }
        Ok(report)
    }

    /// The ways of writing `rel` that really resolve to it. `[[Old]]` in a note that also
    /// links `[[Dir/Old]]` may well be a different note's, and renaming this one must leave that
    /// link exactly as its author wrote it.
    fn targets_of(&self, rel: &str) -> Result<Vec<String>> {
        let index = self.index();
        let mut out = Vec::new();
        for key in markdown::path_keys(rel) {
            if index.resolve_target(&key)?.as_deref() == Some(rel) {
                out.push(key);
            }
        }
        Ok(out)
    }

    /// `Ok(false)` when the note turned out to link nowhere near the renamed file.
    fn rewrite_one(&self, rel: &str, targets: &[String], to: &str) -> Result<bool> {
        let path = self.resolve(rel)?;
        let (text, etag) = fs::read_note(&path)?;
        let Some(rewritten) = markdown::rewrite_targets(&text, targets, to) else {
            return Ok(false);
        };
        fs::write_note(&path, &rewritten, Some(etag))?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(true)
    }

    /// Replace every match of `re` in every note that has one.
    ///
    /// `literal` takes `$1` in `replacement` as two characters rather than a capture group, which
    /// is what the sidebar's non-regex modes mean. Same shape as [`rename`](Self::rename): a note
    /// that could not be written is reported rather than fatal, because a vault where most of the
    /// replacements landed is a real outcome the user has to be told about.
    ///
    /// It reads, substitutes and fsyncs one note at a time on the calling thread, which costs
    /// far more than a main loop can spend: 1.9 s across 245 notes and 35 s across 3.3k of them,
    /// measured on the 3.6k-note generated vault. Callers with a UI run it on a worker thread —
    /// the desktop app does, and the handle is `Send + Sync` so a binding can too.
    ///
    /// ponytail: no progress callback. The one caller shows an indeterminate bar, and a fraction
    /// nothing renders would be machinery for its own sake. Nor is it undoable — see NOTEPAD.
    pub fn replace_all(
        &self,
        re: &Regex,
        replacement: &str,
        literal: bool,
    ) -> Result<ReplaceReport> {
        let mut report = ReplaceReport::default();
        // Collected before the first write: the guard must not still be held while notes are
        // rewritten, and the worker reindexes them as they land.
        for rel in self.searcher().grep_paths(re)? {
            match self.replace_one(&rel, re, replacement, literal) {
                Ok(0) => {}
                Ok(n) => {
                    report.rewritten.push(rel);
                    report.matches += n;
                }
                Err(e) => {
                    tracing::warn!("replacing in {rel}: {e:#}");
                    report.failed.push((rel, format!("{e:#}")));
                }
            }
        }
        Ok(report)
    }

    /// How many matches this note lost. The count comes from the file rather than from the index,
    /// which may be a watcher debounce behind what is on disk.
    fn replace_one(
        &self,
        rel: &str,
        re: &Regex,
        replacement: &str,
        literal: bool,
    ) -> Result<usize> {
        let path = self.resolve(rel)?;
        let (text, etag) = fs::read_note(&path)?;
        let matches = re.find_iter(&text).count();
        if matches == 0 {
            return Ok(0);
        }
        let rewritten = match literal {
            true => re.replace_all(&text, search::NoExpand(replacement)),
            false => re.replace_all(&text, replacement),
        };
        fs::write_note(&path, &rewritten, Some(etag))?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        Ok(matches)
    }

    /// Keep theirs: the conflict copy's bytes replace the original.
    ///
    /// This is a force-write, not a gated one — the caller has already seen the diff and chosen.
    /// So that the choice stays undoable, what the original held is copied to a sibling
    /// `*.sync-conflict-<timestamp>-accent.md` first: a name the vault keeps out of the note
    /// index and the resolve dialog offers back. The copy that won stays on disk too, because
    /// deleting it belongs in the system trash. Keeping mine needs no call here at all, it is
    /// just trashing the copy.
    pub fn adopt_conflict(&self, original: &str, conflict: &str) -> Result<Etag> {
        let (text, _) = fs::read_note(&self.resolve(conflict)?)
            .with_context(|| format!("reading {conflict}"))?;
        let target = self.resolve(original)?;
        let keep = target.with_file_name(accent_conflict_name(
            basename(original),
            chrono::Local::now().naive_local(),
        ));
        // ponytail: a second adopt within the same second overwrites the first copy, because the
        // name only carries whole seconds. Add a counter suffix if that ever costs anyone a note.
        std::fs::copy(&target, &keep)
            .with_context(|| format!("keeping {original} as {}", keep.display()))?;
        let etag = Etag::of(&target).with_context(|| format!("stat {original}"))?;
        let written = fs::write_note(&target, &text, Some(etag))
            .with_context(|| format!("writing {original}"))?;
        self.post(Msg::Update {
            rel: original.to_string(),
            own: true,
        });
        Ok(written)
    }

    /// Side-by-side rows for the conflict UI: mine on the left, theirs on the right.
    pub fn conflict_diff(&self, original: &str, conflict: &str) -> Result<Vec<DiffLine>> {
        let (mine, _) = fs::read_note(&self.resolve(original)?)
            .with_context(|| format!("reading {original}"))?;
        let (theirs, _) = fs::read_note(&self.resolve(conflict)?)
            .with_context(|| format!("reading {conflict}"))?;
        Ok(diff::lines(&mine, &theirs))
    }

    /// Where a template says its notes go today, or `None` when it does not say.
    ///
    /// The `accent-target:` directive goes through the same renderer the body does, so
    /// `Daily/{{date:%Y-%m-%d}}.md` is today's daily note. `{{title}}` in a target is the
    /// template's own name: there is no note yet to take one from.
    pub fn template_target(&self, template: &str) -> Result<Option<String>> {
        let Some(target) = template::parse(&self.read_template(template)?).target else {
            return Ok(None);
        };
        let (rel, _) =
            template::render(&target, &stem(template), chrono::Local::now().naive_local());
        Ok(Some(with_md(&rel)))
    }

    /// Create the note a template names, or open the one it already made.
    ///
    /// Invoked twice on the same day, a dated target hands back the note it made the first time,
    /// byte for byte: that is what makes a daily note daily. `None` is a template that says
    /// nothing about where its notes go, which is the caller's cue that it needs a name.
    pub fn note_from_template(&self, template: &str) -> Result<Option<(String, Vec<usize>)>> {
        let Some(rel) = self.template_target(template)? else {
            return Ok(None);
        };
        if self.resolve(&rel)?.exists() {
            return Ok(Some((rel, Vec::new())));
        }
        self.create_note(&rel, Some(template)).map(Some)
    }

    /// The markdown files directly inside the configured templates directory.
    pub fn templates(&self) -> Result<Vec<String>> {
        // The config lock is taken before the index lock, never inside it.
        let dir = self.config().templates_dir;
        Ok(self
            .index()
            .list_files(&dir)?
            .into_iter()
            .filter(|f| f.kind == FileKind::Markdown)
            .map(|f| f.rel_path)
            .collect())
    }
}

// -------------------------------------------------------------- index reads

impl Local {
    /// Direct children of one directory ("" is the vault root): one level per call, so the tree
    /// costs what it shows.
    ///
    /// The index's own rows, plus the trees it deliberately does not hold — `node_modules`, a
    /// `.venv`, a cargo `target/` — read straight off the disk
    /// ([`walk::unindexed_children`]) and merged in, so the file tree can show every folder in
    /// the vault without any of them being indexed, watched or searched. Those rows carry
    /// `id == 0`, which is what tells them apart. It happens here rather than in the tree so
    /// that a vault on another machine gets it too: this runs on the host holding the files.
    pub fn list_dir(&self, rel: &str) -> Result<Vec<FileRow>> {
        let mut rows = self.index().list_files(rel)?;
        let held: HashSet<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
        // Non-fatal: a directory that vanished mid-listing must not blank the rows the index did
        // answer for.
        let extra = walk::unindexed_children(&self.root, rel, &held).unwrap_or_else(|e| {
            tracing::debug!(dir = rel, "listing the unindexed children: {e}");
            Vec::new()
        });
        if extra.is_empty() {
            return Ok(rows);
        }
        rows.extend(extra.into_iter().map(|(rel_path, kind)| FileRow {
            id: 0,
            rel_path,
            kind,
            title: None,
            size: 0,
            mtime_ns: 0,
        }));
        // `Index::list_files` orders directories first and then by path, case-insensitively; the
        // merged listing has to come out the same way or the disk rows would land in a block of
        // their own at the end. `sort_by_cached_key` folds each path once rather than per compare.
        rows.sort_by_cached_key(|r| (r.kind != FileKind::Dir, r.rel_path.to_ascii_lowercase()));
        Ok(rows)
    }

    /// Ranked full-text search. On the search connection, so a slow query cannot block the tree.
    ///
    /// `include_ignored` is the sidebar's All toggle: off, what git ignores is left out of the
    /// results; on, it is put back. A note is in either way.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        include_ignored: bool,
    ) -> Result<Vec<SearchHit>> {
        self.searcher().search(query, limit, include_ignored)
    }

    /// Exact search: one row per match of `re`, capped at `limit`, plus how many of them a
    /// [`replace_all`](Self::replace_all) would rewrite — markdown only, since that is all it
    /// visits. `include_ignored` means what it does in [`search`](Self::search).
    pub fn grep(
        &self,
        re: &Regex,
        limit: usize,
        include_ignored: bool,
    ) -> Result<(Vec<Match>, usize)> {
        self.searcher().grep(re, limit, include_ignored)
    }

    /// The same exact search over the files the index does not hold at all: those under a
    /// dependency tree, a `node_modules` or a `target/` the walk deliberately never entered.
    ///
    /// This is the second half of the Search pane's All toggle, and only the exact-match path
    /// runs it. The first half drops the git-ignored exclusion, which is a column in the index and
    /// which ranked search reads as well; this one reaches what was never indexed, and it can only
    /// be a walk, so a ranked query has no way to fold it in. Everything already in the index is
    /// skipped by path, so no file is greped twice, and the walk stops as soon as `limit` matches
    /// are in hand.
    ///
    /// The matching runs **inside** the walk ([`walk::visit`]), on its threads, rather than over
    /// a [`walk::ScanResult`] it built first: reading 10 000 dependency files one at a time was
    /// most of what a settled query cost. The row budget is therefore shared — an atomic every
    /// thread reads before it opens a file and writes when it has appended — so a query whose
    /// rows fill early stops the walk instead of finishing it. Nothing is read on the index's
    /// connection: the guard is dropped before the walk starts, and the caller is the sidebar's
    /// search worker either way. `.git` and `.trash` stay unreachable, and so does a symlinked
    /// repository's own gitignored build output: that is somebody else's build tree, and leaving
    /// it out is what keeps a per-query walk affordable.
    ///
    /// The rows are sorted by path before they are returned, because the walk answers in whatever
    /// order its threads got there and the reader is looking at a list. *Which* rows survive a
    /// full budget is no longer deterministic — the threads race for it — and cannot be: that is
    /// the price of not reading every file, and the pane already says the list is capped.
    ///
    /// Rows only, no count beside them: nothing here can be rewritten by Replace All, which
    /// visits the indexed notes, so a number of matches past `limit` would have no reader.
    ///
    /// ponytail: the walk runs per query, with no cache, for as long as All is on.
    pub fn grep_unindexed(&self, re: &Regex, limit: usize) -> Result<Vec<Match>> {
        // Collected before the walk: the guard must not be held across file I/O.
        let known: HashSet<String> = self.searcher().file_paths(true)?.into_iter().collect();
        let opts = walk::ScanOptions {
            include_skipped: true,
            skip_dependency_trees: false,
            // Inside the vault, what git ignores is already indexed, so the walk can skip it and
            // the `known` test would have dropped it anyway.
            vault_gitignore: true,
            target_gitignore: true,
            ..walk::ScanOptions::default()
        };
        let out: Mutex<Vec<Match>> = Mutex::new(Vec::new());
        // How many rows are in `out`. Read without the lock, so a file that matches nothing —
        // which is nearly all of them — never touches it at all.
        let found = AtomicUsize::new(0);
        walk::visit(&self.root, &opts, &|f| {
            if found.load(Ordering::Relaxed) >= limit {
                return false;
            }
            if f.kind == FileKind::Dir || known.contains(&f.rel_path) {
                return true;
            }
            let (mut rows, mut seen) = (Vec::new(), 0usize);
            match fs::read_text(&f.canonical) {
                Ok(fs::Read::Text(t)) if !t.lossy => {
                    Index::matches_in(&f.rel_path, None, &t.text, re, limit, &mut rows, &mut seen);
                }
                Ok(_) => {}
                // A file that vanished or cannot be read is not the query's problem.
                Err(e) => tracing::debug!("grep skipped {}: {e}", f.rel_path),
            }
            if rows.is_empty() {
                return true;
            }
            let mut out = locked(&out);
            out.extend(rows);
            found.store(out.len(), Ordering::Relaxed);
            out.len() < limit
        });
        let mut out = out.into_inner().unwrap_or_else(|e| e.into_inner());
        // Two threads can overshoot the budget between the load and the store; the extra rows
        // are real matches, but the pane asked for `limit` of them.
        out.sort_unstable_by(|a, b| (&a.rel_path, a.line).cmp(&(&b.rel_path, b.line)));
        out.truncate(limit);
        Ok(out)
    }

    pub fn tags(&self) -> Result<Vec<(String, i64)>> {
        self.index().tags()
    }

    pub fn files_with_tag(&self, tag: &str) -> Result<Vec<FileRow>> {
        self.index().files_with_tag(tag)
    }

    pub fn backlinks(&self, rel: &str) -> Result<Vec<Backlink>> {
        self.index().backlinks(rel)
    }

    fn pdf_links(&self, rel: &str) -> Result<Vec<PdfLink>> {
        self.index().pdf_links(rel)
    }

    pub fn note_paths(&self) -> Result<Vec<String>> {
        self.index().note_paths()
    }

    /// Every file the app can open, notes first: what the palette's switcher lists, now that a
    /// tab is not necessarily a note. [`note_paths`](Self::note_paths) stays markdown-only,
    /// because `[[` completion may only offer notes.
    ///
    /// The palette has no All toggle of its own, so it asks with `include_ignored` false and the
    /// build output stays out of Go to File. The tree still lists an ignored file, dimmed, which
    /// is the way to open one.
    pub fn file_paths(&self, include_ignored: bool) -> Result<Vec<String>> {
        self.index().file_paths(include_ignored)
    }

    /// Hand the index what search leaves out, so every later query can leave it out.
    ///
    /// Called from the git refresh, which is the one place in the app that has already asked git
    /// and where the `[search] exclude` list joins git's answer. The write goes to the vault
    /// worker rather than to the caller's connection, because the worker owns the only writing
    /// one: on the caller's it had to wait for the worker's write lock while holding the mutex
    /// the main thread reads through, and `list_dir` queued behind it. Measured on the 40k-entry
    /// test vault, cold: a read waited 5 006 ms and the write then failed outright with "database
    /// is locked", the busy handler having been starved by a reconcile that takes the write lock
    /// back between every batch.
    ///
    /// It still returns only once the write has landed — the worker answers on `reply` — because
    /// the caller re-runs the query on screen the moment it does. What the caller now waits for is
    /// the worker reaching this message, which during a reconcile is the reconcile: a wait, but
    /// never one a reader is behind, and one that ends in the write actually happening.
    pub fn set_excluded(&self, entries: &[String]) -> Result<()> {
        let (reply, answer) = channel();
        self.tx
            .send(Msg::SetExcluded(entries.to_vec(), reply))
            .map_err(|_| anyhow::anyhow!("the vault worker is gone"))?;
        answer
            .recv()
            .context("the vault worker stopped before it recorded the exclusion set")?
    }

    pub fn recent_notes(&self, limit: usize) -> Result<Vec<String>> {
        self.index().recent_notes(limit)
    }

    /// The note a wikilink target points at, or `None` when it dangles and the UI can offer to
    /// create it.
    pub fn resolve_link(&self, target: &str) -> Result<Option<String>> {
        self.index().resolve_target(target)
    }

    /// `(original, conflict copy)` for every `*.sync-conflict-*` file whose original still exists.
    /// A copy of a note that has since been deleted is nothing the resolve UI can act on.
    pub fn conflicts(&self) -> Result<Vec<(String, String)>> {
        conflict_pairs(&self.index())
    }

    /// The conflict copies of one note, for the banner a tab raises over it.
    pub fn conflicts_of(&self, rel: &str) -> Result<Vec<String>> {
        Ok(conflict_pairs(&self.index())?
            .into_iter()
            .filter(|(original, _)| original == rel)
            .map(|(_, copy)| copy)
            .collect())
    }
}

// ----------------------------------------------------------------------- git

impl Local {
    /// The repositories the vault touches: the one holding the root, plus every indexed directory
    /// carrying a `.git` entry. Runs on the caller's thread, which is never the main one.
    ///
    /// The walk hard-skips `.git`, so a repository is only ever found by the directory holding it.
    pub fn repos(&self) -> Vec<Repo> {
        // The searcher's connection, not the main thread's: discovery spawns a `git` process per
        // candidate directory and must not hold the lock the UI reads through.
        let dirs = self.searcher().dirs(&self.root).unwrap_or_else(|e| {
            tracing::debug!("listing vault directories for git discovery: {e}");
            Vec::new()
        });
        let repos = git::discover(&self.root, &dirs);
        // The watcher learns the repositories from here rather than finding them itself: this is
        // the only place that knows them, it already runs off the main thread, and the git pane
        // calls it whenever the set could have changed.
        self.post(Msg::WatchGit(
            repos.iter().map(|r| r.git_dir.clone()).collect(),
        ));
        repos
    }
}

#[cfg(test)]
mod facade_tests {
    use super::*;
    use crate::tests::*;

    #[test]
    fn vault_is_send_and_sync() {
        fn assert<T: Send + Sync>() {}
        assert::<Vault>();
    }

    #[test]
    fn open_reconciles_and_lists_the_root() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "hello");
        f.write("sub/Deep.md", "deep");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        assert_eq!(
            names(&f.vault.list_dir("").unwrap()),
            ["sub", "Note.md"],
            "directories first, then files"
        );
        assert_eq!(names(&f.vault.list_dir("sub").unwrap()), ["sub/Deep.md"]);
    }

    #[test]
    fn list_dir_merges_the_trees_the_index_does_not_hold() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "hello");
        f.write("node_modules/pkg/index.js", "js");
        f.write("apples/a.md", "a");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let root = f.vault.list_dir("").unwrap();
        assert_eq!(
            names(&root),
            ["apples", "node_modules", "Note.md"],
            "the skipped tree sorts among the indexed rows, not after them"
        );
        // Which of them is in the index is what the file tree reads to decide whether a row may
        // be renamed, moved or dropped onto.
        let id = |rel: &str| root.iter().find(|r| r.rel_path == rel).unwrap().id;
        assert_eq!(id("node_modules"), 0);
        assert!(id("apples") > 0);
        // Its contents come from the disk, one level at a time.
        assert_eq!(
            names(&f.vault.list_dir("node_modules").unwrap()),
            ["node_modules/pkg"]
        );
        assert_eq!(
            names(&f.vault.list_dir("node_modules/pkg").unwrap()),
            ["node_modules/pkg/index.js"]
        );
    }

    /// `a.md` links to `B.md`; renaming it to `C.md` must move the link with it.
    fn linked_vault() -> Fixture {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "see [[B]] for details\n");
        f.write("B.md", "the target\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        f
    }

    #[test]
    fn rename_rewrites_wikilinks_and_backlinks_follow() {
        let f = linked_vault();

        let plan = f.vault.plan_rename("B.md", "C.md").unwrap();
        assert_eq!(plan.rewrites, ["a.md"]);

        let report = f.vault.rename(&plan, true).unwrap();
        assert_eq!(report.rewritten, ["a.md"]);
        assert!(report.failed.is_empty());
        assert_eq!(f.read("a.md"), "see [[C]] for details\n");

        assert!(poll_until(
            || f.vault
                .backlinks("C.md")
                .unwrap()
                .iter()
                .any(|b| b.src_rel_path == "a.md"),
            BUDGET
        ));
    }

    /// Replacing across a vault is minutes of fsyncs, so the desktop app runs it on a worker
    /// thread. That only compiles while the handle can cross one.
    #[test]
    fn a_vault_handle_can_cross_a_thread() {
        fn crosses<T: Send + Sync>() {}
        crosses::<Vault>();
    }

    #[test]
    fn replace_all_rewrites_every_match_and_reindexes() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "colour and colour\n");
        f.write("sub/b.md", "Colour\n");
        f.write("c.md", "nothing here\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        assert_eq!(f.vault.grep("colour", plain, 10, false).unwrap().1, 3);

        let report = f.vault.replace_all("colour", plain, "color", true).unwrap();
        assert_eq!(report.rewritten, ["a.md", "sub/b.md"]);
        assert_eq!(report.matches, 3);
        assert!(report.failed.is_empty());
        assert_eq!(f.read("a.md"), "color and color\n");
        assert_eq!(f.read("sub/b.md"), "color\n", "case-insensitive by default");
        assert_eq!(f.read("c.md"), "nothing here\n");

        assert!(
            poll_until(
                || f.vault.grep("colour", plain, 10, false).unwrap().1 == 0,
                BUDGET
            ),
            "the rewrites must reach the index without a rescan"
        );
    }

    /// The other half of All: a tree the index never walked is greped from disk, and a file the
    /// index does hold is not greped twice.
    #[test]
    fn grep_unindexed_reaches_the_trees_the_walk_skipped() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "zorblat in a note\n");
        std::fs::create_dir_all(f.vault.root().join("node_modules")).unwrap();
        f.write("node_modules/dep.js", "// zorblat\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        // The index never walked node_modules, so its own grep cannot see the dependency.
        assert_eq!(f.vault.grep("zorblat", plain, 10, true).unwrap().1, 1);

        let hits = f.vault.grep_unindexed("zorblat", plain, 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "node_modules/dep.js");
        assert!(
            !hits.iter().any(|h| h.rel_path == "a.md"),
            "an indexed note must not be greped a second time: {hits:?}"
        );
    }

    /// The row budget is shared by the walking threads, so a cap is a cap however many of them
    /// matched at once, and the rows come back in path order rather than in finishing order.
    #[test]
    fn grep_unindexed_honours_the_row_budget_and_answers_in_path_order() {
        let f = Fixture::open(VaultConfig::default());
        for i in 0..50 {
            f.write(&format!("node_modules/pkg{i:02}/dep.js"), "// zorblat\n");
        }
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        let hits = f.vault.grep_unindexed("zorblat", plain, 7).unwrap();
        assert_eq!(hits.len(), 7, "{hits:?}");
        let paths: Vec<&str> = hits.iter().map(|h| h.rel_path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "rows must be ordered for the reader");
    }

    /// The exclusion set is written by the vault worker, and the call still means it is written:
    /// the next query must already leave the excluded file out.
    #[test]
    fn set_excluded_has_landed_when_it_returns() {
        let f = Fixture::open(VaultConfig::default());
        f.write("keep.txt", "keep");
        // Not a note: a note is listed whether or not git ignores it.
        f.write("build/out.txt", "out");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        f.vault.set_excluded(&["build/".to_string()]).unwrap();
        let paths = f.vault.file_paths(false).unwrap();
        assert!(paths.contains(&"keep.txt".to_string()), "{paths:?}");
        assert!(!paths.contains(&"build/out.txt".to_string()), "{paths:?}");

        f.vault.set_excluded(&[]).unwrap();
        assert!(
            f.vault
                .file_paths(false)
                .unwrap()
                .contains(&"build/out.txt".to_string())
        );
    }

    /// Bodies outside the notes are in the index now, so the one grep reaches them.
    #[test]
    fn grep_reaches_text_outside_notes() {
        let f = Fixture::open(VaultConfig::default());
        f.write("tool.py", "import os\nprint('zorblat')\n");
        f.write("bin.dat", "\0zorblat\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let (hits, total) = f
            .vault
            .grep("zorblat", Options::default(), 10, false)
            .unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "tool.py");
        assert_eq!(hits[0].line, 2);
        // Listed, but not counted: the count is what Replace All would rewrite, and it rewrites
        // notes. (The NUL byte is what keeps bin.dat out of the rows.)
        assert_eq!(total, 0);
    }

    /// The Search pane's "Replace All (N)": N is what the rewrite touches, not what the list
    /// shows. A source file's matches are rows without being edits.
    #[test]
    fn the_replace_count_is_notes_while_the_rows_are_every_text_file() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "zorblat once\n");
        f.write("tool.py", "zorblat\nzorblat again\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plain = Options::default();
        let (hits, total) = f.vault.grep("zorblat", plain, 10, false).unwrap();
        assert_eq!(hits.len(), 3, "the list shows both files: {hits:?}");
        assert_eq!(total, 1, "only the note's match is a rewrite");
        // And that is exactly what the rewrite then visits.
        let report = f.vault.replace_all("zorblat", plain, "zzz", true).unwrap();
        assert_eq!(report.rewritten, vec!["a.md".to_string()]);
        assert_eq!(report.matches, 1);
        assert_eq!(f.read("tool.py"), "zorblat\nzorblat again\n");
    }

    /// Only regex mode expands `$1`; a literal replacement is written as typed.
    #[test]
    fn replace_expands_groups_only_outside_literal_mode() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "hello world\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let opts = Options {
            regex: true,
            ..Options::default()
        };
        f.vault
            .replace_all(r"hello (\w+)", opts, "bye $1", false)
            .unwrap();
        assert_eq!(f.read("a.md"), "bye world\n");
    }

    #[test]
    fn rename_without_rewrite_leaves_links_alone() {
        let f = linked_vault();

        let plan = f.vault.plan_rename("B.md", "C.md").unwrap();
        let report = f.vault.rename(&plan, false).unwrap();

        assert!(report.rewritten.is_empty());
        assert_eq!(f.read("a.md"), "see [[B]] for details\n");
        assert!(f.vault.root().join("C.md").exists());
    }

    #[test]
    fn a_pure_move_rewrites_nothing() {
        let f = linked_vault();
        assert!(
            f.vault
                .plan_rename("B.md", "sub/B.md")
                .unwrap()
                .rewrites
                .is_empty(),
            "a note that keeps its name keeps its links"
        );
    }

    #[test]
    fn create_note_from_a_template_expands_it_and_returns_the_cursor() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Templates/Note.md", "# {{title}}\n\n{{cursor}}body\n");

        let (rel, cursor) = f
            .vault
            .create_note("Inbox/Weekly sync.md", Some("Templates/Note.md"))
            .unwrap();

        assert_eq!(rel, "Inbox/Weekly sync.md");
        let text = f.read(&rel);
        assert_eq!(text, "# Weekly sync\n\nbody\n");
        assert_eq!(&text[cursor[0]..], "body\n");

        // Once indexed, the same file is what the "new note from template" dialog offers.
        assert!(poll_until(
            || f.vault.templates().unwrap() == ["Templates/Note.md"],
            BUDGET
        ));
    }

    /// The daily note, now that it is only a template with a dated target: created the first
    /// time it is asked for and opened untouched every time after, which is the whole behaviour.
    #[test]
    fn a_dated_target_is_created_once_and_reused() {
        let f = Fixture::open(VaultConfig::default());
        f.write(
            "Templates/Daily.md",
            "---\naccent-target: Daily/{{date:%Y-%m-%d}}.md\n---\n\n# {{date}}\n\n{{cursor}}",
        );

        let (rel, cursor) = f.vault.note_from_template("Daily.md").unwrap().unwrap();
        assert!(rel.starts_with("Daily/") && rel.ends_with(".md"), "{rel}");
        assert!(!cursor.is_empty(), "a fresh note places the caret");
        let text = f.read(&rel);
        assert!(
            !text.contains("accent-target"),
            "the directive is accent's: {text}"
        );

        let (again, cursor) = f.vault.note_from_template("Daily.md").unwrap().unwrap();
        assert_eq!(again, rel);
        assert!(
            cursor.is_empty(),
            "an existing note is opened, not rewritten"
        );
        assert_eq!(f.read(&rel), text);
    }

    #[test]
    fn a_template_that_names_no_target_creates_nothing() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Templates/Meeting.md", "# {{title}}\n\nbody\n");

        assert_eq!(f.vault.template_target("Meeting.md").unwrap(), None);
        assert_eq!(f.vault.note_from_template("Meeting.md").unwrap(), None);
    }

    #[test]
    fn a_target_is_rendered_and_made_markdown() {
        let f = Fixture::open(VaultConfig::default());
        f.write(
            "Templates/Weekly.md",
            "---\naccent-target: Logs/{{title}}\n---\n\nx\n",
        );

        // `{{title}}` is the template's own stem, and a target without `.md` is still a note.
        assert_eq!(
            f.vault.template_target("Weekly.md").unwrap().as_deref(),
            Some("Logs/Weekly.md")
        );
    }

    #[test]
    fn adopt_conflict_replaces_the_original_and_leaves_the_copy() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine\n");
        f.write(CONFLICT, "theirs\n");

        let etag = f.vault.adopt_conflict("Note.md", CONFLICT).unwrap();

        assert_eq!(f.read("Note.md"), "theirs\n");
        assert_eq!(etag, Etag::of(&f.vault.root().join("Note.md")).unwrap());
        assert!(
            f.vault.root().join(CONFLICT).exists(),
            "deleting the copy is the UI's job, through the trash"
        );
    }

    #[test]
    fn resolve_link_matches_a_stem_case_insensitively() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Sub/Meeting Notes.md", "hello");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        assert_eq!(
            f.vault.resolve_link("meeting notes").unwrap().as_deref(),
            Some("Sub/Meeting Notes.md")
        );
        assert_eq!(f.vault.resolve_link("nothing here").unwrap(), None);
    }

    #[test]
    fn an_asset_is_found_by_its_basename() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Attachments/img.png", "not really a png");
        f.write("Note.md", "![[img.png]]\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        // What the preview is handed for `![[img.png]]`: a basename, and the file is elsewhere.
        assert_eq!(
            f.vault.asset("img.png").as_deref(),
            Some("Attachments/img.png")
        );
        // A path that is really there is taken as written.
        assert_eq!(
            f.vault.asset("Attachments/img.png").as_deref(),
            Some("Attachments/img.png")
        );
        assert_eq!(f.vault.asset("nowhere.png"), None);
    }

    #[test]
    fn conflict_diff_reports_the_changed_line() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "alpha\nbravo\n");
        f.write(CONFLICT, "alpha\nbravo two\n");

        let d = f.vault.conflict_diff("Note.md", CONFLICT).unwrap();

        assert_eq!(
            d.iter()
                .filter(|l| l.op != Op::Equal)
                .map(|l| (l.op, l.text.as_str()))
                .collect::<Vec<_>>(),
            [(Op::Delete, "bravo"), (Op::Insert, "bravo two")]
        );
    }

    /// The notes provider through the façade: what the editor sees when a note is opened.
    ///
    /// `block_on` inside the body and never around the fixture: dropping the vault stops the
    /// providers by blocking on the same runtime, which a runtime thread may not do.
    #[test]
    fn notes_provider_completes_and_diagnoses() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "see [[Beta]] and [[Nope]] #rust\n");
        f.write("sub/Beta.md", "# Beta\nbody\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let rt = accent_lsp::runtime();
        let support = rt.block_on(f.vault.open_document(
            "a.md",
            "markdown",
            "see [[Beta]] and [[Nope]] #rust\n".to_string(),
        ));
        assert_eq!(support.unwrap().completion_triggers, ['[', '#']);

        let Some(Event::Diagnostics { rel, items }) =
            f.wait(|e| matches!(e, Event::Diagnostics { .. }))
        else {
            panic!("opening a note has to say what is wrong with it");
        };
        assert_eq!(rel, "a.md");
        assert_eq!(items.len(), 1, "[[Beta]] resolves, [[Nope]] does not");
        assert_eq!(items[0].severity, Severity::Hint);
        assert_eq!(items[0].message, "No note named Nope");
        assert_eq!(items[0].range.start.character, 17);

        rt.block_on(async {
            // The provider answers about the text the editor has, not about the file on disk.
            f.vault
                .change_document("a.md", "see [[Be]]".to_string())
                .await
                .unwrap();
            let at = |character| Pos { line: 0, character };
            let items = f.vault.completion("a.md", at(8), None).await.unwrap().items;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].label, "Beta");
            assert_eq!(items[0].insert, "[[Beta]]");
            assert_eq!(
                items[0].replace,
                Range {
                    start: at(4),
                    end: at(10)
                },
                "the trigger and the `]]` the auto-pair left both go"
            );

            f.vault
                .change_document("a.md", "a #ru".to_string())
                .await
                .unwrap();
            let items = f.vault.completion("a.md", at(5), None).await.unwrap().items;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].insert, "#rust");
            assert_eq!(items[0].kind, Kind::Tag);
        });
    }

    #[test]
    fn notes_provider_follows_links_both_ways() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "see [[Beta]]\n");
        f.write("sub/Beta.md", "intro\n\n# Beta\nbody\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        accent_lsp::runtime().block_on(async {
            let caret = Pos {
                line: 0,
                character: 7,
            };
            f.vault
                .open_document("a.md", "markdown", "see [[Beta]]\n".to_string())
                .await
                .unwrap();

            let target = f.vault.definition("a.md", caret).await.unwrap();
            assert_eq!(
                target,
                [Location {
                    path: "sub/Beta.md".to_string(),
                    range: Range::default()
                }]
            );
            let hover = f.vault.hover("a.md", caret).await.unwrap().unwrap();
            assert!(hover.text.contains("**Beta**"), "{}", hover.text);

            f.vault
                .open_document(
                    "sub/Beta.md",
                    "markdown",
                    "intro\n\n# Beta\nbody\n".to_string(),
                )
                .await
                .unwrap();
            let refs = f.vault.references("sub/Beta.md", caret).await.unwrap();
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].path, "a.md");
            assert_eq!(refs[0].range.start.character, 4, "the link as written");

            // An anchor lands on the heading rather than on the first line.
            f.vault
                .change_document("a.md", "see [[Beta#Beta]]\n".to_string())
                .await
                .unwrap();
            let target = f.vault.definition("a.md", caret).await.unwrap();
            assert_eq!(target[0].range.start.line, 2);
        });
    }

    /// `[[Old]]` here belongs to a different note; renaming `Dir/Old.md` must leave it alone.
    #[test]
    fn rename_leaves_a_link_that_resolves_elsewhere_alone() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Dir/Old.md", "the deep one\n");
        f.write("Old.md", "a different note\n");
        f.write("Ref.md", "deep: [[Dir/Old]]\nshallow: [[Old]]\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plan = f.vault.plan_rename("Dir/Old.md", "Dir/Renamed.md").unwrap();
        assert_eq!(plan.rewrites, ["Ref.md"]);
        let report = f.vault.rename(&plan, true).unwrap();

        assert_eq!(report.rewritten, ["Ref.md"]);
        assert_eq!(
            f.read("Ref.md"),
            "deep: [[Dir/Renamed]]\nshallow: [[Old]]\n"
        );
    }

    /// The same two links, with nothing else called `Old`: now both do point at the renamed
    /// note, and both have to move with it.
    #[test]
    fn rename_rewrites_every_link_that_resolves_to_it() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Dir/Old.md", "the deep one\n");
        f.write("Ref.md", "deep: [[Dir/Old]]\nshallow: [[Old]]\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let plan = f.vault.plan_rename("Dir/Old.md", "Dir/Renamed.md").unwrap();
        let report = f.vault.rename(&plan, true).unwrap();

        assert_eq!(report.rewritten, ["Ref.md"]);
        assert_eq!(
            f.read("Ref.md"),
            "deep: [[Dir/Renamed]]\nshallow: [[Renamed]]\n"
        );
    }

    /// The façade is the boundary MCP and Android call directly, so it decides what is inside
    /// the vault; an `accent-target:` of `../Outside/x.md` reaches it too.
    #[test]
    fn paths_outside_the_vault_are_refused() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let outside = outer.path().join("outside.md");
        std::fs::write(&outside, "keep me\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let (vault, _events) = Vault::open_at(
            &root,
            &cache.path().join("index.db"),
            VaultConfig::default(),
        )
        .unwrap();

        let escapes = [
            "../outside.md".to_string(),
            "a/../../outside.md".to_string(),
            outside.to_string_lossy().into_owned(),
        ];
        for rel in &escapes {
            assert!(vault.save(rel, "CLOBBERED\n", None).is_err(), "{rel}");
            assert!(vault.create_note(rel, None).is_err(), "{rel}");
            assert!(vault.read(rel).is_err(), "{rel}");
            assert!(vault.create_dir(rel).is_err(), "{rel}");
        }
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep me\n");

        // A path that stays inside still works, `..` in the middle of it and all.
        vault.create_note("sub/Nested.md", None).unwrap();
        vault.save("sub/../sub/Nested.md", "fine\n", None).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("sub/Nested.md")).unwrap(),
            "fine\n"
        );
    }

    /// Moving a folder takes its subtree with it in the index, or every note under it drops out
    /// of search, backlinks and the switcher until the next restart.
    #[test]
    fn renaming_a_directory_keeps_its_subtree_in_the_index() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a/b/note.md", "kumquat harvest\n");
        f.write("a/b/sub/x.md", "deep\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        assert_eq!(
            f.vault.note_paths().unwrap(),
            ["a/b/note.md", "a/b/sub/x.md"]
        );

        let plan = f.vault.plan_rename("a/b", "a/c").unwrap();
        f.vault.rename(&plan, true).unwrap();

        assert!(
            poll_until(
                || f.vault.note_paths().unwrap() == ["a/c/note.md", "a/c/sub/x.md"],
                BUDGET
            ),
            "the subtree is gone from the index: {:?}",
            f.vault.note_paths().unwrap()
        );
        assert_eq!(
            names(&f.vault.list_dir("a/c").unwrap()),
            ["a/c/sub", "a/c/note.md"]
        );
        assert!(!f.vault.search("kumquat", 10, false).unwrap().is_empty());
    }

    /// "Keep theirs" overwrites the user's own version, so what it replaced has to survive
    /// somewhere: a conflict copy of its own, which the resolve dialog can undo from.
    #[test]
    fn adopt_conflict_keeps_the_replaced_version_as_a_conflict_copy() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine\n");
        f.write(CONFLICT, "theirs\n");

        f.vault.adopt_conflict("Note.md", CONFLICT).unwrap();

        assert_eq!(f.read("Note.md"), "theirs\n");
        let backups: Vec<String> = std::fs::read_dir(f.vault.root())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| fs::is_sync_conflict(n) && n != CONFLICT)
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert_eq!(
            fs::conflict_original(&backups[0]).as_deref(),
            Some("Note.md")
        );
        assert_eq!(f.read(&backups[0]), "mine\n");
    }

    #[test]
    fn repos_lists_the_vault_repo_and_a_nested_one() {
        // No git binary, nothing to discover; the rest of the vault works either way.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let f = Fixture::open(VaultConfig::default());
        let root = f.vault.root().to_path_buf();
        f.write("sub/Note.md", "hi");
        for dir in [root.clone(), root.join("sub")] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["init", "-q", "-b", "main"])
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "git init in {}", dir.display());
        }
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let repos = f.vault.repos();
        assert_eq!(
            repos.len(),
            2,
            "the vault's own repository and the nested one"
        );
        assert_eq!(repos[0].root, root);
        assert_eq!(repos[1].root, root.join("sub"));
    }
}
