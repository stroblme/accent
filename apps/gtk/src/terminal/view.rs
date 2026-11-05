//! The VTE widget's scroller, colours and font, and the tab keys it takes before the shell does.

use gtk::prelude::*;
use gtk::{gdk, glib, pango};
use vte4::TerminalExt;
use vte4::TerminalExtManual;

/// The page of a shell's tab: `view` in a scroller, VTE being scrollable itself and drawing no
/// scrollbar of its own. The scroller never takes the keyboard: a tab clicked to the front asks
/// its page for it, after the tab view has given it to the shell (DESIGN.md, Terminal).
///
/// The scrollback is the scroller's to scroll, in pixels, so a wheel eases there as everywhere
/// else and a touchpad glides: VTE would step it a row at a time itself. A program reading the
/// mouse, or the alternate screen turning the wheel into arrow keys, still has it first.
pub(crate) fn scroller(view: &vte4::Terminal) -> gtk::ScrolledWindow {
    view.set_enable_fallback_scrolling(false);
    view.set_scroll_unit_is_pixels(true);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .focusable(false)
        .child(view)
        .build();
    crate::widgets::smooth_wheel(&scroller);
    scroller
}

/// Handle tab navigation before the terminal turns it into shell input.
///
/// AdwTabView's bubble-phase Page Up/Down handling is too late for VTE. Ctrl+Tab stays with
/// window MRU actions; terminal Copy and Paste stay in the rebindable action table.
pub(super) fn install_keys(view: &vte4::Terminal, tabs: &adw::TabView) {
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

/// Set foreground, background and all sixteen ANSI colours from `theme.rs`.
///
/// VTE's named CSS colours bypass Solarized variables, and its default palette has poor
/// contrast on our backgrounds. A shared palette makes restyling consistent and readable.
pub(super) fn paint(view: &vte4::Terminal) {
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

pub(super) fn monospace() -> pango::FontDescription {
    pango::FontDescription::from_string(&crate::platform::monospace_font())
}
