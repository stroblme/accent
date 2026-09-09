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

use accent_core::search;

use crate::local::Local;
use crate::{
    Backlink, Commit, DiffLine, Etag, Event, FileRow, Match, Options, PdfLink, RenamePlan,
    RenameReport, ReplaceReport, Repo, SaveError, SearchHit, Session, Status, Submodule,
    VaultConfig, fs, git, remote, rpc, ssh,
};

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
pub(crate) fn remote_err(e: rpc::RpcError) -> anyhow::Error {
    anyhow::anyhow!("{}", e.message)
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
/// What is *not* here is anything whose two sides differ: a save, which has an error of its own;
/// the searches, which compile their pattern where the files are; the transfers, which carry
/// bytes outside the protocol; and `repos`, which posts to the vault's own worker.
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

    ($(
        $(#[$doc:meta])*
        $group:ident $name:ident $(= $core:ident)? ($($arg:ident : $kind:tt $t:ty),* $(,)?) -> $ret:ty;
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
                            .call(stringify!($name), json!([$($arg),*]))
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
                    // One `arg` per position, counted by shadowing rather than by hand.
                    let _i = 0usize;
                    $(
                        let $arg: methods!(@wty $kind $t) = match rpc::arg(p, _i) {
                            Ok(value) => value,
                            Err(e) => return Some(Err(e)),
                        };
                        let _i = _i + 1;
                    )*
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
    any plan_rename(from: ref str, to: ref str) -> RenamePlan;
    any rename(plan: ref RenamePlan, rewrite_links: val bool) -> RenameReport;
    any adopt_conflict(original: ref str, conflict: ref str) -> Etag;
    any conflict_diff(original: ref str, conflict: ref str) -> Vec<DiffLine>;
    any template_target(template: ref str) -> Option<String>;
    any note_from_template(template: ref str) -> Option<(String, Vec<usize>)>;
    any render_template(template: ref str, title: ref str) -> (String, Vec<usize>);
    any templates() -> Vec<String>;

    // ------------------------------------------------------------ index reads
    any list_dir(rel: ref str) -> Vec<FileRow>;
    any search(query: ref str, limit: val usize, include_ignored: val bool) -> Vec<SearchHit>;
    any tags() -> Vec<(String, i64)>;
    any files_with_tag(tag: ref str) -> Vec<FileRow>;
    any backlinks(rel: ref str) -> Vec<Backlink>;
    /// The note links that highlight a page of this PDF. Asked of the host on a remote vault,
    /// because that is where the notes and the index are.
    any pdf_links(rel: ref str) -> Vec<PdfLink>;
    any note_paths() -> Vec<String>;
    any file_paths(include_ignored: val bool) -> Vec<String>;
    any set_excluded(entries: ref [String]) -> ();
    any recent_notes(limit: val usize) -> Vec<String>;
    any resolve_link(target: ref str) -> Option<String>;
    any conflicts() -> Vec<(String, String)>;
    any conflicts_of(rel: ref str) -> Vec<String>;

    // -------------------------------------------------------------------- git
    // The repositories belong to the machine the files are on, so every one of these runs there —
    // the `git` binary the user configured, with their hooks and their credential helper.
    git git_status = status(repo: ref Repo) -> Status;
    /// One page of history. The graph itself is computed where it is drawn: [`git::lanes`] is a
    /// forward pass over every commit so far, so the pane keeps the list and re-lanes it, and
    /// there is nothing in it for a remote host to do.
    git git_log = log(repo: ref Repo, skip: val usize, limit: val usize) -> Vec<Commit>;
    git git_show = show(repo: ref Repo, rev: ref str, path: ref str) -> Option<git::Blob>;
    git git_changed_files = changed_files(repo: ref Repo, oid: ref str) -> Vec<(char, String)>;
    git git_submodules = submodules(repo: ref Repo) -> Vec<Submodule>;
    git git_branches = branches(repo: ref Repo) -> Vec<String>;
    git git_checkout = checkout(repo: ref Repo, branch: ref str) -> ();
    git git_checkout_commit = checkout_commit(repo: ref Repo, oid: ref str) -> ();
    git git_create_branch = create_branch(repo: ref Repo, name: ref str, checkout: val bool) -> ();
    git git_delete_branch = delete_branch(repo: ref Repo, name: ref str, force: val bool) -> ();
    git git_commit = commit(repo: ref Repo, message: ref str, all: val bool) -> String;
    git git_sync = sync(repo: ref Repo) -> String;
    /// Bring the remote-tracking refs up to date. Bounded by [`git::FETCH_TIMEOUT`], which is
    /// under [`rpc::DEADLINE`] so that a fetch on a remote vault answers rather than times out.
    git git_fetch = fetch(repo: ref Repo) -> String;
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

    /// The repositories the vault touches. A failure is the caller's to see: offline used to read
    /// as "no repositories", which is what emptied the git pane on a dropped connection.
    pub fn repos(&self) -> Result<Vec<Repo>> {
        match &self.backend {
            Backend::Local(v) => Ok(v.repos()),
            Backend::Remote(r) => r.call("repos", json!([])).map_err(remote_err),
        }
    }

    /// `git` takes its paths as `&[&str]` and the wire carries owned strings, so these three
    /// borrow before they call rather than going through the table.
    pub fn git_stage(&self, repo: &Repo, paths: &[String]) -> Result<()> {
        self.git_paths("git_stage", git::stage, repo, paths)
    }

    pub fn git_unstage(&self, repo: &Repo, paths: &[String]) -> Result<()> {
        self.git_paths("git_unstage", git::unstage, repo, paths)
    }

    pub fn git_discard(&self, repo: &Repo, paths: &[String]) -> Result<()> {
        self.git_paths("git_discard", git::discard, repo, paths)
    }

    fn git_paths(
        &self,
        method: &str,
        run: fn(&Repo, &[&str]) -> Result<(), git::Error>,
        repo: &Repo,
        paths: &[String],
    ) -> Result<()> {
        match &self.backend {
            Backend::Local(_) => {
                let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
                run(repo, &borrowed).map_err(anyhow::Error::from)
            }
            Backend::Remote(r) => r.call(method, json!([repo, paths])).map_err(remote_err),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::*;
    use crate::{Event, Kind, Location, Pos, Range, Severity, Vault, VaultConfig};

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
}
