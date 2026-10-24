//! Relay terminal I/O to held shells, with one-shot kill and list commands.
//!
//! `attach` forwards input, SIGWINCH sizes and output on separate threads. It keeps no
//! persistent state: terminal closure ends the relay while the holder retains the shell.

use super::protocol::{
    self, ACK, ATTACH_EXISTING, DETACHED, ERROR, EXIT, HELLO, Hello, INPUT, KILL, OUTPUT, RESIZE,
    REVOKE, SHARE,
};
use super::{FAILED, SOCKET, clip, discovery, lock};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Attach this terminal to shell `id`, starting the holder and shell in `cwd` for `owner` if needed.
/// Returns the shell's exit status, 0 on takeover, or [`FAILED`] if attachment fails.
pub fn attach(id: &str, cwd: Option<PathBuf>, owner: &str) -> i32 {
    // Absolute: the holder runs in `/`.
    let cwd = std::path::absolute(cwd.unwrap_or_else(|| ".".into())).unwrap_or_default();
    let winch = block_winch();
    let saved = raw();
    let code = relay(id, &cwd, owner, winch, false);
    if let Some(saved) = saved {
        // SAFETY: `saved` is the mode `tcgetattr` gave for this descriptor.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved) };
    }
    code
}

/// Attach only to a live terminal owned by or shared with this vault; never create a shell.
pub fn attach_existing(id: &str, vault: &Path) -> i32 {
    let cwd = match vault.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("accent-cli: cannot locate vault: {e}");
            return FAILED;
        }
    };
    let owner = match super::mcp_owner(&cwd) {
        Ok(owner) => owner,
        Err(e) => {
            eprintln!("accent-cli: cannot identify vault: {e}");
            return FAILED;
        }
    };
    let winch = block_winch();
    let saved = raw();
    let code = relay(id, &cwd, &owner, winch, true);
    if let Some(saved) = saved {
        // SAFETY: `saved` is the mode `tcgetattr` gave for this descriptor.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved) };
    }
    code
}

/// End shell `id`, starting a holder if needed to prevent a pending attach from creating it.
pub fn kill(id: &str) -> io::Result<()> {
    let mut conn = match discovery::find(id)? {
        Some(socket) => UnixStream::connect(socket)?,
        None => connect()?,
    };
    protocol::write_frame(&mut conn, KILL, id.as_bytes())
}

/// Print one JSON object per held shell. Do not start a holder to list them.
pub fn held() -> io::Result<()> {
    let holders = discovery::running()?;
    let mut out = io::stdout().lock();
    for holder in holders {
        for shell in holder.shells {
            writeln!(out, "{shell}")?;
        }
    }
    Ok(())
}

/// Share a live terminal with the MCP server for one vault.
pub fn share(id: &str, vault: &Path) -> io::Result<()> {
    let owner = super::mcp_owner(vault)?;
    let payload = serde_json::json!({"id": id, "owner": owner}).to_string();
    one_shot(id, SHARE, payload.as_bytes())
}

/// Immediately revoke a terminal's MCP share.
pub fn revoke(id: &str) -> io::Result<()> {
    one_shot(id, REVOKE, id.as_bytes())
}

fn one_shot(id: &str, tag: u8, payload: &[u8]) -> io::Result<()> {
    let socket = discovery::find(id)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "terminal not found"))?;
    if socket.file_name().is_none_or(|name| name != SOCKET) {
        return Err(io::Error::other(
            "this terminal's older holder does not support MCP sharing",
        ));
    }
    let mut conn = UnixStream::connect(socket)?;
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    conn.set_write_timeout(Some(Duration::from_secs(5)))?;
    protocol::write_frame(&mut conn, tag, payload)?;
    match protocol::read_frame(&mut conn)? {
        Some((ACK, _)) => Ok(()),
        Some((ERROR, reason)) => Err(io::Error::other(
            String::from_utf8_lossy(&reason).into_owned(),
        )),
        _ => Err(io::Error::other("terminal holder gave no answer")),
    }
}

/// Connect to the holder if one is running.
pub(super) fn holder() -> io::Result<Option<UnixStream>> {
    discovery::open(&super::dir()?.join(SOCKET))
}

/// Relay until shell exit or takeover. Reattach after an unexpected disconnect, restarting a
/// crashed current holder if needed; never move a legacy session to another holder.
fn relay(id: &str, cwd: &Path, owner: &str, winch: libc::sigset_t, existing: bool) -> i32 {
    let legacy = if existing {
        None
    } else {
        match discovery::find(id) {
            Ok(socket) => socket.filter(|p| p.file_name().is_some_and(|name| name != SOCKET)),
            Err(e) => {
                eprintln!("accent-cli: cannot locate terminal: {e}");
                return FAILED;
            }
        }
    };
    let to = Arc::new(Mutex::new(None));
    let mut out = io::stdout().lock();
    let mut copies = clip::Scanner::default();
    let (mut forwarding, mut shown, mut silent) = (false, false, 0);
    while silent < 3 {
        let mut conn = match if let Some(socket) = &legacy {
            UnixStream::connect(socket)
        } else if existing {
            holder().and_then(|conn| {
                conn.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no terminal holder"))
            })
        } else {
            connect()
        } {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("accent-cli: cannot start the terminal holder: {e}");
                return FAILED;
            }
        };
        let (rows, cols) = winsize();
        let hello = Hello {
            id: id.into(),
            cwd: cwd.into(),
            owner: owner.into(),
            rows,
            cols,
            env: std::env::vars_os().collect(),
        };
        let tag = if existing { ATTACH_EXISTING } else { HELLO };
        if protocol::write_frame(&mut conn, tag, &hello.encode()).is_err() {
            silent += 1;
            continue;
        }
        *lock(&to) = conn.try_clone().ok();
        // Only once attached, so that what is typed meanwhile waits in the terminal.
        if !forwarding && !existing {
            forward(&to, winch);
            forwarding = true;
        }
        let mut heard = false;
        loop {
            match protocol::read_frame(&mut conn) {
                Ok(Some((OUTPUT, bytes))) => {
                    if !forwarding {
                        forward(&to, winch);
                        forwarding = true;
                    }
                    // Attached again: the replay is the whole screen, so start from a blank one.
                    if shown && !heard {
                        let _ = out.write_all(b"\x1b[H\x1b[2J\x1b[3J");
                    }
                    heard = true;
                    // The terminal went away: that is a detach.
                    if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
                        return 0;
                    }
                    // A copy the terminal will not make itself: see `clip`.
                    if let Some(copy) = copies.feed(&bytes)
                        && clip::keep(id, &copy).is_ok()
                    {
                        let _ = out.write_all(clip::SIGNAL).and_then(|()| out.flush());
                    }
                }
                Ok(Some((EXIT, code))) => {
                    return code
                        .as_slice()
                        .try_into()
                        .map_or(FAILED, i32::from_be_bytes);
                }
                Ok(Some((DETACHED, _))) => return 0,
                Ok(Some((ERROR, reason))) => {
                    eprintln!("accent-cli: {}", String::from_utf8_lossy(&reason));
                    return FAILED;
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        shown |= heard;
        silent = if heard { 0 } else { silent + 1 };
    }
    eprintln!("accent-cli: the terminal holder keeps going away");
    FAILED
}

/// Forward keystrokes and SIGWINCH sizes to the current connection. EOF on stdin exits
/// the relay, which detaches the terminal.
fn forward(to: &Arc<Mutex<Option<UnixStream>>>, winch: libc::sigset_t) {
    let keys_to = to.clone();
    thread::spawn(move || {
        // Larger than stdin's own buffer, so reads go straight to the terminal.
        let mut keys = vec![0; 1 << 14];
        loop {
            match io::stdin().read(&mut keys) {
                Ok(0) | Err(_) => std::process::exit(0),
                Ok(n) => {
                    if let Some(conn) = lock(&keys_to).as_mut() {
                        let _ = protocol::write_frame(conn, INPUT, &keys[..n]);
                    }
                }
            }
        }
    });
    // SIGWINCH goes to the terminal's foreground process group, which is ours when a terminal or
    // ssh started us. Run through `flatpak-spawn --host` it is not: we are in a session of our
    // own on the host and hear of no resize, so there we look at the size instead.
    // SAFETY: both only read this process's and the descriptor's state.
    let told = unsafe { libc::tcgetpgrp(0) == libc::getpgrp() };
    let size_to = to.clone();
    thread::spawn(move || {
        let mut sent = winsize();
        loop {
            if told {
                let mut signal = 0;
                // SAFETY: `winch` is an initialised set, and `signal` a place for the answer.
                if unsafe { libc::sigwait(&winch, &mut signal) } != 0 {
                    return;
                }
            } else {
                // ponytail: ten looks a second per Flatpak tab; a resize is up to 100 ms late.
                thread::sleep(Duration::from_millis(100));
            }
            let size = winsize();
            if size != sent {
                sent = size;
                if let Some(conn) = lock(&size_to).as_mut() {
                    let _ = protocol::write_frame(conn, RESIZE, &protocol::size(size.0, size.1));
                }
            }
        }
    });
}

/// Connect to the holder's socket, starting a holder if needed. Retry for up to 2 seconds; start
/// another holder every fifth attempt in case the previous one found the lock held while exiting.
// ponytail: polling puts the first shell after a holder start at about 60 ms (measured), 50 of
// them this sleep. A shorter step, or a pipe the holder closes once bound, if that ever shows.
pub(super) fn connect() -> io::Result<UnixStream> {
    let socket = super::dir()?.join(SOCKET);
    let mut tries = 0;
    loop {
        match UnixStream::connect(&socket) {
            Ok(conn) => return Ok(conn),
            Err(e) if tries == 40 => return Err(e),
            Err(_) => {}
        }
        if tries % 5 == 0 {
            start()?;
        }
        tries += 1;
        thread::sleep(Duration::from_millis(50));
    }
}

/// Start a holder, detached from this terminal: a session of its own and no stdio is all it takes,
/// since it opens every pty `O_NOCTTY` and so never gets a controlling terminal again. On a host,
/// `current_exe` is the provisioned `accent-cli-<hash>`.
fn start() -> io::Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("hold")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and touches no memory of ours.
    unsafe {
        cmd.pre_exec(|| match libc::setsid() {
            -1 => Err(io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    let mut holder = cmd.spawn()?;
    // Reaped when it leaves, which a holder that lost the race for the lock does at once.
    thread::spawn(move || holder.wait());
    Ok(())
}

/// Block SIGWINCH here, and so in every thread started from now on, for the one that waits for it
/// with `sigwait`. Must run before the first thread exists: a thread that has it unblocked would
/// take the signal and, with no handler, drop it.
fn block_winch() -> libc::sigset_t {
    let mut set = MaybeUninit::uninit();
    // SAFETY: the set is initialised by `sigemptyset` before anything reads it.
    unsafe {
        libc::sigemptyset(set.as_mut_ptr());
        libc::sigaddset(set.as_mut_ptr(), libc::SIGWINCH);
        let set = set.assume_init();
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

/// Put the terminal in raw mode, as ssh does: every key goes to the shell as typed (Ctrl+C is the
/// byte 3, not a signal to us), and output passes unchanged (no CR added before each LF). The
/// mode to restore, or `None` when stdin is not a terminal.
fn raw() -> Option<libc::termios> {
    let mut mode = MaybeUninit::uninit();
    // SAFETY: `tcgetattr` fills `mode` when it returns 0, and only then is it read.
    unsafe {
        if libc::tcgetattr(0, mode.as_mut_ptr()) != 0 {
            return None;
        }
        let saved = mode.assume_init();
        let mut raw = saved;
        libc::cfmakeraw(&mut raw);
        libc::tcsetattr(0, libc::TCSANOW, &raw);
        Some(saved)
    }
}

/// Return the terminal size on stdin, or 24×80 if unavailable.
fn winsize() -> (u16, u16) {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ fills one `winsize`. The constant goes in as libc declares it (see
    // `daemon::set_winsize`).
    let known = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) } == 0;
    match (size.ws_row, size.ws_col) {
        (rows, cols) if known && rows > 0 && cols > 0 => (rows, cols),
        _ => (24, 80),
    }
}
