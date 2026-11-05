//! The holder, `accent-cli`: what it says about a held shell, the copies its programs make,
//! and ending the shells no tab or session needs any more.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use accent_api::ssh;
use gtk::glib;
use gtk::prelude::*;

use super::Term;
use super::key::{KEY, id, is_key};

/// Shell facts for [`Term::facts`], reported by `accent-cli held`.
/// Unknown or not-yet-returned program, PID, directory, foreground command and start fields are `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub program: Option<String>,
    pub pid: Option<u64>,
    /// The machine it runs on: this one's name, or the host's as its address gives it.
    pub host: String,
    pub dir: Option<String>,
    /// The command line of what runs in the foreground; `None` at the prompt.
    pub running: Option<String>,
    /// When it started, in seconds since the epoch.
    pub started: Option<i64>,
    /// The title the shell last set (OSC 0 or 2), which the attach's replay hands VTE.
    pub title: Option<String>,
    /// The vault token granted MCP access, if this shell was explicitly shared.
    pub shared_with: Option<String>,
}

/// The termprop `accent-cli attach` raises when a program in the shell has copied something with
/// OSC 52, which VTE does not do itself (`hold::clip` in `accent-cli`).
const CLIPBOARD: &str = "vte.ext.accent.clipboard";

/// Make [`CLIPBOARD`] known to VTE, which takes a termprop only before the first terminal exists.
#[cfg(feature = "termprops")]
pub fn install_termprops() {
    let name = std::ffi::CString::new(CLIPBOARD).unwrap_or_default();
    // SAFETY: a NUL-terminated name under VTE's `vte.ext.` prefix, a type and no flags, from
    // `main` before any window, so before any `VteTerminal`, as VTE requires.
    unsafe {
        vte4::ffi::vte_install_termprop(
            name.as_ptr(),
            vte4::ffi::VTE_PROPERTY_VALUELESS,
            vte4::ffi::VTE_PROPERTY_FLAG_NONE,
        );
    }
}

/// Fetch OSC 52 clipboard content through `accent-cli clip` off-thread, locally or remotely.
/// [`CLIPBOARD`] signals availability; weak ownership follows [`on_exit`](super::on_exit).
/// Without attach there is no signal.
pub(super) fn take_copies(term: &Rc<Term>) {
    // Termprops are VTE 0.78's: before it the signal does not exist, and connecting to it panics.
    if !cfg!(feature = "termprops") {
        return;
    }
    let weak = Rc::downgrade(term);
    let signal = format!("termprop-changed::{CLIPBOARD}");
    term.view.connect_local(&signal, false, move |_| {
        let term = weak.upgrade()?;
        let argv = term.shell.clip(id(&term.key))?;
        let clipboard = term.view.clipboard();
        glib::spawn_future_local(async move {
            let copy =
                crate::work::attempt("take the terminal's copy", move || output(&argv)).await;
            match copy {
                Ok(base64) => {
                    let text = glib::base64_decode(&String::from_utf8_lossy(&base64));
                    clipboard.set_text(&String::from_utf8_lossy(&text));
                }
                Err(why) => tracing::warn!("{why}"),
            }
        });
        None
    });
}

/// Run `argv` and hand back what it printed, or what it said on stderr when it failed.
pub(super) fn output(argv: &[String]) -> std::io::Result<Vec<u8>> {
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .output()?;
    match out.status.success() {
        true => Ok(out.stdout),
        false => Err(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )),
    }
}

/// What `accent-cli held` says about the shell `id`: nothing when it does not hold it.
pub(super) fn told(listing: &str, id: &str) -> Facts {
    let Some(shell) = listing
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|shell| shell["id"] == id)
    else {
        return Facts::default();
    };
    let text = |name: &str| shell[name].as_str().map(str::to_string);
    Facts {
        program: text("shell"),
        pid: shell["pid"].as_u64(),
        dir: text("cwd"),
        running: text("running"),
        started: shell["started"].as_i64(),
        title: text("title").filter(|title| !title.is_empty()),
        shared_with: text("shared_with"),
        ..Facts::default()
    }
}

/// Keep a verified cwd through a failed holder query, without retaining stale command details.
pub(super) fn facts_after_query(previous: &Facts, listing: Option<&[u8]>, id: &str) -> Facts {
    let mut facts = listing
        .map(|listing| told(&String::from_utf8_lossy(listing), id))
        .unwrap_or_default();
    if facts.dir.is_none() {
        facts.dir.clone_from(&previous.dir);
    }
    if listing.is_none() {
        facts.shared_with.clone_from(&previous.shared_with);
    }
    facts
}

/// The command that runs the holder's `accent-cli`: the one beside this executable in a build or
/// an installation, or in the Flatpak the one [`host_cli`] runs on the host. The static remote
/// upload binary is located separately by `ssh::server_binary`.
pub fn cli() -> Option<Vec<String>> {
    if let Ok(info) = std::fs::read_to_string("/.flatpak-info") {
        return host_cli(&info);
    }
    let cli = std::env::current_exe().ok()?.with_file_name("accent-cli");
    cli.is_file()
        .then(|| vec![cli.to_string_lossy().into_owned()])
}

/// Launch the Flatpak's static `accent-cli` on the host so shells use the user's tools. The
/// `app-path` in `/.flatpak-info` locates it. If Flatpak bus access is unavailable, the caller
/// falls back to an unheld sandbox shell.
fn host_cli(info: &str) -> Option<Vec<String>> {
    let file = glib::KeyFile::new();
    file.load_from_data(info, glib::KeyFileFlags::NONE).ok()?;
    let app = file.string("Instance", "app-path").ok()?;
    let mut argv = host_prefix(info)?;
    argv.push(format!(
        "{app}/lib/accent/accent-cli-{}",
        std::env::consts::ARCH
    ));
    Some(argv)
}

/// What runs a command on the host from the Flatpak, where its shells are, with VTE's
/// environment values; `None` once host access is revoked.
pub fn host_prefix(info: &str) -> Option<Vec<String>> {
    let file = glib::KeyFile::new();
    file.load_from_data(info, glib::KeyFileFlags::NONE).ok()?;
    let policy = file
        .string("Session Bus Policy", "org.freedesktop.Flatpak")
        .ok()?;
    if !matches!(policy.as_str(), "talk" | "own") {
        return None;
    }
    // SAFETY: plain getters of constants compiled into libvte.
    let vte = unsafe {
        vte_sys::vte_get_major_version() * 10000
            + vte_sys::vte_get_minor_version() * 100
            + vte_sys::vte_get_micro_version()
    };
    Some(vec![
        "flatpak-spawn".to_string(),
        "--host".to_string(),
        "--env=TERM=xterm-256color".to_string(),
        "--env=COLORTERM=truecolor".to_string(),
        format!("--env=VTE_VERSION={vte}"),
    ])
}

/// End shell `key` held at `at`, locally or through the host's master.
/// Without `accent-cli`, the local shell already ended with its tab.
pub fn end(key: &str, at: &str) {
    if ssh::is_remote(at) {
        let url = ssh::parse(at).map_err(|e| e.to_string());
        match url {
            Ok(url) => kill_remote(Orphan {
                ctl: ssh::control_path(&accent_api::link::host(&url)),
                at: url,
                id: id(key).to_string(),
            }),
            Err(e) => tracing::warn!("cannot end the shell at {at}: {e}"),
        }
        return;
    }
    if let Some(mut argv) = cli() {
        argv.extend(["kill".to_string(), id(key).to_string()]);
        run_kill(argv, None);
    }
}

/// End every shell the session stored under `key` holds, here or on a host: a session removed
/// while no window has it open, whose shells nothing would attach to again.
pub fn end_stored(key: &Path) {
    for (id, place) in accent_core::config::Session::load(key).terminals {
        end(&id, &place.at);
    }
}

/// End local held shells that no session names; leave remote hosts untouched.
///
/// Run once before the first window so current shells cannot be mistaken for orphans between
/// holder startup and tab attachment. Read sessions only when the holder lists shells.
pub fn sweep() {
    let Some(mut argv) = cli() else {
        return;
    };
    argv.push("held".to_string());
    let held = match output(&argv) {
        Ok(out) if !out.is_empty() => out,
        _ => return,
    };
    // A session that cannot be read may be the one that names them.
    let Some(sessions) = accent_core::config::Session::stored() else {
        return tracing::warn!("a session cannot be read, so no held shell is ended");
    };
    let named: HashSet<String> = sessions
        .into_iter()
        .flat_map(|s| s.open.into_iter().chain(s.terminals.into_keys()))
        .collect();
    for key in orphans(&String::from_utf8_lossy(&held), &named, &owner()) {
        tracing::info!("ending the held shell {key}, which no session names");
        end(&key, "");
    }
}

/// Select orphaned shells from `held` for [`sweep`]: unattached Accent IDs started by `owner`
/// and absent from `named` sessions. Leave other instances and manual attaches alone.
fn orphans(held: &str, named: &HashSet<String>, owner: &str) -> Vec<String> {
    held.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|shell| shell["attached"] == false && shell["owner"] == owner)
        .filter_map(|shell| Some(format!("{KEY}{}", shell["id"].as_str()?)))
        .filter(|key| is_key(key) && !named.contains(key))
        .collect()
}

/// Who this app's shells are for, as the holder keeps it: the state directory its sessions live
/// in. A native app's and the Flatpak's differ, as a drill's scratch one does, so neither sweeps
/// shells the other's sessions name; they may share one holder, the Flatpak's being the host's.
pub(super) fn owner() -> String {
    accent_core::config::xdg("XDG_STATE_HOME", ".local/state")
        .to_string_lossy()
        .into_owned()
}

/// Run a shell kill off the main thread so Close Tab never waits on a holder or host.
/// An empty argument vector means no holder owns the shell.
///
/// A remote kill names its `orphan`, kept for [`end_orphans`] when it finds the link down so it
/// cannot leave an unreachable shell running.
fn run_kill(argv: Vec<String>, orphan: Option<Orphan>) {
    let Some((program, args)) = argv.split_first() else {
        return;
    };
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(std::process::Stdio::null());
    let _ = std::thread::Builder::new()
        .name("accent-kill".to_string())
        .spawn(move || match (command.status(), orphan) {
            // ssh's own failure rather than the command's: the host never heard.
            (Ok(status), Some(orphan)) if status.code() == Some(255) => {
                tracing::debug!("ending a shell once {} is back", orphan.ctl.display());
                ORPHANED
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(orphan);
            }
            (Ok(_), _) => {}
            (Err(e), _) => tracing::warn!("cannot end the shell: {e}"),
        });
}

/// A remote shell to end: held as `id` on the host `at` names, reached over the master `ctl`.
pub(super) struct Orphan {
    pub(super) at: ssh::Url,
    pub(super) ctl: PathBuf,
    pub(super) id: String,
}

/// End `orphan` with the server last made ready on its host ([`accent_api::link::prepared`]), its
/// command made now rather than when the tab opened: a reconnect since may have put another
/// build there and swept the one before.
pub(super) fn kill_remote(orphan: Orphan) {
    match accent_api::link::prepared(&orphan.at) {
        Ok(hash) => {
            let command = ssh::kill_cmd(&ssh::server_path(&hash), &orphan.id);
            run_kill(ssh::run(&orphan.at, &orphan.ctl, &command), Some(orphan));
        }
        Err(e) => tracing::warn!("cannot end the shell on {}: {e}", orphan.at.host),
    }
}

/// The ends of remote shells that found the link down.
static ORPHANED: Mutex<Vec<Orphan>> = Mutex::new(Vec::new());

/// End the shells closed while the link through `ctl` was down, now that it is up again.
pub fn end_orphans(ctl: &Path) {
    let due: Vec<Orphan> = {
        let mut orphaned = ORPHANED.lock().unwrap_or_else(|e| e.into_inner());
        let (due, kept) = std::mem::take(&mut *orphaned)
            .into_iter()
            .partition(|orphan| orphan.ctl == ctl);
        *orphaned = kept;
        due
    };
    for orphan in due {
        kill_remote(orphan);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In the Flatpak a shell is the host's, through the static accent-cli the Flatpak ships, as
    /// long as the app may still run host commands.
    #[test]
    fn a_flatpak_holds_its_shells_on_the_host() {
        let info = "[Application]\nname=io.github.stroblme.Accent\n\n\
            [Instance]\napp-path=/var/lib/flatpak/app/io.github.stroblme.Accent/current/active/files\n\n\
            [Session Bus Policy]\norg.freedesktop.Flatpak=talk\n";
        let cli = host_cli(info).unwrap();
        assert_eq!(cli[..2], ["flatpak-spawn", "--host"]);
        assert!(
            cli.contains(&"--env=TERM=xterm-256color".to_string()),
            "{cli:?}"
        );
        assert_eq!(
            cli.last().unwrap(),
            &format!(
                "/var/lib/flatpak/app/io.github.stroblme.Accent/current/active/files/lib/accent/accent-cli-{}",
                std::env::consts::ARCH
            )
        );
        // Revoked (Flatseal, `flatpak override`): the sandbox's own shell, unheld.
        assert_eq!(host_cli(&info.replace("=talk", "=none")), None);
    }

    /// What the startup sweep ends: accent's own shells, that no terminal has, that this state
    /// directory started, and that no session names.
    #[test]
    fn a_held_shell_no_session_names_is_an_orphan() {
        let held = [
            r#"{"attached":false,"id":"00000000000000a1","owner":"/home/me/.local/state"}"#,
            // Named by a session, so it comes back when that session is opened.
            r#"{"attached":false,"id":"00000000000000a2","owner":"/home/me/.local/state"}"#,
            // In a terminal right now: another instance of accent on the same holder.
            r#"{"attached":true,"id":"00000000000000a3","owner":"/home/me/.local/state"}"#,
            // Not an id accent chose: an `accent-cli attach` typed by hand.
            r#"{"attached":false,"id":"demo","owner":"/home/me/.local/state"}"#,
            // Another state directory's (the Flatpak's, a drill's), whose sessions are not read here.
            r#"{"attached":false,"id":"00000000000000a5","owner":"/home/me/.var/app/x/.local/state"}"#,
            // Started by no app: a remote attach, or one typed by hand.
            r#"{"attached":false,"id":"00000000000000a6","owner":""}"#,
        ]
        .join("\n");
        let named = HashSet::from(["terminal:00000000000000a2".to_string()]);
        assert_eq!(
            orphans(&held, &named, "/home/me/.local/state"),
            ["terminal:00000000000000a1"]
        );
    }

    /// The holder lists every shell it keeps; the tab reads its own line, and a shell it does not
    /// hold, or a field it does not say, is left unknown.
    #[test]
    fn a_shell_reads_its_own_line_of_the_listing() {
        let listing = [
            r#"{"attached":true,"cwd":"/srv","id":"00000000000000b1","pid":7,"title":""}"#,
            r#"{"attached":true,"cwd":"/home/me/src","id":"00000000000000b2","pid":42,"title":"cargo build","shell":"/bin/zsh","started":1760000000,"running":"cargo build","shared_with":"mcp:vault"}"#,
        ]
        .join("\n");
        assert_eq!(
            told(&listing, "00000000000000b2"),
            Facts {
                program: Some("/bin/zsh".into()),
                pid: Some(42),
                dir: Some("/home/me/src".into()),
                running: Some("cargo build".into()),
                started: Some(1_760_000_000),
                title: Some("cargo build".into()),
                shared_with: Some("mcp:vault".into()),
                ..Facts::default()
            }
        );
        assert_eq!(told(&listing, "00000000000000b1").running, None);
        // A shell that set no title has none to name its tab with.
        assert_eq!(told(&listing, "00000000000000b1").title, None);
        assert_eq!(told(&listing, "00000000000000b1").shared_with, None);
        assert_eq!(told(&listing, "00000000000000ff"), Facts::default());
    }

    #[test]
    fn a_failed_holder_query_keeps_the_last_verified_directory() {
        let prior = Facts {
            dir: Some("/srv/changed-repo".into()),
            running: Some("git status".into()),
            shared_with: Some("mcp:vault".into()),
            ..Facts::default()
        };
        let after = facts_after_query(&prior, None, "shell-id");
        assert_eq!(after.dir.as_deref(), Some("/srv/changed-repo"));
        assert_eq!(after.running, None);
        assert_eq!(after.shared_with.as_deref(), Some("mcp:vault"));
        assert_eq!(
            facts_after_query(&prior, Some(b""), "shell-id").dir,
            prior.dir
        );
        assert_eq!(
            facts_after_query(&prior, Some(b""), "shell-id").shared_with,
            None
        );
        assert_eq!(
            facts_after_query(&Facts::default(), None, "shell-id").dir,
            None
        );
    }
}
