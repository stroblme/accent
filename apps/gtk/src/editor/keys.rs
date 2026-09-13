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
//! 3. **a template's Tab stops**, because a snippet the user is walking outranks both a
//!    suggestion and a list item's indent;
//! 4. **ghost text** (Tab, Escape, `Ctrl+Right`), while a suggestion is painted;
//! 5. **the extra carets**, which replay what they understand at every caret;
//! 6. **the markdown typing helpers** in a note (Return, Backspace, a delimiter, Tab on a list
//!    item);
//! 7. the view itself: its snippets, its indent, its bindings.

use super::Tab;
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

    // Whether the popup is up is two signals away and is asked for by three of the steps above,
    // so it is read from one cell rather than tracked by each of them. A `true` is checked against
    // the view before it is believed — see [`Tab::popup_shown`] — because a `hide` that never
    // arrived would otherwise leave every key to the view for the rest of this tab's life.
    let completion = sourceview5::prelude::ViewExt::completion(&tab.view);
    completion.connect_show(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| {
            tab.popup.set(true);
            ghost::clear(&tab);
        }
    ));
    completion.connect_hide(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| {
            tab.popup.set(false);
            // The popup was in the way of every answer while it was up, and nothing has been
            // edited since, so there is no refresh coming: ask again here. Through a flush, or the
            // answer is about the text the server was last given — which costs nothing when it
            // already has this one.
            glib::spawn_future_local(async move {
                crate::lang::flush(tab.clone()).await;
                ghost::request(&tab).await;
            });
        }
    ));
}

/// One press, driven from a drill rather than from the keyboard: what the controller above calls.
pub(crate) fn press(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    dispatch(tab, key, state)
}

/// One key press, offered to each step of the chain in turn.
fn dispatch(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    if tab.popup_shown() {
        return glib::Propagation::Proceed;
    }
    if let Some(answer) = signature::on_key(tab, key) {
        return answer;
    }
    // A template's stops are what Tab means while one is being walked, so neither the suggestion
    // nor a list item's indent is even asked: `push_snippet` is the only thing that can be in the
    // middle of a template.
    let stepping_a_template = key == gdk::Key::Tab && tab.snippet_active();
    if !stepping_a_template && let Some(answer) = ghost::on_key(tab, key, state) {
        return answer;
    }
    if let Some(view) = tab.ghost_view().filter(|view| view.has_carets()) {
        return view.press(key, state);
    }
    if tab.flavour().is_note() && !stepping_a_template {
        return typing::on_key(&tab.view, key, state);
    }
    glib::Propagation::Proceed
}

/// Whether the completion popup is on screen right now, asked of the widgets rather than of the
/// cell that mirrors the completion's two signals.
///
/// `GtkSourceCompletion` has no getter for this, so it is read the way GtkSourceView 5.20 builds
/// the popup rather than guessed (`gtksourcecompletion.c`, `gtksourceview-assistants.c`): the
/// popup is one `GtkSourceCompletionList`, a `GtkPopover` that `gtk_widget_set_parent` hangs off
/// the view, and showing and hiding it is `gtk_widget_set_visible` on that widget. So it is a
/// child of the view, and its own visible flag is the answer. The hover assistant and the
/// signature popover are children of the view too, which is why the type is read and not merely
/// "some popover is up".
pub(super) fn popup_visible(view: &sourceview5::View) -> bool {
    let mut child = view.first_child();
    while let Some(widget) = child {
        if widget.get_visible() && widget.type_().name().starts_with("GtkSourceCompletion") {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}
