//! What a shell's tab runs, and the command line that runs it.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use accent_api::ssh;
use accent_core::config::Edits;

use super::holder::{cli, owner};

/// One of the user's commands (`[[commands]]` in config.toml), which the held shell runs in
/// place of its prompt and ends with (DESIGN.md, Terminal).
pub struct Run {
    /// Its name, which titles the tab.
    pub name: String,
    /// The command line, until the holder has been asked to start it. Every attach after that, a
    /// reconnect's or a restored tab's, takes the command up only while it still runs: one that
    /// has ended is not started again.
    line: RefCell<Option<String>>,
    /// Whether the last attach was such a take-up, so that one finding nothing closes the tab.
    pub(super) resumed: Cell<bool>,
    /// The file a terminal editor's tab edits, which it is titled, iconed and put back as.
    pub(super) edits: Option<Edits>,
}

impl Run {
    /// `line` run under `name` in a new shell.
    pub fn new(name: String, line: String) -> Run {
        Run {
            name,
            line: RefCell::new(Some(line)),
            resumed: Cell::new(false),
            edits: None,
        }
    }

    /// A terminal editor's `line` on `edits`' file. Its line stays: every attach, a restored
    /// tab's and a reconnect's, finds the editor still running or starts it on the file again.
    pub fn editor(edits: Edits, line: String) -> Run {
        Run {
            name: crate::doc::file_name(&edits.file).to_string(),
            line: RefCell::new(Some(line)),
            resumed: Cell::new(false),
            edits: Some(edits),
        }
    }

    /// The command `name` a session remembers, taken up only if it still runs.
    pub fn resume(name: String) -> Run {
        Run {
            name,
            line: RefCell::new(None),
            resumed: Cell::new(true),
            edits: None,
        }
    }

    /// What `attach` is given for it: `--run <line>` the first time, `--existing` after, and an
    /// editor's line every time.
    pub(super) fn flags(&self) -> Vec<String> {
        let line = match self.edits {
            Some(_) => self.line.borrow().clone(),
            None => self.line.take(),
        };
        self.resumed.set(line.is_none());
        match line {
            Some(line) => vec!["--run".to_string(), line],
            None => vec!["--existing".to_string()],
        }
    }
}

/// The command a shell tab runs.
///
/// Remote vaults run shells on the host so builds see the vault's files and repository.
/// Both local and remote shells use the same tab widget.
#[derive(Clone)]
pub enum Shell {
    /// The user's own shell, in a directory on this machine.
    Local(PathBuf),
    /// A shell held on a host, attached over `ssh -t`: see [`Shell::remote`]. Its commands name
    /// the server last made ready there ([`accent_api::link::prepared`]) as they run, never one
    /// kept from before a reconnect, whose upload may have swept it.
    Remote {
        /// Where it was opened: the host, and the directory on it.
        at: ssh::Url,
        /// The address whose ssh master it rides, which [`start`](super::attach::start) makes
        /// ready before attaching: a remote vault's own, or the host's.
        link: ssh::Url,
    },
    /// A debugged program, run as its debug adapter asked: not held, since the program is its
    /// session's and must not outlive it.
    Program(Rc<Program>),
    /// The Debug Console, which runs nothing: its transcript and its prompt are fed
    /// (`debug::console`).
    Console,
}

/// What a debug adapter asked to have run in a terminal (`runInTerminal`).
pub struct Program {
    pub args: Vec<String>,
    pub cwd: String,
    /// Added to the tab's environment.
    pub env: Vec<(String, String)>,
    /// The program's file name, which titles the tab.
    pub title: String,
    /// The remote vault's address, whose master is up already; `None` on this machine.
    pub link: Option<ssh::Url>,
    /// Told the program's process id once it runs, `None` on a host, where ssh's would be this
    /// machine's; or why it could not be run.
    pub started: Box<dyn Fn(Result<Option<u32>, String>)>,
}

impl Shell {
    /// The terminal and vault must be on the same machine and SSH account.
    pub fn same_host(&self, vault: Option<&ssh::Url>) -> bool {
        match (self, vault) {
            (Shell::Local(_), None) => true,
            (Shell::Remote { at, .. }, Some(vault)) => at.authority() == vault.authority(),
            _ => false,
        }
    }

    pub fn shareable(&self, vault: Option<&ssh::Url>) -> bool {
        self.same_host(vault) && self.held().is_some()
    }

    pub(super) fn share_command(&self, id: &str, vault: &Path, share: bool) -> Option<Vec<String>> {
        match self {
            Shell::Program(_) | Shell::Console => None,
            Shell::Local(_) => {
                let mut argv = cli()?;
                argv.push(if share { "share" } else { "revoke" }.into());
                argv.push(id.into());
                if share {
                    argv.push("--vault".into());
                    argv.push(vault.to_str()?.into());
                }
                Some(argv)
            }
            Shell::Remote { at, link } => {
                let server = ssh::server_path(&accent_api::link::prepared(at).ok()?);
                let command = match share {
                    true => ssh::share_cmd(&server, id, vault),
                    false => ssh::revoke_cmd(&server, id),
                };
                Some(ssh::run(at, &ssh::control_path(link), &command))
            }
        }
    }

    /// A shell held on the host `at` names and started in its path, riding the master of `link`;
    /// refused where this machine has no server to upload.
    pub fn remote(at: ssh::Url, link: ssh::Url) -> Result<Self, String> {
        accent_api::link::server().map_err(|e| e.to_string())?;
        Ok(Self::Remote { at, link })
    }

    /// Command to list held shells locally or through the host master. `None` without `accent-cli`.
    pub(super) fn held(&self) -> Option<Vec<String>> {
        match self {
            Shell::Program(_) | Shell::Console => None,
            Shell::Local(_) => {
                let mut argv = cli()?;
                argv.push("held".to_string());
                Some(argv)
            }
            Shell::Remote { at, link } => {
                let server = ssh::server_path(&accent_api::link::prepared(at).ok()?);
                Some(ssh::run(
                    at,
                    &ssh::control_path(link),
                    &ssh::held_cmd(&server),
                ))
            }
        }
    }

    /// Command to fetch shell `id`'s last clipboard copy
    /// ([`take_copies`](super::holder::take_copies)) locally or through the host master.
    pub(super) fn clip(&self, id: &str) -> Option<Vec<String>> {
        match self {
            Shell::Program(_) | Shell::Console => None,
            Shell::Local(_) => {
                let mut argv = cli()?;
                argv.extend(["clip".to_string(), id.to_string()]);
                Some(argv)
            }
            Shell::Remote { at, link } => {
                let server = ssh::server_path(&accent_api::link::prepared(at).ok()?);
                let ctl = ssh::control_path(link);
                Some(ssh::run(at, &ctl, &ssh::clip_cmd(&server, id)))
            }
        }
    }
}

/// The working directory, command line and added environment that run `shell`, the held shell
/// `id`, `flags` going to its `attach` ([`Run::flags`]); `None` for the console, which runs
/// nothing.
pub(super) fn command(
    shell: &Shell,
    id: &str,
    flags: &[String],
) -> Option<(Option<PathBuf>, Vec<String>, Vec<String>)> {
    Some(match (shell, cli()) {
        (Shell::Local(cwd), Some(mut argv)) => {
            argv.extend([
                "attach".to_string(),
                "--cwd".to_string(),
                cwd.to_string_lossy().into_owned(),
                "--owner".to_string(),
                owner(),
            ]);
            argv.extend(flags.iter().cloned());
            argv.push(id.to_string());
            (None, argv, Vec::new())
        }
        // Unheld: a command runs as the holder would run it, and one to take up again is gone.
        (Shell::Local(cwd), None) => match flags {
            [_, line] => {
                let argv = [user_shell().as_str(), "-l", "-i", "-c", line].map(str::to_string);
                (Some(cwd.clone()), argv.to_vec(), Vec::new())
            }
            [_] => return None,
            _ => (Some(cwd.clone()), vec![user_shell()], Vec::new()),
        },
        // ssh decides where it lands, and a cwd on this machine means nothing to it.
        (Shell::Remote { at, link }, _) => {
            let server = ssh::server_path(&accent_api::link::prepared(at).ok()?);
            let argv = ssh::attach(at, &ssh::control_path(link), &server, id, flags);
            (None, argv, Vec::new())
        }
        (Shell::Program(program), _) => match &program.link {
            None => (
                Some(PathBuf::from(&program.cwd)),
                program.args.clone(),
                program
                    .env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect(),
            ),
            // On the host, in its folder with its environment, over the vault's master.
            Some(link) => {
                let ctl = ssh::control_path(link);
                let argv = ssh::program(link, &ctl, &program.cwd, &program.env, &program.args);
                (None, argv, Vec::new())
            }
        },
        (Shell::Console, _) => return None,
    })
}

/// Use `$SHELL` or `/bin/sh`; vte4 0.10 does not bind password-database `vte_get_user_shell`.
pub(super) fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_held_shell_on_the_vault_host_can_be_shared() {
        let local = Shell::Local(PathBuf::from("/home/me"));
        let at = ssh::parse("ssh://me@host/srv/other").unwrap();
        let vault = ssh::parse("ssh://me@host/srv/vault").unwrap();
        let elsewhere = ssh::parse("ssh://other@host/srv/vault").unwrap();
        let remote = Shell::Remote {
            at,
            link: vault.clone(),
        };
        assert!(local.same_host(None));
        assert!(!local.same_host(Some(&vault)));
        assert!(remote.same_host(Some(&vault)));
        assert!(!remote.same_host(Some(&elsewhere)));
        assert!(!remote.same_host(None));
        assert!(!Shell::Console.same_host(None));
    }
}
