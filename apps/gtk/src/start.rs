//! Start screen: pick a vault when the app is launched without one.
//!
//! Nothing here knows about `App`: the window takes the shared config and two callbacks and hands
//! itself back, so the caller opens the vault and closes this window on its own terms.

use accent_api::ssh;
use accent_core::config::Config;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// AdwStatusPage scrolls its child, so without a size request the window could be dragged
/// smaller than the button it exists to show.
const MIN_WIDTH: i32 = 420;
const MIN_HEIGHT: i32 = 400;
/// Keeps the button and the recent list a readable column instead of the window's full width.
const COLUMN_WIDTH: i32 = 360;
/// The response the connect dialog opens a remote with.
const CONNECT: &str = "connect";

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
    if let Some(list) = recent_list(&config, &on_open) {
        column.append(&list);
    }

    let status = adw::StatusPage::builder()
        .icon_name(crate::APP_ID)
        .title("Accent")
        .description("Open a folder of markdown notes to start writing.")
        .child(&column)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&status));
    window.set_content(Some(&toolbar));

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

/// The recent-vaults list, or `None` when none of the entries is still there.
fn recent_list(
    config: &Rc<RefCell<Config>>,
    on_open: &Rc<dyn Fn(PathBuf)>,
) -> Option<gtk::ListBox> {
    let recent = existing(&config.borrow().recent_vaults);
    if recent.is_empty() {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");
    for path in recent {
        list.append(&recent_row(path, home.as_deref(), config, on_open));
    }
    Some(list)
}

fn recent_row(
    path: PathBuf,
    home: Option<&Path>,
    config: &Rc<RefCell<Config>>,
    on_open: &Rc<dyn Fn(PathBuf)>,
) -> adw::ActionRow {
    let (title, subtitle) = labels(&path, home);
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        // Directory names are plain text, not Pango markup: an "R&D" vault must not warn.
        .use_markup(false)
        .build();
    // Only a remote is marked: most rows are folders on this machine, and an icon on every one of
    // them would say nothing. `network-server-symbolic` is in Adwaita 50 under `symbolic/places/`.
    if ssh::is_remote_path(&path) {
        row.add_prefix(&gtk::Image::from_icon_name("network-server-symbolic"));
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
        let (path, config) = (path.clone(), config.clone());
        move |button| {
            {
                let mut cfg = config.borrow_mut();
                cfg.recent_vaults.retain(|p| p != &path);
                if let Err(e) = cfg.save() {
                    tracing::warn!("saving config: {e:#}");
                }
            }
            // Looked up rather than captured, so the row does not hold a reference to itself.
            if let Some(row) = button.ancestor(adw::ActionRow::static_type())
                && let Some(list) = row.parent().and_downcast::<gtk::ListBox>()
            {
                list.remove(&row);
            }
        }
    });
    row.add_suffix(&forget);
    row
}

/// What a recent row says about a vault: a local one is named by its folder and placed by its
/// path, a remote one by its host and by the path on that host — two vaults called `Notes` on two
/// machines have to read differently. An address that will not parse is shown as it was stored,
/// since anything else would be a guess about what the user meant.
pub(crate) fn labels(path: &Path, home: Option<&Path>) -> (String, String) {
    if ssh::is_remote_path(path) {
        return match ssh::parse(&path.to_string_lossy()) {
            Ok(url) => (url.host, url.path.display().to_string()),
            Err(_) => (path.display().to_string(), String::new()),
        };
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    (name, abbreviate(path, home))
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

/// Recent vaults still worth offering. A local directory that has gone — deleted, or on a drive
/// nobody has mounted — is dropped rather than offered as a row that can only fail.
///
/// A remote is kept whatever state it is in: the only way to find out is to connect, and dialling
/// out to draw a start screen would be far worse than an entry that might not answer.
pub(crate) fn existing(recent: &[PathBuf]) -> Vec<PathBuf> {
    recent
        .iter()
        .filter(|p| ssh::is_remote_path(p) || p.is_dir())
        .cloned()
        .collect()
}

/// The recent vaults a window can switch to: the ones still worth offering, minus the one it is
/// already on. Keys, not paths — `Vault::key`, `Config::touch_recent` and this list all spell a
/// vault the same way, a canonical path or an `ssh://` address, so plain equality is the answer.
///
/// A vault that already has a window of its own stays in: picking it raises that window, which is
/// the one-vault-one-window rule doing its job rather than a row that fails.
pub(crate) fn other_vaults(recent: &[PathBuf], current: Option<&Path>) -> Vec<String> {
    existing(recent)
        .into_iter()
        .filter(|p| Some(p.as_path()) != current)
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

// ------------------------------------------------------------------ connecting

/// Ask for a host and a path, and hand the address they make to `on_open_remote`.
///
/// An `AdwAlertDialog` like the ones in `fileops`: Cancel, one verb, and the form as its extra
/// child. Any response closes such a dialog, so an address that does not parse is refused by
/// keeping Connect insensitive and saying why under the fields, rather than by closing on a
/// failure the user would then have to reopen the dialog to correct. The start screen has no
/// toast overlay, so there is nowhere else for that sentence to go anyway.
pub(crate) fn connect_dialog(
    window: &impl IsA<gtk::Widget>,
    on_open_remote: impl Fn(String) + 'static,
) {
    let host = gtk::Entry::builder()
        .placeholder_text("server.example.com")
        .activates_default(true)
        .hexpand(true)
        .build();
    let path = gtk::Entry::builder()
        .placeholder_text("/home/you/Notes")
        .activates_default(true)
        .build();
    let why = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    why.add_css_class("error");

    // 12 px between related widgets, as the name dialogs in `fileops` use.
    let form = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();
    form.append(&host_field(&host));
    form.append(&path);
    form.append(&why);

    let dialog = adw::AlertDialog::new(Some("Open Remote Vault"), None);
    dialog.set_extra_child(Some(&form));
    dialog.add_responses(&[("cancel", "Cancel"), (CONNECT, "Connect")]);
    dialog.set_response_appearance(CONNECT, adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some(CONNECT));
    dialog.set_close_response("cancel");
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
        move || {
            let address = address(&host.text(), &path.text());
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

    dialog.choose(Some(window), gio::Cancellable::NONE, {
        let (host, path) = (host.clone(), path.clone());
        move |response| {
            if response != CONNECT {
                return;
            }
            // Connect is only sensitive while the two fields make an address, so this holds.
            if let Ok(address) = address(&host.text(), &path.text()) {
                on_open_remote(address);
            }
        }
    });
    // The entry is mapped once the dialog has been presented, not before.
    host.grab_focus();
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
fn address(host: &str, path: &str) -> Result<String, String> {
    let (host, path) = (host.trim(), path.trim());
    if host.is_empty() || path.is_empty() {
        return Err(String::new());
    }
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
    fn existing_drops_recent_vaults_that_are_gone() {
        // ponytail: `std::env::temp_dir` rather than a `tempfile` dev-dependency apps/gtk does
        // not have. One directory, named after the process, removed at the end.
        let dir = std::env::temp_dir().join(format!("accent-start-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.md");
        std::fs::write(&file, "x").unwrap();

        let kept = existing(&[dir.clone(), dir.join("gone"), file]);
        assert_eq!(kept, std::slice::from_ref(&dir));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_keeps_a_remote_it_cannot_check() {
        let remote = PathBuf::from("ssh://box/srv/vault");
        let gone = PathBuf::from("/no/such/vault/on/this/machine");
        assert_eq!(existing(&[remote.clone(), gone]), [remote]);
    }

    #[test]
    fn other_vaults_leaves_out_the_one_this_window_is_on() {
        let here = PathBuf::from("/tmp");
        let remote = PathBuf::from("ssh://box/srv/vault");
        let gone = PathBuf::from("/no/such/vault/on/this/machine");
        let recent = [remote.clone(), here.clone(), gone];
        // Recency order kept, the missing directory dropped, the remote kept unchecked.
        assert_eq!(
            other_vaults(&recent, Some(&here)),
            ["ssh://box/srv/vault".to_string()]
        );
        assert_eq!(other_vaults(&recent, Some(&remote)), ["/tmp".to_string()]);
        // No vault at all — a loose window — leaves every surviving entry in.
        assert_eq!(
            other_vaults(&recent, None),
            ["ssh://box/srv/vault".to_string(), "/tmp".to_string()]
        );
    }

    #[test]
    fn a_remote_row_is_named_by_its_host_and_placed_by_its_remote_path() {
        assert_eq!(
            labels(Path::new("ssh://me@box:2222/srv/vault"), None),
            ("box".to_string(), "/srv/vault".to_string())
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
            address(" box ", " /srv/vault "),
            Ok("ssh://box/srv/vault".to_string())
        );
        assert_eq!(
            address("me@box:2222", "/srv/vault"),
            Ok("ssh://me@box:2222/srv/vault".to_string())
        );
        // Nothing to say about a form that is not finished.
        assert_eq!(address("", "/srv/vault"), Err(String::new()));
        assert_eq!(address("box", ""), Err(String::new()));
        assert_eq!(
            address("box", "srv/vault"),
            Err("the path must be absolute".to_string())
        );
    }
}
