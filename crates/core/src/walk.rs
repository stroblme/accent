//! Vault walk: symlink rules, (dev, ino) dedup, Syncthing temp/conflict filtering.
//!
//! Obsidian's *rules* (not its behaviour): a directory symlink is followed once, loops are
//! skipped, a symlink whose target lands inside the vault root (or inside a symlink target we
//! already accepted) is skipped and reported, file symlinks are followed and de-duplicated by
//! `(dev, ino)` so the same inode reached by two paths yields one [`FileMeta`] plus an alias.
//!
//! Ignore files differ inside and outside the vault, so the walk runs in two kinds of pass.
//! The **vault tree** honours no ignore file *for files*: a notes vault routinely gitignores
//! `*.md` on purpose, and that must not hide the user's notes. What git ignores file by file is
//! left out of *queries* instead ([`crate::index::Index::set_excluded`]), which is the only place
//! it can be done without the walk having to guess. A gitignored **directory** is a different
//! animal — an `mlruns/`, a `node_modules/`, a `.venv/`, somebody's build output — and the walk
//! does not enter one ([`dir_ignores`]); its contents reach the file tree a level at a time when
//! the reader opens the row ([`unindexed_children`]). A **symlink target** is somebody else's
//! tree — usually a code repo — so it honours its own `.gitignore`/`.ignore` whole, files
//! included, which is what keeps `.venv`, `target` and friends out.
//! Each pass therefore walks with `follow_links(false)` and hands accepted directory symlinks
//! back as new passes; nested symlinks under a target obey exactly the same rules.
//!
//! Inside the vault that leaves dependency trees nobody gitignored, because the user never wrote
//! an ignore file at all: a `.venv` or a `target/` dropped next to the notes. Those are skipped by
//! their own **marker file** ([`DEPENDENCY_MARKERS`]) rather than by name, so one rule covers
//! cargo, `python -m venv`, and every other tool that follows the `CACHEDIR.TAG` convention.
//!
//! ponytail: unix-only (`MetadataExt` for dev/ino/mtime_nsec). Targets are Linux + Android;
//! a Windows port would need a `cfg` branch using `FileIndex`/`VolumeSerialNumber`.

use ignore::{IncrementalIgnore, WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

/// Directory names the walk refuses whatever the options say.
///
/// `.git` is the repository's own storage, which the app reads through git rather than as files,
/// and `.trash` is what the user already threw away. Neither is reachable even with
/// [`ScanOptions::include_skipped`]: a search widened to "everything" still means everything in
/// the vault, not its plumbing.
const ALWAYS_SKIP_DIRS: &[&str] = &[".git", ".trash"];

/// Dependency trees that plant no marker file, skipped by name unless the caller asks for them.
///
/// Deliberately short: matching dependency trees by name is whack-a-mole, so anything that
/// plants a marker file is caught by [`is_dependency_tree`] instead. These are the trees that
/// plant none.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
];

/// Files that mark their directory as somebody's dependency or build tree.
///
/// `CACHEDIR.TAG` is the cross-tool cache marker (`tar --exclude-caches`, borg, restic); cargo
/// writes one into every `target/`. `pyvenv.cfg` is the PEP 405 virtualenv root marker, written
/// by `python -m venv` and by virtualenv ≥ 20. Between them they cover the two trees that
/// actually blow up a vault, with no per-ecosystem name list to maintain.
const DEPENDENCY_MARKERS: &[&str] = &["CACHEDIR.TAG", "pyvenv.cfg"];

/// Does this directory carry a marker saying nobody's notes live in it?
///
/// ponytail: presence is enough. `CACHEDIR.TAG` also fixes its first line to a signature, but
/// checking it would cost an open per directory instead of a stat, and a file of that exact name
/// that is not a cache tag has never been seen in the wild.
fn is_dependency_tree(dir: &Path) -> bool {
    DEPENDENCY_MARKERS.iter().any(|m| dir.join(m).exists())
}

/// A matcher for the vault's own ignore files, asked about **directories only**.
///
/// `ignore`'s own walker cannot do this: turning its gitignore on hides ignored files too, and a
/// vault that gitignores `*.md` would lose its notes. [`IncrementalIgnore`] is the same matcher
/// stack asked one path at a time, so the rule can be applied to directories and to nothing else.
///
/// `.gitignore` and `.git/info/exclude` only. Not the machine's global excludes file — that is a
/// preference of the person, not a fact about the project — and not `.ignore`, which is
/// ripgrep's file and not git's.
///
/// ponytail: a fresh matcher per call, and callers that ask about one path ([`stat_one`]) build
/// one per path. It reads an ignore file per directory on the way down and caches nothing across
/// calls, which is a handful of failed `open`s for a watcher event. Holding one per vault would
/// have to be invalidated whenever a `.gitignore` is edited, which is the harder half.
fn dir_ignores(root: &Path) -> IncrementalIgnore {
    let mut b = WalkBuilder::new(root);
    b.hidden(false)
        .parents(false)
        .git_global(false)
        .git_ignore(true)
        .git_exclude(true)
        .ignore(false)
        .require_git(false);
    b.build_matchers()
        .pop()
        .expect("one walk root yields one matcher")
}

/// Does a directory on the way to `rel` — or `rel` itself, when it is one — get ignored by git?
///
/// `is_dir` decides which: a file is asked about its parent, never about its own name, because a
/// gitignored file is indexed like any other and left out of queries instead.
fn in_ignored_dir(ignores: &mut IncrementalIgnore, rel: &str, is_dir: bool) -> bool {
    let dir = match is_dir {
        true => rel,
        false => rel.rsplit_once('/').map(|(parent, _)| parent).unwrap_or(""),
    };
    !dir.is_empty() && ignores.matched(dir, true).is_ignore()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileKind {
    Dir,
    Markdown,
    Pdf,
    Other,
    /// `*.sync-conflict-*`: stored in the index so the UI can offer a merge, never searched.
    Conflict,
}

impl FileKind {
    pub fn as_i64(self) -> i64 {
        match self {
            FileKind::Dir => 0,
            FileKind::Markdown => 1,
            FileKind::Pdf => 2,
            FileKind::Other => 3,
            FileKind::Conflict => 4,
        }
    }
    pub fn from_i64(v: i64) -> Self {
        match v {
            0 => FileKind::Dir,
            1 => FileKind::Markdown,
            2 => FileKind::Pdf,
            4 => FileKind::Conflict,
            _ => FileKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    /// Vault-relative, `/`-separated — the path the user sees.
    pub rel_path: String,
    pub canonical: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub mtime_ns: i64,
    pub size: u64,
    pub kind: FileKind,
}

/// A second path reaching an inode already listed in [`ScanResult::files`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    /// The duplicate vault-relative path.
    pub rel_path: String,
    /// The `rel_path` of the [`FileMeta`] that won this inode.
    pub target_rel_path: String,
    pub canonical: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipReason {
    /// Symlink target is inside the vault root — the files are already reachable directly.
    TargetInsideVault,
    /// Symlink target is inside (or equal to) a symlink target we already followed.
    TargetOverlapsSymlink,
    /// `ignore` detected the target is an ancestor of the link: following it would loop.
    SymlinkLoop,
    /// A dependency or build tree: the directory carries one of [`DEPENDENCY_MARKERS`].
    DependencyTree,
    /// A directory git ignores. Not walked, and opened by hand in the file tree instead.
    GitIgnored,
    /// Broken symlink, permission denied, vanished mid-walk.
    Io,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SkipReason::TargetInsideVault => "symlink target is inside the vault",
            SkipReason::TargetOverlapsSymlink => "symlink target overlaps another symlink",
            SkipReason::SymlinkLoop => "symlink loop",
            SkipReason::DependencyTree => "dependency or build tree",
            SkipReason::GitIgnored => "git ignores this directory",
            SkipReason::Io => "io error",
        })
    }
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Follow directory symlinks (subject to the rules above).
    pub follow_links: bool,
    /// Honour `.gitignore`/`.ignore` *inside the vault tree*. Off by default: vaults are
    /// commonly git repos that ignore `*.md`, and hiding the user's notes is never right.
    pub vault_gitignore: bool,
    /// Honour `.gitignore`/`.ignore` inside directory-symlink targets. On by default: those
    /// are external trees (code repos) whose build output nobody wants in a note index.
    pub target_gitignore: bool,
    /// Skip directories carrying a [`DEPENDENCY_MARKERS`] file. On by default: one virtualenv
    /// measured on the author's machine is 105 456 files in 12 390 directories, which is both
    /// an index nobody wants and 9 % of the kernel's inotify watch budget.
    ///
    /// ponytail: if someone really keeps notes under a `CACHEDIR.TAG`, this becomes a per-vault
    /// preference; until then the file tree still lists such a tree and Search's All reaches it.
    pub skip_dependency_trees: bool,
    /// Walk [`SKIP_DIRS`] and marked dependency trees anyway. Off by default, and never turned on
    /// for the index: this is the Search pane's All toggle reaching, for one query, the trees the
    /// index deliberately does not hold. [`ALWAYS_SKIP_DIRS`] is not opened by it.
    pub include_skipped: bool,
    /// Walk into the directories git ignores in the vault tree rather than skipping them, each
    /// still reported in [`ScanResult::skipped`] as [`SkipReason::GitIgnored`] so the caller can
    /// tell their files from the rest. Every other skip holds, dependency trees included. Off by
    /// default and never on for the index: [`ignored_files`] is what it is for.
    pub enter_ignored_dirs: bool,
    /// 0 = one thread per core.
    pub threads: usize,
    pub max_depth: Option<usize>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            follow_links: true,
            vault_gitignore: false,
            target_gitignore: true,
            skip_dependency_trees: true,
            include_skipped: false,
            enter_ignored_dirs: false,
            threads: 0,
            max_depth: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanResult {
    /// One entry per distinct `(dev, ino)`, keyed by the shallowest/lexicographically-first path.
    pub files: Vec<FileMeta>,
    /// Extra paths reaching an inode already listed in `files`.
    pub aliases: Vec<Alias>,
    pub skipped: Vec<Skipped>,
}

/// Names the walk refuses at any depth, whatever the ignore files say. `include_skipped` opens
/// [`SKIP_DIRS`] but never [`ALWAYS_SKIP_DIRS`].
fn never_walked(name: &str, include_skipped: bool) -> bool {
    ALWAYS_SKIP_DIRS.contains(&name)
        || crate::fs::is_syncthing_temp(name)
        || (!include_skipped && SKIP_DIRS.contains(&name))
}

/// Names the vault never lists at any depth, not even in the trees the walk skips:
/// [`ALWAYS_SKIP_DIRS`], Syncthing's temporaries and our own `.accent-` save temporaries. Nothing
/// may be created or renamed under one either, since it would never be seen again.
pub fn out_of_reach(name: &str) -> bool {
    never_walked(name, true) || name.starts_with(".accent-")
}

/// Classify by file name. Conflict wins over extension: `Note.sync-conflict-….md` is not a note.
fn classify(name: &str) -> FileKind {
    if crate::fs::is_sync_conflict(name) {
        return FileKind::Conflict;
    }
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) {
        Some(e) if e == "md" || e == "markdown" => FileKind::Markdown,
        Some(e) if e == "pdf" => FileKind::Pdf,
        _ => FileKind::Other,
    }
}

/// The row the index stores for one already-stat'ed entry. `meta` must come from a stat that
/// followed symlinks, so a linked-in note dedups against the inode it really points at.
fn file_meta(rel_path: String, path: &Path, meta: &std::fs::Metadata) -> FileMeta {
    let kind = if meta.is_dir() {
        FileKind::Dir
    } else {
        classify(rel_path.rsplit('/').next().unwrap_or(&rel_path))
    };
    FileMeta {
        rel_path,
        canonical: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        dev: meta.dev(),
        ino: meta.ino(),
        mtime_ns: meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
        size: meta.len(),
        kind,
    }
}

/// Stat a single vault path the way [`scan`] would (follows symlinks). `Ok(None)` means the name is
/// one `scan` never yields: a hard-skipped directory component, a path inside a dependency tree,
/// a Syncthing temp file, or one of our own `.accent-` save temporaries.
///
/// This is the watcher's counterpart to `scan`: one changed path costs one stat instead of a walk.
/// The two must agree, or a `pip install` in a skipped tree would put back, one event at a time,
/// exactly what the walk refused.
pub fn stat_one(root: &Path, rel: &str) -> crate::Result<Option<FileMeta>> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    // Always the strict rule: the watcher must agree with the index's own walk, and the All
    // toggle never feeds the watcher, so the two cannot drift apart.
    if rel.split('/').any(|part| never_walked(part, false)) || name.starts_with(".accent-") {
        return Ok(None);
    }
    // Every directory from the root down, the path itself included: a marker anywhere on the way
    // means the walk never descended here. The root is the user's choice and is never tested.
    let mut dir = root.to_path_buf();
    for part in rel.split('/') {
        dir.push(part);
        if is_dependency_tree(&dir) {
            return Ok(None);
        }
    }
    let path = root.join(rel);
    let meta = std::fs::metadata(&path).map_err(|e| crate::Error::io(rel, e))?;
    // Last, because it is the only test that needs to know whether the path is a directory: a
    // gitignored *file* is indexed, a gitignored *directory* is not entered.
    if in_ignored_dir(&mut dir_ignores(root), rel, meta.is_dir()) {
        return Ok(None);
    }
    Ok(Some(file_meta(rel.to_string(), &path, &meta)))
}

/// What the walk makes of a directory, and the one thing the file tree needs beyond "the index
/// does not hold it": *why*.
///
/// [`Unindexed::Dependency`] is somebody else's tree — a `node_modules`, a `.venv`, a cargo
/// `target/` — opened to look at and never to edit; [`Unindexed::Ignored`] is a folder the reader
/// gitignored on purpose, an `mlruns/` or a build output, which is theirs to change like any other.
/// The two are kept apart here so the tree does not have to guess at a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unindexed {
    Dependency,
    Ignored,
}

/// Does the walk refuse to look inside `rel`, and why? `Some` for the trees [`scan`] never enters:
/// [`ALWAYS_SKIP_DIRS`], [`SKIP_DIRS`], anything at or under a [`DEPENDENCY_MARKERS`] file, and
/// anything at or under a directory git ignores.
///
/// This is [`stat_one`]'s rule asked as a question, so the file tree and the watcher cannot
/// disagree about which paths the index holds. The vault root is the user's own choice and is
/// never refused. A dependency tree inside a gitignored folder — the usual `node_modules` under an
/// ignored build directory — answers `Dependency`: the stricter of the two wins.
fn is_unindexed(root: &Path, rel: &str, ignores: &mut IncrementalIgnore) -> Option<Unindexed> {
    if rel.is_empty() {
        return None;
    }
    if rel.split('/').any(|part| never_walked(part, false)) {
        return Some(Unindexed::Dependency);
    }
    let mut dir = root.to_path_buf();
    let marked = rel.split('/').any(|part| {
        dir.push(part);
        is_dependency_tree(&dir)
    });
    if marked {
        return Some(Unindexed::Dependency);
    }
    // Only ever asked about a directory the reader has opened, so the name is a directory's.
    in_ignored_dir(ignores, rel, true).then_some(Unindexed::Ignored)
}

/// The children of the directory `rel` that `held` — the index's own listing of it — does not
/// name, as `(rel_path, kind)`.
///
/// The file tree shows every folder in the vault, the ones the index deliberately does not walk
/// included: a listing that silently leaves `node_modules` out is a listing nobody can trust.
/// Those trees stay out of the index, the watcher and every query all the same — nothing here is
/// stored — so the only place their contents can come from is a `read_dir`, one level at a time,
/// when the reader opens the row.
///
/// Two kinds of row come back: the refused directories themselves — a `node_modules`, a marked
/// dependency tree, a directory git ignores — listed beside their indexed siblings, and, where
/// `rel` is already inside one, everything in it. [`ALWAYS_SKIP_DIRS`],
/// Syncthing's temporaries and our own save temporaries are refused at every depth, exactly as
/// [`scan`] refuses them, so `.git` and `.trash` are out of reach here too.
///
/// `held` is both the thing that keeps a row from being listed twice and what keeps this cheap:
/// deciding whether a directory is a dependency tree costs a stat per [`DEPENDENCY_MARKERS`]
/// entry, and a directory the index already holds cannot be one, so it is never asked. On the
/// test vault's 2 400-directory `Resources/library/storage` that is the difference between 4 800
/// stats per expansion and none.
pub fn unindexed_children(
    root: &Path,
    rel: &str,
    held: &std::collections::HashSet<&str>,
) -> crate::Result<Vec<(String, FileKind, Unindexed)>> {
    // One matcher for the whole listing: it caches the ignore files it reads on the way down, so
    // a directory of a thousand children asks the disk for them once.
    let mut ignores = dir_ignores(root);
    let inside = is_unindexed(root, rel, &mut ignores);
    let mut out = Vec::new();
    let listed = |e| crate::Error::io(rel, e);
    for entry in std::fs::read_dir(root.join(rel)).map_err(listed)? {
        let entry = entry.map_err(listed)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // The skipped trees are exactly what this lists; what stays refused is `ALWAYS_SKIP_DIRS`
        // and the temporaries.
        if out_of_reach(&name) {
            continue;
        }
        let rel_path = match rel.is_empty() {
            true => name.clone(),
            false => format!("{rel}/{name}"),
        };
        if held.contains(rel_path.as_str()) {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type().ok();
        let dir = match kind {
            // The dirent's own `d_type`, so an ordinary file or folder costs no syscall at all.
            Some(t) if !t.is_symlink() => t.is_dir(),
            // A link is followed, so a package linked into `node_modules` — which npm does by the
            // hundred — reads as the directory it points at rather than as an unopenable file. A
            // link that loops costs nothing: this is one level, opened by hand.
            _ => std::fs::metadata(&path).is_ok_and(|m| m.is_dir()),
        };
        // Outside a skipped tree, a child the index does not hold is only listed when it is one
        // of the trees the walk refuses. Anything else missing from the index is missing for a
        // reason of its own — a symlink pointing back into the vault, a file that vanished
        // between the scan and now — and guessing at it here is not this function's business.
        let refused = dir.then(|| {
            if SKIP_DIRS.contains(&name.as_str()) || is_dependency_tree(&path) {
                return Some(Unindexed::Dependency);
            }
            in_ignored_dir(&mut ignores, &rel_path, true).then_some(Unindexed::Ignored)
        });
        // A child of an unindexed tree is of its parent's kind, unless it is a stricter tree of
        // its own: a `node_modules` under a gitignored build folder is still nobody's to edit.
        let why = match (inside, refused.flatten()) {
            (_, Some(Unindexed::Dependency)) | (Some(Unindexed::Dependency), _) => {
                Unindexed::Dependency
            }
            (Some(Unindexed::Ignored), _) | (_, Some(Unindexed::Ignored)) => Unindexed::Ignored,
            (None, None) => continue,
        };
        out.push((
            rel_path,
            if dir { FileKind::Dir } else { classify(&name) },
            why,
        ));
    }
    Ok(out)
}

/// Every file inside a directory git ignores, by vault-relative path, in path order: the folders
/// the index never walks, listed whole for Go to File, `[[` completion and a link naming a file in
/// one. A dependency tree stays out, inside an ignored folder too: it is somebody else's, as the
/// file tree says ([`Unindexed::Dependency`]), and a `node_modules` is the 40 000 files a listing
/// must not hold. So does a conflict copy, which is never opened as a file.
///
/// One walk of the whole vault, since nothing short of one says where the ignored folders are.
pub fn ignored_files(root: &Path) -> Vec<String> {
    let opts = ScanOptions {
        enter_ignored_dirs: true,
        ..ScanOptions::default()
    };
    let r = scan(root, &opts);
    let dirs: HashSet<String> = r
        .skipped
        .iter()
        .filter(|s| s.reason == SkipReason::GitIgnored)
        .filter_map(|s| {
            Some(
                s.path
                    .strip_prefix(root)
                    .ok()?
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect();
    let inside = |rel: &str| {
        rel.match_indices('/')
            .any(|(i, _)| dirs.contains(&rel[..i]))
    };
    let mut out: Vec<String> = r
        .files
        .into_iter()
        .filter(|f| !matches!(f.kind, FileKind::Dir | FileKind::Conflict) && inside(&f.rel_path))
        .map(|f| f.rel_path)
        .collect();
    out.sort_unstable();
    out
}

/// Walk `root`, applying the symlink rules. Returns files, aliases and skip reports.
pub fn scan(root: &Path, opts: &ScanOptions) -> ScanResult {
    scan_until(root, "", &[], opts, &|| false, &AtomicUsize::new(0))
}

/// [`scan`], stoppable, and of the folder `dir` alone (vault-relative, `""` for the whole vault).
/// `stop` is asked once per entry, on the walking threads, so a walk halts within one directory
/// entry rather than at the end of the vault.
///
/// A folder's entries come out as the whole walk lists them: its folders are held to the
/// `.gitignore` of every folder above them as well as their own, and a link inside it to one of
/// `linked` — the canonical folders the vault already reaches through a link outside `dir` — is
/// refused as the second way in that it is. `dir` itself is not listed, and nothing above it is
/// asked whether the walk would enter it: that is the caller's to know.
///
/// `found` counts the entries as they are kept, before the `(dev, ino)` dedup: what another
/// thread can report while the walk is still running, which has no total until it ends.
///
/// What comes back from a stopped walk is **short of files the vault holds**, and it is not
/// marked: a caller that reads it as the whole vault would take everything it never reached for a
/// deletion. The one caller asks `stop` again itself and drops the result whole
/// ([`crate::index::Index::reconcile_with`]).
pub fn scan_until(
    root: &Path,
    dir: &str,
    linked: &[PathBuf],
    opts: &ScanOptions,
    stop: &(dyn Fn() -> bool + Sync),
    found: &AtomicUsize,
) -> ScanResult {
    let collected: Mutex<Vec<FileMeta>> = Mutex::new(Vec::new());
    let mut skipped = walk_passes(root, dir, linked, opts, &|f| {
        if stop() {
            return WalkState::Quit;
        }
        collected.lock().unwrap_or_else(|e| e.into_inner()).push(f);
        found.fetch_add(1, Ordering::Relaxed);
        WalkState::Continue
    });
    let mut files = collected.into_inner().unwrap_or_else(|e| e.into_inner());

    // Deterministic winner: shallowest path, then lexicographic. `build_parallel` yields in an
    // arbitrary order, so "first path wins" only means anything after a sort.
    files.sort_unstable_by(|a, b| {
        let (da, db) = (
            a.rel_path.matches('/').count(),
            b.rel_path.matches('/').count(),
        );
        da.cmp(&db).then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    let mut seen: HashMap<(u64, u64), String> = HashMap::with_capacity(files.len());
    let mut aliases = Vec::new();
    files.retain(|f| match seen.get(&(f.dev, f.ino)) {
        None => {
            seen.insert((f.dev, f.ino), f.rel_path.clone());
            true
        }
        Some(winner) => {
            aliases.push(Alias {
                rel_path: f.rel_path.clone(),
                target_rel_path: winner.clone(),
                canonical: f.canonical.clone(),
            });
            false
        }
    });
    skipped.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    // Debug, not a toast: skipping a `.venv` is the expected outcome, not news the user has to
    // acknowledge on every start. It still has to be findable when a folder is missing from the tree.
    for s in &skipped {
        if matches!(
            s.reason,
            SkipReason::DependencyTree | SkipReason::GitIgnored
        ) {
            tracing::debug!(tree = %s.path.display(), why = %s.reason, "not indexed");
        }
    }

    ScanResult {
        files,
        aliases,
        skipped,
    }
}

/// Hand every file [`scan`] would list to `on_file`, on the walking threads, as it is found.
///
/// [`scan`] answers with a [`FileMeta`] per file and nothing else, so a caller that then *reads*
/// those files does it afterwards, one at a time, however many cores are idle: that is what
/// [`crate::index::Index::matches_in`]'s caller in the search pane was paying for. Here the work
/// happens inside the walk instead. `on_file` returns `false` to stop it — [`WalkState::Quit`] on
/// the thread that said so, and the passes still queued are dropped — which is how a filled row
/// budget stops a walk it no longer needs.
///
/// Two things [`scan`] does are not on offer, both because they are whole-result operations that
/// cannot exist while the walk is still running: the `(dev, ino)` dedup, so a file reachable by
/// two paths is handed over twice, and any order at all. A caller that needs either sorts what it
/// kept, which is cheap exactly when the caller keeps few rows.
pub fn visit(root: &Path, opts: &ScanOptions, on_file: &(dyn Fn(FileMeta) -> bool + Send + Sync)) {
    walk_passes(root, "", &[], opts, &|f| match on_file(f) {
        true => WalkState::Continue,
        false => WalkState::Quit,
    });
}

/// The pass queue both entry points share: the vault (or its folder `dir`), then one pass per
/// accepted symlink target. Returns what was skipped; the files went to `on_file`.
fn walk_passes(
    root: &Path,
    dir: &str,
    linked: &[PathBuf],
    opts: &ScanOptions,
    on_file: &(dyn Fn(FileMeta) -> WalkState + Send + Sync),
) -> Vec<Skipped> {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // A Mutex around the accepted-symlink-target list: `admit_symlink` holds it across the
    // overlap check and the push, so two threads cannot accept overlapping targets. A handful of
    // directory symlinks in a real vault means contention is nil.
    let followed: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(linked.to_vec()));
    // `ignore` stops the pass that quit, and nothing tells us it did: the flag is what keeps a
    // symlink target from being walked after the caller has said it has enough.
    let quit = AtomicBool::new(false);
    let sink = |f: FileMeta| {
        let state = on_file(f);
        if matches!(state, WalkState::Quit) {
            quit.store(true, Ordering::Relaxed);
        }
        state
    };

    let mut skipped = Vec::new();
    let mut queue: VecDeque<Pass> = VecDeque::new();
    queue.push_back(Pass {
        root: match dir {
            "" => root.to_path_buf(),
            dir => root.join(dir),
        },
        prefix: dir.to_string(),
        gitignore: opts.vault_gitignore,
        // The All toggle exists to reach exactly these trees, so it turns the rule off.
        dir_gitignore: !opts.vault_gitignore && !opts.include_skipped,
    });

    // Breadth-first over passes: the vault, then one pass per accepted symlink target, then
    // any symlink targets found inside those. The `followed` set is shared, so a target can
    // only ever be walked once however deep the chain of links is.
    while let Some(pass) = queue.pop_front() {
        if quit.load(Ordering::Relaxed) {
            break;
        }
        let out = walk_pass(&pass, opts, &canonical_root, &followed, &sink);
        skipped.extend(out.skipped);
        for (target, prefix) in out.targets {
            queue.push_back(Pass {
                root: target,
                prefix,
                gitignore: opts.target_gitignore,
                dir_gitignore: false,
            });
        }
    }
    skipped
}

/// One walk root: the vault itself, or a directory-symlink target mapped under `prefix`.
struct Pass {
    root: PathBuf,
    /// Vault-relative path this root appears at ("" for the vault itself).
    prefix: String,
    /// Honour the ignore files whole, files included.
    gitignore: bool,
    /// Honour them for directories only: the vault's rule, where hiding a file would hide a note.
    /// Never set on a symlink target — that tree either honours its ignore files whole or has
    /// been told to ignore them, and a half rule in between is nobody's ask.
    dir_gitignore: bool,
}

#[derive(Default)]
struct PassOut {
    skipped: Vec<Skipped>,
    /// Accepted directory symlinks: `(canonical target, vault-relative path of the link)`.
    targets: Vec<(PathBuf, String)>,
}

enum Msg {
    Skip(Skipped),
    Target(PathBuf, String),
}

fn walk_pass(
    pass: &Pass,
    opts: &ScanOptions,
    canonical_root: &Path,
    followed: &Arc<Mutex<Vec<PathBuf>>>,
    on_file: &(dyn Fn(FileMeta) -> WalkState + Send + Sync),
) -> PassOut {
    let mut b = WalkBuilder::new(&pass.root);
    b.follow_links(false) // symlinks are admitted by hand, then walked as their own pass
        .hidden(false) // `.obsidian` must be indexed; `.git` is filtered explicitly below
        .parents(false) // never inherit ignore rules from above the walk root
        .git_global(false)
        .git_ignore(pass.gitignore)
        .git_exclude(pass.gitignore)
        .ignore(pass.gitignore)
        .require_git(false) // targets are repos, but the vault usually is not
        .max_depth(opts.max_depth)
        .threads(if opts.threads == 0 {
            std::thread::available_parallelism().map_or(4, |n| n.get())
        } else {
            opts.threads
        });

    let (tx, rx) = mpsc::channel::<Msg>();
    b.build_parallel().run(|| {
        let tx = tx.clone();
        let followed = Arc::clone(followed);
        let canonical_root = canonical_root.to_path_buf();
        let walk_root = pass.root.clone();
        let prefix = pass.prefix.clone();
        let follow_links = opts.follow_links;
        let skip_deps = opts.skip_dependency_trees && !opts.include_skipped;
        let include_skipped = opts.include_skipped;
        let enter_ignored = opts.enter_ignored_dirs;
        // Per thread rather than shared: `IncrementalIgnore` caches what it reads behind `&mut`,
        // and a lock per directory would be paid on the one hot path the walk has. Rooted at the
        // vault whichever folder the pass starts at, so the rules above that folder hold too.
        let mut ignores = pass.dir_gitignore.then(|| dir_ignores(&canonical_root));
        Box::new(move |result| {
            let entry = match result {
                Ok(e) => e,
                Err(err) => {
                    let _ = tx.send(Msg::Skip(classify_error(&err)));
                    return WalkState::Continue;
                }
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            if entry.depth() > 0 && never_walked(&name, include_skipped) {
                return WalkState::Skip;
            }
            // Depth 0 is the walk root — the vault the user opened, or a symlink target they
            // linked in — and is always kept, marker or not.
            if skip_deps
                && entry.depth() > 0
                && entry.file_type().is_some_and(|t| t.is_dir())
                && is_dependency_tree(path)
            {
                let _ = tx.send(Msg::Skip(Skipped {
                    path: path.to_path_buf(),
                    reason: SkipReason::DependencyTree,
                }));
                return WalkState::Skip;
            }
            let rel_path = match path.strip_prefix(&walk_root) {
                Ok(r) if r.as_os_str().is_empty() => return WalkState::Continue, // the root itself
                Ok(r) if prefix.is_empty() => r.to_string_lossy().into_owned(),
                Ok(r) => format!("{prefix}/{}", r.to_string_lossy()),
                Err(_) => return WalkState::Continue,
            };
            // A directory git ignores is not walked; the file tree opens it a level at a time
            // (`unindexed_children`), and `ignored_files` walks it, told which it is. Real
            // directories only — a directory *symlink* is admitted
            // below and then walked as its own pass, which honours the target's own ignore files
            // whole, so the tree behind it is pruned there instead.
            if let Some(ignores) = ignores.as_mut()
                && entry.file_type().is_some_and(|t| t.is_dir())
                && ignores.matched(&rel_path, true).is_ignore()
            {
                let _ = tx.send(Msg::Skip(Skipped {
                    path: path.to_path_buf(),
                    reason: SkipReason::GitIgnored,
                }));
                if !enter_ignored {
                    return WalkState::Skip;
                }
            }
            let io_skip = |tx: &mpsc::Sender<Msg>| {
                let _ = tx.send(Msg::Skip(Skipped {
                    path: path.to_path_buf(),
                    reason: SkipReason::Io,
                }));
            };

            // `follow_links(false)` means `entry.metadata()` is an lstat, so a symlink needs an
            // explicit stat of its target: that is what makes file symlinks dedup by (dev, ino).
            let meta = if entry.path_is_symlink() {
                match std::fs::metadata(path) {
                    Ok(m) => {
                        if m.is_dir() {
                            if !follow_links {
                                return WalkState::Skip;
                            }
                            match admit_symlink(path, &canonical_root, &followed) {
                                Ok(target) => {
                                    let _ = tx.send(Msg::Target(target, rel_path.clone()));
                                }
                                Err(reason) => {
                                    let _ = tx.send(Msg::Skip(Skipped {
                                        path: path.to_path_buf(),
                                        reason,
                                    }));
                                    return WalkState::Skip;
                                }
                            }
                        }
                        m
                    }
                    Err(_) => {
                        io_skip(&tx); // broken symlink
                        return WalkState::Skip;
                    }
                }
            } else {
                match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => {
                        io_skip(&tx);
                        return WalkState::Continue;
                    }
                }
            };

            on_file(file_meta(rel_path, path, &meta))
        })
    });
    drop(tx);

    let mut out = PassOut::default();
    for msg in rx {
        match msg {
            Msg::Skip(s) => out.skipped.push(s),
            Msg::Target(t, rel) => out.targets.push((t, rel)),
        }
    }
    out
}

/// Decide whether a directory symlink may be walked, recording it if so.
fn admit_symlink(
    link: &Path,
    canonical_root: &Path,
    followed: &Mutex<Vec<PathBuf>>,
) -> Result<PathBuf, SkipReason> {
    let target = link.canonicalize().map_err(|_| SkipReason::Io)?;
    // The link sits inside its own target: descending would revisit its own ancestors forever.
    let here = link
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or_default();
    if here.starts_with(&target) {
        return Err(SkipReason::SymlinkLoop);
    }
    // Already reachable by walking the vault directly.
    if target.starts_with(canonical_root) {
        return Err(SkipReason::TargetInsideVault);
    }
    let mut followed = followed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if followed.iter().any(|t| target.starts_with(t)) {
        return Err(SkipReason::TargetOverlapsSymlink);
    }
    followed.push(target.clone());
    Ok(target)
}

/// `ignore::Error` nests the interesting variant inside `WithPath`/`WithDepth` wrappers.
fn classify_error(err: &ignore::Error) -> Skipped {
    let mut path = PathBuf::new();
    let mut e = err;
    loop {
        match e {
            ignore::Error::WithPath { path: p, err } => {
                path = p.clone();
                e = err;
            }
            ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
                e = err
            }
            ignore::Error::Loop { child, .. } => {
                return Skipped {
                    path: child.clone(),
                    reason: SkipReason::SymlinkLoop,
                };
            }
            _ => {
                return Skipped {
                    path,
                    reason: SkipReason::Io,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn rels(r: &ScanResult) -> Vec<&str> {
        r.files.iter().map(|f| f.rel_path.as_str()).collect()
    }

    #[test]
    fn dir_symlink_to_outside_is_followed() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("ext.md"), "ext").unwrap();
        let vault = tempfile::tempdir().unwrap();
        fs::write(vault.path().join("a.md"), "a").unwrap();
        symlink(outside.path(), vault.path().join("linked")).unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        assert!(rels(&r).contains(&"a.md"), "{:?}", rels(&r));
        assert!(rels(&r).contains(&"linked/ext.md"), "{:?}", rels(&r));
        assert!(r.skipped.is_empty(), "{:?}", r.skipped);
    }

    #[test]
    fn dir_symlink_into_vault_is_skipped_not_duplicated() {
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("real")).unwrap();
        fs::write(vault.path().join("real/n.md"), "n").unwrap();
        symlink(vault.path().join("real"), vault.path().join("shortcut")).unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        assert_eq!(rels(&r), vec!["real", "real/n.md"]);
        assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
        assert_eq!(r.skipped[0].reason, SkipReason::TargetInsideVault);
    }

    #[test]
    fn two_symlinks_to_one_dir_yield_one_copy() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("ext.md"), "ext").unwrap();
        let vault = tempfile::tempdir().unwrap();
        symlink(outside.path(), vault.path().join("one")).unwrap();
        symlink(outside.path(), vault.path().join("two")).unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        // Exactly one of the two links is followed; the other is reported, not walked.
        assert_eq!(
            r.files
                .iter()
                .filter(|f| f.rel_path.ends_with("ext.md"))
                .count(),
            1
        );
        assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
        assert_eq!(r.skipped[0].reason, SkipReason::TargetOverlapsSymlink);
    }

    #[test]
    fn symlink_loop_terminates() {
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("d")).unwrap();
        // d/up -> the vault root: classic self-referential loop.
        symlink(vault.path(), vault.path().join("d/up")).unwrap();
        fs::write(vault.path().join("d/n.md"), "n").unwrap();

        let r = scan(vault.path(), &ScanOptions::default()); // must not hang
        assert_eq!(rels(&r), vec!["d", "d/n.md"]);
        assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
    }

    #[test]
    fn file_symlink_becomes_an_alias() {
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("d")).unwrap();
        fs::write(vault.path().join("d/note.md"), "hello").unwrap();
        symlink(
            vault.path().join("d/note.md"),
            vault.path().join("alias.md"),
        )
        .unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        // Shallowest path wins the FileMeta; the deeper one becomes the alias.
        assert!(rels(&r).contains(&"alias.md"), "{:?}", rels(&r));
        assert!(!rels(&r).contains(&"d/note.md"), "{:?}", rels(&r));
        assert_eq!(r.aliases.len(), 1);
        assert_eq!(r.aliases[0].rel_path, "d/note.md");
        assert_eq!(r.aliases[0].target_rel_path, "alias.md");
    }

    #[test]
    fn hardlink_dedups_to_one_file_plus_alias() {
        let vault = tempfile::tempdir().unwrap();
        fs::write(vault.path().join("a.md"), "x").unwrap();
        fs::hard_link(vault.path().join("a.md"), vault.path().join("b.md")).unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        assert_eq!(rels(&r), vec!["a.md"]);
        assert_eq!(r.aliases.len(), 1);
        assert_eq!(r.aliases[0].rel_path, "b.md");
        assert_eq!(r.aliases[0].target_rel_path, "a.md");
    }

    #[test]
    fn syncthing_temp_dropped_and_conflicts_marked() {
        let vault = tempfile::tempdir().unwrap();
        fs::write(vault.path().join("n.md"), "n").unwrap();
        fs::write(vault.path().join(".syncthing.n.md.tmp"), "t").unwrap();
        fs::write(vault.path().join("~syncthing~n.md.tmp"), "t").unwrap();
        fs::write(
            vault
                .path()
                .join("n.sync-conflict-20240101-120000-ABCDEFG.md"),
            "c",
        )
        .unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        assert_eq!(
            rels(&r),
            vec!["n.md", "n.sync-conflict-20240101-120000-ABCDEFG.md"]
        );
        assert_eq!(r.files[0].kind, FileKind::Markdown);
        assert_eq!(r.files[1].kind, FileKind::Conflict);
    }

    #[test]
    fn hard_skip_dirs_and_ignore_files() {
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir_all(vault.path().join(".git/objects")).unwrap();
        fs::write(vault.path().join(".git/objects/x"), "x").unwrap();
        fs::create_dir(vault.path().join("node_modules")).unwrap();
        fs::write(vault.path().join("node_modules/y.md"), "y").unwrap();
        fs::create_dir(vault.path().join(".obsidian")).unwrap();
        fs::write(vault.path().join(".obsidian/app.json"), "{}").unwrap();
        fs::create_dir(vault.path().join(".trash")).unwrap();
        fs::write(vault.path().join(".trash/t.md"), "t").unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        let paths = rels(&r);
        assert!(!paths.iter().any(|p| p.starts_with(".git/")), "{paths:?}");
        assert!(
            !paths.iter().any(|p| p.starts_with("node_modules/")),
            "{paths:?}"
        );
        assert!(!paths.iter().any(|p| p.starts_with(".trash")), "{paths:?}");
        assert!(paths.contains(&".obsidian/app.json"), "{paths:?}");
    }

    /// The whole point of the marker rule: a dependency tree costs one skip report instead of
    /// tens of thousands of rows, and a directory of the same shape without a marker is untouched.
    #[test]
    fn dependency_markers_skip_the_tree_but_a_plain_directory_survives() {
        let vault = tempfile::tempdir().unwrap();
        for (dir, marker) in [(".venv", "pyvenv.cfg"), ("target", "CACHEDIR.TAG")] {
            fs::create_dir_all(vault.path().join(dir).join("deep")).unwrap();
            fs::write(vault.path().join(dir).join(marker), "x").unwrap();
            fs::write(vault.path().join(dir).join("deep/junk.md"), "junk").unwrap();
        }
        // Same shape, no marker: an ordinary folder of notes that happens to be called `target`.
        fs::create_dir_all(vault.path().join("Projects/target/deep")).unwrap();
        fs::write(vault.path().join("Projects/target/deep/n.md"), "n").unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        let paths = rels(&r);
        assert!(!paths.iter().any(|p| p.contains("junk.md")), "{paths:?}");
        assert!(!paths.contains(&".venv"), "{paths:?}");
        assert!(paths.contains(&"Projects/target/deep/n.md"), "{paths:?}");
        assert_eq!(
            r.skipped
                .iter()
                .filter(|s| s.reason == SkipReason::DependencyTree)
                .count(),
            2,
            "{:?}",
            r.skipped
        );

        // `stat_one` is the watcher's path into the same rows and must agree, or a `pip install`
        // would put the tree back one event at a time.
        assert!(
            stat_one(vault.path(), ".venv/deep/junk.md")
                .unwrap()
                .is_none()
        );
        assert!(stat_one(vault.path(), "target").unwrap().is_none());
        assert!(
            stat_one(vault.path(), "Projects/target/deep/n.md")
                .unwrap()
                .is_some()
        );

        // The escape hatch: opting back in indexes them again.
        let loose = ScanOptions {
            skip_dependency_trees: false,
            ..ScanOptions::default()
        };
        assert!(
            rels(&scan(vault.path(), &loose))
                .iter()
                .any(|p| p.contains("junk.md"))
        );
    }

    /// What the Search pane's All toggle reaches, and what it still cannot: every skipped tree
    /// opens, `.git` and `.trash` do not.
    #[test]
    fn include_skipped_opens_every_tree_but_the_two_that_are_never_ours() {
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir_all(vault.path().join("node_modules")).unwrap();
        fs::write(vault.path().join("node_modules/dep.js"), "js").unwrap();
        fs::create_dir_all(vault.path().join("target")).unwrap();
        fs::write(vault.path().join("target/CACHEDIR.TAG"), "x").unwrap();
        fs::write(vault.path().join("target/out.txt"), "out").unwrap();
        fs::create_dir_all(vault.path().join(".git")).unwrap();
        fs::write(vault.path().join(".git/config"), "c").unwrap();
        fs::create_dir_all(vault.path().join(".trash")).unwrap();
        fs::write(vault.path().join(".trash/x.md"), "x").unwrap();
        // A symlinked code repo, which is the one pass that honours a `.gitignore`.
        let ext = tempfile::tempdir().unwrap();
        fs::write(ext.path().join(".gitignore"), "build/\n").unwrap();
        fs::create_dir_all(ext.path().join("build")).unwrap();
        fs::write(ext.path().join("build/o.js"), "o").unwrap();
        std::os::unix::fs::symlink(ext.path(), vault.path().join("code")).unwrap();
        fs::write(vault.path().join("n.md"), "n").unwrap();

        let default_scan = scan(vault.path(), &ScanOptions::default());
        let tight = rels(&default_scan);
        for skipped in ["node_modules/dep.js", "target/out.txt", "code/build/o.js"] {
            assert!(
                !tight.contains(&skipped),
                "{skipped} is in the index's walk"
            );
        }

        // Everything open. `Vault::grep_unindexed` keeps `target_gitignore` on, so a symlinked
        // repo's own build output stays out even with All; that is somebody else's build tree,
        // and leaving it out is what keeps a per-query walk affordable.
        let loose = ScanOptions {
            include_skipped: true,
            target_gitignore: false,
            skip_dependency_trees: false,
            ..ScanOptions::default()
        };
        let loose_scan = scan(vault.path(), &loose);
        let paths = rels(&loose_scan);
        for wanted in ["node_modules/dep.js", "target/out.txt", "code/build/o.js"] {
            assert!(paths.contains(&wanted), "{wanted} missing from {paths:?}");
        }
        assert!(
            !paths.iter().any(|p| p.starts_with(".git")),
            "the repository's own storage is never ours: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with(".trash")),
            "what the user threw away stays thrown away: {paths:?}"
        );
    }

    #[test]
    fn vault_gitignore_does_not_hide_notes_unless_asked() {
        let vault = tempfile::tempdir().unwrap();
        // Exactly the user's real vault shape: a notes repo that gitignores its own markdown.
        fs::write(vault.path().join(".gitignore"), "*.md\n!README.md\n").unwrap();
        fs::write(vault.path().join("note.md"), "n").unwrap();
        fs::create_dir(vault.path().join("refs")).unwrap();
        fs::write(vault.path().join("refs/r.md"), "r").unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        assert!(rels(&r).contains(&"note.md"), "{:?}", rels(&r));
        assert!(rels(&r).contains(&"refs/r.md"), "{:?}", rels(&r));

        let strict = ScanOptions {
            vault_gitignore: true,
            ..ScanOptions::default()
        };
        let r = scan(vault.path(), &strict);
        assert!(!rels(&r).contains(&"note.md"), "{:?}", rels(&r));
    }

    /// The heavy-folder rule: a gitignored *directory* costs the index nothing, while a
    /// gitignored *file* is indexed like any other. The tree is what opens the directory.
    #[test]
    fn a_gitignored_directory_is_not_walked_but_a_gitignored_file_is_indexed() {
        let vault = tempfile::tempdir().unwrap();
        let at = |p: &str| vault.path().join(p);
        fs::create_dir(at("Projects")).unwrap();
        fs::write(at("Projects/.gitignore"), "mlruns/\nscratch.md\n").unwrap();
        fs::write(at("Projects/note.md"), "n").unwrap();
        fs::write(at("Projects/scratch.md"), "s").unwrap();
        fs::create_dir_all(at("Projects/mlruns/0/run")).unwrap();
        fs::write(at("Projects/mlruns/0/run/meta.yaml"), "m").unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        let paths = rels(&r);
        assert!(paths.contains(&"Projects/note.md"), "{paths:?}");
        assert!(paths.contains(&"Projects/scratch.md"), "{paths:?}");
        assert!(
            !paths.iter().any(|p| p.contains("mlruns")),
            "the ignored tree was walked: {paths:?}"
        );
        assert_eq!(
            r.skipped
                .iter()
                .filter(|s| s.reason == SkipReason::GitIgnored)
                .count(),
            1,
            "{:?}",
            r.skipped
        );

        // The watcher path agrees, or a training run would put the tree back event by event.
        assert!(stat_one(vault.path(), "Projects/mlruns").unwrap().is_none());
        assert!(
            stat_one(vault.path(), "Projects/mlruns/0/run/meta.yaml")
                .unwrap()
                .is_none()
        );
        assert!(
            stat_one(vault.path(), "Projects/scratch.md")
                .unwrap()
                .is_some(),
            "a gitignored file is still a file the vault holds"
        );

        // And the tree lists it beside its indexed siblings, then opens it a level at a time.
        let names = |rel: &str, held: HashSet<&str>| {
            let mut n: Vec<String> = unindexed_children(vault.path(), rel, &held)
                .unwrap()
                .into_iter()
                .map(|(rel, ..)| rel)
                .collect();
            n.sort();
            n
        };
        let held = HashSet::from(["Projects/note.md", "Projects/scratch.md"]);
        assert_eq!(names("Projects", held), ["Projects/mlruns"]);
        assert_eq!(
            names("Projects/mlruns", HashSet::new()),
            ["Projects/mlruns/0"]
        );

        // The Search pane's All toggle is the way in: `grep_unindexed` walks with it on.
        let all = ScanOptions {
            include_skipped: true,
            skip_dependency_trees: false,
            ..ScanOptions::default()
        };
        assert!(
            rels(&scan(vault.path(), &all))
                .iter()
                .any(|p| p.contains("mlruns/0/run/meta.yaml"))
        );
    }

    #[test]
    fn symlink_target_honours_its_own_gitignore() {
        let repo = tempfile::tempdir().unwrap();
        fs::write(repo.path().join(".gitignore"), "target/\n.venv/\n").unwrap();
        fs::write(repo.path().join("README.md"), "readme").unwrap();
        for junk in ["target", ".venv"] {
            fs::create_dir(repo.path().join(junk)).unwrap();
            fs::write(repo.path().join(junk).join("junk.md"), "junk").unwrap();
        }
        let vault = tempfile::tempdir().unwrap();
        fs::write(vault.path().join("n.md"), "n").unwrap();
        symlink(repo.path(), vault.path().join("code")).unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        let paths = rels(&r);
        assert!(paths.contains(&"code/README.md"), "{paths:?}");
        assert!(!paths.iter().any(|p| p.contains("junk.md")), "{paths:?}");

        // Turning it off pulls the build output in, which is why it defaults to on.
        let loose = ScanOptions {
            target_gitignore: false,
            ..ScanOptions::default()
        };
        let r = scan(vault.path(), &loose);
        assert!(rels(&r).iter().any(|p| p.contains("junk.md")));
    }

    /// `stat_one` is the watcher path into the same rows `scan` produces: if the two ever
    /// disagree, an incremental update writes a row a full reconcile would then rewrite.
    #[test]
    fn stat_one_matches_scan_entry() {
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("sub")).unwrap();
        fs::write(vault.path().join("sub/n.md"), "n").unwrap();
        fs::write(
            vault
                .path()
                .join("n.sync-conflict-20240101-120000-ABCDEFG.md"),
            "c",
        )
        .unwrap();
        fs::create_dir(vault.path().join(".git")).unwrap();
        fs::write(vault.path().join(".git/config"), "c").unwrap();
        fs::write(vault.path().join(".accent-xyz"), "t").unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        for rel in [
            "sub",
            "sub/n.md",
            "n.sync-conflict-20240101-120000-ABCDEFG.md",
        ] {
            let from_scan = r.files.iter().find(|f| f.rel_path == rel).unwrap();
            let from_stat = stat_one(vault.path(), rel).unwrap().unwrap();
            assert_eq!(&from_stat, from_scan, "{rel}");
        }
        assert_eq!(
            r.files.iter().find(|f| f.rel_path == "sub").unwrap().kind,
            FileKind::Dir
        );

        for hidden in [".git/config", ".accent-xyz", ".syncthing.n.md.tmp"] {
            assert!(
                stat_one(vault.path(), hidden).unwrap().is_none(),
                "{hidden}"
            );
        }
        assert_eq!(
            stat_one(vault.path(), "gone.md").unwrap_err(),
            crate::Error::NotFound("gone.md".to_string())
        );
    }

    #[test]
    fn symlink_nested_in_a_target_obeys_the_same_rules() {
        let outer = tempfile::tempdir().unwrap();
        let inner = tempfile::tempdir().unwrap();
        fs::write(inner.path().join("deep.md"), "deep").unwrap();
        // outer/link -> inner, and outer/back -> outer (a loop through the target tree).
        symlink(inner.path(), outer.path().join("link")).unwrap();
        symlink(outer.path(), outer.path().join("back")).unwrap();
        let vault = tempfile::tempdir().unwrap();
        symlink(outer.path(), vault.path().join("ext")).unwrap();

        let r = scan(vault.path(), &ScanOptions::default()); // must not hang
        assert!(rels(&r).contains(&"ext/link/deep.md"), "{:?}", rels(&r));
        let loops: Vec<_> = r
            .skipped
            .iter()
            .filter(|s| s.reason == SkipReason::SymlinkLoop)
            .collect();
        assert_eq!(loops.len(), 1, "{:?}", r.skipped);
    }

    /// A vault holding one skipped tree of each kind, beside an ordinary note.
    fn skipped_vault() -> tempfile::TempDir {
        let vault = tempfile::tempdir().unwrap();
        let at = |p: &str| vault.path().join(p);
        fs::write(at("Note.md"), "n").unwrap();
        fs::create_dir_all(at("node_modules/pkg")).unwrap();
        fs::write(at("node_modules/pkg/index.js"), "js").unwrap();
        fs::create_dir(at(".venv")).unwrap();
        fs::write(at(".venv/pyvenv.cfg"), "").unwrap();
        fs::create_dir(at(".git")).unwrap();
        fs::write(at(".git/HEAD"), "").unwrap();
        vault
    }

    #[test]
    fn is_unindexed_covers_the_trees_the_walk_refuses() {
        let vault = skipped_vault();
        let mut ignores = dir_ignores(vault.path());
        let mut un = |rel| is_unindexed(vault.path(), rel, &mut ignores);
        let dep = Some(Unindexed::Dependency);
        assert_eq!(un("node_modules"), dep);
        assert_eq!(un("node_modules/pkg/index.js"), dep);
        assert_eq!(
            un(".venv"),
            dep,
            "a dependency marker is checked on the path itself"
        );
        assert_eq!(un(".venv/lib"), dep);
        assert_eq!(un(".git"), dep);
        assert_eq!(un("Note.md"), None);
        // The root is the user's own choice, marker or not.
        assert_eq!(un(""), None);
    }

    #[test]
    fn unindexed_children_lists_the_skipped_trees_beside_indexed_siblings() {
        let vault = skipped_vault();
        let names = |rel| {
            let mut n: Vec<String> = unindexed_children(vault.path(), rel, &HashSet::new())
                .unwrap()
                .into_iter()
                .map(|(rel, ..)| rel)
                .collect();
            n.sort();
            n
        };
        // At an indexed level only the skipped directories are missing from the index; the note
        // beside them is already in the listing this merges into, and `.git` is never reachable.
        assert_eq!(names(""), [".venv", "node_modules"]);
        // Inside one, everything is: nothing under it is indexed at all.
        assert_eq!(names("node_modules"), ["node_modules/pkg"]);
        assert_eq!(names("node_modules/pkg"), ["node_modules/pkg/index.js"]);
    }

    /// The tree's whole question: a gitignored folder is the reader's own, a dependency tree is
    /// not, and one nested in the other is still not.
    #[test]
    fn unindexed_children_says_which_trees_are_somebody_elses() {
        let vault = tempfile::tempdir().unwrap();
        let at = |p: &str| vault.path().join(p);
        fs::write(at(".gitignore"), "build/\n").unwrap();
        fs::create_dir_all(at("build/node_modules/pkg")).unwrap();
        fs::write(at("build/out.md"), "o").unwrap();
        fs::write(at("build/node_modules/pkg/index.js"), "i").unwrap();

        let why = |rel: &str, name: &str| {
            unindexed_children(vault.path(), rel, &HashSet::new())
                .unwrap()
                .into_iter()
                .find(|(r, ..)| r == name)
                .map(|(.., why)| why)
        };
        assert_eq!(why("", "build"), Some(Unindexed::Ignored));
        assert_eq!(why("build", "build/out.md"), Some(Unindexed::Ignored));
        assert_eq!(
            why("build", "build/node_modules"),
            Some(Unindexed::Dependency),
            "a dependency tree under a gitignored folder is still nobody's to edit"
        );
        assert_eq!(
            why("build/node_modules", "build/node_modules/pkg"),
            Some(Unindexed::Dependency)
        );
    }

    #[test]
    fn unindexed_children_leaves_out_what_the_index_already_holds() {
        let vault = skipped_vault();
        // A directory that gained its marker since the last scan is still in the index, and must
        // not come back a second time from the disk.
        let held = HashSet::from(["node_modules"]);
        let rows = unindexed_children(vault.path(), "", &held).unwrap();
        assert_eq!(
            rows.iter().map(|(r, ..)| r.as_str()).collect::<Vec<_>>(),
            [".venv"]
        );
    }

    #[test]
    fn unindexed_children_classifies_by_name_as_the_walk_does() {
        let vault = skipped_vault();
        fs::write(vault.path().join("node_modules/README.md"), "r").unwrap();
        let rows = unindexed_children(vault.path(), "node_modules", &HashSet::new()).unwrap();
        let kind = |rel: &str| rows.iter().find(|(r, ..)| r == rel).map(|(_, k, _)| *k);
        assert_eq!(kind("node_modules/README.md"), Some(FileKind::Markdown));
        assert_eq!(kind("node_modules/pkg"), Some(FileKind::Dir));
    }

    /// The files in a gitignored folder are listed, a dependency tree inside one is not, and
    /// nothing the index walks is.
    #[test]
    fn ignored_files_lists_the_gitignored_folders_and_nothing_else() {
        let vault = tempfile::tempdir().unwrap();
        let write = |rel: &str| {
            let path = vault.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        };
        fs::write(vault.path().join(".gitignore"), "ignored/\n*.log\n").unwrap();
        for rel in [
            "note.md",
            "kept.log",
            "node_modules/dep.md",
            "ignored/Deep Note.md",
            "ignored/sub/paper.pdf",
            "ignored/node_modules/dep.md",
            "ignored/env/pyvenv.cfg",
            "ignored/env/lib.md",
        ] {
            write(rel);
        }
        assert_eq!(
            ignored_files(vault.path()),
            ["ignored/Deep Note.md", "ignored/sub/paper.pdf"]
        );
    }

    /// The visitor sees what `scan` lists, symlinked target and all, and stops when it says so.
    #[test]
    fn visit_sees_what_scan_sees_and_stops_when_asked() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("ext.md"), "ext").unwrap();
        let vault = tempfile::tempdir().unwrap();
        fs::create_dir(vault.path().join("d")).unwrap();
        for i in 0..50 {
            fs::write(vault.path().join(format!("d/n{i:02}.md")), "n").unwrap();
        }
        symlink(outside.path(), vault.path().join("linked")).unwrap();

        let opts = ScanOptions::default();
        let seen = Mutex::new(Vec::new());
        visit(vault.path(), &opts, &|f| {
            seen.lock().unwrap().push(f.rel_path);
            true
        });
        let mut seen = seen.into_inner().unwrap();
        seen.sort();
        let mut want: Vec<String> = rels(&scan(vault.path(), &opts))
            .iter()
            .map(|s| s.to_string())
            .collect();
        want.sort();
        assert_eq!(seen, want);

        // One thread, so stopping is not a race: the walk ends on the visitor's word rather
        // than reading the other fifty entries.
        let single = ScanOptions {
            threads: 1,
            ..ScanOptions::default()
        };
        let count = std::sync::atomic::AtomicUsize::new(0);
        visit(vault.path(), &single, &|_| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4
        });
        let n = count.into_inner();
        assert!(
            n < want.len(),
            "the walk did not stop: {n} of {}",
            want.len()
        );
    }
}
