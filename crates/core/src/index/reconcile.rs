//! Disk to rows: the full walk and the watcher's single-file path share one `upsert`.

use super::links::{BEST_FILE, resolve_links_of};
use super::schema::{BATCH, MAX_INDEXED_BODY};
use super::{Change, Index, Phase, Progress, ReconcileStats};
use crate::walk::{self, FileKind, ScanOptions};
use crate::{markdown, path};
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

/// What the diff decided to do with one scanned entry.
struct Job {
    idx: usize,
    existing_id: Option<i64>,
}

impl Index {
    /// Walk `root` and bring the index in line with it. Returns what changed.
    pub fn reconcile(
        &mut self,
        root: &Path,
        mut on_progress: impl FnMut(Progress),
    ) -> Result<ReconcileStats> {
        self.reconcile_with(root, &ScanOptions::default(), |_, p| on_progress(p))
    }

    /// [`reconcile`](Self::reconcile) with explicit walk options, and the index handed to
    /// `on_progress`. It is called between batches with no transaction open, so the caller can
    /// write in the middle of a long walk instead of after it.
    pub fn reconcile_with(
        &mut self,
        root: &Path,
        opts: &ScanOptions,
        mut on_progress: impl FnMut(&mut Index, Progress),
    ) -> Result<ReconcileStats> {
        let t_scan = Instant::now();
        let scan = walk::scan(root, opts);
        let mut stats = ReconcileStats {
            scanned: scan.files.len(),
            aliases: scan.aliases.len(),
            conflicts: scan
                .files
                .iter()
                .filter(|f| f.kind == FileKind::Conflict)
                .count(),
            skipped_symlinks: scan
                .skipped
                .iter()
                .filter(|s| {
                    !matches!(
                        s.reason,
                        walk::SkipReason::DependencyTree | walk::SkipReason::GitIgnored
                    )
                })
                .count(),
            scan_ms: t_scan.elapsed().as_millis() as u64,
            ..Default::default()
        };
        on_progress(
            self,
            Progress {
                phase: Phase::Scan,
                done: scan.files.len(),
                total: scan.files.len(),
            },
        );

        // One pass over `files` into memory. 47k rows of five integers is a few MB and turns the
        // per-file diff into a hash lookup instead of a query.
        let mut existing: HashMap<String, (i64, i64, i64, i64)> = HashMap::new();
        {
            let mut st = self
                .conn
                .prepare("SELECT id, rel_path, mtime_ns, size, ino FROM files")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                existing.insert(r.get(1)?, (r.get(0)?, r.get(2)?, r.get(3)?, r.get(4)?));
            }
        }

        // Nothing to compare against: the cold build, which resolves its links in one pass at
        // the end rather than file by file.
        let cold = existing.is_empty();

        let mut jobs: Vec<Job> = Vec::new();
        for (idx, f) in scan.files.iter().enumerate() {
            match existing.remove(&f.rel_path) {
                // `remove` doubles as the "seen" marker: leftovers are deletions.
                Some((id, mtime_ns, size, ino)) => {
                    if mtime_ns == f.mtime_ns && size == f.size as i64 && ino == f.ino as i64 {
                        stats.unchanged += 1;
                    } else {
                        jobs.push(Job {
                            idx,
                            existing_id: Some(id),
                        });
                    }
                }
                None => jobs.push(Job {
                    idx,
                    existing_id: None,
                }),
            }
        }
        let removed: Vec<i64> = existing.values().map(|(id, ..)| *id).collect();
        stats.removed = removed.len();

        let dirty = !jobs.is_empty() || !removed.is_empty();
        let total = jobs.len();

        if !removed.is_empty() {
            let tx = self.write_tx()?;
            for id in &removed {
                delete_file_rows(&tx, *id)?;
            }
            tx.commit()?;
        }

        let mut done = 0usize;
        // The files whose content really changed: a touched one kept its links and its keys.
        let mut changed: Vec<usize> = Vec::new();
        for chunk in jobs.chunks(BATCH) {
            let tx = self.write_tx()?;
            for job in chunk {
                let touched = stats.touched;
                upsert(&tx, &scan.files[job.idx], job.existing_id, &mut stats)?;
                if stats.touched == touched {
                    changed.push(job.idx);
                }
                done += 1;
            }
            tx.commit()?;
            on_progress(
                self,
                Progress {
                    phase: Phase::Index,
                    done,
                    total,
                },
            );
        }

        if dirty {
            // Aliases are a handful of rows; rewriting them beats diffing them.
            let tx = self.write_tx()?;
            tx.execute("DELETE FROM aliases", [])?;
            for a in &scan.aliases {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO aliases(rel_path, file_id)
                     SELECT ?1, id FROM files WHERE rel_path = ?2",
                )?
                .execute(params![a.rel_path, a.target_rel_path])?;
            }
            tx.commit()?;
        }
        // A removed file re-pointed its incoming links as it went; what is left is the changed
        // files' own links and the links their names answer to. A Syncthing pass that rewrote
        // every note with the same bytes changed nothing and resolves nothing.
        if cold {
            self.resolve_links()?;
        } else if !changed.is_empty() {
            let tx = self.write_tx()?;
            for idx in &changed {
                resolve_links_of(&tx, &scan.files[*idx].rel_path)?;
            }
            tx.commit()?;
        }
        if cold || !changed.is_empty() {
            on_progress(
                self,
                Progress {
                    phase: Phase::Resolve,
                    done: total,
                    total,
                },
            );
        }

        Ok(stats)
    }

    /// Bring one path in line with the disk. This is the watcher's entry point: the caller turns a
    /// filesystem event into a vault-relative path and lets the index decide what it means.
    ///
    /// A row whose `(mtime_ns, size, ino)` still match the disk is left completely untouched, which
    /// is what makes the echo of our own save free, and lets the UI ignore the resulting
    /// [`Change::Unchanged`] instead of reloading the buffer the user is typing in.
    ///
    /// ponytail: no `(dev, ino)` dedup here, so a note linked in twice gets a row per path until
    /// the next full reconcile collapses them. Aliases are rare and never wrong, only duplicated.
    pub fn update_file(&mut self, root: &Path, rel: &str) -> Result<Change> {
        let change = self.update_file_batched(root, rel)?;
        // The file's own links were just written unresolved, and a new note can be what a link
        // written long before it existed was waiting for — or a shorter path for one that
        // resolved deeper. Both are the links its keys name, and nothing else moved.
        if matches!(change, Change::Added(_) | Change::Updated(_)) {
            self.resolve_links_of(rel)?;
        }
        Ok(change)
    }

    /// [`update_file`](Self::update_file) without the link resolution, for a caller that is
    /// working through a batch of files and calls [`resolve_links`](Self::resolve_links) once
    /// when it is done. Links into the file stay unresolved until it does.
    pub fn update_file_batched(&mut self, root: &Path, rel: &str) -> Result<Change> {
        let meta = match walk::stat_one(root, rel) {
            Ok(Some(meta)) => meta,
            Ok(None) => return Ok(Change::Ignored),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.remove_file_batched(rel)?;
                return Ok(Change::Removed);
            }
            Err(e) => return Err(e).with_context(|| format!("stat {rel}")),
        };

        let existing: Option<(i64, i64, i64, i64)> = self
            .conn
            .prepare_cached("SELECT id, mtime_ns, size, ino FROM files WHERE rel_path = ?1")?
            .query_row([rel], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        if let Some((_, mtime_ns, size, ino)) = existing
            && mtime_ns == meta.mtime_ns
            && size == meta.size as i64
            && ino == meta.ino as i64
        {
            return Ok(Change::Unchanged);
        }

        let existing_id = existing.map(|(id, ..)| id);
        let tx = self.write_tx()?;
        upsert(&tx, &meta, existing_id, &mut ReconcileStats::default())?;
        tx.commit()?;
        Ok(match existing_id {
            Some(_) => Change::Updated(meta.kind),
            None => Change::Added(meta.kind),
        })
    }

    /// Drop `rel` and everything below it. A directory removal arrives as one event, so the
    /// subtree has to go with it. Returns how many rows went.
    ///
    /// The links that pointed into the subtree are re-resolved as each row goes
    /// ([`delete_file_rows`]), so there is nothing left for a batch caller to defer:
    /// [`remove_file_batched`](Self::remove_file_batched) is the same call, kept for symmetry
    /// with [`update_file_batched`](Self::update_file_batched).
    pub fn remove_file(&mut self, rel: &str) -> Result<usize> {
        self.remove_file_batched(rel)
    }

    /// See [`remove_file`](Self::remove_file).
    pub fn remove_file_batched(&mut self, rel: &str) -> Result<usize> {
        let (lo, hi) = path::subtree_range(rel);
        let ids: Vec<i64> = {
            let mut st = self.conn.prepare_cached(
                "SELECT id FROM files WHERE rel_path = ?1 OR (rel_path >= ?2 AND rel_path < ?3)",
            )?;
            let rows = st.query_map(params![rel, lo, hi], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if ids.is_empty() {
            return Ok(0);
        }

        let tx = self.write_tx()?;
        for id in &ids {
            delete_file_rows(&tx, *id)?;
        }
        tx.commit()?;
        Ok(ids.len())
    }
}

/// Index one scanned entry: read and hash markdown, then replace its file row and everything
/// derived from it. `existing_id` is the row it replaces, if any. Returns the file id.
///
/// The one place a file becomes index rows, shared by the full reconcile and the watcher's
/// [`Index::update_file`] so the two can never drift apart.
fn upsert(
    tx: &rusqlite::Transaction<'_>,
    f: &walk::FileMeta,
    existing_id: Option<i64>,
    stats: &mut ReconcileStats,
) -> Result<i64> {
    // Everything is read through `fs::read_text`, the same call the editor opens a tab with, so
    // that every byte offset the index hands out — a search hit, a backlink, a heading — lands on
    // the buffer it is applied to. That is what makes a CRLF note work: the buffer holds `\n`,
    // and so must the text the offsets were counted in. `read_text` also brings the NUL sniff
    // and the `lossy` flag; a lossy note is still indexed because its buffer holds the same
    // replacement characters, while a lossy source file is dropped rather than indexed as text
    // it is not. Only a note is read past [`MAX_INDEXED_BODY`], and the size test uses the stat
    // the walk already took rather than pulling a 15 MiB file into memory for the cap to throw
    // away. PDFs and binaries stay cheap `(mtime, size, ino)` rows; the `pdf` feature will add
    // text extraction and can reuse the same hash column when it does.
    //
    // The hash is of the normalised text, never of the raw file. Safe, because a hash is only
    // ever compared against an earlier hash of the same file taken the same way.
    let read = match f.kind {
        FileKind::Markdown => crate::fs::read_text(&f.canonical).ok(),
        // A diagram's body is XML whose every style key would come back as a search hit, so it
        // keeps a stat row only; searching its labels is a job for the diagram crate.
        FileKind::Other if f.size <= MAX_INDEXED_BODY && !crate::path::is_diagram(&f.rel_path) => {
            crate::fs::read_text(&f.canonical).ok()
        }
        _ => None,
    };
    let (hash, text) = match read {
        Some(crate::fs::Read::Text(t)) if f.kind == FileKind::Markdown || !t.lossy => {
            stats.bytes_read += t.text.len() as u64;
            (Some(blake3::hash(t.text.as_bytes())), Some(t.text))
        }
        // Vanished or unreadable mid-walk, binary, or over the cap: keep the stat row, drop the
        // content.
        _ => (None, None),
    };

    // Syncthing preserves origin mtimes, so mtime alone lies both ways; the hash is
    // the arbiter for "did the content really change".
    let same_content = match (existing_id, hash.as_ref()) {
        (Some(id), Some(h)) => {
            let old: Option<Vec<u8>> = tx
                .prepare_cached("SELECT content_hash FROM files WHERE id = ?1")?
                .query_row([id], |r| r.get(0))
                .optional()?
                .flatten();
            old.as_deref() == Some(h.as_bytes().as_slice())
        }
        _ => false,
    };

    if same_content && let Some(id) = existing_id {
        tx.prepare_cached(
            "UPDATE files SET canonical=?2, dev=?3, ino=?4, mtime_ns=?5, size=?6 WHERE id=?1",
        )?
        .execute(params![
            id,
            f.canonical.to_string_lossy(),
            f.dev as i64,
            f.ino as i64,
            f.mtime_ns,
            f.size as i64,
        ])?;
        stats.touched += 1;
        return Ok(id);
    }

    // Only a note is markdown. A `.py`'s `#` comments are not tags and its `#!` line is not a
    // heading, so nothing but a note reaches the analyser; the `path::stem` fallback below is what
    // gives every other file a title.
    let analysis = match f.kind {
        FileKind::Markdown => text.as_deref().map(markdown::analyze),
        _ => None,
    };
    let title = analysis
        .as_ref()
        .and_then(|a| a.title.clone())
        .or_else(|| Some(path::stem(&f.rel_path)));

    // `git_ignored` is the parent directory's, and only on an insert: an existing row keeps
    // whatever [`Index::set_excluded`] last said about it, and a new one under a directory that
    // is already excluded has to be excluded too. Git reports a wholly ignored directory as a
    // single entry, so a file created inside one leaves the exclusion set unchanged and no
    // second `set_excluded` is ever written to mark it — it would otherwise stay in search until
    // something unrelated moved the set. A full walk answers the same way: `walk::scan` orders
    // its files shallowest first, so a directory is always upserted before its children.
    let id: i64 = tx
        .prepare_cached(
            "INSERT INTO files(rel_path, parent_dir, canonical, dev, ino, mtime_ns, size, kind, title, content_hash, git_ignored)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,
                    COALESCE((SELECT git_ignored FROM files WHERE rel_path = ?2), 0))
             ON CONFLICT(rel_path) DO UPDATE SET
                parent_dir=excluded.parent_dir,
                canonical=excluded.canonical, dev=excluded.dev, ino=excluded.ino,
                mtime_ns=excluded.mtime_ns, size=excluded.size, kind=excluded.kind,
                title=excluded.title, content_hash=excluded.content_hash
             RETURNING id",
        )?
        .query_row(
            params![
                f.rel_path,
                path::parent_dir(&f.rel_path),
                f.canonical.to_string_lossy(),
                f.dev as i64,
                f.ino as i64,
                f.mtime_ns,
                f.size as i64,
                f.kind.as_i64(),
                title,
                hash.as_ref().map(|h| h.as_bytes().to_vec()),
            ],
            |r| r.get(0),
        )?;

    if existing_id.is_some() {
        clear_derived(tx, id)?;
        stats.updated += 1;
    } else {
        stats.added += 1;
    }

    // The names a link reaches this file by. A directory is not a link target. Rewritten rather
    // than kept because a row can change kind under the same path.
    tx.prepare_cached("DELETE FROM file_keys WHERE file_id = ?1")?
        .execute([id])?;
    if f.kind != FileKind::Dir {
        for key in markdown::path_keys(&f.rel_path) {
            tx.prepare_cached("INSERT INTO file_keys(file_id, key) VALUES(?1, ?2)")?
                .execute(params![id, key])?;
        }
    }

    if let Some(a) = analysis.as_ref() {
        for l in &a.links {
            // A wikilink names a note from the vault root; a markdown link names it from the
            // note's own directory, so it is turned into a vault path here, where that
            // directory is known, and resolves by the same rules as everything else.
            let target = match l.kind {
                markdown::LinkKind::Markdown => {
                    path::resolve(path::parent_dir(&f.rel_path), &l.target)
                }
                _ => l.target.clone(),
            };
            tx.prepare_cached(
                "INSERT INTO links(src_file, target, key, resolved_file, kind, anchor, alias, byte_start, byte_end)
                 VALUES(?1,?2,?3,NULL,?4,?5,?6,?7,?8)",
            )?
            .execute(params![
                id,
                target,
                markdown::link_key(&target),
                link_kind_i64(l.kind),
                l.anchor,
                l.alias,
                l.range.start as i64,
                l.range.end as i64,
            ])?;
        }
        for t in &a.tags {
            tx.prepare_cached("INSERT INTO tags(file_id, name, byte_start) VALUES(?1,?2,?3)")?
                .execute(params![id, t.name, t.range.start as i64])?;
        }
        for h in &a.headings {
            tx.prepare_cached(
                "INSERT INTO headings(file_id, level, text, byte_start) VALUES(?1,?2,?3,?4)",
            )?
            .execute(params![id, h.level as i64, h.text, h.range.start as i64])?;
        }
    }
    if let Some(body) = text.as_ref() {
        tx.prepare_cached("INSERT INTO notes(file_id, body, title) VALUES(?1,?2,?3)")?
            .execute(params![id, body, title.as_deref().unwrap_or_default()])?;
    }
    Ok(id)
}

/// Everything a file's content produced, so a re-read starts clean — and a file that could not
/// be read keeps no stale body to be found by.
///
/// The `notes` row is deleted here and inserted afresh rather than `REPLACE`d: REPLACE would only
/// fire the FTS delete trigger with `recursive_triggers` on.
fn clear_derived(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<()> {
    tx.prepare_cached("DELETE FROM links WHERE src_file = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM tags WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM headings WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM notes WHERE file_id = ?1")?
        .execute([id])?;
    Ok(())
}

/// Every row of one file, and the links that pointed at it re-resolved: with its keys gone they
/// find the next candidate — a namesake deeper in the vault — or dangle.
pub(super) fn delete_file_rows(tx: &rusqlite::Transaction<'_>, id: i64) -> Result<()> {
    clear_derived(tx, id)?;
    tx.prepare_cached("DELETE FROM aliases WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached("DELETE FROM file_keys WHERE file_id = ?1")?
        .execute([id])?;
    tx.prepare_cached(&format!(
        "UPDATE links SET resolved_file = {BEST_FILE} WHERE resolved_file = ?1"
    ))?
    .execute([id])?;
    tx.prepare_cached("DELETE FROM files WHERE id = ?1")?
        .execute([id])?;
    Ok(())
}

fn link_kind_i64(k: markdown::LinkKind) -> i64 {
    match k {
        markdown::LinkKind::Wiki => 0,
        markdown::LinkKind::Embed => 1,
        markdown::LinkKind::Markdown => 2,
        markdown::LinkKind::External => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::testing::{fixture, open};
    use std::fs;

    #[test]
    fn first_pass_indexes_then_second_pass_is_a_no_op() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        let s1 = ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(s1.added, 5, "{s1:?}"); // 4 files + 1 dir
        assert_eq!(s1.unchanged, 0);
        assert_eq!(s1.conflicts, 1);

        let s2 = ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(s2.added + s2.updated + s2.touched + s2.removed, 0, "{s2:?}");
        assert_eq!(s2.unchanged, 5);
        assert_eq!(s2.bytes_read, 0, "warm pass must not open any file");

        let st = ix.stats().unwrap();
        assert_eq!(st.files, 4);
        assert_eq!(st.notes, 2, "conflicts and pdfs are not searchable notes");
        assert_eq!(st.conflicts, 1);
    }

    /// The `!BUG`: a CRLF note was indexed from its raw bytes while the editor held it as LF, so
    /// every offset drifted by one byte per preceding line.
    #[test]
    fn offsets_in_a_crlf_note_address_the_normalised_buffer() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("crlf.md"),
            "# Title\r\nline\r\n[[Beta]]\r\n",
        )
        .unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let bl = ix.backlinks("sub/Beta.md").unwrap();
        let from_crlf = bl.iter().find(|b| b.src_rel_path == "crlf.md").unwrap();
        assert_eq!(from_crlf.byte_start, "# Title\nline\n".len() as i64);
        let crate::fs::Read::Text(buffer) =
            crate::fs::read_text(&vault.path().join("crlf.md")).unwrap()
        else {
            panic!("a note reads as text");
        };
        assert_eq!(
            &buffer.text[from_crlf.byte_start as usize..from_crlf.byte_end as usize],
            "[[Beta]]"
        );
    }

    /// A markdown link is relative to the note that holds it, so `../a.md` in `sub/` is `a.md`.
    #[test]
    fn a_relative_markdown_link_resolves_from_the_notes_directory() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("sub/Beta.md"),
            "# Beta\n[up](../a.md) [near](Beta.md) [dot](./c.pdf)\n",
        )
        .unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let from_beta = |rel: &str| {
            ix.backlinks(rel)
                .unwrap()
                .iter()
                .filter(|b| b.src_rel_path == "sub/Beta.md")
                .count()
        };
        assert_eq!(from_beta("a.md"), 1);
        assert_eq!(from_beta("sub/Beta.md"), 1, "a plain name is a sibling");
        assert_eq!(
            ix.unresolved_links().unwrap(),
            vec![("sub/Beta.md".to_string(), "sub/c.pdf".to_string())],
            "the dangling target is reported as the vault path it names"
        );
    }

    /// A note that stopped being readable — a NUL byte written into it, say — must not keep
    /// answering searches with the body it used to have.
    #[test]
    fn a_note_that_becomes_unreadable_loses_its_stale_body() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.search("ferris", 10, false).unwrap().len(), 1);

        fs::write(vault.path().join("sub/Beta.md"), b"\0 not text any more").unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert!(ix.search("ferris", 10, false).unwrap().is_empty());
        assert!(
            ix.get_file("sub/Beta.md").unwrap().is_some(),
            "the stat row stays"
        );
    }

    #[test]
    fn touched_but_identical_content_only_updates_stats() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // Rewrite byte-identical content: mtime moves, hash does not.
        let p = vault.path().join("a.md");
        let body = fs::read(&p).unwrap();
        fs::write(&p, &body).unwrap();

        let s = ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(s.touched, 1, "{s:?}");
        assert_eq!(s.updated, 0, "{s:?}");
        assert_eq!(s.unchanged, 4, "{s:?}");
    }

    #[test]
    fn content_change_reindexes_and_deletion_removes() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        fs::write(vault.path().join("a.md"), "# Alpha\ncompletely new text\n").unwrap();
        fs::remove_file(vault.path().join("c.pdf")).unwrap();
        let s = ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(s.updated, 1, "{s:?}");
        assert_eq!(s.removed, 1, "{s:?}");
        assert!(ix.get_file("c.pdf").unwrap().is_none());
        assert!(ix.get_file("a.md").unwrap().is_some());
    }

    #[test]
    fn edited_note_leaves_the_fts_index_consistent() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        fs::write(
            vault.path().join("sub/Beta.md"),
            "# Beta\nnow about crabs\n",
        )
        .unwrap();
        ix.reconcile(vault.path(), |_| {}).unwrap();

        assert!(
            ix.search("ferris", 10, false).unwrap().is_empty(),
            "stale FTS row"
        );
        assert_eq!(ix.search("crabs", 10, false).unwrap().len(), 1);
    }

    #[test]
    fn aliases_are_recorded() {
        let vault = tempfile::tempdir().unwrap();
        fs::write(vault.path().join("a.md"), "x").unwrap();
        std::os::unix::fs::symlink(vault.path().join("a.md"), vault.path().join("b.md")).unwrap();
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.stats().unwrap().aliases, 1);
    }

    /// Every row two indexes must agree on, keyed by path: `ino`, `mtime` and `canonical` are
    /// per-vault by nature, so comparing them across two temp dirs would say nothing.
    fn dump(ix: &Index) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |sql: &str, cols: usize| {
            let mut st = ix.conn.prepare(sql).unwrap();
            let rows = st
                .query_map([], |r| {
                    let mut line = String::new();
                    for i in 0..cols {
                        line.push_str(&format!("{:?}|", r.get_ref(i).unwrap()));
                    }
                    Ok(line)
                })
                .unwrap();
            out.extend(rows.map(|r| r.unwrap()));
        };
        push(
            "SELECT rel_path, parent_dir, kind, title FROM files ORDER BY rel_path",
            4,
        );
        push(
            "SELECT s.rel_path, l.target, t.rel_path, l.kind, l.anchor, l.byte_start, l.byte_end
             FROM links l JOIN files s ON s.id = l.src_file
             LEFT JOIN files t ON t.id = l.resolved_file
             ORDER BY s.rel_path, l.byte_start",
            7,
        );
        push(
            "SELECT f.rel_path, g.name, g.byte_start FROM tags g JOIN files f ON f.id = g.file_id
             ORDER BY 1, 3",
            3,
        );
        push(
            "SELECT f.rel_path, h.level, h.text, h.byte_start FROM headings h
             JOIN files f ON f.id = h.file_id ORDER BY 1, 4",
            4,
        );
        out
    }

    #[test]
    fn update_file_adds_note_and_resolves_incoming_links() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("a.md"), "# Alpha\nsee [[Gamma]]\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.unresolved_links().unwrap().len(), 1);

        fs::write(vault.path().join("Gamma.md"), "# Gamma\n").unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "Gamma.md").unwrap(),
            Change::Added(FileKind::Markdown)
        );
        assert!(
            ix.unresolved_links().unwrap().is_empty(),
            "the new note must resolve the link that was waiting for it"
        );
        assert_eq!(ix.backlinks("Gamma.md").unwrap().len(), 1);
        assert_eq!(
            ix.get_file("Gamma.md").unwrap().unwrap().title.unwrap(),
            "Gamma"
        );
    }

    /// The worker indexes a whole batch and resolves once at the end: resolution is a
    /// whole-vault pass, so paying for it per file turns a Syncthing pull into seconds of work.
    #[test]
    fn batched_update_leaves_link_resolution_to_the_caller() {
        let (vault, db) = fixture();
        fs::write(vault.path().join("a.md"), "# Alpha\nsee [[Gamma]]\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.unresolved_links().unwrap().len(), 1);

        fs::write(vault.path().join("Gamma.md"), "# Gamma\n").unwrap();
        assert_eq!(
            ix.update_file_batched(vault.path(), "Gamma.md").unwrap(),
            Change::Added(FileKind::Markdown)
        );
        assert_eq!(
            ix.unresolved_links().unwrap().len(),
            1,
            "the batched variant must not resolve on its own"
        );

        ix.resolve_links().unwrap();
        assert!(ix.unresolved_links().unwrap().is_empty());
        assert_eq!(ix.backlinks("Gamma.md").unwrap().len(), 1);
    }

    #[test]
    fn update_file_unchanged_when_stat_matches() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        // A hand-edited title only survives if `update_file` wrote nothing at all.
        ix.conn
            .execute(
                "UPDATE files SET title = 'sentinel' WHERE rel_path = 'a.md'",
                [],
            )
            .unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "a.md").unwrap(),
            Change::Unchanged
        );
        assert_eq!(
            ix.get_file("a.md").unwrap().unwrap().title.as_deref(),
            Some("sentinel")
        );
    }

    #[test]
    fn update_file_keeps_fts_consistent() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        fs::write(
            vault.path().join("sub/Beta.md"),
            "# Beta\nnow about crabs\n",
        )
        .unwrap();
        assert_eq!(
            ix.update_file(vault.path(), "sub/Beta.md").unwrap(),
            Change::Updated(FileKind::Markdown)
        );
        assert!(
            ix.search("ferris", 10, false).unwrap().is_empty(),
            "stale FTS row"
        );
        assert_eq!(ix.search("crabs", 10, false).unwrap().len(), 1);
    }

    #[test]
    fn update_file_ignores_temp_and_hard_skipped_names() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        fs::create_dir(vault.path().join(".git")).unwrap();
        fs::write(vault.path().join(".git/config"), "c").unwrap();
        fs::write(vault.path().join(".syncthing.n.md.tmp"), "t").unwrap();
        fs::write(vault.path().join(".accent-xyz"), "t").unwrap();

        for rel in [".git/config", ".syncthing.n.md.tmp", ".accent-xyz"] {
            assert_eq!(
                ix.update_file(vault.path(), rel).unwrap(),
                Change::Ignored,
                "{rel}"
            );
            assert!(ix.get_file(rel).unwrap().is_none(), "{rel}");
        }
    }

    #[test]
    fn remove_file_drops_prefix_and_unresolves_links() {
        let (vault, db) = fixture();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();
        assert_eq!(ix.backlinks("sub/Beta.md").unwrap().len(), 1);

        // One event for the directory has to take the note under it with it.
        assert_eq!(ix.remove_file("sub").unwrap(), 2);
        assert!(ix.get_file("sub").unwrap().is_none());
        assert!(ix.get_file("sub/Beta.md").unwrap().is_none());
        assert!(ix.search("ferris", 10, false).unwrap().is_empty());
        assert_eq!(
            ix.unresolved_links().unwrap(),
            vec![("a.md".to_string(), "Beta".to_string())]
        );
        assert_eq!(
            ix.remove_file("sub").unwrap(),
            0,
            "removing twice is a no-op"
        );
    }

    /// The incremental path must produce the rows a full reconcile would, or the index slowly
    /// drifts away from the vault between restarts.
    #[test]
    fn reconcile_and_update_file_yield_identical_rows() {
        let (whole, db_whole) = fixture();
        let mut ix_whole = open(&db_whole);
        ix_whole.reconcile(whole.path(), |_| {}).unwrap();

        let (partial, db_partial) = fixture();
        let note = partial.path().join("sub/Beta.md");
        let body = fs::read_to_string(&note).unwrap();
        fs::remove_file(&note).unwrap();
        let mut ix_partial = open(&db_partial);
        ix_partial.reconcile(partial.path(), |_| {}).unwrap();

        fs::write(&note, &body).unwrap();
        assert_eq!(
            ix_partial
                .update_file(partial.path(), "sub/Beta.md")
                .unwrap(),
            Change::Added(FileKind::Markdown)
        );
        assert_eq!(dump(&ix_partial), dump(&ix_whole));
    }

    /// A text file that is not a note is searchable, a huge or binary one is not, and none of
    /// them is analysed as markdown.
    #[test]
    fn non_markdown_text_is_indexed_within_the_cap() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("tool.py"),
            "# zorblat helper\nimport os  # not #atag\n",
        )
        .unwrap();
        fs::write(vault.path().join("big.txt"), "zorblat\n".repeat(300_000)).unwrap();
        fs::write(vault.path().join("bin.dat"), b"\0zorblat\n").unwrap();
        fs::write(
            vault.path().join("flow.drawio"),
            r#"<mxfile><diagram><mxGraphModel><root><mxCell id="0" value="zorblat"/></root></mxGraphModel></diagram></mxfile>"#,
        )
        .unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        assert!(vault.path().join("big.txt").metadata().unwrap().len() > MAX_INDEXED_BODY);

        // Both query paths reach the source file with no change of their own.
        let hits = ix.search("zorblat", 10, false).unwrap();
        assert_eq!(
            hits.iter().map(|h| h.rel_path.as_str()).collect::<Vec<_>>(),
            ["tool.py"],
            "{hits:?}"
        );
        let re = crate::search::pattern("zorblat", crate::search::Options::default()).unwrap();
        let (matches, total) = ix.grep(&re, 10, false).unwrap();
        assert_eq!(matches[0].rel_path, "tool.py");
        // Listed, not counted: the count is what a Replace All would rewrite, and it rewrites
        // notes.
        assert_eq!(total, 0);

        // Over the cap and binary: a stat row each, and nothing to match against.
        let body_count = |rel: &str| -> i64 {
            ix.conn
                .query_row(
                    "SELECT COUNT(*) FROM notes n JOIN files f ON f.id = n.file_id
                     WHERE f.rel_path = ?1",
                    [rel],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(body_count("tool.py"), 1);
        assert_eq!(body_count("big.txt"), 0, "over MAX_INDEXED_BODY");
        assert_eq!(body_count("bin.dat"), 0, "a NUL byte is not text");
        assert_eq!(
            body_count("flow.drawio"),
            0,
            "a diagram's XML is not searched"
        );
        assert!(
            ix.get_file("big.txt").unwrap().is_some(),
            "still a file row"
        );
        assert!(
            ix.get_file("bin.dat").unwrap().is_some(),
            "still a file row"
        );

        // The markdown analyser never sees it: `#` is a comment, not a tag or a heading.
        let headings: i64 = ix
            .conn
            .query_row(
                "SELECT COUNT(*) FROM headings h JOIN files f ON f.id = h.file_id
                 WHERE f.rel_path = 'tool.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(headings, 0);
        assert!(
            !ix.tags().unwrap().iter().any(|(t, _)| t == "atag"),
            "{:?}",
            ix.tags().unwrap()
        );
        assert_eq!(
            ix.get_file("tool.py").unwrap().unwrap().title.as_deref(),
            Some("tool"),
            "the file stem is the title fallback"
        );

        // Replace All stays notes-only: `a` is in all three bodies, only the notes come back.
        let re = crate::search::pattern("a", crate::search::Options::default()).unwrap();
        assert_eq!(ix.grep_paths(&re).unwrap(), vec!["a.md", "sub/Beta.md"]);
    }
}
