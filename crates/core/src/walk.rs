//! Vault walk: symlink rules, (dev, ino) dedup, Syncthing temp/conflict filtering.
//!
//! Obsidian's *rules* (not its behaviour): a directory symlink is followed once, loops are
//! skipped, a symlink whose target lands inside the vault root (or inside a symlink target we
//! already accepted) is skipped and reported, file symlinks are followed and de-duplicated by
//! `(dev, ino)` so the same inode reached by two paths yields one [`FileMeta`] plus an alias.
//!
//! Ignore files differ inside and outside the vault, so the walk runs in two kinds of pass.
//! The **vault tree** honours only `.accentignore` (plus the hard-skip list): a notes vault
//! routinely gitignores `*.md` on purpose, and that must not hide the user's notes. A
//! **symlink target** is somebody else's tree — usually a code repo — so it honours its own
//! `.gitignore`/`.ignore` as well, which is what keeps `.venv`, `target` and friends out.
//! Each pass therefore walks with `follow_links(false)` and hands accepted directory symlinks
//! back as new passes; nested symlinks under a target obey exactly the same rules.
//!
//! ponytail: unix-only (`MetadataExt` for dev/ino/mtime_nsec). Targets are Linux + Android;
//! a Windows port would need a `cfg` branch using `FileIndex`/`VolumeSerialNumber`.

use ignore::{WalkBuilder, WalkState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

/// Directory names never worth indexing, whatever the ignore files say.
const HARD_SKIP_DIRS: &[&str] = &[".git", ".trash", "node_modules"];

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
    /// Broken symlink, permission denied, vanished mid-walk.
    Io,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SkipReason::TargetInsideVault => "symlink target is inside the vault",
            SkipReason::TargetOverlapsSymlink => "symlink target overlaps another symlink",
            SkipReason::SymlinkLoop => "symlink loop",
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
    /// `.accentignore` is always honoured.
    pub vault_gitignore: bool,
    /// Honour `.gitignore`/`.ignore` inside directory-symlink targets. On by default: those
    /// are external trees (code repos) whose build output nobody wants in a note index.
    pub target_gitignore: bool,
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

/// Walk `root`, applying the symlink rules. Returns files, aliases and skip reports.
pub fn scan(root: &Path, opts: &ScanOptions) -> ScanResult {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // ponytail: a Mutex around the accepted-symlink-target list. There are a handful of dir
    // symlinks in a real vault, so contention is nil; the race (two threads accepting
    // overlapping targets simultaneously) would only cost a duplicate walk, and the
    // (dev, ino) dedup below cleans that up anyway.
    let followed: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));

    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut queue: VecDeque<Pass> = VecDeque::new();
    queue.push_back(Pass {
        root: root.to_path_buf(),
        prefix: String::new(),
        gitignore: opts.vault_gitignore,
    });

    // Breadth-first over passes: the vault, then one pass per accepted symlink target, then
    // any symlink targets found inside those. The `followed` set is shared, so a target can
    // only ever be walked once however deep the chain of links is.
    while let Some(pass) = queue.pop_front() {
        let out = walk_pass(&pass, opts, &canonical_root, &followed);
        files.extend(out.files);
        skipped.extend(out.skipped);
        for (target, prefix) in out.targets {
            queue.push_back(Pass {
                root: target,
                prefix,
                gitignore: opts.target_gitignore,
            });
        }
    }

    // Deterministic winner: shallowest path, then lexicographic. `build_parallel` yields in an
    // arbitrary order, so "first path wins" only means anything after a sort.
    files.sort_unstable_by(|a, b| {
        let (da, db) = (a.rel_path.matches('/').count(), b.rel_path.matches('/').count());
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

    ScanResult {
        files,
        aliases,
        skipped,
    }
}

/// One walk root: the vault itself, or a directory-symlink target mapped under `prefix`.
struct Pass {
    root: PathBuf,
    /// Vault-relative path this root appears at ("" for the vault itself).
    prefix: String,
    gitignore: bool,
}

#[derive(Default)]
struct PassOut {
    files: Vec<FileMeta>,
    skipped: Vec<Skipped>,
    /// Accepted directory symlinks: `(canonical target, vault-relative path of the link)`.
    targets: Vec<(PathBuf, String)>,
}

enum Msg {
    File(FileMeta),
    Skip(Skipped),
    Target(PathBuf, String),
}

fn walk_pass(
    pass: &Pass,
    opts: &ScanOptions,
    canonical_root: &Path,
    followed: &Arc<Mutex<Vec<PathBuf>>>,
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
        .add_custom_ignore_filename(".accentignore") // always honoured, everywhere
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

            if entry.depth() > 0
                && (HARD_SKIP_DIRS.contains(&name.as_str()) || crate::fs::is_syncthing_temp(&name))
            {
                return WalkState::Skip;
            }
            let rel_path = match path.strip_prefix(&walk_root) {
                Ok(r) if r.as_os_str().is_empty() => return WalkState::Continue, // the root itself
                Ok(r) if prefix.is_empty() => r.to_string_lossy().into_owned(),
                Ok(r) => format!("{prefix}/{}", r.to_string_lossy()),
                Err(_) => return WalkState::Continue,
            };
            let io_skip = |tx: &mpsc::Sender<Msg>| {
                let _ = tx.send(Msg::Skip(Skipped {
                    path: path.to_path_buf(),
                    reason: SkipReason::Io,
                }));
            };

            // `follow_links(false)` means `entry.metadata()` is an lstat, so a symlink needs an
            // explicit stat of its target: that is what makes file symlinks dedup by (dev, ino).
            let (meta, is_dir) = if entry.path_is_symlink() {
                match std::fs::metadata(path) {
                    Ok(m) => {
                        let is_dir = m.is_dir();
                        if is_dir {
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
                        (m, is_dir)
                    }
                    Err(_) => {
                        io_skip(&tx); // broken symlink
                        return WalkState::Skip;
                    }
                }
            } else {
                match entry.metadata() {
                    Ok(m) => {
                        let is_dir = m.is_dir();
                        (m, is_dir)
                    }
                    Err(_) => {
                        io_skip(&tx);
                        return WalkState::Continue;
                    }
                }
            };

            let kind = if is_dir { FileKind::Dir } else { classify(&name) };
            let _ = tx.send(Msg::File(FileMeta {
                rel_path,
                canonical: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
                dev: meta.dev(),
                ino: meta.ino(),
                mtime_ns: meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
                size: meta.len(),
                kind,
            }));
            WalkState::Continue
        })
    });
    drop(tx);

    let mut out = PassOut::default();
    for msg in rx {
        match msg {
            Msg::File(f) => out.files.push(f),
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
    let mut followed = followed.lock().unwrap();
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
        assert_eq!(r.files.iter().filter(|f| f.rel_path.ends_with("ext.md")).count(), 1);
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
        symlink(vault.path().join("d/note.md"), vault.path().join("alias.md")).unwrap();

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
        fs::write(vault.path().join("n.sync-conflict-20240101-120000-ABCDEFG.md"), "c").unwrap();

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
        fs::write(vault.path().join(".accentignore"), "secret/\n").unwrap();
        fs::create_dir(vault.path().join("secret")).unwrap();
        fs::write(vault.path().join("secret/s.md"), "s").unwrap();

        let r = scan(vault.path(), &ScanOptions::default());
        let paths = rels(&r);
        assert!(!paths.iter().any(|p| p.starts_with(".git/")), "{paths:?}");
        assert!(!paths.iter().any(|p| p.starts_with("node_modules/")), "{paths:?}");
        assert!(!paths.iter().any(|p| p.starts_with("secret")), "{paths:?}");
        assert!(paths.contains(&".obsidian/app.json"), "{paths:?}");
    }

    #[test]
    fn vault_gitignore_does_not_hide_notes_unless_asked() {
        let vault = tempfile::tempdir().unwrap();
        // Exactly the user's real vault shape: a notes repo that gitignores its own markdown.
        fs::write(vault.path().join(".gitignore"), "*.md\n!README.md\nrefs\n").unwrap();
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
}
