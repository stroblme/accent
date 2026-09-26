//! The remote backend against a real host, which is the only place several of its parts are
//! exercised at all: ssh's own multiplexing, a shell that is not ours, and a server binary that
//! has to start on a machine older than the one that built it.
//!
//! Opt-in, because it needs a host and a key: set `ACCENT_TEST_REMOTE` to an `ssh://` address
//! whose path may be created and deleted. Without it the test says so and passes, so `make check`
//! is unchanged for everyone else. The host needs `git`, `ss` and `pgrep`, and [`HOST_PORT`] free.
//!
//!     make server
//!     ACCENT_TEST_REMOTE=ssh://myhost/tmp/accent-probe cargo test -p accent-cli --test remote
//!
//! One test function, because the first connection after a rebuild uploads the server, and two
//! tests connecting at once would race each other's upload.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use accent_api::link;
use accent_api::ssh::{self, Direction, Forward, Url};
use accent_api::{Event, Vault, VaultConfig, git, rpc};

/// Long enough for a master, an upload of six megabytes and a first reconcile.
const BUDGET: Duration = Duration::from_secs(120);

/// The port the host is made to listen on for a forward back to this machine.
const HOST_PORT: u16 = 47913;

#[test]
fn a_remote_vault_connects_indexes_and_answers() {
    let Ok(address) = std::env::var("ACCENT_TEST_REMOTE") else {
        eprintln!("set ACCENT_TEST_REMOTE=ssh://host/path to run this");
        return;
    };
    let url = accent_api::ssh::parse(&address).expect("a usable address");
    let p = ssh::quote(&url.path.to_string_lossy());

    // A vault of known shape, made over the same connection the app will use: two notes, a
    // folder with one more, a dependency tree the index leaves out, and a repository whose origin
    // takes 20 s to answer a fetch, which is past both the fetch's own cap and the RPC's deadline.
    on_host(
        &url,
        &format!(
            "rm -rf {p} && mkdir -p {p}/node_modules/pkg {p}/sub \
             && printf 'hello [[b]]\\n' > {p}/a.md && printf 'old\\n' > {p}/sub/old.md \
             && printf '#tag\\n' > {p}/b.md && printf '1\\n' > {p}/node_modules/pkg/index.js \
             && git -C {p} init -q && git -C {p} remote add origin {p} \
             && git -C {p} config remote.origin.uploadpack 'sleep 20; git-upload-pack'"
        ),
    );

    let t = Instant::now();
    let (vault, events) = Vault::open_remote(&address, VaultConfig::default()).unwrap();
    // Opening must not wait for the connection: that is what lets the window paint at once.
    assert!(
        t.elapsed() < Duration::from_millis(200),
        "open_remote blocked for {:?}",
        t.elapsed()
    );
    assert!(vault.is_remote());

    let deadline = Instant::now() + BUDGET;
    let (mut connected, mut reconciled) = (false, false);
    while Instant::now() < deadline && !(connected && reconciled) {
        match events.recv_timeout(Duration::from_millis(500)) {
            Ok(Event::Connecting { what, .. }) => eprintln!("  {what}"),
            Ok(Event::Connected) => connected = true,
            Ok(Event::Reconciled(_)) => reconciled = true,
            Ok(Event::Disconnected(why)) => panic!("disconnected: {why}"),
            Ok(_) | Err(_) => {}
        }
    }
    assert!(connected, "never connected");
    assert!(reconciled, "never indexed");
    eprintln!("connected and indexed in {:?}", t.elapsed());

    // The root the server canonicalised, which is what every rel is relative to.
    assert_eq!(vault.root(), url.path);

    // The dependency tree is listed though the index never held it: `list_dir` merges it in on
    // the host, and an id of zero is what says it came off the disk.
    let rows = vault.list_dir("").unwrap();
    let mut names: Vec<&str> = rows.iter().map(|r| r.rel_path.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a.md", "b.md", "node_modules", "sub"]);
    assert!(
        rows.iter()
            .any(|r| r.rel_path == "node_modules" && r.id == 0)
    );
    let inside: Vec<String> = vault
        .list_dir("node_modules")
        .unwrap()
        .into_iter()
        .map(|r| r.rel_path)
        .collect();
    assert_eq!(inside, ["node_modules/pkg"]);

    // A link the remote index resolved, which means the whole indexing path ran over there.
    assert_eq!(vault.resolve_link("b").unwrap().as_deref(), Some("b.md"));

    // Write, and read back what the far side actually stored.
    let (text, etag) = vault.read("a.md").unwrap();
    assert_eq!(text, "hello [[b]]\n");
    let fresh = vault.save("a.md", "hello again\n", Some(etag)).unwrap();
    assert_ne!(fresh, etag);
    assert_eq!(vault.read("a.md").unwrap().0, "hello again\n");

    // A stale etag has to be refused, or two windows would overwrite each other silently.
    assert!(matches!(
        vault.save("a.md", "no\n", Some(etag)),
        Err(accent_api::SaveError::ChangedOnDisk { .. })
    ));

    // `fetch` is what the PDF and image readers use: a real file on this machine.
    let local = vault.fetch("a.md").unwrap();
    assert_eq!(std::fs::read_to_string(&local).unwrap(), "hello again\n");

    // Upload and Download… carry bytes over ssh's pipes: bytes that are not text, and many pipe
    // buffers of them, have to come back exactly as they went.
    let bytes: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8 ^ 0x80).collect();
    assert!(std::str::from_utf8(&bytes).is_err());
    let scratch = std::env::temp_dir().join(format!("accent-remote-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let (sent, got) = (scratch.join("sent.bin"), scratch.join("got.bin"));
    std::fs::write(&sent, &bytes).unwrap();
    vault.upload(&sent, "blob.bin").unwrap();
    vault.download("blob.bin", &got).unwrap();
    assert!(
        std::fs::read(&got).unwrap() == bytes,
        "the download differs"
    );

    // A file that lands in a folder reaches the window as that folder changing, which is what
    // refreshes its rows in the tree, however it got there: made by the app, uploaded, or written
    // on the host by something else.
    let made = changes_dir(&events, "sub", || {
        vault.create_note("sub/made.md", None).unwrap();
    });
    let small = scratch.join("small.txt");
    std::fs::write(&small, "up\n").unwrap();
    let uploaded = changes_dir(&events, "sub", || {
        vault.upload(&small, "sub/uploaded.txt").unwrap();
    });
    let outside = changes_dir(&events, "sub", || {
        on_host(&url, &format!("printf 'there\\n' > {p}/sub/outside.md"));
    });
    let listed: Vec<String> = vault
        .list_dir("sub")
        .unwrap()
        .into_iter()
        .map(|r| r.rel_path)
        .collect();
    assert_eq!(
        listed,
        [
            "sub/made.md",
            "sub/old.md",
            "sub/outside.md",
            "sub/uploaded.txt"
        ]
    );
    eprintln!(
        "`sub` changed {made:?} after create_note, {uploaded:?} after an upload, \
         {outside:?} after a write on the host"
    );
    std::fs::remove_dir_all(&scratch).unwrap();

    // A fetch whose origin does not answer in time. The host's cap has to answer before the RPC
    // gives up on it, and the reads beside it must not wait for either.
    let repos = vault.repos().unwrap();
    assert_eq!(repos.len(), 1);
    let (fetched, took, slowest, listings) = std::thread::scope(|s| {
        let fetch = s.spawn(|| {
            let t = Instant::now();
            (vault.git_fetch(&repos[0]), t.elapsed())
        });
        let (mut slowest, mut listings) = (Duration::ZERO, 0);
        while !fetch.is_finished() {
            let t = Instant::now();
            vault.list_dir("").unwrap();
            slowest = slowest.max(t.elapsed());
            listings += 1;
        }
        let (fetched, took) = fetch.join().unwrap();
        (fetched, took, slowest, listings)
    });
    let why = fetched.expect_err("the origin sleeps for 20 s").to_string();
    assert!(why.contains("did not finish within"), "{why}");
    assert!(
        (git::FETCH_TIMEOUT..rpc::DEADLINE).contains(&took),
        "the fetch answered after {took:?}"
    );
    assert!(
        slowest < Duration::from_secs(1),
        "a listing took {slowest:?}"
    );
    eprintln!("capped fetch: {took:?}; {listings} listings beside it, the slowest {slowest:?}");
    // Killed at the cap, it must have left nothing behind that stops the next one.
    on_host(
        &url,
        &format!("git -C {p} config --unset remote.origin.uploadpack"),
    );
    vault.git_fetch(&repos[0]).unwrap();

    // The branch popover and the history's labels, whose types changed on the wire. The origin is
    // the repository itself, so a branch fetched and then deleted is one that only a
    // remote-tracking ref still names.
    on_host(
        &url,
        &format!(
            "cd {p} && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m one \
             && git switch -q -c x \
             && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m two \
             && git switch -q - && git fetch -q origin && git branch -q -D x"
        ),
    );
    let branches = vault.git_branches(&repos[0]).unwrap();
    assert!(
        branches.remote.contains(&"origin/x".to_string()),
        "{branches:?}"
    );
    assert!(!branches.local.contains(&"x".to_string()), "{branches:?}");
    vault.git_track(&repos[0], "origin/x").unwrap();
    let branches = vault.git_branches(&repos[0]).unwrap();
    assert!(branches.local.contains(&"x".to_string()), "{branches:?}");
    let log = vault.git_log(&repos[0], 0, 10).unwrap();
    let tip = log.iter().find(|c| c.summary == "two").expect("x's commit");
    let label = |name: &str, kind, head| git::Ref {
        name: name.to_string(),
        kind,
        head,
    };
    assert_eq!(
        tip.refs,
        [
            label("x", git::RefKind::LocalBranch, true),
            label("origin/x", git::RefKind::RemoteBranch, false),
        ]
    );

    // And a delete, which on a remote vault is permanent by design.
    vault.delete("b.md").unwrap();
    assert!(!vault.exists("b.md"));

    // A forward the other way round: the host listens and reaches this machine.
    let remote = vault.remote().unwrap();
    assert!(
        !host_listens(&url, HOST_PORT),
        "port {HOST_PORT} is taken on the host"
    );
    let back = Forward {
        local: 8080,
        remote: HOST_PORT,
        direction: Direction::ToLocal,
    };
    remote.forward(back).unwrap();
    assert!(host_listens(&url, HOST_PORT), "the host does not listen");

    // A dropped link: the master dies, and the server and the forward go with it. The window
    // hears of it at once, without having to ask the host for anything first.
    kill_master(&url, remote.control_path());
    let (why, took) = wait_lost(&events);
    assert!(why.contains(&url.host), "{why}");
    eprintln!("lost link said in {took:?}: {why}");
    assert!(
        eventually(|| !host_listens(&url, HOST_PORT)),
        "the forward outlived its master"
    );
    // A reconnect makes a new master that has to carry the forward again by the time it says it
    // is connected.
    let t = Instant::now();
    vault.reconnect();
    wait_for(&events, |e| matches!(e, Event::Connected));
    eprintln!("reconnected in {:?}", t.elapsed());
    assert!(
        host_listens(&url, HOST_PORT),
        "the reconnect lost the forward"
    );
    assert!(vault.exists("a.md"));

    remote.cancel_forward(back).unwrap();
    assert!(
        eventually(|| !host_listens(&url, HOST_PORT)),
        "the cancel left it up"
    );

    // Documents open when the link drops are open again on the new server, with the text they
    // were sent, by the time it says it is connected. The attempt is a quiet one, as the window's
    // own retries are, so nothing on the way may prompt.
    let docs = ["One", "Two", "Three"];
    for name in docs {
        remote
            .call::<serde_json::Value>(
                "open_document",
                serde_json::json!([format!("{name}.md"), "markdown", format!("# {name}\n")]),
            )
            .unwrap();
    }
    kill_master(&url, remote.control_path());
    wait_lost(&events);
    let t = Instant::now();
    remote.reconnect_quietly();
    let mut reopening = None;
    let reopened = loop {
        match events.recv_timeout(BUDGET.saturating_sub(t.elapsed())) {
            Ok(Event::Connected) => break reopening.map(|at: Instant| at.elapsed()),
            Ok(Event::Connecting { what, .. }) => {
                eprintln!("  {what}");
                if what == "Reopening the documents" {
                    reopening = Some(Instant::now());
                }
            }
            Ok(Event::Disconnected(why)) => panic!("disconnected: {why}"),
            Ok(_) => {}
            Err(_) => panic!("no quiet reconnect in {BUDGET:?}"),
        }
    };
    let reopened = reopened.expect("the documents were never reopened");
    eprintln!(
        "quiet reconnect with {} documents in {:?}, reopening them {reopened:?} of it",
        docs.len(),
        t.elapsed()
    );
    for name in docs {
        let symbols: Vec<accent_api::Symbol> = remote
            .call("symbols", serde_json::json!([format!("{name}.md")]))
            .unwrap_or_else(|e| panic!("{name}.md after the reconnect: {e}"));
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, [name]);
    }

    // A link nothing answers to is defined where New File would write it, and one into the tree
    // the index never walked as the file there: the host's own disk decides which.
    let links = "[[Nowhere/New]]\n[[node_modules/pkg/index.js]]\n";
    remote
        .call::<serde_json::Value>(
            "open_document",
            serde_json::json!(["c.md", "markdown", links]),
        )
        .unwrap();
    for (line, path, missing) in [
        (0, "Nowhere/New.md", true),
        (1, "node_modules/pkg/index.js", false),
    ] {
        let at = serde_json::json!({ "line": line, "character": 3 });
        let found: Vec<accent_api::Location> = remote
            .call("definition", serde_json::json!(["c.md", at]))
            .unwrap();
        let found: Vec<_> = found.iter().map(|l| (l.path.as_str(), l.missing)).collect();
        assert_eq!(found, [(path, missing)]);
    }

    // `[[paper.pdf#` lists the PDF's bookmarks, though the host's `serve` has no PDF reader: the
    // host answers with the file and the link, and the window lists them from its own copy.
    vault.write_file("paper.pdf", &bookmarked_pdf()).unwrap();
    assert!(
        eventually(
            || vault.resolve_link("paper.pdf").ok().flatten().as_deref() == Some("paper.pdf")
        ),
        "the host never indexed paper.pdf"
    );
    remote
        .call::<serde_json::Value>(
            "open_document",
            serde_json::json!(["d.md", "markdown", "[[paper.pdf#\n"]),
        )
        .unwrap();
    let at = accent_api::Pos {
        line: 0,
        character: 12,
    };
    let asked: accent_api::Completions = remote
        .call("completion", serde_json::json!(["d.md", at, null]))
        .unwrap();
    assert!(asked.items.is_empty(), "{asked:?}");
    assert_eq!(asked.pages.map(|p| p.rel).as_deref(), Some("paper.pdf"));
    let listed = wait(vault.completion("d.md", at, None)).unwrap();
    match listed.pages {
        // A client built without a PDF reader leaves the question as the host did.
        Some(_) => eprintln!("no PDF reader here either: run with --features accent-api/pdf"),
        None => {
            let rows: Vec<(&str, &str)> = listed
                .items
                .iter()
                .map(|c| (c.label.as_str(), c.insert.as_str()))
                .collect();
            assert_eq!(rows, [("Second", "[[paper.pdf#page=2]]")]);
        }
    }

    // A page edit's links are rewritten on the host, where the notes and the index are.
    vault
        .write_file("e.md", b"[[paper.pdf#page=1]] [t](paper.pdf#page=2)\n")
        .unwrap();
    assert!(
        eventually(|| vault
            .backlinks("paper.pdf")
            .is_ok_and(|links| links.iter().any(|b| b.src_rel_path == "e.md"))),
        "the host never indexed e.md"
    );
    let moved = accent_api::PageEdit::Move { from: 0, to: 1 };
    let report = vault.repage_links("paper.pdf", moved, &[]).unwrap();
    assert_eq!(
        (report.rewritten, report.moved),
        (vec!["e.md".to_string()], 2)
    );
    assert_eq!(
        vault.read("e.md").unwrap().0,
        "[[paper.pdf#page=2]] [t](paper.pdf#page=1)\n"
    );
    // Save As's copy in another folder has its paths pointed back by the host's index.
    assert_eq!(
        vault
            .relink_copy("e.md", "x/copy.md", "[t](paper.pdf#page=1) [[paper.pdf]]\n")
            .unwrap()
            .as_deref(),
        Some("[t](../paper.pdf#page=1) [[paper.pdf]]\n")
    );

    // Closing the vault takes the server and every forward it still has off the host. The master
    // is left its ControlPersist minute on purpose, so it is not what is asserted on.
    remote.forward(back).unwrap();
    assert!(host_listens(&url, HOST_PORT), "the host does not listen");
    assert!(host_serves(&url), "no server to see stop");
    let t = Instant::now();
    drop(vault);
    let closed = t.elapsed();
    assert!(
        eventually(|| !host_serves(&url) && !host_listens(&url, HOST_PORT)),
        "the close left the server or the forward behind"
    );
    eprintln!("closed in {closed:?}; host clear {:?} after", t.elapsed());

    // A mistyped path is refused rather than served as an empty vault, and the reason is what
    // the window's banner says.
    let missing = format!("{}-nonesuch", address.trim_end_matches('/'));
    let (vault, events) = Vault::open_remote(&missing, VaultConfig::default()).unwrap();
    let why = loop {
        match events.recv_timeout(BUDGET) {
            Ok(Event::Refused(why)) => break why,
            Ok(Event::Disconnected(why)) => panic!("{missing} failed as a link would: {why}"),
            Ok(Event::Connected) => panic!("connected to {missing}"),
            Ok(_) => {}
            Err(_) => panic!("{missing} neither failed nor connected in {BUDGET:?}"),
        }
    };
    let path = format!("{}-nonesuch", url.path.display());
    assert_eq!(
        why,
        format!(
            "cannot open the vault on {}: {path} is not a folder",
            url.host
        )
    );
    drop(vault);

    // The host made ready on its own, as a terminal window's is: a master of the host's rather
    // than a vault's, quietly, so nothing on the way may prompt. The first pass makes the master,
    // the second finds it up and skips the handshake.
    let host = link::host(&url);
    let ctl = ssh::control_path(&host);
    let exit = || {
        let argv = ssh::exit(&host, &ctl);
        let _ = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output();
    };
    exit();
    for pass in ["cold", "warm"] {
        let t = Instant::now();
        link::prepare(&host, &ctl, true, &|what, _| eprintln!("  {what}"))
            .unwrap_or_else(|why| panic!("preparing {}: {why}", host.host));
        eprintln!("host prepared, {pass}, in {:?}", t.elapsed());
    }
    exit();
}

/// Wait for a [`accent_api::Task`] without a runtime of our own: it runs on the library's, and
/// this only asks whether it has finished.
fn wait<T>(task: impl std::future::Future<Output = T>) -> T {
    let mut task = std::pin::pin!(task);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(answer) = task.as_mut().poll(&mut cx) {
            return answer;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Two blank pages and one bookmark, "Second", on the second: the smallest PDF with an outline,
/// written by hand as `accent-api`'s own notes tests write it.
fn bookmarked_pdf() -> Vec<u8> {
    let objs = [
        "<</Type/Catalog/Pages 2 0 R/Outlines 4 0 R>>",
        "<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>",
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>",
        "<</Type/Outlines/First 6 0 R/Last 6 0 R/Count 1>>",
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>",
        "<</Title(Second)/Parent 4 0 R/Dest[5 0 R /XYZ 0 80 0]>>",
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objs.len() + 1
    ));
    for off in &offsets {
        out.push_str(&format!("{off:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    ));
    out.into_bytes()
}

/// Run `script` on the host over a connection of its own, and answer with what it printed.
fn on_host(url: &Url, script: &str) -> String {
    let out = std::process::Command::new("ssh")
        .arg(url.destination())
        .arg("--")
        .arg(script)
        .output()
        .expect("ssh must be on PATH");
    assert!(
        out.status.success(),
        "{script}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn host_listens(url: &Url, port: u16) -> bool {
    !on_host(url, &format!("ss -ltnH 'sport = :{port}'"))
        .trim()
        .is_empty()
}

/// Whether an `accent-cli serve` runs on the host for this vault. The bracket keeps the pattern
/// from matching the shell that carries it.
fn host_serves(url: &Url) -> bool {
    let pattern = ssh::quote(&format!("[s]erve --vault {}", url.path.display()));
    !on_host(url, &format!("pgrep -f {pattern} || true"))
        .trim()
        .is_empty()
}

/// `kill -9` the master behind `ctl`, which is a link dropped without a word to either end.
fn kill_master(url: &Url, ctl: &std::path::Path) {
    let check = std::process::Command::new("ssh")
        .arg("-o")
        .arg(format!("ControlPath={}", ctl.display()))
        .args(["-O", "check", &url.destination()])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&check.stderr);
    let pid = said
        .split("pid=")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .unwrap_or_else(|| panic!("ssh -O check said {said:?}"));
    assert!(
        std::process::Command::new("kill")
            .args(["-9", pid])
            .status()
            .unwrap()
            .success()
    );
}

/// The lost link as the window hears it, with no call made: within three seconds of the kill.
fn wait_lost(events: &Receiver<Event>) -> (String, Duration) {
    let t = Instant::now();
    loop {
        match events.recv_timeout(Duration::from_secs(3).saturating_sub(t.elapsed())) {
            Ok(Event::Disconnected(why)) => return (why, t.elapsed()),
            Ok(_) => {}
            Err(_) => panic!("nothing said of the lost link {:?} after it", t.elapsed()),
        }
    }
}

/// Run `action`, then wait up to five seconds from its start for a `DirsChanged` naming `dir`.
/// What an earlier step left in the channel is dropped first, so it cannot answer for this one.
fn changes_dir(events: &Receiver<Event>, dir: &str, action: impl FnOnce()) -> Duration {
    for _ in events.try_iter() {}
    let t = Instant::now();
    action();
    loop {
        match events.recv_timeout(Duration::from_secs(5).saturating_sub(t.elapsed())) {
            Ok(Event::DirsChanged(dirs)) if dirs.iter().any(|d| d == dir) => return t.elapsed(),
            Ok(_) => {}
            Err(_) => panic!("no DirsChanged naming {dir} within 5 s"),
        }
    }
}

/// Whether `probe` comes true within ten seconds.
fn eventually(probe: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Wait for the event `wanted` picks out, printing the connection's steps on the way. Any other
/// disconnect fails the test.
fn wait_for(events: &Receiver<Event>, wanted: impl Fn(&Event) -> bool) {
    let deadline = Instant::now() + BUDGET;
    while Instant::now() < deadline {
        match events.recv_timeout(Duration::from_millis(500)) {
            Ok(event) if wanted(&event) => return,
            Ok(Event::Connecting { what, .. }) => eprintln!("  {what}"),
            Ok(Event::Disconnected(why)) => panic!("disconnected: {why}"),
            Ok(_) | Err(_) => {}
        }
    }
    panic!("waited {BUDGET:?} for an event that did not come");
}
