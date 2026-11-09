//! The `rpc` module's tests.

use super::*;
use crate::{Etag, Session, VaultConfig};
use accent_core::Error as Core;
use std::time::{Duration, Instant};

/// Join a client and server with real pipes, as SSH does, to test the protocol
/// end to end without a network.
struct Wired {
    client: Client,
    server: Option<std::thread::JoinHandle<()>>,
    _events: Receiver<Event>,
    _root: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

impl Wired {
    fn open() -> Wired {
        Wired::seeded(&|root| {
            std::fs::write(root.join("a.md"), "hello [[b]]\n").unwrap();
            std::fs::write(root.join("b.md"), "#tag\n").unwrap();
        })
    }

    /// [`open`](Self::open) over a vault the caller fills, for a test that needs a walk still
    /// running when the first request lands.
    fn seeded(write: &dyn Fn(&std::path::Path)) -> Wired {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        write(root.path());

        let (vault, vault_events) = Local::open_at(
            root.path(),
            &cache.path().join("index.db"),
            VaultConfig::default(),
        )
        .unwrap();

        // to_server: client writes, server reads. to_client: the other way round.
        let (server_in, client_out) = std::io::pipe().unwrap();
        let (client_in, server_out) = std::io::pipe().unwrap();
        let server = std::thread::spawn(move || {
            serve_local(vault, vault_events, server_in, server_out, SILENCE);
        });

        let (events, event_rx) = channel();
        let client = Client::new(
            Box::new(client_out),
            Box::new(client_in),
            events,
            Box::new(|| {}),
        );
        let hello: Hello = client
            .call("hello", json!([VaultConfig::default(), true, true]))
            .unwrap();
        assert_eq!(hello.version, env!("CARGO_PKG_VERSION"));

        Wired {
            client,
            server: Some(server),
            _events: event_rx,
            _root: root,
            _cache: cache,
        }
    }

    /// Drain until `f` is happy, or give up. The reconcile is asynchronous, so a test that
    /// asks about the index has to wait for it the way the UI does.
    fn wait(&self, f: impl Fn(&Event) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match self._events.recv_timeout(Duration::from_millis(200)) {
                Ok(e) if f(&e) => return true,
                Ok(_) => {}
                Err(_) => {}
            }
        }
        false
    }
}

impl Drop for Wired {
    fn drop(&mut self) {
        self.client.shutdown();
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

#[test]
fn copy_page_rpc_caps_each_directory_reply() {
    let w = Wired::seeded(&|root| {
        std::fs::create_dir(root.join("F")).unwrap();
        for n in 0..5 {
            std::fs::write(root.join(format!("F/{n}.md")), "x").unwrap();
        }
    });
    let mut after: Option<Vec<u8>> = None;
    let mut names = Vec::new();
    loop {
        let page: crate::vault::CopyPage = w
            .client
            .call("copy_page", json!(["F", "", after, 2]))
            .unwrap();
        let crate::vault::CopyPage::Directory { entries, next } = page else {
            panic!("directory page expected");
        };
        assert!(entries.len() <= 2);
        names.extend(entries.into_iter().map(|(name, _)| name));
        match next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(names, ["0.md", "1.md", "2.md", "3.md", "4.md"]);
}

#[test]
fn a_call_crosses_the_wire_and_comes_back_typed() {
    let w = Wired::open();
    assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

    let rows: Vec<crate::FileRow> = w.client.call("list_dir", json!([""])).unwrap();
    let mut names: Vec<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a.md", "b.md"]);

    let target: Option<String> = w.client.call("resolve_link", json!(["b"])).unwrap();
    assert_eq!(target.as_deref(), Some("b.md"));

    let ignored: Vec<String> = w.client.call("ignored_notes", json!([true])).unwrap();
    assert!(ignored.is_empty());
}

/// The host reads its own kernelspecs, a venv's in the vault among them.
#[test]
fn the_hosts_kernelspecs_cross_the_wire() {
    let w = Wired::seeded(&|root| {
        let at = root.join(".venv/share/jupyter/kernels/venvlang");
        std::fs::create_dir_all(&at).unwrap();
        let json = r#"{"argv": ["k", "{connection_file}"], "display_name": "Venv Lang",
                "language": "venvlang", "interrupt_mode": "message"}"#;
        std::fs::write(at.join("kernel.json"), json).unwrap();
    });
    let found: crate::kernel::Specs = w.client.call("kernel_specs", json!([])).unwrap();
    let spec = found.specs.iter().find(|s| s.name == "venvlang").unwrap();
    assert_eq!(
        (spec.display_name.as_str(), spec.message_interrupt),
        ("Venv Lang", true)
    );
    assert!(spec.dir.ends_with(".venv/share/jupyter/kernels/venvlang"));
}

#[test]
fn a_save_that_lost_the_race_comes_back_as_the_same_refusal() {
    let w = Wired::open();
    assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

    let (_, etag): (String, Etag) = w.client.call("read", json!(["a.md"])).unwrap();
    let fresh: Etag = w
        .client
        .call("save", json!(["a.md", "first\n", etag]))
        .unwrap();
    assert_ne!(fresh, etag, "the file was rewritten");

    // The stale etag is exactly what a second window would still be holding.
    let refused = w
        .client
        .call::<Etag>("save", json!(["a.md", "second\n", etag]))
        .unwrap_err();
    assert_eq!(
        refused,
        Error::Core(Core::ChangedOnDisk { current: fresh }),
        "the client has to be handed the error the host met, with the etag on disk"
    );
}

/// Every error crosses as the value it was: the code a JSON-RPC client reads, and the error
/// itself in `data`.
#[test]
fn every_error_crosses_the_wire_as_itself() {
    let etag = Etag {
        mtime_ns: 1,
        size: 2,
        ino: 3,
    };
    let errors = [
        Error::Core(Core::NotFound("a.md".into())),
        Error::Core(Core::AlreadyExists("a.md".into())),
        Error::Core(Core::ChangedOnDisk { current: etag }),
        Error::Core(Core::NotMerged {
            branch: "side".into(),
            kept: Some("origin/side".into()),
        }),
        Error::Core(Core::Cancelled),
        Error::Core(Core::Io("a.md: denied".into())),
        Error::Core(Core::Index("locked".into())),
        Error::Core(Core::Git("fatal".into())),
        Error::Core(Core::Config("bad".into())),
        Error::Core(Core::Pdf("broken".into())),
        Error::Core(Core::Invalid("outside".into())),
        Error::Offline("down".into()),
        Error::Lost("gone".into()),
        Error::Refused("no folder".into()),
        Error::Remote("timed out".into()),
        Error::UnknownMethod("nonesuch".into()),
        Error::Protocol("argument 0".into()),
        Error::Language("no server".into()),
        Error::NotYet("loading".into()),
    ];
    for e in errors {
        let wire = to_wire(&e);
        assert_eq!(wire["message"], e.to_string());
        assert_eq!(from_wire(&wire), e);
    }
    assert_eq!(
        to_wire(&Error::UnknownMethod("x".into()))["code"],
        NO_METHOD
    );
    assert_eq!(to_wire(&Error::Protocol("x".into()))["code"], BAD_PARAMS);
    assert_eq!(to_wire(&Error::Core(Core::Cancelled))["code"], FAILED);
}

/// New File on a name that is taken says so on a remote vault as it does locally, which is
/// what the window offers another name on.
#[test]
fn a_taken_name_comes_back_as_already_exists() {
    let w = Wired::open();
    let refused = w
        .client
        .call::<(String, Vec<usize>)>("create_note", json!(["a.md", null]))
        .unwrap_err();
    assert_eq!(refused, Error::Core(Core::AlreadyExists("a.md".into())));
}

#[test]
fn an_event_on_the_far_side_arrives_as_a_notification() {
    let w = Wired::open();
    assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

    w.client
        .call::<Etag>("save", json!(["c.md", "new note\n", Option::<Etag>::None]))
        .unwrap();
    assert!(
        w.wait(|e| matches!(e, Event::DirsChanged(dirs) if dirs.iter().any(|d| d.is_empty()))),
        "the vault's own worker has to reach the client"
    );
}

/// A language request runs where the files are, and what the provider has to say about the
/// document comes back the way any other event does.
#[test]
fn a_document_is_opened_and_answered_for_across_the_wire() {
    let w = Wired::open();
    assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

    let text = "# Title\n\nsee [[nope]]\n";
    let support: crate::Support = w
        .client
        .call("open_document", json!(["a.md", "markdown", text, 1]))
        .unwrap();
    assert_eq!(support.completion_triggers, ['[', '#', '(']);

    let symbols: Vec<crate::Symbol> = w.client.call("symbols", json!(["a.md"])).unwrap();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "Title");

    // Ghost text crosses the wire like everything else. This host names no `accent-cli` to
    // run merl, so only the round trip is asserted here; `apps/cli/tests/serve.rs` has merl.
    let at = crate::Pos {
        line: 2,
        character: 3,
    };
    let ghost: Option<String> = w
        .client
        .call("inline_completion", json!(["a.md", at]))
        .unwrap();
    assert_eq!(ghost, None, "a two-line note has nothing to suggest");
    let _: () = w.client.call("settle", json!(["a.md"])).unwrap();

    assert!(
        w.wait(|e| matches!(e, Event::Diagnostics { rel, items, .. }
            if rel == "a.md" && items.len() == 1)),
        "the dangling link has to reach the client as a notification"
    );
}

/// A caller branches on the kind — the open path offers to create a note that is not there —
/// so the kind has to survive the wire rather than flattening to `Other`.
#[test]
fn a_missing_file_comes_back_as_not_found() {
    let w = Wired::open();
    let refused = w
        .client
        .call::<(String, Etag)>("read", json!(["nope.md"]))
        .unwrap_err();
    assert_eq!(refused, Error::Core(Core::NotFound("nope.md".into())));
}

/// An exact search crosses as what was typed plus its toggles and is compiled on the host, so
/// case-insensitivity survives the wire and a pattern that does not compile says why.
#[test]
fn a_search_pattern_is_compiled_where_the_files_are() {
    let w = Wired::open();
    assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

    let (rows, total): (Vec<crate::Match>, usize) = w
        .client
        .call(
            "grep",
            json!(["HELLO", crate::Options::default(), 10, false]),
        )
        .unwrap();
    assert_eq!((rows.len(), total), (1, 1));

    let regex = crate::Options {
        regex: true,
        ..crate::Options::default()
    };
    let refused = w
        .client
        .call::<(Vec<crate::Match>, usize)>("grep", json!(["(", regex, 10, false]))
        .unwrap_err();
    assert!(
        matches!(&refused, Error::Core(Core::Invalid(why)) if why.contains("unclosed group")),
        "{refused}"
    );
}

/// A query's qualifiers are read on the host, where the index is, and a saved search takes the
/// same route as the pane's; one the host cannot read says why.
#[test]
fn a_query_is_read_where_the_index_is() {
    let w = Wired::open();
    assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

    let hits: Vec<crate::SearchHit> = w.client.call("saved_search", json!(["#tag", 10])).unwrap();
    let files: Vec<&str> = hits.iter().map(|h| h.rel_path.as_str()).collect();
    assert_eq!(files, ["b.md"]);
    let refused = w
        .client
        .call::<Vec<crate::SearchHit>>("search", json!(["kind:pfd", 10, false]))
        .unwrap_err();
    assert!(
        matches!(&refused, Error::Core(Core::Invalid(why)) if why.contains("is not a kind")),
        "{refused}"
    );
}

/// A cancel is a notification: it is taken, nothing comes back for it, and the connection
/// carries on answering. One for a request that has already finished is simply not there.
#[test]
fn a_cancel_is_taken_and_leaves_the_connection_answering() {
    let w = Wired::open();
    assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

    w.client.cancel(9999);
    let tags: Vec<(String, i64)> = w.client.call("tags", json!([])).unwrap();
    assert!(tags.iter().any(|(tag, _)| tag == "tag"), "{tags:?}");
}

/// A server over raw pipes, for a test that writes its own lines: its thread, what it reads,
/// its answers by id (the events left out), and the folder its vault is in.
fn raw(
    silence: Duration,
) -> (
    std::thread::JoinHandle<()>,
    std::io::PipeWriter,
    impl Iterator<Item = Value>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    let db = dir.path().join("index.db");
    let (vault, events) = Local::open_at(&root, &db, VaultConfig::default()).unwrap();
    let (server_in, input) = std::io::pipe().unwrap();
    let (output, server_out) = std::io::pipe().unwrap();
    let server = std::thread::spawn(move || {
        serve_local(vault, events, server_in, server_out, silence);
    });
    let answers = BufReader::new(output)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
        .filter(|msg| msg.get("id").is_some());
    (server, input, answers, dir)
}

/// This process's threads, where `/proc` counts them.
fn threads() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let count = status.lines().find_map(|l| l.strip_prefix("Threads:"))?;
    count.trim().parse().ok()
}

const PING_LINE: &str = r#"{"jsonrpc":"2.0","method":"ping"}"#;

/// A burst past the workers waits for them without a thread of its own, and past the queue it
/// is told at once that the host is busy. Pings and cancels are read all the while: the
/// server outlives its silence, and a cancelled request that was waiting never runs.
#[test]
fn a_burst_waits_for_the_workers_and_past_the_queue_hears_the_host_is_busy() {
    let silence = Duration::from_secs(1);
    let (server, mut input, mut answers, dir) = raw(silence);
    let release = dir.path().join("release");
    let before = threads();
    writeln!(input, "{PING_LINE}").unwrap();
    let busy = WORKERS + QUEUED + 1;
    for id in 1..=busy {
        let held = json!({"jsonrpc": "2.0", "id": id, "method": "test_wait", "params": [release]});
        writeln!(input, "{held}").unwrap();
    }
    let first = answers.next().unwrap();
    assert_eq!(first["id"], busy, "{first}");
    let said = first["error"]["message"].as_str().unwrap();
    assert!(said.contains("busy"), "{said}");
    if let (Some(before), Some(now)) = (before, threads()) {
        assert!(
            now < before + QUEUED / 2,
            "{} threads for {busy} requests",
            now - before
        );
    }

    let cancelled = WORKERS + 1;
    writeln!(
        input,
        r#"{{"jsonrpc":"2.0","method":"cancel","params":[{cancelled}]}}"#
    )
    .unwrap();
    for _ in 0..15 {
        writeln!(input, "{PING_LINE}").unwrap();
        std::thread::sleep(silence / 10);
    }
    assert!(!server.is_finished(), "the pings went unread");

    std::fs::write(&release, "").unwrap();
    let mut answered: Vec<u64> = (&mut answers)
        .take(busy - 2)
        .map(|answer| {
            assert_eq!(answer["result"], Value::Null, "{answer}");
            answer["id"].as_u64().unwrap()
        })
        .collect();
    answered.sort_unstable();
    let expected: Vec<u64> = (1..busy as u64)
        .filter(|&id| id != cancelled as u64)
        .collect();
    assert_eq!(answered, expected);
    drop(input);
    server.join().unwrap();
    assert!(answers.next().is_none(), "the cancelled request ran");
}

/// A client that stops reading holds up the writer and then the workers, never the reading:
/// pings keep the server alive, its threads stay bounded, and what was answered comes out
/// once the client reads again.
#[test]
fn a_client_that_stops_reading_holds_back_the_workers_and_not_the_pings() {
    let silence = Duration::from_secs(1);
    let (server, mut input, answers, _dir) = raw(silence);
    let before = threads();
    writeln!(input, "{PING_LINE}").unwrap();
    // Each answered at once, together more than the pipe and the writer's queue hold.
    for id in 1..=3 * QUEUED {
        writeln!(
            input,
            r#"{{"jsonrpc":"2.0","id":{id},"method":"nonesuch"}}"#
        )
        .unwrap();
    }
    for _ in 0..15 {
        writeln!(input, "{PING_LINE}").unwrap();
        std::thread::sleep(silence / 10);
    }
    assert!(!server.is_finished(), "the pings went unread");
    if let (Some(before), Some(now)) = (before, threads()) {
        assert!(now < before + QUEUED / 2, "{} threads", now - before);
    }
    drop(input);
    let mut ids: Vec<u64> = answers.map(|a| a["id"].as_u64().unwrap()).collect();
    let answered = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), answered, "an answer came twice");
    assert!(answered > WORKERS + QUEUED, "{answered} answers");
    server.join().unwrap();
}

/// A line longer than a host reads is read past, and the request after it is answered.
#[test]
fn a_line_too_long_is_skipped_and_the_next_request_answered() {
    let (server, mut input, mut answers, _dir) = raw(SILENCE);
    let writer = std::thread::spawn(move || {
        let mut long = br#"{"jsonrpc":"2.0","id":1,"method":"nonesuch","params":[""#.to_vec();
        long.resize(MAX_LINE + 1, b'x');
        long.extend_from_slice(b"\"]}\n");
        input.write_all(&long).unwrap();
        writeln!(input, r#"{{"jsonrpc":"2.0","id":2,"method":"nonesuch"}}"#).unwrap();
        input
    });
    let answer = answers.next().unwrap();
    assert_eq!(answer["id"], 2, "{answer}");
    drop(writer.join().unwrap());
    server.join().unwrap();
}

/// A request longer than a host reads fails at once with an error of its own, and the
/// connection carries on answering.
#[test]
fn a_request_too_long_for_the_host_is_refused_before_it_is_sent() {
    let w = Wired::open();
    let text = "x".repeat(MAX_LINE);
    let e = w
        .client
        .call::<Etag>("save", json!(["big.md", text, null]))
        .unwrap_err();
    // A request sent and never answered would be the deadline's `Remote` instead.
    assert!(
        matches!(&e, Error::Protocol(said) if said.contains("MiB")),
        "{e:?}"
    );
    w.client
        .call::<Vec<(String, i64)>>("tags", json!([]))
        .unwrap();
}

/// A method the host does not have is JSON-RPC's own -32601, never read as an empty answer:
/// what an `accent-cli` older than the window answers.
#[test]
fn a_missing_method_is_its_own_error_and_not_a_dropped_call() {
    let w = Wired::open();
    let e = w.client.call::<()>("nonesuch", json!([])).unwrap_err();
    assert_eq!(e, Error::UnknownMethod("nonesuch".into()));

    let dir = tempfile::tempdir().unwrap();
    let (vault, events) = Local::open_at(
        dir.path(),
        &dir.path().join("index.db"),
        VaultConfig::default(),
    )
    .unwrap();
    let (server_in, mut input) = std::io::pipe().unwrap();
    let (output, server_out) = std::io::pipe().unwrap();
    let server = std::thread::spawn(move || {
        serve_local(vault, events, server_in, server_out, SILENCE);
    });
    writeln!(input, r#"{{"jsonrpc":"2.0","id":7,"method":"nonesuch"}}"#).unwrap();
    let line = BufReader::new(output)
        .lines()
        .map_while(std::result::Result::ok)
        .find(|line| line.contains("\"id\":7"))
        .unwrap();
    let answer: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(answer["error"]["code"], -32601);
    assert_eq!(
        answer["error"]["data"],
        json!({"UnknownMethod": "nonesuch"})
    );
    drop(input);
    server.join().unwrap();
}

/// The window closed: `serve` must return rather than sit on a dead pipe, and the calls that
/// were in flight must fail at once rather than each spending the full deadline.
#[test]
fn closing_the_connection_ends_the_server_and_frees_every_caller() {
    let mut w = Wired::open();
    assert!(w.wait(|e| matches!(e, Event::Reconciled(_))));

    w.client.shutdown();
    let server = w.server.take().expect("still running");
    let t = Instant::now();
    server.join().expect("serve must return on EOF");
    assert!(t.elapsed() < DEADLINE, "it waited instead of noticing");

    let t = Instant::now();
    let e = w.client.call::<()>("tags", json!([])).unwrap_err();
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "a dead client must fail immediately, not after {DEADLINE:?}"
    );
    assert_eq!(e, Error::Offline("not connected".into()));
}

/// Verify caller-specific deadlines: longer waits permit host work, shorter
/// waits time out and cancel work whose answer is no longer wanted.
#[test]
fn a_call_waits_for_as_long_as_it_was_given() {
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, mut server_out) = std::io::pipe().unwrap();
    let cancelled: Arc<Mutex<Vec<Value>>> = Arc::default();
    // A host that takes 300 ms over every answer, and ignores the pings.
    let heard = cancelled.clone();
    let server = std::thread::spawn(move || {
        for line in BufReader::new(server_in)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let message = serde_json::from_str::<Value>(&line).unwrap();
            let Some(id) = message.get("id").cloned() else {
                if message.get("method") == Some(&json!("cancel")) {
                    locked(&heard).push(message["params"][0].clone());
                }
                continue;
            };
            std::thread::sleep(Duration::from_millis(300));
            let answer = json!({"jsonrpc": "2.0", "id": id, "result": "done"});
            if writeln!(server_out, "{answer}").is_err() {
                break;
            }
        }
    });
    let client = Client::new(
        Box::new(client_out),
        Box::new(client_in),
        channel().0,
        Box::new(|| {}),
    );
    let ask =
        |deadline| client.call_tracked::<String>("slow", json!([]), &Mutex::new(None), deadline);

    let e = ask(Duration::from_millis(100)).unwrap_err();
    assert_eq!(e, Error::Remote("slow timed out".into()));
    // The next answer proves the host has read past the cancel the timeout sent.
    assert_eq!(ask(Duration::from_secs(2)).unwrap(), "done");
    assert_eq!(
        *locked(&cancelled),
        vec![json!(1)],
        "the request given up on"
    );
    client.shutdown();
    server.join().unwrap();
}

/// An answer still arriving holds its call past the deadline, which counts from the last byte
/// read rather than from the request: a note read over a slow link is one long line.
#[test]
fn an_answer_still_arriving_holds_its_call_past_the_deadline() {
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, mut server_out) = std::io::pipe().unwrap();
    // A host that sends each answer in twelve pieces, 50 ms apart.
    let server = std::thread::spawn(move || {
        for line in BufReader::new(server_in)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let message = serde_json::from_str::<Value>(&line).unwrap();
            let Some(id) = message.get("id").cloned() else {
                continue;
            };
            let answer = encode(&json!({"jsonrpc": "2.0", "id": id, "result": "x".repeat(600)}));
            for piece in answer.chunks(answer.len().div_ceil(12)) {
                std::thread::sleep(Duration::from_millis(50));
                if server_out.write_all(piece).is_err() {
                    return;
                }
            }
        }
    });
    let client = Client::new(
        Box::new(client_out),
        Box::new(client_in),
        channel().0,
        Box::new(|| {}),
    );
    let t = Instant::now();
    let deadline = Duration::from_millis(200);
    let answer = client.call_tracked::<String>("read", json!([]), &Mutex::new(None), deadline);
    assert_eq!(answer.map(|text| text.len()), Ok(600));
    assert!(t.elapsed() > 2 * deadline, "it took {:?}", t.elapsed());
    client.shutdown();
    server.join().unwrap();
}

/// A cancel never waits on the link: the window drops requests on its own thread, and a
/// long request may hold the writer for minutes. The one that found it busy follows it.
#[test]
fn a_cancel_does_not_wait_behind_a_request_still_being_sent() {
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, server_out) = std::io::pipe().unwrap();
    let client = Arc::new(Client::new(
        Box::new(client_out),
        Box::new(client_in),
        channel().0,
        Box::new(|| {}),
    ));
    // A request far longer than the pipe holds, which nobody reads yet.
    let sending = std::thread::spawn({
        let client = client.clone();
        move || client.call::<Value>("save", json!(["x".repeat(1 << 20)]))
    });
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    client.cancel(42);
    assert!(
        t.elapsed() < Duration::from_millis(100),
        "{:?}",
        t.elapsed()
    );
    // Read now: the cancel comes after the request it waited behind.
    let cancel = BufReader::new(server_in)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
        .find(|msg| msg["method"] == "cancel")
        .unwrap();
    assert_eq!(cancel["params"], json!([42]));
    drop(server_out);
    let _ = sending.join().unwrap();
    client.shutdown();
}

/// A request line still arriving keeps the host from its silence, the client's pings waiting
/// behind it, and the host says once a second that it is still receiving, which the client's
/// deadline counts from once its writer has handed ssh the rest.
#[test]
fn a_request_still_arriving_keeps_the_host_listening_and_says_so() {
    let silence = Duration::from_millis(300);
    let dir = tempfile::tempdir().unwrap();
    let (vault, events) = Local::open_at(
        dir.path(),
        &dir.path().join("index.db"),
        VaultConfig::default(),
    )
    .unwrap();
    let (server_in, mut input) = std::io::pipe().unwrap();
    let (output, server_out) = std::io::pipe().unwrap();
    let server = std::thread::spawn(move || {
        serve_local(vault, events, server_in, server_out, silence);
    });
    writeln!(input, "{PING_LINE}").unwrap();
    let request = encode(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "nonesuch", "params": ["x".repeat(4000)]}),
    );
    // Twice the host's silence in pieces, 50 ms apart, the last with the newline.
    let writer = std::thread::spawn(move || {
        for piece in request.chunks(request.len().div_ceil(48)) {
            std::thread::sleep(Duration::from_millis(50));
            input.write_all(piece).unwrap();
        }
        input
    });
    let mut said = BufReader::new(output)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
        .filter(|msg| msg.get("method") != Some(&json!("event")));
    let mut receiving = 0;
    let answer = loop {
        let msg = said.next().expect("the host stopped mid-request");
        if msg.get("id").is_some() {
            break msg;
        }
        receiving += usize::from(msg["method"] == "receiving");
    };
    assert_eq!(answer["id"], 1, "{answer}");
    assert!(receiving >= 2, "{receiving} notes for 2.4 s of a line");
    drop(writer.join().unwrap());
    server.join().unwrap();
}

/// A call given up on lets its caller go at once rather than when the host answers, or at the
/// deadline when it never does.
#[test]
fn a_cancelled_call_lets_its_caller_go_at_once() {
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, server_out) = std::io::pipe().unwrap();
    // A host that reads every request and answers none.
    let server = std::thread::spawn(move || {
        for _ in BufReader::new(server_in)
            .lines()
            .map_while(std::result::Result::ok)
        {}
    });
    let client = Arc::new(Client::new(
        Box::new(client_out),
        Box::new(client_in),
        channel().0,
        Box::new(|| {}),
    ));
    let asked: Arc<Mutex<Option<u64>>> = Arc::default();
    let call = std::thread::spawn({
        let (client, asked) = (client.clone(), asked.clone());
        move || {
            let t = Instant::now();
            let answer = client.call_tracked::<Value>("slow", json!([]), &asked, DEADLINE);
            (answer, t.elapsed())
        }
    });
    let id = loop {
        if let Some(id) = *locked(&asked) {
            break id;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    client.cancel(id);
    let (answer, took) = call.join().unwrap();
    assert_eq!(answer.unwrap_err(), Error::Core(Core::Cancelled));
    assert!(took < Duration::from_secs(1), "the caller waited {took:?}");
    // The host's end first: the reader the shutdown joins ends with it.
    drop(server_out);
    client.shutdown();
    server.join().unwrap();
}

/// A link that goes while a call is out leaves it unknown whether the host did what it was
/// asked; one refused before it was sent did nothing, and only that one is asked again.
#[test]
fn a_call_the_link_dropped_is_not_unasked() {
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, server_out) = std::io::pipe().unwrap();
    // A host that reads the request and goes without answering it. The client's first ping
    // may come before it, and is no request.
    let server = std::thread::spawn(move || {
        let _request = BufReader::new(server_in)
            .lines()
            .map_while(std::result::Result::ok)
            .find(|line| line.contains("\"id\""));
        drop(server_out);
    });
    let client = Client::new(
        Box::new(client_out),
        Box::new(client_in),
        channel().0,
        Box::new(|| {}),
    );
    let e = client.call::<()>("rewrite", json!([])).unwrap_err();
    assert!(e.is_offline() && !e.unasked(), "{e}");
    server.join().unwrap();
    client.join();
    let e = client.call::<()>("rewrite", json!([])).unwrap_err();
    assert!(e.unasked(), "{e}");
}

/// The reader is the first to know the link has gone, and says so once. An end the client
/// asked for is not a lost link, or every window close would read as one.
#[test]
fn a_lost_link_is_said_once_and_a_closed_one_not_at_all() {
    let ends = |closed: bool| {
        let (client_in, server_out) = std::io::pipe().unwrap();
        let (_server_in, client_out) = std::io::pipe().unwrap();
        let lost = Arc::new(AtomicU64::new(0));
        let client = Client::new(
            Box::new(client_out),
            Box::new(client_in),
            channel().0,
            Box::new({
                let lost = lost.clone();
                move || {
                    lost.fetch_add(1, Ordering::SeqCst);
                }
            }),
        );
        if closed {
            client.close();
        }
        drop(server_out);
        client.join();
        lost.load(Ordering::SeqCst)
    };
    assert_eq!(ends(false), 1, "the server went away unasked");
    assert_eq!(ends(true), 0, "the client closed it");
}

/// A link that dies without closing leaves the server a stdin that neither ends nor speaks.
/// Pinged once, the server takes the silence as the window gone; never pinged, as when it is
/// driven by hand, it waits for its stdin to close.
#[test]
fn a_pinged_server_stops_when_the_pings_do_and_an_unpinged_one_waits() {
    let silence = Duration::from_millis(200);
    let serve = || {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        let db = dir.path().join("index.db");
        let (vault, events) = Local::open_at(&root, &db, VaultConfig::default()).unwrap();
        let (server_in, input) = std::io::pipe().unwrap();
        let (output, server_out) = std::io::pipe().unwrap();
        let server = std::thread::spawn(move || {
            serve_local(vault, events, server_in, server_out, silence);
        });
        (server, input, (output, dir))
    };
    let (unpinged, quiet, _kept) = serve();
    let (pinged, mut input, _also_kept) = serve();

    writeln!(input, r#"{{"jsonrpc":"2.0","method":"ping"}}"#).unwrap();
    let t = Instant::now();
    while !pinged.is_finished() && t.elapsed() < silence + Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        pinged.is_finished(),
        "still serving {:?} after the ping",
        t.elapsed()
    );

    std::thread::sleep(3 * silence);
    assert!(!unpinged.is_finished(), "nothing armed the silence");
    drop(quiet);
    unpinged.join().expect("stdin closing still ends it");
}

/// A link that goes quiet in the middle of a commit leaves the commit to finish on the host:
/// the server waits for it before letting the vault go, rather than exiting around it.
#[test]
fn a_server_gone_quiet_still_finishes_the_commit_it_was_running() {
    use std::os::unix::fs::PermissionsExt;
    let run = |root: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    std::fs::create_dir(&root).unwrap();
    run(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Accent Test"),
        ("user.email", "test@accent.invalid"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "hooks"),
    ] {
        run(&root, &["config", key, value]);
    }
    let hook = root.join("hooks/pre-commit");
    std::fs::create_dir(root.join("hooks")).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nsleep 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(root.join("a.md"), "one\n").unwrap();
    run(&root, &["add", "a.md"]);
    let repo = crate::git::toplevel(&root).unwrap().unwrap();

    let silence = Duration::from_millis(200);
    let (vault, events) =
        Local::open_at(&root, &dir.path().join("index.db"), VaultConfig::default()).unwrap();
    let (server_in, mut input) = std::io::pipe().unwrap();
    let (_output, server_out) = std::io::pipe().unwrap();
    let t = Instant::now();
    let server = std::thread::spawn(move || {
        serve_local(vault, events, server_in, server_out, silence);
    });
    writeln!(input, r#"{{"jsonrpc":"2.0","method":"ping"}}"#).unwrap();
    let commit = json!({"jsonrpc": "2.0", "id": 1, "method": "git_commit",
        "params": [repo, "first", false]});
    writeln!(input, "{commit}").unwrap();

    while !server.is_finished() && t.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        server.is_finished(),
        "still serving after {:?}",
        t.elapsed()
    );
    assert!(
        crate::git::status(&repo).unwrap().branch.oid.is_some(),
        "the server stopped before the commit landed"
    );
}

/// A mistyped root is refused rather than served as an empty vault, and the refusal is the
/// answer to the `hello` the window opens with, so the window's banner can say why.
#[test]
fn serve_refuses_a_root_that_is_not_a_folder_and_says_so_to_hello() {
    let dir = tempfile::tempdir().unwrap();
    let (root, db) = (dir.path().join("nonesuch"), dir.path().join("index.db"));
    let (server_in, client_out) = std::io::pipe().unwrap();
    let (client_in, server_out) = std::io::pipe().unwrap();
    let server = std::thread::spawn({
        let (root, db) = (root.clone(), db.clone());
        move || serve(&root, Some(&db), server_in, server_out)
    });
    let client = Client::new(
        Box::new(client_out),
        Box::new(client_in),
        channel().0,
        Box::new(|| {}),
    );

    let e = client
        .call::<Hello>("hello", json!([VaultConfig::default()]))
        .unwrap_err();
    // Its own kind, so the window can tell it from a link that failed and stop retrying.
    assert_eq!(
        e,
        Error::Refused(format!("{} is not a folder", root.display()))
    );
    assert!(server.join().unwrap().is_err(), "serve must exit failing");
    assert!(!db.exists(), "no index for a vault that is not there");
}

/// Verify Stop reaches a running host scan and Resume restarts it. Separate
/// request threads set a flag without queueing behind the walk; enough notes
/// keep the scan running past `Wired::open`'s handshake.
#[test]
fn stop_and_resume_reach_the_hosts_worker_over_the_wire() {
    let w = Wired::seeded(&|root| {
        for i in 0..4000 {
            std::fs::write(root.join(format!("n{i}.md")), "body").unwrap();
        }
    });
    w.client.call::<()>("stop_indexing", json!([])).unwrap();
    assert!(
        w.wait(|e| matches!(e, Event::Reconciled(s) if s.stopped)),
        "the host's walk was not stopped"
    );
    w.client.call::<()>("resume_indexing", json!([])).unwrap();
    assert!(
        w.wait(|e| matches!(e, Event::Reconciled(s) if !s.stopped)),
        "the host's walk did not finish after a resume"
    );
}

/// The host runs the adapter beside `serve`: the requests reach its sessions, and the news
/// comes back as the vault's events. The window runs the program, here this test.
#[test]
fn a_debug_session_crosses_the_wire() {
    use crate::debug::{Breakpoint, Context, Frame, Launch, News};
    let demo = "def double(x):\n    y = x * 2\n    return y\n\ntotal = double(21)\nprint(\"total\", total)\n";
    let w = Wired::seeded(&|root| std::fs::write(root.join("demo.py"), demo).unwrap());
    let refused = w.client.call::<()>("debug_threads", json!([9]));
    assert!(matches!(refused, Err(Error::Language(_))), "{refused:?}");
    if !crate::tests::has_debugpy() {
        eprintln!("debugpy is not installed: skipping the session over the wire");
        return;
    }
    let next = |want: fn(&News) -> bool| {
        let event = crate::tests::wait_for(
            &w._events,
            |e| matches!(e, Event::Debug { session: 1, news } if want(news)),
            Duration::from_secs(30),
        );
        match event {
            Some(Event::Debug { news, .. }) => news,
            e => panic!("{e:?}"),
        }
    };
    // A condition that holds: it crosses the wire and the program still stops.
    let held = Breakpoint {
        condition: Some("x == 21".to_string()),
        ..Breakpoint::at(1)
    };
    let launch = Launch {
        program: "demo.py".to_string(),
        language: "python3".to_string(),
        breakpoints: vec![("demo.py".to_string(), vec![held.clone()])],
        exceptions: Vec::new(),
    };
    std::thread::scope(|s| {
        let started = s.spawn(|| {
            let asked = Mutex::new(None);
            let launch = json!([1, launch]);
            let deadline = Duration::from_secs(60);
            w.client
                .call_tracked::<()>("debug_start", launch, &asked, deadline)
        });
        let News::Terminal {
            seq,
            args,
            cwd,
            env,
        } = next(|n| matches!(n, News::Terminal { .. }))
        else {
            unreachable!()
        };
        let mut program = std::process::Command::new(&args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .envs(env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let answer = json!([1, seq, program.id()]);
        w.client.call::<()>("debug_answer", answer).unwrap();

        let News::Breakpoints { placed, .. } = next(|n| matches!(n, News::Breakpoints { .. }))
        else {
            unreachable!()
        };
        assert_eq!(placed[0].breakpoint, held);
        let News::Stopped { thread, .. } = next(|n| matches!(n, News::Stopped { .. })) else {
            unreachable!()
        };
        started.join().unwrap().unwrap();
        let frames: Vec<Frame> = w.client.call("debug_stack", json!([1, thread])).unwrap();
        assert_eq!(
            (frames[0].name.as_str(), frames[0].path.as_deref()),
            ("double", Some("demo.py"))
        );
        let asked = json!([1, frames[0].id, "x * 2", Context::Repl]);
        let twice: crate::debug::Evaluated = w.client.call("debug_evaluate", asked).unwrap();
        assert_eq!(twice.result, "42");
        w.client
            .call::<()>("debug_continue", json!([1, thread]))
            .unwrap();
        let ended = next(|n| matches!(n, News::Ended { .. }));
        assert_eq!(
            ended,
            News::Ended {
                code: Some(0),
                error: None
            }
        );
        program.wait().unwrap();
    });
    // A session already over is stopped without complaint.
    w.client.call::<()>("debug_stop", json!([1])).unwrap();
}

/// The window went away while the console's code ran on, which may never return: `serve`
/// ends the vault's debug sessions rather than wait for it.
#[test]
fn closing_the_connection_ends_a_debug_session_whatever_it_runs() {
    the_window_gone_ends_a_debug_session(false);
}

/// A link gone silent is a window gone as well, and `serve` does not linger on the evaluation.
#[test]
fn a_silent_connection_ends_a_debug_session_whatever_it_runs() {
    the_window_gone_ends_a_debug_session(true);
}

/// The window closing its link, or with `quiet` going silent after a ping, while the console
/// evaluates code that never returns.
fn the_window_gone_ends_a_debug_session(quiet: bool) {
    use crate::debug::{Context, Launch};
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let config = VaultConfig {
        debug: crate::tests::fake_adapter(),
        ..VaultConfig::default()
    };
    let (vault, events) =
        Local::open_at(root.path(), &cache.path().join("index.db"), config).unwrap();
    // Bare pipes rather than a `Client`, whose shutdown would wait on a `serve` that hangs.
    let (server_in, mut client_out) = std::io::pipe().unwrap();
    let (client_in, server_out) = std::io::pipe().unwrap();
    let silence = Duration::from_millis(200);
    let server =
        std::thread::spawn(move || serve_local(vault, events, server_in, server_out, silence));
    let mut ask = move |id: u64, method: &str, params: Value| {
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(client_out, "{line}").unwrap();
    };
    let mut heard = BufReader::new(client_in)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap());

    let launch = Launch {
        program: "demo.py".to_string(),
        language: "python3".to_string(),
        breakpoints: Vec::new(),
        exceptions: Vec::new(),
    };
    ask(1, "debug_start", json!([1, launch]));
    let started = heard.find(|m| m["id"] == 1).unwrap();
    assert!(started.get("result").is_some(), "{started}");
    let forever = json!([1, null, "while True: pass", Context::Repl]);
    ask(2, "debug_evaluate", forever);
    let appears = |name: &str| {
        crate::tests::poll_until(|| root.path().join(name).exists(), Duration::from_secs(10))
    };
    assert!(appears("evaluate"), "the adapter was never asked");

    // Pinged once, the server takes the silence that follows for the window gone.
    match quiet {
        true => ask(3, "ping", json!([])),
        false => drop(ask),
    }
    let ended = crate::tests::poll_until(|| server.is_finished(), Duration::from_secs(10));
    assert!(ended, "serve waits on the evaluation (quiet: {quiet})");
    server.join().unwrap();
    assert!(appears("ended"), "the adapter outlived serve");
}

/// The session belongs to the machine the window is on, so it is not on the wire at all.
#[test]
fn the_session_is_not_a_remote_method() {
    let w = Wired::open();
    assert!(w.client.call::<Session>("session", json!([])).is_err());
}
