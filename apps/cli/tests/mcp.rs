//! `accent-cli mcp` as a client meets it: a real process, spoken to in JSON-RPC lines on its
//! stdin, in both of the protocol's lifecycles, and gone once its stdin is.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A vault with a symlink out of it, an index and config and cache directories of our own, so
/// the test never touches the caches of whoever runs it.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("accent-mcp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["vault", "outside", "config", "cache"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let write = |rel: &str, text: &str| std::fs::write(dir.join(rel), text).unwrap();
        write(
            "vault/a.md",
            "# Title\nintro\n## Part\nbody about kumquat [[b]]\n",
        );
        write("vault/b.md", "#tag\nsee [[a#Part]]\n");
        write("outside/secret.md", "secret kumquat\n");
        std::os::unix::fs::symlink(dir.join("outside"), dir.join("vault/link")).unwrap();
        Scratch { dir }
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join("vault").join(rel)).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The server, and the two ends of its pipe.
struct Client {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    id: u64,
}

impl Client {
    fn start(scratch: &Scratch, args: &[&str]) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_accent-cli"))
            .arg("mcp")
            .arg("--vault")
            .arg(scratch.dir.join("vault"))
            .arg("--db")
            .arg(scratch.dir.join("index.db"))
            .args(args)
            .env("XDG_CONFIG_HOME", scratch.dir.join("config"))
            .env("XDG_CACHE_HOME", scratch.dir.join("cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("accent-cli must be built");
        Client {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            id: 0,
        }
    }

    fn send(&mut self, msg: Value) {
        let input = self.input.as_mut().unwrap();
        writeln!(input, "{msg}").unwrap();
        input.flush().unwrap();
    }

    /// Ask, and read past anything else the server says until it answers.
    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let id = self.id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(
                self.output.read_line(&mut line).unwrap() > 0,
                "the server closed its stdout"
            );
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("not JSON on stdout: {line:?} {e}"));
            if msg["id"] == id {
                return msg;
            }
        }
    }

    /// A tool's answer: its text blocks, and whether it is an error.
    fn call(&mut self, tool: &str, args: Value) -> (Vec<String>, bool) {
        let msg = self.ask("tools/call", json!({"name": tool, "arguments": args}));
        let result = &msg["result"];
        let texts = result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("no content: {msg}"))
            .iter()
            .map(|c| c["text"].as_str().unwrap_or_default().to_string())
            .collect();
        (texts, result["isError"] == true)
    }

    /// Close stdin, which is all a client does to end it, and wait for the process to go.
    fn close(mut self) {
        drop(self.input.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "mcp exited with {status}");
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("mcp outlived its stdin");
    }
}

fn names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("no tools: {list}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

const READ_TOOLS: [&str; 6] = [
    "list_backlinks",
    "list_tags",
    "pdf_annotations",
    "read_note",
    "resolve_link",
    "search_notes",
];

#[test]
fn mcp_answers_after_initialize_and_holds_to_the_vault() {
    let scratch = Scratch::new("init");
    let mut c = Client::start(&scratch, &[]);
    let init = c.ask(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
               "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let tools = names(&c.ask("tools/list", json!({})));
    for tool in READ_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "{tool} missing: {tools:?}"
        );
    }

    // The symlinked note says kumquat too, and is no part of the vault an agent is shown.
    let (hits, error) = c.call("search_notes", json!({"query": "kumquat"}));
    assert!(!error, "{hits:?}");
    let hits: Value = serde_json::from_str(&hits[0]).unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 1, "{hits}");
    assert_eq!(hits[0]["path"], "a.md");
    assert_eq!(hits[0]["line"], 4);

    let (read, error) = c.call("read_note", json!({"path": "a.md", "heading": "part"}));
    assert!(!error, "{read:?}");
    let head: Value = serde_json::from_str(&read[0]).unwrap();
    assert!(head["etag"].is_string(), "{head}");
    assert_eq!(read[1], "body about kumquat [[b]]\n");

    for path in ["../a.md", "link/secret.md", "/etc/passwd", ".git/config"] {
        let (said, error) = c.call("read_note", json!({"path": path}));
        assert!(error, "{path} was read: {said:?}");
    }

    let (link, _) = c.call("resolve_link", json!({"target": "[[a#Part|here]]"}));
    assert_eq!(link[0], r#"{"line":3,"path":"a.md"}"#);
    c.close();
}

/// The 2026-07-28 lifecycle has no `initialize`: every request says its version and the client's
/// capabilities in its `_meta`.
#[test]
fn mcp_answers_a_client_that_never_initializes() {
    let scratch = Scratch::new("stateless");
    let mut c = Client::start(&scratch, &[]);
    let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                      "io.modelcontextprotocol/clientCapabilities": {}});
    let list = c.ask("tools/list", json!({"_meta": meta}));
    let tools = names(&list);
    for tool in READ_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "{tool} missing: {tools:?}"
        );
    }
    let read = c.ask(
        "tools/call",
        json!({"name": "read_note", "arguments": {"path": "b.md"}, "_meta": meta}),
    );
    assert_eq!(read["result"]["content"][1]["text"], scratch.read("b.md"));
    c.close();
}
