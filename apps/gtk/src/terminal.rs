//! A shell in a tab.
//!
//! A terminal is a document like any other: it lives in a pane's tab view, so it splits, drags
//! between panes and windows, and takes the tab context menu with it. There is no terminal panel
//! and no terminal-shaped hole in the layout — a shell at the bottom of the window is a pane split
//! downwards, which is the same gesture that puts a note there.
//!
//! A focused shell owns the keyboard, and it wins by default: the window keeps only Close Tab
//! (`Ctrl+W`), New Terminal, Go to File (`Ctrl+E`), the three zoom chords, Fullscreen and the
//! `Ctrl+Shift` half of the action table, and everything else — `Ctrl+C`, `Ctrl+D`, `Ctrl+K`,
//! `Ctrl+L`, `Ctrl+R` and the rest of readline — reaches the shell; with Forward All Keys on it
//! keeps Copy and Paste in Terminal alone. The window is where that happens, not here: GTK
//! dispatches a window's application accelerators ahead of the VTE, so nothing a controller on
//! this widget claims can beat them, and `Shell::apply_accels` in `main` unbinds the rest of the
//! table for as long as the active window's focus is a terminal (`actions::kept`, `has_focus`).
//!
//! Nothing hung on the shell's widgets may hold them: the page owns the scroller, the scroller owns
//! the view, so a strong reference captured by a signal handler, a gesture or an action group the
//! view itself carries closes a ring that closing the tab cannot cut. GTK4 finalises a widget by
//! reference count alone — there is no `destroy` to break one from outside — and a `VteTerminal`
//! that is never finalised never drops its `VtePty`, so the pty master stays open and the shell
//! never gets its hangup. That is how every closed terminal tab used to leak a live shell.
//!
//! A shell is held by `accent-cli` now, and what runs in the tab's pty is `accent-cli attach`: a
//! hangup detaches it and leaves the shell running for the next window to take up, and Close Tab
//! ends it explicitly ([`Term::kill`]). A leaked view is an attach that never lets go. What a
//! window leaves held without a session to name it in is ended at the next start ([`sweep`]).

use std::cell::Cell;
use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use accent_api::ssh;
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use vte4::TerminalExt;
use vte4::TerminalExtManual;

/// Scrollback, in lines. Enough to read back a build, far short of a memory question.
const SCROLLBACK: i64 = 10_000;
/// Breathing room either side of the shell, off DESIGN.md's spacing scale. Not the editor's 48 px
/// page gutter: that is a measure for prose, and a terminal is a grid that should keep its columns.
const PAD: i32 = 12;
/// The action group the two link items live in. They are not window actions: what they act on is
/// the URL under the pointer, so there is nothing for a chord or a palette entry to name.
const GROUP: &str = "link";
/// What counts as a link on the screen: an `http`, `https` or `mailto` URL, stopping before the
/// punctuation that ends a sentence rather than swallowing it.
const LINK: &str = r"(?:https?://|mailto:)[^\s<>\x22'`]*[^\s<>\x22'`.,:;!?)\]}]";
/// `PCRE2_MULTILINE`. vte4 re-exports no PCRE2 flags, so the value is written out here; a match
/// has to be able to end at a wrapped line rather than only at the end of the buffer.
const PCRE2_MULTILINE: u32 = 0x0000_0400;

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
}

impl Term {
    pub fn key(&self) -> String {
        self.key.clone()
    }

    /// What the header says under the window's name. VTE reports `user@host:dir` a moment after
    /// the shell has started and again on every `cd`; until the first of those, where the shell
    /// was started is the closest thing to the same answer.
    pub fn subtitle(&self) -> String {
        self.view
            .window_title()
            .map(|t| t.to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| match &self.shell {
                Shell::Local(cwd) => cwd.display().to_string(),
                Shell::Remote { at, .. } => at.host.clone(),
            })
    }

    /// Where the shell is, as the session writes it down: the directory it last reported through
    /// OSC 7 on this machine, or the one it was started in until it has reported one. OSC 7 does
    /// not arrive over ssh, so a remote shell is where it was opened. `None` for a directory whose
    /// name is not UTF-8, which a session file cannot hold.
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
        }
    }

    /// Attach again to the shell a dropped link cut this tab off from. The host still holds it and
    /// replays its screen, history and all, so the old one is cleared first rather than shown
    /// twice. Nothing to do for a shell that is still attached.
    pub fn reopen(self: &Rc<Self>) {
        if self.lost.replace(false) {
            self.view.reset(true, true);
            start(self);
        }
    }

    /// End the shell for good, which is what Close Tab means: closing a window only lets go of
    /// its shells, and they are still there when it opens again. Nothing to do for one that has
    /// ended already.
    pub fn kill(&self) {
        if self.ended.get() {
            return;
        }
        match &self.shell {
            Shell::Local(_) => end(&self.key, &self.at().unwrap_or_default()),
            Shell::Remote { kill, .. } => run_kill(kill.clone()),
        }
    }

    pub fn restyle(&self) {
        paint(&self.view);
    }

    pub fn refont(&self) {
        self.view.set_font(Some(&monospace()));
    }

    /// How far this shell is zoomed. A terminal carries its own: it is a grid of columns, not a
    /// page of prose, so it does not follow the document font the way the editor and the preview
    /// do. VTE scales the font it was given, and `paint` only ever sets the description, so a
    /// scale survives a restyle and a font change.
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
}

/// Whether the keyboard is in a shell right now: whether the focus widget of the window that has
/// it is a `vte4::Terminal`, a leaf widget whose own focus is the whole question. Asked of the
/// application rather than of a window because the accelerator table `Shell::apply_accels`
/// narrows on the answer is the application's, and the window rebuilding it is not always the
/// one with the keyboard.
pub fn has_focus(gtk_app: &gtk::Application) -> bool {
    gtk_app
        .active_window()
        .and_then(|window| gtk::prelude::GtkWindowExt::focus(&window))
        .is_some_and(|widget| widget.is::<vte4::Terminal>())
}

/// What a shell tab runs.
///
/// A remote vault's shell belongs on the remote: the files are there, the repository is there,
/// and a build the user starts in it has to see them. It is an ordinary tab either way — the
/// same split, the same drag, the same chords — because it is the same widget with a different
/// argument vector.
#[derive(Clone)]
pub enum Shell {
    /// The user's own shell, in a directory on this machine.
    Local(PathBuf),
    /// A shell held on a host by the server this build uploads there, attached over `ssh -t`:
    /// see [`Shell::remote`].
    Remote {
        argv: Vec<String>,
        /// What ends it for good, run on this machine.
        kill: Vec<String>,
        /// Where it was opened: the host, and the directory on it.
        at: ssh::Url,
        /// The address whose ssh master it rides, which [`start`] makes ready before attaching: a
        /// remote vault's own, or the host's.
        link: ssh::Url,
    },
}

impl Shell {
    /// The shell `key` names, held on the host `at` names and started in its path, riding the
    /// master of `link`.
    pub fn remote(at: ssh::Url, link: ssh::Url, key: &str) -> Result<Self, String> {
        let server = ssh::server_path(&accent_api::link::server()?.hash);
        let ctl = ssh::control_path(&link);
        Ok(Self::Remote {
            argv: ssh::attach(&at, &ctl, &server, id(key)),
            kill: ssh::run(&at, &ctl, &ssh::kill_cmd(&server, id(key))),
            at,
            link,
        })
    }

    /// What prints the last copy a program in shell `id` made ([`take_copies`]), run on this
    /// machine: `accent-cli clip`, here or over the master on the host.
    fn clip(&self, id: &str) -> Option<Vec<String>> {
        match self {
            Shell::Local(_) => {
                let cli = cli()?.to_string_lossy().into_owned();
                Some(vec![cli, "clip".to_string(), id.to_string()])
            }
            Shell::Remote { at, link, .. } => {
                let server = ssh::server_path(&accent_api::link::server().ok()?.hash);
                let ctl = ssh::control_path(link);
                Some(ssh::run(at, &ctl, &ssh::clip_cmd(&server, id)))
            }
        }
    }
}

/// Open a shell as a tab of `tabs`, and start it.
pub fn open(tabs: &adw::TabView, shell: &Shell, key: String) -> Rc<Term> {
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
    // VTE is scrollable itself and draws no scrollbar of its own.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&view)
        .build();

    // Before the append, which maps the widget when the pane is on screen and would otherwise fire
    // this signal before there was a handler to hear it: that was a first terminal in VTE's own
    // black while every later one came up in the theme's colours.
    view.connect_map(paint);

    let page = tabs.append(&scroller);
    page.set_title(
        match shell {
            Shell::Local(_) => "Terminal".to_string(),
            Shell::Remote { at, .. } => at.host.clone(),
        }
        .as_str(),
    );
    page.set_icon(Some(&gio::ThemedIcon::new("utilities-terminal-symbolic")));
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
    install_links(&view);

    let term = Rc::new(Term {
        key,
        page,
        view,
        shell: shell.clone(),
        lost: Cell::new(false),
        ended: Cell::new(false),
    });
    reattach_on_key(&term);
    take_copies(&term);
    start(&term);
    term
}

/// Run the shell once there is somewhere to run it: on this machine at once, and on a host once
/// [`accent_api::link::prepare`] has a master up and the server installed there. A window of
/// shells has no connect bar, so the tab's own screen says how far that has got, on one line
/// written over in place.
fn start(term: &Rc<Term>) {
    let Shell::Remote { link, .. } = &term.shell else {
        return spawn(&term.view, &term.shell, id(&term.key));
    };
    term.view
        .feed(format!("Connecting to {}…", link.host).as_bytes());
    let (link, weak) = (link.clone(), Rc::downgrade(term));
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
        let Some(term) = weak.upgrade() else {
            return;
        };
        term.view.feed(CLEAR_LINE);
        match ready {
            Ok(()) => spawn(&term.view, &term.shell, id(&term.key)),
            Err(why) => {
                term.lost.set(true);
                let line = format!("[{why}. Press a key to try again.]");
                term.view.feed(line.replace('\n', "\r\n").as_bytes());
            }
        }
    });
}

/// Back to the start of the line, and the line cleared.
const CLEAR_LINE: &[u8] = b"\r\x1b[2K";

/// The termprop `accent-cli attach` raises when a program in the shell has copied something with
/// OSC 52, which VTE does not do itself (`hold::clip` in `accent-cli`).
const CLIPBOARD: &str = "vte.ext.accent.clipboard";

/// Make [`CLIPBOARD`] known to VTE, which takes a termprop only before the first terminal exists.
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

/// Put what a program in the shell copied on the clipboard: `attach` has kept the copy beside the
/// holder, here or on the host, and raised [`CLIPBOARD`]; `accent-cli clip` hands it over as
/// base64, off the main loop since on a host it is a round trip. Weak, for the reason [`on_exit`]
/// is. Without `accent-cli` there is no `attach` to raise it.
fn take_copies(term: &Rc<Term>) {
    let weak = Rc::downgrade(term);
    let signal = format!("termprop-changed::{CLIPBOARD}");
    term.view.connect_local(&signal, false, move |_| {
        let term = weak.upgrade()?;
        let argv = term.shell.clip(id(&term.key))?;
        let clipboard = term.view.clipboard();
        glib::spawn_future_local(async move {
            let copy = crate::work::attempt("take the terminal's copy", move || {
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
            })
            .await;
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

/// Bring a lost shell back on a key press: a window of shells has no banner to press Reconnect
/// on, and the tab is where the reader is looking. Not on a modifier alone, so the Ctrl of a
/// Ctrl+W that gives up on the tab does not dial out first — which may raise a passphrase dialog.
/// Capture phase, so the key that asks is not also typed into the shell it brings back.
fn reattach_on_key(term: &Rc<Term>) {
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
                term.reopen();
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    });
    term.view.add_controller(keys);
}

/// Run `shell` in `view`, which may have run one before: VTE takes a new child once the last has
/// exited.
///
/// A local one is `accent-cli attach`, which starts the shell `id` names in the holder, or finds
/// it there still running, and relays it into this pty: so the shell outlives the tab's pty and a
/// closed window, and comes back when its tab does. Without `accent-cli` it is the shell itself,
/// ending with its tab, as it always used to.
///
/// Not before the view is laid out ([`when_laid_out`]): the holder redraws a shell it kept at the
/// size the attach finds its pty at, and until then that is VTE's default of 24 rows, or for a
/// restored tab behind another no size at all. A screen replayed at the wrong size stays that way
/// until the program in it redraws. So a restored tab behind another attaches when it is first
/// shown, and is called "Terminal" until then; a new shell starts two frames later than it could.
fn spawn(view: &vte4::Terminal, shell: &Shell, id: &str) {
    let (cwd, argv) = match (shell, cli()) {
        (Shell::Local(cwd), Some(cli)) => (
            None,
            vec![
                cli.to_string_lossy().into_owned(),
                "attach".to_string(),
                "--cwd".to_string(),
                cwd.to_string_lossy().into_owned(),
                id.to_string(),
            ],
        ),
        (Shell::Local(cwd), None) => (Some(cwd.clone()), vec![user_shell()]),
        // ssh decides where it lands, and a cwd on this machine means nothing to it.
        (Shell::Remote { argv, .. }, _) => (None, argv.clone()),
    };
    when_laid_out(view, move |view| {
        let named = argv.join(" ");
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        view.spawn_async(
            vte4::PtyFlags::DEFAULT,
            cwd.as_deref().and_then(Path::to_str),
            &args,
            &[],
            // A remote shell's `ssh` from `$PATH`, as the link's own is: without it VTE looks in
            // `/bin:/usr/bin` only, and an ssh installed elsewhere ran the master but not the tab.
            glib::SpawnFlags::SEARCH_PATH,
            || {},
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(e) = result {
                    tracing::warn!("cannot start {named}: {e}");
                }
            },
        );
    });
}

/// Run `then` once `view` is on screen at the size it will keep: at once if it is laid out
/// already, else once it is shown and two frames in a row have given it the same size. The
/// second frame is for a restored split, whose handles move for a few frames before they settle
/// (`session::hold_ratios`): a shell attached at the first of those sizes can be a dozen columns
/// wide. GTK 4 has no signal for an allocation, so a tick callback looks each frame, but only
/// while the view is mapped: a tab that stays behind another costs nothing while it waits. The
/// view comes in as an argument, as nothing hung on it may hold it.
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

/// Run `changed` whenever VTE reports a title of its own: once a moment after the shell has
/// started, and again on every `cd`. Weak, for the reason [`on_exit`] is.
pub fn on_title(term: &Rc<Term>, changed: impl Fn(&Rc<Term>) + 'static) {
    let weak = Rc::downgrade(term);
    term.view.connect_window_title_changed(move |_| {
        if let Some(term) = weak.upgrade() {
            changed(&term);
        }
    });
}

/// The shell exited: hand the terminal back so the caller can close its tab.
///
/// Except a remote shell whose link went, which the host still holds. Its tab stays, saying so,
/// and [`Term::reopen`] attaches again on a key press, or when a remote vault's window is back;
/// closing the tab meanwhile is what ends it. A local command's 255 is an ordinary exit. And
/// except an `accent-cli attach` that could not do its job, whose tab stays so the line it printed
/// can be read; `Ctrl+W` closes it.
pub fn on_exit(term: &Rc<Term>, done: impl Fn(&Rc<Term>) + 'static) {
    let weak = Rc::downgrade(term);
    term.view.connect_child_exited(move |view, status| {
        let Some(term) = weak.upgrade() else {
            return;
        };
        match &term.shell {
            Shell::Remote { at, .. } if link_lost(status) => {
                term.lost.set(true);
                let line = format!(
                    "\r\n[Lost the connection to {}. Press a key to reconnect.]\r\n",
                    at.host
                );
                view.feed(line.as_bytes());
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

/// What `accent-cli attach` exits with when it could not reach the holder or start the shell,
/// rather than with the status of a shell it carried.
///
/// ponytail: a shell's own `exit 254` keeps its tab the same way, until `Ctrl+W`.
const ATTACH_FAILED: i32 = 254;

/// Whether ssh ended itself rather than the shell it carried: it exits 255 on its own errors, a
/// dropped link among them, and with the remote command's status otherwise.
///
/// ponytail: a remote `exit 255` reads as a lost link too, and keeps its tab until a key press
/// attaches to a shell the host no longer holds — which starts a new one — or a close.
fn link_lost(status: i32) -> bool {
    exited_with(status, 255)
}

/// Whether a child exited with `code`. `status` is the wait status VTE passes on, as `waitpid`
/// gave it: an exit is a zero signal byte, and its code is the byte above.
fn exited_with(status: i32, code: i32) -> bool {
    status & 0x7f == 0 && (status >> 8) & 0xff == code
}

/// What the terminal answers itself: moving between tabs, which `AdwTabView` binds at the window
/// in the bubble phase — too late, because the shell has already turned the chord into an escape
/// sequence by then. Copy and paste used to be here as hard-coded callbacks; they are
/// `win.terminal-copy` and `win.terminal-paste` now, so they rebind and list in the palette.
///
/// Page Up and Page Down only. Ctrl+Tab is the window's reserved `win.next-tab`, which is
/// dispatched before any controller of ours and walks the pane's most-recent order rather than
/// the bar's — so a copy here never ran, and would have stepped the wrong way if it had.
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

/// The shell's own context menu, the links in its output, and the click that follows one.
///
/// A shell writes whatever it likes to its own screen, so nothing here is trusted: only the three
/// schemes `launchable` names ever reach the desktop, and a URL travels as a menu item's target
/// rather than through a cell a second click could have moved under it.
fn install_links(view: &vte4::Terminal) {
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
    fill_menu(&menu, None);
    view.set_context_menu_model(Some(&menu));

    // Capture phase, so the model is rebuilt before VTE reads it to pop the menu up.
    let secondary = gtk::GestureClick::new();
    secondary.set_button(gdk::BUTTON_SECONDARY);
    secondary.set_propagation_phase(gtk::PropagationPhase::Capture);
    secondary.connect_pressed(glib::clone!(
        #[weak(rename_to = under)]
        view,
        move |_, _, x, y| fill_menu(&menu, link_at(&under, x, y).as_deref())
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
fn fill_menu(menu: &gio::Menu, link: Option<&str>) {
    menu.remove_all();
    let clipboard = gio::Menu::new();
    for action in ["win.terminal-copy", "win.terminal-paste"] {
        clipboard.append(Some(crate::actions::label_of(action)), Some(action));
    }
    menu.append_section(None, &clipboard);
    let tab = gio::Menu::new();
    for action in ["win.terminal", "win.close-tab"] {
        tab.append(Some(crate::actions::label_of(action)), Some(action));
    }
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

/// The URL under (x, y): an OSC 8 hyperlink first, because a program that marked its own link
/// knows better than the pattern does, and the regex match otherwise. Filtered here, so what this
/// app will not open never reaches the menu either.
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

/// Whether a URL a shell put on the screen may be handed to the desktop at all.
///
/// An allowlist rather than a check for something dangerous: the output of anything running in the
/// terminal is untrusted text, so `file:`, `javascript:` and every scheme a helper application has
/// registered stay unopenable, whatever the pattern happened to match or an OSC 8 escape claimed.
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

/// Foreground, background and the sixteen ANSI colours, all from `theme.rs`.
///
/// The foreground used to be read off the widget's resolved CSS, which is how it stayed behind on a
/// theme change: libvte styles its own widget with `color: @theme_text_color`, and that named colour
/// is upstream of the CSS variables Solarized redeclares, so it never saw them. The palette used to
/// be VTE's own, which is arithmetic rather than designed: its blue lands at 1.41:1 on our dark
/// background, which is why a shell's output was hard to read. A GNOME app owes its terminal
/// colours that work on the background it chose, so all three now come from one place.
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
    pango::FontDescription::from_string(&adw::StyleManager::default().monospace_font_name())
}

/// ponytail: `$SHELL` or `/bin/sh`. vte4 0.10 does not bind `vte_get_user_shell`, which is what
/// would read the password database when the variable is unset.
fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// What a terminal's key starts with. Not a path, so nothing that walks the open documents by path
/// can collide with one.
const KEY: &str = "terminal:";

/// A key for a new shell: sixteen random hex digits, which is also the name the shell is held
/// under, so it has to be unique across windows, session files and machines, not only within a
/// window.
pub fn new_key() -> String {
    format!("{KEY}{:08x}{:08x}", glib::random_int(), glib::random_int())
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

/// The `accent-cli` beside this binary, which is what holds the shells: `target/<profile>/` in a
/// build, `bin/` in an install. Not `ssh::server_binary`, which finds the static build uploaded
/// to a host.
pub fn cli() -> Option<PathBuf> {
    let cli = std::env::current_exe().ok()?.with_file_name("accent-cli");
    cli.is_file().then_some(cli)
}

/// End the shell `key` names, held where `at` says: Close Tab on this machine, and a saved
/// session's shell that a session written over it leaves out. Without `accent-cli` nothing holds
/// a shell here, and it ended with its tab.
///
/// On a host, over that host's own master, which is the one a window of shells rides there.
pub fn end(key: &str, at: &str) {
    if ssh::is_remote(at) {
        let url = ssh::parse(at);
        match url.and_then(|url| Shell::remote(url.clone(), accent_api::link::host(&url), key)) {
            Ok(Shell::Remote { kill, .. }) => run_kill(kill),
            Ok(Shell::Local(_)) => {}
            Err(e) => tracing::warn!("cannot end the shell at {at}: {e}"),
        }
        return;
    }
    if let Some(cli) = cli() {
        let cli = cli.to_string_lossy().into_owned();
        run_kill(vec![cli, "kill".to_string(), id(key).to_string()]);
    }
}

/// End every shell the session stored under `key` holds, here or on a host: a session removed
/// while no window has it open, whose shells nothing would attach to again.
pub fn end_stored(key: &Path) {
    for (id, place) in accent_core::config::Session::load(key).terminals {
        end(&id, &place.at);
    }
}

/// End the shells this machine's holder keeps that no session names: what is left of a window
/// that did not close the ordinary way — a quit, SIGTERM, a logout — or of a Close Tab whose kill
/// died with the process. Hosts are left alone.
///
/// Once per process, before its first window, so every shell the holder lists was started by an
/// earlier run: none of this one's can be caught between the holder starting it and its tab
/// attaching, when it would look like one nobody has. That puts one `accent-cli held` on the way
/// to the first frame (3–4 ms in a debug build, most of it the process start), and the sessions
/// are read only when something is held.
pub fn sweep() {
    let Some(cli) = cli() else {
        return;
    };
    let held = match std::process::Command::new(cli).arg("held").output() {
        Ok(out) if out.status.success() && !out.stdout.is_empty() => out.stdout,
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
    for key in orphans(&String::from_utf8_lossy(&held), &named, same_state) {
        tracing::info!("ending the held shell {key}, which no session names");
        end(&key, "");
    }
}

/// The keys of the shells in `held` (what `accent-cli held` prints) that [`sweep`] ends: those no
/// terminal has, which leaves another instance's alone, whose id is one accent chose, which
/// leaves a hand-typed `accent-cli attach` alone, that `ours` says were started under this state
/// directory, and that no session in `named` has.
fn orphans(held: &str, named: &HashSet<String>, ours: impl Fn(u64) -> bool) -> Vec<String> {
    held.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|shell| shell["attached"] == false)
        .filter(|shell| shell["pid"].as_u64().is_some_and(&ours))
        .filter_map(|shell| Some(format!("{KEY}{}", shell["id"].as_str()?)))
        .filter(|key| is_key(key) && !named.contains(key))
        .collect()
}

/// Whether process `pid` started with this process's `XDG_STATE_HOME`, where the sessions that
/// could name it are. An instance run on a state directory of its own — a drill, `make smoke` —
/// shares the holder whenever it shares `$TMPDIR`, and its sessions say nothing about the shells
/// of the one the user runs.
fn same_state(pid: u64) -> bool {
    let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    let theirs = environ
        .split(|&b| b == 0)
        .find_map(|var| var.strip_prefix(b"XDG_STATE_HOME="));
    let ours = std::env::var_os("XDG_STATE_HOME");
    theirs == ours.as_deref().map(OsStrExt::as_bytes)
}

/// Run a kill's argument vector and wait for it on a thread of its own, so Close Tab never waits
/// on the holder or on a host. An empty one is a shell nothing holds.
fn run_kill(argv: Vec<String>) {
    let Some((program, args)) = argv.split_first() else {
        return;
    };
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(std::process::Stdio::null());
    let _ = std::thread::Builder::new()
        .name("accent-kill".to_string())
        .spawn(move || {
            if let Err(e) = command.status() {
                tracing::warn!("cannot end the shell: {e}");
            }
        });
}

/// The directory an OSC 7 `file://host/path` URI names, if `host` is this machine. A shell that
/// has ssh'd somewhere reports the other machine's directory, which is no place to start one here.
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

    /// What the startup sweep ends: accent's own shells, that no terminal has, that were started
    /// under this state directory, and that no session names.
    #[test]
    fn a_held_shell_no_session_names_is_an_orphan() {
        let held = [
            r#"{"attached":false,"cwd":"/","id":"00000000000000a1","pid":1,"title":""}"#,
            // Named by a session, so it comes back when that session is opened.
            r#"{"attached":false,"cwd":"/","id":"00000000000000a2","pid":2,"title":""}"#,
            // In a terminal right now: another instance of accent on the same holder.
            r#"{"attached":true,"cwd":"/","id":"00000000000000a3","pid":3,"title":""}"#,
            // Not an id accent chose: an `accent-cli attach` typed by hand.
            r#"{"attached":false,"cwd":"/","id":"demo","pid":4,"title":""}"#,
            // Started under another state directory, whose sessions are not read here.
            r#"{"attached":false,"cwd":"/","id":"00000000000000a5","pid":5,"title":""}"#,
        ]
        .join("\n");
        let named = HashSet::from(["terminal:00000000000000a2".to_string()]);
        assert_eq!(
            orphans(&held, &named, |pid| pid != 5),
            ["terminal:00000000000000a1"]
        );
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
