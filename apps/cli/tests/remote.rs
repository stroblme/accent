//! The remote backend against a real host, which is the only place several of its parts are
//! exercised at all: ssh's own multiplexing, a shell that is not ours, and a server binary that
//! has to start on a machine older than the one that built it.
//!
//! Opt-in, because it needs a host and a key: set `ACCENT_TEST_REMOTE` to an `ssh://` address
//! whose path may be created and deleted. Without it the test says so and passes, so `make check`
//! is unchanged for everyone else. The host needs `git` and `ss`, and [`HOST_PORT`] free.
//!
//!     make server
//!     ACCENT_TEST_REMOTE=ssh://myhost/tmp/accent-probe cargo test -p accent-cli --test remote
//!
//! One test function, because the first connection after a rebuild uploads the server, and two
//! tests connecting at once would race each other's upload.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

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
    // dependency tree the index leaves out, and a repository whose origin takes 20 s to answer a
    // fetch, which is past both the fetch's own cap and the RPC's deadline.
    on_host(
        &url,
        &format!(
            "rm -rf {p} && mkdir -p {p}/node_modules/pkg && printf 'hello [[b]]\\n' > {p}/a.md \
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
    assert_eq!(names, ["a.md", "b.md", "node_modules"]);
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

    // A dropped link: the master dies, and the server and the forward go with it.
    let check = std::process::Command::new("ssh")
        .arg("-o")
        .arg(format!("ControlPath={}", remote.control_path().display()))
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
    assert!(
        eventually(|| !host_listens(&url, HOST_PORT)),
        "the forward outlived its master"
    );
    // The next call finds the link gone, as the window's would, and a reconnect makes a new
    // master that has to carry the forward again by the time it says it is connected.
    assert!(eventually(|| vault.list_dir("").is_err()));
    wait_for(&events, |e| matches!(e, Event::Disconnected(_)));
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
