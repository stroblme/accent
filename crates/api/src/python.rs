//! Python environments available to a vault's notebooks and source files.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::language::{Server, Task, remote_task_then};
use crate::vault::Backend;
use crate::{Local, Result, Vault};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// A Python command with ipykernel installed, selectable for this vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub command: Vec<String>,
    pub label: String,
}

/// The available commands, the current choice, and a project setting that overrides Pyright.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environments {
    pub choices: Vec<Environment>,
    pub selected: Vec<String>,
    pub pyright_override: Option<String>,
}

impl Vault {
    /// Find Python environments on the machine holding this vault, for `rel`'s project.
    pub fn python_environments(&self, rel: &str) -> Task<Environments> {
        match &self.backend {
            Backend::Local(v) => {
                let (root, cfg, rel) = (v.root().to_path_buf(), v.config(), rel.to_string());
                Task::blocking(move || discover(&root, &rel, &cfg))
            }
            Backend::Remote(r) => remote_task_then(
                r.clone(),
                "python_environments",
                json!([rel]),
                Duration::from_secs(30),
                |_, answer| Ok(answer),
            ),
        }
    }
}

pub(crate) fn dispatch(vault: &Local, method: &str, p: &Value) -> Option<Result<Value>> {
    (method == "python_environments").then(|| {
        let rel: String = crate::rpc::arg(p, 0)?;
        crate::rpc::answer(discover(vault.root(), &rel, &vault.config()))
    })
}

fn discover(
    root: &Path,
    rel: &str,
    cfg: &accent_core::config::VaultConfig,
) -> Result<Environments> {
    let at = Local::join(root, rel)?;
    let selected = cfg.debug.python.clone();
    let mut commands = Vec::new();
    commands.push(selected.clone());
    commands.push(vec!["python3".into()]);
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            if path.join("pyvenv.cfg").is_file() || name == ".venv" || name == "venv" {
                let python = path.join("bin/python");
                if python.symlink_metadata().is_ok() {
                    let python = python.strip_prefix(root).unwrap_or(&python);
                    commands.push(vec![python.to_string_lossy().into_owned()]);
                }
                continue;
            }
            if !matches!(
                name.to_str(),
                Some(".git" | ".trash" | "node_modules" | "target" | ".lake")
            ) {
                dirs.push(path);
            }
        }
    }
    commands.sort();
    commands.dedup();
    let mut choices: Vec<Environment> = commands
        .into_iter()
        .filter(|command| !command.is_empty())
        .filter(|command| has_ipykernel(root, command))
        .map(|command| Environment {
            label: command.join(" "),
            command,
        })
        .collect();
    choices.insert(
        0,
        Environment {
            command: Vec::new(),
            label: "Use default Python configuration".into(),
        },
    );
    let pyright = matches!(
        crate::language::server(&cfg.lsp, "python3"),
        Some(Server::External { argv, .. })
            if argv.first().and_then(|name| Path::new(name).file_name())
                .is_some_and(|name| name == "pyright-langserver")
    );
    let pyright_override = pyright.then(|| project_override(root, &at)).flatten();
    Ok(Environments {
        choices,
        selected,
        pyright_override,
    })
}

fn has_ipykernel(root: &Path, command: &[String]) -> bool {
    has_ipykernel_within(root, command, PROBE_TIMEOUT)
}

fn has_ipykernel_within(root: &Path, command: &[String], timeout: Duration) -> bool {
    let Some(mut check) = python_command(root, command) else {
        return false;
    };
    check.args(["-c", "import ipykernel"]);
    run_within(check, timeout, false).is_some_and(|(status, _)| status.success())
}

/// Resolve a wrapper command to the interpreter it actually starts, for Pyright and Jedi.
pub(crate) fn executable(root: &Path, command: &[String]) -> Option<String> {
    match command {
        [python] if Path::new(python).is_absolute() => return Some(python.clone()),
        [python] if !python.contains('/') => {
            return std::env::var_os("PATH")
                .and_then(|paths| {
                    std::env::split_paths(&paths)
                        .map(|dir| dir.join(python))
                        .find(|path| path.is_file())
                })
                .map(|path| path.to_string_lossy().into_owned());
        }
        _ => {}
    }
    let mut check = python_command(root, command)?;
    check.args(["-c", "import sys; print(sys.executable)"]);
    let (status, output) = run_within(check, PROBE_TIMEOUT, true)?;
    status
        .success()
        .then(|| String::from_utf8(output).ok())
        .flatten()
        .map(|path| path.trim().to_string())
        .filter(|path| Path::new(path).is_absolute())
}

fn python_command(root: &Path, command: &[String]) -> Option<Command> {
    let (python, args) = command.split_first()?;
    let python = if python.contains('/') && Path::new(python).is_relative() {
        root.join(python).to_string_lossy().into_owned()
    } else {
        python.clone()
    };
    let sandbox =
        std::env::var("FLATPAK_ID").ok().is_some() && Path::new("/.flatpak-info").exists();
    let mut check = if sandbox {
        let mut cmd = Command::new("flatpak-spawn");
        cmd.args([
            "--host",
            &format!("--directory={}", root.display()),
            &python,
        ]);
        cmd
    } else {
        Command::new(&python)
    };
    check.args(args).current_dir(root);
    Some(check)
}

fn run_within(
    mut command: Command,
    timeout: Duration,
    capture: bool,
) -> Option<(std::process::ExitStatus, Vec<u8>)> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    command
        .stdin(Stdio::null())
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().ok()?;
    let until = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut output = Vec::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let fd = stdout.as_raw_fd();
                    // A wrapper may exit while a descendant still owns stdout. Reading the
                    // answer must not wait for that descendant after the command has ended.
                    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                    if flags < 0
                        || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
                    {
                        return None;
                    }
                    let mut bytes = [0; 4096];
                    match stdout.read(&mut bytes) {
                        Ok(n) => output.extend_from_slice(&bytes[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => return None,
                    }
                }
                return Some((status, output));
            }
            Ok(None) if Instant::now() >= until => {
                // Wrappers such as `uv run` may have started Python in the same group.
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn project_override(root: &Path, file: &Path) -> Option<String> {
    for dir in file
        .parent()?
        .ancestors()
        .take_while(|dir| dir.starts_with(root))
    {
        let config = dir.join("pyrightconfig.json");
        if config.is_file() {
            let settings: Value =
                serde_json::from_str(&std::fs::read_to_string(&config).ok()?).ok()?;
            return if pyright_venv(&settings) {
                Some(
                    config
                        .strip_prefix(root)
                        .ok()?
                        .to_string_lossy()
                        .into_owned(),
                )
            } else {
                None
            };
        }
        let config = dir.join("pyproject.toml");
        if config.is_file() {
            let table: toml::Table = std::fs::read_to_string(&config).ok()?.parse().ok()?;
            if let Some(settings) = table.get("tool").and_then(|tool| tool.get("pyright")) {
                return if settings.get("venv").is_some() && settings.get("venvPath").is_some() {
                    Some(
                        config
                            .strip_prefix(root)
                            .ok()?
                            .to_string_lossy()
                            .into_owned(),
                    )
                } else {
                    None
                };
            }
        }
    }
    None
}

fn pyright_venv(settings: &Value) -> bool {
    settings.get("venv").is_some() && settings.get("venvPath").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn finds_a_nested_python_environment_with_ipykernel() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let env = root.path().join("project/.venv/bin");
        std::fs::create_dir_all(&env).unwrap();
        std::fs::write(root.path().join("project/code.py"), "print(1)").unwrap();
        let python = env.join("python");
        std::fs::write(&python, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();

        let found = discover(
            root.path(),
            "project/code.py",
            &accent_core::config::VaultConfig::default(),
        )
        .unwrap();
        assert!(found.selected.is_empty());
        assert!(found.choices.iter().any(|env| env.command.is_empty()));
        assert!(
            found
                .choices
                .iter()
                .any(|env| env.command == ["project/.venv/bin/python"])
        );
    }

    #[test]
    fn project_config_override_is_named() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("pyproject.toml"),
            "[tool.pyright]\nvenvPath = '.'\nvenv = '.venv'\n",
        )
        .unwrap();
        assert_eq!(
            project_override(root.path(), &project.join("code.py")),
            Some("project/pyproject.toml".into())
        );
        std::fs::write(
            project.join("pyrightconfig.json"),
            r#"{"typeCheckingMode": "basic"}"#,
        )
        .unwrap();
        assert_eq!(
            project_override(root.path(), &project.join("code.py")),
            None
        );
        std::fs::write(
            project.join("pyrightconfig.json"),
            r#"{"venvPath": ".", "venv": ".venv"}"#,
        )
        .unwrap();
        assert_eq!(
            project_override(root.path(), &project.join("code.py")),
            Some("project/pyrightconfig.json".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_multi_argument_python_command_reports_its_executable() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let shim = root.path().join("python-shim");
        std::fs::write(
            &shim,
            "#!/bin/sh\n[ \"$1\" = --isolated ] || exit 1\n[ \"$2\" = -c ] || exit 1\nprintf '/tmp/python-from-shim\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            executable(
                root.path(),
                &[shim.to_string_lossy().into_owned(), "--isolated".into()]
            ),
            Some("/tmp/python-from-shim".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_python_probe_ends_within_its_budget() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let shim = root.path().join("python-shim");
        std::fs::write(&shim, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let start = std::time::Instant::now();
        assert!(!has_ipykernel_within(
            root.path(),
            &[shim.to_string_lossy().into_owned()],
            Duration::from_millis(100),
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
