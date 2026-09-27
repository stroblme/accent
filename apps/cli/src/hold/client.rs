//! The other end of the holder's socket: `attach`, the terminal end of a held shell, and the
//! one-shot `kill` and `held`.
//!
//! `attach` runs in the terminal's own pty (a VTE tab, or `ssh -t` on a host) and only relays:
//! keystrokes to the holder on one thread, SIGWINCH as a new size on another, and the shell's
//! output back to the terminal on the main one. It keeps no state worth losing, so when the
//! terminal goes away it just exits, and the shell stays with the holder.

use super::protocol::{self, DETACHED, EXIT, HELLO, Hello, INPUT, KILL, LIST, OUTPUT, RESIZE};
use super::{FAILED, SOCKET, clip, lock};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Relay this terminal to held shell `id`, starting the holder, and the shell in `cwd` (default:
/// here), if need be. Returns the exit status for `attach`: the shell's when it ended, 0 when the
/// terminal was taken over by another, [`FAILED`] when there was nothing to attach to.
pub fn attach(id: &str, cwd: Option<PathBuf>) -> i32 {
    // Absolute: the holder runs in `/`.
    let cwd = std::path::absolute(cwd.unwrap_or_else(|| ".".into())).unwrap_or_default();
    let winch = block_winch();
    let saved = raw();
    let code = relay(id, &cwd, winch);
    if let Some(saved) = saved {
        // SAFETY: `saved` is the mode `tcgetattr` gave for this descriptor.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved) };
    }
    code
}

/// End shell `id`. A holder is started if none is running, since the `attach` that is to start
/// the shell may still be on its way (Close Tab in the moment after New Terminal), and the kill
/// has to be there before it: the holder then starts nothing for it.
pub fn kill(id: &str) -> io::Result<()> {
    protocol::write_frame(&mut connect()?, KILL, id.as_bytes())
}

/// Print the held shells, one JSON object per line: nothing when no holder is running, and none
/// is started to find out.
pub fn held() -> io::Result<()> {
    let Some(mut conn) = holder()? else {
        return Ok(());
    };
    protocol::write_frame(&mut conn, LIST, &[])?;
    if let Some((OUTPUT, lines)) = protocol::read_frame(&mut conn)? {
        io::stdout().write_all(&lines)?;
    }
    Ok(())
}

/// The running holder, if there is one.
fn holder() -> io::Result<Option<UnixStream>> {
    Ok(UnixStream::connect(super::dir()?.join(SOCKET)).ok())
}

/// Attach and pass output on until the shell ends or another terminal takes it. A connection
/// that ends without either is attached again: the holder may have exited as this one connected,
/// dropped this terminal for leaving output unread, or crashed (then a fresh shell starts in the
/// same place). Three such rounds without a byte of output, and it gives up.
fn relay(id: &str, cwd: &Path, winch: libc::sigset_t) -> i32 {
    let to = Arc::new(Mutex::new(None));
    let mut out = io::stdout().lock();
    let mut copies = clip::Scanner::default();
    let (mut forwarding, mut shown, mut silent) = (false, false, 0);
    while silent < 3 {
        let mut conn = match connect() {
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
            rows,
            cols,
            env: std::env::vars_os().collect(),
        };
        if protocol::write_frame(&mut conn, HELLO, &hello.encode()).is_err() {
            silent += 1;
            continue;
        }
        *lock(&to) = conn.try_clone().ok();
        // Only once attached, so that what is typed meanwhile waits in the terminal.
        if !forwarding {
            forward(&to, winch);
            forwarding = true;
        }
        let mut heard = false;
        loop {
            match protocol::read_frame(&mut conn) {
                Ok(Some((OUTPUT, bytes))) => {
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

/// The two threads that talk to whichever connection `to` holds: keystrokes as they come, and
/// the terminal's size each time SIGWINCH arrives. The end of stdin is the terminal going away,
/// which detaches: the process exits and the holder sees the connection close.
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
    let size_to = to.clone();
    thread::spawn(move || {
        let mut signal = 0;
        // SAFETY: `winch` is an initialised set, and `signal` a place for the answer.
        while unsafe { libc::sigwait(&winch, &mut signal) } == 0 {
            let (rows, cols) = winsize();
            if let Some(conn) = lock(&size_to).as_mut() {
                let _ = protocol::write_frame(conn, RESIZE, &protocol::size(rows, cols));
            }
        }
    });
}

/// The holder's socket, starting a holder when none answers. Up to 2 s, starting one again every
/// fifth try: a holder started while the last one was still exiting finds the lock taken and
/// leaves at once.
// ponytail: polling puts the first shell after a holder start at about 60 ms (measured), 50 of
// them this sleep. A shorter step, or a pipe the holder closes once bound, if that ever shows.
fn connect() -> io::Result<UnixStream> {
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

/// The size of the terminal on stdin; 24×80 off a terminal, or for one that has no size yet.
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
