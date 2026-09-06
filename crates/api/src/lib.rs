//! accent-api: the UI-facing façade. Plain serde data types only; no GTK, no Android types.
//! Desktop links this directly; Android gets uniffi bindings of this crate; the CLI renders it as JSON-RPC over stdio.
//!
//! [`Vault`] owns the lifecycle of one open vault: three SQLite connections, the filesystem
//! watcher, and the batching that turns a Syncthing pull of 500 files into a single [`Event`].
//! The caller reads on its own connection and never waits for the worker, which is what keeps a
//! UI thread free while the vault is being indexed.

use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, MutexGuard};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use accent_core::index::{Change, Index};
use accent_core::walk;
use accent_core::watch::{VaultEvent, Watcher};
use accent_core::{diff, fs, markdown, template};

pub use accent_core::config::{Config, Session, VaultConfig};
pub use accent_core::diff::{DiffLine, Op};
pub use accent_core::fs::{Etag, SaveError};
// The module as well as its types: the git operations take a `Repo`, not a `Vault`, so callers
// reach them as `accent_api::git::status(&repo)` after asking the vault which repos there are.
pub use accent_core::git;
pub use accent_core::git::{Branch, Commit, Entry, LogRow, Repo, Status, Submodule};
pub use accent_core::index::{
    Backlink, FileRow, HeadingRow, Match, Progress, ReconcileStats, SearchHit, Stats,
};
pub use accent_core::search::{self, Options, Regex};
pub use accent_core::walk::FileKind;

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
    Error(String),
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

/// One open vault: the index, the watcher, and the worker thread that owns both writers.
pub struct Vault {
    root: PathBuf,
    /// The caller's connection. The mutex is not about contention (WAL readers never block):
    /// it is what makes `Vault` `Send + Sync`, which uniffi will need in Phase 3.
    index: Mutex<Index>,
    /// A reader of its own for the sidebar's search, which runs on a worker thread. A regex scan
    /// of every note holds its connection for as long as it takes, and the main thread's
    /// `list_dir` and `backlinks` must never queue behind one on the same mutex.
    search: Mutex<Index>,
    cfg: Mutex<VaultConfig>,
    tx: Sender<Msg>,
    worker: Option<JoinHandle<()>>,
}

// ---------------------------------------------------------------------- open

impl Vault {
    /// Open `root` with its index in the shared cache directory.
    pub fn open(root: &Path, cfg: VaultConfig) -> Result<(Vault, Receiver<Event>)> {
        let db = accent_core::index::default_db_path(root);
        Vault::open_at(root, &db, cfg)
    }

    /// [`open`](Self::open) with an explicit index file, for tests and tooling.
    pub fn open_at(root: &Path, db: &Path, cfg: VaultConfig) -> Result<(Vault, Receiver<Event>)> {
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
        let worker = Worker {
            root: root.clone(),
            index: writer,
            rx,
            tx: tx.clone(),
            events,
            watcher: None,
            symlinks: Vec::new(),
            seen_conflicts: BTreeSet::new(),
        };
        let handle = std::thread::Builder::new()
            .name("accent-vault".to_string())
            .spawn(move || worker.run())
            .context("spawning the vault worker")?;

        Ok((
            Vault {
                root,
                index: Mutex::new(index),
                search: Mutex::new(search),
                cfg: Mutex::new(cfg),
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
        self.locked(&self.cfg).clone()
    }

    pub fn set_config(&self, cfg: VaultConfig) {
        *self.locked(&self.cfg) = cfg;
    }

    /// Ask for a full walk: after a resume, or when the UI suspects it missed something.
    pub fn rescan(&self) {
        self.post(Msg::Rescan);
    }

    /// Join `rel` to the vault root, refusing anything that would land outside it. Every
    /// path-taking method goes through this: the GTK app sanitises its own input, but a
    /// `daily_dir` of `../Outside` arrives here straight from the config, and Phase 2's MCP
    /// server and Phase 3's Android bindings call the façade with whatever their caller said.
    ///
    /// The test is deliberately lexical and never `canonicalize`s: a vault links external
    /// directories in on purpose, so resolving would reject the very paths the walk indexed and
    /// a note reached through a directory symlink has to stay openable.
    pub fn resolve(&self, rel: &str) -> io::Result<PathBuf> {
        let mut out = self.root.clone();
        for part in Path::new(rel).components() {
            match part {
                Component::Normal(name) => out.push(name),
                Component::CurDir => {}
                Component::ParentDir => {
                    // `..` may walk back down to the root, never past it.
                    out.pop();
                    if !out.starts_with(&self.root) {
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
        self.locked(&self.index)
    }

    fn searcher(&self) -> MutexGuard<'_, Index> {
        self.locked(&self.search)
    }

    /// A panic in one query must not take the whole vault down with it, so a poisoned lock is
    /// used rather than propagated: the index is a cache and the next query rebuilds what it needs.
    fn locked<'a, T>(&self, m: &'a Mutex<T>) -> MutexGuard<'a, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn post(&self, msg: Msg) {
        if self.tx.send(msg).is_err() {
            tracing::debug!("vault worker is gone; dropping an index update");
        }
    }
}

impl Drop for Vault {
    /// Stop the worker before the vault goes away, so no thread outlives the window that opened it.
    ///
    /// ponytail: the join waits for whatever the worker is doing, and a cold reconcile of a large
    /// vault takes seconds. Give `reconcile` a cancellation flag if closing a window ever stalls.
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

// --------------------------------------------------------------------- files

impl Vault {
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

    /// Create a note, optionally from a template. Returns the final path, which may have gained
    /// the `.md` the user did not type, and where the caret belongs.
    pub fn create_note(
        &self,
        rel: &str,
        template: Option<&str>,
    ) -> Result<(String, Option<usize>)> {
        let rel = with_md(rel);
        let (text, cursor) = match template {
            Some(t) => {
                let (raw, _) = fs::read_note(&self.resolve(t)?)
                    .with_context(|| format!("reading template {t}"))?;
                template::render(&raw, &stem(&rel), chrono::Local::now().naive_local())
            }
            None => (String::new(), None),
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
    /// ponytail: the rewrite runs on the calling thread, like every other write here. It reads,
    /// substitutes and fsyncs one note at a time, so a replace across thousands of notes will
    /// stall the caller; move it to the worker with a progress event if that ever bites.
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

    /// Today's note, created from the configured template the first time it is asked for.
    pub fn daily_note(&self) -> Result<(String, Option<usize>)> {
        let cfg = self.config();
        let name = template::strftime(&cfg.daily_pattern, chrono::Local::now().naive_local())
            .with_context(|| {
                format!(
                    "daily_pattern {:?} is not a strftime format",
                    cfg.daily_pattern
                )
            })?;
        let rel = with_md(&if cfg.daily_dir.is_empty() {
            name
        } else {
            format!("{}/{name}", cfg.daily_dir.trim_end_matches('/'))
        });
        if self.resolve(&rel)?.exists() {
            return Ok((rel, None));
        }
        self.create_note(&rel, cfg.daily_template.as_deref())
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

impl Vault {
    /// Direct children of one directory ("" is the vault root): one level per call, so the tree
    /// costs what it shows.
    pub fn list_dir(&self, rel: &str) -> Result<Vec<FileRow>> {
        self.index().list_files(rel)
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

    /// Exact search: one row per match of `re`, capped at `limit`, plus the total match count.
    /// `include_ignored` means what it does in [`search`](Self::search).
    pub fn grep(
        &self,
        re: &Regex,
        limit: usize,
        include_ignored: bool,
    ) -> Result<(Vec<Match>, usize)> {
        self.searcher().grep(re, limit, include_ignored)
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

    /// Hand the index what git ignores, so every later query can leave it out.
    ///
    /// Called from the git refresh, which is the one place in the app that has already asked git.
    /// This is the only write that does not go through the vault worker; the connections carry a
    /// busy timeout so a reconcile in flight costs a wait rather than a lost update.
    pub fn set_git_ignored(&self, entries: &[String]) -> Result<()> {
        self.index().set_git_ignored(entries)
    }

    pub fn recent_notes(&self, limit: usize) -> Result<Vec<String>> {
        self.index().recent_notes(limit)
    }

    pub fn headings(&self, rel: &str) -> Result<Vec<HeadingRow>> {
        self.index().headings(rel)
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

    /// Notes for the `[[` completion: prefix on the name or the whole path, shortest path first
    /// because that is the one the user most likely means.
    pub fn complete_notes(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let prefix = prefix.to_lowercase();
        let mut hits: Vec<String> = self
            .index()
            .note_paths()?
            .into_iter()
            .filter(|rel| {
                markdown::strip_ext(basename(rel))
                    .to_lowercase()
                    .starts_with(&prefix)
                    || rel.to_lowercase().starts_with(&prefix)
            })
            .collect();
        // Stable, so paths of equal length keep the index's alphabetical order.
        hits.sort_by_key(String::len);
        hits.truncate(limit);
        Ok(hits)
    }

    /// Tags for the `#` completion, most used first.
    pub fn complete_tags(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let prefix = prefix.to_lowercase();
        Ok(self
            .index()
            .tags()?
            .into_iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&prefix))
            .map(|(name, _)| name)
            .take(limit)
            .collect())
    }

    pub fn session(&self) -> Session {
        Session::load(&self.root)
    }

    pub fn save_session(&self, s: &Session) -> Result<()> {
        s.save(&self.root)
    }
}

// ----------------------------------------------------------------------- git

impl Vault {
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
        git::discover(&self.root, &dirs)
    }
}

// -------------------------------------------------------------------- worker

/// The worker's inbox. The watcher pushes `Fs`, the public API pushes the rest.
enum Msg {
    Fs(VaultEvent),
    Update { rel: String, own: bool },
    Rescan,
    Shutdown,
}

/// Owns the writing connection and the watcher. Everything here runs on one thread, so the
/// index has exactly one writer and the UI never waits for it.
struct Worker {
    root: PathBuf,
    index: Index,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    events: Sender<Event>,
    watcher: Option<Watcher>,
    /// `(canonical target, rel_path of the link)`, longest target first: inotify reports paths
    /// inside a symlinked directory by their real location, and the UI needs the vault path.
    symlinks: Vec<(PathBuf, String)>,
    /// Conflict copies the UI has already been offered, so a rescan never repeats one.
    seen_conflicts: BTreeSet<String>,
}

/// What one batch has accumulated: the directories whose children changed, the paths it took out
/// of the index, and whether anything happened that link resolution has to see.
#[derive(Default)]
struct Batch {
    dirs: BTreeSet<String>,
    removed: BTreeSet<String>,
    resolve: bool,
    /// A directory was added, so the watch set — one watch per directory — is short one entry.
    rewatch: bool,
}

impl Worker {
    fn run(mut self) {
        // Watch before walking, so a change made during the first reconcile is not lost between
        // the two.
        //
        // ponytail: on a cold cache the index lists no directories yet, so this first watcher
        // covers the vault root alone and a change made deeper during the initial reconcile is
        // missed until the rebuild below. Build the watch set from the scan result instead of
        // the cache if that ever bites.
        self.rebuild_watcher();
        self.reconcile();

        while let Ok(first) = self.rx.recv() {
            // One `recv` plus the rest of the burst: a Syncthing pull of 500 files is one batch,
            // and therefore one event for the UI.
            let mut batch = vec![first];
            batch.extend(self.rx.try_iter());
            let stop = batch.iter().any(|m| matches!(m, Msg::Shutdown));
            self.process(batch);
            if stop {
                break;
            }
        }
    }

    fn process(&mut self, batch: Vec<Msg>) {
        if batch.iter().any(|m| self.needs_rescan(m)) {
            // The walk replaces the index wholesale, but the moves in this batch are still news:
            // a tab open on a path that was renamed under it has to follow.
            for msg in &batch {
                if let Msg::Fs(VaultEvent::Renamed { from, to }) = msg
                    && let (Some(from), Some(to)) = (self.rel(from), self.rel(to))
                {
                    self.emit(Event::FileRenamed { from, to });
                }
            }
            self.reconcile();
            return;
        }
        let mut batched = Batch::default();
        for msg in batch {
            match msg {
                Msg::Rescan | Msg::Shutdown => {}
                Msg::Update { rel, own } => self.update(&rel, own, &mut batched),
                Msg::Fs(ev) => self.apply(ev, &mut batched),
            }
        }
        // Once for the batch, not once per file: resolution is a whole-vault pass (225 ms at the
        // 56k links of `testvault/`), so a Syncthing pull of 100 notes would otherwise hold the
        // worker — and anyone closing the window, which joins it — for twenty seconds.
        if batched.resolve
            && let Err(e) = self.index.resolve_links()
        {
            self.fail("resolving links", e);
        }
        // A new directory is watched from now on, not from the next reconcile: watches are
        // per-directory, so `mkdir Ideas` followed by a write into it would otherwise be silent.
        if batched.rewatch {
            self.rebuild_watcher();
        }
        if !batched.dirs.is_empty() {
            self.emit(Event::DirsChanged(batched.dirs.into_iter().collect()));
        }
    }

    fn needs_rescan(&self, msg: &Msg) -> bool {
        match msg {
            Msg::Rescan | Msg::Fs(VaultEvent::Rescan) => true,
            // A directory that shows up with children was moved in whole, and inotify reports
            // nothing about what is inside it: only a walk can find those files. A directory that
            // was renamed — by us or in a terminal — is the same story, and worse: the removal of
            // the old name drops the subtree, and indexing the new one adds back the directory row
            // alone, so every note under it would vanish until the next restart. An *empty* new
            // directory needs no walk, only a watch: see `Batch::rewatch`.
            Msg::Fs(VaultEvent::Changed(p)) => *p != self.root && has_children(p),
            Msg::Fs(VaultEvent::Renamed { to, .. }) => *to != self.root && has_children(to),
            Msg::Update { rel, .. } => !rel.is_empty() && has_children(&self.root.join(rel)),
            _ => false,
        }
    }

    /// A full walk, then a fresh watcher because symlinked directories may have come or gone.
    /// [`Event::Reconciled`] is emitted once both are done, so receiving it means the vault is
    /// indexed *and* watched.
    fn reconcile(&mut self) {
        let events = self.events.clone();
        let stats = self.index.reconcile(&self.root, |p| {
            let _ = events.send(Event::Progress(p));
        });
        self.rebuild_watcher();
        match stats {
            Ok(stats) => {
                self.emit(Event::Reconciled(stats));
                self.emit_conflicts();
            }
            Err(e) => self.fail("reconciling the vault", e),
        }
    }

    /// Conflicts the walk found. Syncthing usually leaves them while accent is closed, so no
    /// watcher event will ever announce them and the resolve dialog would never be offered.
    fn emit_conflicts(&mut self) {
        match conflict_pairs(&self.index) {
            Ok(pairs) => {
                for (original, conflict) in pairs {
                    if self.seen_conflicts.insert(conflict.clone()) {
                        self.emit(Event::Conflict { original, conflict });
                    }
                }
            }
            Err(e) => self.fail("listing the conflicts in the vault", e),
        }
    }

    fn rebuild_watcher(&mut self) {
        let mut symlinks = self.index.symlink_dirs(&self.root).unwrap_or_else(|e| {
            tracing::warn!("listing symlinked directories: {e:#}");
            Vec::new()
        });
        // Longest target first, so a link inside a linked tree maps through the deeper one.
        symlinks.sort_by_key(|(target, _)| std::cmp::Reverse(target.as_os_str().len()));
        // The watch set is what the walk kept, one watch per directory: a `.venv` the walk refused
        // must not come back in through a recursive watch on the root.
        let dirs = self.index.dirs(&self.root).unwrap_or_else(|e| {
            tracing::warn!("listing the directories to watch: {e:#}");
            Vec::new()
        });

        let tx = self.tx.clone();
        // Drop the old watch set first: two registrations on one tree would double every event.
        self.watcher = None;
        match Watcher::new(&self.root, &dirs, move |e| {
            let _ = tx.send(Msg::Fs(e));
        }) {
            Ok(w) => {
                self.watcher = Some(w);
                self.symlinks = symlinks;
            }
            Err(e) => self.fail("watching the vault", e),
        }
    }

    fn apply(&mut self, ev: VaultEvent, b: &mut Batch) {
        match ev {
            // Handled in `needs_rescan` before the batch is walked.
            VaultEvent::Rescan => {}
            VaultEvent::Changed(p) => {
                if let Some(rel) = self.rel(&p) {
                    self.update(&rel, false, b);
                }
            }
            VaultEvent::Removed(p) => {
                if let Some(rel) = self.rel(&p) {
                    // A temp file renamed over a note is reported as Remove + Create once the
                    // debouncer has that inode cached, which is what an external `sed -i` or a
                    // second accent looks like. Believe a removal only when the path is really
                    // gone, or the tab the user is typing in closes under them.
                    if matches!(walk::stat_one(&self.root, &rel), Ok(Some(_))) {
                        self.update(&rel, false, b);
                    } else {
                        self.remove(&rel, b);
                    }
                }
            }
            VaultEvent::Renamed { from, to } => {
                if let (Some(from), Some(to)) = (self.rel(&from), self.rel(&to)) {
                    if let Err(e) = self.index.remove_file_batched(&from) {
                        self.fail(&format!("removing {from}"), e);
                    }
                    b.resolve = true;
                    b.removed.insert(from.clone());
                    self.update(&to, false, b);
                    b.dirs.insert(parent_dir(&from).to_string());
                    b.dirs.insert(parent_dir(&to).to_string());
                    self.emit(Event::FileRenamed { from, to });
                }
            }
            VaultEvent::ConflictAppeared(p) => {
                if let Some(conflict) = self.rel(&p) {
                    self.update(&conflict, false, b);
                    if let Some(original) = conflict_original_rel(&conflict)
                        && self.seen_conflicts.insert(conflict.clone())
                    {
                        self.emit(Event::Conflict { original, conflict });
                    }
                }
            }
        }
    }

    /// Take a path, and everything under it, out of the index.
    fn remove(&mut self, rel: &str, b: &mut Batch) {
        if let Err(e) = self.index.remove_file_batched(rel) {
            self.fail(&format!("removing {rel}"), e);
        }
        b.resolve = true;
        b.removed.insert(rel.to_string());
        b.dirs.insert(parent_dir(rel).to_string());
        self.seen_conflicts.remove(rel);
        self.emit(Event::FileRemoved(rel.to_string()));
    }

    /// Bring one path in line with the disk. `own` marks a change we made ourselves, which must
    /// never come back to the UI as "someone else edited this note".
    fn update(&mut self, rel: &str, own: bool, b: &mut Batch) {
        if rel.is_empty() {
            return;
        }
        match self.index.update_file_batched(&self.root, rel) {
            Ok(Change::Added(kind)) => {
                b.dirs.insert(parent_dir(rel).to_string());
                b.resolve = true;
                b.rewatch |= kind == FileKind::Dir;
                // A path this batch removed and is seeing again was rewritten, not created:
                // whoever has it open has to reload it.
                if kind != FileKind::Dir && !own && b.removed.remove(rel) {
                    self.emit(Event::FileChanged(rel.to_string()));
                }
            }
            Ok(Change::Removed) => {
                b.dirs.insert(parent_dir(rel).to_string());
                b.removed.insert(rel.to_string());
                b.resolve = true;
            }
            Ok(Change::Updated(kind)) => {
                b.resolve = true;
                // Every kind but a directory reports: a PDF rebuilt by a tool or a source file
                // edited in another editor has to refresh in the UI just like a note does.
                if kind != FileKind::Dir && !own {
                    self.emit(Event::FileChanged(rel.to_string()));
                }
            }
            // `Unchanged` is the watcher echoing our own save back at us.
            Ok(Change::Unchanged | Change::Ignored) => {}
            Err(e) => self.fail(&format!("indexing {rel}"), e),
        }
    }

    /// Watcher paths are absolute. Map one back into the vault, through a directory symlink when
    /// the change happened in an external tree linked in.
    fn rel(&self, abs: &Path) -> Option<String> {
        let mapped = abs
            .strip_prefix(&self.root)
            .ok()
            .map(Path::to_path_buf)
            .or_else(|| {
                self.symlinks.iter().find_map(|(target, link)| {
                    abs.strip_prefix(target)
                        .ok()
                        .map(|rest| Path::new(link).join(rest))
                })
            })?;
        match mapped.to_str() {
            Some(rel) => Some(rel.to_string()),
            None => {
                tracing::warn!(path = %abs.display(), "dropping a non-UTF-8 path");
                None
            }
        }
    }

    /// Never kill the thread over one bad file: log it, tell the UI, carry on.
    fn fail(&self, what: &str, e: impl std::fmt::Display) {
        tracing::warn!("{what}: {e:#}");
        self.emit(Event::Error(format!("{what}: {e:#}")));
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

// -------------------------------------------------------------------- paths

/// Directory part of a vault-relative path; "" is the vault root.
fn parent_dir(rel: &str) -> &str {
    split_parent(rel).0
}

fn split_parent(rel: &str) -> (&str, &str) {
    match rel.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", rel),
    }
}

fn basename(rel: &str) -> &str {
    split_parent(rel).1
}

/// The note title a template sees: the file name without its extension.
fn stem(rel: &str) -> String {
    markdown::strip_ext(basename(rel))
}

/// What a wikilink resolves a path by, so "did the name change" is asked the way links are.
fn stem_key(rel: &str) -> String {
    markdown::link_key(&stem(rel))
}

/// Notes are markdown, and the UI lets the user leave the extension off.
fn with_md(rel: &str) -> String {
    match rel.rsplit_once('.') {
        Some((_, ext))
            if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") =>
        {
            rel.to_string()
        }
        _ => format!("{rel}.md"),
    }
}

/// `(original, conflict copy)` for every `*.sync-conflict-*` file whose original still exists.
/// A copy of a note that has since been deleted is nothing the resolve UI can act on.
fn conflict_pairs(index: &Index) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for copy in index.conflicts()? {
        let Some(original) = conflict_original_rel(&copy) else {
            continue;
        };
        if index.get_file(&original)?.is_some() {
            out.push((original, copy));
        }
    }
    Ok(out)
}

/// `Dir/Note.sync-conflict-….md` -> `Dir/Note.md`, or `None` when `copy` is not one.
pub fn conflict_original_rel(copy: &str) -> Option<String> {
    let (dir, name) = split_parent(copy);
    let original = fs::conflict_original(name)?;
    Some(if dir.is_empty() {
        original
    } else {
        format!("{dir}/{original}")
    })
}

/// What [`Vault::adopt_conflict`] calls the version it replaces: Syncthing's own naming, with
/// `accent` where the device id would be, so the vault treats it as the conflict copy it is.
fn accent_conflict_name(name: &str, now: chrono::NaiveDateTime) -> String {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    format!(
        "{stem}.sync-conflict-{}-accent{ext}",
        now.format("%Y%m%d-%H%M%S")
    )
}

fn outside(rel: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{rel} is outside the vault"),
    )
}

/// A directory with something in it, which is what a moved-in tree looks like.
fn has_children(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Long enough for a 300 ms watcher debounce plus a reconcile of a handful of files.
    const BUDGET: Duration = Duration::from_secs(10);

    /// A vault in a tempdir with its index in a second one, so no test can reach the user's
    /// real cache. `vault` is declared first: dropping it stops the worker before the
    /// directories it walks disappear.
    struct Fixture {
        vault: Vault,
        events: Receiver<Event>,
        root: TempDir,
        _cache: TempDir,
    }

    impl Fixture {
        fn open(cfg: VaultConfig) -> Fixture {
            Fixture::open_dir(tempfile::tempdir().unwrap(), cfg)
        }

        /// Opens `root` and waits for the first reconcile, so the vault is indexed and watched.
        /// The directory may already hold files: that is the state Syncthing leaves behind while
        /// accent is closed.
        fn open_dir(root: TempDir, cfg: VaultConfig) -> Fixture {
            let cache = tempfile::tempdir().unwrap();
            let (vault, events) =
                Vault::open_at(root.path(), &cache.path().join("index.db"), cfg).unwrap();
            let f = Fixture {
                vault,
                events,
                root,
                _cache: cache,
            };
            assert!(
                f.wait(|e| matches!(e, Event::Reconciled(_))).is_some(),
                "the initial reconcile never finished"
            );
            f
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.vault.root().join(rel);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).unwrap();
            }
            std::fs::write(path, text).unwrap();
        }

        fn read(&self, rel: &str) -> String {
            std::fs::read_to_string(self.vault.root().join(rel)).unwrap()
        }

        fn wait(&self, pred: impl Fn(&Event) -> bool) -> Option<Event> {
            wait_for(&self.events, pred, BUDGET)
        }
    }

    /// Drain events until one matches, or the budget runs out.
    fn wait_for(
        rx: &Receiver<Event>,
        pred: impl Fn(&Event) -> bool,
        budget: Duration,
    ) -> Option<Event> {
        let deadline = Instant::now() + budget;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(e) if pred(&e) => return Some(e),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    }

    /// Poll index state, which changes without an event of its own.
    fn poll_until(mut f: impl FnMut() -> bool, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        f()
    }

    fn names(rows: &[FileRow]) -> Vec<String> {
        rows.iter().map(|r| r.rel_path.clone()).collect()
    }

    const CONFLICT: &str = "Note.sync-conflict-20260903-101500-ABCDEFG.md";

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

    /// Depends on real inotify events.
    #[test]
    fn external_write_emits_file_changed_and_becomes_searchable() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "hello");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        f.write("Note.md", "kumquat harvest");

        assert!(
            f.wait(|e| matches!(e, Event::FileChanged(p) if p == "Note.md"))
                .is_some(),
            "an external edit must reach the UI"
        );
        assert!(poll_until(
            || !f.vault.search("kumquat", 10, false).unwrap().is_empty(),
            BUDGET
        ));
    }

    /// Non-markdown files are stat-only rows in the index, but an external edit still has to
    /// reach whoever has the file open. Depends on real inotify events.
    #[test]
    fn external_write_to_a_code_file_emits_file_changed() {
        let f = Fixture::open(VaultConfig::default());
        f.write("tool.py", "print(1)\n");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        f.write("tool.py", "print(2)\n");

        assert!(
            f.wait(|e| matches!(e, Event::FileChanged(p) if p == "tool.py"))
                .is_some(),
            "an external edit to a source file must reach the UI"
        );
    }

    /// The watch set is one watch per directory, built from the index, so a subdirectory that was
    /// already there when the vault opened has to be in it — that is the everyday case of editing
    /// a note in another editor.
    #[test]
    fn an_edit_in_a_pre_existing_subdirectory_reaches_the_ui() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Projects")).unwrap();
        std::fs::write(root.path().join("Projects/Plan.md"), "one").unwrap();
        let f = Fixture::open_dir(root, VaultConfig::default());

        f.write("Projects/Plan.md", "two");

        assert!(
            f.wait(|e| matches!(e, Event::FileChanged(p) if p == "Projects/Plan.md"))
                .is_some(),
            "an edit inside an indexed subdirectory must reach the UI"
        );
    }

    /// A linked-in folder is watched through its vault path — `inotify_add_watch` resolves the
    /// link — which is what keeps "symlinked folders handled" true now that the watch set is a
    /// list of directories rather than a recursive watch plus the resolved targets.
    #[test]
    fn an_edit_inside_a_symlinked_directory_reaches_the_ui() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("Ext.md"), "one").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        let f = Fixture::open_dir(root, VaultConfig::default());

        std::fs::write(outside.path().join("Ext.md"), "two").unwrap();

        assert!(
            f.wait(|e| matches!(e, Event::FileChanged(p) if p == "linked/Ext.md"))
                .is_some(),
            "an edit in a linked-in folder must reach the UI"
        );
    }

    #[test]
    fn own_save_updates_the_index_without_a_file_changed_event() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "hello");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        let (_, etag) = f.vault.read("Note.md").unwrap();
        let saved = f
            .vault
            .save("Note.md", "quokka census", Some(etag))
            .unwrap();
        assert_eq!(saved, Etag::of(&f.vault.root().join("Note.md")).unwrap());

        assert!(poll_until(
            || !f.vault.search("quokka", 10, false).unwrap().is_empty(),
            BUDGET
        ));
        // Well past the watcher's 300 ms debounce, so the echo of our own save has been and gone.
        assert!(
            wait_for(
                &f.events,
                |e| matches!(e, Event::FileChanged(p) if p == "Note.md"),
                Duration::from_secs(2),
            )
            .is_none(),
            "our own save came back as someone else's edit"
        );
    }

    /// Depends on real inotify events.
    #[test]
    fn a_new_file_in_a_subdirectory_reports_its_parent_dir() {
        let f = Fixture::open(VaultConfig::default());
        std::fs::create_dir(f.vault.root().join("sub")).unwrap();
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        f.write("sub/New.md", "fresh");

        match f
            .wait(|e| matches!(e, Event::DirsChanged(d) if d.contains(&"sub".to_string())))
            .expect("no DirsChanged for the parent directory")
        {
            Event::DirsChanged(dirs) => assert_eq!(dirs, ["sub"]),
            other => panic!("expected DirsChanged, got {other:?}"),
        }
    }

    /// Depends on real inotify events.
    #[test]
    fn a_conflict_copy_emits_conflict_and_stays_out_of_search() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "mine");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        f.write(CONFLICT, "wombat census");

        match f
            .wait(|e| matches!(e, Event::Conflict { .. }))
            .expect("no Conflict event")
        {
            Event::Conflict { original, conflict } => {
                assert_eq!(original, "Note.md");
                assert_eq!(conflict, CONFLICT);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        assert!(poll_until(
            || f.vault.conflicts().unwrap() == [("Note.md".to_string(), CONFLICT.to_string())],
            BUDGET
        ));
        assert_eq!(f.vault.conflicts_of("Note.md").unwrap(), [CONFLICT]);
        assert!(f.vault.conflicts_of("Other.md").unwrap().is_empty());
        assert!(
            f.vault.search("wombat", 10, false).unwrap().is_empty(),
            "a conflict copy is never a note"
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

    #[test]
    fn replace_all_rewrites_every_match_and_reindexes() {
        let f = Fixture::open(VaultConfig::default());
        f.write("a.md", "colour and colour\n");
        f.write("sub/b.md", "Colour\n");
        f.write("c.md", "nothing here\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        let re = search::pattern("colour", Options::default()).unwrap();
        assert_eq!(f.vault.grep(&re, 10, false).unwrap().1, 3);

        let report = f.vault.replace_all(&re, "color", true).unwrap();
        assert_eq!(report.rewritten, ["a.md", "sub/b.md"]);
        assert_eq!(report.matches, 3);
        assert!(report.failed.is_empty());
        assert_eq!(f.read("a.md"), "color and color\n");
        assert_eq!(f.read("sub/b.md"), "color\n", "case-insensitive by default");
        assert_eq!(f.read("c.md"), "nothing here\n");

        assert!(
            poll_until(|| f.vault.grep(&re, 10, false).unwrap().1 == 0, BUDGET),
            "the rewrites must reach the index without a rescan"
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

        let re = search::pattern("zorblat", Options::default()).unwrap();
        let (hits, total) = f.vault.grep(&re, 10, false).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rel_path, "tool.py");
        assert_eq!(hits[0].line, 2);
        assert_eq!(total, 1, "the NUL byte keeps bin.dat out");
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
        let re = search::pattern(r"hello (\w+)", opts).unwrap();
        f.vault.replace_all(&re, "bye $1", false).unwrap();
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
            .create_note("Inbox/Weekly sync", Some("Templates/Note.md"))
            .unwrap();

        assert_eq!(rel, "Inbox/Weekly sync.md");
        let text = f.read(&rel);
        assert_eq!(text, "# Weekly sync\n\nbody\n");
        assert_eq!(&text[cursor.unwrap()..], "body\n");

        // Once indexed, the same file is what the "new note from template" dialog offers.
        assert!(poll_until(
            || f.vault.templates().unwrap() == ["Templates/Note.md"],
            BUDGET
        ));
    }

    #[test]
    fn daily_note_is_created_once_and_reused() {
        let f = Fixture::open(VaultConfig {
            daily_dir: "Daily".to_string(),
            daily_template: Some("Templates/Daily.md".to_string()),
            ..VaultConfig::default()
        });
        f.write("Templates/Daily.md", "# {{date}}\n\n{{cursor}}");

        let (rel, cursor) = f.vault.daily_note().unwrap();
        assert!(rel.starts_with("Daily/") && rel.ends_with(".md"), "{rel}");
        assert!(cursor.is_some(), "a fresh daily note places the caret");
        let text = f.read(&rel);

        let (again, cursor) = f.vault.daily_note().unwrap();
        assert_eq!(again, rel);
        assert_eq!(cursor, None, "an existing note is opened, not rewritten");
        assert_eq!(f.read(&rel), text);
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

    #[test]
    fn complete_notes_and_tags_are_prefix_filtered_and_capped() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Alpha.md", "on #alpha and #alphabet, more #alpha\n");
        f.write("Alphabet.md", "letters\n");
        f.write("Beta.md", "unrelated #beta\n");
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

        assert_eq!(
            f.vault.complete_notes("alp", 10).unwrap(),
            ["Alpha.md", "Alphabet.md"]
        );
        assert_eq!(f.vault.complete_notes("alp", 1).unwrap(), ["Alpha.md"]);
        assert!(f.vault.complete_notes("zzz", 10).unwrap().is_empty());

        // Count ordering: `#alpha` twice beats `#alphabet` once.
        assert_eq!(
            f.vault.complete_tags("alp", 10).unwrap(),
            ["alpha", "alphabet"]
        );
        assert_eq!(f.vault.complete_tags("alp", 1).unwrap(), ["alpha"]);
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

    /// notify reports a temp-plus-rename over a note it has the inode of as `Remove` + `Create`,
    /// which is what an external `sed -i` (or another accent) looks like. A note that is still
    /// on disk must never reach the UI as a removal, or the app closes the tab it is open in.
    #[test]
    fn an_external_atomic_rewrite_is_a_change_not_a_removal() {
        let f = Fixture::open(VaultConfig::default());
        f.write("Note.md", "one\n");
        assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());

        let path = f.vault.root().join("Note.md");
        let _ = std::fs::read_to_string(&path).unwrap();
        fs::write_note(&path, "someone else wrote this\n", None).unwrap();

        let (mut removed, mut changed) = (false, false);
        let deadline = Instant::now() + BUDGET;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match f.events.recv_timeout(left) {
                Ok(Event::FileRemoved(p)) if p == "Note.md" => removed = true,
                Ok(Event::FileChanged(p)) if p == "Note.md" => {
                    changed = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(
            !removed,
            "a note that is still on disk was reported removed"
        );
        assert!(changed, "the external rewrite never reached the UI");
    }

    /// The façade is the boundary MCP and Android call directly, so it decides what is inside
    /// the vault; `daily_dir: "../Outside"` reaches it too.
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
        vault.create_note("sub/Nested", None).unwrap();
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

    /// A Syncthing pull is one batch of files; the links in all of them still have to resolve.
    /// That the batch resolves once rather than once per file is pinned in `index`.
    #[test]
    fn a_burst_of_writes_resolves_the_links_in_all_of_them() {
        let f = Fixture::open(VaultConfig::default());
        for i in 0..10 {
            f.write(&format!("n{i}.md"), "see [[Target]]\n");
        }
        f.write("Target.md", "here\n");

        assert!(poll_until(
            || f.vault.backlinks("Target.md").unwrap().len() == 10,
            BUDGET
        ));
    }

    /// The usual case: Syncthing left the conflict while accent was closed, so no watcher event
    /// will ever announce it.
    #[test]
    fn conflicts_already_in_the_vault_reach_the_ui_once() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Note.md"), "mine\n").unwrap();
        std::fs::write(root.path().join(CONFLICT), "theirs\n").unwrap();

        let f = Fixture::open_dir(root, VaultConfig::default());

        match f
            .wait(|e| matches!(e, Event::Conflict { .. }))
            .expect("no Conflict event for a conflict that was already there")
        {
            Event::Conflict { original, conflict } => {
                assert_eq!(
                    (original.as_str(), conflict.as_str()),
                    ("Note.md", CONFLICT)
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // A rescan finds the same pair; the UI must not be offered it twice.
        f.vault.rescan();
        assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());
        assert!(
            wait_for(
                &f.events,
                |e| matches!(e, Event::Conflict { .. }),
                Duration::from_secs(1)
            )
            .is_none(),
            "the same conflict was offered twice"
        );
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
    fn dropping_the_vault_stops_the_worker() {
        let f = Fixture::open(VaultConfig::default());
        let Fixture {
            vault,
            events,
            root: _root,
            _cache,
        } = f;
        drop(vault);

        // The worker holds the only sender: a disconnect proves the thread is gone.
        let deadline = Instant::now() + BUDGET;
        loop {
            match events.recv_timeout(Duration::from_millis(200)) {
                Ok(_) => {}
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    assert!(Instant::now() < deadline, "the worker outlived the vault")
                }
            }
        }
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
