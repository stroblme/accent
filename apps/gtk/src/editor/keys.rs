//! One key controller on the view, and the order the modules see a press in.
//!
//! Five things in this editor want the same handful of keys — Tab, Escape, Return — and they used
//! to take them through four capture-phase controllers on one widget, each guessing whether
//! another had already acted. GTK runs controllers on the same widget in the order they were
//! added, which is an order nothing here could read, so the guessing was the design.
//!
//! This is the order, written down once:
//!
//! 1. **the completion popup**, which owns every key while it is up;
//! 2. **the signature popover**, which owns Escape while it is showing;
//! 3. **a template's Tab stops**, because a snippet the user is walking outranks a suggestion;
//! 4. **ghost text** (Tab, Escape, `Ctrl+Right`), while a suggestion is painted;
//! 5. **the extra carets**, which replay what they understand at every caret;
//! 6. **the markdown typing helpers** in a note (Return, Backspace, a delimiter, Tab on a list
//!    item);
//! 7. the view itself: its snippets, its indent, its bindings.

use super::{Tab, caret, line_prefix};
use crate::{ghost, signature, typing};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::rc::Rc;

/// Give `tab` its one key controller, and the popup flag every step of the chain reads.
pub(super) fn install(tab: &Rc<Tab>) {
    // Capture, so the keys the chain claims never reach the view's own bindings.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, state| dispatch(&tab, key, state)
    ));
    tab.view.add_controller(keys);

    // Whether the popup is up is two signals away and is asked for by three of the steps above,
    // so it is read from one cell rather than tracked by each of them.
    let completion = sourceview5::prelude::ViewExt::completion(&tab.view);
    completion.connect_show(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| {
            tab.popup_shown.set(true);
            ghost::clear(&tab);
        }
    ));
    completion.connect_hide(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| {
            tab.popup_shown.set(false);
            // The popup was in the way of every answer while it was up, and nothing has been
            // edited since, so there is no refresh coming: ask again here.
            glib::spawn_future_local(async move { ghost::request(&tab).await });
        }
    ));
}

/// One key press, offered to each step of the chain in turn.
fn dispatch(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    if tab.popup_shown.get() {
        return glib::Propagation::Proceed;
    }
    if let Some(answer) = signature::on_key(tab, key) {
        return answer;
    }
    // A template's stops are what Tab means while one is being walked, so the suggestion is not
    // even asked: `push_snippet` is the only thing that can be in the middle of a template.
    if !(key == gdk::Key::Tab && tab.snippet_active())
        && let Some(answer) = ghost::on_key(tab, key, state, indents_a_list(tab))
    {
        return answer;
    }
    if let Some(view) = tab.ghost_view().filter(|view| view.has_carets()) {
        return view.press(key, state);
    }
    if tab.flavour().is_note() {
        return typing::on_key(&tab.view, key, state);
    }
    glib::Propagation::Proceed
}

/// Whether the caret sits on a line that is nothing but its indent and a list marker, where Tab
/// means "indent this item" and a suggestion has no business taking the key.
fn indents_a_list(tab: &Tab) -> bool {
    tab.flavour().is_note() && typing::marker_only(&line_prefix(&tab.buffer, &caret(&tab.buffer)))
}
