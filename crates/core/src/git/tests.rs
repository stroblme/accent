//! The `git` module's tests. They drive the real `git` binary in a temporary repository, so they
//! live beside it rather than in `tests/`: everything they reach for is private to the module.

use super::*;
use std::collections::BTreeSet;

/// These tests drive the real `git` binary. On a machine without one they skip rather than
/// fail: nothing in accent requires git to be installed.
fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

/// Run git with the developer's own configuration shut out, so a signing key, a hooks path or
/// another `init.defaultBranch` in `~/.gitconfig` cannot decide whether this suite passes.
fn sh(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Accent Test")
        .env("GIT_AUTHOR_EMAIL", "test@accent.invalid")
        .env("GIT_COMMITTER_NAME", "Accent Test")
        .env("GIT_COMMITTER_EMAIL", "test@accent.invalid")
        .env("LC_ALL", "C")
        .output()
        .expect("git should be runnable")
}

fn ok(dir: &Path, args: &[&str]) {
    let out = sh(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with a deterministic branch name plus an identity and hook path of its own.
/// These land in the repository's *local* config on purpose: the functions under test run the
/// user's git in the user's environment, so only local config reaches them too.
fn init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    ok(dir, &["init", "-b", "main"]);
    configure(dir);
}

fn configure(dir: &Path) {
    ok(dir, &["config", "user.name", "Accent Test"]);
    ok(dir, &["config", "user.email", "test@accent.invalid"]);
    ok(dir, &["config", "commit.gpgsign", "false"]);
    ok(dir, &["config", "core.hooksPath", ".git/hooks-disabled"]);
}

fn write_file(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn commit_all(dir: &Path, message: &str) {
    ok(dir, &["add", "-A"]);
    ok(dir, &["commit", "-m", message]);
}

fn head(dir: &Path) -> String {
    String::from_utf8_lossy(&sh(dir, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string()
}

fn open(dir: &Path) -> Repo {
    toplevel(dir)
        .unwrap()
        .expect("a repository at the test root")
}

fn paths<'a>(entries: impl Iterator<Item = &'a Entry>) -> Vec<&'a str> {
    let mut out: Vec<&str> = entries.map(|e| e.path.as_str()).collect();
    out.sort_unstable();
    out
}

// ------------------------------------------------------------------ status

#[test]
fn parse_status_reads_headers_changes_untracked_and_ignored() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    write_file(dir, ".gitignore", "build/\n");
    commit_all(dir, "first");

    write_file(dir, "a.md", "one\ntwo\n");
    write_file(dir, "b.md", "b\n");
    ok(dir, &["add", "b.md"]);
    write_file(dir, "c d.md", "c\n");
    write_file(dir, "build/out.txt", "x\n");

    let st = status(&open(dir)).unwrap();
    assert_eq!(st.branch.head.as_deref(), Some("main"));
    assert!(st.branch.oid.as_deref().is_some_and(|oid| oid.len() == 40));
    assert_eq!(st.branch.upstream, None);
    assert_eq!((st.branch.ahead, st.branch.behind), (0, 0));

    let find = |p: &str| {
        st.entries
            .iter()
            .find(|e| e.path == p)
            .unwrap_or_else(|| panic!("no entry for {p}"))
    };
    assert_eq!((find("a.md").x, find("a.md").y), ('.', 'M'));
    assert_eq!((find("b.md").x, find("b.md").y), ('A', '.'));
    let untracked = find("c d.md");
    assert_eq!((untracked.x, untracked.y), ('?', '?'));
    assert!(!untracked.submodule);
    assert_eq!(st.ignored, ["build/"], "a whole ignored tree is one entry");

    assert_eq!(paths(st.staged()), ["b.md"]);
    assert_eq!(paths(st.changes()), ["a.md", "c d.md"]);
    assert_eq!(st.conflicts().count(), 0);
    assert!(st.dirty());

    // Untracked alone still counts: the dot and the changes list read the same set.
    ok(dir, &["stash", "-q", "--include-untracked"]);
    let clean = status(&open(dir)).unwrap();
    assert!(!clean.dirty(), "an ignored tree is not uncommitted work");
    assert_eq!(clean.ignored, ["build/"]);
    write_file(dir, "new.md", "new\n");
    assert!(status(&open(dir)).unwrap().dirty());
}

#[test]
fn a_rename_carries_its_original_path() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "first");
    ok(dir, &["mv", "a.md", "b.md"]);

    let st = status(&open(dir)).unwrap();
    assert_eq!(
        st.entries.len(),
        1,
        "the original path is a second token, not a second entry"
    );
    assert_eq!(st.entries[0].path, "b.md");
    assert_eq!(st.entries[0].orig.as_deref(), Some("a.md"));
    assert_eq!(st.entries[0].x, 'R');
}

#[test]
fn an_unmerged_entry_is_a_conflict() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "f.md", "base\n");
    commit_all(dir, "base");
    ok(dir, &["checkout", "-q", "-b", "side"]);
    write_file(dir, "f.md", "side\n");
    commit_all(dir, "side");
    ok(dir, &["checkout", "-q", "main"]);
    write_file(dir, "f.md", "main\n");
    commit_all(dir, "main");
    assert!(
        !sh(dir, &["merge", "side"]).status.success(),
        "the merge is supposed to conflict"
    );

    let st = status(&open(dir)).unwrap();
    assert_eq!(paths(st.conflicts()), ["f.md"]);
    assert_eq!(st.conflicts().next().unwrap().x, 'U');
    assert_eq!(st.staged().count(), 0, "a conflict is not staged work");
    assert_eq!(st.changes().count(), 0, "nor an ordinary change");
}

#[test]
fn ahead_behind_counts_against_a_local_clone() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    init(&source);
    write_file(&source, "a.md", "one\n");
    commit_all(&source, "first");
    // The origin is bare: pushing to a repository that has the branch checked out is refused.
    ok(tmp.path(), &["clone", "--bare", "-q", "source", "origin"]);
    ok(tmp.path(), &["clone", "-q", "origin", "work"]);

    let work = tmp.path().join("work");
    configure(&work);
    write_file(&work, "b.md", "b\n");
    commit_all(&work, "second");

    let repo = open(&work);
    let st = status(&repo).unwrap();
    assert_eq!(st.branch.upstream.as_deref(), Some("origin/main"));
    assert_eq!((st.branch.ahead, st.branch.behind), (1, 0));

    assert!(push(&repo).is_ok());
    assert_eq!(status(&repo).unwrap().branch.ahead, 0);
    assert!(pull(&repo).is_ok());
}

// ------------------------------------------------------------- the remote

/// A clone, a commit pushed into its origin by somebody else, and the two things that only a
/// fetch can tell us: how far behind we are, and which commits those are.
#[test]
fn a_fetch_is_what_makes_behind_and_incoming_true() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    init(&source);
    write_file(&source, "a.md", "one\n");
    commit_all(&source, "first");
    ok(tmp.path(), &["clone", "--bare", "-q", "source", "origin"]);
    for clone in ["work", "other"] {
        ok(tmp.path(), &["clone", "-q", "origin", clone]);
        configure(&tmp.path().join(clone));
    }

    let other = tmp.path().join("other");
    write_file(&other, "theirs.md", "theirs\n");
    commit_all(&other, "theirs");
    ok(&other, &["push", "-q"]);
    let theirs = head(&other);

    let work = tmp.path().join("work");
    let repo = open(&work);
    let st = status(&repo).unwrap();
    assert_eq!(
        (st.branch.ahead, st.branch.behind),
        (0, 0),
        "the remote moved, but nothing has looked yet"
    );
    assert_eq!(
        incoming(&repo).unwrap(),
        Vec::<String>::new(),
        "and the range is empty for the same reason"
    );

    fetch(&repo).unwrap();
    assert_eq!(
        status(&repo).unwrap().branch.behind,
        1,
        "the fetch found it"
    );
    assert_eq!(incoming(&repo).unwrap(), std::slice::from_ref(&theirs));
    // A fetch merges nothing: the commit is in the history without being in the worktree.
    assert!(!work.join("theirs.md").exists());
    assert!(
        log(&repo, 0, 50).unwrap().iter().any(|c| c.id == theirs),
        "`log --all` lists it, which is what the pane marks as not pulled"
    );

    pull(&repo).unwrap();
    assert!(
        incoming(&repo).unwrap().is_empty(),
        "and nothing after a pull"
    );
}

#[test]
fn incoming_refuses_where_there_is_no_upstream() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "first");
    assert!(incoming(&open(dir)).is_err());
}

// ----------------------------------------------------------------- history

/// The rules [`lanes`] must keep, whatever shape the history has.
fn check_invariants(rows: &[LogRow]) {
    for (r, row) in rows.iter().enumerate() {
        assert_eq!(
            row.below.is_empty(),
            row.commit.parents.is_empty(),
            "row {r}: an edge leaves for every parent, and only then"
        );
        if let Some(&first) = row.below.first() {
            assert_eq!(
                first, row.column,
                "row {r}: the first parent stays in this commit's column"
            );
        }
        assert!(
            !row.through.contains(&row.column),
            "row {r}: a lane cannot pass through its own commit"
        );
        let has_child = rows[..r]
            .iter()
            .any(|drawn| drawn.commit.parents.contains(&row.commit.id));
        assert_eq!(
            row.above.is_empty(),
            !has_child,
            "row {r}: edges come in exactly when a child was drawn above"
        );
        if r > 0 {
            let leaving: BTreeSet<usize> = rows[r - 1]
                .below
                .iter()
                .chain(&rows[r - 1].through)
                .copied()
                .collect();
            let entering: BTreeSet<usize> = row.above.iter().chain(&row.through).copied().collect();
            assert_eq!(
                leaving, entering,
                "row {r}: every edge leaving the row above has to enter this one"
            );
        }
    }
}

#[test]
fn parse_log_and_lanes_on_a_linear_history() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    for message in ["one", "two", "three"] {
        write_file(dir, "a.md", message);
        commit_all(dir, message);
    }

    let repo = open(dir);
    let commits = log(&repo, 0, 10).unwrap();
    let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["three", "two", "one"]);
    assert_eq!(commits[0].author, "Accent Test");
    assert!(commits[0].time > 0);
    assert!(commits[0].refs.iter().any(|r| r.contains("main")));
    assert!(
        commits[1].refs.is_empty(),
        "an undecorated commit has no refs"
    );
    assert_eq!(commits[0].parents, [commits[1].id.clone()]);
    assert!(commits[2].parents.is_empty(), "the root has no parent");
    assert_eq!(
        log(&repo, 1, 1).unwrap()[0].summary,
        "two",
        "skip and limit"
    );

    let rows = lanes(commits);
    check_invariants(&rows);
    assert!(rows.iter().all(|r| r.column == 0 && r.through.is_empty()));

    // The subject and the rest of the message are two fields, and the body's own newlines
    // survive the record split because the record separator is not one of them.
    write_file(dir, "a.md", "four");
    commit_all(dir, "four\n\nwhy it happened\nand a second line");
    let head = log(&repo, 0, 1).unwrap();
    assert_eq!(head[0].summary, "four");
    assert_eq!(head[0].body, "why it happened\nand a second line");
    assert_eq!(log(&repo, 1, 1).unwrap()[0].body, "", "a one-line message");
}

#[test]
fn lanes_on_a_diamond_merge() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "f.md", "base\n");
    commit_all(dir, "base");
    ok(dir, &["checkout", "-q", "-b", "side"]);
    write_file(dir, "s.md", "s\n");
    commit_all(dir, "side");
    ok(dir, &["checkout", "-q", "main"]);
    write_file(dir, "m.md", "m\n");
    commit_all(dir, "main");
    ok(dir, &["merge", "-q", "--no-ff", "side", "-m", "merge"]);

    let rows = lanes(log(&open(dir), 0, 10).unwrap());
    check_invariants(&rows);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].commit.summary, "merge");
    assert_eq!(rows[0].below, [0, 1], "the second parent opens a column");
    let base = rows.last().unwrap();
    assert_eq!(base.commit.summary, "base");
    assert_eq!(base.above, [0, 1], "both sides come back together here");
    assert!(base.below.is_empty());
    assert!(rows.iter().all(|r| r.column <= 1), "a diamond is two wide");
}

#[test]
fn lanes_on_two_unmerged_tips_stay_two_columns_wide() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "f.md", "base\n");
    commit_all(dir, "base");
    ok(dir, &["checkout", "-q", "-b", "side"]);
    write_file(dir, "s.md", "s\n");
    commit_all(dir, "side");
    ok(dir, &["checkout", "-q", "main"]);
    write_file(dir, "m.md", "m\n");
    commit_all(dir, "main");

    let rows = lanes(log(&open(dir), 0, 10).unwrap());
    check_invariants(&rows);
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.column <= 1), "two tips, two columns");
    let base = rows.last().unwrap();
    assert_eq!(base.above, [0, 1], "the fork drains both columns");
    assert_eq!(base.column, 0);
    assert!(base.through.is_empty());
}

#[test]
fn lanes_on_an_octopus_merge() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "f.md", "base\n");
    commit_all(dir, "base");
    ok(dir, &["branch", "b"]);
    ok(dir, &["branch", "c"]);
    for branch in ["b", "c"] {
        ok(dir, &["checkout", "-q", branch]);
        write_file(dir, &format!("{branch}.md"), branch);
        commit_all(dir, branch);
    }
    ok(dir, &["checkout", "-q", "main"]);
    write_file(dir, "m.md", "m\n");
    commit_all(dir, "main");
    ok(dir, &["merge", "-q", "b", "c", "-m", "octopus"]);

    let rows = lanes(log(&open(dir), 0, 10).unwrap());
    check_invariants(&rows);
    assert_eq!(rows[0].commit.summary, "octopus");
    assert_eq!(rows[0].commit.parents.len(), 3);
    assert_eq!(rows[0].below, [0, 1, 2], "one column per parent");
    assert_eq!(rows.last().unwrap().above, [0, 1, 2]);
}

#[test]
fn lanes_on_two_orphan_roots() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "f.md", "one\n");
    commit_all(dir, "first root");
    write_file(dir, "f.md", "two\n");
    commit_all(dir, "first tip");
    ok(dir, &["checkout", "-q", "--orphan", "other"]);
    ok(dir, &["rm", "-r", "-q", "-f", "."]);
    write_file(dir, "g.md", "one\n");
    commit_all(dir, "second root");
    write_file(dir, "g.md", "two\n");
    commit_all(dir, "second tip");

    let rows = lanes(log(&open(dir), 0, 10).unwrap());
    check_invariants(&rows);
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter().filter(|r| r.commit.parents.is_empty()).count(),
        2,
        "two roots"
    );
    assert!(
        rows.iter().all(|r| r.column == 0),
        "the first root frees column 0 and the second history takes it back"
    );
}

// --------------------------------------------------------------- discovery

#[test]
fn discover_finds_the_toplevel_and_nested_repos() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let vault = tmp.path().join("vault");
    init(&vault);
    write_file(&vault, "a.md", "one\n");
    commit_all(&vault, "first");
    ok(&vault, &["init", "-b", "main", "sub/inner"]);
    // A linked worktree has `.git` as a file, which is the case a `is_dir` check would miss.
    ok(&vault, &["worktree", "add", "-q", "wt", "-b", "wt"]);

    let dirs = ["sub", "sub/inner", "wt"].map(|d| vault.join(d)).to_vec();
    let repos = discover(&vault, &dirs);
    let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        ["vault", "inner", "wt"],
        "the vault's own repo leads"
    );
    assert_eq!(repos[1].root, vault.join("sub/inner"));
    assert!(repos[2].git_dir.ends_with(".git/worktrees/wt"));
    assert_eq!(
        repos
            .iter()
            .map(|r| &r.git_dir)
            .collect::<BTreeSet<_>>()
            .len(),
        3,
        "no repository is listed twice"
    );

    let plain = tempfile::tempdir().unwrap();
    assert!(discover(plain.path(), &[]).is_empty());
}

// -------------------------------------------------------- read and write

#[test]
fn show_reads_head_index_and_reports_missing() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "first");
    write_file(dir, "a.md", "two\n");
    ok(dir, &["add", "a.md"]);
    write_file(dir, "new.md", "new\n");

    let repo = open(dir);
    assert_eq!(
        show(&repo, "HEAD", "a.md").unwrap(),
        Some(Blob::Text("one\n".into()))
    );
    assert_eq!(
        show(&repo, "", "a.md").unwrap(),
        Some(Blob::Text("two\n".into()))
    );
    assert_eq!(show(&repo, "HEAD", "gone.md").unwrap(), None);
    assert_eq!(
        show(&repo, "", "new.md").unwrap(),
        None,
        "untracked on disk"
    );
}

#[test]
fn stage_unstage_commit_round_trip() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    let repo = open(dir);

    stage(&repo, &["a.md"]).unwrap();
    assert_eq!(paths(status(&repo).unwrap().staged()), ["a.md"]);

    let id = commit(&repo, "first\n\nwith a body\n", false).unwrap();
    assert!(!id.is_empty());
    assert!(status(&repo).unwrap().entries.is_empty(), "a clean tree");
    assert_eq!(log(&repo, 0, 1).unwrap()[0].summary, "first");

    write_file(dir, "b.md", "b\n");
    stage(&repo, &["b.md"]).unwrap();
    assert_eq!(paths(status(&repo).unwrap().staged()), ["b.md"]);
    unstage(&repo, &["b.md"]).unwrap();
    assert_eq!(paths(status(&repo).unwrap().changes()), ["b.md"]);

    write_file(dir, "a.md", "clobbered\n");
    discard(&repo, &["a.md"]).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "one\n");
}

#[test]
fn unstage_without_a_commit_takes_the_file_back_out_of_the_index() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    let repo = open(dir);

    write_file(dir, "a.md", "one\n");
    stage(&repo, &["a.md"]).unwrap();
    assert_eq!(paths(status(&repo).unwrap().staged()), ["a.md"]);

    unstage(&repo, &["a.md"]).unwrap();
    let st = status(&repo).unwrap();
    assert_eq!(paths(st.changes()), ["a.md"]);
    assert_eq!(st.entries[0].x, '?', "back to untracked");

    // Edited after staging: the index matches neither the worktree nor a HEAD that is not
    // there, which is the case `rm --cached` refuses without `-f`.
    stage(&repo, &["a.md"]).unwrap();
    write_file(dir, "a.md", "two\n");
    unstage(&repo, &["a.md"]).unwrap();
    assert_eq!(paths(status(&repo).unwrap().changes()), ["a.md"]);
    assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "two\n");
}

#[test]
fn branches_lists_the_local_ones_and_checkout_refuses_to_clobber() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "root");
    ok(dir, &["branch", "side"]);
    let repo = open(dir);

    let listed = branches(&repo).unwrap();
    assert_eq!(listed.local, ["main", "side"]);
    assert!(
        listed.remote.is_empty(),
        "no remote, no remote-tracking branches"
    );

    checkout(&repo, "side").unwrap();
    assert_eq!(status(&repo).unwrap().branch.head.as_deref(), Some("side"));

    // A change that the other branch would overwrite is git's own refusal, and the whole
    // point of driving `git switch`: nothing here decides whether a checkout is safe.
    write_file(dir, "a.md", "two\n");
    commit_all(dir, "side moves on");
    checkout(&repo, "main").unwrap();
    write_file(dir, "a.md", "uncommitted\n");
    let refused = checkout(&repo, "side").unwrap_err();
    assert!(
        refused.to_string().contains("would be overwritten"),
        "{refused}"
    );
    assert_eq!(
        status(&repo).unwrap().branch.head.as_deref(),
        Some("main"),
        "a refused switch leaves HEAD where it was"
    );
}

#[test]
fn checkout_commit_detaches_head_and_a_branch_takes_it_back() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "first");
    let first = head(dir);
    write_file(dir, "a.md", "two\n");
    commit_all(dir, "second");
    let repo = open(dir);

    checkout_commit(&repo, &first).unwrap();
    let detached = status(&repo).unwrap().branch;
    assert_eq!(detached.head, None, "no branch to be on");
    assert_eq!(detached.oid.as_deref(), Some(first.as_str()));

    checkout(&repo, "main").unwrap();
    assert_eq!(status(&repo).unwrap().branch.head.as_deref(), Some("main"));
}

#[test]
fn branches_are_created_and_deleted_and_git_says_when_work_would_be_lost() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "root");
    let repo = open(dir);

    create_branch(&repo, "side", false).unwrap();
    assert_eq!(branches(&repo).unwrap().local, ["main", "side"]);
    assert_eq!(
        status(&repo).unwrap().branch.head.as_deref(),
        Some("main"),
        "created without being switched to"
    );
    assert!(
        create_branch(&repo, "side", false).is_err(),
        "already taken"
    );

    create_branch(&repo, "work", true).unwrap();
    assert_eq!(status(&repo).unwrap().branch.head.as_deref(), Some("work"));

    // Nothing on `side` that `main` does not already have, so `-d` is enough.
    checkout(&repo, "main").unwrap();
    delete_branch(&repo, "side", false).unwrap();
    assert_eq!(branches(&repo).unwrap().local, ["main", "work"]);

    checkout(&repo, "work").unwrap();
    write_file(dir, "b.md", "b\n");
    commit_all(dir, "work moves on");
    checkout(&repo, "main").unwrap();
    let refused = delete_branch(&repo, "work", false).unwrap_err().to_string();
    assert!(unmerged(&refused), "{refused}");
    delete_branch(&repo, "work", true).unwrap();
    assert_eq!(branches(&repo).unwrap().local, ["main"]);

    // The checked-out branch is a refusal nothing can force, so it must not read as one.
    let refused = delete_branch(&repo, "main", false).unwrap_err().to_string();
    assert!(!unmerged(&refused), "{refused}");
}

#[test]
fn unmerged_is_gits_own_wording() {
    assert!(unmerged("error: the branch 'side' is not fully merged."));
    assert!(!unmerged(
        "error: cannot delete branch 'main' used by worktree at '/tmp/v'"
    ));
}

#[test]
fn parse_branches_splits_local_from_remote_and_drops_the_symbolic_ones() {
    let listed = parse_branches(
        b"refs/heads/main\0\nrefs/heads/feature/x\0\n\
          refs/remotes/origin/HEAD\0refs/remotes/origin/main\nrefs/remotes/origin/main\0\n",
    );
    assert_eq!(listed.local, ["main", "feature/x"]);
    assert_eq!(listed.remote, ["origin/main"], "origin/HEAD is a pointer");
    assert_eq!(parse_branches(b""), Branches::default());
}

/// A clone whose origin has a branch the clone never checked out: the remote-tracking branch is
/// listed, and tracking it is what makes it a local branch.
#[test]
fn a_remote_only_branch_is_listed_and_tracking_it_checks_it_out() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    init(&source);
    write_file(&source, "a.md", "one\n");
    commit_all(&source, "first");
    ok(&source, &["branch", "side"]);
    ok(tmp.path(), &["clone", "--bare", "-q", "source", "origin"]);
    ok(tmp.path(), &["clone", "-q", "origin", "work"]);
    let work = tmp.path().join("work");
    configure(&work);
    let repo = open(&work);

    let listed = branches(&repo).unwrap();
    assert_eq!(listed.local, ["main"]);
    assert_eq!(listed.remote, ["origin/main", "origin/side"]);

    track(&repo, "origin/side").unwrap();
    let branch = status(&repo).unwrap().branch;
    assert_eq!(branch.head.as_deref(), Some("side"));
    assert_eq!(branch.upstream.as_deref(), Some("origin/side"));
    assert_eq!(branches(&repo).unwrap().local, ["main", "side"]);
    assert!(
        track(&repo, "origin/side").is_err(),
        "the local branch exists now"
    );
}

#[test]
fn changed_files_reads_a_commit_a_root_a_merge_and_a_rename() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    write_file(dir, "b.md", "b\n");
    commit_all(dir, "root");
    let root = head(dir);
    write_file(dir, "a.md", "two\n");
    commit_all(dir, "second");
    let second = head(dir);
    ok(dir, &["mv", "a.md", "renamed.md"]);
    commit_all(dir, "rename");
    let rename = head(dir);
    ok(dir, &["checkout", "-q", "-b", "side", &root]);
    write_file(dir, "c.md", "c\n");
    commit_all(dir, "side");
    ok(dir, &["checkout", "-q", "main"]);
    ok(dir, &["merge", "-q", "--no-ff", "side", "-m", "merge"]);
    let merge = head(dir);

    let repo = open(dir);
    let files = |oid: &str| changed_files(&repo, oid).unwrap();
    assert_eq!(
        files(&root),
        [('A', "a.md".to_string()), ('A', "b.md".to_string())],
        "a root commit adds everything in it"
    );
    assert_eq!(files(&second), [('M', "a.md".to_string())]);
    assert_eq!(
        files(&rename),
        [('R', "renamed.md".to_string())],
        "a rename is one row, under the name it now has"
    );
    assert_eq!(
        files(&merge),
        [('A', "c.md".to_string())],
        "a merge shows its first-parent diff"
    );
}

#[test]
fn commit_all_takes_tracked_changes_and_leaves_untracked_files_alone() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "first");

    write_file(dir, "a.md", "two\n");
    write_file(dir, "new.md", "new\n");
    let repo = open(dir);
    assert_eq!(status(&repo).unwrap().staged().count(), 0, "nothing staged");

    commit(&repo, "everything tracked\n", true).unwrap();
    let st = status(&repo).unwrap();
    assert_eq!(paths(st.changes()), ["new.md"], "still untracked");
    assert_eq!(
        show(&repo, "HEAD", "a.md").unwrap(),
        Some(Blob::Text("two\n".into())),
        "the tracked change went in"
    );
}

/// `base` on `main`, then `side` and `main` each writing their own `f.md`. The merge settings are
/// local, so a developer's own `merge.ff` or `merge.autoStash` cannot decide what these tests see.
fn diverged(dir: &Path) {
    init(dir);
    ok(dir, &["config", "merge.ff", "true"]);
    ok(dir, &["config", "merge.autoStash", "false"]);
    write_file(dir, "f.md", "base\n");
    commit_all(dir, "base");
    ok(dir, &["checkout", "-q", "-b", "side"]);
    write_file(dir, "f.md", "side\n");
    commit_all(dir, "side");
    ok(dir, &["checkout", "-q", "main"]);
    write_file(dir, "f.md", "main\n");
    commit_all(dir, "main");
}

#[test]
fn merge_fast_forwards_commits_and_says_when_there_is_nothing_to_merge() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    ok(dir, &["config", "merge.ff", "true"]);
    write_file(dir, "f.md", "base\n");
    commit_all(dir, "base");
    ok(dir, &["checkout", "-q", "-b", "side"]);
    write_file(dir, "s.md", "s\n");
    commit_all(dir, "side");
    let side = head(dir);
    ok(dir, &["checkout", "-q", "main"]);
    let repo = open(dir);

    assert_eq!(merge(&repo, "side").unwrap(), Merge::FastForward);
    assert_eq!(head(dir), side);
    assert_eq!(merge(&repo, "side").unwrap(), Merge::UpToDate);

    write_file(dir, "m.md", "m\n");
    commit_all(dir, "main moves on");
    ok(dir, &["checkout", "-q", "side"]);
    write_file(dir, "t.md", "t\n");
    commit_all(dir, "side moves on");
    ok(dir, &["checkout", "-q", "main"]);
    assert_eq!(merge(&repo, "side").unwrap(), Merge::Commit);
    let top = &log(&repo, 0, 1).unwrap()[0];
    assert_eq!(top.parents.len(), 2);
    assert_eq!(top.summary, "Merge branch 'side'");
    assert!(!status(&repo).unwrap().merging);
}

#[test]
fn a_conflicting_merge_waits_for_a_commit_or_an_abort() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    diverged(dir);
    let repo = open(dir);

    // Uncommitted work the merge would overwrite is git's refusal, and nothing is left under way.
    write_file(dir, "f.md", "uncommitted\n");
    let refused = merge(&repo, "side").unwrap_err().to_string();
    assert!(refused.contains("would be overwritten"), "{refused}");
    assert!(!status(&repo).unwrap().merging);
    ok(dir, &["checkout", "--", "f.md"]);

    // git says CONFLICT on stdout, so this is read off the repository and not off its words.
    assert_eq!(
        merge(&repo, "side").unwrap(),
        Merge::Conflicts(vec!["f.md".to_string()])
    );
    assert!(status(&repo).unwrap().merging);
    assert!(merge(&repo, "side").is_err(), "one merge at a time");

    merge_abort(&repo).unwrap();
    assert!(!status(&repo).unwrap().merging);
    assert_eq!(std::fs::read_to_string(dir.join("f.md")).unwrap(), "main\n");

    // Resolved, staged and committed with no message: the merge's own, without its conflict list.
    assert!(matches!(merge(&repo, "side"), Ok(Merge::Conflicts(_))));
    write_file(dir, "f.md", "both\n");
    stage(&repo, &["f.md"]).unwrap();
    commit(&repo, "", false).unwrap();
    let top = &log(&repo, 0, 1).unwrap()[0];
    assert_eq!(top.parents.len(), 2);
    assert_eq!(top.summary, "Merge branch 'side'");
    assert!(!top.body.contains("Conflicts"), "{:?}", top.body);
    assert!(!status(&repo).unwrap().merging);

    // Outside a merge there is no message to fall back on, and git still refuses an empty one.
    write_file(dir, "f.md", "after\n");
    assert!(commit(&repo, "", true).is_err());
}

#[test]
fn sync_moves_a_commit_each_way_through_the_bare_origin() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    init(&source);
    write_file(&source, "a.md", "one\n");
    commit_all(&source, "first");
    ok(tmp.path(), &["clone", "--bare", "-q", "source", "origin"]);
    for clone in ["work", "other"] {
        ok(tmp.path(), &["clone", "-q", "origin", clone]);
        configure(&tmp.path().join(clone));
    }

    // The other clone puts a commit on the origin, which is what our pull has to bring back.
    let other = tmp.path().join("other");
    write_file(&other, "theirs.md", "theirs\n");
    commit_all(&other, "theirs");
    ok(&other, &["push", "-q"]);

    let work = tmp.path().join("work");
    write_file(&work, "mine.md", "mine\n");
    commit_all(&work, "mine");
    let repo = open(&work);

    sync(&repo).unwrap();
    let st = status(&repo).unwrap();
    assert_eq!(
        (st.branch.ahead, st.branch.behind),
        (0, 0),
        "both halves ran"
    );
    assert!(work.join("theirs.md").exists(), "the pull brought theirs");
    ok(&other, &["pull", "-q"]);
    assert!(other.join("mine.md").exists(), "the push sent mine");
}

#[test]
fn a_sync_without_an_upstream_publishes_the_branch() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    init(&source);
    write_file(&source, "a.md", "one\n");
    commit_all(&source, "first");
    ok(tmp.path(), &["clone", "--bare", "-q", "source", "origin"]);
    ok(tmp.path(), &["clone", "-q", "origin", "work"]);

    let work = tmp.path().join("work");
    configure(&work);
    // A branch made locally, which is the shape that has no upstream: pushing it is the whole
    // of a sync there.
    ok(&work, &["checkout", "-q", "-b", "side"]);
    write_file(&work, "b.md", "b\n");
    commit_all(&work, "second");
    let repo = open(&work);
    assert_eq!(status(&repo).unwrap().branch.upstream, None);

    sync(&repo).unwrap();
    let st = status(&repo).unwrap();
    assert_eq!(st.branch.upstream.as_deref(), Some("origin/side"));
    assert_eq!((st.branch.ahead, st.branch.behind), (0, 0));
    // And the next sync is an ordinary one, which is the point of tracking it.
    sync(&repo).unwrap();
}

#[test]
fn publishing_refuses_to_pick_between_remotes() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "first");
    let repo = open(dir);

    assert!(
        matches!(default_remote(&repo), Err(Error::Git(msg)) if msg.contains("no remote")),
        "nowhere to publish to"
    );
    ok(dir, &["remote", "add", "upstream", "../elsewhere"]);
    assert_eq!(default_remote(&repo).unwrap(), "upstream", "the only one");
    ok(dir, &["remote", "add", "fork", "../fork"]);
    assert!(
        matches!(default_remote(&repo), Err(Error::Git(msg)) if msg.contains("fork")),
        "two remotes and no origin is the user's choice, not ours"
    );
    ok(dir, &["remote", "add", "origin", "../origin"]);
    assert_eq!(
        default_remote(&repo).unwrap(),
        "origin",
        "git's own default"
    );
}

#[test]
fn a_repo_without_submodules_lists_none() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    write_file(dir, "a.md", "one\n");
    commit_all(dir, "first");
    assert!(submodules(&open(dir)).unwrap().is_empty());
}

#[test]
fn parse_submodule_reads_state_oid_path_and_describe() {
    let line = " 1234567890abcdef1234567890abcdef12345678 vendor/lib (v1.2-3-gabc)";
    let sub = parse_submodule(line).unwrap();
    assert_eq!(sub.state, ' ');
    assert_eq!(sub.oid, "1234567890abcdef1234567890abcdef12345678");
    assert_eq!(sub.path, "vendor/lib");
    assert_eq!(sub.describe.as_deref(), Some("v1.2-3-gabc"));

    let bare = parse_submodule("-0000000000000000000000000000000000000000 vendor/off").unwrap();
    assert_eq!(bare.state, '-');
    assert_eq!(bare.path, "vendor/off");
    assert_eq!(bare.describe, None);
}
