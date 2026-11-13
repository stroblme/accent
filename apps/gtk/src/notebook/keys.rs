//! The notebook's own keys, heard ahead of the cells' editors (DESIGN.md, Keyboard › Notebook).

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::NotebookTab;

impl NotebookTab {
    /// The notebook's own keys ([`own_key`]), as a diagram's canvas's are, heard ahead of the
    /// cells' editors.
    pub(super) fn wire_keys(self: &Rc<Self>, host: &gtk::Box) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let entry_focused = tab
                    .scroller
                    .root()
                    .and_then(|root| root.focus())
                    .is_some_and(|focus| {
                        std::iter::successors(Some(focus), |widget| widget.parent()).any(|widget| {
                            widget.is::<gtk::Entry>() || widget.is::<gtk::PasswordEntry>()
                        })
                    });
                let Some(action) = own_key(key, state, entry_focused) else {
                    return glib::Propagation::Proceed;
                };
                let _ = tab.scroller.activate_action(action, None);
                glib::Propagation::Stop
            }
        ));
        host.add_controller(keys);
    }
}

/// The command a key pressed over the notebook is the chord of (DESIGN.md, Keyboard › Notebook).
/// Undo and Redo are its one history's, the cells' buffers keeping none, and the cell chords take
/// GtkTextView's `Ctrl+Shift+Up` / `Down`, which extend the selection by a paragraph, in a cell.
fn own_key(key: gdk::Key, state: gdk::ModifierType, entry_focused: bool) -> Option<&'static str> {
    if entry_focused {
        return None;
    }
    use gdk::ModifierType as M;
    let mods = state
        & (crate::platform::PRIMARY_MODIFIER
            | M::CONTROL_MASK
            | M::SHIFT_MASK
            | M::ALT_MASK
            | M::SUPER_MASK
            | M::META_MASK);
    let (ctrl, shift, alt) = (
        crate::platform::PRIMARY_MODIFIER,
        M::SHIFT_MASK,
        M::ALT_MASK,
    );
    Some(match key {
        gdk::Key::z | gdk::Key::Z if mods == ctrl => "win.notebook-undo",
        gdk::Key::z | gdk::Key::Z if mods == ctrl | shift => "win.notebook-redo",
        gdk::Key::y | gdk::Key::Y if mods == ctrl => "win.notebook-redo",
        // `B`, Jupyter's below: `Alt+Return` is Quick Fix's (user decision 2026-10-22).
        gdk::Key::b | gdk::Key::B if mods == alt => "win.notebook-insert-below",
        gdk::Key::Return | gdk::Key::KP_Enter if mods == alt | shift => "win.notebook-insert-above",
        gdk::Key::Return | gdk::Key::KP_Enter if mods == shift => "win.notebook-run-advance",
        gdk::Key::minus | gdk::Key::underscore if mods == ctrl | shift => "win.notebook-split",
        gdk::Key::Up | gdk::Key::KP_Up if mods == ctrl | shift => "win.notebook-move-up",
        gdk::Key::Down | gdk::Key::KP_Down if mods == ctrl | shift => "win.notebook-move-down",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdk::ModifierType as M;

    #[test]
    fn the_notebook_keys_name_their_commands() {
        let (ctrl, shift, alt) = (
            crate::platform::PRIMARY_MODIFIER,
            M::SHIFT_MASK,
            M::ALT_MASK,
        );
        let cases = [
            (gdk::Key::z, ctrl, Some("win.notebook-undo")),
            (gdk::Key::Z, ctrl | shift, Some("win.notebook-redo")),
            (gdk::Key::y, ctrl, Some("win.notebook-redo")),
            (gdk::Key::b, alt, Some("win.notebook-insert-below")),
            (gdk::Key::Return, alt, None),
            (
                gdk::Key::Return,
                alt | shift,
                Some("win.notebook-insert-above"),
            ),
            (gdk::Key::Return, shift, Some("win.notebook-run-advance")),
            (gdk::Key::KP_Enter, shift, Some("win.notebook-run-advance")),
            // Shift turns the minus key into an underscore on most layouts.
            (
                gdk::Key::underscore,
                ctrl | shift,
                Some("win.notebook-split"),
            ),
            (gdk::Key::minus, ctrl | shift, Some("win.notebook-split")),
            (gdk::Key::Up, ctrl | shift, Some("win.notebook-move-up")),
            (gdk::Key::Down, ctrl | shift, Some("win.notebook-move-down")),
            // Caps Lock changes nothing.
            (gdk::Key::Z, ctrl | M::LOCK_MASK, Some("win.notebook-undo")),
            (
                gdk::Key::Up,
                ctrl | shift | M::LOCK_MASK,
                Some("win.notebook-move-up"),
            ),
            // Chords the editor and the window keep.
            (gdk::Key::Return, M::empty(), None),
            (gdk::Key::Return, ctrl, None),
            (gdk::Key::Return, ctrl | shift, None),
            (gdk::Key::minus, ctrl, None),
            (gdk::Key::Up, ctrl, None),
            (gdk::Key::Up, alt, None),
            (gdk::Key::Up, alt | shift, None),
            (gdk::Key::z, ctrl | alt, None),
        ];
        for (key, state, action) in cases {
            assert_eq!(own_key(key, state, false), action, "{key:?} {state:?}");
        }
    }

    #[test]
    fn an_input_entry_keeps_its_own_keys() {
        assert_eq!(own_key(gdk::Key::Return, M::SHIFT_MASK, true), None);
        assert_eq!(
            own_key(gdk::Key::z, crate::platform::PRIMARY_MODIFIER, true),
            None
        );
    }
}
