//! Shells as ordinary tabs, sharing pane splitting, dragging and context menus.
//!
//! Focused terminals receive shell keys. The window keeps the subset in `actions::kept`;
//! Forward All Keys keeps only terminal Copy and Paste. Application accelerators run before
//! VTE controllers, so `Shell::apply_accels` must unbind the others while a terminal has focus.
//!
//! Handlers, gestures and action groups must hold widgets weakly. GTK4 finalises by reference
//! count; cycles retain the VTE pty and its attached process after a tab closes.
//!
//! `accent-cli` holds the shell; the tab runs `accent-cli attach`. Hangup detaches, Close Tab
//! ends it explicitly ([`Term::kill`]), and [`sweep`] ends shells no saved session names. A
//! user's command ([`Run`]) is such a shell running its command line in place of a prompt, and so
//! is a terminal editor on a file ([`Run::editor`]).

mod attach;
mod exit;
mod holder;
mod key;
mod links;
mod shell;
mod view;

pub use exit::{Ended, on_exit};
#[cfg(feature = "termprops")]
pub use holder::install_termprops;
pub use holder::{Facts, cli, end, end_orphans, end_stored, host_prefix, sweep};
pub use key::{SESSION, console_key, is_key, new_key, program_key, session_key, session_name};
pub use shell::{Program, Run, Shell};
pub(crate) use view::scroller;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use accent_api::ssh;
use accent_core::config::Edits;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use vte4::TerminalExt;

use attach::start;
use holder::{Orphan, facts_after_query, kill_remote, output, take_copies};
use key::id;
use links::install_links;
use shell::user_shell;
use view::{install_keys, monospace, paint};

/// Scrollback, in lines. Enough to read back a build, far short of a memory question.
const SCROLLBACK: i64 = 10_000;
/// Horizontal padding from DESIGN.md's spacing scale, retaining more terminal columns than prose gutters.
const PAD: i32 = 12;

type OnFacts = Box<dyn Fn(&Rc<Term>)>;

/// One open shell.
pub struct Term {
    key: String,
    pub page: adw::TabPage,
    pub view: vte4::Terminal,
    /// What it runs, kept so a remote shell can be run again once its link is back.
    shell: Shell,
    /// Whether a dropped link ended the shell and the tab is waiting for [`reopen`](Self::reopen).
    lost: Cell<bool>,
    /// Whether the shell has ended, so there is nothing left for [`kill`](Self::kill) to end.
    ended: Cell<bool>,
    /// Last holder facts, in-flight request state and whether one follow-up request is due.
    holder: RefCell<Facts>,
    asking: Cell<bool>,
    again: Cell<bool>,
    waiting: RefCell<Vec<OnFacts>>,
    /// This vault's root on the shell's host, only when this held shell can be shared with it.
    share_vault: Option<PathBuf>,
    sharing: Cell<bool>,
    /// The user's command the shell runs, if it runs one rather than a prompt.
    run: Option<Run>,
}

/// A focused shell's live working directory and machine for Git discovery.
pub enum GitAt {
    Local(PathBuf),
    Remote(ssh::Url, PathBuf),
}

impl Term {
    pub fn git_at(&self) -> Option<GitAt> {
        if self.lost.get() || self.ended.get() {
            return None;
        }
        match &self.shell {
            Shell::Local(start) => {
                let osc7 = self
                    .view
                    .current_directory_uri()
                    .and_then(|uri| dir_of(&uri, &glib::host_name()));
                let holder = self.holder.borrow();
                Some(GitAt::Local(local_git_dir(
                    osc7,
                    holder.dir.as_deref(),
                    start,
                )))
            }
            Shell::Remote { at, .. } => Some(GitAt::Remote(
                at.clone(),
                self.holder
                    .borrow()
                    .dir
                    .as_ref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| at.path.clone()),
            )),
            Shell::Program(_) | Shell::Console => None,
        }
    }

    pub fn key(&self) -> String {
        self.key.clone()
    }

    /// The name of the user's command it runs, if it runs one.
    pub fn command_name(&self) -> Option<String> {
        let run = self.run.as_ref().filter(|run| run.edits.is_none());
        run.map(|run| run.name.clone())
    }

    /// What a terminal editor's tab edits.
    pub fn edits(&self) -> Option<&Edits> {
        self.run.as_ref()?.edits.as_ref()
    }

    /// The file a terminal editor's tab edits, which the tab stands for as a text tab would.
    pub fn file(&self) -> Option<String> {
        self.edits().map(|edits| edits.file.clone())
    }

    /// Whether what it ran has ended, its tab kept saying how.
    pub fn ended(&self) -> bool {
        self.ended.get()
    }

    /// Header subtitle from VTE's `user@host:dir`, falling back to the launch directory until first reported.
    pub fn subtitle(&self) -> String {
        self.view
            .window_title()
            .map(|t| t.to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| match &self.shell {
                Shell::Local(cwd) => cwd.display().to_string(),
                Shell::Remote { at, .. } => at.host.clone(),
                Shell::Program(program) => program.cwd.clone(),
                Shell::Console => String::new(),
            })
    }

    /// Session directory from local OSC 7, falling back to the launch directory.
    /// Remote OSC 7 does not arrive over SSH; non-UTF-8 directories return `None`.
    pub fn at(&self) -> Option<String> {
        match &self.shell {
            Shell::Local(start) => {
                let told = self
                    .view
                    .current_directory_uri()
                    .and_then(|uri| dir_of(&uri, &glib::host_name()));
                told.as_deref()
                    .unwrap_or(start)
                    .to_str()
                    .map(str::to_string)
            }
            Shell::Remote { at, .. } => Some(at.to_string()),
            Shell::Program(_) | Shell::Console => None,
        }
    }

    /// Combine holder facts with tab-known fallbacks for Details.
    pub fn facts(&self) -> Facts {
        let told = self.holder.borrow().clone();
        let (host, dir, program) = match &self.shell {
            Shell::Local(_) => (glib::host_name().to_string(), self.at(), Some(user_shell())),
            Shell::Remote { at, .. } => (
                at.host.clone(),
                Some(at.path.to_string_lossy().into_owned()),
                None,
            ),
            Shell::Program(program) => (
                program
                    .link
                    .as_ref()
                    .map_or_else(|| glib::host_name().to_string(), |link| link.host.clone()),
                Some(program.cwd.clone()),
                program.args.first().cloned(),
            ),
            Shell::Console => (glib::host_name().to_string(), None, None),
        };
        Facts {
            program: told.program.or(program),
            host,
            dir: told.dir.or(dir),
            ..told
        }
    }

    /// Query holder facts off-thread and call `landed` after updating [`facts`](Self::facts).
    /// Coalesce overlapping requests into one follow-up; skip unheld or disconnected shells.
    pub fn ask(self: &Rc<Self>, landed: impl Fn(&Rc<Term>) + 'static) {
        self.ask_boxed(Box::new(landed));
    }

    fn ask_boxed(self: &Rc<Self>, landed: OnFacts) {
        let Some(argv) = self.shell.held().filter(|_| !self.lost.get()) else {
            return;
        };
        if self.asking.replace(true) {
            self.again.set(true);
            self.waiting.borrow_mut().push(landed);
            return;
        }
        let (weak, id) = (Rc::downgrade(self), id(&self.key).to_string());
        glib::spawn_future_local(async move {
            let listing = crate::work::attempt("list the held shells", move || output(&argv)).await;
            let Some(term) = weak.upgrade() else {
                return;
            };
            term.asking.set(false);
            let previous = term.holder.borrow().clone();
            *term.holder.borrow_mut() = match listing {
                Ok(listing) => facts_after_query(&previous, Some(&listing), &id),
                Err(why) => {
                    tracing::debug!("{why}");
                    facts_after_query(&previous, None, &id)
                }
            };
            landed(&term);
            if term.again.take() {
                let waiting = std::mem::take(&mut *term.waiting.borrow_mut());
                term.ask_boxed(Box::new(move |term| {
                    for callback in &waiting {
                        callback(term);
                    }
                }));
            }
        });
    }

    /// Reattach a disconnected shell, clearing old output before the holder replays its screen and history.
    /// Already-attached shells need no action.
    pub fn reopen(self: &Rc<Self>) {
        if self.lost.replace(false) {
            self.view.reset(true, true);
            start(self);
        }
    }

    /// End the shell on Close Tab; window closes only detach it for restoration. Already-ended shells are skipped.
    pub fn kill(&self) {
        if self.ended.get() {
            return;
        }
        match &self.shell {
            Shell::Local(_) => end(&self.key, &self.at().unwrap_or_default()),
            Shell::Remote { at, link } => kill_remote(Orphan {
                at: at.clone(),
                ctl: ssh::control_path(link),
                id: id(&self.key).to_string(),
            }),
            // Nothing holds a program: its pty's hangup ends it as the tab goes. A console runs
            // nothing.
            Shell::Program(_) | Shell::Console => {}
        }
    }

    /// The address whose master a remote shell rides.
    fn link(&self) -> Option<&ssh::Url> {
        match &self.shell {
            Shell::Remote { link, .. } => Some(link),
            // A program has no reconnect, so it is in no reconnect's way.
            Shell::Local(_) | Shell::Program(_) | Shell::Console => None,
        }
    }

    /// Whether it runs a debugged program rather than a shell.
    pub fn is_program(&self) -> bool {
        matches!(self.shell, Shell::Program(_))
    }

    /// Whether it is the Debug Console.
    pub fn is_console(&self) -> bool {
        matches!(self.shell, Shell::Console)
    }

    /// Whether `other` rides the same master, so that a link that dropped took both.
    pub fn shares_link(&self, other: &Term) -> bool {
        self.link().is_some() && self.link() == other.link()
    }

    pub fn restyle(&self) {
        paint(&self.view);
    }

    pub fn refont(&self) {
        self.view.set_font(Some(&monospace()));
    }

    /// Terminal-local font zoom, independent of document zoom.
    /// VTE font scaling survives restyling and font-description changes.
    pub fn zoom(&self) -> f64 {
        self.view.font_scale()
    }

    /// VTE clamps a scale to [0.25, 4.0], which contains the window's own [0.5, 3.0], so the two
    /// agree about what a zoom can be.
    pub fn set_zoom(&self, zoom: f64) {
        self.view.set_font_scale(crate::zoom::clamp_zoom(zoom));
    }

    /// What the status bar says about it, or nothing at all when the shell is at its own size.
    pub fn zoom_label(&self) -> Option<String> {
        let zoom = self.zoom();
        (zoom != 1.0).then(|| format!("{} %", (zoom * 100.0).round() as i32))
    }

    /// VTE binds neither chord itself, so `win.terminal-copy` and `win.terminal-paste` are what
    /// the accelerator table and the context menu both reach.
    pub fn copy(&self) {
        self.view.copy_clipboard_format(vte4::Format::Text);
    }

    pub fn paste(&self) {
        self.view.paste_clipboard();
    }

    pub(crate) fn share_state(&self) -> Option<bool> {
        self.share_vault
            .as_ref()
            .filter(|_| !self.lost.get() && !self.ended.get())
            .map(|_| self.holder.borrow().shared_with.is_some())
    }

    /// Change this tab's MCP access on its host, then report the holder's answer on the main loop.
    pub fn change_share(
        self: &Rc<Self>,
        share: bool,
        done: impl FnOnce(&Rc<Self>, Result<(), String>) + 'static,
    ) {
        let Some(vault) = self.share_vault.as_deref() else {
            return done(self, Err("This terminal is not on the vault's host".into()));
        };
        if self.lost.get() || self.ended.get() {
            return done(self, Err("The terminal is disconnected or ended".into()));
        }
        if self.sharing.replace(true) {
            return done(
                self,
                Err("A terminal sharing request is still running".into()),
            );
        }
        let Some(argv) = self.shell.share_command(id(&self.key), vault, share) else {
            self.sharing.set(false);
            return done(
                self,
                Err("Cannot use this terminal or vault path with accent-cli".into()),
            );
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = crate::work::attempt("change terminal MCP access", move || output(&argv))
                .await
                .map(|_| ());
            if let Some(term) = weak.upgrade() {
                term.sharing.set(false);
                done(&term, result);
            }
        });
    }
}

/// Open a shell as a tab of `tabs`, at the end or at position `at`, and start it, running `run`
/// in place of a prompt when given one.
pub fn open(
    tabs: &adw::TabView,
    at: Option<i32>,
    shell: &Shell,
    key: String,
    share_vault: Option<PathBuf>,
    run: Option<Run>,
) -> Rc<Term> {
    let view = vte4::Terminal::new();
    view.set_scrollback_lines(SCROLLBACK);
    view.set_vexpand(true);
    view.set_hexpand(true);
    view.set_margin_start(PAD);
    view.set_margin_end(PAD);
    view.set_margin_top(PAD / 2);
    // An underline rather than a block, so the character under the cursor stays readable. VTE owns
    // the blink itself; System follows GNOME's own cursor-blink setting.
    view.set_cursor_shape(vte4::CursorShape::Underline);
    view.set_cursor_blink_mode(vte4::CursorBlinkMode::System);
    let scroller = scroller(&view);

    // Before the append, which maps the widget when the pane is on screen and would otherwise fire
    // this signal before there was a handler to hear it: that was a first terminal in VTE's own
    // black while every later one came up in the theme's colours.
    view.connect_map(paint);

    let page = match at {
        // Pinned tabs come first, and nothing else may go among them.
        Some(at) => tabs.insert(&scroller, at.max(tabs.n_pinned_pages())),
        None => tabs.append(&scroller),
    };
    page.set_title(
        match (shell, &run) {
            (_, Some(run)) => run.name.clone(),
            (Shell::Local(_), None) => "Terminal".to_string(),
            (Shell::Remote { at, .. }, None) => at.host.clone(),
            (Shell::Program(program), None) => program.title.clone(),
            (Shell::Console, None) => "Debug Console".to_string(),
        }
        .as_str(),
    );
    let edits = run.as_ref().and_then(|run| run.edits.as_ref());
    page.set_icon(Some(&gio::ThemedIcon::new(match (shell, edits) {
        (_, Some(edits)) => crate::doc::icon_for(&edits.file),
        (Shell::Console, None) => "dap-console-symbolic",
        _ => "utilities-terminal-symbolic",
    })));
    // What tells the window's keys a terminal editor from a shell (`actions::Keys::at`).
    if edits.is_some() {
        view.set_widget_name(EDITOR);
    }
    // A tab appended to a visible pane is mapped already, so the signal above has been and gone.
    if view.is_mapped() {
        paint(&view);
    }

    // Weak: the page owns this view by way of the scroller, so holding it here would be a ring.
    // A command's tab keeps its name, whatever the program calls itself.
    if run.is_none() {
        view.connect_window_title_changed(glib::clone!(
            #[weak(rename_to = title)]
            page,
            move |view| {
                if let Some(text) = view.window_title().filter(|t| !t.is_empty()) {
                    title.set_title(&text);
                }
            }
        ));
    }

    install_keys(&view, tabs);
    let term = Rc::new(Term {
        key,
        page,
        view,
        shell: shell.clone(),
        lost: Cell::new(false),
        ended: Cell::new(false),
        holder: RefCell::default(),
        asking: Cell::new(false),
        again: Cell::new(false),
        waiting: RefCell::default(),
        share_vault,
        sharing: Cell::new(false),
        run,
    });
    install_links(&term);
    take_copies(&term);
    start(&term);
    term
}

fn local_git_dir(osc7: Option<PathBuf>, holder: Option<&str>, start: &Path) -> PathBuf {
    osc7.or_else(|| holder.map(PathBuf::from))
        .unwrap_or_else(|| start.to_path_buf())
}

/// Reconnect a lost shell on a non-modifier key, also restoring others on the same link.
/// Capture the key so it is not typed into the resumed shell. Ignore modifiers so Ctrl+W can
/// close without first triggering an authentication prompt.
pub fn on_reconnect(term: &Rc<Term>, reconnect: impl Fn(&Rc<Term>) + 'static) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    // Weak: the view owns this controller, and the term owns the view.
    let weak = Rc::downgrade(term);
    keys.connect_key_pressed(move |keys, _, _, _| {
        let modifier = keys
            .current_event()
            .and_then(|event| event.downcast::<gdk::KeyEvent>().ok())
            .is_some_and(|key| key.is_modifier());
        match weak.upgrade().filter(|term| term.lost.get() && !modifier) {
            Some(term) => {
                reconnect(&term);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    });
    term.view.add_controller(keys);
}

/// The termprops the shell integration VTE ships (`vte.sh`) raises as a command starts and as
/// the prompt comes back.
const SHELL_TURNS: [&str; 2] = ["vte.shell.preexec", "vte.shell.precmd"];

/// Call `changed` on title, OSC 7 directory and shell-integration command/prompt updates.
/// Use weak ownership as in [`on_exit`].
pub fn on_report(term: &Rc<Term>, changed: impl Fn(&Rc<Term>) + 'static) {
    let weak = Rc::downgrade(term);
    let report = Rc::new(move || {
        if let Some(term) = weak.upgrade() {
            changed(&term);
        }
    });
    let view = &term.view;
    view.connect_window_title_changed(glib::clone!(
        #[strong]
        report,
        move |_| report()
    ));
    view.connect_current_directory_uri_changed(glib::clone!(
        #[strong]
        report,
        move |_| report()
    ));
    // As in `take_copies`: the command and prompt turns come as termprops.
    if !cfg!(feature = "termprops") {
        return;
    }
    for name in SHELL_TURNS {
        let report = report.clone();
        view.connect_local(&format!("termprop-changed::{name}"), false, move |_| {
            report();
            None
        });
    }
}

/// The widget name of a terminal editor's view.
pub const EDITOR: &str = "accent-editor";

/// Decode a local-host OSC 7 directory URI. Ignore other hosts because their paths cannot launch local shells.
fn dir_of(uri: &str, host: &str) -> Option<PathBuf> {
    let (dir, from) = glib::filename_from_uri(uri).ok()?;
    match from.as_deref() {
        None | Some("" | "localhost") => Some(dir),
        Some(from) => (from == host).then_some(dir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OSC 7 names the host it was sent from, and a directory on another machine is no place to
    /// start a shell here.
    #[test]
    fn osc7_names_a_directory_only_on_this_machine() {
        assert_eq!(
            dir_of("file://box/home/me/src", "box"),
            Some(PathBuf::from("/home/me/src"))
        );
        // No host, or `localhost`, is this one; the path is decoded.
        assert_eq!(
            dir_of("file:///tmp/a%20b", "box"),
            Some(PathBuf::from("/tmp/a b"))
        );
        assert_eq!(
            dir_of("file://localhost/tmp", "box"),
            Some(PathBuf::from("/tmp"))
        );
        assert_eq!(dir_of("file://elsewhere/home/me", "box"), None);
        assert_eq!(dir_of("https://box/home/me", "box"), None);
    }

    #[test]
    fn local_git_prefers_live_osc7_over_cached_holder_cwd() {
        let start = Path::new("/home/me");
        let old = Some("/home/me/old-repo");
        let live = PathBuf::from("/home/me/new-repo");
        assert_eq!(local_git_dir(Some(live.clone()), old, start), live);
        assert_eq!(
            local_git_dir(None, old, start),
            PathBuf::from("/home/me/old-repo")
        );
        assert_eq!(local_git_dir(None, None, start), start);
    }
}
