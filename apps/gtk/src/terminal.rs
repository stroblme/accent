//! A shell in a tab.
//!
//! A terminal is a document like any other: it lives in a pane's tab view, so it splits, drags
//! between panes and windows, and takes the tab context menu with it. There is no terminal panel
//! and no terminal-shaped hole in the layout — a shell at the bottom of the window is a pane split
//! downwards, which is the same gesture that puts a note there.
//!
//! A focused shell owns the keyboard, and it wins by default: the window keeps only Close Tab
//! (`Ctrl+W`), New Terminal, the three zoom chords, Fullscreen and the `Ctrl+Shift` half of the
//! action table, and everything else — `Ctrl+C`, `Ctrl+D`, `Ctrl+K`, `Ctrl+L`, `Ctrl+R` and the
//! rest of readline — reaches the shell. The window is where that happens, not here: GTK
//! dispatches a window's application accelerators ahead of the VTE, so nothing a controller on
//! this widget claims can beat them, and `Shell::apply_accels` in `main` unbinds the rest of the
//! table for as long as the active window's focus is a terminal (`main::reserved`, `has_focus`).
//!
//! Nothing hung on the shell's widgets may hold them: the page owns the scroller, the scroller owns
//! the view, so a strong reference captured by a signal handler, a gesture or an action group the
//! view itself carries closes a ring that closing the tab cannot cut. GTK4 finalises a widget by
//! reference count alone — there is no `destroy` to break one from outside — and a `VteTerminal`
//! that is never finalised never drops its `VtePty`, so the pty master stays open and the shell
//! never gets its hangup. That is how every closed terminal tab used to leak a live shell.

use std::path::{Path, PathBuf};
use std::rc::Rc;

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
}

impl Term {
    pub fn key(&self) -> String {
        self.key.clone()
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
        self.view.set_font_scale(crate::clamp_zoom(zoom));
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

/// Open a shell in `cwd` as a tab of `tabs`.
/// What a shell tab runs.
///
/// A remote vault's shell belongs on the remote: the files are there, the repository is there,
/// and a build the user starts in it has to see them. It is an ordinary tab either way — the
/// same split, the same drag, the same chords — because it is the same widget with a different
/// argument vector.
pub enum Shell {
    /// The user's own shell, in a directory on this machine.
    Local(PathBuf),
    /// An interactive login on the host, landing in the vault root. Built by
    /// `accent_api::ssh::shell`, which is also what carries the ControlPath.
    Remote { argv: Vec<String>, host: String },
}

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
            Shell::Remote { host, .. } => host.clone(),
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

    let (cwd, argv) = match shell {
        Shell::Local(cwd) => (Some(cwd.clone()), vec![user_shell()]),
        // ssh decides where it lands, and a cwd on this machine means nothing to it.
        Shell::Remote { argv, .. } => (None, argv.clone()),
    };
    let named = argv.join(" ");
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    view.spawn_async(
        vte4::PtyFlags::DEFAULT,
        cwd.as_deref().and_then(Path::to_str),
        &args,
        &[],
        glib::SpawnFlags::DEFAULT,
        || {},
        -1,
        gio::Cancellable::NONE,
        move |result| {
            if let Err(e) = result {
                tracing::warn!("cannot start {named}: {e}");
            }
        },
    );

    install_keys(&view, tabs);
    install_links(&view);

    Rc::new(Term { key, page, view })
}

/// The shell exited: hand the terminal back so the caller can close its tab.
pub fn on_exit(term: &Rc<Term>, done: impl Fn(&Rc<Term>) + 'static) {
    let weak = Rc::downgrade(term);
    term.view.connect_child_exited(move |_, _| {
        if let Some(term) = weak.upgrade() {
            done(&term);
        }
    });
}

/// What the terminal answers itself: moving between tabs, which `AdwTabView` binds at the window
/// in the bubble phase — too late, because the shell has already turned the chord into an escape
/// sequence by then. Copy and paste used to be here as hard-coded callbacks; they are
/// `win.terminal-copy` and `win.terminal-paste` now, so they rebind and list in the palette.
fn install_keys(view: &vte4::Terminal, tabs: &adw::TabView) {
    let keys = gtk::ShortcutController::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);

    let add = |accel: &str, action: gtk::CallbackAction| {
        if let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) {
            keys.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
        }
    };
    for (accel, next) in [
        ("<Control>Page_Up", false),
        ("<Control><Shift>Tab", false),
        ("<Control>Page_Down", true),
        ("<Control>Tab", true),
    ] {
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

/// A terminal's key. Not a path, so nothing that walks the open documents by path can collide with
/// one, and the number only has to be unique within a window.
pub fn key(n: usize) -> String {
    format!("terminal:{n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_key_is_never_a_path() {
        assert_eq!(key(3), "terminal:3");
        // Not loose, which is what would send it through the file machinery.
        assert!(!crate::doc::is_loose_key(&key(1)));
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
