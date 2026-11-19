//! The Flatpak's sandbox, and the host commands a local vault's kernels and language servers run
//! as from inside it: the sandbox ships none of the user's tools.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// The Flatpak's app id where this runs inside its sandbox.
pub(crate) fn sandbox() -> Option<String> {
    let id = std::env::var("FLATPAK_ID").ok();
    id.filter(|_| Path::new("/.flatpak-info").exists())
}

/// `argv` run on the host from the sandbox in `cwd`, with the user's own `PATH` ([`login_path`])
/// and `env` beside the host session's environment, and ended once its `flatpak-spawn` is gone
/// (`--watch-bus`). Blocks the first time, while the host is asked for that `PATH`.
pub(crate) fn on_host(argv: Vec<String>, cwd: &Path, env: &[(String, String)]) -> Vec<String> {
    command_line(argv, cwd, login_path(), env)
}

/// [`on_host`]'s command line, `path` the `PATH` it is given in place of the host session's. With
/// one, `env` starts `argv`, so its program is looked for there: flatpak-session-helper looks on
/// its own.
fn command_line(
    argv: Vec<String>,
    cwd: &Path,
    path: Option<&str>,
    env: &[(String, String)],
) -> Vec<String> {
    let host = ["flatpak-spawn", "--host", "--watch-bus"].map(String::from);
    let directory = format!("--directory={}", cwd.display());
    let path = path.map(|path| format!("--env=PATH={path}"));
    let lookup = path.is_some().then(|| "env".to_string());
    let env = env.iter().map(|(k, v)| format!("--env={k}={v}"));
    host.into_iter()
        .chain([directory])
        .chain(path)
        .chain(env)
        .chain(lookup)
        .chain(argv)
        .collect()
}

/// What the host user's `PATH` is marked with in their shell's answer, which a profile may print
/// anything before.
const MARK: &str = "accent-path=";

/// The host user's `PATH` as a terminal tab's login shell sets it (`-l -i`, the shell the user
/// database names), so `~/.cargo/bin` and an nvm's `bin` count, which the session's environment
/// often lacks. Asked once, within 5 s, and kept for the process; `None` outside the Flatpak and
/// where the shell gave no such answer, the session's `PATH` standing then. Blocks the first time.
pub(crate) fn login_path() -> Option<&'static str> {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    PATH.get_or_init(|| {
        sandbox()?;
        let shell = r#"shell=$(getent passwd "$(id -u)" | cut -d: -f7)
exec "${shell:-${SHELL:-/bin/sh}}" -l -i -c "echo; echo \"$1\$PATH\"""#;
        let ask = ["sh", "-c", shell, "sh", MARK].map(String::from).to_vec();
        let argv = command_line(ask, Path::new("/"), None, &[]);
        let mut command = std::process::Command::new(&argv[0]);
        command.args(&argv[1..]);
        let answer = crate::python::run_within(command, Duration::from_secs(5), true);
        let path = answer.and_then(|(_, out)| path_of(&String::from_utf8_lossy(&out)));
        tracing::debug!("the host's login PATH: {path:?}");
        path
    })
    .as_deref()
}

/// The `PATH` after the last [`MARK`] in a shell's answer: one line naming a folder by its full
/// path at least, else `None`.
fn path_of(answer: &str) -> Option<String> {
    let path = answer.rsplit_once(MARK)?.1.trim_end_matches('\n');
    let whole = !path.contains('\n') && path.split(':').any(|dir| dir.starts_with('/'));
    whole.then(|| path.to_string())
}

/// Whether `program` starts on the host, `sandboxed` being whether this runs in the Flatpak: all
/// but the Flatpak's own, under `/app`, which the host has not got.
pub(crate) fn hosted(program: &str, sandboxed: bool) -> bool {
    sandboxed && !Path::new(program).starts_with("/app")
}

/// What the host's shell runs to find `$1`, as [`on_host`] would start it.
const FIND: &str = "command -v \"$1\" >/dev/null";

/// Whether the host finds `program` on the user's `PATH` ([`on_host`]), or as the file it names,
/// within 3 s. Blocks.
pub(crate) fn finds(program: &str) -> bool {
    let find = ["sh", "-c", FIND, "sh", program].map(String::from).to_vec();
    let argv = on_host(find, Path::new("/"), &[]);
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    let found = crate::python::run_within(command, Duration::from_secs(3), false);
    found.is_some_and(|(status, _)| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user's `PATH` goes first, so a kernelspec's own `env` can still set one.
    #[test]
    fn a_host_command_runs_in_its_folder_with_the_users_path_and_its_environment() {
        let argv = ["python3", "-m", "ipykernel_launcher"]
            .map(String::from)
            .to_vec();
        let env = [("JUPYTER_PATH".to_string(), "/x".to_string())];
        let path = Some("/home/u/.cargo/bin:/usr/bin");
        assert_eq!(
            command_line(argv.clone(), Path::new("/home/u/vault"), path, &env),
            [
                "flatpak-spawn",
                "--host",
                "--watch-bus",
                "--directory=/home/u/vault",
                "--env=PATH=/home/u/.cargo/bin:/usr/bin",
                "--env=JUPYTER_PATH=/x",
                "env",
                "python3",
                "-m",
                "ipykernel_launcher",
            ]
        );
        // No answer from the login shell: the session's own `PATH`, which the helper looks on.
        let line = command_line(argv, Path::new("/home/u/vault"), None, &[]);
        assert_eq!(
            line[3..],
            [
                "--directory=/home/u/vault",
                "python3",
                "-m",
                "ipykernel_launcher"
            ]
        );
    }

    /// Whatever a profile prints goes before the mark; an answer without one, or that breaks the
    /// line, is no `PATH`.
    #[test]
    fn the_login_path_is_read_past_what_a_profile_prints() {
        assert_eq!(
            path_of("Welcome!\n\naccent-path=/home/u/.cargo/bin:/usr/bin\n").as_deref(),
            Some("/home/u/.cargo/bin:/usr/bin")
        );
        assert_eq!(path_of("/usr/bin\n"), None);
        assert_eq!(path_of("accent-path=/usr/bin\nmore\n"), None);
        assert_eq!(path_of("accent-path=bin:.\n"), None);
    }

    /// The bundled `accent-cli ghost` stays in the sandbox, every other server goes to the host,
    /// and outside the Flatpak nothing does.
    #[test]
    fn only_the_flatpaks_own_programs_stay_in_the_sandbox() {
        assert!(!hosted("/app/lib/accent/accent-cli-x86_64", true));
        assert!(hosted("texlab", true));
        assert!(hosted("/usr/bin/clangd", true));
        assert!(hosted("/home/u/.elan/bin/lake", true));
        assert!(hosted("/application/server", true));
        assert!(!hosted("texlab", false));
    }

    /// The host's lookup, run here as the host's `sh` runs it.
    #[test]
    fn the_hosts_lookup_finds_a_program_by_name_or_path() {
        let found = |program: &str| {
            std::process::Command::new("sh")
                .args(["-c", FIND, "sh", program])
                .status()
                .unwrap()
                .success()
        };
        assert!(found("sh"));
        assert!(found("/bin/sh"));
        assert!(!found("no-such-server"));
        assert!(!found("/no/such/server"));
    }
}
