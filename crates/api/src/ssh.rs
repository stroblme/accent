//! The vocabulary for driving the system `ssh` binary against a remote vault: addresses, quoting,
//! socket paths and argument vectors.
//!
//! One background ControlMaster per address carries everything that follows — the RPC session, a
//! terminal tab, a port forward — so the user authenticates once and their own `~/.ssh/config`,
//! ProxyJump and agent apply, because it is their `ssh` doing the work rather than a reimplementation
//! of it. The trap is that ssh succeeds *without* the master: pointed at a dead socket it quietly
//! opens a direct connection and then blocks on a passphrase prompt that has no terminal to appear
//! on, which is why the one-shot commands force `BatchMode=yes`.
//!
//! Nothing here spawns a process. It builds strings, so all of it is testable without a network.

use std::path::{Component, Path, PathBuf};

// ----------------------------------------------------------------- addresses

/// A remote vault: `ssh://[user@]host[:port]/absolute/path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// The login name, when the address named one. Otherwise ssh picks it from the user's config.
    pub user: Option<String>,
    /// Bare host, never bracketed: an IPv6 literal is stored as `::1`, not `[::1]`.
    pub host: String,
    /// Only when the address named one; ssh's own default (or the user's config) applies otherwise.
    pub port: Option<u16>,
    /// The vault root on the remote, absolute and without a trailing slash.
    pub path: PathBuf,
}

/// Parse `ssh://[user@]host[:port]/absolute/path`.
///
/// The error is a sentence fragment shown to the user, so it names what is wrong with the address
/// they typed rather than what the parser wanted to see.
pub fn parse(url: &str) -> Result<Url, String> {
    let rest = url
        .get(..6)
        .filter(|scheme| scheme.eq_ignore_ascii_case("ssh://"))
        .map(|scheme| &url[scheme.len()..])
        .ok_or("not an ssh:// address")?;

    // The authority never contains a slash, not even for a bracketed IPv6 literal, so the first
    // one starts the path and stays part of it.
    let slash = rest.find('/').ok_or("the path must be absolute")?;
    let (authority, path) = rest.split_at(slash);

    // ssh splits the login off at the last `@`, which is what lets a host name contain one.
    let (user, hostport) = match authority.rsplit_once('@') {
        Some((user, host)) => (Some(user.to_string()).filter(|u| !u.is_empty()), host),
        None => (None, authority),
    };

    let (host, port) = match hostport.strip_prefix('[') {
        // An IPv6 literal carries colons of its own, so it arrives bracketed and its port, if any,
        // sits after the closing bracket.
        Some(bracketed) => {
            let (host, tail) = bracketed
                .split_once(']')
                .ok_or("unclosed [ around the host")?;
            let port = match tail {
                "" => None,
                tail => Some(
                    tail.strip_prefix(':')
                        .ok_or("something other than a port after the [host]")?,
                ),
            };
            (host, port)
        }
        None => {
            let (host, port) = match hostport.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (hostport, None),
            };
            // Only a bracketed host may hold colons, so what is left of one here is a bare IPv6
            // literal whose last group we would otherwise take for a port.
            if host.contains(':') {
                return Err("an IPv6 address needs brackets, as in ssh://[::1]/srv/vault".into());
            }
            (host, port)
        }
    };
    if host.is_empty() {
        return Err("no host in the address".into());
    }

    let port = port
        .map(|p| {
            p.parse::<u16>()
                .map_err(|_| format!("{p:?} is not a port number"))
        })
        .transpose()?;

    // A trailing slash would make `/srv/vault/` and `/srv/vault` two different vaults with two
    // caches and two masters. The root is the one path that keeps its slash.
    let path = path.trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };

    Ok(Url {
        user,
        host: host.to_string(),
        port,
        path: PathBuf::from(path),
    })
}

/// Whether a vault address names a remote rather than a directory on this machine.
pub fn is_remote(s: &str) -> bool {
    s.get(..6)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("ssh://"))
}

/// Same question for a vault key, which the app stores as a [`PathBuf`] whether it is local or not.
pub fn is_remote_path(p: &Path) -> bool {
    is_remote(&p.to_string_lossy())
}

impl Url {
    /// What goes on the ssh command line as the destination. Never the port: that is `-p`, because
    /// `host:port` would be read as a path by scp-style parsing.
    pub fn destination(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// What a window title or subtitle shows: enough to tell two vaults apart, without the scheme
    /// and without the login name.
    pub fn label(&self) -> String {
        format!("{}:{}", self.host, self.path.display())
    }
}

impl std::fmt::Display for Url {
    /// The canonical form, so that `parse(&url.to_string())` gives the same [`Url`] back and the
    /// string is safe to key a socket and a cache directory by.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ssh://")?;
        if let Some(user) = &self.user {
            write!(f, "{user}@")?;
        }
        if self.host.contains(':') {
            write!(f, "[{}]", self.host)?;
        } else {
            write!(f, "{}", self.host)?;
        }
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        write!(f, "{}", self.path.display())
    }
}

/// Wrap one word for a remote login shell. Everything inside single quotes is literal to POSIX sh
/// except the closing quote itself, so each `'` leaves the quotes, is backslash-escaped, and the
/// quoting resumes.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ------------------------------------------------------------ control socket

/// Where the ControlMaster for `url` keeps its socket: `<runtime dir>/accent/<16 hex of the url>`.
///
/// A `sockaddr_un` holds 108 bytes on Linux, path and terminator together, so the name is a short
/// hash rather than anything readable — `%C` and friends produce paths that overflow on a long
/// host or a long user name and fail with "too long for Unix domain socket".
pub fn control_path(url: &Url) -> PathBuf {
    runtime_dir().join("accent").join(id(url))
}

/// `$XDG_RUNTIME_DIR` when set and absolute, else the temp dir.
///
/// Deliberately not `config::xdg`, whose fallback is `$HOME/<something>`: a home directory may be
/// on NFS, where a socket cannot be bound, and it is not wiped at logout, so a stale socket would
/// outlive the session that made it. The temp dir is the right second choice for a socket.
fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
}

/// The short name a remote is known by on this machine, shared by the socket and the cache so the
/// two can be matched up by eye.
fn id(url: &Url) -> String {
    hash_of(url.to_string().as_bytes())
}

// -------------------------------------------------------------- command line

/// Options every invocation shares: the socket to speak through, and the port when the address
/// named one.
fn base(url: &Url, ctl: &Path) -> Vec<String> {
    let mut argv = vec![
        "ssh".to_string(),
        "-o".to_string(),
        format!("ControlPath={}", ctl.display()),
    ];
    if let Some(port) = url.port {
        argv.push("-p".to_string());
        argv.push(port.to_string());
    }
    argv
}

/// Start the background master, or adopt the one already running, and exit.
///
/// `ControlMaster=auto` rather than `yes` because only `auto` unlinks a stale socket and reuses a
/// live master; `yes` logs "ControlSocket ... already exists, disabling multiplexing", exits 0, and
/// leaves no master behind — a success that breaks everything after it.
///
/// `ControlPersist=60` rather than `yes` because a crashed app must not leak a master forever. The
/// timer only counts idle time, and `serve` holds a session for as long as the vault is open, so a
/// working remote never reaches it.
pub fn master(url: &Url, ctl: &Path) -> Vec<String> {
    let mut argv = base(url, ctl);
    argv.extend(
        [
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=60",
            // A dropped link must surface as an error in 15 seconds rather than a hung read.
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=3",
        ]
        .map(String::from),
    );
    argv.push(url.destination());
    argv.push("true".to_string());
    argv
}

/// One command on the host for a dialog that has not connected yet — the folder completion in the
/// Open Remote form, which dials out so a path can be typed against the real machine.
///
/// `BatchMode=yes` is the whole point: the user has not pressed Connect, so a passphrase or a
/// host-key question is the one thing this must never produce. Agent or key auth, or nothing at
/// all. `ControlMaster=auto` on a socket of the caller's own — never [`control_path`], which an
/// open vault may be using — lets the first call make the master every later one reuses, and it
/// can be shut down with [`exit`] when the dialog closes. `ConnectTimeout` is the caller's and is
/// short: a field that goes quiet is worse than one that never completes.
pub fn probe(url: &Url, ctl: &Path, seconds: u32, command: &str) -> Vec<String> {
    let mut argv = base(url, ctl);
    argv.extend([
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ControlMaster=auto".to_string(),
        "-o".to_string(),
        "ControlPersist=30".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={seconds}"),
    ]);
    argv.push(url.destination());
    argv.push("--".to_string());
    argv.push(command.to_string());
    argv
}

/// The completion probe's own socket, so a dialog can never take down the master an open vault on
/// the same host is talking through.
pub fn probe_path(url: &Url) -> PathBuf {
    runtime_dir()
        .join("accent")
        .join(format!("{}-probe", id(url)))
}

/// Ask whether the master is alive. Exits non-zero when it is not.
pub fn check(url: &Url, ctl: &Path) -> Vec<String> {
    control(url, ctl, "check")
}

/// Tell the master to shut down, so closing a vault leaves no process behind.
pub fn exit(url: &Url, ctl: &Path) -> Vec<String> {
    control(url, ctl, "exit")
}

/// Add a forward to the running master: `localhost:<local>` on this machine reaches `<remote>` on
/// the far side.
pub fn forward(url: &Url, ctl: &Path, local: u16, remote: u16) -> Vec<String> {
    let mut argv = control(url, ctl, "forward");
    insert_forward(&mut argv, local, remote);
    argv
}

/// Take a forward back off the running master.
pub fn cancel(url: &Url, ctl: &Path, local: u16, remote: u16) -> Vec<String> {
    let mut argv = control(url, ctl, "cancel");
    insert_forward(&mut argv, local, remote);
    argv
}

/// One control command against the existing master. The destination is still required: ssh matches
/// it against the socket before it will act on it.
fn control(url: &Url, ctl: &Path, op: &str) -> Vec<String> {
    let mut argv = base(url, ctl);
    argv.push("-O".to_string());
    argv.push(op.to_string());
    argv.push(url.destination());
    argv
}

/// `-L` belongs with the options, ahead of the destination that [`control`] already appended.
fn insert_forward(argv: &mut Vec<String>, local: u16, remote: u16) {
    let at = argv.len() - 1;
    argv.insert(at, "-L".to_string());
    argv.insert(at + 1, format!("{local}:localhost:{remote}"));
}

/// Run one command on the remote over the existing master and collect its output.
///
/// `BatchMode=yes` is what makes a missing master an error instead of a hang: without it ssh falls
/// back to a fresh connection and asks for a passphrase or a host-key confirmation on a stdin that
/// belongs to the app, not to a terminal, and the call never returns.
pub fn run(url: &Url, ctl: &Path, command: &str) -> Vec<String> {
    let mut argv = base(url, ctl);
    argv.push("-o".to_string());
    argv.push("BatchMode=yes".to_string());
    argv.push(url.destination());
    // Everything after `--` is the remote command, even when it starts with a dash.
    argv.push("--".to_string());
    argv.push(command.to_string());
    argv
}

/// An interactive login shell in the vault root, for a terminal tab.
///
/// `-t` forces a pty, which a remote command does not get by default and without which the shell
/// runs non-interactive. No `BatchMode` here: a prompt is exactly what the tab is for.
pub fn shell(url: &Url, ctl: &Path) -> Vec<String> {
    let mut argv = base(url, ctl);
    argv.push("-t".to_string());
    argv.push(url.destination());
    argv.push(format!(
        "cd {} && exec \"$SHELL\"",
        quote(&url.path.to_string_lossy())
    ));
    argv
}

// ------------------------------------------------------- server provisioning

/// Where the uploaded `accent-cli` lives on the remote, relative to the login's home.
/// The one architecture Phase 5 provisions. A host of another shape needs its own build,
/// which is a build-matrix question rather than a code one.
pub const MUSL_TARGET: &str = "x86_64-unknown-linux-musl";

pub const SERVER_DIR: &str = ".local/share/accent/server";

/// The binary is named after its own contents, so a mismatched build is a missing file rather than
/// a protocol error at the first request.
pub fn server_name(hash: &str) -> String {
    format!("accent-cli-{hash}")
}

/// The remote path as a *shell expression*: `$HOME` is left unquoted on purpose so the remote shell
/// expands it — we cannot know the home directory before we have asked. `hash` is hex, so nothing
/// in the rest of the path needs quoting.
///
/// ponytail: a home directory containing a space would break this. Every remote that can run a
/// headless server has a POSIX home path, and the alternative is quoting gymnastics around a
/// variable that must stay expanded.
pub fn server_path(hash: &str) -> String {
    format!("$HOME/{SERVER_DIR}/{}", server_name(hash))
}

/// First 16 hex chars of the blake3 of `bytes`. Same shape as `config::vault_hash`, so remote and
/// local names read alike.
pub fn hash_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_string()
}

/// Test for the server, so a second connection skips the upload.
pub fn have_server_cmd(hash: &str) -> String {
    format!("test -x {}", server_path(hash))
}

/// Read the binary from stdin and install it.
///
/// The write goes to a dotted temp name first and is renamed into place: `cat >` straight onto the
/// final path would make a half-uploaded binary `test -x`-visible, and the next launch would exec
/// it. Older builds are swept afterwards rather than before, so a failed upload leaves the working
/// server where it was. The temp name starts with a dot, which is why the sweep's glob cannot
/// match it.
pub fn install_server_cmd(hash: &str) -> String {
    let dir = format!("$HOME/{SERVER_DIR}");
    let name = server_name(hash);
    let tmp = format!("{dir}/.{name}.tmp");
    let installed = server_path(hash);
    format!(
        "mkdir -p {dir} && cat > {tmp} && chmod 755 {tmp} && mv -f {tmp} {installed} && \
         for f in {dir}/accent-cli-*; do [ \"$f\" = {installed} ] || rm -f \"$f\"; done"
    )
}

/// The command that starts the headless server on a vault. `server` is a path expression from
/// [`server_path`]; the root is a literal, so it is quoted.
pub fn serve_cmd(server: &str, root: &Path) -> String {
    format!("{server} serve --vault {}", quote(&root.to_string_lossy()))
}

// --------------------------------------------------------------------- cache

/// The musl-static `accent-cli` this machine uploads to a host.
///
/// Looked for in three places, in the order that lets a developer run from `target/` and a user
/// run from an install without either having to configure anything: `$ACCENT_SERVER_BIN`, then
/// beside the running binary under `../lib/accent`, then the workspace's own musl output. It is a
/// separate build from the `accent-cli` on `$PATH`, which is linked against this machine's glibc
/// and would not start on an older host.
pub fn server_binary() -> Result<PathBuf, String> {
    if let Some(from_env) = std::env::var_os("ACCENT_SERVER_BIN") {
        let path = PathBuf::from(from_env);
        return match path.is_file() {
            true => Ok(path),
            false => Err(format!(
                "ACCENT_SERVER_BIN is not a file: {}",
                path.display()
            )),
        };
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find my own path: {e}"))?;
    // Up the ancestors rather than one fixed step: the installed layout puts us in `bin/` beside
    // `lib/accent`, a development build runs from `target/<profile>/`, and a test binary from
    // `target/debug/deps/`. Four levels reaches the cargo target directory from all three.
    for dir in exe.ancestors().skip(1).take(4) {
        for candidate in [
            dir.join("lib/accent/accent-cli"),
            dir.join("../lib/accent/accent-cli"),
            dir.join(format!("{MUSL_TARGET}/release/accent-cli")),
        ] {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err("no server binary to upload; run `make server` first".to_string())
}

/// Where files fetched from this remote are kept, under the same short name as the socket.
pub fn cache_dir(url: &Url) -> PathBuf {
    accent_core::config::xdg("XDG_CACHE_HOME", ".cache")
        .join("accent")
        .join("remote")
        .join(id(url))
}

/// Join a remote-relative path onto the cache, refusing anything that would land outside it.
///
/// `rel` comes back over the wire, so a `..` in it is the remote deciding where this machine
/// writes. The test is the lexical walk `Vault::resolve` does, for the same reason: it never
/// touches the filesystem, so it works on paths that do not exist yet.
pub fn cache_path(url: &Url, rel: &str) -> Option<PathBuf> {
    let base = cache_dir(url);
    let mut out = base.clone();
    for part in Path::new(rel).components() {
        match part {
            Component::Normal(name) => out.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                // `..` may walk back down to the cache root, never past it.
                out.pop();
                if !out.starts_with(&base) {
                    return None;
                }
            }
            // A root or a prefix component: that is not a relative path at all.
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    const CTL: &str = "/run/user/1000/accent/0123456789abcdef";

    fn ctl() -> &'static Path {
        Path::new(CTL)
    }

    fn plain() -> Url {
        parse("ssh://box/srv/vault").expect("a plain address parses")
    }

    fn ported() -> Url {
        parse("ssh://me@box:2222/srv/vault").expect("a user and a port parse")
    }

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_address_splits_into_user_host_port_and_path() {
        assert_eq!(
            ported(),
            Url {
                user: Some("me".into()),
                host: "box".into(),
                port: Some(2222),
                path: PathBuf::from("/srv/vault"),
            }
        );
        let bare = plain();
        assert_eq!((&bare.user, bare.port), (&None, None));
        // The port rides in `-p`, never in the destination, and the label drops the login.
        assert_eq!(ported().destination(), "me@box");
        assert_eq!(bare.destination(), "box");
        assert_eq!(ported().label(), "box:/srv/vault");
    }

    #[test]
    fn the_canonical_form_parses_back_to_the_same_address() {
        for text in [
            "ssh://box/srv/vault",
            "ssh://me@box/srv/vault",
            "ssh://me@box:2222/srv/vault",
            "ssh://[::1]:22/srv/vault",
            "ssh://box/",
        ] {
            let url = parse(text).expect("the fixtures parse");
            assert_eq!(url.to_string(), text);
            assert_eq!(parse(&url.to_string()), Ok(url));
        }
    }

    #[test]
    fn an_ipv6_literal_keeps_its_brackets_only_in_the_url() {
        let url = parse("ssh://me@[::1]:22/srv/vault").expect("a bracketed literal parses");
        assert_eq!(url.host, "::1");
        assert_eq!(url.port, Some(22));
        assert_eq!(url.destination(), "me@::1");
        assert_eq!(url.to_string(), "ssh://me@[::1]:22/srv/vault");
    }

    #[test]
    fn a_trailing_slash_goes_away_but_the_root_keeps_its_own() {
        assert_eq!(
            parse("ssh://box/srv/vault/").map(|u| u.path),
            Ok(PathBuf::from("/srv/vault"))
        );
        assert_eq!(parse("ssh://box/").map(|u| u.path), Ok(PathBuf::from("/")));
    }

    #[test]
    fn an_unusable_address_says_what_is_wrong_with_it() {
        assert_eq!(parse("/srv/vault"), Err("not an ssh:// address".into()));
        assert_eq!(parse("ssh://box"), Err("the path must be absolute".into()));
        assert_eq!(parse("ssh:///srv"), Err("no host in the address".into()));
        assert_eq!(
            parse("ssh://box:http/srv"),
            Err("\"http\" is not a port number".into())
        );
        assert!(parse("ssh://::1/srv").is_err());
        assert!(parse("ssh://[::1/srv").is_err());
        // The scheme is the only case-insensitive part.
        assert!(parse("SSH://box/srv/vault").is_ok());
    }

    #[test]
    fn a_vault_key_is_remote_when_it_carries_the_scheme() {
        assert!(is_remote("ssh://box/srv/vault"));
        assert!(is_remote("SSH://box/srv/vault"));
        assert!(!is_remote("/srv/vault"));
        assert!(!is_remote("ssh:/"));
        assert!(is_remote_path(Path::new("ssh://box/srv/vault")));
        assert!(!is_remote_path(Path::new("/home/me/vault")));
    }

    #[test]
    fn quoting_survives_a_space_a_quote_a_dollar_and_a_newline() {
        assert_eq!(quote("/srv/my vault"), "'/srv/my vault'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("$HOME"), "'$HOME'");
        assert_eq!(quote("a\nb"), "'a\nb'");
    }

    #[test]
    fn the_control_socket_stays_well_under_the_unix_path_limit() {
        let path = control_path(&ported());
        assert!(path.is_absolute());
        assert!(
            path.as_os_str().len() < 108,
            "sockaddr_un holds 108 bytes: {}",
            path.display()
        );
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert_eq!(name.len(), 16);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            path.parent().and_then(Path::file_name),
            Some(OsStr::new("accent"))
        );
        // Two addresses that differ only in their port are two masters.
        assert_ne!(control_path(&plain()), control_path(&ported()));
    }

    #[test]
    fn the_master_command_reuses_a_socket_and_notices_a_dead_link() {
        assert_eq!(
            master(&plain(), ctl()),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=60",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "box",
                "true",
            ])
        );
        assert_eq!(
            master(&ported(), ctl()),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=60",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "me@box",
                "true",
            ])
        );
    }

    #[test]
    fn check_and_exit_address_the_running_master() {
        assert_eq!(
            check(&plain(), ctl()),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-O",
                "check",
                "box",
            ])
        );
        assert_eq!(
            exit(&ported(), ctl()),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-O",
                "exit",
                "me@box",
            ])
        );
    }

    #[test]
    fn a_forward_and_its_cancellation_name_the_same_pair() {
        assert_eq!(
            forward(&plain(), ctl(), 8080, 80),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-O",
                "forward",
                "-L",
                "8080:localhost:80",
                "box",
            ])
        );
        assert_eq!(
            cancel(&ported(), ctl(), 8080, 80),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-O",
                "cancel",
                "-L",
                "8080:localhost:80",
                "me@box",
            ])
        );
    }

    #[test]
    fn a_one_shot_command_refuses_to_prompt() {
        assert_eq!(
            run(&plain(), ctl(), "uname -s"),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-o",
                "BatchMode=yes",
                "box",
                "--",
                "uname -s",
            ])
        );
        assert_eq!(
            run(&ported(), ctl(), "uname -s"),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-o",
                "BatchMode=yes",
                "me@box",
                "--",
                "uname -s",
            ])
        );
    }

    #[test]
    fn a_completion_probe_never_prompts_and_gives_up_quickly() {
        assert_eq!(
            probe(&plain(), ctl(), 5, "ls -1p"),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-o",
                "BatchMode=yes",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=30",
                "-o",
                "ConnectTimeout=5",
                "box",
                "--",
                "ls -1p",
            ])
        );
        // Its own socket: an open vault on the same host must not be shut down with the dialog.
        assert_ne!(probe_path(&plain()), control_path(&plain()));
    }

    #[test]
    fn an_interactive_shell_starts_in_the_vault_root() {
        assert_eq!(
            shell(&plain(), ctl()),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-t",
                "box",
                "cd '/srv/vault' && exec \"$SHELL\"",
            ])
        );
        let spaced = parse("ssh://box:2222/srv/my vault").expect("a spaced path parses");
        assert_eq!(
            shell(&spaced, ctl()),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-t",
                "box",
                "cd '/srv/my vault' && exec \"$SHELL\"",
            ])
        );
    }

    #[test]
    fn the_server_is_named_and_found_by_its_hash() {
        let hash = hash_of(b"a pretend binary");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hash, hash_of(b"a different binary"));

        let hash = "0123456789abcdef";
        assert_eq!(server_name(hash), "accent-cli-0123456789abcdef");
        assert_eq!(
            server_path(hash),
            "$HOME/.local/share/accent/server/accent-cli-0123456789abcdef"
        );
        assert_eq!(
            have_server_cmd(hash),
            "test -x $HOME/.local/share/accent/server/accent-cli-0123456789abcdef"
        );
    }

    #[test]
    fn installing_the_server_is_atomic_and_sweeps_stale_builds() {
        let hash = "0123456789abcdef";
        let cmd = install_server_cmd(hash);
        let installed = server_path(hash);
        let tmp = "$HOME/.local/share/accent/server/.accent-cli-0123456789abcdef.tmp";
        assert!(cmd.contains("mkdir -p $HOME/.local/share/accent/server"));
        // The binary is never written to the path it is finally seen at.
        assert!(cmd.contains(&format!("cat > {tmp}")));
        assert!(cmd.contains(&format!("chmod 755 {tmp}")));
        assert!(cmd.contains(&format!("mv -f {tmp} {installed}")));
        assert!(!cmd.contains(&format!("cat > {installed}")));
        // The sweep spares what was just installed.
        assert!(cmd.contains("accent-cli-*"));
        assert!(cmd.contains(&format!("[ \"$f\" = {installed} ] || rm -f \"$f\"")));
    }

    #[test]
    fn serve_quotes_the_vault_root() {
        assert_eq!(
            serve_cmd("$HOME/bin/accent-cli", Path::new("/srv/my vault")),
            "$HOME/bin/accent-cli serve --vault '/srv/my vault'"
        );
    }

    #[test]
    fn the_cache_lives_under_the_same_name_as_the_socket() {
        let url = ported();
        let dir = cache_dir(&url);
        assert!(dir.is_absolute());
        assert_eq!(dir.file_name(), control_path(&url).file_name());
        assert_eq!(
            dir.parent().and_then(Path::file_name),
            Some(OsStr::new("remote"))
        );
        assert_eq!(
            cache_path(&url, "notes/today.md"),
            Some(dir.join("notes/today.md"))
        );
        // A `.` is noise, and a `..` that stays inside is fine.
        assert_eq!(
            cache_path(&url, "./notes/../today.md"),
            Some(dir.join("today.md"))
        );
    }

    #[test]
    fn nothing_the_remote_says_can_escape_the_cache() {
        let url = ported();
        assert_eq!(cache_path(&url, "../../etc/passwd"), None);
        assert_eq!(cache_path(&url, "notes/../../../etc/passwd"), None);
        assert_eq!(cache_path(&url, "/etc/passwd"), None);
    }
}
