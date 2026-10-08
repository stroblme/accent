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
        for sub in ["vault/Templates", "outside", "config", "cache"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let write = |rel: &str, text: &str| std::fs::write(dir.join(rel), text).unwrap();
        write(
            "vault/a.md",
            "# Title\nintro\n## Part\nbody about kumquat [[b]]\n",
        );
        write("vault/b.md", "#tag\nsee [[a#Part]]\n");
        write(
            "vault/Templates/Day.md",
            "---\naccent-target: Days/{{title}}.md\n---\n# Made\n",
        );
        write("outside/secret.md", "secret kumquat\n");
        std::os::unix::fs::symlink(dir.join("outside"), dir.join("vault/link")).unwrap();
        Scratch { dir }
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join("vault").join(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        self.dir.join(rel).exists()
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

    /// The 2025-11-25 lifecycle: `initialize`, then the notification that ends it.
    fn initialize(&mut self) -> Value {
        let init = self.ask(
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}),
        );
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        init
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

const READ_TOOLS: [&str; 10] = [
    "explore",
    "list_backlinks",
    "list_dir",
    "list_tags",
    "missing_notes",
    "pdf_annotations",
    "read_note",
    "recent_changes",
    "resolve_link",
    "search_notes",
];

#[test]
fn mcp_answers_after_initialize_and_holds_to_the_vault() {
    let scratch = Scratch::new("init");
    let mut c = Client::start(&scratch, &[]);
    let init = c.initialize();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");

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

/// Every write is checked against the read before it: a patch with an etag the note has moved
/// past changes nothing, and a file that exists is overwritten only with one. Nothing is written
/// where a symlink leads out of the vault.
#[test]
fn mcp_writes_only_over_what_it_read() {
    let scratch = Scratch::new("write");
    let mut c = Client::start(&scratch, &[]);
    c.initialize();
    assert_eq!(names(&c.ask("tools/list", json!({}))).len(), 13);

    let (read, _) = c.call("read_note", json!({"path": "a.md"}));
    let etag = serde_json::from_str::<Value>(&read[0]).unwrap()["etag"].clone();
    let patch = json!({"path": "a.md", "heading": "Part", "mode": "replace",
                       "content": "new body", "etag": etag});
    // Once the index is up, a write is in it by the time the call answers.
    c.call("search_notes", json!({"query": "kumquat"}));
    let (said, error) = c.call("patch_note", patch.clone());
    assert!(!error, "{said:?}");
    let patched = "# Title\nintro\n## Part\nnew body\n";
    assert_eq!(scratch.read("a.md"), patched);
    let (hits, _) = c.call("search_notes", json!({"query": "new body"}));
    assert!(hits[0].contains(r#""path":"a.md""#), "{hits:?}");
    let (said, error) = c.call("patch_note", patch);
    assert!(error && said[0].contains("changed since"), "{said:?}");
    assert_eq!(scratch.read("a.md"), patched);

    let (said, error) = c.call(
        "write_note",
        json!({"path": "new/n.md", "content": "fresh\n"}),
    );
    assert!(!error && said[0].contains(r#""created":true"#), "{said:?}");
    assert_eq!(scratch.read("new/n.md"), "fresh\n");
    let (said, error) = c.call("write_note", json!({"path": "a.md", "content": "gone\n"}));
    assert!(
        error,
        "an existing note was overwritten without its etag: {said:?}"
    );
    let (_, error) = c.call("write_note", json!({"path": "link/x.md", "content": "x"}));
    assert!(error && !scratch.exists("outside/x.md"));

    let (said, error) = c.call(
        "create_note_from_template",
        json!({"template": "Templates/Day.md"}),
    );
    assert!(!error, "{said:?}");
    assert_eq!(said[0], r#"{"created":true,"path":"Days/Day.md"}"#);
    assert_eq!(scratch.read("Days/Day.md"), "# Made\n");
    c.close();
}

/// The cards of `explore`: a note linked from two of the files a word finds outranks one found
/// the same way that nothing links, a path alone is answered with its whole card, and the
/// answer keeps to its budget.
#[test]
fn explore_ranks_by_links_and_answers_a_path_with_its_card() {
    let scratch = Scratch::new("explore");
    let write = |rel: &str, text: &str| std::fs::write(scratch.dir.join("vault").join(rel), text);
    write("plain.md", "quince\n").unwrap();
    write("x.md", "quince [[linked]]\n").unwrap();
    write("y.md", "quince [[linked]]\n").unwrap();
    let long = "a longer note that says more about other things than fruit ".repeat(4);
    write("linked.md", &format!("{long}quince\n")).unwrap();
    let big: String = (1..=500).map(|n| format!("quince line {n}\n")).collect();
    write("big.md", &big).unwrap();
    let mut c = Client::start(&scratch, &[]);
    c.initialize();

    let (said, error) = c.call("explore", json!({"query": "quince"}));
    assert!(!error, "{said:?}");
    let at = |rel: &str| said[0].find(&format!("### `{rel}`"));
    assert!(
        at("linked.md").unwrap() < at("plain.md").unwrap(),
        "{}",
        said[0]
    );

    let (said, _) = c.call("explore", json!({"query": "a.md"}));
    let card = &said[0];
    for row in [
        "### `a.md` — Title (4 lines)",
        "Outline:\n  1  # Title\n  3  ## Part\n",
        "Links out:\n  4 → `b.md`\n",
        "Backlinks:\n  `b.md`:2  see [[a#Part]]\n",
        "1\t# Title\n2\tintro\n3\t## Part\n4\tbody about kumquat [[b]]\n",
    ] {
        assert!(card.contains(row), "{row:?} not in {card}");
    }

    let (said, _) = c.call("explore", json!({"query": "big.md", "max_chars": 4000}));
    assert!(said[0].len() <= 4000, "{} chars", said[0].len());
    assert!(said[0].contains("(cut: read_note `big.md`"), "{}", said[0]);
    c.close();
}

/// `explore` on code: a declaration named is shown with its callers, what it calls, whether a
/// test reaches it and the calls from it to another named, and a code file's card is its
/// outline.
#[test]
fn explore_follows_code_by_name() {
    let scratch = Scratch::new("code");
    let write = |rel: &str, text: &str| std::fs::write(scratch.dir.join("vault").join(rel), text);
    write(
        "lib.rs",
        "pub struct Index;\n\nimpl Index {\n    /// Finds the links.\n    pub fn backlinks(&self) -> \
         usize {\n        helper()\n    }\n}\n\nfn helper() -> usize {\n    1\n}\n\n\
         #[cfg(test)]\nmod tests {\n    #[test]\n    fn counts() {\n        \
         assert_eq!(super::Index.backlinks(), 1);\n    }\n}\n",
    )
    .unwrap();
    write(
        "use.rs",
        "fn caller(ix: &Index) -> usize {\n    ix.backlinks()\n}\n",
    )
    .unwrap();
    let mut c = Client::start(&scratch, &[]);
    c.initialize();

    let (said, error) = c.call("explore", json!({"query": "Index::backlinks helper"}));
    assert!(!error, "{said:?}");
    for row in [
        "- `Index::backlinks` method (lib.rs:5) — 2 calls in `lib.rs`, `use.rs`; tested by \
         `counts` (lib.rs)",
        "  calls `helper`",
        "**Call paths**\n- `backlinks` → `helper`",
        "4\t    /// Finds the links.\n5\t    pub fn backlinks(&self) -> usize {",
    ] {
        assert!(said[0].contains(row), "{row:?} not in {}", said[0]);
    }

    let (said, _) = c.call("explore", json!({"query": "lib.rs"}));
    assert!(
        said[0].contains(
            "Outline:\n  1  struct Index\n  5  method Index::backlinks\n  10  fn helper\n  \
             15  mod tests\n  17  fn counts\n"
        ),
        "{}",
        said[0]
    );
    c.close();
}

/// The listings beside `explore`: a folder, the files changed last, and the notes links wait for.
#[test]
fn mcp_lists_folders_recent_files_and_missing_notes() {
    let scratch = Scratch::new("lists");
    std::fs::write(
        scratch.dir.join("vault/c.md"),
        "[[Later]]\nand [[Later#Part]]\n",
    )
    .unwrap();
    let mut c = Client::start(&scratch, &[]);
    c.initialize();

    let (said, error) = c.call("list_dir", json!({}));
    assert!(!error, "{said:?}");
    let rows: Value = serde_json::from_str(&said[0]).unwrap();
    let paths: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    // Folders first, and nothing a symlink brings in from outside.
    assert_eq!(paths, ["Templates", "a.md", "b.md", "c.md"], "{rows}");
    assert_eq!(rows[1]["kind"], "note");
    assert_eq!(rows[1]["title"], "Title");
    assert!(
        rows[1]["modified"].as_str().unwrap().ends_with('Z'),
        "{rows}"
    );
    let (said, error) = c.call("list_dir", json!({"path": "link"}));
    assert!(error, "a folder outside the vault was listed: {said:?}");

    let (said, _) = c.call("recent_changes", json!({"limit": 2}));
    let rows: Value = serde_json::from_str(&said[0]).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2, "{rows}");

    let (said, _) = c.call("missing_notes", json!({}));
    assert_eq!(
        said[0],
        r#"[{"linked_from":[{"line":1,"path":"c.md"},{"line":2,"path":"c.md"}],"path":"Later.md"}]"#
    );
    c.close();
}

#[test]
fn read_only_leaves_the_writing_tools_out() {
    let scratch = Scratch::new("read-only");
    let mut c = Client::start(&scratch, &["--read-only"]);
    c.initialize();
    assert_eq!(names(&c.ask("tools/list", json!({}))), READ_TOOLS);
    c.close();
}
