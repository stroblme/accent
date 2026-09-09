//! The fixture the vault's own tests share: a vault in a tempdir with its index in a second
//! one, so nothing here can reach the machine's real cache, and the event waits every test
//! spells the same way.

use crate::*;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Long enough for a 300 ms watcher debounce plus a reconcile of a handful of files.
pub(crate) const BUDGET: Duration = Duration::from_secs(10);

/// A vault in a tempdir with its index in a second one, so no test can reach the user's
/// real cache. `vault` is declared first: dropping it stops the worker before the
/// directories it walks disappear.
pub(crate) struct Fixture {
    pub(crate) vault: Vault,
    pub(crate) events: Receiver<Event>,
    pub(crate) root: TempDir,
    pub(crate) _cache: TempDir,
}

impl Fixture {
    pub(crate) fn open(cfg: VaultConfig) -> Fixture {
        Fixture::open_dir(tempfile::tempdir().unwrap(), cfg)
    }

    /// Opens `root` and waits for the first reconcile, so the vault is indexed and watched.
    /// The directory may already hold files: that is the state Syncthing leaves behind while
    /// accent is closed.
    pub(crate) fn open_dir(root: TempDir, cfg: VaultConfig) -> Fixture {
        let cache = tempfile::tempdir().unwrap();
        let (vault, events) =
            Vault::open_at(root.path(), &cache.path().join("index.db"), cfg).unwrap();
        let f = Fixture {
            vault,
            events,
            root,
            _cache: cache,
        };
        assert!(
            f.wait(|e| matches!(e, Event::Reconciled(_))).is_some(),
            "the initial reconcile never finished"
        );
        f
    }

    pub(crate) fn write(&self, rel: &str, text: &str) {
        let path = self.vault.root().join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    pub(crate) fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.vault.root().join(rel)).unwrap()
    }

    pub(crate) fn wait(&self, pred: impl Fn(&Event) -> bool) -> Option<Event> {
        wait_for(&self.events, pred, BUDGET)
    }
}

/// Drain events until one matches, or the budget runs out.
pub(crate) fn wait_for(
    rx: &Receiver<Event>,
    pred: impl Fn(&Event) -> bool,
    budget: Duration,
) -> Option<Event> {
    let deadline = Instant::now() + budget;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) if pred(&e) => return Some(e),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Poll index state, which changes without an event of its own.
pub(crate) fn poll_until(mut f: impl FnMut() -> bool, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

pub(crate) fn names(rows: &[FileRow]) -> Vec<String> {
    rows.iter().map(|r| r.rel_path.clone()).collect()
}

pub(crate) const CONFLICT: &str = "Note.sync-conflict-20260903-101500-ABCDEFG.md";
