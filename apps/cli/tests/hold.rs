//! Integration tests for real held shells, pty replay, termination and holder cleanup. Speak the
//! wire protocol directly to avoid testing through the client. Raw mode and SIGWINCH require
//! manual terminal checks.

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
/// Owner used for test shells.
const OWNER: &str = "/home/me/.local/state";

/// Isolate the holder in a scratch `TMPDIR` and terminate it after the test.
struct Holder {
    tmp: PathBuf,
    child: Child,
    version: u8,
}

impl Holder {
    /// Return `None` when the environment has no pty.
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
        let holder = Holder {
            tmp,
            child,
            version: 3,
        };
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

    /// The versioned socket; the scratch dir is ours, so its owner is our uid.
    fn socket(&self) -> PathBuf {
        let uid = std::fs::metadata(&self.tmp).unwrap().uid();
        self.tmp
            .join(format!("accent-{uid}/hold-{}.sock", self.version))
    }

    /// Model an older holder's namespace; the basic v2 terminal wire is unchanged in v3.
    fn version(&mut self, version: u8) {
        let socket = self.socket();
        self.version = version;
        std::fs::rename(
            socket.with_extension("lock"),
            self.socket().with_extension("lock"),
        )
        .unwrap();
        std::fs::rename(socket, self.socket()).unwrap();
    }

    /// Attach to shell `id` the way `attach` does, for [`OWNER`], with a small, known environment.
    fn hello(&self, id: &str, shell: &str) -> UnixStream {
        let conn = UnixStream::connect(self.socket()).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut hello = vec![0, 24, 0, 80];
        let shell = format!("SHELL={shell}");
        let tmp = self.tmp.to_str().unwrap();
        for part in [id, tmp, OWNER, &shell, "PATH=/usr/bin:/bin"] {
            hello.extend(part.as_bytes());
            hello.push(0);
        }
        send(&conn, HELLO, &hello);
        conn
    }

    /// Wait for the holder to exit and remove its socket after the last shell ends.
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

/// Read until output contains `want`, or until exit when `want` is `None`.
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
fn scoped_terminal_requests_do_not_take_over_an_attached_terminal() {
    let Some(mut holder) = Holder::start("scoped") else {
        return;
    };
    let id = "mcp-owned-1";
    let owner = "mcp:test-vault";
    let mut hello = vec![0, 24, 0, 80];
    let tmp = holder.tmp.to_str().unwrap();
    for part in [id, tmp, owner, "SHELL=/bin/sh", "PATH=/usr/bin:/bin"] {
        hello.extend(part.as_bytes());
        hello.push(0);
    }
    let create = UnixStream::connect(holder.socket()).unwrap();
    send(&create, b'c', &hello);
    assert_eq!(frame(&create).0, b'a');

    let attached = UnixStream::connect(holder.socket()).unwrap();
    attached
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(&attached, HELLO, &hello);
    let _ = frame(&attached);

    let read = UnixStream::connect(holder.socket()).unwrap();
    send(
        &read,
        b'q',
        serde_json::json!({"id": id, "owner": owner, "offset": 0, "limit": 1024})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&read).0, OUTPUT);
    let denied = UnixStream::connect(holder.socket()).unwrap();
    send(
        &denied,
        b'q',
        serde_json::json!({"id": id, "owner": "mcp:other", "offset": 0, "limit": 1024})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&denied).0, b'e');

    let write = UnixStream::connect(holder.socket()).unwrap();
    send(
        &write,
        b's',
        serde_json::json!({"id": id, "owner": owner, "input": "echo mcp-scoped-ok\n"})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&write).0, b'a');
    let (seen, exit) = until(&attached, Some("mcp-scoped-ok"));
    assert!(seen.contains("mcp-scoped-ok"));
    assert_eq!(exit, None, "one-shot requests detached the terminal");

    let stop = UnixStream::connect(holder.socket()).unwrap();
    send(
        &stop,
        b't',
        serde_json::json!({"id": id, "owner": owner})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&stop).0, b'a');
    holder.assert_gone();
}

#[test]
fn a_gui_terminal_is_readable_only_while_shared_with_the_vault() {
    let Some(mut holder) = Holder::start("share") else {
        return;
    };
    let attached = holder.hello("gui-1", "/bin/sh");
    let _ = frame(&attached);
    let before = UnixStream::connect(holder.socket()).unwrap();
    send(
        &before,
        b'q',
        serde_json::json!({"id": "gui-1", "owner": "mcp:any", "offset": 0})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&before).0, b'e');

    assert!(
        cli(&holder.tmp)
            .args(["share", "gui-1", "--vault"])
            .arg(&holder.tmp)
            .status()
            .unwrap()
            .success()
    );
    let held: serde_json::Value =
        serde_json::from_slice(&cli(&holder.tmp).arg("held").output().unwrap().stdout).unwrap();
    let owner = held["shared_with"].as_str().unwrap();
    let during = UnixStream::connect(holder.socket()).unwrap();
    send(
        &during,
        b'q',
        serde_json::json!({"id": "gui-1", "owner": owner, "offset": 0})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&during).0, OUTPUT);

    assert!(
        cli(&holder.tmp)
            .args(["revoke", "gui-1"])
            .status()
            .unwrap()
            .success()
    );
    let after = UnixStream::connect(holder.socket()).unwrap();
    send(
        &after,
        b'q',
        serde_json::json!({"id": "gui-1", "owner": owner, "offset": 0})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&after).0, b'e');
    send(&attached, INPUT, b"echo still-attached\n");
    assert!(
        until(&attached, Some("still-attached"))
            .0
            .contains("still-attached")
    );
    assert!(
        cli(&holder.tmp)
            .args(["kill", "gui-1"])
            .status()
            .unwrap()
            .success()
    );
    holder.assert_gone();
}

#[test]
fn attach_existing_rejects_missing_and_other_vault_shells_without_creating() {
    let Some(mut holder) = Holder::start("existing") else {
        return;
    };
    let missing = cli(&holder.tmp)
        .args(["attach", "--existing", "--vault"])
        .arg(&holder.tmp)
        .arg("missing")
        .status()
        .unwrap();
    assert_eq!(missing.code(), Some(254));
    let listed = cli(&holder.tmp).arg("held").output().unwrap();
    assert!(listed.stdout.is_empty(), "missing attach created a shell");

    let attached = holder.hello("gui-other", "/bin/sh");
    let _ = frame(&attached);
    let denied = cli(&holder.tmp)
        .args(["attach", "--existing", "--vault"])
        .arg(&holder.tmp)
        .arg("gui-other")
        .status()
        .unwrap();
    assert_eq!(denied.code(), Some(254));
    send(&attached, INPUT, b"echo still-owned-by-gui\n");
    assert!(
        until(&attached, Some("still-owned-by-gui"))
            .0
            .contains("still-owned-by-gui")
    );
    assert!(
        cli(&holder.tmp)
            .args(["kill", "gui-other"])
            .status()
            .unwrap()
            .success()
    );
    holder.assert_gone();
}

#[test]
fn scoped_read_preserves_utf8_with_single_byte_page_and_lossless_bytes() {
    let Some(mut holder) = Holder::start("utf8") else {
        return;
    };
    let id = "utf8-owned";
    let owner = "mcp:utf8";
    let mut hello = vec![0, 24, 0, 80];
    let tmp = holder.tmp.to_str().unwrap();
    for part in [id, tmp, owner, "SHELL=/bin/sh", "PATH=/usr/bin:/bin"] {
        hello.extend(part.as_bytes());
        hello.push(0);
    }
    let create = UnixStream::connect(holder.socket()).unwrap();
    send(&create, b'c', &hello);
    assert_eq!(frame(&create).0, b'a');
    let mut cursor = {
        let read = UnixStream::connect(holder.socket()).unwrap();
        send(
            &read,
            b'q',
            serde_json::json!({"id": id, "owner": owner, "offset": 0})
                .to_string()
                .as_bytes(),
        );
        let (_, payload) = frame(&read);
        serde_json::from_slice::<serde_json::Value>(&payload).unwrap()["next_offset"]
            .as_u64()
            .unwrap()
    };
    let write = UnixStream::connect(holder.socket()).unwrap();
    send(
        &write,
        b's',
        serde_json::json!({"id": id, "owner": owner, "input": "printf '€'\n"})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&write).0, b'a');
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let read = UnixStream::connect(holder.socket()).unwrap();
        send(
            &read,
            b'q',
            serde_json::json!({"id": id, "owner": owner, "offset": cursor, "limit": 1})
                .to_string()
                .as_bytes(),
        );
        let (_, payload) = frame(&read);
        let page: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        if page["output"] == "€" {
            assert_eq!(page["next_offset"].as_u64().unwrap() - cursor, 3);
            assert_eq!(page["output_base64"], "4oKs");
            break;
        }
        assert_ne!(page["output"], "�", "UTF-8 was split: {page}");
        cursor = page["next_offset"].as_u64().unwrap();
        assert!(
            Instant::now() < deadline,
            "UTF-8 page was not intact: {page}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        cli(&holder.tmp)
            .args(["kill", id])
            .status()
            .unwrap()
            .success()
    );
    holder.assert_gone();
}

#[test]
fn a_nonreading_terminal_cannot_hold_scoped_input_or_screen_access() {
    let Some(mut holder) = Holder::start("nonreading") else {
        return;
    };
    let id = "mcp-nonreading";
    let owner = "mcp:slow";
    let mut hello = vec![0, 24, 0, 80];
    let tmp = holder.tmp.to_str().unwrap();
    for part in [id, tmp, owner, "SHELL=/bin/sh", "PATH=/usr/bin:/bin"] {
        hello.extend(part.as_bytes());
        hello.push(0);
    }
    let create = UnixStream::connect(holder.socket()).unwrap();
    send(&create, b'c', &hello);
    assert_eq!(frame(&create).0, b'a');
    let command = UnixStream::connect(holder.socket()).unwrap();
    send(
        &command,
        b's',
        serde_json::json!({"id": id, "owner": owner, "input": "stty -icanon -echo; touch ready; exec sleep 5\n"})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&command).0, b'a');
    let ready_by = Instant::now() + Duration::from_secs(2);
    while !holder.tmp.join("ready").exists() {
        assert!(
            Instant::now() < ready_by,
            "the nonreading program did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let large = UnixStream::connect(holder.socket()).unwrap();
    large
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    send(
        &large,
        b's',
        serde_json::json!({"id": id, "owner": owner, "input": "x".repeat(64 * 1024)})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(
        frame(&large).0,
        b'e',
        "input must time out while the PTY is full"
    );
    let read = UnixStream::connect(holder.socket()).unwrap();
    read.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    send(
        &read,
        b'q',
        serde_json::json!({"id": id, "owner": owner, "offset": 0})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(frame(&read).0, OUTPUT);
    assert!(
        cli(&holder.tmp)
            .args(["kill", id])
            .status()
            .unwrap()
            .success()
    );
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
    assert_eq!(held["owner"], OWNER);
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
fn an_upgrade_lists_resumes_and_kills_the_previous_holders_shell() {
    let Some(mut holder) = Holder::start("upgrade") else {
        return;
    };
    let conn = holder.hello("old", "/bin/sh");
    send(&conn, INPUT, b"echo before-$$; sleep 1; echo after-$$\n");
    until(&conn, Some("before-"));
    drop(conn);
    holder.version(2);

    let listed = cli(&holder.tmp).arg("held").output().unwrap();
    assert!(listed.status.success());
    let old: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(old["id"], "old");
    let pid = old["pid"].as_u64().unwrap();
    let log = holder.tmp.join("attach.log");
    let mut attached = cli(&holder.tmp)
        .env("SHELL", "/bin/sh")
        .args(["attach", "--cwd"])
        .arg(&holder.tmp)
        .arg("old")
        .stdin(Stdio::piped())
        .stdout(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    // Child::wait closes its stored stdin, which would detach before the holder reports exit.
    let _input = attached.stdin.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = std::fs::read_to_string(&log).unwrap();
        if output.contains(&format!("after-{pid}")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "old job did not resume: {output}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!holder.socket().with_file_name("hold-3.sock").exists());
    let shared = cli(&holder.tmp)
        .args(["share", "old", "--vault"])
        .arg(&holder.tmp)
        .output()
        .unwrap();
    assert!(
        !shared.status.success(),
        "legacy shells cannot use MCP sharing"
    );
    assert!(
        cli(&holder.tmp)
            .args(["kill", "old"])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(attached.wait().unwrap().code(), Some(129));
    holder.child.wait().unwrap();
}

#[test]
fn an_unknown_live_holder_prevents_a_replacement_shell() {
    let Some(mut holder) = Holder::start("unknown-wire") else {
        return;
    };
    let conn = holder.hello("old", "/bin/sh");
    holder.version(99);
    let out = cli(&holder.tmp)
        .env("SHELL", "/bin/sh")
        .args(["attach", "--cwd"])
        .arg(&holder.tmp)
        .arg("old")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(254));
    assert!(!holder.socket().with_file_name("hold-3.sock").exists());
    send(&conn, INPUT, b"echo still-running; exit\n");
    assert!(until(&conn, None).0.contains("still-running"));
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

/// A kill arriving before HELLO must prevent creation of a shell after its tab closes.
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

/// With no holder running, kill must start one to record the request before a pending attach.
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
