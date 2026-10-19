//! The `worker` module's tests.

use super::{Msg, RUN, spawn};
use crate::local::Ignored;
use crate::tests::*;
use crate::{Etag, Event, VaultConfig, fs};
use accent_core::index::Index;
use accent_core::watch::VaultEvent;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// Verify pause retains rows and ignores other scan triggers until resume
/// finishes indexing. Enough files keep the walk active when `open` returns.
#[test]
fn a_stopped_vault_keeps_its_index_and_waits_to_be_resumed() {
    let root = tempfile::tempdir().unwrap();
    let notes = 1000;
    for i in 0..notes {
        std::fs::write(root.path().join(format!("n{i}.md")), "body").unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let (vault, events) = crate::Vault::open_at(
        root.path(),
        &cache.path().join("index.db"),
        VaultConfig::default(),
    )
    .unwrap();
    vault.stop_indexing().unwrap();

    let first = wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET);
    let Some(Event::Reconciled(stats)) = first else {
        panic!("no reconcile: {first:?}");
    };
    assert!(stats.stopped, "the first walk was stopped: {stats:?}");
    let partial = vault.file_paths(false).unwrap().len();
    assert!(partial < notes, "a partial index, {partial} of {notes}");

    // A rescan is what a folder moved in, an Android resume and a reconnect all come down to.
    // None of them may restart the walk the user has just stopped.
    vault.rescan().unwrap();
    assert!(
        wait_for(
            &events,
            |e| matches!(e, Event::Reconciled(_)),
            Duration::from_millis(500),
        )
        .is_none(),
        "a paused vault walked again on a rescan"
    );

    vault.resume_indexing().unwrap();
    let done = wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET);
    let Some(Event::Reconciled(stats)) = done else {
        panic!("no reconcile after resume: {done:?}");
    };
    assert!(!stats.stopped);
    assert_eq!(vault.file_paths(false).unwrap().len(), notes);
}

/// Closing a vault mid-walk stops the walk rather than waiting for it, and the next open
/// carries on by itself: a close is no pause.
#[test]
fn closing_a_vault_mid_walk_stops_the_walk_and_the_next_open_finishes_it() {
    let root = tempfile::tempdir().unwrap();
    let notes = 1000;
    for i in 0..notes {
        std::fs::write(root.path().join(format!("n{i}.md")), "body").unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("index.db");
    let open = || crate::Vault::open_at(root.path(), &db, VaultConfig::default()).unwrap();

    drop(open());
    let partial = Index::open(&db).unwrap().file_paths(false).unwrap().len();
    assert!(partial < notes, "the close waited for the walk");

    let (vault, events) = open();
    let done = wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET);
    let Some(Event::Reconciled(stats)) = done else {
        panic!("no reconcile after the reopen: {done:?}");
    };
    assert!(!stats.stopped);
    assert_eq!(vault.file_paths(false).unwrap().len(), notes);
}

/// Verify scans report edits before watcher echoes. Without watching,
/// only the scan can notify the open tab.
#[test]
fn a_file_a_walk_finds_changed_is_reported_as_changed() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Note.md"), "old\n").unwrap();
    std::fs::write(root.path().join("Other.md"), "same\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (vault, events) = crate::Vault::open_unwatched_at(
        root.path(),
        &cache.path().join("index.db"),
        VaultConfig::default(),
    )
    .unwrap();
    assert!(wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());

    std::fs::write(root.path().join("Note.md"), "new text\n").unwrap();
    vault.rescan().unwrap();
    let mut changed = Vec::new();
    loop {
        match events.recv_timeout(BUDGET).expect("no reconcile") {
            Event::FileChanged(rel) => changed.push(rel),
            Event::Reconciled(_) => break,
            _ => {}
        }
    }
    assert_eq!(changed, ["Note.md"]);
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

/// A note that is a symlink to a file in no watched folder hears of its target being rewritten,
/// in place or by a rename over it: its target's folder is watched for it. Depends on real
/// inotify events.
#[test]
fn a_symlinked_note_hears_of_its_target() {
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("real.md");
    std::fs::write(&target, "old\n").unwrap();
    let root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(&target, root.path().join("linked.md")).unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());
    let linked = |e: &Event| matches!(e, Event::FileChanged(p) if p == "linked.md");

    std::fs::write(&target, "in place\n").unwrap();
    assert!(
        f.wait(linked).is_some(),
        "a write to the target went unheard"
    );

    let temp = outside.path().join(".real.md.tmp");
    std::fs::write(&temp, "renamed over\n").unwrap();
    std::fs::rename(&temp, &target).unwrap();
    assert!(
        f.wait(linked).is_some(),
        "a rename over the target went unheard"
    );
}

/// A second path to a note, an alias the walk keeps rather than a row, hears of the note being
/// written: the watcher's news names the row alone. Depends on real inotify events.
#[test]
fn an_alias_hears_of_its_file() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("note.md"), "old\n").unwrap();
    std::fs::create_dir(root.path().join("d")).unwrap();
    std::os::unix::fs::symlink("../note.md", root.path().join("d/link.md")).unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());

    f.write("note.md", "new\n");
    assert!(
        f.wait(|e| matches!(e, Event::FileChanged(p) if p == "d/link.md"))
            .is_some(),
        "the alias heard nothing"
    );
    assert!(
        f.vault
            .file_paths(false)
            .unwrap()
            .iter()
            .all(|p| p != "d/link.md"),
        "the alias became a row"
    );
}

/// News naming an alias is its row's: a hard link written through its own name and a save
/// through a symlinked alias update the row, which hears of it, and neither alias becomes a row.
/// Depends on real inotify events.
#[test]
fn news_naming_an_alias_is_its_rows() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("note.md"), "old\n").unwrap();
    std::fs::create_dir(root.path().join("d")).unwrap();
    std::fs::hard_link(root.path().join("note.md"), root.path().join("d/hard.md")).unwrap();
    std::os::unix::fs::symlink("../note.md", root.path().join("d/link.md")).unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());

    f.write("d/hard.md", "quokka census\n");
    assert!(
        f.wait(|e| matches!(e, Event::FileChanged(p) if p == "note.md"))
            .is_some(),
        "the row heard nothing"
    );
    let (_, etag) = f.vault.read("d/link.md").unwrap();
    f.vault
        .save("d/link.md", "wombat census\n", Some(etag))
        .unwrap();
    assert!(poll_until(
        || !f.vault.search("wombat", 10, false).unwrap().is_empty(),
        BUDGET
    ));
    let rows = f.vault.file_paths(false).unwrap();
    assert!(
        !rows.iter().any(|p| p.starts_with("d/")),
        "an alias became a row: {rows:?}"
    );
}

/// Go to File's ignored notes are walked for in the folders the walk found, and a folder git
/// ignores made since joins them. Depends on real inotify events.
#[test]
fn an_ignored_folder_made_since_the_walk_is_listed() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".gitignore"), "build/\nout/\n").unwrap();
    std::fs::create_dir(root.path().join("build")).unwrap();
    std::fs::write(root.path().join("build/a.md"), "a\n").unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());
    assert_eq!(f.vault.ignored_notes(true).unwrap(), ["build/a.md"]);

    f.write("out/b.md", "b\n");
    assert!(poll_until(
        || f.vault.ignored_notes(true).unwrap() == ["build/a.md", "out/b.md"],
        BUDGET
    ));
}

/// Verify watches include subdirectories already present at vault open,
/// so external note edits reach the UI.
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

/// Verify recreating a folder renews its inode watch, even at the same path.
#[test]
fn a_folder_removed_and_made_again_is_watched_again() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(root.path().join("sub/a.md"), "a").unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());
    let sub = f.vault.root().join("sub");

    // Each step waits for the batch before it to end, so the three land in three batches.
    std::fs::remove_dir_all(&sub).unwrap();
    assert!(
        f.wait(|e| matches!(e, Event::FileRemoved(p) if p == "sub"))
            .is_some()
    );
    assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());
    std::fs::create_dir(&sub).unwrap();
    assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());
    f.write("sub/b.md", "b");

    assert!(
        f.wait(|e| matches!(e, Event::DirsChanged(d) if d.iter().any(|d| d == "sub")))
            .is_some(),
        "a file written into the folder made again was not seen"
    );
}

/// Verify remove/recreate in one debounce batch drops old indexed contents
/// and watches the new folder despite no child events.
#[test]
fn a_folder_removed_and_made_again_in_one_batch_drops_what_it_held() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(root.path().join("sub/a.md"), "a").unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());
    let sub = f.vault.root().join("sub");

    std::fs::remove_dir_all(&sub).unwrap();
    std::fs::create_dir(&sub).unwrap();

    assert!(
        f.wait(|e| matches!(e, Event::FileRemoved(p) if p == "sub"))
            .is_some(),
        "the old folder's removal never reached the tabs"
    );
    let paths = f.vault.file_paths(true).unwrap();
    assert!(
        !paths.contains(&"sub/a.md".to_string()),
        "the old folder's note is still indexed: {paths:?}"
    );
    // The end of the batch, by when the new folder is watched. Under load the debouncer can
    // hand the removal and the making to two batches, and a write landing between them is
    // walked in rather than reported, so the index is what is asked, not the event.
    assert!(f.wait(|e| matches!(e, Event::DirsChanged(_))).is_some());
    f.write("sub/b.md", "b");
    let seen = || {
        f.vault
            .file_paths(true)
            .unwrap()
            .contains(&"sub/b.md".to_string())
    };
    assert!(
        poll_until(seen, BUDGET),
        "a file written into the folder made again was not seen"
    );
}

/// And made again with files in it already, which are walked in.
#[test]
fn a_folder_made_again_in_one_batch_brings_back_what_it_holds() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(root.path().join("sub/a.md"), "a").unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());
    let sub = f.vault.root().join("sub");

    std::fs::remove_dir_all(&sub).unwrap();
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("a.md"), "again").unwrap();
    std::fs::write(sub.join("c.md"), "c").unwrap();

    let back = || {
        let paths = f.vault.file_paths(true).unwrap();
        paths.contains(&"sub/a.md".to_string()) && paths.contains(&"sub/c.md".to_string())
    };
    assert!(
        poll_until(back, BUDGET),
        "what the new folder holds is not indexed"
    );
}

/// A tab asks for the folder of whatever file it opens to be watched, not knowing whether the
/// walk holds it: asked for the vault root, the worker must go on indexing the notes there.
#[test]
fn a_walked_folder_asked_to_be_watched_as_unindexed_stays_the_index_s() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.md"), "one").unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());

    f.vault.watch_unindexed(&[String::new()]).unwrap();
    f.write("a.md", "two");

    assert!(
        f.wait(|e| matches!(e, Event::FileChanged(p) if p == "a.md"))
            .is_some(),
        "an edit of a note at the root was taken for news of an unindexed folder"
    );
}

/// The tree lists what the index leaves out, so a gitignored folder made empty beside the
/// notes changes its parent's listing although nothing reaches the index.
#[test]
fn an_empty_ignored_folder_made_in_an_indexed_one_reports_its_parent() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".gitignore"), "out/\n").unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());

    std::fs::create_dir(f.vault.root().join("out")).unwrap();

    assert!(
        f.wait(|e| matches!(e, Event::DirsChanged(d) if d.iter().any(|d| d.is_empty())))
            .is_some(),
        "the root's listing was not reported changed"
    );
    assert!(
        !f.vault
            .file_paths(true)
            .unwrap()
            .contains(&"out".to_string())
    );
}

/// Verify linked folders are watched through vault paths; inotify follows
/// the symlink when installing directory watches.
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

/// Verify two workers sharing an index still report each other's writes
/// when rows are already current, while own saves stay quiet. Save different
/// notes to avoid late echoes being mistaken for own writes. Requires inotify.
#[test]
fn a_write_through_another_vault_on_the_same_index_reaches_this_one() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("Note.md"), "old\n").unwrap();
    std::fs::write(root.path().join("Other.md"), "old\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("index.db");
    let open = || {
        let (vault, events) =
            crate::Vault::open_at(root.path(), &db, VaultConfig::default()).unwrap();
        assert!(wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());
        (vault, events)
    };
    let (app, events) = open();
    let (mcp, mcp_events) = open();

    let (_, etag) = mcp.read("Note.md").unwrap();
    mcp.save("Note.md", "new\n", Some(etag)).unwrap();
    mcp.create_note("New.md", None).unwrap();
    assert!(
        wait_for(
            &events,
            |e| matches!(e, Event::FileChanged(p) if p == "Note.md"),
            BUDGET
        )
        .is_some(),
        "the edit never reached the other vault"
    );
    assert!(
        wait_for(
            &events,
            |e| matches!(e, Event::DirsChanged(d) if d.iter().any(|d| d.is_empty())),
            BUDGET
        )
        .is_some(),
        "the new note never reached the other vault's tree"
    );
    let changed =
        |rel: &'static str| move |e: &Event| matches!(e, Event::FileChanged(p) if p == rel);
    assert!(
        wait_for(&mcp_events, changed("Note.md"), Duration::from_secs(1)).is_none(),
        "a save came back to the vault that made it"
    );

    let (_, etag) = app.read("Other.md").unwrap();
    app.save("Other.md", "newer\n", Some(etag)).unwrap();
    assert!(wait_for(&mcp_events, changed("Other.md"), BUDGET).is_some());
    assert!(
        wait_for(&events, changed("Other.md"), Duration::from_secs(1)).is_none(),
        "a save came back to the vault that made it"
    );
}

/// A worker that panics says so on its way out, though the event channel stays open.
#[test]
fn a_worker_that_panics_says_it_is_gone() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (tx, rx) = channel();
    let (events, event_rx) = channel();
    // The language providers' sender, which keeps the channel open past the worker.
    let _providers = events.clone();
    let worker = spawn(
        root.path().canonicalize().unwrap(),
        Index::open(&cache.path().join("index.db")).unwrap(),
        rx,
        tx.clone(),
        events,
        false,
        Arc::new(AtomicU8::new(RUN)),
        Arc::new(Ignored::new(PathBuf::new())),
    )
    .unwrap();
    assert!(wait_for(&event_rx, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());

    tx.send(Msg::Panic).unwrap();
    let said = |e: &Event| matches!(e, Event::Error(why) if why.contains("the test's own panic"));
    assert!(
        wait_for(&event_rx, said, BUDGET).is_some(),
        "no reason given"
    );
    assert!(
        wait_for(&event_rx, |e| matches!(e, Event::WorkerGone), BUDGET).is_some(),
        "nothing heard of the worker's end"
    );
    assert!(worker.join().is_ok(), "the panic left the thread");
}

/// Verify an own save suppresses a late external echo in the same batch
/// without hiding subsequent external edits. An unwatched `sub` scan
/// holds the worker while test messages queue together.
#[test]
fn an_own_save_stays_quiet_behind_a_late_echo_of_another_change() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let note = root_path.join("Note.md");
    std::fs::write(&note, "old\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("index.db");
    let (tx, rx) = channel();
    let (events, event_rx) = channel();
    let worker = spawn(
        root_path.clone(),
        Index::open(&db).unwrap(),
        rx,
        tx.clone(),
        events,
        false,
        Arc::new(AtomicU8::new(RUN)),
        Arc::new(Ignored::new(PathBuf::new())),
    )
    .unwrap();
    assert!(wait_for(&event_rx, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());
    std::fs::create_dir(root_path.join("sub")).unwrap();
    for i in 0..600 {
        std::fs::write(root_path.join(format!("sub/n{i}.md")), "body").unwrap();
    }
    // The other process writes the note and takes it into the index first.
    let mut other = Index::open(&db).unwrap();
    let mut theirs = |text: &str| {
        fs::write_note(&note, text, None).unwrap();
        other.update_file(&root_path, "Note.md").unwrap();
    };
    let changed = || {
        let (reply, done) = channel();
        tx.send(Msg::Settled(reply)).unwrap();
        done.recv().unwrap();
        event_rx
            .try_iter()
            .any(|e| matches!(e, Event::FileChanged(p) if p == "Note.md"))
    };
    theirs("theirs\n");
    let etag = fs::write_note(&note, "mine\n", None).unwrap();
    tx.send(Msg::Rescan("sub".to_string())).unwrap();
    tx.send(Msg::Fs(VaultEvent::Changed(note.clone()))).unwrap();
    tx.send(Msg::Saved {
        rel: "Note.md".to_string(),
        etag,
    })
    .unwrap();
    assert!(!changed(), "an own save came back behind another's echo");

    theirs("theirs again\n");
    tx.send(Msg::Fs(VaultEvent::Changed(note.clone()))).unwrap();
    assert!(changed(), "another's change after it was lost");
    tx.send(Msg::Shutdown).unwrap();
    worker.join().unwrap();
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

/// Verify temp-plus-rename `Remove`/`Create` events do not report a note
/// removed while it remains on disk, which would close its tab.
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

/// A Syncthing pull is one batch of files; the links in all of them still have to resolve.
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

/// A vault opened on `files`, each `(rel, text)`.
fn open_with(files: &[(&str, &str)]) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        std::fs::write(root.path().join(rel), text).unwrap();
    }
    Fixture::open_dir(root, VaultConfig::default())
}

/// A batch resolves only what its files can have changed, so each kind of change is pinned
/// here: an edit moves the note's own links.
#[test]
fn an_edit_points_its_links_at_their_new_targets() {
    let f = open_with(&[("A.md", "a\n"), ("B.md", "b\n"), ("n.md", "see [[A]]\n")]);
    assert_eq!(f.vault.backlinks("A.md").unwrap().len(), 1);

    f.vault.save("n.md", "see [[B]]\n", None).unwrap();

    assert!(poll_until(
        || f.vault.backlinks("B.md").unwrap().len() == 1,
        BUDGET
    ));
    assert!(f.vault.backlinks("A.md").unwrap().is_empty());
}

/// A new note takes the links other notes wrote to it before it existed.
#[test]
fn a_new_note_takes_the_links_that_were_waiting_for_it() {
    let f = open_with(&[("n.md", "see [[Later]]\n")]);
    assert_eq!(f.vault.missing_notes().unwrap(), ["Later.md"]);

    f.vault.save("Later.md", "here\n", None).unwrap();

    assert!(poll_until(
        || f.vault.backlinks("Later.md").unwrap().len() == 1,
        BUDGET
    ));
    assert!(f.vault.missing_notes().unwrap().is_empty());
}

/// A deleted note leaves the links to it dangling, offered again as a note to write.
#[test]
fn a_deleted_note_leaves_the_links_to_it_dangling() {
    let f = open_with(&[("Gone.md", "soon\n"), ("n.md", "see [[Gone]]\n")]);
    assert!(f.vault.missing_notes().unwrap().is_empty());

    f.vault.delete("Gone.md").unwrap();

    assert!(poll_until(
        || f.vault.missing_notes().unwrap() == ["Gone.md"],
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
    f.vault.rescan().unwrap();
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

/// Verify external commits emit git events from explicitly watched repo
/// paths, without indexing `.git` or reporting its files as vault changes.
#[test]
fn a_commit_outside_the_app_reports_as_a_git_change() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let f = Fixture::open(VaultConfig::default());
    let root = f.vault.root().to_path_buf();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    f.write("a.md", "one\n");
    f.vault.rescan().unwrap();
    assert!(f.wait(|e| matches!(e, Event::Reconciled(_))).is_some());

    // Asking for the repositories is what puts `.git` in the watch set.
    assert_eq!(f.vault.repos().unwrap().len(), 1);
    git(&["add", "a.md"]);
    git(&["commit", "-qm", "one"]);

    assert!(
        f.wait(|e| matches!(e, Event::GitChanged)).is_some(),
        "a commit has to reach the pane"
    );

    // A push moves its remote-tracking ref and nothing else the watcher saw: one made in a
    // shell, or one a host finished after the link dropped, has to reach the pane too. Here to
    // a remote added after the watch set was made, of a branch whose ref is a level down.
    let bare = tempfile::tempdir().unwrap();
    git(&["init", "-q", "--bare", &bare.path().to_string_lossy()]);
    git(&["remote", "add", "origin", &bare.path().to_string_lossy()]);
    git(&["switch", "-q", "-c", "feature/x"]);
    git(&["push", "-q", "-u", "origin", "feature/x"]);
    git(&["commit", "-q", "--allow-empty", "-m", "two"]);
    let _ = wait_for(&f.events, |_| false, Duration::from_millis(1500));
    let repo = &f.vault.repos().unwrap()[0];
    assert_eq!(crate::git::status(repo).unwrap().branch.ahead, 1);
    git(&["push", "-q"]);
    assert!(
        f.wait(|e| matches!(e, Event::GitChanged)).is_some(),
        "a push has to reach the pane"
    );
    assert_eq!(crate::git::status(repo).unwrap().branch.ahead, 0);
}

/// Verify `.gitignore` edits remove newly ignored trees and index newly
/// unignored ones. Query index state, since unrelated reconciliation
/// events could satisfy an event wait.
#[test]
fn editing_a_gitignore_walks_the_vault_again() {
    let f = Fixture::open(VaultConfig::default());
    let run = "mlruns/run.md".to_string();
    let indexed = || f.vault.file_paths(false).unwrap().contains(&run);
    f.write("keep.md", "keep\n");
    f.write(&run, "run\n");
    f.vault.rescan().unwrap();
    assert!(poll_until(indexed, BUDGET), "the walk missed the tree");

    f.write(".gitignore", "mlruns/\n");
    assert!(
        poll_until(|| !indexed(), BUDGET),
        "a newly ignored tree stayed in the index"
    );

    f.write(".gitignore", "# nothing\n");
    assert!(
        poll_until(indexed, BUDGET),
        "the tree stayed lazy after it stopped being ignored"
    );
}

/// A `.gitignore` below the root rules nothing outside its own folder, so editing one walks
/// that folder alone: `sub` and its `.gitignore`, the tree it now ignores gone.
#[test]
fn editing_a_nested_gitignore_walks_its_folder_alone() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("keep.md"), "keep\n").unwrap();
    std::fs::create_dir_all(root.path().join("sub/mlruns")).unwrap();
    std::fs::write(root.path().join("sub/mlruns/run.md"), "run\n").unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());

    f.write("sub/.gitignore", "mlruns/\n");
    let done = f.wait(|e| matches!(e, Event::Reconciled(_)));
    let Some(Event::Reconciled(stats)) = done else {
        panic!("no walk: {done:?}");
    };
    assert_eq!(stats.scanned, 2, "{stats:?}");
    let paths = f.vault.file_paths(false).unwrap();
    assert_eq!(paths, ["keep.md", "sub/.gitignore"]);
}

/// Reload on a folder walks that folder: what no watcher reported under it is taken in, and
/// what changed outside it is left to a walk of its own. Unwatched, so only a walk finds either.
#[test]
fn a_walk_of_one_folder_takes_in_that_folder_alone() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (vault, events) = crate::Vault::open_unwatched_at(
        root.path(),
        &cache.path().join("index.db"),
        VaultConfig::default(),
    )
    .unwrap();
    assert!(wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());

    std::fs::write(root.path().join("sub/new.md"), "new\n").unwrap();
    std::fs::write(root.path().join("top.md"), "top\n").unwrap();
    vault.rescan_dir("sub").unwrap();
    assert!(wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());
    assert_eq!(vault.file_paths(false).unwrap(), ["sub/new.md"]);
    assert!(vault.rescan_dir("../elsewhere").is_err());
}

/// A `chmod` or a `touch` of a folder holding files reads as a change of the folder, as a
/// folder moved in does: either is a walk of that folder, never of the vault.
#[test]
fn a_folder_changed_in_place_walks_that_folder_alone() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("top.md"), "top\n").unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(root.path().join("sub/a.md"), "a\n").unwrap();
    let f = Fixture::open_dir(root, VaultConfig::default());

    let sub = f.vault.root().join("sub");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
    let walked = f.wait(|e| matches!(e, Event::Reconciled(_)));
    let Some(Event::Reconciled(stats)) = walked else {
        panic!("no walk: {walked:?}");
    };
    assert_eq!((stats.dir.as_str(), stats.scanned), ("sub", 2), "{stats:?}");

    // Moved in whole: inotify says nothing of what is inside, so only a walk finds it.
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(outside.path().join("moved/deep")).unwrap();
    std::fs::write(outside.path().join("moved/deep/b.md"), "b\n").unwrap();
    std::fs::rename(outside.path().join("moved"), f.vault.root().join("moved")).unwrap();
    let walked = f.wait(|e| matches!(e, Event::Reconciled(_)));
    let Some(Event::Reconciled(stats)) = walked else {
        panic!("no walk: {walked:?}");
    };
    assert_eq!(stats.dir, "moved", "{stats:?}");
    assert_eq!(
        f.vault.file_paths(false).unwrap(),
        ["moved/deep/b.md", "sub/a.md", "top.md"]
    );
}

/// A file written behind the index's back — over ssh, on a host — is in the index once the
/// writer says so, with no watcher to report it: unwatched, nothing else would.
#[test]
fn a_file_said_to_be_written_is_indexed_without_a_watcher() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (vault, events) = crate::Vault::open_unwatched_at(
        root.path(),
        &cache.path().join("index.db"),
        VaultConfig::default(),
    )
    .unwrap();
    assert!(wait_for(&events, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());

    std::fs::write(root.path().join("pasted.png"), "png").unwrap();
    vault.wrote("pasted.png").unwrap();
    assert!(wait_for(&events, |e| matches!(e, Event::DirsChanged(_)), BUDGET).is_some());
    assert_eq!(
        vault.resolve_link("pasted.png").unwrap().as_deref(),
        Some("pasted.png")
    );
    assert!(vault.wrote("../elsewhere.png").is_err());
}

/// Verify `.gitignore` saves within [`WALK_FLOOR`](super::WALK_FLOOR)
/// coalesce into a scan at its end, with later saves scanning immediately.
#[test]
fn saves_of_a_gitignore_inside_the_floor_are_one_walk() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::create_dir(root_path.join("mlruns")).unwrap();
    std::fs::write(root_path.join("mlruns/run.md"), "run\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("index.db");
    let (tx, rx) = channel();
    let (events, event_rx) = channel();
    // Unwatched: the saves below are the worker's only news, as `Vault::save` posts them.
    let worker = spawn(
        root_path.clone(),
        Index::open(&db).unwrap(),
        rx,
        tx.clone(),
        events,
        false,
        Arc::new(AtomicU8::new(RUN)),
        Arc::new(Ignored::new(PathBuf::new())),
    )
    .unwrap();
    assert!(wait_for(&event_rx, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());
    let save = |text: &str| {
        std::fs::write(root_path.join(".gitignore"), text).unwrap();
        tx.send(Msg::Update {
            rel: ".gitignore".to_string(),
            own: true,
        })
        .unwrap();
    };
    let walks = |within: Duration| {
        let deadline = Instant::now() + within;
        let mut n = 0;
        while let Ok(e) = event_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            n += usize::from(matches!(e, Event::Reconciled(_)));
        }
        n
    };

    for text in ["m", "mlr", "mlruns/\n"] {
        save(text);
        std::thread::sleep(Duration::from_millis(150));
    }
    assert_eq!(walks(super::WALK_FLOOR + Duration::from_secs(1)), 1);
    let indexed = Index::open(&db).unwrap().file_paths(false).unwrap();
    assert!(
        !indexed.contains(&"mlruns/run.md".to_string()),
        "the walk read the last save: {indexed:?}"
    );

    std::thread::sleep(super::WALK_FLOOR);
    save("# nothing\n");
    assert_eq!(
        walks(Duration::from_millis(500)),
        1,
        "the save after the floor waited"
    );
    tx.send(Msg::Shutdown).unwrap();
    worker.join().unwrap();
}

/// Verify watch-set changes retain pending debounced events, so `repos()`
/// refresh cannot discard a note edit waiting to be indexed.
#[test]
fn a_write_just_before_the_watch_set_changes_is_still_indexed() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::create_dir_all(root_path.join(".git/refs/heads")).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("index.db");
    let (tx, rx) = channel();
    let (events, event_rx) = channel();
    let worker = spawn(
        root_path.clone(),
        Index::open(&db).unwrap(),
        rx,
        tx.clone(),
        events,
        true,
        Arc::new(AtomicU8::new(RUN)),
        Arc::new(Ignored::new(PathBuf::new())),
    )
    .unwrap();
    assert!(wait_for(&event_rx, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());

    std::fs::write(root_path.join("late.md"), "late\n").unwrap();
    tx.send(Msg::WatchGit(vec![root_path.join(".git")]))
        .unwrap();

    let indexed = || {
        Index::open(&db)
            .unwrap()
            .file_paths(false)
            .unwrap()
            .contains(&"late.md".to_string())
    };
    assert!(poll_until(indexed, BUDGET), "the write was lost");
    tx.send(Msg::Shutdown).unwrap();
    worker.join().unwrap();
}

/// Verify unindexed-folder watches refresh direct-child listings without
/// indexing or recursive scans, and renew after folder recreation.
#[test]
fn a_watched_unindexed_folder_is_relisted_and_never_indexed() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::write(root_path.join(".gitignore"), "build/\n").unwrap();
    std::fs::create_dir_all(root_path.join("build/sub")).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("index.db");
    let (tx, rx) = channel();
    let (events, event_rx) = channel();
    let worker = spawn(
        root_path.clone(),
        Index::open(&db).unwrap(),
        rx,
        tx.clone(),
        events,
        true,
        Arc::new(AtomicU8::new(RUN)),
        Arc::new(Ignored::new(PathBuf::new())),
    )
    .unwrap();
    assert!(wait_for(&event_rx, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());
    let dirs = vec!["build".to_string(), "build/sub".to_string()];
    tx.send(Msg::WatchUnindexed(dirs, true)).unwrap();
    let (reply, armed) = channel();
    tx.send(Msg::Settled(reply)).unwrap();
    armed.recv().unwrap();
    let relisted = |dir: &str| {
        wait_for(
            &event_rx,
            |e| matches!(e, Event::UnindexedChanged(d) if d.iter().any(|d| d == dir)),
            BUDGET,
        )
        .is_some()
    };

    std::fs::write(root_path.join("build/new.md"), "new\n").unwrap();
    assert!(
        relisted("build"),
        "a file made in the folder was not reported"
    );
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.md"), "x\n").unwrap();
    std::fs::rename(outside.path(), root_path.join("build/moved")).unwrap();
    let next = wait_for(
        &event_rx,
        |e| matches!(e, Event::UnindexedChanged(_) | Event::Reconciled(_)),
        BUDGET,
    );
    assert!(
        matches!(next, Some(Event::UnindexedChanged(_))),
        "a directory moved into the folder walked the vault: {next:?}"
    );
    let paths = Index::open(&db).unwrap().file_paths(true).unwrap();
    assert!(
        !paths.iter().any(|p| p.starts_with("build")),
        "the folder reached the index: {paths:?}"
    );

    std::fs::remove_dir(root_path.join("build/sub")).unwrap();
    std::fs::create_dir(root_path.join("build/sub")).unwrap();
    assert!(
        relisted("build/sub"),
        "the folder made anew was not reported"
    );
    std::fs::write(root_path.join("build/sub/again.md"), "again\n").unwrap();
    assert!(relisted("build/sub"), "the folder made anew is not watched");

    tx.send(Msg::Shutdown).unwrap();
    worker.join().unwrap();
}

/// Verify git events and exclusion replies are processed during the first
/// large scan, avoiding a blocked caller's remote deadline. Queue both
/// before the worker starts to ensure they arrive during scanning.
#[test]
fn the_inbox_is_read_between_the_batches_of_a_walk() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::write(root_path.join("keep.txt"), "keep").unwrap();
    std::fs::create_dir(root_path.join("build")).unwrap();
    std::fs::write(root_path.join("build/out.txt"), "out").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("index.db");

    let (tx, rx) = channel();
    let (events, event_rx) = channel();
    let (reply, answer) = channel();
    tx.send(Msg::SetExcluded(vec!["build/".to_string()], reply))
        .unwrap();
    tx.send(Msg::Fs(VaultEvent::Git(root_path.join(".git"))))
        .unwrap();
    let worker = spawn(
        root_path.clone(),
        Index::open(&db).unwrap(),
        rx,
        tx.clone(),
        events,
        true,
        Arc::new(AtomicU8::new(RUN)),
        Arc::new(Ignored::new(PathBuf::new())),
    )
    .unwrap();

    let first = wait_for(
        &event_rx,
        |e| matches!(e, Event::GitChanged | Event::Reconciled(_)),
        BUDGET,
    );
    assert!(
        matches!(first, Some(Event::GitChanged)),
        "the git change waited for the walk: {first:?}"
    );
    assert!(
        matches!(answer.try_recv(), Ok(Ok(()))),
        "set_excluded waited for the walk"
    );

    // Written before the walk had added a row, so what left `build/out.txt` out is the set
    // being written again once the walk was over.
    assert!(wait_for(&event_rx, |e| matches!(e, Event::Reconciled(_)), BUDGET).is_some());
    assert_eq!(
        Index::open(&db).unwrap().file_paths(false).unwrap(),
        ["keep.txt"]
    );

    tx.send(Msg::Shutdown).unwrap();
    worker.join().unwrap();
}
