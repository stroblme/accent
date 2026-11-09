//! The `reconcile` module's tests.

use super::*;
use crate::index::testing::{fixture, open};
use std::fs;

/// Stop is a pause: what was written stays, and the next reconcile is a diff that indexes
/// only what is left. More than one batch of notes, so there is a second batch to cut.
#[test]
fn a_stop_mid_walk_keeps_its_rows_and_the_next_reconcile_finishes_them() {
    let vault = tempfile::tempdir().unwrap();
    let notes = BATCH + 100;
    for i in 0..notes {
        fs::write(
            vault.path().join(format!("n{i}.md")),
            format!("# Note {i}\n"),
        )
        .unwrap();
    }
    let db = tempfile::tempdir().unwrap();
    let mut ix = open(&db);

    // Stop once the first batch has been committed and reported.
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let stats = ix
        .reconcile_with(
            vault.path(),
            "",
            &ScanOptions::default(),
            &|| stopped.load(std::sync::atomic::Ordering::Relaxed),
            |_, p| {
                if p.phase == Phase::Index {
                    stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
        )
        .unwrap();
    assert!(stats.stopped, "the walk reports that it was stopped");
    assert_eq!(stats.added, BATCH, "one batch written, the rest left");
    assert_eq!(ix.file_paths(false).unwrap().len(), BATCH);
    // A batch's bodies are written together at its end, a stopped one's too.
    assert_eq!(ix.stats().unwrap().notes, BATCH as i64);
    assert_eq!(ix.search("Note", 1, false).unwrap().len(), 1);

    // Nothing said the index is partial except the flag, so the next open just reconciles.
    let stats = ix.reconcile(vault.path(), |_| {}).unwrap();
    assert!(!stats.stopped);
    assert_eq!(stats.added, notes - BATCH, "only the remainder is indexed");
    assert_eq!(
        stats.unchanged, BATCH,
        "what was written is not written again"
    );
    assert_eq!(ix.file_paths(false).unwrap().len(), notes);
}

/// A first index writes without syncing, on its own connection alone, and that connection
/// syncs as before once the walk ends, a stopped one too.
#[test]
fn a_first_index_syncs_as_usual_again_once_it_ends() {
    let vault = tempfile::tempdir().unwrap();
    for i in 0..BATCH + 10 {
        fs::write(vault.path().join(format!("n{i}.md")), "# Note\n").unwrap();
    }
    let db = tempfile::tempdir().unwrap();
    let (mut ix, other) = (open(&db), open(&db));
    let sync = |ix: &Index| -> i64 {
        ix.conn
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .unwrap()
    };
    let marker = || crate::index::unsynced_marker(&db.path().join("i.db")).exists();
    let (off, normal) = (0, 1);
    let mut during = Vec::new();
    let stopped = std::sync::atomic::AtomicBool::new(false);
    ix.reconcile_with(
        vault.path(),
        "",
        &ScanOptions::default(),
        &|| stopped.load(std::sync::atomic::Ordering::Relaxed),
        |ix, p| {
            if p.phase == Phase::Index {
                during.push((sync(ix), sync(&other), marker()));
                stopped.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
    )
    .unwrap();
    during.dedup();
    assert_eq!(during, [(off, normal, true)]);
    assert_eq!(sync(&ix), normal, "after a stopped first index");
    assert!(!marker());

    stopped.store(false, std::sync::atomic::Ordering::Relaxed);
    during.clear();
    ix.reconcile_with(
        vault.path(),
        "",
        &ScanOptions::default(),
        &|| false,
        |ix, p| {
            if p.phase == Phase::Index {
                during.push((sync(ix), sync(&other), marker()));
            }
        },
    )
    .unwrap();
    assert_eq!(
        during,
        [(normal, normal, false)],
        "the resume is no first index"
    );
    assert_eq!(sync(&ix), normal);
}

/// Files are read on a pool, in whatever order its threads finish them, and written as the
/// walk found them: each row holds its own file's content, and the ids follow the scan, so a
/// folder's row is there before its children's.
#[test]
fn files_read_on_the_pool_are_written_in_scan_order() {
    let vault = tempfile::tempdir().unwrap();
    for i in 0..BATCH + 100 {
        let dir = vault.path().join(format!("d{}", i % 7));
        fs::create_dir_all(&dir).unwrap();
        // Sizes apart by orders of magnitude, so the reads finish out of order.
        let pad = "x ".repeat(if i % 50 == 0 { 20_000 } else { i % 10 });
        fs::write(dir.join(format!("n{i}.md")), format!("# Note {i}\n{pad}\n")).unwrap();
    }
    let db = tempfile::tempdir().unwrap();
    let mut ix = open(&db);
    ix.reconcile(vault.path(), |_| {}).unwrap();

    let scanned: Vec<String> = walk::scan(vault.path(), &ScanOptions::default())
        .files
        .into_iter()
        .map(|f| f.rel_path)
        .collect();
    let rows: Vec<(String, Option<String>, Option<String>)> = ix
        .conn
        .prepare(
            "SELECT f.rel_path, f.title, n.body FROM files f
                 LEFT JOIN notes n ON n.file_id = f.id ORDER BY f.id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        scanned
    );
    for (rel, title, body) in rows.iter().filter(|r| r.0.ends_with(".md")) {
        let i = &rel[rel.find("/n").unwrap() + 2..rel.len() - 3];
        assert_eq!(title.as_deref(), Some(format!("Note {i}").as_str()));
        assert!(body.as_ref().unwrap().starts_with(&format!("# Note {i}\n")));
    }
}

/// Near-limit files share one allowance from their read until they are consumed, however
/// many threads run, while small files between them still run side by side. The results
/// still come in the order of the items, and a consumer that stops early is not kept waiting.
#[test]
fn files_being_read_or_waiting_stay_within_the_allowance() {
    use std::sync::atomic::AtomicU64;
    let sizes: Vec<u64> = (0..300)
        .map(|i| match i % 3 {
            0 => crate::fs::MAX_TEXT,
            _ => 1024,
        })
        .collect();
    let (held, peak) = (AtomicU64::new(0), AtomicU64::new(0));
    let (working, side_by_side) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let work = |&size: &u64| {
        peak.fetch_max(
            held.fetch_add(size, Ordering::SeqCst) + size,
            Ordering::SeqCst,
        );
        side_by_side.fetch_max(working.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(1));
        working.fetch_sub(1, Ordering::SeqCst);
        size
    };
    // A consumer slower than the pool, which a queue bounded by count alone would fill with
    // near-limit files.
    let got: Vec<u64> = in_order(
        &sizes,
        |&size| size,
        work,
        |results| {
            results
                .inspect(|&size| {
                    std::thread::sleep(Duration::from_millis(1));
                    held.fetch_sub(size, Ordering::SeqCst);
                })
                .collect()
        },
    );
    assert_eq!(got, sizes, "in the order of the items");
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak <= ALLOWANCE, "{} MiB held", peak >> 20);
    if std::thread::available_parallelism().map_or(1, |n| n.get()) > 1 {
        assert!(
            side_by_side.load(Ordering::SeqCst) > 1,
            "small files ran alone"
        );
    }

    let first: Vec<u64> = in_order(
        &sizes,
        |&size| size,
        |&size| size,
        |results| results.take(5).collect(),
    );
    assert_eq!(first, sizes[..5]);
}

/// Two processes share one index (the app and `accent-cli mcp`), so a row this walk found
/// missing may be written by the other before this walk's batch reaches it. It is then
/// rewritten whole, never added a second time over what is there.
#[test]
fn a_row_another_connection_wrote_mid_walk_is_not_written_twice() {
    let vault = tempfile::tempdir().unwrap();
    let notes = BATCH + 10;
    for i in 0..notes {
        fs::write(
            vault.path().join(format!("n{i}.md")),
            format!("# Note {i}\nsee [[Target]] #tag\n"),
        )
        .unwrap();
    }
    let db = tempfile::tempdir().unwrap();
    let mut ix = open(&db);
    let mut other = Some(open(&db));
    ix.reconcile_with(
        vault.path(),
        "",
        &ScanOptions::default(),
        &|| false,
        |_, p| {
            // Between the first batch and the second: the other connection indexes it all.
            if p.phase == Phase::Index
                && let Some(mut other) = other.take()
            {
                other.reconcile(vault.path(), |_| {}).unwrap();
            }
        },
    )
    .unwrap();
    let stats = ix.stats().unwrap();
    assert_eq!(stats.notes, notes as i64);
    assert_eq!(stats.links, notes as i64, "one link per note, not two");
    let tagged: i64 = ix
        .conn
        .query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tagged, notes as i64);
}

/// A walk of one folder brings that folder in line with the disk and leaves the rest of the
/// index as it was, under the rules the whole walk applies there: a `.gitignore` above the
/// folder, and a folder linked in elsewhere, which a link inside it does not bring in twice.
#[test]
fn a_walk_of_one_folder_changes_that_folder_alone() {
    let (vault, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let write = |rel: &str| {
        let path = vault.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "# note\n").unwrap();
    };
    fs::write(vault.path().join(".gitignore"), "sub/build/\n").unwrap();
    fs::write(outside.path().join("ext.md"), "# ext\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), vault.path().join("linked")).unwrap();
    write("sub/gone.md");
    write("other.md");
    let db = tempfile::tempdir().unwrap();
    let mut ix = open(&db);
    ix.reconcile(vault.path(), |_| {}).unwrap();

    write("sub/new.md");
    write("sub/build/out.md");
    write("later.md");
    fs::remove_file(vault.path().join("sub/gone.md")).unwrap();
    std::os::unix::fs::symlink(outside.path(), vault.path().join("sub/again")).unwrap();
    let stats = ix
        .reconcile_with(
            vault.path(),
            "sub",
            &ScanOptions::default(),
            &|| false,
            |_, _| {},
        )
        .unwrap();

    let paths: Vec<String> = ix
        .conn
        .prepare("SELECT rel_path FROM files ORDER BY rel_path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        paths,
        [
            ".gitignore",
            "linked",
            "linked/ext.md",
            "other.md",
            "sub",
            "sub/new.md"
        ]
    );
    assert_eq!((stats.added, stats.removed), (1, 1), "{stats:?}");
    assert_eq!(stats.dir, "sub", "the walk says which folder it was");
    assert_eq!(ix.stats().unwrap().aliases, 0, "the second link was walked");
}

/// A walk over an index that is already there resolves each batch's links as it commits it,
/// so no reader between two batches sees a changed note link to nothing; a link to a note a
/// later batch adds still resolves when that note arrives.
#[test]
fn a_warm_walk_resolves_each_batch_as_it_commits_it() {
    let vault = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| fs::write(vault.path().join(rel), text).unwrap();
    write("old.md", "old\n");
    let db = tempfile::tempdir().unwrap();
    let mut ix = open(&db);
    ix.reconcile(vault.path(), |_| {}).unwrap();

    // Sorted by name, `a.md` opens the first batch and `z.md` closes the second.
    write("a.md", "[[old]] [[z]]\n");
    for i in 0..BATCH {
        write(&format!("n{i:04}.md"), "n\n");
    }
    write("z.md", "z\n");
    let mut between = None;
    ix.reconcile_with(
        vault.path(),
        "",
        &ScanOptions::default(),
        &|| false,
        |ix, p| {
            if p.phase == Phase::Index && between.is_none() {
                between = Some(ix.backlinks("old.md").unwrap().len());
            }
        },
    )
    .unwrap();

    assert_eq!(
        between,
        Some(1),
        "a.md's link was unresolved between batches"
    );
    assert_eq!(ix.backlinks("z.md").unwrap().len(), 1);
}

/// A scan cut short is short of files the vault holds, and the diff would read every one of
/// them as a deletion. It is dropped whole instead, so a stop can never empty the index.
#[test]
fn a_stop_during_the_scan_writes_nothing_and_deletes_nothing() {
    let (vault, db) = fixture();
    let mut ix = open(&db);
    ix.reconcile(vault.path(), |_| {}).unwrap();
    let before = ix.file_paths(false).unwrap().len();
    assert!(before > 0);

    let stats = ix
        .reconcile_with(
            vault.path(),
            "",
            &ScanOptions::default(),
            &|| true,
            |_, p| assert_eq!(p.total, 0, "a stopped scan reports no more than its count"),
        )
        .unwrap();
    assert!(stats.stopped);
    assert_eq!(
        stats.removed, 0,
        "nothing the walk never reached is deleted"
    );
    assert_eq!(ix.file_paths(false).unwrap().len(), before);
}

/// A large vault over slow storage takes seconds to scan, and the scan writes nothing, so the
/// count is all there is to show meanwhile. The walk is held on its first entry until a count
/// has been reported, which makes it as slow as it needs to be without guessing at a timing.
#[test]
fn a_scan_still_running_says_how_many_files_it_has_found() {
    use std::sync::atomic::AtomicBool;

    let (vault, db) = fixture();
    let mut ix = open(&db);
    let counted = AtomicBool::new(false);
    let mut scans = Vec::new();
    ix.reconcile_with(
        vault.path(),
        "",
        &ScanOptions::default(),
        &|| {
            // Bounded, so a scan that never reports fails the test rather than hanging it.
            let until = Instant::now() + Duration::from_secs(5);
            while !counted.load(Ordering::Relaxed) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
            }
            false
        },
        |_, p| {
            if p.phase == Phase::Scan {
                counted.store(true, Ordering::Relaxed);
                scans.push(p);
            }
        },
    )
    .unwrap();

    assert!(
        scans.len() >= 2,
        "a count while scanning, then the total: {scans:?}"
    );
    assert_eq!(
        scans[0].total, 0,
        "no total while the walk is still running"
    );
    let last = scans.last().unwrap();
    assert_eq!(
        (last.done, last.total),
        (5, 5),
        "4 files + 1 dir, once it has ended"
    );
    assert!(scans[0].done <= last.done);
}

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

/// Indexed CRLF notes must use editor-normalised LF text so byte offsets agree.
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
        vec![("sub/Beta.md".to_string(), "sub/c.pdf".to_string(), 37)],
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
    fs::remove_file(vault.path().join("sub/Beta.md")).unwrap();
    let s = ix.reconcile(vault.path(), |_| {}).unwrap();
    assert_eq!(s.updated, 2, "a.md, and the folder Beta.md left: {s:?}");
    assert_eq!(s.removed, 2, "{s:?}");
    assert!(ix.get_file("c.pdf").unwrap().is_none());
    assert!(ix.get_file("a.md").unwrap().is_some());
    // The removed note's body is gone from both FTS tables, whole word and mid-word.
    assert!(ix.search("ferris", 10, false).unwrap().is_empty());
    assert!(ix.search("erri", 10, false).unwrap().is_empty());
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
    assert_eq!(ix.remove_file_batched("sub").unwrap(), 2);
    assert!(ix.get_file("sub").unwrap().is_none());
    assert!(ix.get_file("sub/Beta.md").unwrap().is_none());
    assert!(ix.search("ferris", 10, false).unwrap().is_empty());
    assert_eq!(
        ix.unresolved_links().unwrap(),
        vec![("a.md".to_string(), "Beta".to_string(), 12)]
    );
    assert_eq!(
        ix.remove_file_batched("sub").unwrap(),
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
    // Named as a binary format, so never opened: text or not, it is not read.
    fs::write(vault.path().join("pic.png"), "zorblat\n").unwrap();
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
    // Counted too: the count is what a Replace All would rewrite, and it rewrites every body.
    assert_eq!(total, 1);

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
    assert_eq!(body_count("pic.png"), 0, "a picture is not read");
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

    // Replace All reaches the source file as well: `a` is in all three bodies.
    let re = crate::search::pattern("a", crate::search::Options::default()).unwrap();
    assert_eq!(
        ix.grep_paths(&re, false).unwrap(),
        vec!["a.md", "sub/Beta.md", "tool.py"]
    );
}

/// A diagram is found by what its labels say, a line each, and never by its XML, whether its
/// name says it is one or only its first element does.
#[test]
fn a_diagrams_labels_are_indexed_and_its_xml_is_not() {
    for name in ["flow.drawio", "flow.xml"] {
        diagram_indexed_as_labels(name);
    }
    let (vault, db) = fixture();
    fs::write(
        vault.path().join("plain.xml"),
        r#"<svg><g id="zorblat"/></svg>"#,
    )
    .unwrap();
    let mut ix = open(&db);
    ix.reconcile(vault.path(), |_| {}).unwrap();
    let re = crate::search::pattern("zorblat", crate::search::Options::default()).unwrap();
    assert_eq!(ix.grep(&re, 10, false).unwrap().1, 1, "other XML is text");
}

/// An `.ipynb` is found by its cells' sources, never by its outputs or its JSON, while a
/// marimo notebook is the Python it is.
#[test]
fn a_notebooks_cells_are_indexed_and_its_json_is_not() {
    let (vault, db) = fixture();
    fs::write(
        vault.path().join("n.ipynb"),
        r#"{"cells": [
              {"cell_type": "code", "execution_count": 1, "metadata": {}, "source": ["print(1)"],
               "outputs": [{"name": "stdout", "output_type": "stream", "text": ["quux\n"]}]},
              {"cell_type": "markdown", "metadata": {}, "source": ["zorblat tea"]}
            ], "metadata": {"kernelspec": {"name": "python3"}}, "nbformat": 4, "nbformat_minor": 4}"#,
    )
    .unwrap();
    fs::write(
        vault.path().join("m.py"),
        "import marimo\napp = marimo.App()\n\n\n@app.cell\ndef _():\n    x = 1\n    return\n",
    )
    .unwrap();
    let mut ix = open(&db);
    ix.reconcile(vault.path(), |_| {}).unwrap();

    let hits = ix.search("zorblat", 10, false).unwrap();
    assert_eq!(hits[0].rel_path, "n.ipynb");
    assert_eq!(hits[0].line, Some(2), "the second cell's line");
    let count = |pattern: &str| {
        let re = crate::search::pattern(pattern, crate::search::Options::default()).unwrap();
        ix.grep(&re, 10, false).unwrap().1
    };
    assert_eq!(count("quux|kernelspec|cell_type"), 0, "no output, no JSON");
    assert_eq!(count("@app.cell"), 1, "marimo's own lines");
}

/// A file the walk leaves out joins the index once opened, stays through a walk and an edit,
/// leaves with its file, and only the last [`OPENED_CAP`] stay.
#[test]
fn an_opened_file_the_walk_leaves_out_is_indexed_until_the_queue_drops_it() {
    let vault = tempfile::tempdir().unwrap();
    let at = |rel: &str| vault.path().join(rel);
    fs::write(at(".gitignore"), "build/\n").unwrap();
    fs::create_dir(at("build")).unwrap();
    fs::write(at("build/out.md"), "zorblat\n").unwrap();
    let db = tempfile::tempdir().unwrap();
    let mut ix = open(&db);
    let found = |ix: &Index, query: &str| -> Vec<String> {
        let hits = ix.search(query, 10, false).unwrap();
        hits.into_iter().map(|h| h.rel_path).collect()
    };
    ix.reconcile(vault.path(), |_| {}).unwrap();
    assert!(found(&ix, "zorblat").is_empty());

    ix.keep_opened(vault.path(), "build/out.md").unwrap();
    assert_eq!(found(&ix, "zorblat"), ["build/out.md"]);
    ix.reconcile(vault.path(), |_| {}).unwrap();
    assert_eq!(found(&ix, "zorblat"), ["build/out.md"], "a walk keeps it");

    fs::write(at("build/out.md"), "quux\n").unwrap();
    ix.update_file(vault.path(), "build/out.md").unwrap();
    assert_eq!(found(&ix, "quux"), ["build/out.md"], "an edit reaches it");

    fs::remove_file(at("build/out.md")).unwrap();
    ix.reconcile(vault.path(), |_| {}).unwrap();
    assert!(found(&ix, "quux").is_empty());
    assert!(!ix.is_opened("build/out.md").unwrap(), "its slot is free");

    for i in 0..=OPENED_CAP {
        fs::write(at(&format!("build/n{i}.md")), "n").unwrap();
        ix.keep_opened(vault.path(), &format!("build/n{i}.md"))
            .unwrap();
    }
    assert!(
        ix.get_file("build/n0.md").unwrap().is_none(),
        "the oldest went"
    );
    assert!(ix.get_file("build/n1.md").unwrap().is_some());
}

fn diagram_indexed_as_labels(name: &str) {
    let (vault, db) = fixture();
    fs::write(
        vault.path().join(name),
        r#"<mxfile><diagram name="P"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="k" value="Kettle" style="fillColor=#zorblat;" vertex="1" parent="1"><mxGeometry as="geometry"/></mxCell>
            <mxCell id="t" value="&lt;b&gt;Hot&lt;/b&gt; tea" style="html=1;" vertex="1" parent="1"><mxGeometry as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#,
    )
    .unwrap();
    let mut ix = open(&db);
    ix.reconcile(vault.path(), |_| {}).unwrap();

    let hits = ix.search("hot tea", 10, false).unwrap();
    assert_eq!(hits[0].rel_path, name);
    assert_eq!(hits[0].line, Some(2), "the second label's line");
    let re = crate::search::pattern("zorblat|<b>", crate::search::Options::default()).unwrap();
    assert_eq!(
        ix.grep(&re, 10, false).unwrap().1,
        0,
        "{name}: no style key, no markup"
    );
}
