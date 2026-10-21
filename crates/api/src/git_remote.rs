//! Git for a held SSH shell, without opening a vault or index on its host.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};

use accent_core::{fs, git};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{Error, Etag, Event, Repo, Result, link, rpc, ssh};

/// One held shell's SSH host. Calls are synchronous and belong on workers.
#[derive(Clone)]
pub struct RemoteGit {
    url: ssh::Url,
    link: ssh::Url,
    ctl: PathBuf,
    server: String,
    ready: Arc<AtomicBool>,
    #[cfg(test)]
    command_for_test: Option<Vec<String>>,
}

impl RemoteGit {
    pub fn new(url: ssh::Url) -> Result<Self> {
        let (link, ctl) = Self::link_of(&url);
        Ok(Self {
            link,
            ctl,
            server: ssh::server_path(&link::server()?.hash),
            url,
            ready: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            command_for_test: None,
        })
    }

    fn link_of(url: &ssh::Url) -> (ssh::Url, PathBuf) {
        let link = link::host(url);
        let ctl = ssh::control_path(&link);
        (link, ctl)
    }

    pub fn url(&self) -> &ssh::Url {
        &self.url
    }

    /// Resolve the live, absolute cwd reported by the holder on its host.
    pub fn repo_at(&self, cwd: &Path) -> Result<Option<Repo>> {
        self.call("repo_at", json!([cwd]), rpc::DEADLINE)
    }

    fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        deadline: std::time::Duration,
    ) -> Result<T> {
        if !self.ready.load(Ordering::Acquire) {
            // A background Git refresh must not raise an SSH passphrase dialog. The held shell
            // already established the master; reconnecting that shell is the explicit prompt.
            link::prepare(&self.link, &self.ctl, true, &|_, _| ())?;
            self.ready.store(true, Ordering::Release);
        }
        // Only a fixed command enters the remote shell; paths travel as JSON on stdin.
        let argv = ssh::run(&self.link, &self.ctl, &format!("{} git-rpc", self.server));
        #[cfg(test)]
        let argv = self.command_for_test.clone().unwrap_or(argv);
        let mut child = link::command(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(link::no_ssh)?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Remote("ssh has no stderr".into()))?;
        let stderr = std::thread::spawn(move || {
            let mut stderr = stderr;
            let mut bytes = Vec::new();
            let mut chunk = [0; 8192];
            while let Ok(n) = stderr.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let keep = n.min((64 * 1024usize).saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..keep]);
            }
            String::from_utf8_lossy(&bytes).trim().to_string()
        });
        let input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Remote("ssh has no stdin".into()))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| Error::Remote("ssh has no stdout".into()))?;
        let (events, _unread) = channel::<Event>();
        let client = Arc::new(rpc::Client::new(
            Box::new(input),
            Box::new(output),
            events,
            Box::new(|| {}),
        ));
        // Client::call_tracked writes synchronously before its own receive timeout starts.
        // Bound the write too, then kill SSH to release a blocked pipe writer.
        let (sent, received) = channel();
        let worker = {
            let client = client.clone();
            let method = method.to_string();
            std::thread::spawn(move || {
                let answer = client.call_tracked::<Value>(
                    &method,
                    params,
                    &std::sync::Mutex::new(None),
                    deadline,
                );
                let _ = sent.send(answer);
            })
        };
        let answer = match received.recv_timeout(deadline) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => Err(Error::Remote(format!("{method} timed out"))),
            Err(RecvTimeoutError::Disconnected) => {
                Err(Error::Lost("the request worker closed".into()))
            }
        };
        // A timeout must not leave a writer or reader waiting on the SSH child.
        let _ = child.kill();
        let status = child.wait();
        client.close();
        client.join();
        let _ = worker.join();
        let said = stderr.join().unwrap_or_default();
        if matches!(&answer, Err(Error::Offline(_) | Error::Lost(_)))
            && !status.is_ok_and(|status| status.success())
        {
            self.ready.store(false, Ordering::Release);
            if !said.is_empty() {
                return Err(Error::Remote(said));
            }
        }
        answer.and_then(|value| {
            serde_json::from_value(value).map_err(|e| {
                Error::Protocol(format!("{method} answered something unreadable: {e}"))
            })
        })
    }

    pub fn read(&self, repo: &Repo, rel: &str) -> Result<(String, Etag)> {
        self.call("read", json!([repo, rel]), rpc::DEADLINE)
    }
    pub fn read_text(&self, repo: &Repo, rel: &str) -> Result<fs::Read> {
        self.call("read_text", json!([repo, rel]), rpc::DEADLINE)
    }
    pub fn stat(&self, repo: &Repo, rel: &str) -> Result<Option<Etag>> {
        self.call("stat", json!([repo, rel]), rpc::DEADLINE)
    }

    /// Permanently remove only paths Git still reports as untracked on the host.
    pub fn remove_untracked(&self, repo: &Repo, paths: &[String]) -> Result<()> {
        self.call("remove_untracked", json!([repo, paths]), rpc::DEADLINE)
    }
}

macro_rules! methods {
    (@fty ref $t:ty) => { &$t };
    (@fty val $t:ty) => { $t };
    (@wty ref $t:ty) => { <$t as ToOwned>::Owned };
    (@wty val $t:ty) => { $t };
    (@pass ref $n:ident) => { &$n };
    (@pass val $n:ident) => { $n };
    (@deadline) => { rpc::DEADLINE };
    (@deadline $bound:expr) => { rpc::DEADLINE + $bound };
    ($(
        $name:ident = $core:ident ($($arg:ident : $kind:tt $t:ty),* $(,)?) -> $ret:ty
            $(, bounded by $bound:expr)?;
    )*) => {
        impl RemoteGit {
            $(
                pub fn $name(&self $(, $arg: methods!(@fty $kind $t))*) -> Result<$ret> {
                    self.call(stringify!($name), json!([$($arg),*]), methods!(@deadline $($bound)?))
                }
            )*
        }
        fn dispatch_git(method: &str, p: &Value) -> Option<Result<Value>> {
            $(
                if method == stringify!($name) {
                    let _i = 0usize;
                    $(
                        let $arg: methods!(@wty $kind $t) = match rpc::arg(p, _i) {
                            Ok(value) => value,
                            Err(e) => return Some(Err(e)),
                        };
                        let _i = _i + 1;
                    )*
                    let _ = _i;
                    let repo: Repo = match rpc::arg(p, 0) {
                        Ok(repo) => repo,
                        Err(e) => return Some(Err(e)),
                    };
                    if let Err(e) = validate_repo(&repo).and_then(|()| validate_git_paths(method, p)) {
                        return Some(Err(e));
                    }
                    return Some(rpc::answer(git::$core($(methods!(@pass $kind $arg)),*)));
                }
            )*
            None
        }
    };
}

methods! {
    git_status = status(repo: ref Repo) -> crate::Status;
    git_untracked = untracked(repo: ref Repo, dir: ref str) -> Vec<String>;
    git_log = log(repo: ref Repo, skip: val usize, limit: val usize) -> Vec<git::Commit>;
    git_show = show(repo: ref Repo, rev: ref str, path: ref str) -> Option<git::Blob>;
    git_changed_files = changed_files(repo: ref Repo, oid: ref str) -> Vec<git::ChangedFile>;
    git_submodules = submodules(repo: ref Repo) -> Vec<git::Submodule>;
    git_branches = branches(repo: ref Repo) -> git::Branches;
    git_checkout = checkout(repo: ref Repo, branch: ref str) -> (), bounded by git::TRANSFER_TIMEOUT;
    git_track = track(repo: ref Repo, remote: ref str) -> (), bounded by git::TRANSFER_TIMEOUT;
    git_checkout_commit = checkout_commit(repo: ref Repo, oid: ref str) -> (), bounded by git::TRANSFER_TIMEOUT;
    git_create_branch = create_branch(repo: ref Repo, name: ref str, checkout: val bool) -> (), bounded by git::TRANSFER_TIMEOUT;
    git_delete_branch = delete_branch(repo: ref Repo, name: ref str, force: val bool) -> ();
    git_merge = merge(repo: ref Repo, branch: ref str) -> git::Merge, bounded by git::TRANSFER_TIMEOUT;
    git_merge_abort = merge_abort(repo: ref Repo) -> ();
    git_rebase_continue = rebase_continue(repo: ref Repo) -> git::Rebase, bounded by git::TRANSFER_TIMEOUT;
    git_rebase_abort = rebase_abort(repo: ref Repo) -> ();
    git_stage = stage(repo: ref Repo, paths: ref [String]) -> ();
    git_unstage = unstage(repo: ref Repo, paths: ref [String]) -> ();
    git_discard = discard(repo: ref Repo, paths: ref [String]) -> ();
    git_stage_text = stage_text(repo: ref Repo, path: ref str, text: ref str) -> (), bounded by git::TRANSFER_TIMEOUT;
    git_commit = commit(repo: ref Repo, message: ref str, all: val bool) -> String, bounded by git::TRANSFER_TIMEOUT;
    git_pull = pull(repo: ref Repo) -> String, bounded by git::TRANSFER_TIMEOUT;
    git_push = push(repo: ref Repo) -> String, bounded by git::TRANSFER_TIMEOUT;
    git_fetch = fetch(repo: ref Repo) -> String, bounded by git::FETCH_TIMEOUT;
    git_incoming = incoming(repo: ref Repo) -> Vec<String>;
}

fn validate_repo(repo: &Repo) -> Result<()> {
    if !repo.root.is_absolute() || !repo.git_dir.is_absolute() {
        return Err(
            accent_core::Error::Invalid("Git repository paths must be absolute".into()).into(),
        );
    }
    let actual = git::toplevel(&repo.root)?
        .ok_or_else(|| accent_core::Error::NotFound(repo.root.display().to_string()))?;
    if actual.root != repo.root || actual.git_dir != repo.git_dir {
        return Err(
            accent_core::Error::Invalid("Git repository changed on the host".into()).into(),
        );
    }
    Ok(())
}

fn relative(path: &str) -> Result<()> {
    if !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    {
        Ok(())
    } else {
        Err(accent_core::Error::Invalid(format!("path leaves the repository: {path}")).into())
    }
}

fn validate_git_paths(method: &str, p: &Value) -> Result<()> {
    match method {
        "git_untracked" | "git_stage_text" => relative(&rpc::arg::<String>(p, 1)?),
        "git_show" => relative(&rpc::arg::<String>(p, 2)?),
        "git_stage" | "git_unstage" | "git_discard" => {
            for path in rpc::arg::<Vec<String>>(p, 1)? {
                relative(&path)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn file(repo: &Repo, rel: &str) -> Result<Option<PathBuf>> {
    relative(rel)?;
    let root = repo
        .root
        .canonicalize()
        .map_err(|e| accent_core::Error::io(repo.root.display(), e))?;
    let path = root.join(rel);
    let real = match path.canonicalize() {
        Ok(real) => Some(real),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A missing path is only "absent" after its nearest existing ancestor is known to
            // be inside the repo. A dangling symlink in a parent may otherwise escape later.
            let ancestor = path
                .ancestors()
                .skip(1)
                .find_map(|part| part.canonicalize().ok())
                .ok_or_else(|| accent_core::Error::NotFound(rel.into()))?;
            if !ancestor.starts_with(&root) {
                return Err(accent_core::Error::Invalid(format!(
                    "path leaves the repository: {rel}"
                ))
                .into());
            }
            None
        }
        Err(error) => return Err(accent_core::Error::io(path.display(), error).into()),
    };
    if real.as_ref().is_some_and(|real| !real.starts_with(&root)) {
        return Err(
            accent_core::Error::Invalid(format!("path leaves the repository: {rel}")).into(),
        );
    }
    Ok(real)
}

/// Answer one JSON-RPC call from stdin, then let the process exit.
pub fn serve_once(mut input: impl std::io::BufRead, mut output: impl std::io::Write) -> Result<()> {
    let request: Value = loop {
        let mut line = String::new();
        if input
            .read_line(&mut line)
            .map_err(|e| Error::Remote(e.to_string()))?
            == 0
        {
            return Err(Error::Lost("the request stream closed".into()));
        }
        let request: Value =
            serde_json::from_str(&line).map_err(|e| Error::Protocol(e.to_string()))?;
        if request.get("id").is_some() {
            break request;
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let reply = match dispatch(method, &params) {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": rpc::to_wire(&error)}),
    };
    serde_json::to_writer(&mut output, &reply).map_err(|e| Error::Protocol(e.to_string()))?;
    writeln!(output).map_err(|e| Error::Remote(e.to_string()))?;
    Ok(())
}

fn dispatch(method: &str, p: &Value) -> Result<Value> {
    if let Some(answer) = dispatch_git(method, p) {
        return answer;
    }
    match method {
        "repo_at" => {
            let cwd: PathBuf = rpc::arg(p, 0)?;
            if !cwd.is_absolute() {
                return Err(
                    accent_core::Error::Invalid("shell cwd must be absolute".into()).into(),
                );
            }
            rpc::answer(git::toplevel(&cwd))
        }
        "remove_untracked" => Err(accent_core::Error::Invalid(
            "Remote untracked deletion is unavailable; remove files in the shell".into(),
        )
        .into()),
        "read" | "read_text" | "stat" => {
            let repo: Repo = rpc::arg(p, 0)?;
            let rel: String = rpc::arg(p, 1)?;
            validate_repo(&repo)?;
            let path = file(&repo, &rel)?;
            match method {
                "read" => rpc::answer(fs::read_note(
                    path.as_deref()
                        .ok_or_else(|| accent_core::Error::NotFound(rel.clone()))?,
                )),
                "read_text" => rpc::answer(fs::read_text(
                    path.as_deref()
                        .ok_or_else(|| accent_core::Error::NotFound(rel.clone()))?,
                )),
                _ => match path.as_deref().map(Etag::of) {
                    None => rpc::ok(None::<Etag>),
                    Some(Ok(etag)) => rpc::ok(Some(etag)),
                    Some(Err(error)) => Err(error.into()),
                },
            }
        }
        _ => Err(Error::UnknownMethod(method.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn remote_git_uses_the_held_shells_host_master() {
        let at = ssh::parse("ssh://user@example.org/projects/repo").unwrap();
        let (link, ctl) = RemoteGit::link_of(&at);
        assert_eq!(link, link::host(&at));
        assert_eq!(ctl, ssh::control_path(&link));
        assert_ne!(ctl, ssh::control_path(&at));
    }

    #[test]
    fn host_discovers_ancestor_repo_and_rejects_outside_paths() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join("nested")).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(repo.join("a.md"), "hello\n").unwrap();

        let found: Option<Repo> =
            serde_json::from_value(dispatch("repo_at", &json!([repo.join("nested")])).unwrap())
                .unwrap();
        let found = found.unwrap();
        assert_eq!(found.root, repo);
        let status: crate::Status =
            serde_json::from_value(dispatch("git_status", &json!([found.clone()])).unwrap())
                .unwrap();
        assert!(status.entries.iter().any(|entry| entry.path == "a.md"));

        let read: fs::Read =
            serde_json::from_value(dispatch("read_text", &json!([found.clone(), "a.md"])).unwrap())
                .unwrap();
        assert!(matches!(read, fs::Read::Text(_)));
        assert!(dispatch("read_text", &json!([found.clone(), "../outside.md"])).is_err());
        let mut forged = found;
        forged.git_dir = root.path().join("wrong");
        assert!(dispatch("git_status", &json!([forged])).is_err());
    }

    #[test]
    fn one_request_round_trip_stages_a_file_and_preserves_typed_errors() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.path().join("a.md"), "hello\n").unwrap();
        let repo: Repo =
            serde_json::from_value(dispatch("repo_at", &json!([root.path()])).unwrap()).unwrap();
        let ask = |method: &str, params: Value| {
            let mut out = Vec::new();
            let request = json!({"jsonrpc":"2.0","id":7,"method":method,"params":params});
            serve_once(std::io::Cursor::new(format!("{request}\n")), &mut out).unwrap();
            let answer: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(answer["id"], 7);
            answer
        };
        let staged = ask("git_stage", json!([repo.clone(), ["a.md"]]));
        assert_eq!(staged["result"], Value::Null);
        let status: crate::Status =
            serde_json::from_value(ask("git_status", json!([repo.clone()]))["result"].clone())
                .unwrap();
        assert!(status.staged().any(|entry| entry.path == "a.md"));
        let refused = ask("git_stage", json!([repo, ["../outside.md"]]));
        let error: Error = serde_json::from_value(refused["error"]["data"].clone()).unwrap();
        assert!(matches!(error, Error::Core(accent_core::Error::Invalid(_))));
    }

    #[test]
    fn one_shot_server_ignores_heartbeat_before_a_request() {
        let root = tempfile::tempdir().unwrap();
        let ping = json!({"jsonrpc":"2.0","method":"ping"});
        let ask = json!({"jsonrpc":"2.0","id":7,"method":"repo_at","params":[root.path()]});
        let mut out = Vec::new();
        serve_once(std::io::Cursor::new(format!("{ping}\n{ask}\n")), &mut out).unwrap();
        let reply: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reply["id"], 7);
        assert!(reply.get("result").is_some());
    }

    #[test]
    fn host_refuses_a_dangling_symlink_to_outside_before_read_or_stat() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let repo = git::toplevel(root.path()).unwrap().unwrap();
        assert!(dispatch("read_text", &json!([repo.clone(), "link/later.md"])).is_err());
        assert!(dispatch("stat", &json!([repo.clone(), "link/later.md"])).is_err());
        assert_eq!(
            dispatch("stat", &json!([repo.clone(), "missing.md"])).unwrap(),
            Value::Null
        );
        std::fs::write(outside.path().join("later.md"), "outside").unwrap();
        assert!(dispatch("read_text", &json!([repo.clone(), "link/later.md"])).is_err());
        assert!(dispatch("stat", &json!([repo, "link/later.md"])).is_err());
    }

    #[test]
    fn client_heartbeat_and_request_share_a_one_shot_server() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let root = tempfile::tempdir().unwrap();
        let (client_read, server_write) = UnixStream::pair().unwrap();
        let (server_read, mut client_write) = UnixStream::pair().unwrap();
        // Force the notification to arrive first, regardless of the heartbeat thread's order.
        client_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
            .unwrap();
        let server = std::thread::spawn(move || {
            serve_once(std::io::BufReader::new(server_read), server_write)
        });
        let (events, _unread) = channel::<Event>();
        let client = rpc::Client::new(
            Box::new(client_write),
            Box::new(client_read),
            events,
            Box::new(|| {}),
        );
        let answer: Option<Repo> = client.call("repo_at", json!([root.path()])).unwrap();
        assert!(answer.is_none());
        client.shutdown();
        server.join().unwrap().unwrap();
    }

    #[test]
    fn remote_call_deadline_includes_a_blocked_request_write() {
        let link = ssh::parse("ssh://example.org/").unwrap();
        let remote = RemoteGit {
            url: link.clone(),
            link,
            ctl: PathBuf::new(),
            server: String::new(),
            ready: Arc::new(AtomicBool::new(true)),
            command_for_test: Some(vec![
                "python3".into(),
                "-c".into(),
                "import time; time.sleep(2)".into(),
            ]),
        };
        let began = std::time::Instant::now();
        let result: Result<Value> = remote.call(
            "git_stage_text",
            json!(["x".repeat(8 * 1024 * 1024)]),
            std::time::Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(
            began.elapsed() < std::time::Duration::from_secs(1),
            "blocked for {:?}",
            began.elapsed()
        );
    }

    #[test]
    fn remote_untracked_removal_is_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.path().join("loose.md"), "keep").unwrap();
        std::fs::write(outside.path().join("outside.md"), "keep").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("outside.md"),
            root.path().join("link.md"),
        )
        .unwrap();
        let repo = git::toplevel(root.path()).unwrap().unwrap();
        let result = dispatch("remove_untracked", &json!([repo, ["loose.md", "link.md"]]));
        assert!(result.is_err());
        assert!(root.path().join("loose.md").exists());
        assert!(root.path().join("link.md").exists());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("outside.md")).unwrap(),
            "keep"
        );
    }
}
