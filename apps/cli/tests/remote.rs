//! The remote backend against a real host, which is the only place several of its parts are
//! exercised at all: ssh's own multiplexing, a shell that is not ours, and a server binary that
//! has to start on a machine older than the one that built it.
//!
//! Opt-in, because it needs a host and a key: set `ACCENT_TEST_REMOTE` to an `ssh://` address
//! whose path may be created and deleted. Without it the test says so and passes, so `make check`
//! is unchanged for everyone else.
//!
//!     make server
//!     ACCENT_TEST_REMOTE=ssh://myhost/home/me/accent-probe cargo test -p accent-cli --test remote

use std::time::{Duration, Instant};

use accent_api::{Event, Vault, VaultConfig};

/// Long enough for a master, an upload of six megabytes and a first reconcile.
const BUDGET: Duration = Duration::from_secs(120);

#[test]
fn a_remote_vault_connects_indexes_and_answers() {
    let Ok(address) = std::env::var("ACCENT_TEST_REMOTE") else {
        eprintln!("set ACCENT_TEST_REMOTE=ssh://host/path to run this");
        return;
    };
    let url = accent_api::ssh::parse(&address).expect("a usable address");

    // A vault of known shape, made over the same connection the app will use.
    let setup = format!(
        "rm -rf {p} && mkdir -p {p} && printf 'hello [[b]]\\n' > {p}/a.md && printf '#tag\\n' > {p}/b.md",
        p = accent_api::ssh::quote(&url.path.to_string_lossy())
    );
    let out = std::process::Command::new("ssh")
        .arg(url.destination())
        .arg("--")
        .arg(&setup)
        .output()
        .expect("ssh must be on PATH");
    assert!(
        out.status.success(),
        "preparing the remote vault: {}",
        String::from_utf8_lossy(&out.stderr)
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

    let mut names: Vec<String> = vault
        .list_dir("")
        .unwrap()
        .into_iter()
        .map(|r| r.rel_path)
        .collect();
    names.sort();
    assert_eq!(names, ["a.md", "b.md"]);

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

    // And a delete, which on a remote vault is permanent by design.
    vault.delete("b.md").unwrap();
    assert!(!vault.exists("b.md"));
}
