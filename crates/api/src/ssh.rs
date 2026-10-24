//! Build system-SSH addresses, shell quoting, socket paths, and arguments. One ControlMaster
//! shares authentication among RPC, shells, and forwards. One-shot commands use BatchMode to
//! avoid hidden prompts after a socket dies. These builders do not spawn processes.

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

/// Parse `ssh://[user@]host[:port]/absolute/path`. Errors are displayable
/// fragments describing what is wrong with the entered address.
pub fn parse(url: &str) -> crate::Result<Url> {
    parsed(url).map_err(|why| accent_core::Error::Invalid(why).into())
}

fn parsed(url: &str) -> Result<Url, String> {
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
    /// SSH destination without the port, which travels separately as `-p`;
    /// `host:port` would be interpreted as an scp-style path.
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

    /// `[user@]host[:port]`, an IPv6 literal bracketed: the address without its scheme and path,
    /// which is what the Open Remote form's host field holds. `ssh://{authority}{path}` parses
    /// back to this same [`Url`].
    pub fn authority(&self) -> String {
        let mut out = String::new();
        if let Some(user) = &self.user {
            out.push_str(user);
            out.push('@');
        }
        match self.host.contains(':') {
            true => out.push_str(&format!("[{}]", self.host)),
            false => out.push_str(&self.host),
        }
        if let Some(port) = self.port {
            out.push_str(&format!(":{port}"));
        }
        out
    }
}

impl std::fmt::Display for Url {
    /// The canonical form, so that `parse(&url.to_string())` gives the same [`Url`] back and the
    /// string is safe to key a socket and a cache directory by.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ssh://{}{}", self.authority(), self.path.display())
    }
}

/// Quote one word for POSIX remote shells. Single quotes preserve literal
/// text; escape each embedded quote outside the quoted region.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ------------------------------------------------------------ control socket

/// ControlMaster socket at `<runtime dir>/accent/<16 hex of the url>`.
/// A short hash fits Linux's 108-byte `sockaddr_un` path including terminator,
/// even when host or user names are long.
pub fn control_path(url: &Url) -> PathBuf {
    runtime_dir().join("accent").join(id(url))
}

/// Use absolute `$XDG_RUNTIME_DIR`, otherwise the temp directory. Avoid
/// `config::xdg`'s home fallback: home may use NFS (no sockets) and retain
/// stale sockets beyond logout.
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

/// Start or reuse a background master. `ControlMaster=auto` reuses live
/// masters and removes stale sockets; `yes` can succeed without creating one.
/// `ControlPersist=60` prevents permanent leaks after crashes and allows quick
/// reopens; an active `serve` session keeps the master non-idle.
/// Use `quiet` for automatic reconnects: BatchMode refuses passphrase prompts
/// without interrupting the user. Explicit attempts may prompt.
pub fn master(url: &Url, ctl: &Path, quiet: bool) -> Vec<String> {
    let mut argv = base(url, ctl);
    if quiet {
        argv.extend(["-o", "BatchMode=yes"].map(String::from));
    }
    argv.extend(
        [
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=60",
            // A host that does not answer fails the attempt in ten seconds, rather than after
            // the kernel's own connect timeout of a minute or two. It covers the handshake too,
            // but not a prompt, which waits for the person answering it.
            "-o",
            "ConnectTimeout=10",
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

/// Probe remote folders before Open Remote connects. BatchMode permits only
/// noninteractive authentication, avoiding premature passphrase or host-key
/// prompts. A caller-owned socket with `ControlMaster=auto` reuses probe
/// connections without touching a vault master; [`exit`] closes it with the
/// dialog. The caller supplies a short `ConnectTimeout` for responsive completion.
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

/// Separate probe socket, so closing a dialog cannot stop an open vault's master.
pub fn probe_path(url: &Url) -> PathBuf {
    runtime_dir()
        .join("accent")
        .join(format!("{}-probe", id(url)))
}

/// Tell the master to shut down, as the Open Remote dialog does with its probe's when it closes.
/// A vault's own master is not told: it lingers its ControlPersist minute for a quick reopen.
pub fn exit(url: &Url, ctl: &Path) -> Vec<String> {
    control(url, ctl, "exit")
}

/// Check the master socket without dialing out, before paying for a handshake.
/// This does not prove the host still accepts sessions; only a command can.
pub fn check(url: &Url, ctl: &Path) -> Vec<String> {
    control(url, ctl, "check")
}

/// Retire a master without interrupting its active shells. Release its
/// socket and refuse new sessions; exit after existing sessions end.
pub fn stop(url: &Url, ctl: &Path) -> Vec<String> {
    control(url, ctl, "stop")
}

/// Which machine listens for a forward's connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// This machine listens on the local port and what connects there reaches the remote one:
    /// `ssh -L`.
    ToRemote,
    /// The host listens on the remote port and what connects there reaches this machine:
    /// `ssh -R`. The host's sshd binds it to its loopback only, unless its own `GatewayPorts`
    /// says otherwise.
    ToLocal,
}

impl Direction {
    /// The arrow for it, pointing the way a connection travels: from the port that listens to the
    /// one it reaches, with the local port on the left.
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::ToRemote => "→",
            Direction::ToLocal => "←",
        }
    }
}

/// One port forward over the master: a port on this machine, a port on the host, and which of the
/// two listens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forward {
    pub local: u16,
    pub remote: u16,
    pub direction: Direction,
}

/// `8080 → 80`, the local port first either way round: the Ports pane's row, and a forward named
/// in a message.
impl std::fmt::Display for Forward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {}",
            self.local,
            self.direction.arrow(),
            self.remote
        )
    }
}

/// Add a forward to the running master.
pub fn forward(url: &Url, ctl: &Path, f: Forward) -> Vec<String> {
    let mut argv = control(url, ctl, "forward");
    insert_forward(&mut argv, f);
    argv
}

/// Take a forward back off the running master. ssh matches it by the same spec it was added with.
pub fn cancel(url: &Url, ctl: &Path, f: Forward) -> Vec<String> {
    let mut argv = control(url, ctl, "cancel");
    insert_forward(&mut argv, f);
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

/// `-L` or `-R` belongs with the options, ahead of the destination that [`control`] already
/// appended. Either spec leads with the port that listens, which for `-R` is the host's.
fn insert_forward(argv: &mut Vec<String>, f: Forward) {
    let (flag, spec) = match f.direction {
        Direction::ToRemote => ("-L", format!("{}:localhost:{}", f.local, f.remote)),
        Direction::ToLocal => ("-R", format!("{}:localhost:{}", f.remote, f.local)),
    };
    let at = argv.len() - 1;
    argv.insert(at, flag.to_string());
    argv.insert(at + 1, spec);
}

/// Run a command over the master and collect output. Bound a fallback connection
/// when its socket has gone stale, and refuse prompts without a terminal.
pub fn run(url: &Url, ctl: &Path, command: &str) -> Vec<String> {
    let mut argv = base(url, ctl);
    argv.extend(
        [
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            "ServerAliveCountMax=3",
        ]
        .map(String::from),
    );
    argv.push(url.destination());
    // Everything after `--` is the remote command, even when it starts with a dash.
    argv.push("--".to_string());
    argv.push(command.to_string());
    argv
}

/// Attach to held shell `id`, starting it in `url`'s path if absent. `server`
/// is a [`server_path`] shell expression. Force a pty with `-t` for raw mode
/// and sizing, and disable SSH escapes with `-e none` so `~.` reaches the shell.
/// Allow prompts in the tab if its master is gone. Missing vault paths use
/// the holder's login-home fallback.
pub fn attach(url: &Url, ctl: &Path, server: &str, id: &str) -> Vec<String> {
    let mut argv = base(url, ctl);
    argv.extend(["-t", "-e", "none"].map(String::from));
    argv.push(url.destination());
    argv.push(format!(
        "{server} attach --cwd {} {}",
        quote(&url.path.to_string_lossy()),
        quote(id)
    ));
    argv
}

/// A debugged program's command line on the host: `args` in `cwd` with `env` added, as the
/// adapter's `runInTerminal` asks, run by the window in the program's terminal tab. [`attach`]'s
/// sibling, `-t` and `-e none` alike, but not held: the program ends with its tab.
pub fn program(
    url: &Url,
    ctl: &Path,
    cwd: &str,
    env: &[(String, String)],
    args: &[String],
) -> Vec<String> {
    let mut argv = base(url, ctl);
    argv.extend(["-t", "-e", "none"].map(String::from));
    argv.push(url.destination());
    let exec = match env.is_empty() {
        true => "exec",
        false => "exec env",
    };
    let set = env.iter().map(|(k, v)| quote(&format!("{k}={v}")));
    let words: Vec<String> = set.chain(args.iter().map(|a| quote(a))).collect();
    argv.push(format!("cd {} && {exec} {}", quote(cwd), words.join(" ")));
    argv
}

// ------------------------------------------------------- server provisioning

/// Supported Linux `uname -m` names, each built as `<arch>-unknown-linux-musl`.
/// Reject other hosts instead of uploading an incompatible binary.
pub const SERVER_ARCHES: [&str; 2] = ["x86_64", "aarch64"];

/// Where the uploaded `accent-cli` lives on the remote, relative to the login's home.
pub const SERVER_DIR: &str = ".local/share/accent/server";

/// The binary is named after the build's contents, so a mismatched build is a missing file rather
/// than a protocol error at the first request.
pub fn server_name(hash: &str) -> String {
    format!("accent-cli-{hash}")
}

/// Remote server path as a shell expression. Leave `$HOME` unquoted for
/// remote expansion; `hash` is hex. Home paths containing spaces are unsupported.
pub fn server_path(hash: &str) -> String {
    format!("$HOME/{SERVER_DIR}/{}", server_name(hash))
}

/// First 16 hex chars of the blake3 of `bytes`. Same shape as `config::vault_hash`, so remote and
/// local names read alike.
pub fn hash_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_string()
}

/// Probe `uname -sm`, installed-server byte count (zero if absent) and `xz` or `raw`,
/// one per stdout line. Check size to reject truncated binaries left by older builds.
/// Use stdout rather than exit status so a failed session is not mistaken
/// for a missing server.
pub fn have_server_cmd(hash: &str) -> String {
    let installed = server_path(hash);
    format!(
        "uname -sm; if test -x {installed}; then wc -c < {installed}; else echo 0; fi; \
         if command -v xz >/dev/null 2>&1; then echo xz; else echo raw; fi"
    )
}

/// Install `size` uncompressed bytes through a private temporary file, decoding XZ if asked.
/// Check decoding and length before renaming; sweep unused older builds only after success.
pub fn install_server_cmd(hash: &str, size: usize, xz: bool) -> String {
    let dir = format!("$HOME/{SERVER_DIR}");
    let name = server_name(hash);
    let template = format!("{dir}/.{name}.XXXXXX");
    let tmp = "\"$tmp\"";
    let installed = server_path(hash);
    let read = if xz { "xz -dc" } else { "cat" };
    format!(
        "mkdir -p {dir} && \
         tmp=$(mktemp {template}) && \
         if ! {read} > {tmp}; then \
         rm -f {tmp}; echo 'cannot receive the server' >&2; exit 1; fi && \
         if [ \"$(wc -c < {tmp})\" -ne {size} ]; then \
         rm -f {tmp}; echo 'the upload was cut short' >&2; exit 1; fi && \
         chmod 755 {tmp} && mv -f {tmp} {installed} && \
         if [ -d /proc/self ]; then \
         for f in {dir}/accent-cli-*; do \
         [ \"$f\" = {installed} ] && continue; used=0; \
         for exe in /proc/[0-9]*/exe; do \
         if [ \"$f\" -ef \"$exe\" ]; then used=1; break; fi; done; \
         [ \"$used\" = 1 ] || rm -f \"$f\"; done; fi"
    )
}

/// The exit status [`put_cmd`] answers a taken name with, its stderr naming the file.
pub const TAKEN: i32 = 17;

/// Write exactly `size` stdin bytes atomically to `path`. Use an unwatched
/// `.accent-` temporary beside it, check length, then rename so interrupted
/// uploads preserve the original. Errors name the destination folder.
///
/// With `replace`, follow symlinks, keep the existing mode, and leave a file moved or deleted
/// during upload absent instead of recreating it under its old name. Without, the name must be
/// free, a final symlink included: once every byte is there it is claimed empty with `set -C`,
/// which every POSIX shell (dash, bash, zsh, BusyBox and BSD `sh`) opens with `O_EXCL` on any
/// filesystem, and the rename then lands on that claim; a taken one exits [`TAKEN`]. The test
/// before the claim keeps it off a FIFO, which `set -C` would open and wait on. The mode is GNU
/// `stat -c`'s, else BSD `stat -f`'s, as macOS has.
pub fn put_cmd(path: &str, size: u64, replace: bool) -> String {
    let path = quote(path);
    let (resolve, land) = match replace {
        true => (
            format!("p=$(readlink -f -- {path}) || p={path}"),
            "m=$(stat -c %a \"$p\" 2>/dev/null || stat -f %Lp \"$p\" 2>/dev/null) \
             && chmod \"$m\" \"$t\"; \
             [ -e \"$p\" ] || { echo \"${p##*/} was moved or deleted\" >&2; exit 1; }; "
                .to_string(),
        ),
        false => (
            format!("p={path}"),
            format!(
                "{{ [ ! -e \"$p\" ] && [ ! -L \"$p\" ] && (set -C; : > \"$p\") 2>/dev/null; }} \
                 || {{ echo \"${{p##*/}}\" >&2; exit {TAKEN}; }}; "
            ),
        ),
    };
    format!(
        "{resolve}; t=\"${{p%/*}}/.accent-$$\"; \
         trap 'rm -f \"$t\"' EXIT; trap 'exit 1' HUP INT TERM; \
         true 2>/dev/null > \"$t\" || {{ echo \"cannot write in ${{p%/*}}\" >&2; exit 1; }}; \
         cat > \"$t\" || exit 1; \
         if [ \"$(wc -c < \"$t\")\" -ne {size} ]; then \
         echo 'the upload was cut short' >&2; exit 1; fi; \
         {land}mv -f \"$t\" \"$p\""
    )
}

/// The command that starts the headless server on a vault. `server` is a path expression from
/// [`server_path`]; the root is a literal, so it is quoted.
pub fn serve_cmd(server: &str, root: &Path) -> String {
    format!("{server} serve --vault {}", quote(&root.to_string_lossy()))
}

/// The command that ends the shell `id` held on the host, for [`run`]: a Close Tab, which must
/// not wait on a prompt.
pub fn kill_cmd(server: &str, id: &str) -> String {
    format!("{server} kill {}", quote(id))
}

/// The command that prints the last copy (OSC 52) the shell `id` made on the host, for [`run`].
pub fn clip_cmd(server: &str, id: &str) -> String {
    format!("{server} clip {}", quote(id))
}

/// The command that lists the shells the host holds, for [`run`].
pub fn held_cmd(server: &str) -> String {
    format!("{server} held")
}

/// Share a held shell with the MCP server for this vault on the same host.
pub fn share_cmd(server: &str, id: &str, vault: &Path) -> String {
    format!(
        "{server} share {} --vault {}",
        quote(id),
        quote(&vault.to_string_lossy())
    )
}

/// Revoke MCP access to a held shell on the host.
pub fn revoke_cmd(server: &str, id: &str) -> String {
    format!("{server} revoke {}", quote(id))
}

// --------------------------------------------------------------------- cache

/// Find the musl-static server for `arch` ([`SERVER_ARCHES`]), or none if absent.
/// Search installed `../lib/accent`, then workspace musl output, supporting
/// installed and development launches without config. The local glibc-linked
/// `accent-cli` may not run on older hosts.
pub fn server_binary(arch: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Up the ancestors rather than one fixed step: the installed layout puts us in `bin/` beside
    // `lib/accent`, a development build runs from `target/<profile>/`, and a test binary from
    // `target/debug/deps/`. Four levels reaches the cargo target directory from all three.
    for dir in exe.ancestors().skip(1).take(4) {
        for candidate in [
            dir.join(format!("lib/accent/accent-cli-{arch}")),
            dir.join(format!("../lib/accent/accent-cli-{arch}")),
            dir.join(format!("{arch}-unknown-linux-musl/release/accent-cli")),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Where files fetched from this remote are kept, under the same short name as the socket.
pub fn cache_dir(url: &Url) -> PathBuf {
    accent_core::config::xdg("XDG_CACHE_HOME", ".cache")
        .join("accent")
        .join("remote")
        .join(id(url))
}

/// Separate `<id>.etag` tree for cached-file stamps. Keeping stamps outside
/// the file tree prevents collisions between extensions and vault `.etag` files.
/// Older adjacent stamps are orphaned.
fn stamp_dir(url: &Url) -> PathBuf {
    cache_dir(url).with_extension("etag")
}

/// Join a wire-supplied remote path to the cache, rejecting lexical escapes.
/// Do not touch the filesystem, so validation also works for missing paths.
/// This prevents a remote `..` from choosing an arbitrary local write target.
pub fn cache_path(url: &Url, rel: &str) -> Option<PathBuf> {
    inside(cache_dir(url), rel)
}

/// Where the stamp of `rel`'s cached copy goes: its [`cache_path`] in the stamp tree.
pub fn stamp_path(url: &Url, rel: &str) -> Option<PathBuf> {
    inside(stamp_dir(url), rel)
}

/// Pending cached-PDF page edits for note relinking (`Relinks`), stored via
/// [`cache_path`] in a separate `<id>.relink` tree.
pub fn relink_path(url: &Url, rel: &str) -> Option<PathBuf> {
    inside(cache_dir(url).with_extension("relink"), rel)
}

/// `rel` joined onto `base`, or `None` where it would land outside it (see [`cache_path`]).
fn inside(base: PathBuf, rel: &str) -> Option<PathBuf> {
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
    fn the_authority_and_the_path_make_the_address_again() {
        for text in [
            "ssh://box/srv/vault",
            "ssh://me@box/srv/vault",
            "ssh://box:2222/srv/vault",
            "ssh://me@[::1]:22/srv/vault",
        ] {
            let url = parse(text).expect("the fixtures parse");
            let again = format!("ssh://{}{}", url.authority(), url.path.display());
            assert_eq!(parse(&again), Ok(url));
        }
        assert_eq!(ported().authority(), "me@box:2222");
        assert_eq!(plain().authority(), "box");
        assert_eq!(
            parse("ssh://[::1]/srv").map(|u| u.authority()),
            Ok("[::1]".to_string())
        );
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
        assert_eq!(parsed("/srv/vault"), Err("not an ssh:// address".into()));
        assert_eq!(parsed("ssh://box"), Err("the path must be absolute".into()));
        assert_eq!(parsed("ssh:///srv"), Err("no host in the address".into()));
        assert_eq!(
            parsed("ssh://box:http/srv"),
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
            master(&plain(), ctl(), false),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=60",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "box",
                "true",
            ])
        );
        // An attempt nobody asked for never prompts.
        assert_eq!(
            master(&ported(), ctl(), true),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-o",
                "BatchMode=yes",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=60",
                "-o",
                "ConnectTimeout=10",
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
    fn exit_addresses_the_running_master() {
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
        assert_eq!(
            check(&ported(), ctl()),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-O",
                "check",
                "me@box",
            ])
        );
        assert_eq!(
            stop(&plain(), ctl()),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-O",
                "stop",
                "box",
            ])
        );
    }

    #[test]
    fn a_forward_and_its_cancellation_name_the_same_pair() {
        let out = Forward {
            local: 8080,
            remote: 80,
            direction: Direction::ToRemote,
        };
        assert_eq!(
            forward(&plain(), ctl(), out),
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
            cancel(&ported(), ctl(), out),
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
        // The other way round the host listens, so its port leads the spec.
        let back = Forward {
            direction: Direction::ToLocal,
            ..out
        };
        for (op, argv) in [
            ("forward", forward(&plain(), ctl(), back)),
            ("cancel", cancel(&plain(), ctl(), back)),
        ] {
            assert_eq!(
                argv,
                words(&[
                    "ssh",
                    "-o",
                    "ControlPath=/run/user/1000/accent/0123456789abcdef",
                    "-O",
                    op,
                    "-R",
                    "80:localhost:8080",
                    "box",
                ])
            );
        }
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
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
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
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
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
    fn a_held_shell_is_attached_over_the_master_in_a_pty_of_its_own() {
        let server = server_path("0123456789abcdef");
        let id = "89abcdef01234567";
        assert_eq!(
            attach(&plain(), ctl(), &server, id),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-t",
                "-e",
                "none",
                "box",
                &format!("{server} attach --cwd '/srv/vault' '{id}'"),
            ])
        );
        let spaced = parse("ssh://me@box:2222/srv/my vault").expect("a spaced path parses");
        assert_eq!(
            attach(&spaced, ctl(), &server, id),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-t",
                "-e",
                "none",
                "me@box",
                &format!("{server} attach --cwd '/srv/my vault' '{id}'"),
            ])
        );
        // Ending it is a one-shot command over the same master, which never prompts, and so is
        // taking the copy a program in it made.
        assert_eq!(
            run(&spaced, ctl(), &kill_cmd(&server, id)).last(),
            Some(&format!("{server} kill '{id}'"))
        );
        assert_eq!(clip_cmd(&server, id), format!("{server} clip '{id}'"));
        assert_eq!(
            share_cmd(&server, id, Path::new("/srv/my vault")),
            format!("{server} share '{id}' --vault '/srv/my vault'")
        );
        assert_eq!(revoke_cmd(&server, id), format!("{server} revoke '{id}'"));
    }

    #[test]
    fn a_debugged_program_runs_over_the_master_in_its_folder_and_environment() {
        let env = [("A".to_string(), "b c".to_string())];
        let args = words(&["python3", "it's.py"]);
        assert_eq!(
            program(&ported(), ctl(), "/srv/my vault", &env, &args),
            words(&[
                "ssh",
                "-o",
                "ControlPath=/run/user/1000/accent/0123456789abcdef",
                "-p",
                "2222",
                "-t",
                "-e",
                "none",
                "me@box",
                r"cd '/srv/my vault' && exec env 'A=b c' 'python3' 'it'\''s.py'",
            ])
        );
        assert_eq!(
            program(&plain(), ctl(), "/srv", &[], &args).last(),
            Some(&r"cd '/srv' && exec 'python3' 'it'\''s.py'".to_string())
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
        // An empty PATH models a host without xz; a shell function makes it available.
        let scratch = tempfile::tempdir().unwrap();
        let probe =
            have_server_cmd(hash).replace("$HOME", &quote(scratch.path().to_str().unwrap()));
        for (setup, encoding) in [("", "raw"), ("xz() { :; };", "xz")] {
            let cmd = format!("PATH=; uname() {{ echo 'Linux x86_64'; }}; {setup} {probe}");
            let out = std::process::Command::new("sh")
                .args(["-c", &cmd])
                .output()
                .unwrap();
            assert!(out.status.success());
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                format!("Linux x86_64\n0\n{encoding}\n")
            );
        }
    }

    #[test]
    fn installing_the_server_is_atomic_and_sweeps_stale_builds() {
        let hash = "0123456789abcdef";
        let cmd = install_server_cmd(hash, 8_300_000, false);
        let installed = server_path(hash);
        let tmp = "\"$tmp\"";
        assert!(cmd.contains("mkdir -p $HOME/.local/share/accent/server"));
        assert!(cmd.contains(
            "mktemp $HOME/.local/share/accent/server/.accent-cli-0123456789abcdef.XXXXXX"
        ));
        // The binary is never written to the path it is finally seen at.
        assert!(cmd.contains(&format!("cat > {tmp}")));
        assert!(cmd.contains(&format!("chmod 755 {tmp}")));
        assert!(cmd.contains(&format!("mv -f {tmp} {installed}")));
        assert!(!cmd.contains(&format!("cat > {installed}")));
        // A short upload is counted, removed and refused before anything is renamed.
        let counted = format!("if [ \"$(wc -c < {tmp})\" -ne 8300000 ]; then rm -f {tmp};");
        let counted = cmd.find(&counted).expect(&cmd);
        assert!(counted < cmd.find("mv -f").expect(&cmd));
        // The sweep spares what was just installed.
        assert!(cmd.contains("accent-cli-*"));
        assert!(cmd.contains(&format!("[ \"$f\" = {installed} ] && continue")));
    }

    #[test]
    fn a_server_install_requires_complete_decoding_and_the_right_size() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join(SERVER_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let hash = "0123456789abcdef";
        let installed = dir.join(server_name(hash));
        let old = dir.join("accent-cli-old");
        std::fs::write(&installed, b"working server").unwrap();
        std::fs::write(&old, b"older server").unwrap();
        let raw = b"new binary";
        let packed = Command::new("python3")
            .args([
                "-c",
                "import lzma,sys; sys.stdout.buffer.write(lzma.compress(b'new binary'))",
            ])
            .output()
            .unwrap();
        assert!(packed.status.success());
        let packed = packed.stdout;
        let install = |bytes: &[u8], size, xz| {
            let cmd = install_server_cmd(hash, size, xz)
                .replace("$HOME", &quote(scratch.path().to_str().unwrap()));
            let mut child = Command::new("sh")
                .args(["-c", &cmd])
                .stdin(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(bytes).unwrap();
            child.wait_with_output().unwrap().status.success()
        };
        // Missing footer: xz can have produced every byte yet still fail validation.
        let mut corrupt = packed.clone();
        corrupt[0] ^= 1;
        for (bytes, size, xz) in [
            (&packed[..packed.len() - 1], raw.len(), true),
            (&corrupt[..], raw.len(), true),
            (&packed[..], raw.len() + 1, true),
            (&raw[..raw.len() - 1], raw.len(), false),
        ] {
            assert!(!install(bytes, size, xz));
            assert_eq!(std::fs::read(&installed).unwrap(), b"working server");
            assert!(old.exists());
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        }
        assert!(install(&packed, raw.len(), true));
        assert_eq!(std::fs::read(&installed).unwrap(), raw);
        assert_ne!(
            std::fs::metadata(&installed).unwrap().permissions().mode() & 0o111,
            0
        );
        // Server hosts are Linux; on a test host without /proc cleanup must retain old builds.
        let retained = !Path::new("/proc/self").is_dir();
        assert_eq!(old.exists(), retained);
        assert!(install(b"raw fallback", 12, false));
        assert_eq!(std::fs::read(&installed).unwrap(), b"raw fallback");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1 + usize::from(retained)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn installing_a_server_retains_executables_still_used_by_a_process() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join(SERVER_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("accent-cli-old");
        std::fs::copy("/bin/sleep", &old).unwrap();
        let mut live = Command::new(&old).arg("30").spawn().unwrap();
        let command = install_server_cmd("0123456789abcdef", 3, false)
            .replace("$HOME", &quote(scratch.path().to_str().unwrap()));
        let install = || {
            let mut child = Command::new("sh")
                .args(["-c", &command])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(b"new").unwrap();
            child.wait().unwrap().success()
        };
        let installed = install();
        let retained = old.exists();
        live.kill().unwrap();
        live.wait().unwrap();
        assert!(installed);
        assert!(retained, "an update removed a running holder's executable");
        assert!(install());
        assert!(!old.exists(), "an unused older build was not swept");
    }

    #[test]
    fn overlapping_server_uploads_do_not_write_into_an_installed_binary() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join(SERVER_DIR);
        let hash = "0123456789abcdef";
        let command = install_server_cmd(hash, 8, false)
            .replace("$HOME", &quote(scratch.path().to_str().unwrap()));
        let start = || {
            Command::new("sh")
                .args(["-c", &command])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let mut first = start();
        let mut input = first.stdin.take().unwrap();
        input.write_all(b"aaaa").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_dir(&dir).is_ok_and(|mut files| {
            files.any(|entry| {
                entry
                    .ok()
                    .and_then(|entry| entry.metadata().ok())
                    .is_some_and(|meta| meta.len() == 4)
            })
        }) {
            assert!(Instant::now() < deadline, "first upload never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut second = start();
        second.stdin.take().unwrap().write_all(b"BBBBBBBB").unwrap();
        assert!(second.wait().unwrap().success());
        let installed = dir.join(server_name(hash));
        assert_eq!(std::fs::read(&installed).unwrap(), b"BBBBBBBB");
        // The first upload continues but is cut short: it must not change what just landed.
        input.write_all(b"aa").unwrap();
        drop(input);
        assert!(!first.wait().unwrap().success());
        assert_eq!(std::fs::read(&installed).unwrap(), b"BBBBBBBB");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    /// The command run by the shell here, as the host's runs it: a file replaced only once every
    /// byte is there, keeping its mode and its symlink, one cut short left as it was, and a name
    /// that is taken, by a file or a dangling link, never written over unless asked to.
    #[test]
    fn an_upload_lands_whole_or_not_at_all() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let put_as = |path: &Path, bytes: &[u8], size: u64, replace: bool| {
            let mut sh = Command::new("sh")
                .arg("-c")
                .arg(put_cmd(&path.to_string_lossy(), size, replace))
                .stdin(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            sh.stdin.take().unwrap().write_all(bytes).unwrap();
            sh.wait_with_output().unwrap().status.code()
        };
        let put = |path: &Path, bytes: &[u8], size: u64| put_as(path, bytes, size, false);
        let (done, failed, taken) = (Some(0), Some(1), Some(TAKEN));
        let (file, link) = (dir.path().join("a b.pdf"), dir.path().join("link.pdf"));
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&file, &link).unwrap();

        assert_eq!(put(&file, b"ne", 3), failed);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        assert_eq!(put(&file, b"new", 3), taken);
        assert_eq!(put(&link, b"new", 3), taken);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        assert_eq!(put_as(&link, b"new", 3, true), done);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        assert!(link.is_symlink());
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
        // A file nothing held yet, unless it is one to replace, which has gone meanwhile; and no
        // temporary left beside any of them.
        assert_eq!(put(&dir.path().join("new.png"), b"png", 3), done);
        assert_eq!(std::fs::read(dir.path().join("new.png")).unwrap(), b"png");
        assert_eq!(
            put_as(&dir.path().join("moved.pdf"), b"pdf", 3, true),
            failed
        );
        let dangling = dir.path().join("dangling.png");
        std::os::unix::fs::symlink("nowhere.png", &dangling).unwrap();
        assert_eq!(put(&dangling, b"png", 3), taken);
        assert!(!dir.path().join("nowhere.png").exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
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

    #[test]
    fn a_stamp_has_a_tree_of_its_own_beside_the_cache() {
        let url = ported();
        let (cache, stamps) = (cache_dir(&url), stamp_dir(&url));
        assert_eq!(stamps.parent(), cache.parent());
        assert!(!stamps.starts_with(&cache) && !cache.starts_with(&stamps));
        assert_eq!(
            stamp_path(&url, "notes/a.pdf"),
            Some(stamps.join("notes/a.pdf"))
        );
        // Two files that differ only in their extension, and a file named like a stamp.
        assert_ne!(stamp_path(&url, "a.pdf"), stamp_path(&url, "a.png"));
        assert_ne!(stamp_path(&url, "x"), cache_path(&url, "x.etag"));
        assert_eq!(stamp_path(&url, "../../etc/passwd"), None);
    }
}
