//! The one handle a window holds, and the only place that knows whether the files are on this
//! machine.
//!
//! Every method here means the same thing either way: a local vault answers from
//! [`Local`](crate::local::Local), a remote one asks the `accent-cli serve` at the other end of an
//! ssh connection. The UI was written against a local vault and did not have to learn anything to
//! work on a remote one, which is the whole point of the split.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use anyhow::Result;
use serde_json::json;

use accent_core::path::linked_path;

use crate::local::Local;
use crate::{
    Backlink, Commit, Etag, Event, FileRow, KeptLink, Location, Match, Options, PageEdit, PdfLink,
    RenamePlan, RenameReport, RepageReport, ReplaceReport, Repo, SaveError, SearchHit, Session,
    Stats, Status, Submodule, UndoReport, VaultConfig, fs, git, remote, rpc, ssh,
};

/// One open vault, wherever it lives.
///
/// A window holds exactly one of these and cannot tell the two apart: every method below means
/// the same thing whether the files are on this machine or on the other end of an ssh connection.
/// That is the whole point of the split — the UI was written against a local vault and did not
/// have to learn anything to work on a remote one.
///
/// Reads and writes are synchronous here, as they always were. A remote call is a round trip, and
/// every window of the desktop app shares one main thread, so it makes them on worker threads,
/// autosave included: a round trip once a second while someone typed held every window. What
/// stays where it is called is a write that has to land before its caller can go on: a tab
/// closing with its buffer dirty, and the writes behind a banner's buttons.
pub struct Vault {
    pub(crate) backend: Backend,
    /// What this vault is called in the config, the recents and the session file: the root for a
    /// local vault, the `ssh://` address for a remote one. Never a path to open.
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

    /// [`open`](Self::open) with no filesystem watcher: Android, where inotify over emulated
    /// storage drops events. Nothing arrives on its own there, so the app calls
    /// [`rescan`](Self::rescan) when it comes back to the foreground; its own writes still
    /// reach the index, since every one of them posts an update to the worker.
    pub fn open_unwatched(root: &Path, cfg: VaultConfig) -> Result<(Vault, Receiver<Event>)> {
        let db = accent_core::index::default_db_path(root);
        Vault::open_unwatched_at(root, &db, cfg)
    }

    /// [`open_unwatched`](Self::open_unwatched) with an explicit index file: `accent-cli`, which
    /// is done before a change could matter.
    pub fn open_unwatched_at(
        root: &Path,
        db: &Path,
        cfg: VaultConfig,
    ) -> Result<(Vault, Receiver<Event>)> {
        let (local, events) = Local::open_with(root, db, cfg, false)?;
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

    /// Never waits, so the main loop can make it: a remote vault sends this and the two below on
    /// to the host from a thread of its own, in the order they were made.
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

    /// Ask for a full walk: after a resume, or when the UI suspects it missed something.
    ///
    /// A local vault posts to its own worker and cannot fail; a remote one is a round trip, and
    /// a failure is the caller's to see rather than something to drop on the floor.
    pub fn rescan(&self) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => {
                v.rescan();
                Ok(())
            }
            Backend::Remote(r) => r.call("rescan", json!([])).map_err(remote_err),
        }
    }

    /// Stop the walk that is running, keeping every row it has already written.
    ///
    /// A pause rather than a cancel: the reconcile it ends reports
    /// [`ReconcileStats::stopped`](accent_core::index::ReconcileStats::stopped), the vault stays
    /// usable with the part of the index that exists, and the remainder is indexed by
    /// [`resume_indexing`](Self::resume_indexing) or by the next open of the vault. A remote
    /// vault's walk runs on the host, so this is a round trip to the worker there; the server
    /// gives every request a thread of its own, so it is answered while that walk runs.
    pub fn stop_indexing(&self) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => {
                v.stop_indexing();
                Ok(())
            }
            Backend::Remote(r) => r.call("stop_indexing", json!([])).map_err(remote_err),
        }
    }

    /// Walk again after [`stop_indexing`](Self::stop_indexing), which is the only thing that
    /// will: a paused vault ignores every other reason to rescan.
    pub fn resume_indexing(&self) -> Result<()> {
        match &self.backend {
            Backend::Local(v) => {
                v.resume_indexing();
                Ok(())
            }
            Backend::Remote(r) => r.call("resume_indexing", json!([])).map_err(remote_err),
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

/// How much longer than a round trip a remote move may take: a folder of a few thousand notes
/// is an index query and a read each to plan, and an fsync per rewritten note to apply.
///
/// An estimate of the work rather than a cap on it. The host runs a move to the end whatever
/// happens, because the files have already moved by the time the links are being rewritten and
/// there is no second run that could finish them: a move cut half way through would leave links
/// naming the wrong place with nothing left to fix them from.
pub(crate) const MOVE_BOUND: std::time::Duration = std::time::Duration::from_secs(50);

/// How long a vault-wide rewrite runs on the host before it stops. [`Local::replace_all`] reads,
/// substitutes and fsyncs one file at a time — 1.9 s across 245 notes and 35 s across 3.3k of
/// them — so this is room for some ten thousand and a cap on the thread either way.
///
/// A cap, unlike [`MOVE_BOUND`], because a rewrite *is* resumable: it stops between files, never
/// inside one, the files it did not reach are listed in the report, and the Search pane asks its
/// question again the moment it returns — so the remaining matches are on screen and Replace All
/// finishes them.
pub(crate) const REPLACE_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// Turn an RPC failure into the `anyhow` error every caller of the façade already handles. It
/// reads as its message, and a caller that has to know whether the host was asked at all
/// downcasts it ([`rpc::RpcError::unasked`]).
pub(crate) fn remote_err(e: rpc::RpcError) -> anyhow::Error {
    anyhow::Error::new(e)
}

/// The methods that mean the same thing wherever the files are, written once.
///
/// Each line here used to be three that had to agree: a [`Vault`] method with a local arm and a
/// remote one, an arm of the rpc dispatch answering it on the host, and the wire name spelling
/// them the same. The name is now the wire name by construction, and a method that is added or
/// removed is added or removed everywhere at once.
///
/// The first word is what the answer travels as. `io` is an [`io::Result`]; `any` is an
/// [`anyhow::Result`], which crosses as a formatted message; `git` runs where the repository is
/// and names the [`accent_core::git`] function after the wire name, because that name carries a
/// `git_` prefix the module does not.
///
/// An argument marked `ref` is taken by reference and travels as the owned form of its type
/// (`str` as a `String`); one marked `val` travels as itself.
///
/// A method the host runs under a timeout of its own ends with `bounded by` that timeout, and a
/// remote caller waits for it plus [`rpc::DEADLINE`] rather than the deadline alone. Without it a
/// merge whose hooks ran past ten seconds timed out here while the host carried on and landed it.
///
/// What is *not* here is anything whose two sides differ: a save, which has an error of its own;
/// the walk for unindexed matches, whose `stop` cannot cross; the transfers, which carry bytes
/// outside the protocol; and `repos`, which posts to the vault's own worker.
macro_rules! methods {
    // What one argument is, on the façade, on the wire, and at the call the host makes.
    (@fty ref $t:ty) => { &$t };
    (@fty val $t:ty) => { $t };
    (@wty ref $t:ty) => { <$t as ToOwned>::Owned };
    (@wty val $t:ty) => { $t };
    (@pass ref $n:ident) => { &$n };
    (@pass val $n:ident) => { $n };

    // What the answer is, and how a remote failure reads as one.
    (@ret io $t:ty) => { io::Result<$t> };
    (@ret any $t:ty) => { Result<$t> };
    (@ret git $t:ty) => { Result<$t> };
    (@err io) => { rpc::RpcError::io_error };
    (@err any) => { remote_err };
    (@err git) => { remote_err };

    // The call itself: on this machine, and on the host answering for it.
    (@here $v:ident io $name:ident ($($a:tt)*)) => { $v.$name($($a)*) };
    (@here $v:ident any $name:ident ($($a:tt)*)) => { $v.$name($($a)*) };
    (@here $v:ident git $name:ident $core:ident ($($a:tt)*)) => {
        git::$core($($a)*).map_err(anyhow::Error::from)
    };
    (@serve $v:ident io $name:ident ($($a:tt)*)) => { rpc::io($v.$name($($a)*)) };
    (@serve $v:ident any $name:ident ($($a:tt)*)) => { rpc::any($v.$name($($a)*)) };
    (@serve $v:ident git $name:ident $core:ident ($($a:tt)*)) => {
        rpc::git_result(git::$core($($a)*))
    };

    // How long a remote caller waits: the host's own bound, if it has one, and a round trip.
    (@deadline) => { rpc::DEADLINE };
    (@deadline $bound:expr) => { $bound + rpc::DEADLINE };

    ($(
        $(#[$doc:meta])*
        $group:ident $name:ident $(= $core:ident)? ($($arg:ident : $kind:tt $t:ty),* $(,)?) -> $ret:ty
            $(, bounded by $bound:expr)?;
    )*) => {
        impl Vault {
            $(
                $(#[$doc])*
                pub fn $name(&self $(, $arg: methods!(@fty $kind $t))*)
                    -> methods!(@ret $group $ret)
                {
                    match &self.backend {
                        Backend::Local(_v) => methods!(@here _v $group $name $($core)? ($($arg),*)),
                        Backend::Remote(r) => r
                            .call_within(
                                stringify!($name),
                                json!([$($arg),*]),
                                methods!(@deadline $($bound)?),
                            )
                            .map_err(methods!(@err $group)),
                    }
                }
            )*
        }

        /// The half of the rpc dispatch this table writes: `None` for a method that is not one of
        /// these, which is [`rpc::dispatch`]'s cue to try the ones it spells out itself.
        pub(crate) fn dispatch(
            vault: &Local,
            method: &str,
            p: &serde_json::Value,
        ) -> Option<Result<serde_json::Value, rpc::RpcError>> {
            $(
                if method == stringify!($name) {
                    rpc::args!(p; $($arg: methods!(@wty $kind $t)),*);
                    return Some(methods!(@serve vault $group $name $($core)?
                        ($(methods!(@pass $kind $arg)),*)));
                }
            )*
            None
        }
    };
}

methods! {
    // ------------------------------------------------------------------ files
    io read(rel: ref str) -> (String, Etag);
    io read_text(rel: ref str) -> fs::Read;
    io stat(rel: ref str) -> Option<Etag>;
    /// Delete a file or a directory. Local vaults go to the system trash through the desktop, so
    /// this is the remote path only — permanent, and confirmed as such by the UI.
    io delete(rel: ref str) -> ();
    io create_dir(rel: ref str) -> ();
    /// Take a file written behind the index's back into it at once, as a local write is, rather
    /// than a watcher debounce later: what [`write_file`](Vault::write_file) asks of a host once
    /// the bytes are there.
    io wrote(rel: ref str) -> ();
    /// Copy a file or a whole directory inside the vault. It runs where the files are, so a
    /// paste inside a remote vault sends nothing over the link; overwriting is not its business,
    /// the caller naming a path nothing holds yet.
    io copy(from: ref str, to: ref str) -> ();
    any plan_moves(moves: ref [(String, String)]) -> RenamePlan, bounded by MOVE_BOUND;
    any rename(plan: ref RenamePlan, update: val bool) -> RenameReport, bounded by MOVE_BOUND;
    /// A note's text as Save As writes it at another path, its relative links pointed back at
    /// what they named: asked where the index is.
    any relink_copy(from: ref str, to: ref str, text: ref str) -> Option<String>;
    /// The notes' links into a PDF after a page edit, rewritten where the notes are.
    any repage_links(rel: ref str, edit: val PageEdit, keep: ref [KeptLink]) -> RepageReport,
        bounded by MOVE_BOUND;
    any adopt_conflict(original: ref str, conflict: ref str) -> Etag;
    any template_target(template: ref str) -> Option<String>;
    any note_from_template(template: ref str) -> Option<(String, Vec<usize>)>;
    any render_template(template: ref str, title: ref str) -> (String, Vec<usize>);
    any templates() -> Vec<String>;
    /// The templates that name a target: one question for New from Template rather than one
    /// [`template_target`](Vault::template_target) per template, a round trip each when remote.
    any template_targets() -> Vec<String>;
    /// Replace every match in every file whose indexed body has one: what
    /// [`grep`](Vault::grep) counts under the same `include_ignored`. The pattern crosses as what
    /// the user typed plus its toggles, and is compiled where the files are.
    any replace_all(
        query: ref str,
        options: val Options,
        replacement: ref str,
        literal: val bool,
        include_ignored: val bool,
    ) -> ReplaceReport, bounded by REPLACE_BOUND;
    /// Put back what the last [`replace_all`](Vault::replace_all) rewrote. The text it needs
    /// stayed wherever the rewrite ran, the host on a remote vault, so only the report crosses.
    any undo_replace() -> UndoReport, bounded by REPLACE_BOUND;

    // ------------------------------------------------------------ index reads
    any list_dir(rel: ref str) -> Vec<FileRow>;
    /// Keep the listings of these folders, which the index does not walk, fresh until
    /// [`unwatch_unindexed`](Vault::unwatch_unindexed): a file made, removed or renamed directly
    /// inside one comes back as [`Event::UnindexedChanged`]. Watched where the files are, one
    /// level each, never the tree under one.
    any watch_unindexed(dirs: ref [String]) -> ();
    any unwatch_unindexed(dirs: ref [String]) -> ();
    /// Walk one folder of the vault (Reload on its row), where [`rescan`](Vault::rescan) walks
    /// all of it: what the watcher never reported under it is taken in.
    io rescan_dir(dir: ref str) -> ();
    any search(query: ref str, limit: val usize, include_ignored: val bool) -> Vec<SearchHit>;
    any search_mid_word(
        query: ref str,
        limit: val usize,
        include_ignored: val bool,
        skip: ref [String],
    ) -> Vec<SearchHit>;
    any grep(query: ref str, options: val Options, limit: val usize, include_ignored: val bool)
        -> (Vec<Match>, usize);
    any tags() -> Vec<(String, i64)>;
    /// What the index holds, counted: `accent-cli stats`.
    any stats() -> Stats;
    any files_with_tag(tag: ref str) -> Vec<FileRow>;
    any backlinks(rel: ref str) -> Vec<Backlink>;
    any backlink_locations(rel: ref str) -> Vec<Location>;
    /// The note links that highlight a page of this PDF. Asked of the host on a remote vault,
    /// because that is where the notes and the index are.
    any pdf_links(rel: ref str) -> Vec<PdfLink>;
    any file_paths(include_ignored: val bool) -> Vec<String>;
    any set_excluded(entries: ref [String]) -> ();
    any recent_files(limit: val usize) -> Vec<String>;
    any resolve_link(target: ref str) -> Option<String>;
    /// What Go to File and `[[` completion offer to write.
    any missing_notes() -> Vec<String>;
    /// `(alias, note)` for every frontmatter alias: what Go to File also finds a note by.
    any note_aliases() -> Vec<(String, String)>;
    any conflicts() -> Vec<(String, String)>;
    any conflicts_of(rel: ref str) -> Vec<String>;

    // -------------------------------------------------------------------- git
    // The repositories belong to the machine the files are on, so every one of these runs there —
    // the `git` binary the user configured, with their hooks and their credential helper.
    git git_status = status(repo: ref Repo) -> Status;
    git git_untracked = untracked(repo: ref Repo, dir: ref str) -> Vec<String>;
    /// One page of history. The graph itself is computed where it is drawn: [`git::lanes`] is a
    /// forward pass over every commit so far, so the pane keeps the list and re-lanes it, and
    /// there is nothing in it for a remote host to do.
    git git_log = log(repo: ref Repo, skip: val usize, limit: val usize) -> Vec<Commit>;
    git git_show = show(repo: ref Repo, rev: ref str, path: ref str) -> Option<git::Blob>;
    git git_changed_files = changed_files(repo: ref Repo, oid: ref str) -> Vec<git::ChangedFile>;
    git git_submodules = submodules(repo: ref Repo) -> Vec<Submodule>;
    git git_branches = branches(repo: ref Repo) -> git::Branches;
    git git_checkout = checkout(repo: ref Repo, branch: ref str) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git git_track = track(repo: ref Repo, remote: ref str) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git git_checkout_commit = checkout_commit(repo: ref Repo, oid: ref str) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git git_create_branch = create_branch(repo: ref Repo, name: ref str, checkout: val bool) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git git_delete_branch = delete_branch(repo: ref Repo, name: ref str, force: val bool) -> ();
    git git_merge = merge(repo: ref Repo, branch: ref str) -> git::Merge,
        bounded by git::TRANSFER_TIMEOUT;
    git git_merge_abort = merge_abort(repo: ref Repo) -> ();
    /// A rebase stopped part way, carried on or given up.
    git git_rebase_continue = rebase_continue(repo: ref Repo) -> git::Rebase,
        bounded by git::TRANSFER_TIMEOUT;
    git git_rebase_abort = rebase_abort(repo: ref Repo) -> ();
    git git_stage = stage(repo: ref Repo, paths: ref [String]) -> ();
    git git_unstage = unstage(repo: ref Repo, paths: ref [String]) -> ();
    git git_discard = discard(repo: ref Repo, paths: ref [String]) -> ();
    /// What Stage and Unstage Selected Lines write.
    git git_stage_text = stage_text(repo: ref Repo, path: ref str, text: ref str) -> (),
        bounded by git::TRANSFER_TIMEOUT;
    git git_commit = commit(repo: ref Repo, message: ref str, all: val bool) -> String,
        bounded by git::TRANSFER_TIMEOUT;
    /// A Sync's two halves, asked for one after the other so the window knows which is running.
    git git_pull = pull(repo: ref Repo) -> String, bounded by git::TRANSFER_TIMEOUT;
    git git_push = push(repo: ref Repo) -> String, bounded by git::TRANSFER_TIMEOUT;
    /// Bring the remote-tracking refs up to date.
    git git_fetch = fetch(repo: ref Repo) -> String, bounded by git::FETCH_TIMEOUT;
    /// The oids a pull would bring in. Asked only where [`git::Status`] says there are any.
    git git_incoming = incoming(repo: ref Repo) -> Vec<String>;
}

/// The rest: the methods whose two sides really do differ.
impl Vault {
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

    /// A path on *this* machine holding `rel`'s current bytes: the file itself when the vault is
    /// local, a cached copy fetched over ssh when it is not. For the readers that need a real
    /// file — the PDF viewer, an image, the preview's assets. `NotFound` when there is nothing at
    /// `rel`, on either backend.
    pub fn fetch(&self, rel: &str) -> io::Result<PathBuf> {
        self.fetch_with(rel, &|_, _| ())
    }

    /// [`fetch`](Self::fetch), telling `progress` the bytes so far and how many there are while
    /// a remote vault's copy downloads. A local file is already here, so it tells nothing.
    pub fn fetch_with(&self, rel: &str, progress: &dyn Fn(u64, u64)) -> io::Result<PathBuf> {
        match &self.backend {
            Backend::Local(v) => v
                .resolve(rel)
                .and_then(|path| std::fs::metadata(&path).map(|_| path)),
            Backend::Remote(r) => r.fetch_with(rel, progress),
        }
    }

    /// Copy a file from this machine into the vault.
    pub fn upload(&self, local: &Path, rel: &str) -> io::Result<()> {
        self.upload_with(local, rel, &|_, _| ())
    }

    /// [`upload`](Self::upload), telling `progress` the bytes sent so far and how many there are
    /// on a remote vault. A local copy is the disk's speed, and tells nothing.
    pub fn upload_with(
        &self,
        local: &Path,
        rel: &str,
        progress: &dyn Fn(u64, u64),
    ) -> io::Result<()> {
        match &self.backend {
            Backend::Local(v) => std::fs::copy(local, v.resolve(rel)?).map(|_| ()),
            Backend::Remote(r) => r.upload_with(local, rel, progress),
        }
    }

    /// Write `bytes` to a file nothing holds yet: an image pasted or dropped into a note. The bytes
    /// travel as an upload's do, over ssh rather than in the protocol.
    pub fn write_file(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        match &self.backend {
            Backend::Local(v) => v.write_file(rel, bytes),
            Backend::Remote(r) => r.write_file(rel, bytes),
        }
    }

    /// Copy a file out of the vault to somewhere on this machine.
    pub fn download(&self, rel: &str, dest: &Path) -> io::Result<()> {
        self.download_with(rel, dest, &|_, _| ())
    }

    /// [`download`](Self::download), telling `progress` the bytes so far and how many there are
    /// on a remote vault.
    pub fn download_with(
        &self,
        rel: &str,
        dest: &Path,
        progress: &dyn Fn(u64, u64),
    ) -> io::Result<()> {
        match &self.backend {
            Backend::Local(v) => std::fs::copy(v.resolve(rel)?, dest).map(|_| ()),
            Backend::Remote(r) => r.download_with(rel, dest, progress),
        }
    }

    /// Create a file, optionally from a template. The template is an `Option<&str>`, which is
    /// neither a reference the table can borrow nor a value it can send, so this one is spelled
    /// out.
    pub fn create_note(&self, rel: &str, template: Option<&str>) -> Result<(String, Vec<usize>)> {
        match &self.backend {
            Backend::Local(v) => v.create_note(rel, template),
            Backend::Remote(r) => r
                .call("create_note", json!([rel, template]))
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
    ///
    /// `stop` ends a local walk early. It cannot cross to a host, so a remote walk runs to its
    /// budget and the caller drops an answer it no longer wants.
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
            Backend::Remote(r) => r
                .call("grep_unindexed", json!([query, options, limit]))
                .map_err(remote_err),
        }
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

    /// Which vault file following a link to `target` opens, or `None` when nothing is there and
    /// the UI can offer to write it.
    ///
    /// The index answers first. A file in a tree it does not hold — a gitignored `build/` it has
    /// not walked, a `node_modules` it never enters — is still there to open, so the file New
    /// File would write for the link ([`linked_path`]) is looked for on disk before it is offered:
    /// one `stat`, and only for a link the index could not place.
    ///
    /// That `stat` is asked through [`stat`](Self::stat) rather than [`exists`](Self::exists),
    /// which cannot say why it answered `false`: on a remote vault that is not answering, a link
    /// to a note that is really there read as one to write, and a click offered New File over
    /// it. Same reasoning as [`repos`](Self::repos) below.
    ///
    /// What a click in the preview follows. Go to Definition asks the note's language provider
    /// instead, which answers the same way on the host, beside the files.
    pub fn follow(&self, target: &str) -> Result<Option<String>> {
        if let Some(rel) = self.resolve_link(target)? {
            return Ok(Some(rel));
        }
        let rel = linked_path(target);
        Ok(self.stat(&rel)?.map(|_| rel))
    }

    /// The repositories the vault touches. A failure is the caller's to see: offline used to read
    /// as "no repositories", which is what emptied the git pane on a dropped connection.
    pub fn repos(&self) -> Result<Vec<Repo>> {
        match &self.backend {
            Backend::Local(v) => Ok(v.repos()),
            Backend::Remote(r) => r.call("repos", json!([])).map_err(remote_err),
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

    /// Replacing across a vault is minutes of fsyncs, so the desktop app runs it on a worker
    /// thread. That only compiles while the handle can cross one.
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
            f.vault.fetch("gone.png").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
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
