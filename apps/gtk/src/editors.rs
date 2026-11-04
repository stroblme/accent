//! Terminal editors (Helix, Vim, …) a text file opens in, in a terminal tab of its own
//! (DESIGN.md, Terminal): the ones accent knows, which of them the user's shell finds, and the
//! tab one runs in, which stands for its file as a text tab does.

use crate::open::Opened;
use crate::terminal::{self, Term};
use crate::{App, doc, fileops};
use accent_api::ssh;
use accent_core::config::{Editor, Edits};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A terminal editor accent knows: what Preferences calls it, the programs it goes by, the first
/// one the shell finds being the one run, and what goes before the file.
struct Known {
    editor: Editor,
    name: &'static str,
    binaries: &'static [&'static str],
    args: &'static [&'static str],
}

const KNOWN: [Known; 7] = [
    Known {
        editor: Editor::Helix,
        name: "Helix",
        binaries: &["hx", "helix"],
        args: &[],
    },
    Known {
        editor: Editor::Neovim,
        name: "Neovim",
        binaries: &["nvim"],
        args: &[],
    },
    Known {
        editor: Editor::Vim,
        name: "Vim",
        binaries: &["vim"],
        args: &[],
    },
    Known {
        editor: Editor::Kakoune,
        name: "Kakoune",
        binaries: &["kak"],
        args: &[],
    },
    Known {
        editor: Editor::Emacs,
        name: "Emacs",
        binaries: &["emacs"],
        args: &["-nw"],
    },
    Known {
        editor: Editor::Nano,
        name: "Nano",
        binaries: &["nano"],
        args: &[],
    },
    Known {
        editor: Editor::Micro,
        name: "Micro",
        binaries: &["micro"],
        args: &[],
    },
];

/// An editor the shell finds, by the program it found it as.
pub type Found = (Editor, &'static str);

fn known(editor: Editor) -> Option<&'static Known> {
    KNOWN.iter().find(|known| known.editor == editor)
}

/// What Preferences calls `editor`.
pub fn name(editor: Editor) -> &'static str {
    known(editor).map_or("Accent", |known| known.name)
}

/// The first of the programs `editor` goes by.
fn first(editor: Editor) -> &'static str {
    known(editor).map_or("", |known| known.binaries[0])
}

/// The program `editor` is run as on this machine: the one the shell found, else its first name,
/// the shell being asked for the next file where it has not been yet.
fn binary(editor: Editor) -> &'static str {
    let found = PROBE.with_borrow(|probe| match probe {
        Probe::Done(found) => found.iter().find(|(e, _)| *e == editor).map(|(_, b)| *b),
        _ => None,
    });
    if found.is_none() {
        installed(|_| {});
    }
    found.unwrap_or_else(|| first(editor))
}

/// The command line that opens `file` in `editor`, run as `bin`.
fn line(editor: Editor, bin: &str, file: &str) -> String {
    let args = known(editor).map_or(&[][..], |known| known.args);
    let mut words: Vec<String> = [bin].iter().chain(args).map(|w| w.to_string()).collect();
    words.push(ssh::quote(file));
    words.join(" ")
}

/// What the probe prints before each program it finds, so the lines a chatty rc file prints are
/// not taken for an answer.
const MARK: &str = "accent-editor";

/// Ask the login shell a terminal tab runs (`$SHELL -l -i -c`, on the host from the Flatpak) for
/// every program [`KNOWN`] names, so its `PATH` and aliases are the ones a tab will have. Blocks.
fn probe() -> Vec<Found> {
    let script: Vec<String> = KNOWN
        .iter()
        .flat_map(|known| known.binaries)
        .map(|bin| format!("command -v {bin} >/dev/null 2>&1 && echo {MARK} {bin}"))
        .collect();
    let host = std::fs::read_to_string("/.flatpak-info")
        .ok()
        .and_then(|info| crate::terminal::host_prefix(&info));
    let mut argv = host.unwrap_or_default();
    argv.extend(
        [
            "sh",
            "-c",
            r#"exec "${SHELL:-/bin/sh}" -l -i -c "$1""#,
            "sh",
            &script.join("; "),
        ]
        .map(str::to_string),
    );
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(out) => parse(&String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            tracing::warn!("cannot ask the shell for terminal editors: {e}");
            Vec::new()
        }
    }
}

/// The editors the probe's output names, each by the first of its programs found.
fn parse(out: &str) -> Vec<Found> {
    let found: Vec<&str> = out
        .lines()
        .filter_map(|line| line.trim().strip_prefix(MARK)?.strip_prefix(' '))
        .collect();
    KNOWN
        .iter()
        .filter_map(|known| {
            let bin = known.binaries.iter().find(|bin| found.contains(bin))?;
            Some((known.editor, *bin))
        })
        .collect()
}

/// What waits for the probe's answer.
type Waiting = Box<dyn FnOnce(&[Found])>;

/// The probe, asked once a process.
enum Probe {
    Idle,
    Running(Vec<Waiting>),
    Done(Rc<[Found]>),
}

thread_local! {
    static PROBE: RefCell<Probe> = const { RefCell::new(Probe::Idle) };
}

/// Call `then` with the editors the shell finds: at once once the probe has answered, else when
/// it does, the probe starting off the main thread the first time.
pub fn installed(then: impl FnOnce(&[Found]) + 'static) {
    let mut then: Option<Waiting> = Some(Box::new(then));
    let (done, start) = PROBE.with_borrow_mut(|probe| match probe {
        Probe::Done(found) => (Some(found.clone()), false),
        Probe::Running(waiting) => {
            waiting.extend(then.take());
            (None, false)
        }
        Probe::Idle => {
            *probe = Probe::Running(then.take().into_iter().collect());
            (None, true)
        }
    });
    if let (Some(found), Some(then)) = (done, then) {
        then(&found);
    }
    if start {
        glib::spawn_future_local(async {
            let found: Rc<[Found]> = crate::work::off_thread("editor probe", probe)
                .await
                .unwrap_or_default()
                .into();
            let waiting = PROBE.with_borrow_mut(|probe| {
                match std::mem::replace(probe, Probe::Done(found.clone())) {
                    Probe::Running(waiting) => waiting,
                    _ => Vec::new(),
                }
            });
            for then in waiting {
                then(&found);
            }
        });
    }
}

impl App {
    /// Open `key` in the terminal editor picked, in a terminal tab of its own, and say whether
    /// it did. It stays accent's where Accent is picked, where Open in Accent's Editor named it
    /// once, where what asked waits for accent's own buffer (`App::with_tab`), and for a host's
    /// dependency or a package manager's file, which are read-only.
    pub(crate) fn open_in_editor(self: &Rc<Self>, key: &str, how: Opened) -> bool {
        let editor = self.config.borrow().editor;
        if self.in_accent.borrow_mut().remove(key)
            || editor.is_accent()
            || self.awaiting.borrow().contains_key(key)
            || self.revealing.borrow().contains_key(key)
            || doc::host_file_path(key).is_some()
            || (doc::is_loose_key(key) && accent_core::path::managed(Path::new(key)))
        {
            return false;
        }
        let shell = match self.editor_shell(key) {
            Ok(shell) => shell,
            Err(why) => {
                let what = format!("open {} in {}", doc::file_name(key), name(editor));
                self.cannot(&what, why);
                return false;
            }
        };
        let edits = Edits {
            editor,
            file: key.to_string(),
        };
        let run = self.editor_run(edits);
        let term = self.wire_term(terminal::new_key(), shell, how, None, Some(run));
        term.page
            .set_tooltip(&fileops::display_path(&self.root(), key));
        self.mark_loose(&term.page, key);
        keep_on_key(self, &term);
        self.watch_folder_of(key);
        self.select_new_page(&term.page, how);
        self.mark_opened(&term.page, how);
        true
    }

    /// Where a terminal editor on `key` runs: at the vault root, on the host for a remote vault's
    /// file, and in its folder for a file outside the vault.
    fn editor_shell(&self, key: &str) -> Result<terminal::Shell, String> {
        let remote = self
            .vault()
            .and_then(|v| v.remote())
            .map(|r| r.url().clone());
        match (remote, doc::is_loose_key(key)) {
            (Some(url), false) => self.remote_shell(url),
            (None, false) => Ok(terminal::Shell::Local(self.root())),
            (_, true) => Ok(terminal::Shell::Local(
                Path::new(key)
                    .parent()
                    .map_or_else(glib::home_dir, Path::to_path_buf),
            )),
        }
    }

    /// What runs `edits`' editor on its file: the program the shell found here, or on a host,
    /// which was not asked, the editor's first name.
    pub(crate) fn editor_run(&self, edits: Edits) -> terminal::Run {
        let file = match doc::is_loose_key(&edits.file) {
            true => PathBuf::from(&edits.file),
            false => self.root().join(&edits.file),
        };
        let bin = match self.on_host(&edits.file) {
            true => first(edits.editor),
            false => binary(edits.editor),
        };
        let line = line(edits.editor, bin, &file.to_string_lossy());
        terminal::Run::editor(edits, line)
    }

    /// The terminal editor still running on `key`.
    pub(crate) fn editor_of(&self, key: &str) -> Option<Rc<Term>> {
        let mut terms = self.terminals().into_iter();
        terms.find(|term| !term.ended() && term.file().as_deref() == Some(key))
    }

    /// The tab of the terminal editor that has `key` open, which an open of `key` comes to the
    /// front in, unless what asked waits for accent's own buffer of it.
    pub(crate) fn editing(&self, key: &str) -> Option<adw::TabPage> {
        if self.awaiting.borrow().contains_key(key) {
            return None;
        }
        self.editor_of(key).map(|term| term.page.clone())
    }

    /// A terminal editor the shell did not find: its tab goes, saying so, and its file opens in
    /// accent's own editor, as a preview where the tab was one.
    pub(crate) fn editor_missing(self: &Rc<Self>, term: &Term) {
        let preview = self.pane_of(&term.page).and_then(|pane| pane.preview());
        let how = match preview.as_ref() == Some(&term.page) {
            true => Opened::Preview,
            false => Opened::Kept,
        };
        self.close_page(&term.page);
        let Some(edits) = term.edits() else {
            return;
        };
        let file = doc::file_name(&edits.file);
        self.toast(&format!(
            "No {} found, so {file} opened in Accent's editor",
            name(edits.editor)
        ));
        self.in_accent.borrow_mut().insert(edits.file.clone());
        self.open_as(&edits.file, how);
    }
}

/// Keep a preview editor's tab on the first key that reaches the editor, as a text tab is kept
/// on its first edit. Not on what the editor is sent: Helix and Vim read the mouse, so a click
/// into the tab would keep it.
fn keep_on_key(app: &Rc<App>, term: &Term) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (app, page) = (Rc::downgrade(app), term.page.downgrade());
    keys.connect_key_pressed(move |keys, _, _, _| {
        let modifier = keys
            .current_event()
            .and_then(|event| event.downcast::<gdk::KeyEvent>().ok())
            .is_some_and(|key| key.is_modifier());
        if let (false, Some(app), Some(page)) = (modifier, app.upgrade(), page.upgrade()) {
            app.promote(&page);
        }
        glib::Propagation::Proceed
    });
    term.view.add_controller(keys);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An rc file may print anything; only the marked lines answer, and Helix is run by the first
    /// of its names the shell finds.
    #[test]
    fn the_probe_reads_its_own_lines_and_the_first_name_wins() {
        let out = "Welcome back!\naccent-editor helix\naccent-editor hx\n\
                   accent-editor vim\nhx is an alias\naccent-editor nanos\n";
        assert_eq!(
            parse(out),
            [(Editor::Helix, "hx"), (Editor::Vim, "vim")],
            "nanos is no nano"
        );
        assert_eq!(parse("accent-editor helix\n"), [(Editor::Helix, "helix")]);
        assert!(parse("").is_empty());
    }

    /// The file is one word to the shell, whatever its name holds.
    #[test]
    fn a_line_quotes_its_file() {
        assert_eq!(
            line(Editor::Emacs, "emacs", "/v/a b's.md"),
            r"emacs -nw '/v/a b'\''s.md'"
        );
        assert_eq!(line(Editor::Helix, "helix", "/v/n.md"), "helix '/v/n.md'");
    }
}
