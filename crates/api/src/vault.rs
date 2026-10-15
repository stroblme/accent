//! A shared interface for local and remote vaults, so the UI need not distinguish them.
//! Local operations use [`Local`]; remote operations use
//! `accent-cli serve` over SSH.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use serde_json::json;

use accent_core::path::linked_path;

use crate::local::Local;
use crate::{
    Backlink, CodeSymbol, Commit, Etag, Event, FileRow, HeadingHit, KeptLink, Location, Match,
    Mention, Options, OutLink, PageEdit, PdfLink, RenamePlan, RenameReport, RepageReport,
    ReplaceReport, Repo, Result, SearchHit, Session, Stats, Status, Submodule, SymbolName,
    UndoReport, VaultConfig, fs, git, remote, rpc, ssh,
};

/// An open vault with the same interface for local and remote files.
///
/// Operations are synchronous. The desktop runs remote calls, including autosave,
/// on worker threads to avoid blocking its shared main loop. Writes that must finish
/// before the caller continues, such as closing a dirty tab, remain on the caller.
pub struct Vault {
    pub(crate) backend: Backend,
    /// Config, recents and session key: the local root or remote `ssh://` address.
    /// Never a path to open.
    key: PathBuf,
}

// ponytail: `Local` is the big variant, so every remote `Vault` carries its footprint too. One
// per window makes that a few hundred bytes in the whole process; box it if that ever stops being
// true.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Backend {
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

    /// [`open`](Self::open) without a filesystem watcher, for Android storage where
    /// inotify drops events. Call [`rescan`](Self::rescan) on foregrounding; writes
    /// through this API still update the index.
    pub fn open_unwatched(root: &Path, cfg: VaultConfig) -> Result<(Vault, Receiver<Event>)> {
        let db = accent_core::index::default_db_path(root);
        Vault::open_unwatched_at(root, &db, cfg)
    }

    /// [`open_unwatched`](Self::open_unwatched) with an explicit index file, for
    /// short-lived `accent-cli` commands.
    pub fn open_unwatched_at(
        root: &Path,
        db: &Path,
        cfg: VaultConfig,
    ) -> Result<(Vault, Receiver<Event>)> {
        let (local, events) = Local::open_with(root, db, cfg, false)?;
        Ok((Vault::of(Backend::Local(local)), events))
    }

    /// Open `ssh://[user@]host[:port]/path` without waiting for the connection.
    /// [`Event::Connecting`] reports progress, followed by [`Event::Connected`]
    /// or [`Event::Disconnected`].
    pub fn open_remote(url: &str, cfg: VaultConfig) -> Result<(Vault, Receiver<Event>)> {
        let url = ssh::parse(url)?;
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

    /// The local root or remote `ssh://` address used by recents, settings and sessions.
    /// Remote vault history stays separate from local directories.
    pub fn key(&self) -> &Path {
        &self.key
    }

    /// Remote-only operations: host shells, port forwarding and uploads.
    /// `None` for a local vault, so the UI can offer the appropriate actions.
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

    /// Apply config without blocking the main loop. Remote updates, including the next
    /// two setters, run on a worker in call order.
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

    /// Whether prose documents are offered words, their own and the dictionary's, which is read
    /// where the vault is. Global like [`Vault::set_ghost`].
    pub fn set_words(&self, on: bool) {
        match &self.backend {
            Backend::Local(v) => v.set_words(on),
            Backend::Remote(r) => r.set_words(on),
        }
    }

    /// Request a full scan after resume or suspected missed changes. Local requests
    /// are queued without failure; remote failures are returned to the caller.
    pub fn rescan(&self) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => {
                v.rescan();
                Ok(())
            }
            Backend::Remote(r) => r.call("rescan", json!([])),
        }
    }

    /// Pause indexing while retaining completed rows. The reconcile reports
    /// [`ReconcileStats::stopped`](accent_core::index::ReconcileStats::stopped), and
    /// the partial index stays usable. Continue with [`resume_indexing`](Self::resume_indexing)
    /// or by reopening the vault. Remote requests reach the host worker while it scans.
    pub fn stop_indexing(&self) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => {
                v.stop_indexing();
                Ok(())
            }
            Backend::Remote(r) => r.call("stop_indexing", json!([])),
        }
    }

    /// Resume after [`stop_indexing`](Self::stop_indexing). Paused vaults ignore all
    /// other rescan requests.
    pub fn resume_indexing(&self) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => {
                v.resume_indexing();
                Ok(())
            }
            Backend::Remote(r) => r.call("resume_indexing", json!([])),
        }
    }

    /// Wait until all preceding vault writes reach the index, so a following query
    /// sees them. Call off the main loop: a scan is one batch. Remote vaults return immediately.
    pub fn settle_index(&self) {
        if let Backend::Local(v) = &self.backend {
            v.settle_index();
        }
    }

    /// Resolve `rel` inside the vault root, rejecting escapes. Remote paths belong
    /// to the host; use [`fetch`](Self::fetch) to obtain a local file.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        match &self.backend {
            Backend::Local(v) => v.resolve(rel),
            Backend::Remote(_) => Local::join(&self.root(), rel),
        }
    }

    /// The saved session on this machine: window and tab state belongs to the desktop,
    /// independently of where vault files live.
    pub fn session(&self) -> Session {
        Session::load(&self.key)
    }

    pub fn save_session(&self, s: &Session) -> Result<()> {
        Ok(s.save(&self.key)?)
    }
}

/// Extra remote-call allowance for planning moves and fsyncing rewritten links.
/// This is an estimate, not a host timeout: moves must finish because interrupted
/// link rewrites cannot be resumed after the source files have moved.
pub(crate) const MOVE_BOUND: std::time::Duration = std::time::Duration::from_secs(50);

/// Host timeout for [`Local::replace_all`], which rewrites and fsyncs each file.
/// Unlike [`MOVE_BOUND`], replacement can stop between files. The report lists
/// unprocessed files, and another Replace All finishes the remaining matches.
pub(crate) const REPLACE_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// Generate [`Vault`] methods, RPC routes, and wire names from one table.
///
/// Methods return [`Result`], preserving errors across the wire. A function after `=` runs beside
/// the repository instead of calling [`Local`]; git routes keep their `git_` prefix.
///
/// `ref` arguments are borrowed locally and serialized as owned values (`str` as `String`);
/// `val` arguments travel unchanged. `bounded by` adds a host timeout to [`rpc::DEADLINE`].
///
/// Operations that differ locally and remotely are defined separately: unindexed searches, byte
/// transfers, and worker-backed `repos`.
macro_rules! methods {
    // What one argument is, on the façade, on the wire, and at the call the host makes.
    (@fty ref $t:ty) => { &$t };
    (@fty val $t:ty) => { $t };
    (@wty ref $t:ty) => { <$t as ToOwned>::Owned };
    (@wty val $t:ty) => { $t };
    (@pass ref $n:ident) => { &$n };
    (@pass val $n:ident) => { $n };

    // The call itself, where the files are: the vault's own method, or the function a row names.
    (@here $v:ident $name:ident ($($a:tt)*)) => { $v.$name($($a)*) };
    (@here $v:ident $name:ident $module:ident $core:ident ($($a:tt)*)) => {
        $module::$core($($a)*).map_err(crate::Error::from)
    };

    // How long a remote caller waits: the host's own bound, if it has one, and a round trip.
    (@deadline) => { rpc::DEADLINE };
    (@deadline $bound:expr) => { $bound + rpc::DEADLINE };

    ($(
        $(#[$doc:meta])*
        $name:ident $(= $module:ident :: $core:ident)? ($($arg:ident : $kind:tt $t:ty),* $(,)?)
            -> $ret:ty
            $(, bounded by $bound:expr)?;
    )*) => {
        impl Vault {
            $(
                $(#[$doc])*
                pub fn $name(&self $(, $arg: methods!(@fty $kind $t))*) -> Result<$ret> {
                    match &self.backend {
                        Backend::Local(_v) => {
                            methods!(@here _v $name $($module $core)? ($($arg),*))
                        }
                        Backend::Remote(r) => r.call_within(
                            stringify!($name),
                            json!([$($arg),*]),
                            methods!(@deadline $($bound)?),
                        ),
                    }
                }
            )*
        }

        /// Dispatch table-defined methods; `None` lets `rpc::dispatch` try its
        /// explicit routes.
        pub(crate) fn dispatch(
            vault: &Local,
            method: &str,
            p: &serde_json::Value,
        ) -> Option<Result<serde_json::Value>> {
            $(
                if method == stringify!($name) {
                    rpc::args!(p; $($arg: methods!(@wty $kind $t)),*);
                    let answer = methods!(@here vault $name $($module $core)?
                        ($(methods!(@pass $kind $arg)),*));
                    return Some(rpc::answer(answer));
                }
            )*
            None
        }
    };
}

methods! {
    // ------------------------------------------------------------------ files
    read(rel: ref str) -> (String, Etag);
    read_text(rel: ref str) -> fs::Read;
    stat(rel: ref str) -> Option<Etag>;
    /// Permanently delete a remote file or directory after UI confirmation.
    /// Local deletion uses the desktop system trash.
    delete(rel: ref str) -> ();
    create_dir(rel: ref str) -> ();
    /// Index an external write immediately, without watcher debounce.
    /// [`write_file`](Vault::write_file) calls this after uploading bytes.
    wrote(rel: ref str) -> ();
    /// Copy a file or directory within the vault, without transferring remote bytes.
    /// Nothing is replaced: a taken `to` fails with `AlreadyExists`, and the caller tries its
    /// next free name.
    copy(from: ref str, to: ref str) -> ();
    plan_moves(moves: ref [(String, String)]) -> RenamePlan, bounded by MOVE_BOUND;
    rename(plan: ref RenamePlan, update: val bool) -> RenameReport, bounded by MOVE_BOUND;
    /// A note's text as Save As writes it at another path, its relative links pointed back at
    /// what they named: asked where the index is.
    relink_copy(from: ref str, to: ref str, text: ref str) -> Option<String>;
    /// The notes' links into a PDF after a page edit, rewritten where the notes are.
    repage_links(rel: ref str, edit: val PageEdit, keep: ref [KeptLink]) -> RepageReport,
        bounded by MOVE_BOUND;
    adopt_conflict(original: ref str, conflict: ref str) -> Etag;
    template_target(template: ref str) -> Option<String>;
    note_from_template(template: ref str) -> Option<(String, Vec<usize>)>;
    render_template(template: ref str, title: ref str) -> (String, Vec<usize>);
    templates() -> Vec<String>;
    /// Fetch all template targets in one request, avoiding a remote round trip
    /// per [`template_target`](Vault::template_target).
    template_targets() -> Vec<String>;
    /// Replace every match in every file whose indexed body has one: what
    /// [`grep`](Vault::grep) counts under the same `include_ignored`. The pattern crosses as what
    /// the user typed plus its toggles, and is compiled where the files are.
    replace_all(
        query: ref str,
        options: val Options,
        replacement: ref str,
        literal: val bool,
        include_ignored: val bool,
    ) -> ReplaceReport, bounded by REPLACE_BOUND;
    /// Put back what the last [`replace_all`](Vault::replace_all) rewrote. The text it needs
    /// stayed wherever the rewrite ran, the host on a remote vault, so only the report crosses.
    undo_replace() -> UndoReport, bounded by REPLACE_BOUND;

    // ------------------------------------------------------------ index reads
    list_dir(rel: ref str) -> Vec<FileRow>;
    /// Watch direct children of unindexed folders until
    /// [`unwatch_unindexed`](Vault::unwatch_unindexed). Creates, removals and renames
    /// emit [`Event::UnindexedChanged`]; subtrees are not watched.
    watch_unindexed(dirs: ref [String]) -> ();
    unwatch_unindexed(dirs: ref [String]) -> ();
    /// Rescan one folder for missed watcher events (Reload);
    /// [`rescan`](Vault::rescan) scans the whole vault.
    rescan_dir(dir: ref str) -> ();
    search(query: ref str, limit: val usize, include_ignored: val bool) -> Vec<SearchHit>;
    search_mid_word(
        query: ref str,
        limit: val usize,
        include_ignored: val bool,
        skip: ref [String],
    ) -> Vec<SearchHit>;
    grep(query: ref str, options: val Options, limit: val usize, include_ignored: val bool)
        -> (Vec<Match>, usize);
    tags() -> Vec<(String, i64)>;
    /// What the index holds, counted: `accent-cli stats`.
    stats() -> Stats;
    files_with_tag(tag: ref str) -> Vec<FileRow>;
    backlinks(rel: ref str) -> Vec<Backlink>;
    backlink_locations(rel: ref str) -> Vec<Location>;
    /// The links written in a note, in order, and where each leads.
    links_from(rel: ref str) -> Vec<OutLink>;
    /// `(note, target, byte offset)` for every link nothing in the vault answers to.
    unresolved_links() -> Vec<(String, String, i64)>;
    /// The headings holding the most of `words`, best first.
    headings_matching(words: ref [String], limit: val usize) -> Vec<HeadingHit>;
    /// `(file, title)` for the files holding any of `words`, best first.
    rank_files(words: ref [String], limit: val usize) -> Vec<(String, Option<String>)>;
    /// A code file's declarations, in order.
    file_symbols(rel: ref str) -> Vec<CodeSymbol>;
    /// The declarations a name or `Container::name` finds, best first.
    find_symbols(query: ref str, limit: val usize) -> Vec<CodeSymbol>;
    /// Every declaration's name and place, which Go to Symbol holds and ranks in the window.
    symbol_names() -> Vec<SymbolName>;
    /// The declarations of each of `names`, those in `near` first.
    symbols_named(names: ref [String], near: ref str, per: val usize) -> Vec<CodeSymbol>;
    /// Where a name is written in code, with the declaration around it.
    mentions(name: ref str, limit: val usize) -> Vec<Mention>;
    /// The declarations named by a question's words, with how many of their parts it holds.
    symbols_by_words(words: ref [String], limit: val usize) -> Vec<(CodeSymbol, usize)>;
    /// The note links that highlight a page of this PDF. Asked of the host on a remote vault,
    /// because that is where the notes and the index are.
    pdf_links(rel: ref str) -> Vec<PdfLink>;
    file_paths(include_ignored: val bool) -> Vec<String>;
    set_excluded(entries: ref [String]) -> ();
    recent_files(limit: val usize) -> Vec<String>;
    resolve_link(target: ref str) -> Option<String>;
    /// What Go to File and `[[` completion offer to write.
    missing_notes() -> Vec<String>;
    /// The notes in the folders git ignores, walked again when `fresh`: what Go to File lists
    /// behind the indexed files. Walked where the files are.
    ignored_notes(fresh: val bool) -> Vec<String>;
    /// The file a link target names in the folders git ignores, when the index has none.
    resolve_ignored(target: ref str) -> Option<String>;
    /// `(alias, note)` for every frontmatter alias: what Go to File also finds a note by.
    note_aliases() -> Vec<(String, String)>;
    conflicts() -> Vec<(String, String)>;
    conflicts_of(rel: ref str) -> Vec<String>;

    // -------------------------------------------------------------------- git
    // The repositories belong to the machine the files are on, so every one of these runs there —
    // the `git` binary the user configured, with their hooks and their credential helper.
    git_status = git::status(repo: ref Repo) -> Status;
    git_untracked = git::untracked(repo: ref Repo, dir: ref str) -> Vec<String>;
    /// Fetch one page of history. The UI retains commits and computes
    /// [`git::lanes`] locally because lane assignment needs all preceding commits.
    git_log = git::log(repo: ref Repo, skip: val usize, limit: val usize) -> Vec<Commit>;
    git_show = git::show(repo: ref Repo, rev: ref str, path: ref str) -> Option<git::Blob>;
    git_changed_files = git::changed_files(repo: ref Repo, oid: ref str) -> Vec<git::ChangedFile>;
    git_submodules = git::submodules(repo: ref Repo) -> Vec<Submodule>;
    git_branches = git::branches(repo: ref Repo) -> git::Branches;
    git_checkout = git::checkout(repo: ref Repo, branch: ref str) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git_track = git::track(repo: ref Repo, remote: ref str) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git_checkout_commit = git::checkout_commit(repo: ref Repo, oid: ref str) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git_create_branch = git::create_branch(repo: ref Repo, name: ref str, checkout: val bool) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git_delete_branch = git::delete_branch(repo: ref Repo, name: ref str, force: val bool) -> ();
    git_merge = git::merge(repo: ref Repo, branch: ref str) -> git::Merge,
        bounded by git::TRANSFER_TIMEOUT;
    git_merge_abort = git::merge_abort(repo: ref Repo) -> ();
    /// A rebase stopped part way, carried on or given up.
    git_rebase_continue = git::rebase_continue(repo: ref Repo) -> git::Rebase,
        bounded by git::TRANSFER_TIMEOUT;
    git_rebase_abort = git::rebase_abort(repo: ref Repo) -> ();
    git_stage = git::stage(repo: ref Repo, paths: ref [String]) -> ();
    git_unstage = git::unstage(repo: ref Repo, paths: ref [String]) -> ();
    git_discard = git::discard(repo: ref Repo, paths: ref [String]) -> ();
    /// What Stage and Unstage Selected Lines write.
    git_stage_text = git::stage_text(repo: ref Repo, path: ref str, text: ref str) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git_commit = git::commit(repo: ref Repo, message: ref str, all: val bool) -> String,
        bounded by git::TRANSFER_TIMEOUT;
    /// A Sync's two halves, asked for one after the other so the window knows which is running.
    git_pull = git::pull(repo: ref Repo) -> String, bounded by git::TRANSFER_TIMEOUT;
    git_push = git::push(repo: ref Repo) -> String, bounded by git::TRANSFER_TIMEOUT;
    /// Bring the remote-tracking refs up to date.
    git_fetch = git::fetch(repo: ref Repo) -> String, bounded by git::FETCH_TIMEOUT;
    /// The oids a pull would bring in. Asked only where [`git::Status`] says there are any.
    git_incoming = git::incoming(repo: ref Repo) -> Vec<String>;
}

/// The rest: the methods whose two sides really do differ.
impl Vault {
    /// Check whether `rel` exists with one `stat`, for the tree and open actions.
    pub fn exists(&self, rel: &str) -> bool {
        matches!(self.stat(rel), Ok(Some(_)))
    }

    /// Write a note, refused with [`accent_core::Error::ChangedOnDisk`] when the file is no longer
    /// at `expected`. `None` writes whatever is there.
    pub fn save(&self, rel: &str, text: &str, expected: Option<Etag>) -> Result<Etag> {
        match &self.backend {
            Backend::Local(v) => v.save(rel, text, expected),
            Backend::Remote(r) => r.call("save", json!([rel, text, expected])),
        }
    }

    /// Return a local file for readers that require one, such as PDF and image viewers.
    /// Local vaults return the file itself; remote vaults fetch a cached copy over SSH.
    /// Both return `NotFound` for a missing file.
    pub fn fetch(&self, rel: &str) -> Result<PathBuf> {
        self.fetch_with(rel, &|_, _| ())
    }

    /// [`fetch`](Self::fetch) with received and total byte counts during remote
    /// downloads. Local files do not report progress.
    pub fn fetch_with(&self, rel: &str, progress: &dyn Fn(u64, u64)) -> Result<PathBuf> {
        match &self.backend {
            Backend::Local(v) => {
                let path = v.resolve(rel)?;
                match std::fs::metadata(&path) {
                    Ok(_) => Ok(path),
                    Err(e) => Err(accent_core::Error::io(rel, e).into()),
                }
            }
            Backend::Remote(r) => r.fetch_with(rel, progress),
        }
    }

    /// Copy a file from this machine into the vault.
    pub fn upload(&self, local: &Path, rel: &str) -> Result<()> {
        self.upload_with(local, rel, &|_, _| ())
    }

    /// [`upload`](Self::upload) with sent and total byte counts for remote uploads.
    /// Local copies do not report progress.
    pub fn upload_with(&self, local: &Path, rel: &str, progress: &dyn Fn(u64, u64)) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => match std::fs::copy(local, v.resolve(rel)?) {
                Ok(_) => Ok(()),
                Err(e) => Err(accent_core::Error::io(local.display(), e).into()),
            },
            Backend::Remote(r) => r.upload_with(local, rel, progress),
        }
    }

    /// Create a new file from `bytes`, for pasted or dropped images. Remote bytes
    /// travel over SSH rather than through the RPC protocol.
    pub fn write_file(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => v.write_file(rel, bytes),
            Backend::Remote(r) => r.write_file(rel, bytes),
        }
    }

    /// Copy a file out of the vault to somewhere on this machine.
    pub fn download(&self, rel: &str, dest: &Path) -> Result<()> {
        self.download_with(rel, dest, &|_, _| ())
    }

    /// [`download`](Self::download), telling `progress` the bytes so far and how many there are
    /// on a remote vault.
    pub fn download_with(&self, rel: &str, dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => match std::fs::copy(v.resolve(rel)?, dest) {
                Ok(_) => Ok(()),
                Err(e) => Err(accent_core::Error::io(rel, e).into()),
            },
            Backend::Remote(r) => r.download_with(rel, dest, progress),
        }
    }

    /// Create a file, optionally from a template. Defined outside the method table
    /// because `Option<&str>` cannot use its `ref` or `val` argument forms.
    pub fn create_note(&self, rel: &str, template: Option<&str>) -> Result<(String, Vec<usize>)> {
        match &self.backend {
            Backend::Local(v) => v.create_note(rel, template),
            Backend::Remote(r) => r.call("create_note", json!([rel, template])),
        }
    }

    /// [`Local::grep_unindexed`] for the Search pane's remaining row budget.
    /// Return immediately for a zero budget to avoid an unnecessary walk or round trip.
    /// `stop` cancels local walks; remote walks run to their budget and callers discard
    /// stale answers because the flag cannot cross the wire.
    pub fn grep_unindexed(
        &self,
        query: &str,
        options: Options,
        limit: usize,
        stop: &(dyn Fn() -> bool + Sync),
    ) -> Result<Vec<Match>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        match &self.backend {
            Backend::Local(v) => v.grep_unindexed(query, options, limit, stop),
            Backend::Remote(r) => r.call("grep_unindexed", json!([query, options, limit])),
        }
    }

    /// Resolve an embed to a vault file, or `None` if missing. An existing explicit
    /// path wins; basename wikilinks use the index to find files in folders such as
    /// `Attachments/`. Shared here so desktop and Android resolve embeds alike.
    pub fn asset(&self, rel: &str) -> Option<String> {
        if self.exists(rel) {
            return Some(rel.to_string());
        }
        self.resolve_link(rel).ok().flatten()
    }

    /// Resolve a link target through the index, [`linked_path`] on disk, then
    /// [`resolve_ignored`](Self::resolve_ignored) for ignored folders. Return `None` only for a
    /// missing file so the UI can offer to create it; [`stat`](Self::stat) preserves remote errors
    /// that [`exists`](Self::exists) would hide. The preview uses this path; Go to Definition uses
    /// the language provider beside the files.
    pub fn follow(&self, target: &str) -> Result<Option<String>> {
        if let Some(rel) = self.resolve_link(target)? {
            return Ok(Some(rel));
        }
        let rel = linked_path(target);
        if self.stat(&rel)?.is_some() {
            return Ok(Some(rel));
        }
        self.resolve_ignored(target)
    }

    /// List repositories touching the vault. Return failures so a disconnected
    /// remote vault does not appear to have no repositories.
    pub fn repos(&self) -> Result<Vec<Repo>> {
        match &self.backend {
            Backend::Local(v) => Ok(v.repos()),
            Backend::Remote(r) => r.call("repos", json!([])),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::*;
    use crate::{Event, Kind, Location, Pos, Range, Severity, Vault, VaultConfig};
    use std::time::Duration;

    #[test]
    fn vault_is_send_and_sync() {
        fn assert<T: Send + Sync>() {}
        assert::<Vault>();
    }

    /// Assert the vault can cross threads: the desktop runs costly vault-wide
    /// replacements on a worker.
    #[test]
    fn a_vault_handle_can_cross_a_thread() {
        fn crosses<T: Send + Sync>() {}
        crosses::<Vault>();
    }

    #[test]
    fn an_asset_is_found_by_its_basename() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Attachments/img.png", "not really a png");
        f.write("Note.md", "![[img.png]]\n");
        f.vault.rescan().unwrap();
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

    /// A link into a tree the walk never enters still opens the file there, which is the one New
    /// File would have offered to write. Only a real miss is `None`.
    #[test]
    fn a_link_into_an_unwalked_tree_follows_to_the_file_on_disk() {
        let f = Fixture::open(VaultConfig::default());
        f.write("node_modules/pkg/doc.pdf", "not really a pdf");
        f.write("node_modules/pkg/Guide.md", "# Guide\n");
        f.write("Note.md", "body\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        for unwalked in ["node_modules/pkg/doc.pdf", "node_modules/pkg/Guide"] {
            assert_eq!(f.vault.resolve_link(unwalked).unwrap(), None, "{unwalked}");
        }
        assert_eq!(
            f.vault
                .follow("node_modules/pkg/doc.pdf")
                .unwrap()
                .as_deref(),
            Some("node_modules/pkg/doc.pdf")
        );
        // A note is named without its extension, as New File would write it.
        assert_eq!(
            f.vault.follow("node_modules/pkg/Guide").unwrap().as_deref(),
            Some("node_modules/pkg/Guide.md")
        );
        assert_eq!(f.vault.follow("note").unwrap().as_deref(), Some("Note.md"));
        // A folder is not a file to open, and nothing at all is what New File is offered for.
        assert_eq!(f.vault.follow("node_modules/pkg").unwrap(), None);
        assert_eq!(f.vault.follow("nowhere.pdf").unwrap(), None);
    }

    /// A note in a gitignored folder is out of the index, and still listed, completed, followed
    /// by its name alone and no note to write; a dependency tree inside the folder stays out.
    #[test]
    fn a_note_in_a_gitignored_folder_is_listed_completed_and_followed() {
        let f = Fixture::open(VaultConfig::default());
        f.write(".gitignore", "ignored/\n");
        f.write("ignored/Deep Note.md", "# Deep\n");
        f.write("ignored/out.log", "log\n");
        f.write("ignored/node_modules/dep/README.md", "dep\n");
        f.write("a.md", "[[Deep Note]] [[Nowhere]]\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        assert_eq!(f.vault.resolve_link("Deep Note").unwrap(), None);
        assert_eq!(
            f.vault.ignored_notes(true).unwrap(),
            ["ignored/Deep Note.md"]
        );
        assert_eq!(
            f.vault.follow("Deep Note").unwrap().as_deref(),
            Some("ignored/Deep Note.md")
        );
        assert_eq!(f.vault.missing_notes().unwrap(), ["Nowhere.md"]);

        let rt = accent_lsp::runtime();
        rt.block_on(f.vault.open_document(
            "a.md",
            "markdown",
            "[[Deep Note]] [[Nowhere]]\n[[Deep\n".to_string(),
        ))
        .unwrap();
        let Some(Event::Diagnostics { items, .. }) =
            f.wait(|e| matches!(e, Event::Diagnostics { .. }))
        else {
            panic!("opening a note has to say what is wrong with it");
        };
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].message, "No note named Nowhere");

        let at = Pos {
            line: 1,
            character: 6,
        };
        let items = rt
            .block_on(f.vault.completion("a.md", at, None))
            .unwrap()
            .items;
        let rows: Vec<(&str, Option<&str>)> = items
            .iter()
            .map(|i| (i.insert.as_str(), i.detail.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [("[[Deep Note]]", Some("ignored/Deep Note.md, ignored"))]
        );
    }

    /// Paste duplicates, so the copy has to take a folder as readily as a file — and the copy is
    /// a file of its own afterwards, not a second name for the same bytes.
    #[test]
    fn copying_takes_a_file_and_a_whole_folder() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "body\n");
        f.write("Folder/inner/deep.md", "deep\n");

        f.vault.copy("Note.md", "Note (copy).md").unwrap();
        assert_eq!(f.read("Note (copy).md"), "body\n");
        assert_eq!(f.read("Note.md"), "body\n", "the original stays");

        f.vault.copy("Folder", "Folder (copy)").unwrap();
        assert_eq!(f.read("Folder (copy)/inner/deep.md"), "deep\n");

        // The two are separate files: writing one must not reach the other.
        f.write("Note (copy).md", "edited\n");
        assert_eq!(f.read("Note.md"), "body\n");

        assert!(f.vault.copy("nowhere.md", "x.md").is_err());
    }

    /// A name taken between choosing it and copying, by another paste or a sync, is refused and
    /// left as it was, a folder's included: the caller takes the next free name.
    #[test]
    fn a_copy_never_replaces_what_is_there() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine\n");
        f.write("Note (copy).md", "theirs\n");
        f.write("Folder/inner/deep.md", "mine\n");
        f.write("Folder/top.md", "mine\n");
        f.write("Folder (copy)/inner/deep.md", "theirs\n");
        let taken = |rel: &str| {
            Err(crate::Error::Core(accent_core::Error::AlreadyExists(
                rel.into(),
            )))
        };

        assert_eq!(
            f.vault.copy("Note.md", "Note (copy).md"),
            taken("Note (copy).md")
        );
        assert_eq!(f.read("Note (copy).md"), "theirs\n");
        assert_eq!(
            f.vault.copy("Folder", "Folder (copy)"),
            taken("Folder (copy)")
        );
        assert_eq!(f.read("Folder (copy)/inner/deep.md"), "theirs\n");
        let listed = std::fs::read_dir(f.vault.root().join("Folder (copy)")).unwrap();
        assert_eq!(listed.count(), 1, "nothing joins the folder that was there");
    }

    /// A copied folder is what Paste carries in from this machine: a link to a file inside it
    /// becomes that file, and a link out of it, one to a folder, a dangling one and a socket stay
    /// behind.
    #[test]
    fn a_copied_folder_keeps_links_inside_it_and_drops_the_rest() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("o.md"), "outside\n").unwrap();
        let f = Fixture::open(VaultConfig::default());
        f.write("F/a.md", "inside\n");
        f.write("F/sub/b.md", "b\n");
        let src = f.vault.root().join("F");
        symlink("a.md", src.join("link.md")).unwrap();
        symlink(outside.path().join("o.md"), src.join("out.md")).unwrap();
        symlink("sub", src.join("dir")).unwrap();
        symlink("gone.md", src.join("dangling.md")).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(src.join("sock")).unwrap();

        f.vault.copy("F", "G").unwrap();
        let copy = f.vault.root().join("G");
        let mut names: Vec<String> = std::fs::read_dir(&copy)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.md", "link.md", "sub"]);
        assert!(!copy.join("link.md").is_symlink());
        assert_eq!(f.read("G/link.md"), "inside\n");
        assert_eq!(f.read("G/sub/b.md"), "b\n");
    }

    /// A folder copied into itself would copy its own copy again, so nothing is made: not by its
    /// path as spelled, `..` included, and not through a link to it.
    #[test]
    fn a_folder_is_never_copied_into_itself() {
        let f = Fixture::open(VaultConfig::default());
        f.write("A/x.md", "x\n");
        let root = f.vault.root().to_path_buf();
        std::os::unix::fs::symlink(root.join("A"), root.join("L")).unwrap();
        let listed = || {
            let mut names: Vec<String> = std::fs::read_dir(root.join("A"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };

        for to in ["A", "A/A", "A/sub/A", "B/../A/A", "L/A"] {
            let copied = f.vault.copy("A", to);
            assert!(
                matches!(
                    copied,
                    Err(crate::Error::Core(accent_core::Error::Invalid(_)))
                ),
                "{to}: {copied:?}"
            );
            assert_eq!(listed(), ["x.md"], "{to}");
        }
    }

    /// A reader learns that a file is missing from the fetch itself, the way a remote vault
    /// already said it, rather than from a path to nothing.
    #[test]
    fn fetching_a_missing_file_is_not_found() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.png", "not really a png");
        assert_eq!(
            f.vault.fetch("a.png").unwrap(),
            f.vault.root().join("a.png")
        );
        assert_eq!(
            f.vault.fetch("gone.png").unwrap_err(),
            crate::Error::Core(accent_core::Error::NotFound("gone.png".into()))
        );
    }

    /// Exercise the notes provider through the façade used by the editor.
    /// `block_on` surrounds only the request: vault teardown blocks on the same
    /// runtime and cannot run on a runtime thread.
    #[test]
    fn notes_provider_completes_and_diagnoses() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "see [[Beta]] and [[Nope]] #rust\n");
        f.write("sub/Beta.md", "# Beta\nbody\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let rt = accent_lsp::runtime();
        let support = rt.block_on(f.vault.open_document(
            "a.md",
            "markdown",
            "see [[Beta]] and [[Nope]] #rust\n".to_string(),
        ));
        assert_eq!(support.unwrap().completion_triggers, ['[', '#', '(']);

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

        // A markdown link is judged by the path it names from the note's own folder.
        rt.block_on(f.vault.change_document(
            "a.md",
            "[b](sub/Beta.md) [n](sub/Gone%20Away.md)".to_string(),
        ))
        .unwrap();
        let Some(Event::Diagnostics { items, .. }) =
            f.wait(|e| matches!(e, Event::Diagnostics { .. }))
        else {
            panic!("a change has to say what is wrong with the note");
        };
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].message, "No note named sub/Gone Away.md");

        // A formula the preview cannot render is a warning over the whole of it, `$` and all.
        rt.block_on(
            f.vault
                .change_document("a.md", "[[Nope]]\nok $x^2$, not $\\left( x$\n".to_string()),
        )
        .unwrap();
        let Some(Event::Diagnostics { items, .. }) =
            f.wait(|e| matches!(e, Event::Diagnostics { .. }))
        else {
            panic!("a change has to say what is wrong with the note");
        };
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[1].severity, Severity::Warning);
        assert!(
            items[1]
                .message
                .starts_with("Cannot render this formula: unbalanced group"),
            "{items:?}"
        );
        let at = |character| Pos { line: 1, character };
        assert_eq!(
            items[1].range,
            Range {
                start: at(14),
                end: at(24)
            }
        );

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
                items[0].filter.as_deref(),
                Some("[[sub/Beta.md"),
                "the popup narrows by what was typed, `[[` and all"
            );
            assert_eq!(
                items[0].replace,
                Range {
                    start: at(4),
                    end: at(10)
                },
                "the trigger and the `]]` the auto-pair left both go"
            );

            // After a `#`, the headings of the note the link names.
            f.vault
                .change_document("a.md", "see [[Beta#]]".to_string())
                .await
                .unwrap();
            let items = f
                .vault
                .completion("a.md", at(11), None)
                .await
                .unwrap()
                .items;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].insert, "[[Beta#Beta]]");
            assert_eq!(
                items[0].replace,
                Range {
                    start: at(4),
                    end: at(13)
                }
            );

            // In a markdown link, this note's headings by the slug the link names them with.
            f.vault
                .change_document("a.md", "## My Section\n[x](#)".to_string())
                .await
                .unwrap();
            let on_link = |character| Pos { line: 1, character };
            let items = f.vault.completion("a.md", on_link(5), None).await.unwrap();
            assert_eq!(items.items.len(), 1, "headings, not tags");
            assert_eq!(items.items[0].label, "My Section");
            assert_eq!(items.items[0].insert, "#my-section");
            assert_eq!(
                items.items[0].replace,
                Range {
                    start: on_link(4),
                    end: on_link(5)
                },
                "the `)` the auto-pair left stays"
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

    /// `![[` offers every file, and a markdown link's destination offers paths from the note's
    /// own folder; each writes what the index resolves back to the file its row named.
    #[test]
    fn notes_provider_completes_embeds_and_link_paths() {
        let f = Fixture::open(VaultConfig::default());
        f.write("sub/n.md", "\n");
        f.write("Attachments/My Logo.png", "not really a png");
        f.write("a/x.md", "## My Part\n");
        f.write("b/c/x.md", "\n");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        accent_lsp::runtime().block_on(async {
            let at = |character| Pos { line: 0, character };
            f.vault
                .open_document("sub/n.md", "markdown", "![[]]".to_string())
                .await
                .unwrap();
            let items = f
                .vault
                .completion("sub/n.md", at(3), None)
                .await
                .unwrap()
                .items;
            let mut inserts: Vec<&str> = items.iter().map(|i| i.insert.as_str()).collect();
            inserts.sort();
            assert_eq!(
                inserts,
                ["[[My Logo.png]]", "[[b/c/x]]", "[[n]]", "[[x]]"],
                "`x` reaches a/x.md, the shorter path, so b/c/x.md is written out"
            );
            assert_eq!(
                items[0].replace,
                Range {
                    start: at(1),
                    end: at(5)
                },
                "the `!` stays, the `]]` the auto-pair left goes"
            );

            f.vault
                .change_document("sub/n.md", "[t]()".to_string())
                .await
                .unwrap();
            let items = f
                .vault
                .completion("sub/n.md", at(4), None)
                .await
                .unwrap()
                .items;
            let logo = items.iter().find(|i| i.label == "My Logo.png").unwrap();
            assert_eq!(logo.insert, "../Attachments/My%20Logo.png");
            assert_eq!(
                logo.replace,
                Range {
                    start: at(4),
                    end: at(4)
                },
                "the `)` the auto-pair left stays"
            );

            // What was typed is a relative, encoded path; the files it names are found anywhere.
            f.vault
                .change_document("sub/n.md", "[t](../My%20L)".to_string())
                .await
                .unwrap();
            let items = f
                .vault
                .completion("sub/n.md", at(13), None)
                .await
                .unwrap()
                .items;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].label, "My Logo.png");

            // And what it writes is followed from the same folder.
            f.vault
                .change_document("sub/n.md", "[t](../Attachments/My%20Logo.png)".to_string())
                .await
                .unwrap();
            let target = f.vault.definition("sub/n.md", at(1)).await.unwrap();
            assert_eq!(target.len(), 1);
            assert_eq!(target[0].path, "Attachments/My Logo.png");

            // After a `#`, the headings of the note the path names, by their slugs.
            f.vault
                .change_document("sub/n.md", "[t](../a/x.md#)".to_string())
                .await
                .unwrap();
            let items = f
                .vault
                .completion("sub/n.md", at(14), None)
                .await
                .unwrap()
                .items;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].label, "My Part");
            assert_eq!(items[0].insert, "../a/x.md#my-part");
            assert_eq!(items[0].replace.start, at(4));
        });
    }

    #[test]
    fn notes_provider_follows_links_both_ways() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "see [[Beta]]\n");
        f.write("sub/Beta.md", "intro\n\n# Beta\nbody\n");
        f.vault.rescan().unwrap();
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
                    ..Location::default()
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

            // A markdown anchor into this note: the slug completion writes, and the heading text
            // that resolved before it.
            f.vault
                .change_document(
                    "a.md",
                    "# Intro\n## My Section\n[x](#my-section) [y](#My%20Section)\n".to_string(),
                )
                .await
                .unwrap();
            for character in [1, 18] {
                let target = f
                    .vault
                    .definition("a.md", Pos { line: 2, character })
                    .await
                    .unwrap();
                assert_eq!(target[0].path, "a.md");
                assert_eq!(target[0].range.start.line, 1, "from character {character}");
            }
        });
    }

    /// A link the index cannot place is still defined: by the file on disk in a tree the walk
    /// never enters, and otherwise by the file New File would write for it, marked missing — a
    /// wikilink's path from the vault root, a markdown link's from the note's folder.
    #[test]
    fn a_link_nothing_answers_to_is_defined_where_new_file_would_write_it() {
        let f = Fixture::open(VaultConfig::default());
        let text = "[[Nowhere/Other Note#Part|there]]\n[t](../Else%20Where#Part)\n\
                    [[node_modules/pkg/Guide]]\n[m](mailto:a@b.c)\n[c](tel:+123)\n[s](sms:+123)\n";
        f.write("node_modules/pkg/Guide.md", "# Guide\n");
        f.write("Notes/Sub/a.md", text);
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        accent_lsp::runtime().block_on(async {
            f.vault
                .open_document("Notes/Sub/a.md", "markdown", text.to_string())
                .await
                .unwrap();
            let mut found = Vec::new();
            for line in 0..6 {
                let at = Pos { line, character: 3 };
                found.extend(f.vault.definition("Notes/Sub/a.md", at).await.unwrap());
            }
            let found: Vec<_> = found
                .iter()
                .map(|l| (l.path.as_str(), l.missing, l.is_url()))
                .collect();
            assert_eq!(
                found,
                [
                    ("Nowhere/Other Note.md", true, false),
                    ("Notes/Else Where.md", true, false),
                    ("node_modules/pkg/Guide.md", false, false),
                    ("mailto:a@b.c", false, true),
                    ("tel:+123", false, true),
                    ("sms:+123", false, true),
                ]
            );
        });
    }

    /// What a link's `#anchor` names beyond a line comes back beside the path: a PDF's page, and
    /// a heading the note does not have, which is answered with the top of the note.
    #[test]
    fn an_anchor_the_range_cannot_place_comes_back_with_the_file() {
        let f = Fixture::open(VaultConfig::default());
        let text = "# Intro\n[[#Nowhere]] [[#Intro]] [[b#Gone]] [[paper.pdf#page=3]]\n";
        f.write("a.md", text);
        f.write("b.md", "# B\n");
        f.write("paper.pdf", "not really a pdf");
        f.vault.rescan().unwrap();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        accent_lsp::runtime().block_on(async {
            f.vault
                .open_document("a.md", "markdown", text.to_string())
                .await
                .unwrap();
            let mut found = Vec::new();
            for character in [2, 15, 26, 40] {
                let at = Pos { line: 1, character };
                found.extend(f.vault.definition("a.md", at).await.unwrap());
            }
            let found: Vec<_> = found
                .iter()
                .map(|l| (l.path.as_str(), l.range.start.line, l.anchor.as_deref()))
                .collect();
            assert_eq!(
                found,
                [
                    ("a.md", 0, Some("Nowhere")),
                    ("a.md", 0, None),
                    ("b.md", 0, Some("Gone")),
                    ("paper.pdf", 0, Some("page=3")),
                ]
            );
        });
    }

    /// An index SQLite cannot read is a cache to build again rather than a vault that will not
    /// open — on the desktop, on Android and on a host alike, all of which open it here — and the
    /// vault's own files are never touched by it.
    #[test]
    fn an_unreadable_index_is_built_anew() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.md"), "# A\n").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let db = cache.path().join("index.db");
        std::fs::write(&db, vec![0x5a; 8192]).unwrap();

        let (vault, events) =
            Vault::open_unwatched_at(root.path(), &db, VaultConfig::default()).unwrap();
        let said = wait_for(&events, |e| matches!(e, Event::Error(_)), BUDGET);
        assert!(
            matches!(&said, Some(Event::Error(m)) if m.contains("index")),
            "{said:?}"
        );
        assert!(wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());
        assert_eq!(vault.file_paths(false).unwrap(), ["a.md"]);
        assert_eq!(
            std::fs::read_to_string(root.path().join("a.md")).unwrap(),
            "# A\n"
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
    /// Android opens vaults this way: nothing arrives on its own, and `rescan` is what the app
    /// calls when it comes back to the foreground.
    #[test]
    fn an_unwatched_vault_hears_nothing_until_it_is_asked_to_rescan() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Note.md"), "one\n").unwrap();
        let (vault, events) = Vault::open_unwatched(root.path(), VaultConfig::default()).unwrap();
        assert!(
            crate::tests::wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET)
                .is_some(),
            "the initial reconcile never finished"
        );

        // Well past the watcher's 300 ms debounce: a watched vault would have reported this.
        std::fs::write(root.path().join("Other.md"), "two\n").unwrap();
        assert!(
            crate::tests::wait_for(
                &events,
                |e| matches!(e, Event::FileChanged(_) | Event::DirsChanged(_)),
                Duration::from_millis(800)
            )
            .is_none(),
            "an unwatched vault reported a change nobody asked about"
        );

        vault.rescan().unwrap();
        assert!(
            crate::tests::wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET)
                .is_some(),
            "the rescan never finished"
        );
        assert!(
            vault
                .list_dir("")
                .unwrap()
                .iter()
                .any(|r| r.rel_path == "Other.md"),
            "the rescan did not pick the new note up"
        );
    }
}
