//! The terminal panel under the document.
//!
//! Several shells as tabs, in a panel that is hidden until it is asked for. A shell starts at the
//! vault root, which is the directory everything else in the window is relative to; a window with
//! no vault opens one at home.
//!
//! A focused terminal owns the keyboard, so the window's accelerators would otherwise be
//! unreachable from inside it and half of them would be eaten by the shell. The rule is that every
//! `Ctrl+Shift` chord in the action table, plus the toggle itself, is claimed here and forwarded to
//! the window; everything else belongs to the shell, `Ctrl+C` and `Ctrl+K` included.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use vte4::TerminalExt;
use vte4::TerminalExtManual;

/// What one of the panel's own chords does to it.
type Act = Box<dyn Fn(&Rc<Panel>)>;

/// Scrollback, in lines. Enough to read back a build, far short of a memory question.
const SCROLLBACK: i64 = 10_000;
/// The share of the column the panel takes the first time it is opened, in percent.
const SHARE: i32 = 30;

pub struct Panel {
    column: gtk::Box,
    pub tabs: adw::TabView,
    /// The chords a focused shell hands back to the window. Refilled by `App::apply_accels`, so a
    /// rebind moves what is forwarded along with it.
    pub forwarded: gtk::ShortcutController,
    cwd: PathBuf,
    /// What the session remembers. Zero means the divider was never dragged.
    height: Cell<i32>,
}

impl Panel {
    pub fn new(cwd: PathBuf) -> Rc<Panel> {
        let tabs = adw::TabView::new();
        // AdwTabView's own shortcuts are window-scoped, so a second tab view would race the
        // document panes for Ctrl+Tab and Alt+1. The panel binds its own below.
        tabs.set_shortcuts(adw::TabViewShortcuts::NONE);

        let bar = adw::TabBar::builder()
            .view(&tabs)
            .autohide(false)
            .css_classes(["inline"])
            .build();
        let new = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New Terminal")
            .action_name("win.terminal-new")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        bar.set_end_action_widget(Some(&new));

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&bar);
        column.append(&tabs);
        column.set_visible(false);

        let forwarded = gtk::ShortcutController::new();
        forwarded.set_propagation_phase(gtk::PropagationPhase::Capture);
        column.add_controller(forwarded.clone());

        let panel = Rc::new(Panel {
            column,
            tabs,
            forwarded,
            cwd,
            height: Cell::new(0),
        });
        panel.install_shortcuts();
        panel
    }

    /// What the terminal answers itself: copy and paste, which VTE does not bind, and switching
    /// between terminals, which its tab view no longer does.
    fn install_shortcuts(self: &Rc<Self>) {
        let fixed = gtk::ShortcutController::new();
        fixed.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = Rc::downgrade(self);
        let add = |accel: &str, action: Act| {
            let this = this.clone();
            let Some(trigger) = gtk::ShortcutTrigger::parse_string(accel) else {
                return;
            };
            fixed.add_shortcut(gtk::Shortcut::new(
                Some(trigger),
                Some(gtk::CallbackAction::new(move |_, _| {
                    if let Some(panel) = this.upgrade() {
                        action(&panel);
                    }
                    glib::Propagation::Stop
                })),
            ));
        };
        add(
            "<Control><Shift>c",
            Box::new(|p| {
                if let Some(term) = p.current() {
                    term.copy_clipboard_format(vte4::Format::Text);
                }
            }),
        );
        add(
            "<Control><Shift>v",
            Box::new(|p| {
                if let Some(term) = p.current() {
                    term.paste_clipboard();
                }
            }),
        );
        add(
            "<Control>Page_Up",
            Box::new(|p| {
                p.tabs.select_previous_page();
            }),
        );
        add(
            "<Control>Page_Down",
            Box::new(|p| {
                p.tabs.select_next_page();
            }),
        );
        self.column.add_controller(fixed);
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.column.upcast_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.n_pages() == 0
    }

    /// The terminal in the selected tab.
    fn current(&self) -> Option<vte4::Terminal> {
        self.tabs.selected_page().and_then(|p| terminal_of(&p))
    }

    /// Open a shell at the panel's directory.
    pub fn spawn(self: &Rc<Self>) {
        let term = vte4::Terminal::new();
        term.set_scrollback_lines(SCROLLBACK);
        term.set_vexpand(true);
        term.set_hexpand(true);
        // VTE is scrollable itself and draws no scrollbar of its own.
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&term)
            .build();

        let page = self.tabs.append(&scroller);
        page.set_title("Terminal");

        // The theme is only readable once the widget is mapped, so paint it there rather than now.
        term.connect_map(paint);
        let tabs = self.tabs.clone();
        term.connect_child_exited(move |term, _| {
            if let Some(scroller) = term.parent()
                && let Some(page) = tabs.page(&scroller).into()
            {
                tabs.close_page(&page);
            }
        });
        let page_title = page.clone();
        term.connect_window_title_changed(move |term| {
            if let Some(title) = term.window_title().filter(|t| !t.is_empty()) {
                page_title.set_title(&title);
            }
        });

        let shell = shell();
        let named = shell.clone();
        term.spawn_async(
            vte4::PtyFlags::DEFAULT,
            self.cwd.to_str(),
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

        self.tabs.set_selected_page(&page);
        term.grab_focus();
    }

    pub fn focus(&self) {
        if let Some(term) = self.current() {
            term.grab_focus();
        }
    }

    pub fn close_current(&self) {
        if let Some(page) = self.tabs.selected_page() {
            self.tabs.close_page(&page);
        }
    }

    pub fn restyle(&self) {
        for term in self.terminals() {
            paint(&term);
        }
    }

    pub fn refont(&self) {
        for term in self.terminals() {
            term.set_font(Some(&monospace()));
        }
    }

    fn terminals(&self) -> Vec<vte4::Terminal> {
        (0..self.tabs.n_pages())
            .filter_map(|i| terminal_of(&self.tabs.nth_page(i)))
            .collect()
    }

    pub fn height(&self) -> i32 {
        self.height.get()
    }

    pub fn set_height(&self, px: i32) {
        self.height.set(px.max(0));
    }
}

/// Where to put the divider of a dock `dock` pixels tall so the panel is `height` tall. A height
/// of zero is a panel that has never been dragged, which opens at a share of the column.
pub fn divider(dock: i32, height: i32) -> i32 {
    let wanted = match height {
        0 => dock * (100 - SHARE) / 100,
        px => dock - px,
    };
    wanted.clamp(0, dock)
}

/// Foreground from the theme, background from the same value the preview is handed, and VTE's own
/// palette for the sixteen ANSI colours: a terminal's red is the shell's to choose, not ours.
fn paint(term: &vte4::Terminal) {
    let fg = term.color();
    let bg = gdk::RGBA::parse(crate::theme::view_bg(
        adw::StyleManager::default().is_dark(),
    ))
    .ok();
    term.set_colors(Some(&fg), bg.as_ref(), &[]);
    term.set_font(Some(&monospace()));
}

fn monospace() -> pango::FontDescription {
    pango::FontDescription::from_string(&adw::StyleManager::default().monospace_font_name())
}

fn terminal_of(page: &adw::TabPage) -> Option<vte4::Terminal> {
    page.child()
        .downcast::<gtk::ScrolledWindow>()
        .ok()
        .and_then(|s| s.child())
        .and_downcast::<vte4::Terminal>()
}

/// ponytail: `$SHELL` or `/bin/sh`. vte4 0.10 does not bind `vte_get_user_shell`, which is what
/// would read the password database when the variable is unset.
fn shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undragged_panel_opens_at_a_share_of_the_column() {
        assert_eq!(divider(1000, 0), 700);
        assert_eq!(divider(0, 0), 0);
    }

    #[test]
    fn a_remembered_height_is_measured_from_the_bottom() {
        assert_eq!(divider(1000, 240), 760);
        // A panel taller than the dock cannot push the divider off the top.
        assert_eq!(divider(300, 900), 0);
    }
}
