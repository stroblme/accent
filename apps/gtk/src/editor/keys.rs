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
//! 6. Markdown typing helpers in a note, brackets and indentation in code.
//! 7. The view's own snippets, indentation and bindings.
//!
//! Unclaimed keys continue through the chain, allowing normal typing to narrow completion.

use super::{Flavour, Tab, indent};
use crate::lang::Editor;
use crate::{completion, multicaret, signature, typing};
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::rc::Rc;

/// Give `tab` its one key controller.
pub(super) fn install(tab: &Rc<Tab>) {
    let keys = attach(&(tab.clone() as Rc<dyn Editor>));
    // Ctrl pressed over a word the pointer is already resting on: the motion controller hears
    // nothing until the pointer moves again, so the underline would wait for a jiggle.
    keys.connect_modifiers(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, state| {
            if let Some((x, y)) = tab.follow_pointer() {
                tab.follow_hint(x, y, state.contains(crate::platform::PRIMARY_MODIFIER));
            }
            glib::Propagation::Proceed
        }
    ));
}

/// Give `editor`'s view its one key controller, which offers each press to the chain below.
pub(crate) fn attach(editor: &Rc<dyn Editor>) -> gtk::EventControllerKey {
    // Capture, so the keys the chain claims never reach the view's own bindings.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    // Weak: the editor holds the view the controller is on.
    let weak = Rc::downgrade(editor);
    keys.connect_key_pressed(move |_, key, _, state| match weak.upgrade() {
        Some(editor) => dispatch(&editor, key, state),
        None => glib::Propagation::Proceed,
    });
    editor.view().add_controller(keys.clone());
    keys
}

/// One press, driven from a drill rather than from the keyboard: what the controller above calls.
#[cfg(feature = "bench")]
pub(crate) fn press(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    dispatch(&(tab.clone() as Rc<dyn Editor>), key, state)
}

/// One key press, offered to each step of the chain in turn.
fn dispatch(editor: &Rc<dyn Editor>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    if let Some(answer) = completion::press(editor.as_ref(), key, state) {
        return answer;
    }
    if let Some(answer) = signature::on_key(editor.as_ref(), key) {
        return answer;
    }
    // A template's stops are what Tab means while one is being walked, so neither the suggestion
    // nor a list item's indent is even asked: `push_snippet` is the only thing that can be in the
    // middle of a template. The view walks them, Shift+Tab back included; Escape ends the walk
    // and leaves the rest of the stops where they are.
    let walking = editor.snippet_active();
    if walking && key == gdk::Key::Escape {
        editor.end_snippet();
        return glib::Propagation::Stop;
    }
    let stepping_a_template = walking
        && matches!(
            key,
            gdk::Key::Tab | gdk::Key::KP_Tab | gdk::Key::ISO_Left_Tab
        );
    if !stepping_a_template && let Some(answer) = editor.clone().ghost_key(key, state) {
        return answer;
    }
    let view = editor.view();
    if let Some(answer) = carets(view, key, state) {
        return answer;
    }
    match stepping_a_template {
        true => glib::Propagation::Proceed,
        false => helpers(view, editor.flavour(), key, state),
    }
}

/// The chain's last steps, which ask nothing of a language: the secondary carets, then a note's
/// typing helpers or code's brackets and indentation. A notebook's markdown cell, which has no
/// document on the language layer, takes these alone.
pub(crate) fn typed(
    view: &sourceview5::View,
    flavour: Flavour,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> glib::Propagation {
    carets(view, key, state).unwrap_or_else(|| helpers(view, flavour, key, state))
}

/// A press for a column of secondary carets, if there is one.
fn carets(
    view: &sourceview5::View,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> Option<glib::Propagation> {
    let view = view
        .downcast_ref::<multicaret::View>()
        .filter(|view| view.has_carets())?;
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
    Some(view.press(key, state))
}

/// Markdown typing helpers in a note, brackets and indentation in code.
fn helpers(
    view: &sourceview5::View,
    flavour: Flavour,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> glib::Propagation {
    match flavour {
        Flavour::Note => typing::on_key(view, key, state),
        Flavour::Code => match typing::on_code_key(view, key, state) {
            glib::Propagation::Proceed => indent::on_key(view, key, state),
            stop => stop,
        },
        Flavour::Csv(_) => glib::Propagation::Proceed,
    }
}
