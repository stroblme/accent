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

/// What the terminal answers itself: copy and paste, which VTE does not bind, and moving between
/// tabs, which `AdwTabView` binds at the window in the bubble phase — too late, because the shell
/// has already turned the chord into an escape sequence by then.
fn install_keys(view: &vte4::Terminal, tabs: &adw::TabView) {
    let keys = gtk::ShortcutController::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);

    let add = |accel: &str, action: gtk::CallbackAction| {
        if let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) {
            keys.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
        }
    };
    let copy = view.clone();
    add(
        "<Control><Shift>c",
        gtk::CallbackAction::new(move |_, _| {
            copy.copy_clipboard_format(vte4::Format::Text);
            glib::Propagation::Stop
        }),
    );
    let paste = view.clone();
    add(
        "<Control><Shift>v",
        gtk::CallbackAction::new(move |_, _| {
            paste.paste_clipboard();
            glib::Propagation::Stop
        }),
    );
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
