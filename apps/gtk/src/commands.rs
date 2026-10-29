//! The user's own commands, `[[commands]]` in config.toml: a window action each, `win.command-<i>`
//! in the file's order, which Run Command lists by the command's name and its shortcut fires. It
//! runs the command line in a held shell of a terminal tab of its own, as New Terminal opens one
//! (DESIGN.md, Terminal).

use crate::open::Opened;
use crate::{App, palette, terminal};
use accent_core::config::Config;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

/// What a command's action is called on the window, before its index.
const NAME: &str = "command-";

/// The window action that runs the command at `index`.
pub fn action(index: usize) -> String {
    format!("win.{NAME}{index}")
}

/// The index of the command a `win.command-<i>` action runs.
pub fn index(action: &str) -> Option<usize> {
    action
        .strip_prefix("win.")?
        .strip_prefix(NAME)?
        .parse()
        .ok()
}

/// The command's chord as the table lists chords: its shortcut, or none.
pub fn accels(config: &Config, index: usize) -> Vec<String> {
    let command = config.commands.get(index);
    command
        .and_then(|c| c.shortcut.clone())
        .into_iter()
        .collect()
}

/// Run Command's rows for the commands, after the table's, `used` being the window's recently
/// run commands.
pub fn items(config: &Config, used: &[String]) -> Vec<palette::Item> {
    (0..config.commands.len())
        .map(|i| palette::Item::Command {
            action: action(i),
            label: config.commands[i].name.clone(),
            accels: accels(config, i),
            recent: used.iter().position(|a| *a == action(i)),
            why: None,
            listed: true,
        })
        .collect()
}

/// Every chord in use with what holds it, but the command at `except`'s own: the table's, the
/// other commands' and the tab bar's, which the rebind dialog refuses by name.
pub fn taken(config: &Config, except: Option<usize>) -> Vec<(String, String)> {
    let table = crate::actions::ACTIONS
        .iter()
        .flat_map(|(action, label, _)| {
            let accels = crate::actions::accels_for(config, action);
            accels.into_iter().map(|accel| (accel, label.to_string()))
        });
    let commands = config.commands.iter().enumerate();
    let commands = commands
        .filter(|(i, _)| Some(*i) != except)
        .filter_map(|(_, c)| Some((c.shortcut.clone()?, c.name.clone())));
    let tabs = crate::panes::widget_chords();
    let tabs = tabs
        .into_iter()
        .map(|(a, what)| (a.to_string(), what.to_string()));
    table.chain(commands).chain(tabs).collect()
}

impl App {
    /// Give the window an action for each command, and take away those of commands gone.
    pub(crate) fn sync_commands(self: &Rc<Self>) {
        let count = self.config.borrow().commands.len();
        for name in self.window.list_actions() {
            let at = name
                .strip_prefix(NAME)
                .and_then(|i| i.parse::<usize>().ok());
            if at.is_some_and(|at| at >= count) {
                self.window.remove_action(&name);
            }
        }
        for i in 0..count {
            let name = format!("{NAME}{i}");
            if self.window.lookup_action(&name).is_some() {
                continue;
            }
            let run = gio::SimpleAction::new(&name, None);
            run.connect_activate(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_, _| {
                    app.command_used(&action(i));
                    app.run_command(i);
                }
            ));
            self.window.add_action(&run);
        }
    }

    /// Run the command at `index` in a new terminal tab, as New Terminal opens a shell: at the
    /// vault root, on the host for a remote vault, at home without a vault.
    fn run_command(self: &Rc<Self>, index: usize) {
        let Some(command) = self.config.borrow().commands.get(index).cloned() else {
            return;
        };
        let shell = match self.vault().and_then(|v| v.remote()) {
            Some(remote) => match self.remote_shell(remote.url().clone()) {
                Ok(shell) => shell,
                Err(why) => return self.cannot(&format!("run {} on the host", command.name), why),
            },
            None => terminal::Shell::Local(self.vault().map_or_else(glib::home_dir, |v| v.root())),
        };
        let run = terminal::Run::new(command.name, command.run);
        self.open_term(terminal::new_key(), shell, Opened::Kept, None, Some(run));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_core::config::CustomCommand;

    #[test]
    fn a_command_is_its_action_and_its_shortcut_its_chord() {
        assert_eq!(index(&action(3)), Some(3));
        assert_eq!(index("win.command-x"), None);
        assert_eq!(index("win.terminal"), None);
        let config = Config {
            commands: vec![
                CustomCommand {
                    name: "Claude".into(),
                    run: "claude".into(),
                    shortcut: Some("<Control><Shift>k".into()),
                },
                CustomCommand {
                    name: "Build".into(),
                    run: "make".into(),
                    shortcut: None,
                },
            ],
            ..Config::default()
        };
        assert_eq!(accels(&config, 0), ["<Control><Shift>k"]);
        assert!(accels(&config, 1).is_empty() && accels(&config, 2).is_empty());
        // Its own chord is not in its way, another command's is.
        let holder = |except| {
            taken(&config, except)
                .into_iter()
                .find(|(accel, _)| accel == "<Control><Shift>k")
                .map(|(_, holder)| holder)
        };
        assert_eq!(holder(Some(0)), None);
        assert_eq!(holder(Some(1)).as_deref(), Some("Claude"));
        assert!(
            taken(&config, None)
                .iter()
                .any(|(_, h)| h == "New Terminal")
        );
    }
}
