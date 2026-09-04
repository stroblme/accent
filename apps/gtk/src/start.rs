//! Start screen: pick a vault when the app is launched without one.
//!
//! Nothing here knows about `App`: the window takes the shared config and one callback and hands
//! itself back, so the caller opens the vault and closes this window on its own terms.

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

/// The window shown when `accent` is launched without a vault path.
/// `on_open` receives the chosen vault directory.
pub fn present(
    app: &adw::Application,
    config: Rc<RefCell<Config>>,
    on_open: impl Fn(PathBuf) + 'static,
) -> adw::ApplicationWindow {
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

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .halign(gtk::Align::Center)
        .width_request(COLUMN_WIDTH)
        .build();
    column.append(&open);
    if let Some(list) = recent_list(&config, &on_open) {
        column.append(&list);
    }

    let status = adw::StatusPage::builder()
        .icon_name("io.github.stroblme.Accent")
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
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let row = adw::ActionRow::builder()
        .title(name)
        .subtitle(abbreviate(&path, home))
        .activatable(true)
        // Directory names are plain text, not Pango markup: an "R&D" vault must not warn.
        .use_markup(false)
        .build();
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

/// Recent vaults whose directory is still there. A deleted or unmounted one is dropped rather
/// than offered as a row that can only fail.
fn existing(recent: &[PathBuf]) -> Vec<PathBuf> {
    recent.iter().filter(|p| p.is_dir()).cloned().collect()
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
}
