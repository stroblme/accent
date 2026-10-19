//! Dispatch editor keys in an explicit priority order.
//!
//! Separate capture controllers depend on GTK's newest-first installation order, making
//! ownership of shared keys unclear. One controller offers each press to:
//!
//! 1. Completion: arrows, Escape, and Return or Tab with a selected row.
//! 2. Signature help: Escape while visible.
//! 3. Template Tab stops: Tab and Escape.
//! 4. Ghost text: Tab, Escape and Ctrl+Right while painted.
//! 5. Secondary carets: supported edits and motions at every caret.
//! 6. Markdown typing helpers in a note, indentation in code.
//! 7. The view's own snippets, indentation and bindings.
//!
//! Unclaimed keys continue through the chain, allowing normal typing to narrow completion.

use super::{Flavour, Tab, indent};
use crate::{completion, ghost, signature, typing};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::rc::Rc;

/// Give `tab` its one key controller.
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
    // Ctrl pressed over a word the pointer is already resting on: the motion controller hears
    // nothing until the pointer moves again, so the underline would wait for a jiggle.
    keys.connect_modifiers(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, state| {
            if let Some((x, y)) = tab.follow_pointer() {
                tab.follow_hint(x, y, state.contains(gdk::ModifierType::CONTROL_MASK));
            }
            glib::Propagation::Proceed
        }
    ));
    tab.view.add_controller(keys);
}

/// One press, driven from a drill rather than from the keyboard: what the controller above calls.
#[cfg(feature = "bench")]
pub(crate) fn press(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    dispatch(tab, key, state)
}

/// One key press, offered to each step of the chain in turn.
fn dispatch(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    if let Some(answer) = completion::press(tab, key, state) {
        return answer;
    }
    if let Some(answer) = signature::on_key(tab, key) {
        return answer;
    }
    // A template's stops are what Tab means while one is being walked, so neither the suggestion
    // nor a list item's indent is even asked: `push_snippet` is the only thing that can be in the
    // middle of a template. The view walks them, Shift+Tab back included; Escape ends the walk
    // and leaves the rest of the stops where they are.
    let walking = tab.snippet_active();
    if walking && key == gdk::Key::Escape {
        tab.end_snippet();
        return glib::Propagation::Stop;
    }
    let stepping_a_template = walking
        && matches!(
            key,
            gdk::Key::Tab | gdk::Key::KP_Tab | gdk::Key::ISO_Left_Tab
        );
    if !stepping_a_template && let Some(answer) = ghost::on_key(tab, key, state) {
        return answer;
    }
    if let Some(view) = tab.ghost_view().filter(|view| view.has_carets()) {
        // A column of carets takes Return and Tab before the list helpers below ever see them,
        // which is one of the ways a note stops continuing its lists (NOTEPAD). A column nobody
        // meant to leave behind looks like no column at all on screen, so say where the carets
        // are: the primary one first, then the extras.
        if matches!(
            key,
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab | gdk::Key::KP_Tab
        ) {
            tracing::debug!(
                key = key.name().as_deref().unwrap_or("?"),
                carets = ?view.caret_positions(),
                "a column of carets answers the key, not the list helpers"
            );
        }
        return view.press(key, state);
    }
    if tab.flavour().is_note() && !stepping_a_template {
        return typing::on_key(&tab.view, key, state);
    }
    if tab.flavour() == Flavour::Code && !stepping_a_template {
        return indent::on_key(&tab.view, key, state);
    }
    glib::Propagation::Proceed
}
