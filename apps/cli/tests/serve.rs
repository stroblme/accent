//! Test the real remote server process: it must answer over the pipe and exit when stdin closes,
//! leaving no server behind.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Create an isolated vault, index, and config directory.
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
    let seen = "The kettle was already boiling.\n";
    std::fs::write(scratch.vault().join("c.md"), format!("{seen}{seen}")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_accent-cli"))
        .arg("serve")
        .arg("--vault")
        .arg(scratch.vault())
        .arg("--db")
        .arg(scratch.dir.join("index.db"))
        .env("XDG_CACHE_HOME", scratch.dir.join("cache"))
        // The home a shell's `~/` path is read from, which only the host knows.
        .env("HOME", scratch.dir.join("home"))
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

    // Ghost text on the host is this binary's own `ghost`, merl compiled in, which answers once
    // its index is built.
    ask(
        3,
        "open_document",
        r#"["d.md", "markdown", {"Whole": "The kettle was"}, 1]"#,
    );
    next(Some(3));
    let ghost = (4..104).find_map(|id| {
        ask(
            id,
            "inline_completion",
            r#"["d.md", {"line": 0, "character": 14}]"#,
        );
        let answer = next(Some(id.into()));
        std::thread::sleep(Duration::from_millis(50));
        answer["result"].as_str().map(str::to_string)
    });
    assert_eq!(ghost.as_deref(), Some(" already boiling."));

    // The graph crosses as its nodes' keys and edges between their indices.
    ask(
        104,
        "graph_around",
        r#"[["a.md"], {"unresolved":true,"tags":false,"images":false,"orphans":false}]"#,
    );
    let answer = next(Some(104));
    let part = &answer["result"];
    assert_eq!(part["nodes"][0]["key"], "a.md", "{answer}");
    assert_eq!(part["nodes"][1]["key"], "b.md", "{answer}");
    assert_eq!(
        part["edges"],
        serde_json::json!([{"from": 0, "to": 1, "kind": "Link"}])
    );

    // A path a shell's output names, from the host's home or outside the vault: a file is
    // answered with its absolute path and then read as a language server's dependency is, and a
    // folder, inside the vault too, or nothing is answered with null.
    let home = scratch.dir.join("home");
    std::fs::create_dir_all(home.join("notes")).unwrap();
    std::fs::write(home.join("notes/x.md"), "from home\n").unwrap();
    std::fs::write(home.join("unasked.md"), "").unwrap();
    let (file, vault) = (home.join("notes/x.md"), scratch.vault());
    let (file, vault) = (file.to_str().unwrap(), vault.to_str().unwrap());
    let asked = [
        ("~/notes/x.md", serde_json::json!(file)),
        (file, serde_json::json!(file)),
        ("~/notes", serde_json::Value::Null),
        (vault, serde_json::Value::Null),
        ("~/missing.md", serde_json::Value::Null),
    ];
    for (id, (path, want)) in (105..).zip(asked) {
        ask(id, "shell_file", &serde_json::json!([path]).to_string());
        let answer = next(Some(id.into()));
        assert_eq!(answer["result"], want, "{path}: {answer}");
    }
    ask(110, "read_named", &serde_json::json!([file]).to_string());
    let answer = next(Some(110));
    assert_eq!(answer["result"]["Text"]["text"], "from home\n", "{answer}");
    let unasked = home.join("unasked.md");
    ask(111, "read_named", &serde_json::json!([unasked]).to_string());
    let answer = next(Some(111));
    assert!(
        answer.get("error").is_some(),
        "a file nothing named: {answer}"
    );

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
