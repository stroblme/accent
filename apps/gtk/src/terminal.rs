//! A shell in a tab.
//!
//! A terminal is a document like any other: it lives in a pane's tab view, so it splits, drags
//! between panes and windows, and takes the tab context menu with it. There is no terminal panel
//! and no terminal-shaped hole in the layout — a shell at the bottom of the window is a pane split
//! downwards, which is the same gesture that puts a note there.
//!
//! A focused shell owns the keyboard, so the window's accelerators would otherwise be unreachable
//! from inside it and half of them would be eaten by the shell. The rule is that every `Ctrl+Shift`
//! chord in the action table, plus the ones that move between tabs and the three zoom chords, is
//! claimed here and forwarded to the window; everything else belongs to the shell, `Ctrl+C` and
//! `Ctrl+K` included.

use std::path::Path;
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
    /// The window chords a focused shell hands back. Refilled by `App::apply_accels`, so a rebind
    /// moves what is forwarded along with it.
    pub forwarded: gtk::ShortcutController,
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

/// Open a shell in `cwd` as a tab of `tabs`.
pub fn open(tabs: &adw::TabView, cwd: &Path, key: String) -> Rc<Term> {
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
    page.set_title("Terminal");
    page.set_icon(Some(&gio::ThemedIcon::new("utilities-terminal-symbolic")));
    // A tab appended to a visible pane is mapped already, so the signal above has been and gone.
    if view.is_mapped() {
        paint(&view);
    }

    let title = page.clone();
    view.connect_window_title_changed(move |view| {
        if let Some(text) = view.window_title().filter(|t| !t.is_empty()) {
            title.set_title(&text);
        }
    });

    let shell = shell();
    let named = shell.clone();
    view.spawn_async(
        vte4::PtyFlags::DEFAULT,
        cwd.to_str(),
        &[&shell],
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

    let forwarded = gtk::ShortcutController::new();
    forwarded.set_propagation_phase(gtk::PropagationPhase::Capture);
    view.add_controller(forwarded.clone());
    install_keys(&view, tabs);
    install_links(&view);

    Rc::new(Term {
        key,
        page,
        view,
        forwarded,
    })
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
    let under = view.clone();
    secondary.connect_pressed(move |_, _, x, y| fill_menu(&menu, link_at(&under, x, y).as_deref()));
    view.add_controller(secondary);

    // Ctrl and the primary button, which is what GNOME Terminal asks for and what a PDF link here
    // already asks for. A plain click cannot have it: VTE claims the sequence for its own
    // selection, which cancels any gesture of ours before the release, and a press that both
    // starts a selection and launches a browser is the wrong gesture in any case.
    let primary = gtk::GestureClick::new();
    primary.set_button(gdk::BUTTON_PRIMARY);
    primary.set_propagation_phase(gtk::PropagationPhase::Capture);
    let under = view.clone();
    primary.connect_pressed(move |gesture, _, x, y| {
        if gesture
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
            && let Some(uri) = link_at(&under, x, y)
        {
            launch(&uri);
        }
    });
    view.add_controller(primary);
}

/// What the menu holds: the clipboard, then the tab, then the link under the pointer when there
/// is one. No Select All — VTE binds nothing for it — and no Split, which is a tab gesture the
/// tab strip already offers.
fn fill_menu(menu: &gio::Menu, link: Option<&str>) {
    menu.remove_all();
    let clipboard = gio::Menu::new();
    for action in ["win.terminal-copy", "win.terminal-paste"] {
        clipboard.append(Some(crate::label_of(action)), Some(action));
    }
    menu.append_section(None, &clipboard);
    let tab = gio::Menu::new();
    for action in ["win.terminal", "win.close-tab"] {
        tab.append(Some(crate::label_of(action)), Some(action));
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
    let view = view.clone();
    copy.connect_activate(move |_, target| {
        if let Some(uri) = target.and_then(|t| t.str()) {
            view.clipboard().set_text(uri);
        }
    });
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

/// Foreground from the theme composited over the background, background from the same value the
/// preview is handed, and VTE's own palette for the sixteen ANSI colours: a terminal's red is the
/// shell's to choose, not ours.
fn paint(view: &vte4::Terminal) {
    let bg = gdk::RGBA::parse(crate::theme::view_bg(
        adw::StyleManager::default().is_dark(),
    ))
    .ok();
    let theme = view.color();
    let fg = match &bg {
        Some(bg) => {
            let [r, g, b] = over(
                [theme.red(), theme.green(), theme.blue(), theme.alpha()],
                [bg.red(), bg.green(), bg.blue()],
            );
            gdk::RGBA::new(r, g, b, 1.0)
        }
        None => theme,
    };
    view.set_colors(Some(&fg), bg.as_ref(), &[]);
    view.set_font(Some(&monospace()));
}

/// VTE stores a foreground as opaque RGB and drops the alpha, so a theme colour like libadwaita's
/// light `view_fg_color` (80 % black) would paint pure black — blacker than every other piece of
/// text in the window. Composited here instead.
fn over(fg: [f32; 4], bg: [f32; 3]) -> [f32; 3] {
    let a = fg[3];
    [
        fg[0] * a + bg[0] * (1.0 - a),
        fg[1] * a + bg[1] * (1.0 - a),
        fg[2] * a + bg[2] * (1.0 - a),
    ]
}

fn monospace() -> pango::FontDescription {
    pango::FontDescription::from_string(&adw::StyleManager::default().monospace_font_name())
}

/// ponytail: `$SHELL` or `/bin/sh`. vte4 0.10 does not bind `vte_get_user_shell`, which is what
/// would read the password database when the variable is unset.
fn shell() -> String {
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

    fn close(got: [f32; 3], want: [f32; 3]) {
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 0.002, "{got:?} is not {want:?}");
        }
    }

    #[test]
    fn a_translucent_foreground_lands_where_the_rest_of_the_text_does() {
        // libadwaita's light `view_fg_color` on `--view-bg-color`: the mid grey every other label
        // in the window composites to, not the pure black VTE would have painted.
        close(
            over([0.0, 0.0, 0.024, 0.8], [1.0, 1.0, 1.0]),
            [0.2, 0.2, 0.219],
        );
        // Dark is opaque either way, so this is a no-op there.
        close(
            over([1.0, 1.0, 1.0, 1.0], [0.11, 0.11, 0.125]),
            [1.0, 1.0, 1.0],
        );
        // Fully transparent is the background and nothing else.
        close(
            over([1.0, 0.0, 0.0, 0.0], [0.11, 0.11, 0.125]),
            [0.11, 0.11, 0.125],
        );
    }
}
