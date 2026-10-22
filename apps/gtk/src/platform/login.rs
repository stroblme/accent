//! An app started from the Dock or Finder has launchd's `PATH=/usr/bin:/bin:/usr/sbin:/sbin`,
//! where no language server the user installed is found (`accent_api::language::in_path`). As
//! VS Code and Zed do, the app takes the login shell's PATH once at start instead.

use std::ffi::OsString;
use std::io::{IsTerminal, Read};
use std::os::unix::ffi::OsStringExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long the login shell may take; past it the app keeps launchd's PATH.
const TIMEOUT: Duration = Duration::from_secs(2);

/// Set PATH to the login shell's, unless started from a terminal, whose PATH is the user's already.
pub fn take_path() {
    if std::io::stdin().is_terminal() {
        return;
    }
    if let Some(path) = login_path() {
        // SAFETY: called first thing in `main`; the one other thread, reading the shell's output,
        // only waits on its pipe if it is still running.
        unsafe { std::env::set_var("PATH", path) };
    }
}

/// The login shell's PATH, or `None` without a `$SHELL`, when it fails, or when it is too slow.
fn login_path() -> Option<OsString> {
    // The NUL first parts the environment from whatever the startup files print.
    let mut child = Command::new(std::env::var_os("SHELL")?)
        .args(["-l", "-c", r"printf '\0'; exec /usr/bin/env -0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (send, env) = mpsc::channel();
    // A thread, as something the startup files left running may hold the pipe open.
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        let _ = send.send(bytes);
    });
    let env = env.recv_timeout(TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
    path_in(&env.ok()?)
}

/// The `PATH` entry of `env -0`'s output.
fn path_in(env: &[u8]) -> Option<OsString> {
    env.split(|&byte| byte == 0)
        .find_map(|entry| entry.strip_prefix(b"PATH="))
        .filter(|path| !path.is_empty())
        .map(|path| OsString::from_vec(path.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_path_is_found_past_what_startup_files_print() {
        let out =
            b"Last login: today\n\0HOME=/Users/me\0PATH=/opt/local/bin:/usr/bin\0SHELL=/bin/zsh\0";
        assert_eq!(path_in(out), Some("/opt/local/bin:/usr/bin".into()));
        assert_eq!(path_in(b"HOME=/Users/me\0MANPATH=/x\0"), None);
    }
}
