use super::*;

/// A note outside every vault has no provider to ask, and gets from its text what the notes
/// provider would answer: its headings nested, and the folds of their sections.
#[test]
fn a_note_is_outlined_from_its_text_alone() {
    let (symbols, folds) = note_outline("n.md", "# One\ntext\n## Two\n");
    assert_eq!(symbols[0].name, "One");
    assert_eq!(symbols[0].children[0].name, "Two");
    assert_eq!(
        folds,
        [Fold {
            start_line: 0,
            end_line: 2
        }]
    );
}

#[test]
fn positions_count_chars_not_bytes() {
    let text = "héllo\n[[wörld]]";
    assert_eq!(
        pos_of(text, 7),
        Pos {
            line: 1,
            character: 0
        }
    );
    assert_eq!(
        pos_of(text, 17),
        Pos {
            line: 1,
            character: 9
        }
    );
    assert_eq!(
        byte_of(
            text,
            Pos {
                line: 1,
                character: 9
            }
        ),
        Some(17)
    );
    assert_eq!(
        byte_of(
            text,
            Pos {
                line: 0,
                character: 99
            }
        ),
        Some(6)
    );
    assert_eq!(
        byte_of(
            text,
            Pos {
                line: 5,
                character: 0
            }
        ),
        None
    );
}

/// A vault whose config names `argv` for `language`.
fn configured(language: &str, argv: &[&str]) -> LspConfig {
    LspConfig {
        servers: [(
            language.to_string(),
            argv.iter().map(|a| a.to_string()).collect(),
        )]
        .into_iter()
        .collect(),
    }
}

#[test]
fn a_language_gets_the_server_it_has() {
    // Something certain to be executable on any machine running this test.
    let me = std::env::current_exe().unwrap();
    let me = me.to_str().unwrap();

    let Some(Server::External { language_id, argv }) =
        server(&configured("rust", &[me, "--stdio"]), "rust")
    else {
        panic!("a configured command line that exists is the server to run")
    };
    assert_eq!(language_id, "rust", "the protocol's name for the language");
    assert_eq!(argv, [me, "--stdio"]);

    // A language the table has no row for takes its own id as the protocol's.
    let Some(Server::External { language_id, .. }) = server(&configured("nim", &[me]), "nim")
    else {
        panic!("a configured server answers for any language")
    };
    assert_eq!(language_id, "nim");

    for (header, id) in [("chdr", "c"), ("cpphdr", "cpp")] {
        let Some(Server::External { language_id, .. }) = server(&configured(header, &[me]), header)
        else {
            panic!("{header} has a server")
        };
        assert_eq!(language_id, id, "a header is its language's");
    }
    // JSX is its own language to the protocol, and typescript-language-server's to answer.
    for (jsx, id) in [
        ("typescript-jsx", "typescriptreact"),
        ("jsx", "javascriptreact"),
    ] {
        let Some(Server::External { language_id, .. }) = server(&configured(jsx, &[me]), jsx)
        else {
            panic!("{jsx} has a server")
        };
        assert_eq!(language_id, id);
        assert_eq!(
            server_name(&LspConfig::default(), jsx).as_deref(),
            Some("typescript-language-server")
        );
    }

    assert!(
        matches!(server(&configured("rust", &["/no/such/server"]), "rust"),
            Some(Server::Missing(name)) if name == "/no/such/server"),
        "a named server that is not there is what the UI reports"
    );
    for notes in ["markdown", "org"] {
        assert!(matches!(
            server(&LspConfig::default(), notes),
            Some(Server::Notes)
        ));
    }
    assert!(server(&LspConfig::default(), "brainfuck").is_none());
}

/// What a status line calls the server a document waits on.
#[test]
fn a_server_is_named_by_its_program() {
    let none = LspConfig::default();
    assert_eq!(server_name(&none, "lean").as_deref(), Some("lake"));
    assert_eq!(server_name(&none, "python3"), None, "pyright or pylsp");
    assert_eq!(server_name(&none, "markdown"), None);
    let mine = configured("python3", &["/opt/bin/pylsp", "-v"]);
    assert_eq!(server_name(&mine, "python3").as_deref(), Some("pylsp"));
}

#[test]
fn a_command_under_home_is_looked_for_there() {
    let home = Path::new("/home/u");
    assert_eq!(
        home_relative("~/.elan/bin/lake", home),
        Path::new("/home/u/.elan/bin/lake")
    );
    assert_eq!(home_relative("lake", home), Path::new("lake"));
}

#[test]
fn a_session_is_rooted_at_the_nearest_checkout() {
    let vault = tempfile::tempdir().unwrap();
    let root = vault.path();
    let crate_dir = root.join("proj/src");
    std::fs::create_dir_all(&crate_dir).unwrap();
    assert_eq!(
        session_root(root, &crate_dir.join("main.rs"), &[]),
        root,
        "no checkout anywhere: the vault is the project"
    );

    // A worktree's `.git` is a file, and it counts the same as a directory.
    std::fs::write(root.join("proj/.git"), "gitdir: /elsewhere\n").unwrap();
    assert_eq!(
        session_root(root, &crate_dir.join("main.rs"), &[]),
        root.join("proj")
    );
    assert_eq!(
        session_root(root, &root.join("loose.rs"), &[]),
        root,
        "a file beside the checkout is not inside it"
    );

    // A Lean project below the checkout is rooted at its lakefile, and a dependency's file
    // under its `.lake`, a checkout with a lakefile of its own, at the project.
    let lean = root.join("proj/lean");
    let mathlib = lean.join(".lake/packages/mathlib");
    std::fs::create_dir_all(mathlib.join("Mathlib")).unwrap();
    std::fs::create_dir(mathlib.join(".git")).unwrap();
    std::fs::write(lean.join("lakefile.toml"), "").unwrap();
    std::fs::write(mathlib.join("lakefile.lean"), "").unwrap();
    let markers = ROOTS[0].1;
    assert_eq!(session_root(root, &lean.join("A.lean"), markers), lean);
    assert_eq!(
        session_root(root, &mathlib.join("Mathlib/X.lean"), markers),
        lean
    );
}

/// The notes provider needs no external binary, so it verifies that a session
/// lives exactly while it holds an open document.
#[test]
fn the_last_document_to_close_takes_its_session_with_it() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.md"), "# A\n").unwrap();
    std::fs::write(root.path().join("b.md"), "# B\n").unwrap();
    let (vault, _events) = Local::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();
    // The ghost session is the vault's, not a document's, and it is not counted here.
    vault.set_ghost(false);

    accent_lsp::runtime().block_on(async {
        for rel in ["a.md", "b.md"] {
            vault
                .open_document(rel, "markdown", "# note\n".to_string())
                .await
                .unwrap();
        }
        assert_eq!(locked(&vault.lang.sessions).len(), 1, "one notes provider");

        vault.close_document("a.md").await.unwrap();
        assert_eq!(
            locked(&vault.lang.sessions).len(),
            1,
            "b.md still has the session open"
        );
        vault.close_document("b.md").await.unwrap();
        assert!(
            locked(&vault.lang.sessions).is_empty(),
            "nothing is open, so nothing is kept"
        );
    });
}

/// A server whose definition answer named `/dep/lib.rs`, as rust-analyzer's names a crate
/// in `~/.cargo/registry`, and answers nothing else.
struct Naming {
    stopped: Option<Arc<AtomicBool>>,
    named: String,
}

impl Language for Naming {
    fn open(&self, _: &str, _: &str, _: String) -> Result<Support> {
        Ok(Support::default())
    }
    fn change(&self, _: &str, _: String) -> Result<()> {
        Ok(())
    }
    fn close(&self, _: &str) {}
    fn completion(&self, _: &str, _: Pos, _: Option<char>) -> Fut<'_, Completions> {
        Box::pin(async { Ok(Completions::default()) })
    }
    fn resolve(&self, _: &str, item: Completion) -> Fut<'_, Completion> {
        Box::pin(async { Ok(item) })
    }
    fn signature_help(&self, _: &str, _: Pos) -> Fut<'_, Option<Signature>> {
        Box::pin(async { Ok(None) })
    }
    fn hover(&self, _: &str, _: Pos) -> Fut<'_, Option<Hover>> {
        Box::pin(async { Ok(None) })
    }
    fn definition(&self, _: &str, _: Pos) -> Fut<'_, Vec<Location>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn symbols(&self, _: &str) -> Fut<'_, Vec<Symbol>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn references(&self, _: &str, _: Pos) -> Fut<'_, Vec<Location>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn folds(&self, _: &str) -> Fut<'_, Vec<Fold>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn named(&self, path: &str) -> bool {
        path == self.named
    }
    fn shutdown(&self) -> Fut<'_, ()> {
        Box::pin(async move {
            if let Some(stopped) = &self.stopped {
                stopped.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
    }
}

#[test]
fn a_session_starting_during_shutdown_is_stopped() {
    let (events, _said) = std::sync::mpsc::channel();
    let langs = Languages::new("/vault".into(), "/index.db".into(), events);
    let stopped = Arc::new(AtomicBool::new(false));
    let (began, seen) = std::sync::mpsc::channel();
    let (resume, waiting) = tokio::sync::oneshot::channel::<()>();
    let starting = accent_lsp::runtime().spawn({
        let (langs, stopped) = (langs.clone(), stopped.clone());
        async move {
            langs
                .session(("server".to_string(), "/vault".into()), async move {
                    began.send(()).unwrap();
                    waiting.await.unwrap();
                    Ok(Arc::new(Naming {
                        stopped: Some(stopped),
                        named: "/dep/lib.rs".into(),
                    }) as Arc<dyn Language>)
                })
                .await
        }
    });
    seen.recv_timeout(crate::tests::BUDGET).unwrap();
    langs.shutdown();
    resume.send(()).unwrap();
    let started = accent_lsp::runtime().block_on(async { starting.await.unwrap() });

    assert!(started.is_err(), "a closed vault accepted a new provider");
    assert!(stopped.load(Ordering::SeqCst));
    assert!(locked(&langs.sessions).is_empty());
}

/// A file outside the vault joins the session that named it, holds it while open and starts
/// nothing; one no session named gets no provider.
#[test]
fn a_dependency_joins_the_session_that_named_it() {
    let (events, _said) = std::sync::mpsc::channel();
    let langs = Languages::new("/vault".into(), "/index.db".into(), events);
    let key = ("rust-analyzer".to_string(), PathBuf::from("/vault"));
    let running = tokio::sync::OnceCell::new_with(Some(Arc::new(Naming {
        stopped: None,
        named: "/dep/lib.rs".into(),
    }) as Arc<dyn Language>));
    locked(&langs.sessions).insert(key.clone(), Arc::new(running));
    let open = |rel: &str| {
        let cfg = VaultConfig::default();
        langs.open_document(rel.into(), "rust".into(), String::new(), &cfg)
    };
    accent_lsp::runtime().block_on(async {
        assert!(open("/dep/lib.rs").await.unwrap().dependency);
        assert!(langs.provider("/dep/lib.rs").is_ok());
        assert_eq!(locked(&langs.docs)["/dep/lib.rs"].0, Some(key));

        assert!(!open("/elsewhere/main.rs").await.unwrap().dependency);
        assert!(langs.provider("/elsewhere/main.rs").is_err());
        assert_eq!(locked(&langs.sessions).len(), 1, "nothing was started");

        langs.close_document("/dep/lib.rs".into()).await.unwrap();
        assert!(
            locked(&langs.sessions).is_empty(),
            "the dependency was the last document holding the session"
        );
    });
}

#[test]
fn a_host_read_requires_an_absolute_file_named_by_a_running_session() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let dep = cache.path().join("lib.rs");
    std::fs::write(&dep, "pub fn helper() {}\n").unwrap();
    let (vault, _events) = Local::open_at(
        root.path(),
        &cache.path().join("index.db"),
        VaultConfig::default(),
    )
    .unwrap();
    let key = ("rust-analyzer".to_string(), root.path().to_path_buf());
    let running = tokio::sync::OnceCell::new_with(Some(Arc::new(Naming {
        stopped: None,
        named: dep.to_str().unwrap().into(),
    }) as Arc<dyn Language>));
    locked(&vault.lang.sessions).insert(key, Arc::new(running));

    let read = vault.read_named(dep.to_str().unwrap()).unwrap();
    assert!(
        matches!(read, accent_core::fs::Read::Text(text) if text.text == "pub fn helper() {}\n")
    );
    let wire = crate::vault::dispatch(&vault, "read_named", &json!([dep.to_str().unwrap()]))
        .unwrap()
        .unwrap();
    let read: accent_core::fs::Read = serde_json::from_value(wire).unwrap();
    assert!(
        matches!(read, accent_core::fs::Read::Text(text) if text.text == "pub fn helper() {}\n")
    );
    assert!(
        vault.read_named("lib.rs").is_err(),
        "relative paths are not host files"
    );
    assert!(
        vault.read_named(root.path().to_str().unwrap()).is_err(),
        "the session did not name this path"
    );
    locked(&vault.lang.sessions).clear();
    assert!(
        vault.read_named(dep.to_str().unwrap()).is_err(),
        "a closed session no longer authorizes a host read"
    );
}

/// Verify longer startup waits until the server first answers, then stop it
/// after `idle` without requests. The fake server models project loading.
#[test]
fn a_server_started_for_calls_is_waited_for_then_stopped_when_idle() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("lib.rs"),
        "fn helper() {}\n\nfn main() {\n    helper();\n}\n",
    )
    .unwrap();
    let calls = cache.path().join("calls.json");
    let called = r#"{"lib.rs:0": {"incoming": [["lib.rs", 2, "main", [3]]]}}"#;
    std::fs::write(&calls, called).unwrap();
    let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/../../build-aux/fake-lsp.py");
    let calls = calls.to_str().unwrap();
    let argv = ["python3", fake, "--calls", calls, "--loading", "4"];
    let cfg = crate::VaultConfig {
        lsp: configured("rust", &argv),
        ..Default::default()
    };
    let (vault, _events) =
        Local::open_at(root.path(), &cache.path().join("index.db"), cfg).unwrap();
    let waits = Waits {
        starting: Duration::from_secs(5),
        asking: Duration::from_millis(500),
        idle: Duration::from_secs(2),
    };

    // Refused four times, a second at `AGAIN` apiece.
    let at = Pos {
        line: 0,
        character: 3,
    };
    let asked = vault
        .lang
        .calls("lib.rs".to_string(), at, true, &vault.config().lsp, waits);
    let found = accent_lsp::runtime().block_on(asked).unwrap().unwrap();
    assert_eq!(
        (found[0].name.as_str(), found[0].lines.as_slice()),
        ("main", &[3][..])
    );
    let session = locked(&vault.lang.sessions)
        .values()
        .find_map(|cell| cell.get().cloned())
        .expect("the server is kept");

    std::thread::sleep(Duration::from_millis(2_500));
    assert!(locked(&vault.lang.sessions).is_empty(), "idle, so stopped");
    let stopped = Instant::now() + Duration::from_secs(3);
    while !session.is_dead() && Instant::now() < stopped {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(session.is_dead(), "the server exited");
}

/// A note created after a link to it was flagged: the hint goes when the walk that found it
/// reports, not at the next keystroke.
#[test]
fn an_indexed_note_clears_the_link_that_was_waiting_for_it() {
    let indexed = |e: &Event| matches!(e, Event::DirsChanged(_) | Event::Reconciled(_));
    let f = crate::tests::Fixture::open(crate::VaultConfig::default());
    f.write("a.md", "[[Beta]]\n");
    // Drained here, so that the wait for Beta's walk below cannot match this one's.
    assert!(f.wait(indexed).is_some(), "a.md reaches the index");
    let rt = accent_lsp::runtime();
    rt.block_on(async {
        f.vault
            .open_document("a.md", "markdown", "[[Beta]]\n".to_string())
            .await
            .unwrap();
    });
    assert!(
        f.wait(
            |e| matches!(e, Event::Diagnostics { rel, items } if rel == "a.md" && items.len() == 1)
        )
        .is_some(),
        "the link is dangling while Beta does not exist"
    );

    f.write("Beta.md", "# Beta\n");
    assert!(f.wait(indexed).is_some(), "the new note reaches the index");
    rt.block_on(async { f.vault.rediagnose("a.md").await.unwrap() });
    assert!(
        f.wait(
            |e| matches!(e, Event::Diagnostics { rel, items } if rel == "a.md" && items.is_empty())
        )
        .is_some(),
        "the link resolves now, so the hint goes"
    );
}

#[test]
fn a_diagnostics_event_survives_the_wire() {
    let event = Event::Diagnostics {
        rel: "a.md".into(),
        items: vec![Diagnostic {
            range: Range::default(),
            severity: Severity::Hint,
            message: "No note named b".into(),
            source: Some("accent".into()),
            code: None,
        }],
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: Event = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, Event::Diagnostics { items, .. } if items.len() == 1));
}

/// Ghost Text off ends `merl-rt` under an open note, and on starts it again, re-indexed,
/// without the note being opened again.
#[test]
fn ghost_text_off_ends_merl_and_on_starts_it_again() {
    if !in_path(GHOST) {
        eprintln!("merl-rt is not installed: skipping the ghost on/off test");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let seen = "The kettle was already boiling.\n";
    std::fs::write(root.path().join("a.md"), format!("{seen}{seen}")).unwrap();
    let (vault, _events) = Local::open_at(
        root.path(),
        &cache.path().join("index.db"),
        crate::VaultConfig::default(),
    )
    .unwrap();
    let typed = "The kettle was";
    let at = Pos {
        line: 0,
        character: typed.chars().count() as u32,
    };
    let key = (GHOST.to_string(), vault.lang.root.clone());
    let running = || {
        locked(&vault.lang.sessions)
            .get(&key)
            .and_then(|cell| cell.get().cloned())
    };
    accent_lsp::runtime().block_on(async {
        let support = vault
            .open_document("b.md", "markdown", typed.to_string())
            .await
            .unwrap();
        assert!(support.inline);
        let suggest = || async { vault.inline_completion("b.md", at).await.unwrap() };
        let mut first = None;
        for _ in 0..100 {
            first = suggest().await;
            if first.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(first, Some(" already boiling.".to_string()));
        let merl = running().expect("merl runs");

        vault.set_ghost(false);
        for _ in 0..100 {
            if merl.is_dead() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(merl.is_dead(), "merl-rt has exited");
        assert!(running().is_none());
        assert_eq!(suggest().await, None, "nothing while it is off");

        vault.set_ghost(true);
        let mut got = None;
        for _ in 0..100 {
            got = suggest().await;
            if got.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            got,
            Some(" already boiling.".to_string()),
            "a fresh merl answers"
        );
        assert!(running().is_some_and(|fresh| !Arc::ptr_eq(&fresh, &merl)));
    });
}

/// Retry spaced-out exits; give up after a burst and report it once.
#[test]
fn a_ghost_session_that_keeps_exiting_is_given_up_on_once() {
    let (events, said) = std::sync::mpsc::channel();
    let langs = Languages::new("/vault".into(), "/index.db".into(), events);
    let t0 = Instant::now();
    let at = |s| t0 + Duration::from_secs(s);
    for s in [0, 30, 50, 61] {
        assert!(langs.ghost_may_start(at(s)), "start at {s} s");
    }
    assert!(
        !langs.ghost_may_start(at(70)),
        "a fourth start within the minute"
    );
    assert!(!langs.ghost_may_start(at(500)), "given up for good");
    let said: Vec<String> = said
        .try_iter()
        .filter_map(|e| match e {
            Event::Error(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(said, ["Ghost text stopped: merl-rt keeps exiting"]);
}
