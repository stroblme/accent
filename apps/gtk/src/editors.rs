//! Terminal editors (Helix, Vim, …) a text file opens in, in a terminal tab of its own
//! (DESIGN.md, Terminal): the ones accent knows, which of them the user's shell finds, and the
//! tab one runs in, which stands for its file as a text tab does.

use crate::doc::Kind;
use crate::open::{Opened, Reveal};
use crate::terminal::{self, Term};
use crate::{App, dialogs, doc, fileops};
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
    match editor {
        Editor::Custom => "Custom Command",
        editor => known(editor).map_or("Accent", |known| known.name),
    }
}

/// The program a command line runs: its first word that sets no variable.
fn program(command: &str) -> Option<&str> {
    command.split_whitespace().find(|word| !word.contains('='))
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

/// A 1-based line, and column where known, a terminal editor opens a file at.
pub type Place = (u32, Option<u32>);

/// The command line that opens `file` in `editor`, run as `bin`, at `at` where given, written as
/// each editor reads a place. Helix runs without `COLORTERM`, since only then is its default theme
/// the sixteen-colour one drawn in the terminal's colours.
fn line(editor: Editor, bin: &str, file: &str, at: Option<Place>) -> String {
    let args = known(editor).map_or(&[][..], |known| known.args);
    let env = (editor == Editor::Helix).then_some("COLORTERM=");
    let words = env.iter().chain([&bin]).chain(args);
    let mut words: Vec<String> = words.map(|w| w.to_string()).collect();
    let file = ssh::quote(file);
    let column = |sep: &str, column: Option<u32>| column.map(|c| format!("{sep}{c}"));
    match at {
        None => words.push(file),
        Some((line, c)) => match editor {
            Editor::Helix => words.push(format!(
                "{file}:{line}{}",
                column(":", c).unwrap_or_default()
            )),
            Editor::Neovim | Editor::Vim => words.extend([format!("+{line}"), file]),
            Editor::Nano => words.extend([
                format!("+{line}{}", column(",", c).unwrap_or_default()),
                file,
            ]),
            _ => words.extend([
                format!("+{line}{}", column(":", c).unwrap_or_default()),
                file,
            ]),
        },
    }
    words.join(" ")
}

/// The user's own `command` on `file`: `{line}` is the line asked for, else 1, and `{file}` the
/// file quoted for the shell, written after the command where it names none.
fn custom_line(command: &str, file: &str, at: Option<Place>) -> String {
    let line = at.map_or(1, |(line, _)| line).to_string();
    // The line first, so a file whose name holds `{line}` keeps it.
    let command = command.trim().replace("{line}", &line);
    let file = ssh::quote(file);
    match command.contains("{file}") {
        true => command.replace("{file}", &file),
        false => format!("{command} {file}"),
    }
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
        // A place a search hit, a symbol or a definition asked for ([`App::edit_at`]); anything
        // else waiting is for accent's own tab.
        let at = match self.revealing.borrow().get(key) {
            Some(Reveal::Line(line, column)) => Some((*line, *column)),
            Some(_) => return false,
            None => None,
        };
        if self.in_accent.borrow_mut().remove(key)
            || self.stays_accents(key)
            || self.awaiting.borrow().contains_key(key)
        {
            return false;
        }
        self.revealing.borrow_mut().remove(key);
        let shell = match self.editor_shell(key) {
            Ok(shell) => shell,
            Err(why) => {
                let what = format!("open {} in {}", doc::file_name(key), self.called(editor));
                self.cannot(&what, why);
                return false;
            }
        };
        let edits = Edits {
            editor,
            file: key.to_string(),
        };
        let term = self.editor_tab(terminal::new_key(), shell, edits, at, how);
        self.mark_opened(&term.page, how);
        true
    }

    /// Whether `key` opens in accent's own editor whatever the reader asks: Accent picked, or a
    /// custom command with no command line, a host's dependency, or a package manager's file, the
    /// last two being read-only.
    fn stays_accents(&self, key: &str) -> bool {
        let config = self.config.borrow();
        let none = match config.editor {
            Editor::Accent => true,
            Editor::Custom => config.editor_command.trim().is_empty(),
            _ => false,
        };
        none || doc::host_file_path(key).is_some()
            || (doc::is_loose_key(key) && accent_core::path::managed(Path::new(key)))
    }

    /// Open `rel` at `at`, as a search hit, a symbol or a definition lands, in the terminal
    /// editor picked, and say whether it did. A tab of the editor's already on the file comes to
    /// the front with its cursor left where it is. It stays the caller's to land where `rel` is
    /// accent's, a tab of accent's own having it among them.
    pub(crate) fn edit_at(self: &Rc<Self>, rel: &str, at: Place, how: Opened) -> bool {
        if self.stays_accents(rel)
            || self.doc_for(rel).is_some()
            || !matches!(doc::kind_of(rel), Kind::Note | Kind::Text)
        {
            return false;
        }
        if let Some(page) = self.editing(rel) {
            self.reveal_page(&page);
            self.focus_picked(&page, how);
            return true;
        }
        // Through the read, whose bytes say whether a `.py` or an `.xml` is a notebook or a
        // diagram after all; one that lands in accent's own editor lands at the place.
        let line = Reveal::Line(at.0, at.1);
        self.revealing.borrow_mut().insert(rel.to_string(), line);
        self.open_as(rel, how);
        true
    }

    /// Put back the terminal editor tab `key` a session names, attached to its editor, or with
    /// the editor started on the file again where nothing runs it any more.
    pub(crate) fn restore_editor(self: &Rc<Self>, key: &str, shell: terminal::Shell, edits: Edits) {
        self.editor_tab(key.to_string(), shell, edits, None, Opened::Restored);
    }

    /// A terminal editor's tab on `edits`' file, opened as `key` and wired as the file's.
    fn editor_tab(
        self: &Rc<Self>,
        key: String,
        shell: terminal::Shell,
        edits: Edits,
        at: Option<Place>,
        how: Opened,
    ) -> Rc<Term> {
        let file = edits.file.clone();
        let run = self.editor_run(edits, at);
        let term = self.wire_term(key, shell, how, None, Some(run));
        term.page
            .set_tooltip(&fileops::display_path(&self.root(), &file));
        self.mark_loose(&term.page, &file);
        keep_on_key(self, &term);
        self.watch_folder_of(&file);
        self.select_new_page(&term.page, how);
        term
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

    /// What runs `edits`' editor on its file: the user's command line, or the program the shell
    /// found here, or on a host, which was not asked, the editor's first name.
    fn editor_run(&self, edits: Edits, at: Option<Place>) -> terminal::Run {
        let file = match doc::is_loose_key(&edits.file) {
            true => PathBuf::from(&edits.file),
            false => self.root().join(&edits.file),
        };
        let file = file.to_string_lossy();
        let line = match (edits.editor, self.on_host(&edits.file)) {
            (Editor::Custom, _) => custom_line(&self.config.borrow().editor_command, &file, at),
            (editor, true) => line(editor, first(editor), &file, at),
            (editor, false) => line(editor, binary(editor), &file, at),
        };
        terminal::Run::editor(edits, line)
    }

    /// What the window's messages call `editor`: its name, or the program a custom command runs.
    fn called(&self, editor: Editor) -> String {
        match editor {
            Editor::Custom => program(&self.config.borrow().editor_command)
                .unwrap_or("The editor")
                .to_string(),
            editor => name(editor).to_string(),
        }
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

    /// The terminal editor whose tab `page` is, where closing it asks first: one still running in
    /// a kept tab. A preview's has taken no key.
    pub(crate) fn editor_asks(&self, page: &adw::TabPage) -> Option<Rc<Term>> {
        let term = self.doc_for_page(page)?.terminal()?.clone();
        let preview = self.pane_of(page).and_then(|pane| pane.preview());
        let running = term.edits().is_some() && !term.ended();
        (running && preview.as_ref() != Some(page)).then_some(term)
    }

    /// Ask before `term`'s tab ends its editor, unsaved changes and all, and tell `then` the
    /// answer: `true` to close.
    pub(crate) fn ask_close_editor(
        self: &Rc<Self>,
        term: &Term,
        then: impl FnOnce(&Rc<App>, bool) + 'static,
    ) {
        let Some(edits) = term.edits() else {
            return then(self, true);
        };
        let heading = format!("Close {}?", doc::file_name(&edits.file));
        let body = format!(
            "{} is still running; unsaved changes in it are lost.",
            self.called(edits.editor)
        );
        self.ask_close(&heading, &body, then);
    }

    /// Ask before an unnamed window's close ends the terminal editors running in it, which a
    /// window keeping no session cannot hand back. `true` once there are none left to ask about.
    pub(crate) fn editors_closed(self: &Rc<Self>) -> bool {
        let running: Vec<Rc<Term>> = (self.terminals().into_iter())
            .filter(|term| self.editor_asks(&term.page).is_some())
            .collect();
        let (Some(first), false) = (running.first(), self.keeps_session() || self.reloading())
        else {
            return true;
        };
        let body = match (running.len(), first.edits()) {
            (1, Some(edits)) => format!(
                "{} is still running in {}; unsaved changes in it are lost.",
                self.called(edits.editor),
                doc::file_name(&edits.file)
            ),
            (n, _) => {
                format!(
                    "Terminal editors are still running in {n} tabs; unsaved changes in them are lost."
                )
            }
        };
        self.ask_close("Close Window?", &body, move |app, close| {
            if !close {
                return app.keep_open();
            }
            // Gone from the window's documents, so the close that follows asks nothing.
            for term in &running {
                app.forget_page(&term.page);
            }
            app.window.close();
        });
        false
    }

    /// Cancel or Close, the second destructive.
    fn ask_close(
        self: &Rc<Self>,
        heading: &str,
        body: &str,
        then: impl FnOnce(&Rc<App>, bool) + 'static,
    ) {
        let dialog = dialogs::alert(
            heading,
            body,
            &[
                ("cancel", "Cancel", adw::ResponseAppearance::Default),
                (
                    dialogs::CONFIRM,
                    "Close",
                    adw::ResponseAppearance::Destructive,
                ),
            ],
            "cancel",
        );
        let app = self.clone();
        dialogs::choose(&dialog, Some(&self.window), move |response| {
            then(&app, response == dialogs::CONFIRM)
        });
    }

    /// Open in Accent's Editor, on the terminal editor's tab the menu was opened on, else the one
    /// in front.
    pub(crate) fn editor_tab_in_accent(self: &Rc<Self>) {
        let page = (self.menu_page.borrow().clone()).or_else(|| self.tabs().selected_page());
        let term = page
            .and_then(|page| self.doc_for_page(&page))
            .and_then(|doc| doc.terminal().cloned());
        if let Some(file) = term.as_ref().and_then(|term| term.file()) {
            self.open_in_accent(&file, term);
        }
    }

    /// Open `key` in accent's own editor, once, as a notebook's Open as Text does: the terminal
    /// editor `term` running on it, else the one [`editor_of`](Self::editor_of) finds, ends with
    /// its tab first, asked about as its × asks.
    pub(crate) fn open_in_accent(self: &Rc<Self>, key: &str, term: Option<Rc<Term>>) {
        let Some(page) = term.or_else(|| self.editor_of(key)).map(|t| t.page.clone()) else {
            return self.once_in_accent(key);
        };
        let asks = self.editor_asks(&page);
        let key = key.to_string();
        let close = move |app: &Rc<App>| {
            app.select_survivor(&page);
            app.forget_page(&page);
            app.close_page(&page);
            app.once_in_accent(&key);
        };
        match asks {
            Some(term) => self.ask_close_editor(&term, move |app, yes| {
                if yes {
                    close(app);
                }
            }),
            None => close(self),
        }
    }

    /// Open `key` in accent's own editor this once.
    fn once_in_accent(self: &Rc<Self>, key: &str) {
        self.in_accent.borrow_mut().insert(key.to_string());
        self.open_path(key);
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
            self.called(edits.editor)
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
            line(Editor::Emacs, "emacs", "/v/a b's.md", None),
            r"emacs -nw '/v/a b'\''s.md'"
        );
        assert_eq!(
            line(Editor::Helix, "helix", "/v/n.md", None),
            "COLORTERM= helix '/v/n.md'"
        );
    }

    /// A custom command has its placeholders filled in, the file quoted, and is given the file
    /// last where it names none; an open that asks no line is at the first.
    #[test]
    fn a_custom_command_fills_in_its_file_and_line() {
        let file = "/v/a b's.md";
        assert_eq!(
            custom_line("kak +{line} {file}", file, Some((12, Some(5)))),
            r"kak +12 '/v/a b'\''s.md'"
        );
        assert_eq!(
            custom_line(" COLORTERM= hx {file}:{line} ", "/v/n.md", None),
            "COLORTERM= hx '/v/n.md':1"
        );
        assert_eq!(custom_line("micro", "/v/n.md", None), "micro '/v/n.md'");
        assert_eq!(
            custom_line("vim {file}", "/v/{line}.md", Some((3, None))),
            "vim '/v/{line}.md'"
        );
        assert_eq!(program("COLORTERM= hx {file}"), Some("hx"));
    }

    /// Each editor reads a place its own way, and one with no column is a line alone.
    #[test]
    fn a_line_puts_the_place_where_each_editor_reads_it() {
        let at = |editor, bin| line(editor, bin, "/v/n.md", Some((12, Some(5))));
        assert_eq!(at(Editor::Helix, "hx"), "COLORTERM= hx '/v/n.md':12:5");
        assert_eq!(at(Editor::Neovim, "nvim"), "nvim +12 '/v/n.md'");
        assert_eq!(at(Editor::Vim, "vim"), "vim +12 '/v/n.md'");
        assert_eq!(at(Editor::Kakoune, "kak"), "kak +12:5 '/v/n.md'");
        assert_eq!(at(Editor::Emacs, "emacs"), "emacs -nw +12:5 '/v/n.md'");
        assert_eq!(at(Editor::Nano, "nano"), "nano +12,5 '/v/n.md'");
        assert_eq!(at(Editor::Micro, "micro"), "micro +12:5 '/v/n.md'");
        let line_only = |editor, bin| line(editor, bin, "/v/n.md", Some((12, None)));
        assert_eq!(line_only(Editor::Helix, "hx"), "COLORTERM= hx '/v/n.md':12");
        assert_eq!(line_only(Editor::Nano, "nano"), "nano +12 '/v/n.md'");
    }
}
