//! Start screen: pick a vault when the app is launched without one.
//!
//! Nothing here knows about `App`: the window takes the shared config and two callbacks and hands
//! itself back, so the caller opens the vault and closes this window on its own terms.

use crate::pathfield;
use accent_api::ssh;
use accent_core::config::Config;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

/// AdwStatusPage scrolls its child, so without a size request the window could be dragged
/// smaller than the button it exists to show.
const MIN_WIDTH: i32 = 420;
const MIN_HEIGHT: i32 = 400;
/// Keeps the button and the recent list a readable column instead of the window's full width.
const COLUMN_WIDTH: i32 = 360;
/// The response the connect dialog opens a remote with.
const CONNECT: &str = "connect";
/// How long the folder completion's own ssh connection may take before it gives up. Deliberately
/// not `rpc::DEADLINE`: nobody has pressed Connect, and a field that goes quiet for ten seconds is
/// worse than one that never completes.
const PROBE_SECONDS: u32 = 5;

/// The window shown when `accent` is launched without a vault path.
/// `on_open` receives the chosen vault directory; a remote is `app.open-remote`'s job.
pub fn present(
    app: &adw::Application,
    config: Rc<RefCell<Config>>,
    on_open: impl Fn(PathBuf) + 'static,
) -> adw::ApplicationWindow {
    // The start screen is a window like any other, so it follows the same theme preference.
    crate::theme::apply(config.borrow().theme);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Accent")
        .default_width(600)
        .default_height(500)
        .width_request(MIN_WIDTH)
        .height_request(MIN_HEIGHT)
        .build();
    let on_open: Rc<dyn Fn(PathBuf)> = Rc::new(on_open);

    // Ellipsis: the label needs a folder before it can act.
    let open = gtk::Button::builder()
        .label("Open Folder…")
        .halign(gtk::Align::Center)
        .build();
    open.add_css_class("suggested-action");
    open.add_css_class("pill");
    open.connect_clicked({
        // Weak window: this button is inside it, so holding it strongly is a cycle that keeps the
        // start screen alive after it closes.
        let (window, on_open) = (window.downgrade(), on_open.clone());
        move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let on_open = on_open.clone();
            gtk::FileDialog::builder()
                .title("Open Vault")
                .build()
                .select_folder(Some(&window), gio::Cancellable::NONE, move |result| {
                    match result {
                        Ok(folder) => {
                            if let Some(path) = folder.path() {
                                on_open(path);
                            }
                        }
                        // Dismissing the chooser lands here too, so this is not worth a message.
                        Err(e) => tracing::debug!("select folder: {e}"),
                    }
                });
        }
    });

    // A pill like its neighbour but not suggested: opening a folder on this machine stays the
    // primary action, and two suggested buttons side by side would name neither of them.
    //
    // The action rather than a handler of its own: `app.open-remote` opens the same dialog from
    // the primary menu and the Open Recent picker, and this window is built with an application,
    // so the button reaches it through its own muxer.
    let remote = gtk::Button::builder()
        .label("Open Remote…")
        .halign(gtk::Align::Center)
        .action_name("app.open-remote")
        .build();
    remote.add_css_class("pill");

    // 12 px, the spacing DESIGN.md gives two related widgets.
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    buttons.append(&open);
    buttons.append(&remote);

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .halign(gtk::Align::Center)
        .width_request(COLUMN_WIDTH)
        .build();
    column.append(&buttons);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    window.set_content(Some(&toolbar));
    match recent_section(&window, &config, &on_open) {
        // Once there are vaults to go back to, they are what this screen is for, so they take the
        // window: no status page, whose icon and title would leave room for a row at most at the
        // default size. A long list fills the height between the margins and scrolls inside it.
        // Top-aligned rather than centred, so the search field stays put while the list it
        // filters grows and shrinks under it.
        Some((recent, search)) => {
            column.append(&recent);
            column.set_valign(gtk::Align::Start);
            column.set_margin_top(24);
            column.set_margin_bottom(24);
            column.set_margin_start(12);
            column.set_margin_end(12);
            toolbar.set_content(Some(&column));
            // The keyboard starts in the search: typing narrows the list, and Enter opens the
            // first row left, which with nothing typed is the newest vault.
            search.grab_focus();
            // The row that empties the list takes the section with it (see `recent_row`), and
            // the two buttons left go to the middle, where a status page would have put them.
            recent.connect_visible_notify(|section| {
                if let Some(column) = section.parent() {
                    column.set_valign(gtk::Align::Center);
                }
            });
        }
        None => {
            let status = adw::StatusPage::builder()
                .icon_name(crate::APP_ID)
                .title("Accent")
                .description("Open a folder of markdown notes to start writing.")
                .child(&column)
                .build();
            toolbar.set_content(Some(&status));
        }
    }

    // The one action on this screen, on the shell's usual accelerator for Open. A controller on
    // the window rather than an app accelerator, so it dies with this window.
    let shortcuts = gtk::ShortcutController::new();
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>o"),
        Some(gtk::CallbackAction::new(move |_, _| {
            open.emit_clicked();
            glib::Propagation::Stop
        })),
    ));
    window.add_controller(shortcuts);

    window.present();
    window
}

/// The vaults the app has seen and the field that searches them, or `None` when it has seen none
/// that are still there.
///
/// One section: the rule that divides them from the two ways to a vault the app has never seen —
/// a bare `GtkSeparator`, with the column's 18 px on either side — a search field, and the list.
/// The list scrolls on its own, because it is not capped and the buttons above it must stay put.
fn recent_section(
    window: &adw::ApplicationWindow,
    config: &Rc<RefCell<Config>>,
    on_open: &Rc<dyn Fn(PathBuf)>,
) -> Option<(gtk::Box, gtk::SearchEntry)> {
    let recent = recent_vaults(config);
    if recent.is_empty() {
        return None;
    }
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .build();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");
    for path in recent {
        list.append(&recent_row(
            path,
            home.as_deref(),
            config,
            on_open,
            &section,
        ));
    }
    list.set_placeholder(Some(
        &gtk::Label::builder()
            .label("No matching vaults")
            .margin_top(12)
            .margin_bottom(12)
            .css_classes(["dim-label"])
            .build(),
    ));

    // The window's keys, so typing searches wherever the keyboard has gone on this screen.
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search recent vaults…")
        .build();
    search.set_key_capture_widget(Some(window));
    list.set_filter_func({
        // Weak: the list is the entry's sibling, and the entry's handlers hold the list.
        let search = search.downgrade();
        move |row| {
            let (Some(search), Some(row)) =
                (search.upgrade(), row.downcast_ref::<adw::ActionRow>())
            else {
                return true;
            };
            matches(
                &search.text(),
                &row.title(),
                &row.subtitle().unwrap_or_default(),
            )
        }
    });
    search.connect_search_changed({
        let list = list.downgrade();
        move |_| {
            if let Some(list) = list.upgrade() {
                list.invalidate_filter();
            }
        }
    });
    // Enter opens what the search has narrowed to, the first row still showing.
    search.connect_activate({
        let list = list.downgrade();
        move |_| {
            let first = list.upgrade().and_then(|list| {
                std::iter::successors(list.first_child(), |w| w.next_sibling())
                    .filter(|w| w.is_child_visible())
                    .find_map(|w| w.downcast::<adw::ActionRow>().ok())
            });
            if let Some(row) = first {
                ActionRowExt::activate(&row);
            }
        }
    });

    // Natural width, so a long path widens the column rather than wrapping in the narrowest one,
    // and natural height, so the list is as tall as its rows until the window says otherwise.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .child(&list)
        .build();
    section.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    section.append(&search);
    section.append(&scroller);
    Some((section, search))
}

/// Whether a row belongs under what is typed in the search: every row while nothing is, and
/// otherwise the rows whose name or path holds the text, case aside.
fn matches(query: &str, title: &str, subtitle: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || title.to_lowercase().contains(&query)
        || subtitle.to_lowercase().contains(&query)
}

fn recent_row(
    path: PathBuf,
    home: Option<&Path>,
    config: &Rc<RefCell<Config>>,
    on_open: &Rc<dyn Fn(PathBuf)>,
    section: &gtk::Box,
) -> adw::ActionRow {
    let (title, subtitle) = labels(&path, home);
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        // Directory names are plain text, not Pango markup: an "R&D" vault must not warn.
        .use_markup(false)
        .build();
    if let Some(icon) = row_icon(&path) {
        row.add_prefix(&gtk::Image::from_icon_name(icon));
    }
    row.connect_activated({
        let (path, on_open) = (path.clone(), on_open.clone());
        move |_| on_open(path.clone())
    });

    let forget = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remove from Recents")
        .valign(gtk::Align::Center)
        .build();
    forget.add_css_class("flat");
    forget.connect_clicked({
        // Weak: the section holds this row, so a strong handle would be a cycle.
        let (path, config, section) = (path.clone(), config.clone(), section.downgrade());
        move |button| {
            forget_vault(&config, &path);
            // Looked up rather than captured, so the row does not hold a reference to itself.
            if let Some(row) = button.ancestor(adw::ActionRow::static_type())
                && let Some(list) = row.parent().and_downcast::<gtk::ListBox>()
            {
                list.remove(&row);
                // Nothing left to offer: the rule, the search and the card are what "the vaults
                // it has seen" is made of, and an empty one says nothing. `recent_section`
                // answers this at build time and cannot answer it again, so the row that empties
                // the list takes the section with it. `row_at_index` rather than `first_child`,
                // which is the placeholder once the rows are gone.
                if list.row_at_index(0).is_none()
                    && let Some(section) = section.upgrade()
                {
                    section.set_visible(false);
                }
            }
        }
    });
    row.add_suffix(&forget);
    row
}

/// Drop `path` from the recent vaults and write the config back. Shared with the Open Recent
/// picker, which offers the same removal from its own rows, and with Close Session.
///
/// A vault's own files are not touched, nor its state file: the list is the only thing that
/// remembers a vault, and its tabs are still there when it is opened again. A terminal session is
/// nothing but its state file, so that goes with the row, and the next start's sweep then finds
/// its held shells named by no session and ends them (`terminal::sweep`); shells on a host are
/// never swept.
pub(crate) fn forget_vault(config: &Rc<RefCell<Config>>, path: &Path) {
    config.borrow_mut().recent_vaults.retain(|p| p != path);
    crate::settings::save(&config.borrow());
    if crate::terminal::session_name(path).is_some() {
        let _ = std::fs::remove_file(accent_core::config::state_path(path));
    }
}

/// What a recent row says about a vault: it is named by its folder and placed by its path, on
/// this machine and on another one alike — a remote row differs by its address and its icon, not
/// by reading the other way round. An address that will not parse is shown as it was stored,
/// since anything else would be a guess about what the user meant.
///
/// ponytail: a non-default port is not in the subtitle. `destination` is what ssh is handed, and
/// `me@box:2222:/srv/vault` reads worse than it informs; add it if two vaults on one host ever
/// differ by port alone.
pub(crate) fn labels(path: &Path, home: Option<&Path>) -> (String, String) {
    if let Some(name) = crate::terminal::session_name(path) {
        return (name.to_string(), "Terminal session".to_string());
    }
    if ssh::is_remote_path(path) {
        return match ssh::parse(&path.to_string_lossy()) {
            Ok(url) => (
                remote_name(&url),
                format!("{}:{}", url.destination(), url.path.display()),
            ),
            Err(_) => (path.display().to_string(), String::new()),
        };
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    (name, abbreviate(path, home))
}

/// The icon a recent row carries, if any. Only what is not a folder here is marked: most rows are
/// folders on this machine, and an icon on every one of them would say nothing. Both names are in
/// Adwaita 50.
pub(crate) fn row_icon(path: &Path) -> Option<&'static str> {
    if crate::terminal::session_name(path).is_some() {
        return Some("utilities-terminal-symbolic");
    }
    ssh::is_remote_path(path).then_some("network-server-symbolic")
}

/// The folder a remote vault is, for the row's title. A path with no last component — the whole
/// host, `ssh://box/` — has only the host to be named by.
fn remote_name(url: &ssh::Url) -> String {
    url.path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| url.host.clone())
}

/// The path as GNOME writes it: under `home`, `/home/me/Notes` becomes `~/Notes`.
pub(crate) fn abbreviate(path: &Path, home: Option<&Path>) -> String {
    let stripped = home
        .filter(|h| !h.as_os_str().is_empty())
        .and_then(|h| path.strip_prefix(h).ok());
    match stripped {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The recent vaults, once [`prune`] has taken out the ones that have gone. What it takes is
/// written back through the usual save, and nothing is written when it takes nothing.
pub(crate) fn recent_vaults(config: &Rc<RefCell<Config>>) -> Vec<PathBuf> {
    if prune(&mut config.borrow_mut().recent_vaults) {
        crate::settings::save(&config.borrow());
    }
    config.borrow().recent_vaults.clone()
}

/// Drop the local vaults whose folder has gone — deleted, or on a drive nobody has mounted — and
/// the terminal sessions that were never written, and say whether any went. The list is not capped, so this is what keeps it to vaults that can
/// still open, rather than a row that can only fail.
///
/// A remote stays whatever state it is in, until it is removed by hand: the only way to find out
/// is to connect, and dialling out to draw a list would be far worse than an entry that might
/// not answer.
fn prune(recent: &mut Vec<PathBuf>) -> bool {
    let before = recent.len();
    recent.retain(|p| openable(p));
    recent.len() < before
}

/// Whether a recent entry can still open: a remote, which cannot be asked without dialling out; a
/// folder that is still there; or a terminal session that has been written down.
pub(crate) fn openable(path: &Path) -> bool {
    ssh::is_remote_path(path)
        || path.is_dir()
        || (crate::terminal::session_name(path).is_some()
            && accent_core::config::state_path(path).is_file())
}

/// The recent vaults a window can switch to: all of them but the one it is already on. Keys, not
/// paths — `Vault::key`, `Config::touch_recent` and this list all spell a vault the same way, a
/// canonical path or an `ssh://` address, so plain equality is the answer.
///
/// A vault that already has a window of its own stays in: picking it raises that window, which is
/// the one-vault-one-window rule doing its job rather than a row that fails.
pub(crate) fn other_vaults(recent: &[PathBuf], current: Option<&Path>) -> Vec<String> {
    recent
        .iter()
        .filter(|p| Some(p.as_path()) != current)
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

// ------------------------------------------------------------------ connecting

/// Ask for a host and a path, and hand the address they make to `on_open_remote`.
///
/// An `AdwAlertDialog` like the ones in `fileops`: Cancel, one verb, and the form as its extra
/// child. Any response closes such a dialog, so an address that does not parse is refused by
/// keeping the verb insensitive and saying why under the fields, rather than by closing on a
/// failure the user would then have to reopen the dialog to correct. The start screen has no
/// toast overlay, so there is nowhere else for that sentence to go anyway.
///
/// `at` fills the form in with a remote window's own address, for Open Folder… there: the host
/// is the one the window is on, and the path is the one it was opened at, ready to be corrected.
/// `title` and `verb` say what the address is for: a vault to open, or a shell to start there.
pub(crate) fn connect_dialog(
    window: &impl IsA<gtk::Widget>,
    at: Option<&ssh::Url>,
    title: &str,
    verb: &str,
    on_open_remote: impl Fn(String) + 'static,
) {
    let host = gtk::Entry::builder()
        .placeholder_text("server.example.com")
        .activates_default(true)
        .hexpand(true)
        .build();
    let path = gtk::Entry::builder()
        .placeholder_text("/home/you/Notes or ~/Notes")
        .activates_default(true)
        .build();
    let why = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    why.add_css_class("error");

    // The folders come from the host itself, over a connection this dialog makes quietly and
    // gives up on without saying anything. `Probe` is where every rule about that lives.
    let probe = Rc::new(Probe::default());
    let path_row = pathfield::path_field(&path, "Folders on the host", {
        // Weak on the entry: it owns this closure through its own handler.
        let (probe, host, asked) = (probe.clone(), host.clone(), path.downgrade());
        move |typed| {
            let Some(entry) = asked.upgrade() else {
                return Vec::new();
            };
            // The keyboard has to be in this field: typing a host name is not asking for a
            // connection to whatever the half-typed name happens to resolve to.
            if !pathfield::typing_here(&entry) {
                return Vec::new();
            }
            let Ok(url) = ssh::parse(&format!("ssh://{}/", host.text().trim())) else {
                return Vec::new();
            };
            probe.aim(&url);
            let Some(dir) = typed_dir(typed) else {
                return Vec::new();
            };
            probe
                .folders(&dir, &entry)
                .map_or_else(Vec::new, |folders| pathfield::completions(typed, &folders))
        }
    });

    // 12 px between related widgets, as the name dialogs in `fileops` use.
    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    form.append(&host_field(&host));
    form.append(&path_row);
    form.append(&why);

    let dialog = crate::dialogs::name_dialog_with(title, CONNECT, verb, &form);
    // Enter in either field activates the default response, which is this one, so an empty form
    // has to leave it unusable rather than merely dim.
    dialog.set_response_enabled(CONNECT, false);

    // Weak throughout: the dialog owns the entries and an entry owns its handlers, so anything
    // held strongly in here would keep the closed dialog alive for the rest of the session.
    let check = Rc::new(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        host,
        #[weak]
        path,
        #[weak]
        why,
        #[strong]
        probe,
        move || {
            let home = probe.home.borrow().clone();
            let address = address(&host.text(), &path.text(), home.as_deref());
            let message = address.as_ref().err().map_or("", String::as_str);
            why.set_label(message);
            why.set_visible(!message.is_empty());
            dialog.set_response_enabled(CONNECT, address.is_ok());
        }
    ));
    for entry in [&host, &path] {
        let check = check.clone();
        entry.connect_changed(move |_| check());
    }

    crate::dialogs::choose(&dialog, Some(window), {
        let (host, path) = (host.clone(), path.clone());
        move |response| {
            probe.close();
            if response != CONNECT {
                return;
            }
            // Connect is only sensitive while the two fields make an address, so this holds.
            let home = probe.home.borrow().clone();
            if let Ok(address) = address(&host.text(), &path.text(), home.as_deref()) {
                on_open_remote(address);
            }
        }
    });
    // The entry is mapped once the dialog has been presented, not before.
    let Some(at) = at else {
        host.grab_focus();
        return;
    };
    // Filled in once the handlers are there, so Connect is enabled by the same check as typing.
    host.set_text(&at.authority());
    path.set_text(&at.path.to_string_lossy());
    // The keyboard in the path, at its end, and the completion asked for straight away: the host
    // is already named, so the folders next to a mistyped one are what this form is for.
    crate::dialogs::focus_entry(&path, |path| path.set_position(-1));
    pathfield::look_again(&path);
}

/// The host's folders, fetched over a connection the dialog makes for itself, so the path can be
/// completed and a `~` resolved before anybody has pressed Connect.
///
/// Every rule here is about not surprising the user, who has asked for a form and not for a
/// connection:
///
/// * **Never on a keystroke of its own.** One attempt for the life of the dialog per address, made
///   the first time a completion is really wanted — the path field has the keyboard and the two
///   fields name a host. Every later folder reuses that connection, and each listing is cached.
/// * **Never a prompt.** [`ssh::probe`] forces `BatchMode=yes`, so a host that would want a
///   passphrase or a host-key answer simply offers no completion. `askpass.rs` is for a connection
///   the user asked for.
/// * **Never a failure on screen.** A refusal, a timeout or a listing error offers nothing and is
///   logged at debug. There is no toast, no banner and nothing in the form.
/// * **Never the vault's socket**, and what it opens is shut down when the dialog closes.
#[derive(Default)]
struct Probe {
    /// The address the answers below are about. A different host starts over.
    url: RefCell<Option<ssh::Url>>,
    /// `$HOME` on the host, once it has said. What a `~` path resolves against.
    home: RefCell<Option<String>>,
    /// Folder names per absolute directory; `None` while its listing is still out.
    dirs: RefCell<HashMap<String, Option<Vec<String>>>>,
    /// Set by the first failure to connect. One attempt is all an address gets.
    silent: Cell<bool>,
    /// Every address this dialog ran a probe against, so all their sockets can be shut down.
    opened: RefCell<Vec<ssh::Url>>,
}

impl Probe {
    /// Point at `url`, forgetting whatever a previous host had said.
    fn aim(&self, url: &ssh::Url) {
        if self.url.borrow().as_ref() == Some(url) {
            return;
        }
        *self.url.borrow_mut() = Some(url.clone());
        *self.home.borrow_mut() = None;
        self.dirs.borrow_mut().clear();
        self.silent.set(false);
    }

    /// The folders in `dir`, or `None` while that is not known — including for good, when the host
    /// could not be reached. Asks once per directory and calls `again` when the answer lands.
    fn folders(self: &Rc<Self>, dir: &str, asked: &gtk::Entry) -> Option<Vec<String>> {
        if let Some(known) = self.dirs.borrow().get(dir) {
            return known.clone();
        }
        if self.silent.get() {
            return None;
        }
        let url = self.url.borrow().clone()?;
        self.dirs.borrow_mut().insert(dir.to_string(), None);
        self.opened.borrow_mut().push(url.clone());
        let (dir, asked, weak) = (dir.to_string(), asked.downgrade(), Rc::downgrade(self));
        // The connection and the listing both happen off the main loop, so the entry stays
        // typeable throughout. Weak, because the dialog may be gone by the time the host answers.
        glib::spawn_future_local(async move {
            let answer = crate::work::off_thread("ssh completion", {
                let (url, dir) = (url.clone(), dir.clone());
                move || ask(&url, &dir)
            })
            .await;
            let Some(probe) = weak.upgrade() else {
                return;
            };
            match answer {
                Some(Some((home, folders))) => {
                    *probe.home.borrow_mut() = Some(home);
                    probe.dirs.borrow_mut().insert(dir, Some(folders));
                }
                answer => {
                    tracing::debug!(dir, "no completion from the host: {answer:?}");
                    probe.silent.set(true);
                    probe.dirs.borrow_mut().remove(&dir);
                }
            }
            if let Some(entry) = asked.upgrade() {
                pathfield::look_again(&entry);
            }
        });
        None
    }

    /// Shut down every socket this dialog opened, on a thread of its own: the dialog is closing
    /// and nothing here waits for an answer.
    fn close(&self) {
        let urls = std::mem::take(&mut *self.opened.borrow_mut());
        if urls.is_empty() {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("accent-probe-exit".to_string())
            .spawn(move || {
                for url in urls {
                    let argv = ssh::exit(&url, &ssh::probe_path(&url));
                    let _ = Command::new(&argv[0])
                        .args(&argv[1..])
                        .stdin(Stdio::null())
                        .output();
                }
            });
    }
}

/// `(the host's `$HOME`, the folders in `dir`)`, or `None` where the host could not be reached.
///
/// One command answers both: the home directory is what a `~` path needs and what says the shell
/// really ran, so an empty first line is how a refused or timed-out connection is told apart from
/// a directory that simply is not there.
///
/// ponytail: `ls -1p` and one line per name, so a folder whose name holds a newline is listed as
/// two. Nothing here writes anything, and the worst case is a completion nobody can use.
fn ask(url: &ssh::Url, dir: &str) -> Option<(String, Vec<String>)> {
    let ctl = ssh::probe_path(url);
    if let Some(parent) = ctl.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let command = format!("printf '%s\\n' \"$HOME\"; ls -1p -- {}", remote_word(dir));
    let argv = ssh::probe(url, &ctl, PROBE_SECONDS, &command);
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        // Belt and braces over `BatchMode`: whatever the session put in the environment, no
        // helper of any kind may put a window on screen for a connection nobody asked for.
        .env("SSH_ASKPASS_REQUIRE", "never")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = text.lines();
    let home = lines.next().filter(|home| home.starts_with('/'))?;
    let folders = lines
        .filter_map(|line| line.strip_suffix('/'))
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
        .map(str::to_string)
        .collect();
    Some((home.to_string(), folders))
}

/// The directory as a word for the remote shell, with a leading `~` left to the host's own
/// `$HOME` rather than resolved here — the whole point being that this machine does not know it.
fn remote_word(dir: &str) -> String {
    match dir.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("\"$HOME\"{}", ssh::quote(rest))
        }
        _ => ssh::quote(dir),
    }
}

/// The folder a half-typed remote path points into: everything up to the last `/`, with the root
/// spelled `/` rather than empty. `~` on its own counts, so a tilde path resolves its home before
/// there is anything to complete.
fn typed_dir(typed: &str) -> Option<String> {
    let typed = typed.trim();
    match typed.rsplit_once('/') {
        Some(("", _)) => Some("/".to_string()),
        Some((head, _)) => Some(head.to_string()),
        None => typed.starts_with('~').then(|| "~".to_string()),
    }
}

/// The host entry, with the hosts from `~/.ssh/config` in a menu beside it.
///
/// A menu button rather than completion inside the entry, because GTK4 dropped
/// `GtkEntryCompletion` and put nothing in its place: a `GtkDropDown` either closes the field to
/// what is listed or needs a factory of its own to stay editable. These are a shortcut for typing,
/// not the only way in, so the cheap shape is the right one. Nothing to suggest, no button.
fn host_field(entry: &gtk::Entry) -> gtk::Widget {
    let hosts = ssh_hosts(&ssh_config());
    if hosts.is_empty() {
        return entry.clone().upcast();
    }
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // A long config is a list that scrolls rather than a popover taller than the screen.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(280)
        .child(&list)
        .build();
    let popover = gtk::Popover::builder().child(&scroller).build();
    for host in hosts {
        let item = gtk::Button::builder()
            .child(&gtk::Label::builder().label(&host).xalign(0.0).build())
            .build();
        item.add_css_class("flat");
        item.connect_clicked({
            let (entry, popover, host) = (entry.clone(), popover.clone(), host.clone());
            move |_| {
                entry.set_text(&host);
                popover.popdown();
            }
        });
        list.append(&item);
    }
    // `view-list-symbolic` for what the button does — show a list — per DESIGN.md's preference for
    // a name that says the action over one that says which way a panel opens.
    let button = gtk::MenuButton::builder()
        .icon_name("view-list-symbolic")
        .tooltip_text("Hosts from ~/.ssh/config")
        .popover(&popover)
        .build();
    let field = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    field.add_css_class("linked");
    field.append(entry);
    field.append(&button);
    field.upcast()
}

/// The user's ssh config, or nothing at all. Having none is ordinary rather than an error: the
/// hosts it holds are a convenience, and every field here still accepts anything typed.
fn ssh_config() -> String {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".ssh/config"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
}

/// The host names an ssh config declares, in the order it declares them.
///
/// A `Host` line carries patterns as well as names, and a pattern (`*`, `?`, or a `!` negation)
/// matches hosts rather than naming one, so there is nothing there to connect to. `Match` blocks
/// and `Include` are not followed: this only saves the user some typing.
fn ssh_hosts(config: &str) -> Vec<String> {
    config
        .lines()
        .filter_map(|line| {
            let (key, rest) = line.trim().split_once(char::is_whitespace)?;
            key.eq_ignore_ascii_case("host").then_some(rest)
        })
        .flat_map(str::split_whitespace)
        .filter(|name| !name.contains(['*', '?', '!']))
        .map(str::to_string)
        .collect()
}

/// The address the two fields make, or why they do not make one yet.
///
/// Assembled as text and read back with [`ssh::parse`] rather than built as an [`ssh::Url`], so a
/// host typed with a login, a port or IPv6 brackets is understood exactly as `Vault` will
/// understand it, and a malformed one is refused here rather than at the connection. A field that
/// is still empty is an unfinished form, not a mistake, so it comes back with nothing to say.
///
/// `~` and `~/…` are the host's home, which only the host knows: `home` is what [`Probe`] read off
/// it, and without it the form says so rather than guessing at this machine's own home. An address
/// is stored, cached and keyed by its path, so the tilde is resolved here and never travels.
fn address(host: &str, path: &str, home: Option<&str>) -> Result<String, String> {
    let (host, path) = (host.trim(), path.trim());
    if host.is_empty() || path.is_empty() {
        return Err(String::new());
    }
    let path = match path == "~" || path.starts_with("~/") {
        false => path.to_string(),
        true => match home {
            Some(home) => format!("{}{}", home.trim_end_matches('/'), &path[1..]),
            None => {
                return Err(
                    "~ needs the host, which has not answered; type the full path".to_string(),
                );
            }
        },
    };
    // Asked here rather than left to the parser, which never sees the two fields apart: `box` and
    // `srv/vault` would join into `ssh://boxsrv/vault`, a host nobody typed.
    if !path.starts_with('/') {
        return Err("the path must be absolute".to_string());
    }
    ssh::parse(&format!("ssh://{host}{path}")).map(|url| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviate_replaces_the_home_directory_with_a_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(
            abbreviate(Path::new("/home/me/Notes"), Some(home)),
            "~/Notes"
        );
        assert_eq!(abbreviate(Path::new("/home/me"), Some(home)), "~");
    }

    #[test]
    fn abbreviate_leaves_paths_outside_home_alone() {
        let home = Path::new("/home/me");
        assert_eq!(
            abbreviate(Path::new("/mnt/vault"), Some(home)),
            "/mnt/vault"
        );
        // A prefix match on the string would turn this one into "~lvin/Notes".
        assert_eq!(
            abbreviate(Path::new("/home/melvin/Notes"), Some(home)),
            "/home/melvin/Notes"
        );
    }

    #[test]
    fn abbreviate_without_home_does_not_panic() {
        let path = Path::new("/home/me/Notes");
        assert_eq!(abbreviate(path, None), "/home/me/Notes");
        assert_eq!(abbreviate(path, Some(Path::new(""))), "/home/me/Notes");
    }

    #[test]
    fn prune_drops_the_local_vaults_that_are_gone_and_keeps_the_remotes() {
        // ponytail: `std::env::temp_dir` rather than a `tempfile` dev-dependency apps/gtk does
        // not have. One directory, named after the process, removed at the end.
        let dir = std::env::temp_dir().join(format!("accent-start-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.md");
        std::fs::write(&file, "x").unwrap();
        let remote = PathBuf::from("ssh://box/srv/vault");

        // A terminal session that was never written down has nothing to open.
        let session = PathBuf::from(format!("terminal://accent-test-{}", std::process::id()));

        // Recency order kept, a folder that went and a file dropped, the remote kept unchecked.
        let mut recent = vec![remote.clone(), dir.join("gone"), dir.clone(), file, session];
        assert!(prune(&mut recent));
        assert_eq!(recent, [remote, dir.clone()]);
        // Nothing left to take, so nothing to write.
        assert!(!prune(&mut recent));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn other_vaults_leaves_out_the_one_this_window_is_on() {
        let here = PathBuf::from("/tmp");
        let remote = PathBuf::from("ssh://box/srv/vault");
        let recent = [remote.clone(), here.clone()];
        assert_eq!(
            other_vaults(&recent, Some(&here)),
            ["ssh://box/srv/vault".to_string()]
        );
        assert_eq!(other_vaults(&recent, Some(&remote)), ["/tmp".to_string()]);
        // No vault at all — a loose window — leaves every entry in, in recency order.
        assert_eq!(
            other_vaults(&recent, None),
            ["ssh://box/srv/vault".to_string(), "/tmp".to_string()]
        );
    }

    #[test]
    fn the_search_matches_a_name_or_a_path_case_aside() {
        assert!(matches("", "Notes", "~/Notes"));
        assert!(matches("  ", "Notes", "~/Notes"));
        assert!(matches("not", "Notes", "~/Notes"));
        // The path counts, a remote's host included.
        assert!(matches("box:", "vault", "me@box:/srv/vault"));
        assert!(matches("~/No", "Notes", "~/Notes"));
        assert!(!matches("thesis", "Notes", "~/Notes"));
    }

    #[test]
    fn a_remote_row_is_named_by_its_folder_and_placed_by_its_address() {
        // The login is part of the address; the port is not shown (see `labels`).
        assert_eq!(
            labels(Path::new("ssh://me@box:2222/srv/vault"), None),
            ("vault".to_string(), "me@box:/srv/vault".to_string())
        );
        assert_eq!(
            labels(Path::new("ssh://box/srv/vault"), None),
            ("vault".to_string(), "box:/srv/vault".to_string())
        );
        // A vault that is the whole of a host has only the host to be named by.
        assert_eq!(
            labels(Path::new("ssh://box/"), None),
            ("box".to_string(), "box:/".to_string())
        );
        // An address that will not parse is shown as it was stored.
        assert_eq!(
            labels(Path::new("ssh://box"), None),
            ("ssh://box".to_string(), String::new())
        );
        // A local row keeps the folder-and-path it has always had.
        assert_eq!(
            labels(Path::new("/home/me/Notes"), Some(Path::new("/home/me"))),
            ("Notes".to_string(), "~/Notes".to_string())
        );
        // A terminal session is its name, and says what it is.
        assert_eq!(
            labels(Path::new("terminal://dev"), None),
            ("dev".to_string(), "Terminal session".to_string())
        );
    }

    #[test]
    fn ssh_hosts_takes_the_names_and_leaves_the_patterns() {
        let config = concat!(
            "Host box tunnel\n",
            "  HostName 10.0.0.1\n",
            "host lowercase\n",
            "Host *\n",
            "  ForwardAgent yes\n",
            "Host *.example.com jump-?\n",
            "Host * !secret\n",
            "# Host commented\n",
        );
        assert_eq!(ssh_hosts(config), ["box", "tunnel", "lowercase"]);
        assert!(ssh_hosts("").is_empty());
    }

    #[test]
    fn an_address_needs_both_fields_and_an_absolute_path() {
        assert_eq!(
            address(" box ", " /srv/vault ", None),
            Ok("ssh://box/srv/vault".to_string())
        );
        assert_eq!(
            address("me@box:2222", "/srv/vault", None),
            Ok("ssh://me@box:2222/srv/vault".to_string())
        );
        // Nothing to say about a form that is not finished.
        assert_eq!(address("", "/srv/vault", None), Err(String::new()));
        assert_eq!(address("box", "", None), Err(String::new()));
        assert_eq!(
            address("box", "srv/vault", None),
            Err("the path must be absolute".to_string())
        );
    }

    #[test]
    fn a_tilde_path_is_the_home_the_host_reported() {
        let home = Some("/home/me");
        assert_eq!(
            address("box", "~/Notes", home),
            Ok("ssh://box/home/me/Notes".to_string())
        );
        assert_eq!(
            address("box", "~", home),
            Ok("ssh://box/home/me".to_string())
        );
        // A trailing slash on the home must not double up.
        assert_eq!(
            address("box", "~/Notes", Some("/home/me/")),
            Ok("ssh://box/home/me/Notes".to_string())
        );
        // Until the host has said, the form says so rather than guessing at this machine's home.
        assert!(address("box", "~/Notes", None).is_err());
        // `~user` is not ours to resolve, so it stays what it is: a path that is not absolute.
        assert_eq!(
            address("box", "~other/Notes", home),
            Err("the path must be absolute".to_string())
        );
    }

    #[test]
    fn typed_dir_is_everything_up_to_the_last_slash() {
        assert_eq!(typed_dir("/home/me/No"), Some("/home/me".to_string()));
        assert_eq!(typed_dir("/home/me/"), Some("/home/me".to_string()));
        // The root is a slash, not an empty path.
        assert_eq!(typed_dir("/srv"), Some("/".to_string()));
        assert_eq!(typed_dir("/"), Some("/".to_string()));
        // A tilde counts before there is anything to complete, because resolving it is what the
        // connection is for.
        assert_eq!(typed_dir("~"), Some("~".to_string()));
        assert_eq!(typed_dir("~/No"), Some("~".to_string()));
        // Nothing typed, and a relative fragment that names no folder yet.
        assert_eq!(typed_dir(""), None);
        assert_eq!(typed_dir("srv"), None);
    }

    #[test]
    fn a_tilde_directory_is_left_for_the_remote_shell_to_expand() {
        assert_eq!(remote_word("~"), "\"$HOME\"''");
        assert_eq!(remote_word("~/Notes"), "\"$HOME\"'/Notes'");
        assert_eq!(remote_word("/srv/vault"), "'/srv/vault'");
        // Not a home reference, and quoted whole so it cannot become one.
        assert_eq!(remote_word("~other"), "'~other'");
    }
}
