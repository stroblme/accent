//! Shared vault fixtures and event waits. Separate temporary vault and index directories
//! keep tests away from the user's cache.

use crate::*;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Long enough for a 300 ms watcher debounce plus a reconcile of a handful of files.
pub(crate) const BUDGET: Duration = Duration::from_secs(10);

/// A vault and index in separate temporary directories. `vault` drops first so its
/// worker stops before the directories disappear.
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

    /// Open `root` and wait until it is indexed and watched. Existing files model
    /// changes synced while accent was closed.
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

/// Whether this machine's `python3` has debugpy, which the debugger's end-to-end tests need.
pub(crate) fn has_debugpy() -> bool {
    std::process::Command::new("python3")
        .args(["-c", "import debugpy"])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A debug adapter that needs no debugpy, for what happens around the protocol: `python3`
/// answering every request with an empty success but `evaluate`, which it never answers, like a
/// console running `while True: pass`. In its folder it leaves a file named for each request it
/// reads, answers nothing while a `hold` file is there, and leaves `ended` once its input closes.
pub(crate) fn fake_adapter() -> accent_core::config::DebugConfig {
    const ADAPTER: &str = r#"
import json, os, sys, time
seq = 0
while True:
    length = 0
    while line := sys.stdin.buffer.readline():
        if line == b"\r\n":
            break
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value)
    if not line:
        break
    request = json.loads(sys.stdin.buffer.read(length))
    open(request["command"], "w").close()
    while os.path.exists("hold"):
        time.sleep(0.01)
    if request["command"] == "evaluate":
        continue
    seq += 1
    answer = json.dumps({"seq": seq, "type": "response", "request_seq": request["seq"],
                         "command": request["command"], "success": True, "body": {}}).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(answer) + answer)
    sys.stdout.buffer.flush()
open("ended", "w").close()
"#;
    let argv = ["python3", "-c", ADAPTER].map(String::from).to_vec();
    accent_core::config::DebugConfig {
        adapters: [("python3".to_string(), argv)].into(),
        ..Default::default()
    }
}

pub(crate) fn names(rows: &[FileRow]) -> Vec<String> {
    rows.iter().map(|r| r.rel_path.clone()).collect()
}

pub(crate) const CONFLICT: &str = "Note.sync-conflict-20260903-101500-ABCDEFG.md";
