//! One holder per user and machine owns shells, ptys, and screen models. A single screen
//! lock orders output replay and live forwarding. Stalled readers are disconnected after
//! [`STALL`] so they cannot block a pty indefinitely.

use super::protocol::{self, DETACHED, EXIT, HELLO, Hello, INPUT, KILL, LIST, OUTPUT, RESIZE};
use super::{FAILED, LOCK, SOCKET, lock, screen};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Maximum time a terminal may leave output unread before disconnection.
const STALL: Duration = Duration::from_secs(5);
/// Interval between checks for a shell whose background job keeps the pty open.
const IDLE_MS: i32 = 1000;
/// Grace period between hangup and forced kill in [`Daemon::kill`].
const GRACE: Duration = Duration::from_secs(1);
/// Maximum pty read and replay chunk size.
const CHUNK: usize = 64 * 1024;
/// Idle timeout before the first shell starts. Allows more than `client::connect`'s two-second
/// connection window, then removes a holder whose attaching client died.
const WAIT: Duration = Duration::from_secs(10);

/// Distinguishes terminals so a departing one cannot detach its replacement.
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Daemon {
    shells: Mutex<Shells>,
    socket: PathBuf,
    /// Last terminal connection time for [`Daemon::watch`].
    seen: Mutex<Instant>,
}

/// Held shells and pending kills share one lock, preventing a late attach after Close Tab.
#[derive(Default)]
struct Shells {
    live: HashMap<String, Arc<Shell>>,
    killed: HashSet<String>,
}

struct Shell {
    pid: u32,
    /// Started command and epoch time, reported by `held`.
    program: PathBuf,
    started: u64,
    /// Owner reported by `held` ([`Hello::owner`]).
    owner: String,
    master: File,
    screen: Mutex<Screen>,
}

/// Shell state shared by the pump and connections under one lock.
struct Screen {
    parser: screen::Parser,
    client: Option<(u64, UnixStream)>,
    exit: Option<i32>,
}

/// Hold shells until the last exits; return immediately if another holder is running.
pub fn run() -> io::Result<()> {
    let dir = super::dir()?;
    // The lock settles holders started at once by attaches racing each other: one gets it and
    // binds, the others leave, and their clients find the winner on the next try. It is held for
    // as long as this process lives.
    let lock = File::create(dir.join(LOCK))?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    let socket = dir.join(SOCKET);
    // A socket here is a dead holder's: a live one would hold the lock.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    let daemon = Arc::new(Daemon {
        shells: Mutex::default(),
        socket,
        seen: Mutex::new(Instant::now()),
    });
    let watched = daemon.clone();
    thread::spawn(move || watched.watch());
    for conn in listener.incoming().flatten() {
        // `super::lock`: `lock` here is the lock file.
        *super::lock(&daemon.seen) = Instant::now();
        let daemon = daemon.clone();
        thread::spawn(move || daemon.serve(conn));
    }
    Ok(())
}

impl Daemon {
    /// Handle one connection according to its first frame.
    fn serve(self: Arc<Self>, mut conn: UnixStream) {
        let Ok(Some((tag, payload))) = protocol::read_frame(&mut conn) else {
            return;
        };
        match tag {
            LIST => {
                let _ = protocol::write_frame(&mut conn, OUTPUT, &self.list());
            }
            KILL => {
                // Let `accent-cli kill` go before the grace period, not after it.
                drop(conn);
                self.kill(&String::from_utf8_lossy(&payload));
            }
            HELLO => {
                if let Some(hello) = Hello::decode(&payload) {
                    self.attach(&hello, conn);
                }
            }
            _ => {}
        }
    }

    /// Attach a terminal, starting its shell if needed, and forward input until disconnection.
    fn attach(self: &Arc<Self>, hello: &Hello, mut conn: UnixStream) {
        let shell = match self.open(hello) {
            Ok(Some(shell)) => shell,
            // Killed before it started: it ends the way the kill would have ended it, with the
            // status of a hangup. The holder stays for [`WAIT`], which is what [`Daemon::watch`]
            // gives it: another tab closed in the same moment may still have its HELLO on the
            // way, and only this holder remembers its kill.
            Ok(None) => {
                let _ = protocol::write_frame(&mut conn, EXIT, &(128 + libc::SIGHUP).to_be_bytes());
                return;
            }
            Err(e) => {
                let why = format!(
                    "accent-cli: cannot start {}: {e}\r\n",
                    program(hello).display()
                );
                let _ = protocol::write_frame(&mut conn, OUTPUT, why.as_bytes());
                let _ = protocol::write_frame(&mut conn, EXIT, &FAILED.to_be_bytes());
                // A holder started for this shell alone has nothing left to hold.
                self.leave_if_idle();
                return;
            }
        };
        let me = NEXT.fetch_add(1, Ordering::Relaxed);
        if !shell.attach(me, &conn, hello.rows, hello.cols) {
            return;
        }
        loop {
            match protocol::read_frame(&mut conn) {
                // A shell that has just ended cannot take it; the pump is about to say so.
                Ok(Some((INPUT, keys))) => {
                    let _ = (&shell.master).write_all(&keys);
                }
                Ok(Some((RESIZE, size))) => {
                    if let Some((rows, cols)) = protocol::parse_size(&size) {
                        shell.resize(rows, cols);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        shell.detach(me);
    }

    /// Find or start `hello`'s shell under the lock. Return `None` if a prior kill names it.
    fn open(self: &Arc<Self>, hello: &Hello) -> io::Result<Option<Arc<Shell>>> {
        let mut shells = lock(&self.shells);
        if let Some(shell) = shells.live.get(&hello.id) {
            return Ok(Some(shell.clone()));
        }
        if shells.killed.remove(&hello.id) {
            return Ok(None);
        }
        let (master, slave) = open_pty()?;
        set_winsize(&master, hello.rows, hello.cols);
        let child = spawn(hello, slave)?;
        let shell = Arc::new(Shell {
            pid: child.id(),
            program: program(hello).to_path_buf(),
            started: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
            owner: hello.owner.clone(),
            master,
            screen: Mutex::new(Screen {
                parser: screen::parser(hello.rows, hello.cols),
                client: None,
                exit: None,
            }),
        });
        shells.live.insert(hello.id.clone(), shell.clone());
        let (daemon, id, pumped) = (self.clone(), hello.id.clone(), shell.clone());
        thread::spawn(move || daemon.pump(&id, &pumped, child));
        Ok(Some(shell))
    }

    /// Read the shell's output until it ends, into its screen model and on to its terminal, then
    /// reap it and say how it ended. The pump owns the `Child`, so the holder needs no SIGCHLD
    /// handler.
    fn pump(&self, id: &str, shell: &Shell, mut child: Child) {
        let mut buf = vec![0; CHUNK];
        loop {
            let mut ready = libc::pollfd {
                fd: shell.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid `pollfd`, whose descriptor `shell` keeps open.
            if unsafe { libc::poll(&mut ready, 1, IDLE_MS) } == 0 {
                match child.try_wait() {
                    Ok(None) => continue,
                    _ => break,
                }
            }
            match (&shell.master).read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => shell.output(&buf[..n]),
            }
        }
        let code = child.wait().map_or(FAILED, status);
        self.gone(id, shell, code);
    }

    /// Forget an ended shell, notify its terminal, and exit if it was the last.
    fn gone(&self, id: &str, shell: &Shell, code: i32) {
        lock(&self.shells).live.remove(id);
        shell.ended(code);
        self.leave_if_idle();
    }

    /// Exit after [`WAIT`] without a shell or terminal, including when an attaching client dies.
    /// After the first shell, exit when the last one ends.
    fn watch(&self) {
        loop {
            let left = wait_left(*lock(&self.seen), Instant::now());
            if left.is_zero() {
                return self.leave_if_idle();
            }
            thread::sleep(left);
        }
    }

    /// Exit, taking the socket along, when no shell is held. The check and the exit happen under
    /// the lock `open` takes, so no shell can be started in between.
    fn leave_if_idle(&self) {
        let shells = lock(&self.shells);
        if shells.live.is_empty() {
            let _ = std::fs::remove_file(&self.socket);
            std::process::exit(0);
        }
    }

    /// Hang up shell `id` and its foreground process groups, then force-kill survivors after
    /// [`GRACE`]. Remember unknown IDs to prevent pending attachments from creating a shell.
    fn kill(&self, id: &str) {
        let shell = {
            let mut shells = lock(&self.shells);
            match shells.live.get(id) {
                Some(shell) => shell.clone(),
                None => {
                    shells.killed.insert(id.to_string());
                    return;
                }
            }
        };
        // SAFETY: a plain query on a descriptor `shell` keeps open.
        let foreground = unsafe { libc::tcgetpgrp(shell.master.as_raw_fd()) };
        let groups = [shell.pid as libc::pid_t, foreground];
        let signal = |sig| {
            for group in groups.into_iter().filter(|&g| g > 0) {
                // SAFETY: `kill` takes plain integers. A group's id stays taken while anything in
                // it lives, so the second signal reaches what is left of it or nothing (git.rs).
                unsafe { libc::kill(-group, sig) };
            }
        };
        signal(libc::SIGHUP);
        thread::sleep(GRACE);
        signal(libc::SIGKILL);
    }

    /// One JSON object per line and shell: its id, pid, working directory, title, whether a
    /// terminal is attached, the program started, when and for whom, and what runs in its
    /// foreground.
    fn list(&self) -> Vec<u8> {
        // Copied out first, so the `screen` locks below are never taken under `shells`.
        let shells: Vec<_> = lock(&self.shells)
            .live
            .iter()
            .map(|(id, shell)| (id.clone(), shell.clone()))
            .collect();
        let mut out = Vec::new();
        for (id, shell) in shells {
            let cwd = cwd_of(shell.pid);
            let (title, attached) = {
                let term = lock(&shell.screen);
                let title = String::from_utf8_lossy(&term.parser.callbacks().0).into_owned();
                (title, term.client.is_some())
            };
            let line = serde_json::json!({
                "id": id,
                "pid": shell.pid,
                "cwd": cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
                "title": title,
                "attached": attached,
                "shell": shell.program.to_string_lossy(),
                "started": shell.started,
                "owner": shell.owner,
                "running": shell.foreground(),
            });
            out.extend(line.to_string().into_bytes());
            out.push(b'\n');
        }
        out
    }
}

impl Shell {
    /// Output from the pump: into the model, and to the terminal if one is attached. A terminal
    /// that left it unread for [`STALL`] is dropped, and comes back to a clean replay.
    fn output(&self, bytes: &[u8]) {
        let mut term = lock(&self.screen);
        term.parser.process(bytes);
        if let Some((_, client)) = &mut term.client
            && protocol::write_frame(client, OUTPUT, bytes).is_err()
        {
            let _ = client.shutdown(Shutdown::Both);
            term.client = None;
        }
    }

    /// Hand the shell to terminal `me`: size the pty and the model to it, take the shell from the
    /// terminal that had it, and send the replay. False when the shell has already ended (the
    /// terminal is told how) or the terminal went away meanwhile.
    fn attach(&self, me: u64, conn: &UnixStream, rows: u16, cols: u16) -> bool {
        let Ok(mut conn) = conn.try_clone() else {
            return false;
        };
        let mut term = lock(&self.screen);
        if let Some(code) = term.exit {
            let _ = protocol::write_frame(&mut conn, EXIT, &code.to_be_bytes());
            return false;
        }
        set_winsize(&self.master, rows, cols);
        screen::resize(&mut term.parser, rows, cols);
        if let Some((_, mut old)) = term.client.take() {
            // Non-blocking: a terminal stuck on its end must not hold up the one taking over.
            let _ = old.set_nonblocking(true);
            let _ = protocol::write_frame(&mut old, DETACHED, &[]);
            let _ = old.shutdown(Shutdown::Both);
        }
        let _ = conn.set_write_timeout(Some(STALL));
        let alt = term.parser.screen().alternate_screen();
        for piece in screen::replay(&mut term.parser).chunks(CHUNK) {
            if protocol::write_frame(&mut conn, OUTPUT, piece).is_err() {
                return false;
            }
        }
        term.client = Some((me, conn));
        drop(term);
        if alt {
            self.nudge();
        }
        true
    }

    fn resize(&self, rows: u16, cols: u16) {
        let mut term = lock(&self.screen);
        set_winsize(&self.master, rows, cols);
        screen::resize(&mut term.parser, rows, cols);
    }

    /// Detach terminal `me` while retaining the shell unless another terminal has taken over.
    fn detach(&self, me: u64) {
        let mut term = lock(&self.screen);
        if term.client.as_ref().is_some_and(|(id, _)| *id == me) {
            term.client = None;
        }
    }

    /// Record shell exit status `code` for later attachments.
    fn ended(&self, code: i32) {
        let mut term = lock(&self.screen);
        term.exit = Some(code);
        if let Some((_, mut client)) = term.client.take() {
            let _ = protocol::write_frame(&mut client, EXIT, &code.to_be_bytes());
            let _ = client.shutdown(Shutdown::Both);
        }
    }

    /// The command line of what runs in the foreground, `None` at the shell's own prompt: the
    /// pty's foreground group, named by its leader, the first command of a pipeline.
    fn foreground(&self) -> Option<String> {
        // SAFETY: a plain query on a descriptor `self` keeps open.
        let group = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        if group <= 0 || group as u32 == self.pid {
            return None;
        }
        command_line(&args_of(group as u32)?)
    }

    /// Request a redraw from full-screen programs whose state replay cannot restore.
    fn nudge(&self) {
        // SAFETY: plain integer calls on a descriptor `self` keeps open.
        unsafe {
            let group = libc::tcgetpgrp(self.master.as_raw_fd());
            if group > 0 {
                libc::kill(-group, libc::SIGWINCH);
            }
        }
    }
}

/// Open a pty pair without a controlling terminal or inheritable master descriptor.
fn open_pty() -> io::Result<(File, File)> {
    let open = |path: &Path| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(path)
    };
    let master = open(Path::new("/dev/ptmx"))?;
    let fd = master.as_raw_fd();
    // 128 bytes, as macOS's `TIOCPTYGNAME` writes.
    let mut name: [libc::c_char; 128] = [0; 128];
    // SAFETY: `fd` is an open pty master, and `name` is a buffer of the length given.
    unsafe {
        if libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 {
            return Err(io::Error::last_os_error());
        }
        // The error comes back as the return value, not through errno (musl sets none).
        #[cfg(not(target_os = "macos"))]
        match libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) {
            0 => {}
            e => return Err(io::Error::from_raw_os_error(e)),
        }
        // The libc crate has no `ptsname_r` for Apple, whose own asks this ioctl.
        #[cfg(target_os = "macos")]
        if libc::ioctl(fd, libc::TIOCPTYGNAME as _, name.as_mut_ptr()) == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: on success the call left a NUL-terminated path in `name`.
    let slave = unsafe { CStr::from_ptr(name.as_ptr()) };
    let slave = open(Path::new(OsStr::from_bytes(slave.to_bytes())))?;
    Ok((master, slave))
}

/// Set the pty size, causing SIGWINCH for its foreground job when changed.
fn set_winsize(pty: &File, rows: u16, cols: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ reads one `winsize`, which outlives the call. The constant goes in as
    // libc declares it: its type differs between glibc and musl, and a cast breaks one of them.
    unsafe { libc::ioctl(pty.as_raw_fd(), libc::TIOCSWINSZ, &size) };
}

/// Start the shell on the pty's slave end: `$SHELL` of the terminal's environment (else
/// `/bin/sh`), with that environment and nothing else, in the terminal's directory (else `$HOME`,
/// else `/`). Interactive and not a login shell, as VTE starts one.
fn spawn(hello: &Hello, slave: File) -> io::Result<Child> {
    let home = var(hello, "HOME").map(Path::new);
    let cwd = [Some(hello.cwd.as_path()), home]
        .into_iter()
        .flatten()
        .find(|dir| dir.is_dir())
        .unwrap_or(Path::new("/"));
    let mut cmd = Command::new(program(hello));
    cmd.env_clear()
        .envs(hello.env.iter().map(|(key, value)| (key, value)))
        .current_dir(cwd)
        .stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave);
    // SAFETY: only async-signal-safe calls run between fork and exec, and they touch no memory of
    // ours but the signal set on the stack.
    unsafe {
        cmd.pre_exec(|| {
            // No signal blocked: std passes the mask on, and the `attach` that started this holder
            // blocks SIGWINCH, which a shell must get. Then a session of its own, with the pty
            // (fd 0 by now) as its controlling terminal.
            let mut none = MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigemptyset(none.as_mut_ptr());
            if libc::sigprocmask(libc::SIG_SETMASK, none.as_ptr(), std::ptr::null_mut()) == -1
                || libc::setsid() == -1
                // Apple declares the request a `u32`, where its `ioctl` takes a `c_ulong`.
                || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // `cmd` drops with the holder's copies of the slave: from here only the shell holds it open.
    cmd.spawn()
}

/// Select the shell for `hello`.
fn program(hello: &Hello) -> &Path {
    var(hello, "SHELL").map_or(Path::new("/bin/sh"), Path::new)
}

/// Return a nonempty terminal environment variable.
fn var<'a>(hello: &'a Hello, name: &str) -> Option<&'a OsStr> {
    hello
        .env
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_os_str())
        .filter(|value| !value.is_empty())
}

/// Maximum command-line length in `held` output, bounded by [`protocol::MAX`].
const LONGEST: usize = 256;

/// Read a process command line, joining NUL-separated arguments and truncating at [`LONGEST`].
/// Return `None` for exited processes or kernel threads.
fn command_line(raw: &[u8]) -> Option<String> {
    let line = String::from_utf8_lossy(raw.strip_suffix(&[0]).unwrap_or(raw)).replace('\0', " ");
    if line.is_empty() {
        return None;
    }
    Some(match line.chars().nth(LONGEST) {
        Some(_) => line.chars().take(LONGEST - 1).chain(['…']).collect(),
        None => line,
    })
}

/// The working directory of process `pid`.
#[cfg(not(target_os = "macos"))]
fn cwd_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// macOS has no `/proc`: the kernel names a process's directory through `proc_pidinfo`.
#[cfg(target_os = "macos")]
fn cwd_of(pid: u32) -> Option<PathBuf> {
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: `info` is the `size` bytes the call writes at most.
    let wrote = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if wrote != size {
        return None;
    }
    // SAFETY: written whole, as `wrote` says.
    let path = unsafe { info.assume_init() }.pvi_cdir.vip_path;
    let bytes: Vec<u8> = path.as_flattened().iter().map(|&c| c as u8).collect();
    let path = CStr::from_bytes_until_nul(&bytes).ok()?;
    Some(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

/// The arguments process `pid` was started with, each ended by a NUL.
#[cfg(not(target_os = "macos"))]
fn args_of(pid: u32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/cmdline")).ok()
}

/// macOS has no `/proc`: `KERN_PROCARGS2` hands over the argument count, the executable's path
/// padded with NULs, then the arguments and the environment, each ended by a NUL.
#[cfg(target_os = "macos")]
fn args_of(pid: u32) -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size = 0;
    let mut buf = Vec::new();
    // SAFETY: the first call only says how large a buffer the second needs; the second writes at
    // most `size` bytes into one that long.
    unsafe {
        let mut ask = |buf: *mut u8, size: &mut libc::size_t| {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.cast(),
                size,
                std::ptr::null_mut(),
                0,
            )
        };
        if ask(std::ptr::null_mut(), &mut size) != 0 {
            return None;
        }
        buf.resize(size, 0);
        if ask(buf.as_mut_ptr(), &mut size) != 0 {
            return None;
        }
    }
    let (argc, rest) = buf[..size].split_first_chunk::<4>()?;
    let args = rest
        .split(|&b| b == 0)
        .skip(1)
        .skip_while(|f| f.is_empty())
        .take(i32::from_ne_bytes(*argc) as usize);
    Some(args.flat_map(|arg| [arg, b"\0"].concat()).collect())
}

/// Remaining [`WAIT`] time since the last terminal connected.
fn wait_left(seen: Instant, now: Instant) -> Duration {
    WAIT.saturating_sub(now.saturating_duration_since(seen))
}

/// Return the exit code or 128 plus the terminating signal.
fn status(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counted from the last terminal that connected; one that connected after the clock was read
    /// leaves the whole wait to go.
    #[test]
    fn a_holder_with_nothing_to_hold_waits_from_the_last_terminal() {
        let seen = Instant::now();
        assert_eq!(wait_left(seen, seen), WAIT);
        assert_eq!(
            wait_left(seen, seen + Duration::from_secs(3)),
            WAIT - Duration::from_secs(3)
        );
        assert_eq!(wait_left(seen, seen + WAIT), Duration::ZERO);
        assert_eq!(wait_left(seen, seen + WAIT * 2), Duration::ZERO);
        assert_eq!(wait_left(seen + Duration::from_secs(1), seen), WAIT);
    }

    #[test]
    fn a_command_line_reads_as_typed_and_never_outgrows_a_frame() {
        assert_eq!(
            command_line(b"vim\0notes.md\0").as_deref(),
            Some("vim notes.md")
        );
        // A kernel thread, or a process gone meanwhile, has none.
        assert_eq!(command_line(b""), None);
        let long = command_line("x\0".repeat(1000).as_bytes()).unwrap();
        assert_eq!(long.chars().count(), LONGEST);
        assert!(long.ends_with('…'));
    }

    /// What `held` says of a shell, read off this process: where it runs and how it was started.
    #[test]
    fn a_process_is_read_where_it_runs_and_as_it_was_started() {
        let me = std::process::id();
        assert_eq!(cwd_of(me), std::env::current_dir().ok());
        let typed: Vec<u8> = std::env::args_os()
            .flat_map(|arg| [arg.as_bytes(), b"\0"].concat())
            .collect();
        assert_eq!(args_of(me), Some(typed));
    }
}
