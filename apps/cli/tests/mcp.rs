//! `accent-cli mcp` as a client meets it: a real process, spoken to in JSON-RPC lines on its
//! stdin, in both of the protocol's lifecycles, and gone once its stdin is.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Create an isolated vault with an external symlink and private index, config, and cache directories.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("accent-mcp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in [
            "vault/Templates",
            "outside",
            "config",
            "cache",
            "state",
            "tmp",
            "home",
        ] {
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

/// The server process and its pipe ends.
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
            .env("XDG_STATE_HOME", scratch.dir.join("state"))
            .env("TMPDIR", scratch.dir.join("tmp"))
            // A held terminal runs `$SHELL` under `$HOME` from this environment: a plain `sh` in
            // a scratch home, never the developer's shell and its startup files.
            .env("SHELL", "/bin/sh")
            .env("HOME", scratch.dir.join("home"))
            .env_remove("ENV")
            // A test never draws on the screen it runs on: `render_diagram` finds no display.
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env("GDK_BACKEND", "x11")
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

    /// Send a request and read until its response arrives.
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

    /// Extract a tool response's text blocks and error flag.
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

    /// Close stdin and wait for the server to exit.
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

#[test]
fn jobs_survive_mcp_disconnect_and_offer_paged_output() {
    let scratch = Scratch::new("jobs");
    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (started, error) = client.call(
        "start_job",
        json!({"cwd": "", "argv": ["/bin/sh", "-c", "printf 'first\\n'; sleep 3; printf 'second\\n'"]}),
    );
    assert!(!error, "{started:?}");
    let id = serde_json::from_str::<Value>(&started[0]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    client.close();

    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (listed, error) = client.call("list_jobs", json!({}));
    assert!(!error, "{listed:?}");
    assert!(listed[0].contains(&id));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (read, error) = client.call("read_job", json!({"id": id, "offset": 0, "limit": 6}));
        assert!(!error, "{read:?}");
        let read: Value = serde_json::from_str(&read[0]).unwrap();
        if read["state"] == "exited" {
            assert_eq!(read["output"], "first\n");
            assert_eq!(read["next_offset"], 6);
            let (rest, error) =
                client.call("read_job", json!({"id": id, "offset": 6, "limit": 100}));
            assert!(!error, "{rest:?}");
            let rest: Value = serde_json::from_str(&rest[0]).unwrap();
            assert_eq!(rest["output"], "second\n");
            assert_eq!(rest["eof"], true);
            break;
        }
        assert!(Instant::now() < deadline, "job did not exit: {read}");
        std::thread::sleep(Duration::from_millis(50));
    }
    client.close();
}

#[test]
fn terminal_survives_mcp_disconnect_and_accepts_input_on_reconnect() {
    let scratch = Scratch::new("terminal");
    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (started, error) = client.call("start_terminal", json!({"cwd": ""}));
    assert!(!error, "{started:?}");
    let started: Value = serde_json::from_str(&started[0]).unwrap();
    let id = started["id"].as_str().unwrap().to_string();
    assert!(
        started["attach_command"]
            .as_str()
            .unwrap()
            .contains(" attach --existing --vault ")
    );
    assert!(
        started["attach_command"]
            .as_str()
            .unwrap()
            .ends_with(&format!(" '{id}'"))
    );
    assert_eq!(started["run_on"], "host");
    client.close();

    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (listed, error) = client.call("list_terminals", json!({}));
    assert!(!error, "{listed:?}");
    assert!(listed[0].contains(&id));
    let (sent, error) = client.call(
        "send_terminal",
        json!({"id": id, "input": "printf 'mcp-pty-ok\\n' > terminal-result; printf done\\n\n"}),
    );
    assert!(!error, "{sent:?}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (read, error) = client.call("read_terminal", json!({"id": id, "offset": 0}));
        assert!(!error, "{read:?}");
        let read: Value = serde_json::from_str(&read[0]).unwrap();
        if scratch.exists("vault/terminal-result")
            && read["output"].as_str().unwrap().contains("done")
        {
            assert!(read["next_offset"].as_u64().unwrap() > 0);
            break;
        }
        assert!(Instant::now() < deadline, "terminal gave no output: {read}");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (stopped, error) = client.call("stop_terminal", json!({"id": id}));
    assert!(!error, "{stopped:?}");
    client.close();
}

#[test]
fn stop_job_ends_the_process_group() {
    let scratch = Scratch::new("job-stop");
    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (started, error) = client.call(
        "start_job",
        json!({"cwd": "", "argv": ["/bin/sh", "-c", "(sleep 2; printf leaked > marker) & wait"]}),
    );
    assert!(!error, "{started:?}");
    let id = serde_json::from_str::<Value>(&started[0]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (stopped, error) = client.call("stop_job", json!({"id": id}));
    assert!(!error, "{stopped:?}");
    let stopped: Value = serde_json::from_str(&stopped[0]).unwrap();
    assert_eq!(stopped["state"], "stopped");
    std::thread::sleep(Duration::from_millis(2300));
    assert!(
        !scratch.exists("vault/marker"),
        "a child escaped the stopped group"
    );
    client.close();
}

#[test]
fn stop_job_kills_a_descendant_that_ignores_term() {
    let scratch = Scratch::new("job-stubborn-child");
    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (started, error) = client.call(
        "start_job",
        json!({"cwd": "", "argv": ["/bin/sh", "-c", "(trap '' TERM; printf ready > ready; sleep 2; printf leaked > marker) & wait"]}),
    );
    assert!(!error, "{started:?}");
    let id = serde_json::from_str::<Value>(&started[0]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ready_by = Instant::now() + Duration::from_secs(2);
    while !scratch.exists("vault/ready") {
        assert!(Instant::now() < ready_by, "the descendant did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (stopped, error) = client.call("stop_job", json!({"id": id}));
    assert!(!error, "{stopped:?}");
    let stopped: Value = serde_json::from_str(&stopped[0]).unwrap();
    assert_eq!(stopped["state"], "stopped");
    std::thread::sleep(Duration::from_millis(2300));
    assert!(
        !scratch.exists("vault/marker"),
        "a TERM-ignoring child survived"
    );
    client.close();
}

#[test]
fn stale_running_job_is_interrupted_and_old_results_are_pruned() {
    let scratch = Scratch::new("job-reboot");
    let root = scratch.dir.join("vault").canonicalize().unwrap();
    let name = accent_api::ssh::hash_of(root.to_string_lossy().as_bytes());
    let base = scratch.dir.join("state/accent/jobs").join(name);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&base)
        .unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (id, state, finished) in [
        ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "running", None),
        (
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "exited",
            Some(now - 8 * 24 * 60 * 60),
        ),
    ] {
        let dir = base.join(id);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        std::fs::write(dir.join("output.log"), "partial\n").unwrap();
        std::fs::write(
            dir.join("meta.json"),
            json!({"id": id, "cwd": "", "state": state, "started": now - 8 * 24 * 60 * 60,
                "finished": finished, "exit_code": null, "error": null,
                "socket": scratch.dir.join("missing.sock")})
            .to_string(),
        )
        .unwrap();
    }
    let incomplete = base.join("cccccccccccccccccccccccccccccccc");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&incomplete)
        .unwrap();
    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (listed, error) = client.call("list_jobs", json!({}));
    assert!(!error, "{listed:?}");
    let listed: Value = serde_json::from_str(&listed[0]).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["state"], "interrupted");
    assert!(!base.join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").exists());
    client.close();
}

#[test]
fn job_output_is_capped_and_reports_truncation() {
    let scratch = Scratch::new("job-cap");
    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (started, error) = client.call(
        "start_job",
        json!({"cwd": "", "argv": ["/bin/sh", "-c", "dd if=/dev/zero bs=1048576 count=17 2>/dev/null"]}),
    );
    assert!(!error, "{started:?}");
    let id = serde_json::from_str::<Value>(&started[0]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (read, error) = client.call("read_job", json!({"id": id, "offset": 0, "limit": 10}));
        assert!(!error, "{read:?}");
        let read: Value = serde_json::from_str(&read[0]).unwrap();
        if read["state"] == "exited" {
            assert_eq!(read["truncated"], true);
            assert_eq!(read["base_offset"], 8388608);
            assert_eq!(read["gap"], 8388608);
            assert_eq!(read["next_offset"], 8388618);
            assert_eq!(read["output"].as_str().unwrap().len(), 10);
            let (last, error) = client.call(
                "read_job",
                json!({"id": id, "offset": 17825782, "limit": 10}),
            );
            assert!(!error, "{last:?}");
            let last: Value = serde_json::from_str(&last[0]).unwrap();
            assert_eq!(last["next_offset"], 17825792);
            assert_eq!(last["gap"], 0);
            assert_eq!(last["eof"], true);
            break;
        }
        assert!(Instant::now() < deadline, "job did not exit: {read}");
        std::thread::sleep(Duration::from_millis(50));
    }
    client.close();
}

#[test]
fn job_read_keeps_multibyte_characters_whole_at_small_page_limits() {
    let scratch = Scratch::new("job-utf8");
    let mut client = Client::start(&scratch, &["--allow-jobs"]);
    client.initialize();
    let (started, error) = client.call(
        "start_job",
        json!({"cwd": "", "argv": ["/bin/sh", "-c", "printf '€Z'"]}),
    );
    assert!(!error, "{started:?}");
    let id = serde_json::from_str::<Value>(&started[0]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (first, error) = client.call("read_job", json!({"id": id, "offset": 0, "limit": 1}));
        assert!(!error, "{first:?}");
        let first: Value = serde_json::from_str(&first[0]).unwrap();
        if first["state"] == "exited" {
            assert_eq!(first["output"], "€");
            assert_eq!(first["next_offset"], 3);
            let (rest, error) = client.call("read_job", json!({"id": id, "offset": 3, "limit": 1}));
            assert!(!error, "{rest:?}");
            let rest: Value = serde_json::from_str(&rest[0]).unwrap();
            assert_eq!(rest["output"], "Z");
            assert_eq!(rest["next_offset"], 4);
            break;
        }
        assert!(Instant::now() < deadline, "job did not exit: {first}");
        std::thread::sleep(Duration::from_millis(20));
    }
    client.close();
}

#[test]
fn jobs_require_opt_in_and_refuse_outside_cwd() {
    let scratch = Scratch::new("job-gates");
    let mut plain = Client::start(&scratch, &[]);
    plain.initialize();
    assert!(!names(&plain.ask("tools/list", json!({}))).contains(&"start_job".into()));
    assert!(!names(&plain.ask("tools/list", json!({}))).contains(&"start_terminal".into()));
    plain.close();

    let mut limited = Client::start(&scratch, &["--allow-jobs", "--read-only"]);
    limited.initialize();
    let tools = names(&limited.ask("tools/list", json!({})));
    assert!(tools.contains(&"list_jobs".into()));
    assert!(tools.contains(&"read_job".into()));
    assert!(tools.contains(&"list_terminals".into()));
    assert!(tools.contains(&"read_terminal".into()));
    assert!(!tools.contains(&"start_job".into()));
    assert!(!tools.contains(&"stop_job".into()));
    assert!(!tools.contains(&"start_terminal".into()));
    assert!(!tools.contains(&"send_terminal".into()));
    assert!(!tools.contains(&"stop_terminal".into()));
    limited.close();

    let mut allowed = Client::start(&scratch, &["--allow-jobs"]);
    allowed.initialize();
    let (_, error) = allowed.call("start_job", json!({"cwd": "link", "argv": ["/bin/true"]}));
    assert!(error);
    let (_, error) = allowed.call("start_job", json!({"cwd": "", "argv": []}));
    assert!(error);
    allowed.close();
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

const READ_TOOLS: [&str; 11] = [
    "explore",
    "list_backlinks",
    "list_dir",
    "list_tags",
    "missing_notes",
    "pdf_annotations",
    "read_note",
    "recent_changes",
    "render_diagram",
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
    // The shorter symlinked note ranks first; the limit counts only what is shown.
    let (hits, _) = c.call("search_notes", json!({"query": "kumquat", "limit": 1}));
    assert!(hits[0].contains(r#""path":"a.md""#), "{hits:?}");

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

/// Writes reject stale etags and external symlink targets; overwrites require an etag.
#[test]
fn mcp_writes_only_over_what_it_read() {
    let scratch = Scratch::new("write");
    let mut c = Client::start(&scratch, &[]);
    c.initialize();
    assert_eq!(names(&c.ask("tools/list", json!({}))).len(), 14);

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

/// Explore ranks linked notes, returns whole cards for explicit paths, and respects its budget.
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

#[test]
fn explore_notebook_lines_match_searchable_cell_sources() {
    let scratch = Scratch::new("notebook-explore");
    std::fs::write(
        scratch.dir.join("vault/analysis.ipynb"),
        r#"{"nbformat":4,"cells":[{"cell_type":"markdown","source":["alpha\n"]},{"cell_type":"code","source":["needle line\n"],"outputs":[{"text":"hidden output"}]}],"metadata":{"name":"hidden metadata"}}"#,
    )
    .unwrap();
    let mut c = Client::start(&scratch, &[]);
    c.initialize();

    let (hits, error) = c.call("search_notes", json!({"query": "needle"}));
    assert!(!error, "{hits:?}");
    let hits: Value = serde_json::from_str(&hits[0]).unwrap();
    assert_eq!(hits[0]["path"], "analysis.ipynb");
    assert_eq!(hits[0]["line"], 3);

    for query in ["needle", "analysis.ipynb"] {
        let (said, error) = c.call("explore", json!({"query": query}));
        assert!(!error, "{said:?}");
        assert!(said[0].contains("3\tneedle line"), "{}", said[0]);
        assert!(!said[0].contains("hidden output"), "{}", said[0]);
        assert!(!said[0].contains("hidden metadata"), "{}", said[0]);
    }
    for args in [
        json!({"path": "analysis.ipynb"}),
        json!({"path": "analysis.ipynb", "view": "sources"}),
    ] {
        let (source, error) = c.call("read_note", args);
        assert!(!error, "{source:?}");
        let head: Value = serde_json::from_str(&source[0]).unwrap();
        assert_eq!(head["view"], "sources");
        assert!(head.get("etag").is_none(), "{head}");
        assert_eq!(source[1], "alpha\n\nneedle line\n");
        let (said, error) = c.call(
            "write_note",
            json!({"path": "analysis.ipynb", "content": source[1]}),
        );
        assert!(error && said[0].contains("pass the etag"), "{said:?}");
    }
    let (raw, error) = c.call(
        "read_note",
        json!({"path": "analysis.ipynb", "view": "raw"}),
    );
    assert!(!error, "{raw:?}");
    let head: Value = serde_json::from_str(&raw[0]).unwrap();
    assert!(head["etag"].is_string());
    assert!(raw[1].contains("hidden output"), "{}", raw[1]);
    let (_, error) = c.call(
        "write_note",
        json!({"path": "analysis.ipynb", "content": raw[1], "etag": head["etag"]}),
    );
    assert!(!error);
    c.close();
}

/// Explore shows named declarations, callers, callees, test reachability, paths, and file outlines.
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

/// Use `build-aux/fake-lsp.py` to test precise language-server callers and fallback
/// when a server is unavailable or still loading.
#[test]
fn explore_asks_the_language_server_when_precise() {
    let scratch = Scratch::new("precise");
    let write = |rel: &str, text: &str| std::fs::write(scratch.dir.join(rel), text).unwrap();
    write(
        "vault/lib.rs",
        "pub struct Index;\n\nimpl Index {\n    pub fn backlinks(&self) -> usize {\n        \
         helper()\n    }\n}\n\nfn helper() -> usize {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    \
         #[test]\n    fn counts() {\n        assert_eq!(super::Index.backlinks(), 1);\n    }\n}\n",
    );
    write(
        "vault/other.rs",
        "pub struct Other;\n\nimpl Other {\n    pub fn backlinks(&self) -> usize {\n        0\n    \
         }\n}\n\nfn caller(o: &Other) -> usize {\n    o.backlinks()\n}\n",
    );
    write(
        "vault/tool.py",
        "def tally():\n    return 0\n\ndef use():\n    return tally()\n",
    );
    // 0-based lines: `backlinks` is called by `counts` and calls `helper`.
    write(
        "calls.json",
        r#"{"lib.rs:3": {"incoming": [["lib.rs", 15, "counts", [16]]],
                         "outgoing": [["lib.rs", 8, "helper", [4]]]},
            "lib.rs:8": {"incoming": [["lib.rs", 3, "backlinks", [4]]]},
            "lib.rs:15": {"outgoing": [["lib.rs", 3, "backlinks", [16]]]}}"#,
    );
    let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/../../build-aux/fake-lsp.py");
    let calls = scratch.dir.join("calls.json");
    std::fs::create_dir_all(scratch.dir.join("config/accent")).unwrap();
    // Keyed by the canonical root, as `config::vault_key` keys it: macOS's temp dir is under the
    // `/var -> /private/var` link.
    write(
        "config/accent/config.toml",
        &format!(
            "[vaults.{:?}.lsp.servers]\nrust = [\"python3\", {fake:?}, \"--calls\", {:?}, \
             \"--loading\", \"2\"]\npython3 = [\"accent-no-such-server\"]\n",
            scratch.dir.join("vault").canonicalize().unwrap(),
            calls,
        ),
    );
    let mut c = Client::start(&scratch, &[]);
    c.initialize();

    let query = "Index::backlinks helper";
    let (said, _) = c.call("explore", json!({"query": query}));
    let by_name = "- `Index::backlinks` method (lib.rs:4) — 2 calls in `lib.rs`, `other.rs`; \
                   tested by `counts` (lib.rs); 2 declarations share this name";
    assert!(said[0].contains(by_name), "{}", said[0]);

    let (said, error) = c.call("explore", json!({"query": query, "precise": true}));
    assert!(!error, "{said:?}");
    for row in [
        "**Blast radius** (precise: by the language servers' call hierarchy)\n",
        "- `Index::backlinks` method (lib.rs:4) — 1 call in `lib.rs`; tested by `counts` \
         (lib.rs)\n  calls `helper`\n",
        "- `helper` fn (lib.rs:9) — 1 call in `lib.rs`; tested via `backlinks` (lib.rs)\n",
        "**Call paths**\n- `backlinks` → `helper`",
    ] {
        assert!(said[0].contains(row), "{row:?} not in {}", said[0]);
    }

    let (said, _) = c.call("explore", json!({"query": "tally", "precise": true}));
    for row in [
        "precise: accent-no-such-server is not installed, name-based callers shown\n",
        "- `tally` fn (tool.py:1) — 1 call in `tool.py`; no tests found within 3 caller hops; \
         by name",
    ] {
        assert!(said[0].contains(row), "{row:?} not in {}", said[0]);
    }
    c.close();
}

/// List folders, recently changed files, and unresolved links.
#[test]
fn mcp_lists_folders_recent_files_and_missing_notes() {
    let scratch = Scratch::new("lists");
    std::fs::write(
        scratch.dir.join("vault/c.md"),
        "[[Later]]\nand [[Later#Part]]\n",
    )
    .unwrap();
    // The symlinked note is the newest, which `recent_changes` must skip and still fill its limit.
    std::fs::File::options()
        .write(true)
        .open(scratch.dir.join("outside/secret.md"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
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

/// A page is drawn by the app beside `accent-cli`; with no display, or no app, the tool says that
/// rather than hanging, and a page or file it cannot be is refused before anything is started.
#[test]
fn render_diagram_says_why_it_cannot_draw() {
    let scratch = Scratch::new("render");
    let diagram = r#"<mxfile><diagram name="One" id="1"><mxGraphModel><root><mxCell id="0"/>
        </root></mxGraphModel></diagram><diagram id="2"><mxGraphModel><root><mxCell id="0"/>
        </root></mxGraphModel></diagram></mxfile>"#;
    std::fs::write(scratch.dir.join("vault/flow.drawio"), diagram).unwrap();
    let mut c = Client::start(&scratch, &["--read-only"]);
    c.initialize();
    let (said, error) = c.call("render_diagram", json!({"path": "a.md"}));
    assert!(
        error && said[0].contains("not a draw.io diagram"),
        "{said:?}"
    );
    let (said, error) = c.call(
        "render_diagram",
        json!({"path": "flow.drawio", "page": "Two"}),
    );
    assert!(error && said[0].contains(r#""One", "Page 2""#), "{said:?}");
    let (said, error) = c.call("render_diagram", json!({"path": "flow.drawio", "page": 2}));
    assert!(error && said[0].contains("no page 2"), "{said:?}");
    let started = Instant::now();
    let (said, error) = c.call(
        "render_diagram",
        json!({"path": "flow.drawio", "page": "Page 2"}),
    );
    assert!(error && said[0].contains("accent app"), "{said:?}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
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
