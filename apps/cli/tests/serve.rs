//! `accent-cli serve` is what runs on a remote host, so the two things that matter are that it
//! answers over the pipe and that it goes away when the pipe does. A server left behind is the
//! zombie process this phase exists to avoid, and no unit test can see it: the process has to be
//! real.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A vault, an index and a config directory of our own, so the test never reads or writes the
/// caches of whoever is running it.
struct Scratch {
    dir: std::path::PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("accent-serve-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("vault")).unwrap();
        std::fs::write(dir.join("vault/a.md"), "hello [[b]]\n").unwrap();
        std::fs::write(dir.join("vault/b.md"), "#tag\n").unwrap();
        Scratch { dir }
    }

    fn vault(&self) -> std::path::PathBuf {
        self.dir.join("vault")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn serve_answers_over_the_pipe_and_stops_when_it_closes() {
    let scratch = Scratch::new("pipe");
    let mut child = Command::new(env!("CARGO_BIN_EXE_accent-cli"))
        .arg("serve")
        .arg("--vault")
        .arg(scratch.vault())
        .arg("--db")
        .arg(scratch.dir.join("index.db"))
        .env("XDG_CACHE_HOME", scratch.dir.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("accent-cli must be built");

    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());

    let mut ask = |id: u32, method: &str, params: &str| {
        writeln!(
            input,
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#
        )
        .unwrap();
        input.flush().unwrap();
    };
    ask(1, "hello", "[{}, true, true]");

    // Events share the pipe with answers, so read past them. The index is still being built when
    // the first request lands — that is the point of it, and it is why the client waits for the
    // reconcile before asking about links, exactly as the window does.
    let mut next = |want: Option<u64>| -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let mut line = String::new();
            if output.read_line(&mut line).unwrap_or(0) == 0 {
                panic!("serve closed the pipe");
            }
            let msg: serde_json::Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("serve wrote something that is not JSON: {line:?} {e}"));
            let id = msg.get("id").and_then(serde_json::Value::as_u64);
            match want {
                Some(_) if id == want => return msg,
                None if id.is_none() && msg["params"].get("Reconciled").is_some() => {
                    return msg;
                }
                _ => {}
            }
        }
        panic!("serve never said what was expected");
    };

    let hello = next(Some(1));
    assert_eq!(hello["result"]["version"], env!("CARGO_PKG_VERSION"));
    next(None);

    ask(2, "resolve_link", r#"["b"]"#);
    let answer = next(Some(2));
    assert_eq!(answer["result"], "b.md", "{answer}");

    // The window closed. Nothing else tells the server to stop.
    drop(input);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "serve exited with {status}");
                break;
            }
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("serve outlived its stdin: that is the zombie we must not ship");
            }
        }
    }
}
