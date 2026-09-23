//! The holder: one process per user and machine that owns every held shell.
//!
//! Threads and blocking I/O: one thread accepts, one serves each connection, one pumps each
//! shell's output. A shell's `screen` lock guards its screen model, its terminal and its exit
//! status together, which is what makes a replay exact: every chunk of output is either in the
//! replay or sent live after it, never both. No thread holds two locks at once.
//!
//! Backpressure is the shell's: a terminal that reads slowly slows the shell down, which is right
//! over ssh. One that reads nothing for [`STALL`] is dropped, so a dead link never leaves a shell
//! blocked on its pty for longer (TCP alone takes about 15 minutes to notice one).

use super::protocol::{self, DETACHED, EXIT, HELLO, Hello, INPUT, KILL, LIST, OUTPUT, RESIZE};
use super::{FAILED, LOCK, SOCKET, lock, screen};
use std::collections::HashMap;
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
use std::time::Duration;

/// How long a terminal may leave output unread before it is dropped.
const STALL: Duration = Duration::from_secs(5);
/// How long a pty stays quiet before the pump checks that its shell is still there: a shell that
/// exited while a background job keeps the pty open (`firefox &`) never reads as end-of-file.
const IDLE_MS: i32 = 1000;
/// Between the hangup and the kill in [`Daemon::kill`].
const GRACE: Duration = Duration::from_secs(1);
/// The largest read from a pty, and the largest piece of a replay.
const CHUNK: usize = 64 * 1024;

/// Tells attached terminals apart, so that one leaving does not detach the one that took over.
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Daemon {
    shells: Mutex<HashMap<String, Arc<Shell>>>,
    socket: PathBuf,
}

struct Shell {
    pid: u32,
    master: File,
    screen: Mutex<Screen>,
}

/// What the pump and the connections share, under one lock.
struct Screen {
    parser: screen::Parser,
    client: Option<(u64, UnixStream)>,
    exit: Option<i32>,
}

/// Hold shells until the last one has ended. Returns at once if another holder is running.
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
    });
    for conn in listener.incoming().flatten() {
        let daemon = daemon.clone();
        thread::spawn(move || daemon.serve(conn));
    }
    Ok(())
}

impl Daemon {
    /// One connection. Its first frame says what it is for.
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

    /// A terminal attached: find its shell or start one, then pass its keystrokes and sizes on
    /// until it goes away.
    fn attach(self: &Arc<Self>, hello: &Hello, mut conn: UnixStream) {
        let shell = match self.open(hello) {
            Ok(shell) => shell,
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

    /// The shell `hello` names, started first if there is none by that id. Under the lock, so two
    /// terminals attaching to a new id at once share one shell.
    fn open(self: &Arc<Self>, hello: &Hello) -> io::Result<Arc<Shell>> {
        let mut shells = lock(&self.shells);
        if let Some(shell) = shells.get(&hello.id) {
            return Ok(shell.clone());
        }
        let (master, slave) = open_pty()?;
        set_winsize(&master, hello.rows, hello.cols);
        let child = spawn(hello, slave)?;
        let shell = Arc::new(Shell {
            pid: child.id(),
            master,
            screen: Mutex::new(Screen {
                parser: screen::parser(hello.rows, hello.cols),
                client: None,
                exit: None,
            }),
        });
        shells.insert(hello.id.clone(), shell.clone());
        let (daemon, id, pumped) = (self.clone(), hello.id.clone(), shell.clone());
        thread::spawn(move || daemon.pump(&id, &pumped, child));
        Ok(shell)
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

    /// A shell ended: forget it, tell its terminal, and leave if it was the last.
    fn gone(&self, id: &str, shell: &Shell, code: i32) {
        lock(&self.shells).remove(id);
        shell.ended(code);
        self.leave_if_idle();
    }

    /// Exit, taking the socket along, when no shell is held. The check and the exit happen under
    /// the lock `open` takes, so no shell can be started in between.
    fn leave_if_idle(&self) {
        let shells = lock(&self.shells);
        if shells.is_empty() {
            let _ = std::fs::remove_file(&self.socket);
            std::process::exit(0);
        }
    }

    /// End shell `id` and what runs in its foreground: a hangup, as a closing terminal gives, and
    /// after [`GRACE`] a kill for whatever ignored it. Both go to process groups, the shell's and
    /// the foreground job's, which reaches every process of a pipeline. The shell hangs up its
    /// other jobs itself. Nothing to do for an unknown id.
    fn kill(&self, id: &str) {
        let Some(shell) = lock(&self.shells).get(id).cloned() else {
            return;
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

    /// One JSON object per line and shell: its id, pid, working directory, title, and whether a
    /// terminal is attached.
    fn list(&self) -> Vec<u8> {
        // Copied out first, so the `screen` locks below are never taken under `shells`.
        let shells: Vec<_> = lock(&self.shells)
            .iter()
            .map(|(id, shell)| (id.clone(), shell.clone()))
            .collect();
        let mut out = Vec::new();
        for (id, shell) in shells {
            let cwd = std::fs::read_link(format!("/proc/{}/cwd", shell.pid)).ok();
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

    /// Terminal `me` went away. The shell stays, unless another terminal already has it.
    fn detach(&self, me: u64) {
        let mut term = lock(&self.screen);
        if term.client.as_ref().is_some_and(|(id, _)| *id == me) {
            term.client = None;
        }
    }

    /// The shell ended with `code`. A terminal attaching from now on is told so instead.
    fn ended(&self, code: i32) {
        let mut term = lock(&self.screen);
        term.exit = Some(code);
        if let Some((_, mut client)) = term.client.take() {
            let _ = protocol::write_frame(&mut client, EXIT, &code.to_be_bytes());
            let _ = client.shutdown(Shutdown::Both);
        }
    }

    /// Ask the foreground program to redraw itself, for a full-screen one whose scroll margins and
    /// character sets the replay cannot bring back.
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

/// A new pty pair. Both ends are opened `O_NOCTTY`, so the holder never gets a controlling
/// terminal (and with it a hangup), and close-on-exec, which std does and `openpty` would not: a
/// master that leaked into another shell would keep its pty open after the holder closed it, and
/// the shell on it would never get its hangup.
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
    let mut name: [libc::c_char; 64] = [0; 64];
    // SAFETY: `fd` is an open pty master, and `name` is a buffer of the length given.
    unsafe {
        if libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 {
            return Err(io::Error::last_os_error());
        }
        // The error comes back as the return value, not through errno (musl sets none).
        match libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) {
            0 => {}
            e => return Err(io::Error::from_raw_os_error(e)),
        }
    }
    // SAFETY: on success `ptsname_r` left a NUL-terminated path in `name`.
    let slave = unsafe { CStr::from_ptr(name.as_ptr()) };
    let slave = open(Path::new(OsStr::from_bytes(slave.to_bytes())))?;
    Ok((master, slave))
}

/// Tell the pty its size. The kernel sends the foreground job SIGWINCH if it changed.
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
                || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // `cmd` drops with the holder's copies of the slave: from here only the shell holds it open.
    cmd.spawn()
}

/// The shell to start for `hello`.
fn program(hello: &Hello) -> &Path {
    var(hello, "SHELL").map_or(Path::new("/bin/sh"), Path::new)
}

/// A variable of the terminal's environment, if set and not empty.
fn var<'a>(hello: &'a Hello, name: &str) -> Option<&'a OsStr> {
    hello
        .env
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_os_str())
        .filter(|value| !value.is_empty())
}

/// A status as a shell reports it: the exit code, or 128 plus the signal that ended the process.
fn status(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}
