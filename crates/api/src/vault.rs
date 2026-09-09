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
