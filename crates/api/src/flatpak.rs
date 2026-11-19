//! The Flatpak's sandbox, and the host commands a local vault's kernels and language servers run
//! as from inside it: the sandbox ships none of the user's tools.

use std::path::Path;
use std::time::Duration;

/// The Flatpak's app id where this runs inside its sandbox.
pub(crate) fn sandbox() -> Option<String> {
    let id = std::env::var("FLATPAK_ID").ok();
    id.filter(|_| Path::new("/.flatpak-info").exists())
}

/// `argv` run on the host from the sandbox in `cwd`, with `env` beside the host session's own,
/// and ended once its `flatpak-spawn` is gone (`--watch-bus`).
pub(crate) fn on_host(argv: Vec<String>, cwd: &Path, env: &[(String, String)]) -> Vec<String> {
    let host = ["flatpak-spawn", "--host", "--watch-bus"].map(String::from);
    let directory = format!("--directory={}", cwd.display());
    let env = env.iter().map(|(k, v)| format!("--env={k}={v}"));
    host.into_iter()
        .chain([directory])
        .chain(env)
        .chain(argv)
        .collect()
}

/// Whether `program` starts on the host, `sandboxed` being whether this runs in the Flatpak: all
/// but the Flatpak's own, under `/app`, which the host has not got.
pub(crate) fn hosted(program: &str, sandboxed: bool) -> bool {
    sandboxed && !Path::new(program).starts_with("/app")
}

/// What the host's shell runs to find `$1`, as [`on_host`] would start it.
const FIND: &str = "command -v \"$1\" >/dev/null";

/// Whether the host finds `program` on its `PATH`, or as the file it names, within 3 s. Blocks.
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

    #[test]
    fn a_host_command_runs_in_its_folder_with_its_environment() {
        let argv = ["python3", "-m", "ipykernel_launcher"]
            .map(String::from)
            .to_vec();
        let env = [("JUPYTER_PATH".to_string(), "/x".to_string())];
        assert_eq!(
            on_host(argv, Path::new("/home/u/vault"), &env),
            [
                "flatpak-spawn",
                "--host",
                "--watch-bus",
                "--directory=/home/u/vault",
                "--env=JUPYTER_PATH=/x",
                "python3",
                "-m",
                "ipykernel_launcher",
            ]
        );
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
