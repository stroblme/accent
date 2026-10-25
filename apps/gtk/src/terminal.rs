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
//! ends it explicitly ([`Term::kill`]), and [`sweep`] ends shells no saved session names.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use accent_api::ssh;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use vte4::TerminalExt;
use vte4::TerminalExtManual;

/// Scrollback, in lines. Enough to read back a build, far short of a memory question.
const SCROLLBACK: i64 = 10_000;
/// Horizontal padding from DESIGN.md's spacing scale, retaining more terminal columns than prose gutters.
const PAD: i32 = 12;
/// Local action group for pointer-targeted URLs, which have no window shortcut or palette action.
const GROUP: &str = "link";
/// What counts as a link on the screen: an `http`, `https` or `mailto` URL, stopping before the
/// punctuation that ends a sentence rather than swallowing it.
const LINK: &str = r"(?:https?://|mailto:)[^\s<>\x22'`]*[^\s<>\x22'`.,:;!?)\]}]";
/// `PCRE2_MULTILINE`. vte4 re-exports no PCRE2 flags, so the value is written out here; a match
/// has to be able to end at a wrapped line rather than only at the end of the buffer.
const PCRE2_MULTILINE: u32 = 0x0000_0400;

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
}

/// A focused shell's live working directory and machine for Git discovery.
pub enum GitAt {
    Local(PathBuf),
    Remote(ssh::Url, PathBuf),
}

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

    fn share_state(&self) -> Option<bool> {
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
        /// The address whose ssh master it rides, which [`start`] makes ready before attaching: a
        /// remote vault's own, or the host's.
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

    fn share_command(&self, id: &str, vault: &Path, share: bool) -> Option<Vec<String>> {
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
    fn held(&self) -> Option<Vec<String>> {
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

    /// Command to fetch shell `id`'s last clipboard copy ([`take_copies`]) locally or through the host master.
    fn clip(&self, id: &str) -> Option<Vec<String>> {
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

/// The page of a shell's tab: `view` in a scroller, VTE being scrollable itself and drawing no
/// scrollbar of its own. The scroller never takes the keyboard: a tab clicked to the front asks
/// its page for it, after the tab view has given it to the shell (DESIGN.md, Terminal).
pub(crate) fn scroller(view: &vte4::Terminal) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .focusable(false)
        .child(view)
        .build()
}

/// Open a shell as a tab of `tabs`, at the end or at position `at`, and start it.
pub fn open(
    tabs: &adw::TabView,
    at: Option<i32>,
    shell: &Shell,
    key: String,
    share_vault: Option<PathBuf>,
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
        match shell {
            Shell::Local(_) => "Terminal".to_string(),
            Shell::Remote { at, .. } => at.host.clone(),
            Shell::Program(program) => program.title.clone(),
            Shell::Console => "Debug Console".to_string(),
        }
        .as_str(),
    );
    page.set_icon(Some(&gio::ThemedIcon::new(match shell {
        Shell::Console => "dap-console-symbolic",
        _ => "utilities-terminal-symbolic",
    })));
    // A tab appended to a visible pane is mapped already, so the signal above has been and gone.
    if view.is_mapped() {
        paint(&view);
    }

    // Weak: the page owns this view by way of the scroller, so holding it here would be a ring.
    view.connect_window_title_changed(glib::clone!(
        #[weak(rename_to = title)]
        page,
        move |view| {
            if let Some(text) = view.window_title().filter(|t| !t.is_empty()) {
                title.set_title(&text);
            }
        }
    ));

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
    });
    install_links(&term);
    take_copies(&term);
    start(&term);
    term
}

/// Start locally at once, or after [`accent_api::link::prepare`] installs the remote server.
/// Shell-only windows show preparation progress in-place in the tab because they have no connect bar.
fn start(term: &Rc<Term>) {
    let Shell::Remote { link, .. } = &term.shell else {
        name_unshown(term);
        return spawn(term);
    };
    term.view
        .feed(format!("Connecting to {}…", link.host).as_bytes());
    let (link, weak) = (link.clone(), Rc::downgrade(term));
    let ctl = ssh::control_path(&link);
    let view = glib::SendWeakRef::from(term.view.downgrade());
    // Set once there is an outcome. A line the worker said just before it returned can reach the
    // main loop after that, and would be drawn over the shell's first prompt.
    let over = Arc::new(AtomicBool::new(false));
    glib::spawn_future_local(async move {
        let what = format!("connect to {}", link.host);
        let listening = over.clone();
        let ready = crate::work::attempt(&what, move || {
            let say = |line: &str, _: Option<f64>| progress(&view, &listening, line);
            accent_api::link::prepare(&link, &ssh::control_path(&link), false, &say)
        })
        .await;
        over.store(true, Ordering::Relaxed);
        if ready.is_ok() {
            end_orphans(&ctl);
        }
        let Some(term) = weak.upgrade() else {
            return;
        };
        term.view.feed(CLEAR_LINE);
        match ready {
            Ok(()) => {
                name_unshown(&term);
                spawn(&term)
            }
            Err(why) => {
                term.lost.set(true);
                let line = format!("[{why}. Press a key to try again.]");
                term.view.feed(line.replace('\n', "\r\n").as_bytes());
            }
        }
    });
}

/// Title a tab whose attach waits until it is first shown (a restored one behind another) with
/// the title its shell last set, which the holder keeps: one `held` listing, where attaching would
/// replay a screen at a size the tab does not have yet ([`when_laid_out`]).
///
/// Checked from an idle: a new tab is put in front right after it opens, so it is mapped by then
/// and asks nothing. Not through [`Term::ask`], whose coalescing would drop the Details pane's own
/// callback.
fn name_unshown(term: &Rc<Term>) {
    let weak = Rc::downgrade(term);
    glib::idle_add_local_once(move || {
        let Some(term) = weak.upgrade().filter(|term| !term.view.is_mapped()) else {
            return;
        };
        let Some(argv) = term.shell.held() else {
            return;
        };
        let (weak, id) = (Rc::downgrade(&term), id(&term.key).to_string());
        glib::spawn_future_local(async move {
            let listing = crate::work::attempt("list the held shells", move || output(&argv)).await;
            let Some(term) = weak.upgrade() else {
                return;
            };
            let title = listing
                .ok()
                .and_then(|listing| told(&String::from_utf8_lossy(&listing), &id).title);
            // VTE's own wins: the tab may have been shown, and its replay have landed, meanwhile.
            if let Some(title) =
                title.filter(|_| term.view.window_title().is_none_or(|t| t.is_empty()))
            {
                term.page.set_title(&title);
            }
        });
    });
}

/// Back to the start of the line, and the line cleared.
const CLEAR_LINE: &[u8] = b"\r\x1b[2K";

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
/// [`CLIPBOARD`] signals availability; weak ownership follows [`on_exit`]. Without attach there is no signal.
fn take_copies(term: &Rc<Term>) {
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
fn output(argv: &[String]) -> std::io::Result<Vec<u8>> {
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
fn told(listing: &str, id: &str) -> Facts {
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
fn facts_after_query(previous: &Facts, listing: Option<&[u8]>, id: &str) -> Facts {
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

fn local_git_dir(osc7: Option<PathBuf>, holder: Option<&str>, start: &Path) -> PathBuf {
    osc7.or_else(|| holder.map(PathBuf::from))
        .unwrap_or_else(|| start.to_path_buf())
}

/// Draw one of [`start`]'s progress lines over the last, from the worker making the host ready:
/// through the main loop, as `pdf::render` hands a page back, and not once `over` is set.
fn progress(view: &glib::SendWeakRef<vte4::Terminal>, over: &Arc<AtomicBool>, line: &str) {
    let (view, over, line) = (view.clone(), over.clone(), format!("{line}…"));
    glib::idle_add_once(move || {
        if let Some(view) = view.upgrade().filter(|_| !over.load(Ordering::Relaxed)) {
            view.feed(CLEAR_LINE);
            view.feed(line.as_bytes());
        }
    });
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

/// Run `shell` in `view` after any previous child exits.
///
/// `accent-cli attach` starts or resumes shell `id` in the holder so it survives tab ptys and
/// window closes. Without `accent-cli`, the shell runs directly and ends with the tab.
///
/// Wait for [`when_laid_out`] before attaching: replay at VTE's default or unsettled size
/// persists until the program redraws. Restored background tabs attach when first shown;
/// new tabs wait for two stable frames. A debugged program behind another tab starts at once, at
/// VTE's default size, as the adapter is waiting for it.
fn spawn(term: &Term) {
    let (view, shell, id) = (&term.view, term.shell.clone(), id(&term.key).to_string());
    if let Shell::Console = shell {
        return;
    }
    let program = match &shell {
        Shell::Program(program) => Some(program.clone()),
        _ => None,
    };
    let behind = program.is_some() && !term.page.is_selected();
    let run = move |view: &vte4::Terminal| {
        // Made as it runs: a tab attached once first shown names the server on the host then.
        let Some((cwd, argv, env)) = command(&shell, &id) else {
            return;
        };
        let named = argv.join(" ");
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        // VTE adds these to the environment the tab would have anyway.
        let env: Vec<&str> = env.iter().map(String::as_str).collect();
        view.spawn_async(
            vte4::PtyFlags::DEFAULT,
            cwd.as_deref().and_then(Path::to_str),
            &args,
            &env,
            // A remote shell's `ssh` from `$PATH`, as the link's own is: without it VTE looks in
            // `/bin:/usr/bin` only, and an ssh installed elsewhere ran the master but not the tab.
            glib::SpawnFlags::SEARCH_PATH,
            || {},
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Some(program) = program {
                    return (program.started)(match result {
                        Ok(pid) if program.link.is_none() => Ok(Some(pid.0 as u32)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e.to_string()),
                    });
                }
                if let Err(e) = result {
                    tracing::warn!("cannot start {named}: {e}");
                }
            },
        );
    };
    match behind {
        true => run(view),
        false => when_laid_out(view, run),
    }
}

/// The working directory, command line and added environment that run `shell`, the held shell
/// `id`; `None` for the console, which runs nothing.
fn command(shell: &Shell, id: &str) -> Option<(Option<PathBuf>, Vec<String>, Vec<String>)> {
    Some(match (shell, cli()) {
        (Shell::Local(cwd), Some(mut argv)) => {
            argv.extend([
                "attach".to_string(),
                "--cwd".to_string(),
                cwd.to_string_lossy().into_owned(),
                "--owner".to_string(),
                owner(),
                id.to_string(),
            ]);
            (None, argv, Vec::new())
        }
        (Shell::Local(cwd), None) => (Some(cwd.clone()), vec![user_shell()], Vec::new()),
        // ssh decides where it lands, and a cwd on this machine means nothing to it.
        (Shell::Remote { at, link }, _) => {
            let server = ssh::server_path(&accent_api::link::prepared(at).ok()?);
            let argv = ssh::attach(at, &ssh::control_path(link), &server, id);
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

/// Run `then` once `view` is mapped with a stable allocation, or immediately if already laid out.
///
/// Two equal-sized frames let restored split ratios settle before shell replay. GTK4 has no
/// allocation signal, so mapped-only ticks detect stability without work on background tabs.
/// The callback receives the view rather than capturing it, avoiding a reference cycle.
fn when_laid_out(view: &vte4::Terminal, then: impl FnOnce(&vte4::Terminal) + 'static) {
    if view.is_mapped() && view.height() > 0 {
        return then(view);
    }
    let then = Rc::new(Cell::new(Some(then)));
    let wait = move |view: &vte4::Terminal| {
        let (then, last) = (then.clone(), Cell::new((0, 0)));
        view.add_tick_callback(move |view, _| {
            // Hidden again first: the next map waits afresh.
            if !view.is_mapped() {
                return glib::ControlFlow::Break;
            }
            let size = (view.width(), view.height());
            if size.1 == 0 || last.replace(size) != size {
                return glib::ControlFlow::Continue;
            }
            if let Some(then) = then.take() {
                then(view);
            }
            glib::ControlFlow::Break
        });
    };
    if view.is_mapped() {
        wait(view);
    }
    view.connect_map(wait);
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

/// Notify the caller of shell exit so it can close the tab.
///
/// Remote disconnects retain the tab for [`Term::reopen`]; local exit 255 is ordinary exit.
/// Failed `accent-cli attach` tabs also remain so their error can be read. Ctrl+W closes either.
pub fn on_exit(
    term: &Rc<Term>,
    done: impl Fn(&Rc<Term>) + 'static,
    lost: impl Fn(&Rc<Term>) + 'static,
) {
    let weak = Rc::downgrade(term);
    term.view.connect_child_exited(move |view, status| {
        let Some(term) = weak.upgrade() else {
            return;
        };
        match &term.shell {
            // Kept, saying how it ended: its output is what was being debugged.
            Shell::Program(program) => {
                term.ended.set(true);
                // On a line of its own, after a blank one: a program that ended mid-line gets both.
                let lead = match view.cursor_position().0 {
                    0 => "\r\n",
                    _ => "\r\n\r\n",
                };
                let line = exit_line(status, program.link.is_some());
                view.feed(format!("{lead}{line}\r\n").as_bytes());
            }
            Shell::Remote { at, .. } if link_lost(status) => {
                term.lost.set(true);
                let line = format!(
                    "\r\n[Lost the connection to {}. Press a key to reconnect.]\r\n",
                    at.host
                );
                view.feed(line.as_bytes());
                lost(&term);
            }
            _ => {
                term.ended.set(true);
                if !exited_with(status, ATTACH_FAILED) {
                    done(&term);
                }
            }
        }
    });
}

/// Attach failure exit code for an unreachable holder or shell startup failure.
/// A shell explicitly exiting 254 also retains its tab until Ctrl+W.
const ATTACH_FAILED: i32 = 254;

/// Whether SSH exited with 255, indicating connection failure rather than normal remote status.
/// A remote shell's own exit 255 is indistinguishable and may start a new held shell on reconnect.
fn link_lost(status: i32) -> bool {
    exited_with(status, 255)
}

/// How a debugged program ended, from its wait status: on a host ssh's own 255 is the link going,
/// not the program.
fn exit_line(status: i32, remote: bool) -> String {
    match (status & 0x7f, (status >> 8) & 0xff) {
        (0, 255) if remote => "[Lost the connection to host]".to_string(),
        (0, code) => format!("[Exited with code {code}]"),
        (signal, _) => format!("[Ended by signal {signal}]"),
    }
}

/// Whether a child exited with `code`. `status` is the wait status VTE passes on, as `waitpid`
/// gave it: an exit is a zero signal byte, and its code is the byte above.
fn exited_with(status: i32, code: i32) -> bool {
    status & 0x7f == 0 && (status >> 8) & 0xff == code
}

/// Handle tab navigation before the terminal turns it into shell input.
///
/// AdwTabView's bubble-phase Page Up/Down handling is too late for VTE. Ctrl+Tab stays with
/// window MRU actions; terminal Copy and Paste stay in the rebindable action table.
fn install_keys(view: &vte4::Terminal, tabs: &adw::TabView) {
    let keys = gtk::ShortcutController::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);

    let add = |accel: &str, action: gtk::CallbackAction| {
        if let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) {
            keys.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
        }
    };
    for (accel, next) in [("<Control>Page_Up", false), ("<Control>Page_Down", true)] {
        let tabs = tabs.clone();
        add(
            accel,
            gtk::CallbackAction::new(move |_, _| {
                match next {
                    true => tabs.select_next_page(),
                    false => tabs.select_previous_page(),
                };
                glib::Propagation::Stop
            }),
        );
    }
    view.add_controller(keys);
}

/// Install terminal menus and safe output-link handling.
/// Only `launchable` schemes reach the desktop; action parameters retain the clicked URL
/// even if later pointer motion changes the hover target.
fn install_links(term: &Rc<Term>) {
    let view = &term.view;
    // OSC 8, for the programs that mark their own links instead of printing a bare URL.
    view.set_allow_hyperlink(true);
    match vte4::Regex::for_match(LINK, PCRE2_MULTILINE) {
        Ok(regex) => {
            let tag = view.match_add_regex(&regex, 0);
            view.match_set_cursor_name(tag, "pointer");
        }
        Err(e) => tracing::warn!("terminal links are off, the pattern did not compile: {e}"),
    }
    view.insert_action_group(GROUP, Some(&link_actions(view)));

    let menu = gio::Menu::new();
    fill_menu(&menu, None, term.share_state());
    view.set_context_menu_model(Some(&menu));

    // Capture phase, so the model is rebuilt before VTE reads it to pop the menu up.
    let secondary = gtk::GestureClick::new();
    secondary.set_button(gdk::BUTTON_SECONDARY);
    secondary.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(term);
    secondary.connect_pressed(glib::clone!(
        #[weak(rename_to = under)]
        view,
        move |_, _, x, y| {
            let Some(term) = weak.upgrade() else { return };
            let link = link_at(&under, x, y);
            fill_menu(&menu, link.as_deref(), term.share_state());
            if term.share_vault.is_some() {
                let menu = menu.clone();
                term.ask(move |term| fill_menu(&menu, link.as_deref(), term.share_state()));
            }
        }
    ));
    view.add_controller(secondary);

    // Ctrl and the primary button, which is what GNOME Terminal asks for and what a PDF link here
    // already asks for. A plain click cannot have it: VTE claims the sequence for its own
    // selection, which cancels any gesture of ours before the release, and a press that both
    // starts a selection and launches a browser is the wrong gesture in any case.
    let primary = gtk::GestureClick::new();
    primary.set_button(gdk::BUTTON_PRIMARY);
    primary.set_propagation_phase(gtk::PropagationPhase::Capture);
    primary.connect_pressed(glib::clone!(
        #[weak(rename_to = under)]
        view,
        move |gesture, _, x, y| {
            if gesture
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
                && let Some(uri) = link_at(&under, x, y)
            {
                launch(&uri);
            }
        }
    ));
    view.add_controller(primary);
}

/// What the menu holds: the clipboard, then the tab, then the link under the pointer when there
/// is one. No Select All — VTE binds nothing for it — and no Split, which is a tab gesture the
/// tab strip already offers.
fn fill_menu(menu: &gio::Menu, link: Option<&str>, shared: Option<bool>) {
    menu.remove_all();
    let clipboard = gio::Menu::new();
    for action in ["win.terminal-copy", "win.terminal-paste"] {
        clipboard.append(Some(crate::actions::label_of(action)), Some(action));
    }
    menu.append_section(None, &clipboard);
    let tab = gio::Menu::new();
    tab.append(
        Some(crate::actions::label_of("win.terminal")),
        Some("win.terminal"),
    );
    if let Some(shared) = shared {
        let action = if shared {
            "win.terminal-revoke"
        } else {
            "win.terminal-share"
        };
        tab.append(Some(crate::actions::label_of(action)), Some(action));
    }
    tab.append(
        Some(crate::actions::label_of("win.close-tab")),
        Some("win.close-tab"),
    );
    menu.append_section(None, &tab);
    let Some(uri) = link else {
        return;
    };
    let links = gio::Menu::new();
    for (label, action) in [("Open Link", "open"), ("Copy Link Address", "copy")] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(
            Some(&format!("{GROUP}.{action}")),
            Some(&uri.to_variant()),
        );
        links.append_item(&item);
    }
    menu.append_section(None, &links);
}

/// The two link items' actions, each taking the URL as its parameter, as the tree's row menu does
/// with a path: a detailed-action string would have to quote it.
fn link_actions(view: &vte4::Terminal) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let open = gio::SimpleAction::new("open", Some(glib::VariantTy::STRING));
    open.connect_activate(|_, target| {
        if let Some(uri) = target.and_then(|t| t.str()) {
            launch(uri);
        }
    });
    group.add_action(&open);
    let copy = gio::SimpleAction::new("copy", Some(glib::VariantTy::STRING));
    copy.connect_activate(glib::clone!(
        #[weak]
        view,
        move |_, target| {
            if let Some(uri) = target.and_then(|t| t.str()) {
                view.clipboard().set_text(uri);
            }
        }
    ));
    group.add_action(&copy);
    group
}

/// Return the permitted URL at `(x, y)`, preferring OSC 8 links over regex matches.
/// Filter before building the menu so unsupported schemes never appear.
fn link_at(view: &vte4::Terminal, x: f64, y: f64) -> Option<String> {
    view.check_hyperlink_at(x, y)
        .or_else(|| view.check_match_at(x, y).0)
        .map(|uri| uri.to_string())
        .filter(|uri| launchable(uri))
}

/// Hand a URL to the desktop.
fn launch(uri: &str) {
    if let Err(e) = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE) {
        tracing::warn!("cannot open {uri}: {e}");
    }
}

/// Allow only approved URL schemes from untrusted terminal output.
/// Reject file, JavaScript and other registered helper schemes regardless of regex or OSC 8 claims.
fn launchable(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto"
        )
}

/// Set foreground, background and all sixteen ANSI colours from `theme.rs`.
///
/// VTE's named CSS colours bypass Solarized variables, and its default palette has poor
/// contrast on our backgrounds. A shared palette makes restyling consistent and readable.
fn paint(view: &vte4::Terminal) {
    let dark = adw::StyleManager::default().is_dark();
    let fg = gdk::RGBA::parse(crate::theme::view_fg(dark)).ok();
    let bg = gdk::RGBA::parse(crate::theme::view_bg(dark)).ok();
    // VTE asserts on a palette that is neither empty nor exactly 8, 16, 232 or 256 long, so a
    // colour that failed to parse drops the whole palette back to VTE's default rather than
    // shortening this one.
    let parsed: Vec<gdk::RGBA> = crate::theme::terminal_palette(dark)
        .iter()
        .filter_map(|c| gdk::RGBA::parse(*c).ok())
        .collect();
    let palette: Vec<&gdk::RGBA> = match parsed.len() == 16 {
        true => parsed.iter().collect(),
        false => Vec::new(),
    };
    view.set_colors(fg.as_ref(), bg.as_ref(), &palette);
    view.set_font(Some(&monospace()));
}

fn monospace() -> pango::FontDescription {
    pango::FontDescription::from_string(&crate::platform::monospace_font())
}

/// Use `$SHELL` or `/bin/sh`; vte4 0.10 does not bind password-database `vte_get_user_shell`.
fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// What a terminal's key starts with. Not a path, so nothing that walks the open documents by path
/// can collide with one.
const KEY: &str = "terminal:";

/// Generate a sixteen-hex-digit shell key, unique across windows, saved sessions and machines.
pub fn new_key() -> String {
    format!("{KEY}{:08x}{:08x}", glib::random_int(), glib::random_int())
}

/// A debugged program's tab key: never a held shell's, so neither the holder nor a session
/// restore takes it for one.
pub fn program_key() -> String {
    format!(
        "program:{:08x}{:08x}",
        glib::random_int(),
        glib::random_int()
    )
}

/// The Debug Console's tab key: one per window, and never a held shell's.
pub fn console_key() -> String {
    "debug:console".to_string()
}

/// Whether a session's key names a shell rather than a file.
pub fn is_key(key: &str) -> bool {
    key.strip_prefix(KEY)
        .is_some_and(|id| id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// What a saved terminal session's key starts with: `terminal://<name>`, the way a remote vault's
/// is `ssh://…`, so the recent list, the state file and the command line all take it as they are.
pub const SESSION: &str = "terminal://";

/// The key the terminal session `name` is remembered by.
pub fn session_key(name: &str) -> PathBuf {
    PathBuf::from(format!("{SESSION}{name}"))
}

/// The name in a terminal session's key: `None` for any other key, and for a name that is empty
/// or holds a `/`.
pub fn session_name(key: &Path) -> Option<&str> {
    key.to_str()?
        .strip_prefix(SESSION)
        .filter(|name| !name.is_empty() && !name.contains('/'))
}

/// The name the holder knows the shell by: its key without the prefix.
fn id(key: &str) -> &str {
    key.strip_prefix(KEY).unwrap_or(key)
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
/// falls back to an unheld sandbox shell. Pass VTE's environment values to the host holder.
fn host_cli(info: &str) -> Option<Vec<String>> {
    let file = glib::KeyFile::new();
    file.load_from_data(info, glib::KeyFileFlags::NONE).ok()?;
    let policy = file
        .string("Session Bus Policy", "org.freedesktop.Flatpak")
        .ok()?;
    if !matches!(policy.as_str(), "talk" | "own") {
        return None;
    }
    let app = file.string("Instance", "app-path").ok()?;
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
        format!("{app}/lib/accent/accent-cli-{}", std::env::consts::ARCH),
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
fn owner() -> String {
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
struct Orphan {
    at: ssh::Url,
    ctl: PathBuf,
    id: String,
}

/// End `orphan` with the server last made ready on its host ([`accent_api::link::prepared`]), its
/// command made now rather than when the tab opened: a reconnect since may have put another
/// build there and swept the one before.
fn kill_remote(orphan: Orphan) {
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

    #[test]
    fn a_terminal_key_is_never_a_path() {
        let key = new_key();
        assert!(is_key(&key), "{key}");
        // Sixteen hex digits after the prefix, and a fresh one each time.
        assert_eq!(key.len(), "terminal:".len() + 16);
        assert_ne!(key, new_key());
        // Not loose, which is what would send it through the file machinery.
        assert!(!crate::doc::is_loose_key(&key));
        // A note that merely starts the same way is still a note.
        assert!(!is_key("terminal:notes.md"));
    }

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

    #[test]
    fn a_session_key_reads_back_as_its_name() {
        assert_eq!(session_key("dev"), PathBuf::from("terminal://dev"));
        assert_eq!(session_name(&session_key("dev")), Some("dev"));
        assert_eq!(session_name(&session_key("my work")), Some("my work"));
        // No name, a name that would be a path, and every other kind of key.
        assert_eq!(session_name(Path::new("terminal://")), None);
        assert_eq!(session_name(Path::new("terminal://a/b")), None);
        assert_eq!(session_name(Path::new("ssh://box/srv/vault")), None);
        assert_eq!(session_name(Path::new("/home/me/Notes")), None);
    }

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

    #[test]
    fn terminal_menu_offers_only_the_current_sharing_action() {
        let menu = gio::Menu::new();
        let actions = |menu: &gio::Menu| {
            let tab = menu.item_link(1, "section").unwrap();
            (0..tab.n_items())
                .filter_map(|i| tab.item_attribute_value(i, "action", None))
                .filter_map(|action| action.str().map(str::to_string))
                .collect::<Vec<_>>()
        };
        fill_menu(&menu, None, None);
        assert_eq!(actions(&menu), ["win.terminal", "win.close-tab"]);
        fill_menu(&menu, None, Some(false));
        assert_eq!(
            actions(&menu),
            ["win.terminal", "win.terminal-share", "win.close-tab"]
        );
        fill_menu(&menu, None, Some(true));
        assert_eq!(
            actions(&menu),
            ["win.terminal", "win.terminal-revoke", "win.close-tab"]
        );
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

    /// VTE hands over the wait status, so ssh's 255 arrives as 255 << 8.
    #[test]
    fn only_ssh_ending_itself_is_a_lost_link() {
        assert!(link_lost(255 << 8));
        // The remote shell's own exit, clean or not.
        assert!(!link_lost(0));
        assert!(!link_lost(1 << 8));
        // Killed by a signal, whatever the byte above says.
        assert!(!link_lost((255 << 8) | 9));
    }

    /// A debugged program's tab says how it ended, from the wait status VTE hands over.
    #[test]
    fn a_program_says_how_it_ended() {
        assert_eq!(exit_line(0, false), "[Exited with code 0]");
        assert_eq!(exit_line(3 << 8, false), "[Exited with code 3]");
        assert_eq!(exit_line(15, false), "[Ended by signal 15]");
        // On a host, ssh's own 255 is the link, not the program.
        assert_eq!(exit_line(255 << 8, true), "[Lost the connection to host]");
        assert_eq!(exit_line(255 << 8, false), "[Exited with code 255]");
    }

    /// The shell's output is untrusted, so this is the one that has to be a list.
    #[test]
    fn only_a_browser_or_a_mail_client_is_ever_launched() {
        assert!(launchable("https://example.org/x"));
        assert!(launchable("http://example.org"));
        assert!(launchable("mailto:someone@example.org"));
        // Case is the URL's, not ours.
        assert!(launchable("HTTPS://example.org"));
        // Anything a shell could print to reach the filesystem or run something.
        assert!(!launchable("file:///etc/passwd"));
        assert!(!launchable("javascript:alert(1)"));
        assert!(!launchable("ssh://box/x"));
        assert!(!launchable("smb://share"));
        // Not a URL at all.
        assert!(!launchable("https:"));
        assert!(!launchable("example.org"));
    }
}
