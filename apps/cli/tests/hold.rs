//! `accent-cli hold` keeps shells between windows, so what matters is what no unit test can see:
//! that a real shell on a real pty runs, comes back with its screen after its terminal went away,
//! ends when told to, and that the holder leaves nothing behind once the last shell is gone.
//!
//! The protocol is spoken by hand, as tests/serve.rs does, so this checks the wire and not our
//! own client. `attach`'s raw mode and SIGWINCH need a terminal and are checked by hand.

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const HELLO: u8 = b'h';
const INPUT: u8 = b'i';
const OUTPUT: u8 = b'o';
const EXIT: u8 = b'x';

/// A `TMPDIR` of our own, so the holder under test never meets the one of whoever runs the tests,
/// and a holder that outlives a failed test is ended with it.
struct Holder {
    tmp: PathBuf,
    child: Child,
}

impl Holder {
    /// `None` where no pty can be had (some containers and sandboxes): there is nothing to test.
    fn start(name: &str) -> Option<Holder> {
        if std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/ptmx")
            .is_err()
        {
            eprintln!("skipped: /dev/ptmx cannot be opened here");
            return None;
        }
        let tmp = std::env::temp_dir().join(format!("accent-hold-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let child = cli(&tmp)
            .arg("hold")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("accent-cli must be built");
        let holder = Holder { tmp, child };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !holder.socket().exists() {
            assert!(
                Instant::now() < deadline,
                "the holder never made its socket"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Some(holder)
    }

    /// `$TMPDIR/accent-<uid>/hold-1.sock`; the scratch dir is ours, so its owner is our uid.
    fn socket(&self) -> PathBuf {
        let uid = std::fs::metadata(&self.tmp).unwrap().uid();
        self.tmp.join(format!("accent-{uid}/hold-1.sock"))
    }

    /// Attach to shell `id` the way `attach` does, with a small, known environment.
    fn hello(&self, id: &str, shell: &str) -> UnixStream {
        let conn = UnixStream::connect(self.socket()).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut hello = vec![0, 24, 0, 80];
        let shell = format!("SHELL={shell}");
        for part in [id, self.tmp.to_str().unwrap(), &shell, "PATH=/usr/bin:/bin"] {
            hello.extend(part.as_bytes());
            hello.push(0);
        }
        send(&conn, HELLO, &hello);
        conn
    }

    /// The holder exits on its own once its last shell has, and takes its socket with it.
    fn assert_gone(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "the holder outlived its last shell"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!self.socket().exists(), "the holder left its socket behind");
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn cli(tmp: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_accent-cli"));
    cmd.env("TMPDIR", tmp).stderr(Stdio::null());
    cmd
}

fn send(mut conn: &UnixStream, tag: u8, payload: &[u8]) {
    let mut frame = vec![tag];
    frame.extend((payload.len() as u32).to_be_bytes());
    frame.extend(payload);
    conn.write_all(&frame).unwrap();
}

fn frame(mut conn: &UnixStream) -> (u8, Vec<u8>) {
    let mut head = [0; 5];
    conn.read_exact(&mut head).expect("the holder said nothing");
    let mut payload = vec![0; u32::from_be_bytes(head[1..].try_into().unwrap()) as usize];
    conn.read_exact(&mut payload).unwrap();
    (head[0], payload)
}

/// Read output until it holds `want`, or with `None` until the shell ends; its status if it did.
fn until(conn: &UnixStream, want: Option<&str>) -> (String, Option<i32>) {
    let mut seen = String::new();
    loop {
        match frame(conn) {
            (OUTPUT, bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
            (EXIT, code) => return (seen, Some(i32::from_be_bytes(code.try_into().unwrap()))),
            (tag, _) => panic!("unexpected frame {:?}", tag as char),
        }
        if want.is_some_and(|want| seen.contains(want)) {
            return (seen, None);
        }
    }
}

#[test]
fn a_held_shell_runs_and_says_how_it_ended() {
    let Some(mut holder) = Holder::start("ends") else {
        return;
    };
    let conn = holder.hello("t1", "/bin/sh");
    // The echo of what was typed holds `40+2`, never `42`.
    send(&conn, INPUT, b"echo $((40+2)); exit 3\n");
    let (seen, code) = until(&conn, None);
    assert!(seen.contains("42"), "{seen:?}");
    assert_eq!(code, Some(3));
    holder.assert_gone();
}

#[test]
fn a_detached_shell_is_redrawn_on_reattach_and_ends_on_kill() {
    let Some(mut holder) = Holder::start("back") else {
        return;
    };
    let conn = holder.hello("t2", "/bin/sh");
    send(&conn, INPUT, b"echo $((40+2))\n");
    until(&conn, Some("42"));
    // The terminal went away; the shell did not.
    drop(conn);

    let conn = holder.hello("t2", "/bin/sh");
    let (replay, code) = until(&conn, Some("42"));
    assert_eq!(code, None, "the shell ended on detach: {replay:?}");

    let held = cli(&holder.tmp).arg("held").output().unwrap();
    assert!(held.status.success());
    let held: serde_json::Value = serde_json::from_slice(&held.stdout).unwrap();
    assert_eq!(held["id"], "t2");
    assert_eq!(held["attached"], true);
    let cwd = std::fs::canonicalize(&holder.tmp).unwrap();
    assert_eq!(held["cwd"], cwd.to_str().unwrap());

    assert!(
        cli(&holder.tmp)
            .args(["kill", "t2"])
            .status()
            .unwrap()
            .success()
    );
    // SIGHUP: 128 + 1, as a shell reports a child that died of it.
    assert_eq!(until(&conn, None).1, Some(129));
    holder.assert_gone();
}

#[test]
fn a_shell_that_cannot_start_says_why_and_leaves_no_holder() {
    let Some(mut holder) = Holder::start("fails") else {
        return;
    };
    let conn = holder.hello("t3", "/nonexistent/sh");
    let (said, code) = until(&conn, None);
    assert!(said.contains("cannot start /nonexistent/sh"), "{said:?}");
    // 254, never ssh's 255: the window keeps this tab to show the line above.
    assert_eq!(code, Some(254));
    holder.assert_gone();
}

/// Close Tab in the moment between a tab's `attach` connecting and the holder starting its shell:
/// the kill arrives first and finds nothing to end. The HELLO after it must not start a shell
/// that nobody will ever attach to again.
#[test]
fn a_shell_killed_before_it_started_never_starts() {
    let Some(holder) = Holder::start("early") else {
        return;
    };
    assert!(
        cli(&holder.tmp)
            .args(["kill", "t4"])
            .status()
            .unwrap()
            .success()
    );
    // `kill` returns once it has asked, not once the holder has acted on it.
    std::thread::sleep(Duration::from_millis(200));
    let conn = holder.hello("t4", "/bin/sh");
    // Ended the way a kill ends one, by a hangup, and before any output.
    assert_eq!(frame(&conn), (EXIT, 129i32.to_be_bytes().to_vec()));
    let held = cli(&holder.tmp).arg("held").output().unwrap();
    assert!(held.stdout.is_empty(), "{held:?}");
}

/// The same with no holder running: the kill has to start one, or the `attach` on its way would
/// start it, and the shell with it, after the kill had found nobody to tell.
#[test]
fn a_kill_with_no_holder_running_still_comes_first() {
    let Some(mut stopped) = Holder::start("none") else {
        return;
    };
    // Gone the hard way, as a crashed one would be, leaving its socket behind.
    stopped.child.kill().unwrap();
    stopped.child.wait().unwrap();
    assert!(
        cli(&stopped.tmp)
            .args(["kill", "t5"])
            .status()
            .unwrap()
            .success()
    );
    std::thread::sleep(Duration::from_millis(200));
    let conn = stopped.hello("t5", "/bin/sh");
    assert_eq!(frame(&conn), (EXIT, 129i32.to_be_bytes().to_vec()));
    // The holder the kill started leaves as any other does, once its last shell has.
    let conn = stopped.hello("t6", "/bin/sh");
    send(&conn, INPUT, b"exit\n");
    assert_eq!(until(&conn, None).1, Some(0));
    let deadline = Instant::now() + Duration::from_secs(5);
    while stopped.socket().exists() {
        assert!(
            Instant::now() < deadline,
            "the holder outlived its last shell"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
