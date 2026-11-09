//! The `external` module's tests.

use super::*;
use crate::language::Severity;
use serde_json::json;

/// Held by the tests that start a Python server, so one runs at a time: two jedis filling a
/// cold cache together read each other's half-written pickles, which pylsp answers with no
/// completions (parso, ISSUES.md).
static PYTHON: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn an_old_ghost_session_cannot_clear_its_successor() {
    let current = Arc::new(Mutex::new(1));
    let docs: Docs = Arc::new(Mutex::new(HashMap::new()));
    let (events, seen) = std::sync::mpsc::channel();
    let progress = || accent_lsp::Notification {
        method: "$/progress".to_string(),
        params: json!({"value": {"kind": "begin", "title": "Indexing"}}),
    };
    let start = |generation, events| {
        let (sender, notifications) = tokio::sync::mpsc::unbounded_channel();
        let busy = Busy {
            what: "suggestions",
            generation,
            current: current.clone(),
        };
        let docs = docs.clone();
        let task = accent_lsp::runtime().spawn(forward_notifications(
            notifications,
            docs,
            None,
            PathBuf::from("/vault"),
            Encoding::Utf16,
            Some(busy),
            events,
        ));
        (sender, task)
    };

    let (old, old_task) = start(1, events.clone());
    old.send(progress()).unwrap();
    assert!(matches!(
        seen.recv().unwrap(),
        Event::Busy { busy: true, .. }
    ));
    *locked(&current) = 2;
    let (new, new_task) = start(2, events);
    new.send(progress()).unwrap();
    assert!(matches!(
        seen.recv().unwrap(),
        Event::Busy { busy: true, .. }
    ));

    drop(old);
    accent_lsp::runtime().block_on(old_task).unwrap();
    assert!(
        seen.try_recv().is_err(),
        "old exit must not clear new indexing"
    );
    drop(new);
    accent_lsp::runtime().block_on(new_task).unwrap();
    assert!(matches!(
        seen.recv().unwrap(),
        Event::Busy { busy: false, .. }
    ));
}

/// merl's `$/progress`, as `merl.c` sends it: busy from `begin` through every `report`, with
/// the report's own words, and done at `end`.
#[test]
fn progress_says_busy_and_how_far() {
    let value = |v: Value| json!({"token": "merl/index", "value": v});
    assert_eq!(
        progress_of(&value(json!({"kind": "begin", "title": "Indexing"}))),
        Some((true, None))
    );
    assert_eq!(
        progress_of(&value(json!({"kind": "report", "message": "1200 files"}))),
        Some((true, Some("1200 files".to_string())))
    );
    assert_eq!(
        progress_of(&value(json!({"kind": "end"}))),
        Some((false, None))
    );
    assert_eq!(progress_of(&value(json!({"kind": "other"}))), None);
}

/// Lean's `$/lean/fileProgress`: the lines still being checked, a range ending at the start
/// of a line leaving it out, ranges that overlap or touch made one, and nothing once all that
/// is left is a fatal error or no range at all.
#[test]
fn lean_says_which_lines_it_is_still_checking() {
    let progress = |processing: Value| {
        json!({"textDocument": {"uri": "file:///v/A.lean", "version": 1},
               "processing": processing})
    };
    let range = |from: u32, to: u32, character: u32| {
        json!({"start": {"line": from, "character": 0},
               "end": {"line": to, "character": character}})
    };
    // Lean 4.34 on a file of fourteen lines: declarations in flight on 5–8, 10 and 12–13.
    assert_eq!(
        checking_of(&progress(json!([
            {"range": range(12, 14, 0), "kind": 1},
            {"range": range(5, 5, 30), "kind": 1},
            {"range": range(5, 5, 30), "kind": 1},
            {"range": range(6, 6, 30)},
            {"range": range(7, 8, 30), "kind": 1},
            {"range": range(10, 10, 9), "kind": 1},
            {"range": range(2, 3, 0), "kind": 2},
        ]))),
        Some(vec![5..9, 10..11, 12..14])
    );
    assert_eq!(checking_of(&progress(json!([]))), Some(vec![]));
    assert_eq!(
        checking_of(&progress(json!([{"range": range(4, 9, 0), "kind": 2}]))),
        Some(vec![])
    );
}

/// A check's first state goes on at once; one said within the interval after waits for its
/// end, and what goes on then is the last state said.
#[test]
fn a_check_goes_on_at_most_once_an_interval() {
    let start = Instant::now();
    let mut check = Check {
        lines: vec![3..4, 6..20],
        sent: None,
    };
    assert_eq!(check.due(start), Some(vec![3..4, 6..20]));
    assert_eq!(check.held(), None);
    check.lines = vec![6..7, 8..20];
    assert_eq!(check.due(start + CHECKING_EVERY / 2), None);
    check.lines = vec![8..9, 10..20];
    assert_eq!(check.held(), Some(start + CHECKING_EVERY));
    assert_eq!(check.due(start + CHECKING_EVERY), Some(vec![8..9, 10..20]));
    assert_eq!(check.held(), None);
    assert_eq!(check.due(start + 3 * CHECKING_EVERY), None, "nothing new");
}

/// Clear gutter diagnostics when the notification stream closes after a
/// server exit. Dropping the sender models this without a server process.
#[test]
fn a_server_that_exits_clears_what_it_painted() {
    let (server, notifications) = tokio::sync::mpsc::unbounded_channel();
    let (events, seen) = std::sync::mpsc::channel();
    let text = "int main(void) { return addd(3); }\n";
    let docs: Docs = Arc::new(Mutex::new(HashMap::from([(
        "a.c".to_string(),
        Doc {
            version: 1,
            text: text.to_string(),
        },
    )])));
    server
        .send(accent_lsp::Notification {
            method: "textDocument/publishDiagnostics".to_string(),
            params: json!({
                "uri": "file:///vault/a.c",
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 24},
                              "end": {"line": 0, "character": 28}},
                    "severity": 1,
                    "message": "addd is not declared",
                }],
            }),
        })
        .unwrap();
    drop(server);

    accent_lsp::runtime().block_on(forward_notifications(
        notifications,
        docs,
        None,
        PathBuf::from("/vault"),
        Encoding::Utf16,
        None,
        events,
    ));

    let painted = seen.recv().expect("the diagnostic the server published");
    assert!(
        matches!(painted, Event::Diagnostics { ref rel, ref items, .. }
        if rel == "a.c" && items.len() == 1)
    );
    let cleared = seen.recv().expect("the server's exit clears its marks");
    assert!(
        matches!(cleared, Event::Diagnostics { ref rel, ref items, .. }
        if rel == "a.c" && items.is_empty())
    );
}

/// A publish the server says is for an older text than the one it was sent last is stale; one
/// for that text, or one that does not say, is not.
#[test]
fn a_publish_for_an_older_text_is_stale() {
    let (server, notifications) = tokio::sync::mpsc::unbounded_channel();
    let (events, seen) = std::sync::mpsc::channel();
    let docs: Docs = Arc::new(Mutex::new(HashMap::from([(
        "a.c".to_string(),
        Doc {
            version: 2,
            ..Doc::default()
        },
    )])));
    for version in [json!(1), json!(2), json!(null)] {
        let params = json!({"uri": "file:///vault/a.c", "version": version, "diagnostics": []});
        let method = "textDocument/publishDiagnostics".to_string();
        server
            .send(accent_lsp::Notification { method, params })
            .unwrap();
    }
    drop(server);

    accent_lsp::runtime().block_on(forward_notifications(
        notifications,
        docs,
        None,
        PathBuf::from("/vault"),
        Encoding::Utf16,
        None,
        events,
    ));

    let stale: Vec<bool> = seen
        .try_iter()
        .map(|e| matches!(e, Event::Diagnostics { stale: true, .. }))
        .collect();
    assert_eq!(stale, [true, false, false]);
}

// ------------------------------------------------------------ with a real server

/// A definition to jump to, a call that resolves, and a call that does not.
const C_FILE: &str =
    "int add(int a, int b) { return a + b; }\nint main(void) { return add(1, 2) + addd(3); }\n";

/// The clangd processes running right now, so the test can tell its own from everyone else's.
fn clangd_pids() -> std::collections::BTreeSet<String> {
    std::process::Command::new("pgrep")
        .args(["-x", "clangd"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The one test with a language server actually running. Skipped where clangd is not
/// installed, so a checkout without it still has a green suite.
#[test]
fn clangd_answers_about_a_c_file_and_leaves_nothing_behind() {
    if !super::super::in_path("clangd") {
        eprintln!("clangd is not installed: skipping the end-to-end test");
        return;
    }
    let before = clangd_pids();
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.c"), C_FILE).unwrap();
    let (vault, events) = crate::Vault::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();

    let rt = accent_lsp::runtime();
    rt.block_on(async {
        let support = vault
            .open_document("main.c", "c", C_FILE.to_string())
            .await
            .unwrap();
        assert_eq!(support.missing, None, "clangd is on PATH");
        assert!(support.completion_triggers.contains(&'.'));

        let names: Vec<String> = vault
            .symbols("main.c")
            .await
            .unwrap()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(names.contains(&"add".to_string()) && names.contains(&"main".to_string()));

        // The caret inside the `add(1, 2)` call on the second line.
        let call = C_FILE.lines().nth(1).unwrap().find("add(1").unwrap();
        let at = Pos {
            line: 1,
            character: call as u32 + 1,
        };
        let to = vault.definition("main.c", at).await.unwrap();
        assert_eq!(to.len(), 1, "one definition of add");
        assert_eq!(to[0].path, "main.c");
        assert_eq!(to[0].range.start.line, 0);
    });

    // Diagnostics arrive when clangd has built its preamble, not when it is asked.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut error = None;
    while error.is_none() && std::time::Instant::now() < deadline {
        let Ok(event) = events.recv_timeout(std::time::Duration::from_millis(500)) else {
            continue;
        };
        if let Event::Diagnostics { rel, items, .. } = event
            && rel == "main.c"
        {
            error = items.into_iter().find(|d| d.severity == Severity::Error);
        }
    }
    let error = error.expect("clangd has to complain about the call to addd");
    assert_eq!(error.range.start.line, 1);
    assert!(error.message.contains("addd"), "{}", error.message);

    // The last document to close takes the session with it: a server started for one file
    // must not sit on a vault nobody is reading code in any more.
    rt.block_on(async { vault.close_document("main.c").await.unwrap() });
    assert!(
        clangd_left(&before).is_empty(),
        "clangd outlived the only document it was started for"
    );
    // And reopening starts a fresh one, rather than asking the one that has exited.
    rt.block_on(async {
        let support = vault
            .open_document("main.c", "c", C_FILE.to_string())
            .await
            .unwrap();
        assert!(support.completion_triggers.contains(&'.'), "a new session");
    });

    // Dropping the vault stops the server: nothing of ours may outlive the window.
    drop(vault);
    assert!(
        clangd_left(&before).is_empty(),
        "clangd still running after the vault closed"
    );
}

/// The clangd processes this test started that are still running, given a little while to
/// exit: a server is asked to stop and stops on its own thread.
fn clangd_left(before: &std::collections::BTreeSet<String>) -> Vec<String> {
    let mut left = Vec::new();
    for _ in 0..30 {
        left = clangd_pids().difference(before).cloned().collect();
        if left.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    left
}

/// clangd's two kinds of code action: a fix-it, offered only for the diagnostic it was
/// published with, and a tweak, a command whose edit comes back through `applyEdit`.
#[test]
fn clangd_fixes_and_tweaks_come_back_as_edits() {
    if !super::super::in_path("clangd") {
        eprintln!("clangd is not installed: skipping the end-to-end test");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let broken = "int main(void) {\n    int x = 1\n    return x;\n}\n";
    let tweak = "int f(int a) {\n    return a * 3;\n}\n";
    std::fs::write(root.path().join("fix.c"), broken).unwrap();
    std::fs::write(root.path().join("tweak.c"), tweak).unwrap();
    let (vault, events) = crate::Vault::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();
    let rt = accent_lsp::runtime();
    rt.block_on(vault.open_document("fix.c", "c", broken.to_string()))
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut error = None;
    while error.is_none() && std::time::Instant::now() < deadline {
        if let Ok(Event::Diagnostics { rel, items, .. }) =
            events.recv_timeout(std::time::Duration::from_millis(500))
            && rel == "fix.c"
        {
            error = items.into_iter().find(|d| d.severity == Severity::Error);
        }
    }
    let error = error.expect("clangd has to miss the ';'");
    let fixes = rt
        .block_on(vault.code_actions("fix.c", error.range, vec![error.clone()], false))
        .unwrap();
    let fix = fixes
        .into_iter()
        .find(|a| a.preferred)
        .expect("a preferred fix");
    assert_eq!(fix.kind, "quickfix");
    let edits = rt.block_on(vault.code_action_edits("fix.c", fix)).unwrap();
    let at = Pos {
        line: 1,
        character: 13,
    };
    let insert = super::super::TextEdit {
        range: super::super::Range { start: at, end: at },
        text: ";".to_string(),
    };
    assert_eq!(edits.open, [("fix.c".to_string(), vec![insert])]);

    rt.block_on(vault.open_document("tweak.c", "c", tweak.to_string()))
        .unwrap();
    let star = Pos {
        line: 1,
        character: 13,
    };
    let range = super::super::Range {
        start: star,
        end: star,
    };
    let swap = rt
        .block_on(vault.code_actions("tweak.c", range, Vec::new(), false))
        .unwrap()
        .into_iter()
        .find(|a| a.raw["command"]["command"] == "clangd.applyTweak")
        .expect("a tweak at the operator");
    let edits = rt
        .block_on(vault.code_action_edits("tweak.c", swap))
        .unwrap();
    let (rel, swapped) = &edits.open[0];
    assert_eq!(rel, "tweak.c");
    assert!(swapped.iter().any(|e| e.text == "a"), "{swapped:?}");
}

/// pylsp is the fallback of the Python row; pyright is tried first where it is installed.
/// Completion is what the bare install answers (jedi is a hard dependency, the linters are
/// extras), so that is what proves the session rather than a diagnostic.
#[test]
fn pylsp_answers_about_a_python_file() {
    if super::super::in_path("pyright-langserver") || !super::super::in_path("pylsp") {
        eprintln!("pylsp is not the Python server here: skipping the end-to-end test");
        return;
    }
    let _python = PYTHON.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let py = "import os\nos.\n";
    std::fs::write(root.path().join("tool.py"), py).unwrap();
    let (vault, _events) = crate::Vault::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();
    accent_lsp::runtime().block_on(async {
        let support = vault
            .open_document("tool.py", "python3", py.to_string())
            .await
            .unwrap();
        assert_eq!(support.missing, None, "pylsp is on PATH");
        assert!(support.completion_triggers.contains(&'.'));
        let after_dot = Pos {
            line: 1,
            character: 3,
        };
        let items = vault
            .completion("tool.py", after_dot, Some('.'))
            .await
            .unwrap()
            .items;
        assert!(
            items.iter().any(|c| c.label == "path"),
            "os. should offer os.path, got {} items",
            items.len()
        );
    });
    drop(vault);
}

/// A notebook's program is a document with no file behind it (`<path>.py` for an `.ipynb`):
/// the server answers from the text it was given.
#[test]
fn a_document_no_file_stands_behind_is_answered() {
    if !super::super::in_path("pyright-langserver") && !super::super::in_path("pylsp") {
        eprintln!("no Python server here: skipping the end-to-end test");
        return;
    }
    let _python = PYTHON.lock().unwrap_or_else(|e| e.into_inner());
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let (vault, _events) = crate::Vault::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();
    accent_lsp::runtime().block_on(async {
        let rel = "nb.ipynb.py";
        let program = "import os\nos.\n".to_string();
        let support = vault.open_document(rel, "python3", program).await.unwrap();
        assert_eq!(support.missing, None);
        let after_dot = Pos {
            line: 1,
            character: 3,
        };
        let items = vault
            .completion(rel, after_dot, Some('.'))
            .await
            .unwrap()
            .items;
        assert!(
            items.iter().any(|c| c.label == "path"),
            "{} items",
            items.len()
        );
    });
    assert!(!root.path().join("nb.ipynb.py").exists());
    drop(vault);
}

/// The goals of a core-only proof, from `lean --server` as a vault would configure it: the
/// hypotheses and target inside the proof, none at its end. Skipped where elan has not
/// installed Lean.
#[test]
fn lean_answers_the_goals_of_a_proof() {
    let lean = "~/.elan/bin/lean";
    let home = std::env::var_os("HOME").unwrap_or_default();
    if !super::super::home_relative(lean, Path::new(&home)).is_file() {
        eprintln!("lean is not installed: skipping the end-to-end test");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let proof = "theorem t (p : Prop) (h : p) : p := by\n  exact h\n\n\
                 example (n : Nat) : n ≤ n := by\n  exact?";
    std::fs::write(root.path().join("T.lean"), proof).unwrap();
    let config = crate::VaultConfig {
        lsp: crate::LspConfig {
            servers: [(
                "lean".to_string(),
                vec![lean.to_string(), "--server".to_string()],
            )]
            .into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let (vault, events) =
        crate::Vault::open_at(root.path(), &cache.path().join("index.db"), config).unwrap();
    let rt = accent_lsp::runtime();
    let support = rt
        .block_on(vault.open_document("T.lean", "lean", proof.to_string()))
        .unwrap();
    assert!(support.goals, "Lean answers goals");
    let checked = |e: &Event| match e {
        Event::Checking { rel, lines } => rel == "T.lean" && lines.is_empty(),
        _ => false,
    };
    let budget = std::time::Duration::from_secs(60);
    assert!(crate::tests::wait_for(&events, checked, budget).is_some());

    let at = |line, character| {
        let goals = rt.block_on(vault.goals("T.lean", Pos { line, character }));
        goals.unwrap().expect("Lean answers")
    };
    assert_eq!(
        at(1, 2).tactic,
        Some(vec!["p : Prop\nh : p\n⊢ p".to_string()])
    );
    let end = at(1, 9);
    assert_eq!(end.tactic, Some(vec![]), "the proof is finished");
    assert_eq!(
        end.expected.as_deref(),
        Some("p : Prop\nh : p\n⊢ p"),
        "`h` is a `p`"
    );

    // `exact?`'s "Try this" is a code action replacing the tactic.
    let tactic = Pos {
        line: 4,
        character: 4,
    };
    let range = super::super::Range {
        start: tactic,
        end: tactic,
    };
    let actions = rt
        .block_on(vault.code_actions("T.lean", range, Vec::new(), false))
        .unwrap();
    let try_this = actions
        .into_iter()
        .find(|a| a.title.starts_with("Try this: exact"))
        .expect("Lean offers what exact? found");
    let edits = rt
        .block_on(vault.code_action_edits("T.lean", try_this))
        .unwrap();
    let (rel, replaced) = &edits.open[0];
    assert_eq!(rel, "T.lean");
    assert_eq!(
        replaced[0].range.start,
        Pos {
            line: 4,
            character: 2
        }
    );
    assert!(replaced[0].text.starts_with("exact "), "{replaced:?}");
}

/// Verify texlab supplies `\ref` and `\cite` completion without
/// additional accent-side implementation.
#[test]
fn texlab_completes_labels_and_citations() {
    if !super::super::in_path("texlab") {
        eprintln!("texlab is not installed: skipping the end-to-end test");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let tex = "\\documentclass{article}\n\\begin{document}\n\\label{fig:abc}\n\\ref{fig:}\n\\cite{}\n\\ci\n\\bibliography{refs}\n\\input{}\n\\input{fig/}\n\\cite{knuth}\n\\end{document}\n";
    std::fs::write(root.path().join("main.tex"), tex).unwrap();
    std::fs::create_dir(root.path().join("fig")).unwrap();
    for name in ["a.tex", "plot.pgf"] {
        std::fs::write(root.path().join("fig").join(name), "").unwrap();
    }
    std::fs::write(
        root.path().join("refs.bib"),
        "@article{knuth84, author = {Knuth}, title = {Literate Programming}, year = 1984}\n",
    )
    .unwrap();
    let (vault, events) = crate::Vault::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();
    // `\input{` lists the folder from the index.
    let reconciled = |e: &crate::Event| matches!(e, crate::Event::Reconciled(_));
    assert!(crate::tests::wait_for(&events, reconciled, crate::tests::BUDGET).is_some());
    accent_lsp::runtime().block_on(async {
        let support = vault
            .open_document("main.tex", "latex", tex.to_string())
            .await
            .unwrap();
        assert_eq!(support.missing, None, "texlab is on PATH");
        let labels: Vec<String> = vault
            .completion(
                "main.tex",
                Pos {
                    line: 3,
                    character: 9,
                },
                None,
            )
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert!(labels.iter().any(|l| l == "fig:abc"), "labels: {labels:?}");
        let keys: Vec<String> = vault
            .completion(
                "main.tex",
                Pos {
                    line: 4,
                    character: 6,
                },
                None,
            )
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert!(keys.iter().any(|k| k == "knuth84"), "citations: {keys:?}");
        // Asked inside a key, the fragment is all of it: `\cite{knu|th}`.
        let inside = vault
            .completion(
                "main.tex",
                Pos {
                    line: 9,
                    character: 9,
                },
                None,
            )
            .await
            .unwrap();
        let span = inside
            .fragment
            .map(|f| (f.start.character, f.end.character));
        assert_eq!(span, Some((6, 11)), "{:?}", inside.items.first());
        // A command: texlab caps its answer at 50 and says so, which is what makes the
        // popup ask again as the name grows instead of narrowing a list that lacks `cite`.
        let after_backslash = vault
            .completion(
                "main.tex",
                Pos {
                    line: 5,
                    character: 1,
                },
                Some('\\'),
            )
            .await
            .unwrap();
        assert!(after_backslash.incomplete, "texlab caps the command list");
        let commands = vault
            .completion(
                "main.tex",
                Pos {
                    line: 5,
                    character: 3,
                },
                None,
            )
            .await
            .unwrap();
        assert!(
            commands.items.iter().any(|c| c.label == "cite"),
            "\\ci offers cite"
        );
        // texlab's `.tex` files and folders, and the other files beside them, once each.
        let input = |line, character| {
            let pos = Pos { line, character };
            let answer = vault.completion("main.tex", pos, Some('{'));
            async move {
                let mut labels: Vec<String> = answer
                    .await
                    .unwrap()
                    .items
                    .into_iter()
                    .map(|c| c.label)
                    .collect();
                labels.sort();
                labels
            }
        };
        assert_eq!(input(7, 7).await, ["fig", "main.tex", "refs.bib"]);
        assert_eq!(input(8, 11).await, ["a.tex", "plot.pgf"]);
    });
    drop(vault);
}

/// Verify rust-analyzer rename edits for same-folder modules and imports.
/// Cross-folder moves return no edits, reflecting the server's current limit.
#[test]
fn rust_analyzer_follows_a_renamed_module() {
    if !super::super::in_path("rust-analyzer") {
        eprintln!("rust-analyzer is not installed: skipping the end-to-end test");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let lib = "mod foo;\npub use foo::f;\n";
    std::fs::write(root.path().join("src/lib.rs"), lib).unwrap();
    std::fs::write(root.path().join("src/foo.rs"), "pub fn f() {}\n").unwrap();

    let (vault, events) = crate::Vault::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();
    let budget = std::time::Duration::from_secs(60);
    assert!(
        crate::tests::wait_for(&events, |e| matches!(e, Event::Reconciled(_)), budget).is_some()
    );
    accent_lsp::runtime().block_on(async {
        vault
            .open_document("src/lib.rs", "rust", lib.to_string())
            .await
            .unwrap();
    });
    // Until the crate graph has loaded the server knows no module to rename.
    let moves = [("src/foo.rs".to_string(), "src/bar.rs".to_string())];
    let started = std::time::Instant::now();
    let mut plan = vault.plan_moves(&moves).unwrap();
    while plan.imports.is_empty() && started.elapsed() < budget {
        std::thread::sleep(std::time::Duration::from_millis(500));
        plan = vault.plan_moves(&moves).unwrap();
    }
    eprintln!("willRenameFiles answered after {:?}", started.elapsed());
    assert!(plan.unchecked.is_empty(), "rust-analyzer was asked");
    assert_eq!(plan.imports.len(), 1, "{:?}", plan.imports);

    let away = [("src/foo.rs".to_string(), "src/sub/foo.rs".to_string())];
    let away = vault.plan_moves(&away).unwrap();
    assert!(away.imports.is_empty(), "{:?}", away.imports);
    assert!(
        away.unchecked.is_empty(),
        "asked, and answered with nothing"
    );

    let report = vault.rename(&plan, true).unwrap();
    assert_eq!(report.rewritten, ["src/lib.rs"]);
    assert_eq!(
        std::fs::read_to_string(root.path().join("src/lib.rs")).unwrap(),
        "mod bar;\npub use bar::f;\n"
    );
    drop(vault);
}

#[test]
fn rust_analyzer_answers_about_a_crate() {
    if !super::super::in_path("rust-analyzer") {
        eprintln!("rust-analyzer is not installed: skipping the end-to-end test");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let main = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() { let s: i32 = \"no\"; println!(\"{}\", add(1, 2)); }\n";
    std::fs::write(root.path().join("src/main.rs"), main).unwrap();

    let (vault, events) = crate::Vault::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();
    accent_lsp::runtime().block_on(async {
        let support = vault
            .open_document("src/main.rs", "rust", main.to_string())
            .await
            .unwrap();
        assert_eq!(support.missing, None);
        // Symbols are answered from the syntax tree, so they do not wait for the crate graph.
        let names: Vec<String> = vault
            .symbols("src/main.rs")
            .await
            .unwrap()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, ["add", "main"]);
        // The compiler's diagnostics come from `cargo check`, which rust-analyzer runs on a
        // save and on nothing else: the whole reason the save is a notification of its own.
        vault.save_document("src/main.rs").await.unwrap();
    });
    // rust-analyzer's own type check reports first; the compiler's answer is the one whose
    // source is rustc, and it is the one that proves the save was heard.
    let started = std::time::Instant::now();
    let mut errors = Vec::new();
    while started.elapsed() < std::time::Duration::from_secs(60) && errors.is_empty() {
        let Ok(event) = events.recv_timeout(std::time::Duration::from_millis(500)) else {
            continue;
        };
        if let crate::Event::Diagnostics { rel, items, .. } = event
            && rel == "src/main.rs"
        {
            errors = items
                .into_iter()
                .filter(|d| d.source.as_deref() == Some("rustc"))
                .collect();
        }
    }
    eprintln!("cargo check diagnostics after {:?}", started.elapsed());
    assert!(
        errors
            .iter()
            .any(|d| d.message.contains("mismatched types")),
        "expected the type error from cargo check, got {errors:?}"
    );
    drop(vault);
}
