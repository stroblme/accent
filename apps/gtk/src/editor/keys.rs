//! One key controller on the view, and the order the modules see a press in.
//!
//! Five things in this editor want the same handful of keys — Tab, Escape, Return — and they used
//! to take them through four capture-phase controllers on one widget, each guessing whether
//! another had already acted. GTK runs controllers on the same widget newest first, which is an
//! order nothing here could read, so the guessing was the design.
//!
//! This is the order, written down once:
//!
//! 1. **the completion popup**, which owns every key while it is up, except Return and Tab while
//!    no row in it is selected, and Escape puts away and stops there (on the scroller, see
//!    [`install`]);
//! 2. **the signature popover**, which owns Escape while it is showing;
//! 3. **a template's Tab stops** (Tab, and Escape to stop walking them), because a snippet the
//!    user is walking outranks both a suggestion and a list item's indent;
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
    // Escape over the popup, a level up: GtkSourceView hides the popup on Escape from a capture
    // controller of its own on the view and lets the press go on ("still propagate after
    // hiding", `gtksourcecompletionlist.c`), and being added once the popup first shows, that
    // controller runs ahead of the one above. Let on, the press reached the window's Escape
    // (`wire::dismiss`), which closed the find bar or the comparison under the popup with it.
    // Hidden here as GtkSourceView hides it, so no `hide` asks for a suggestion in its place.
    let escape = gtk::EventControllerKey::new();
    escape.set_propagation_phase(gtk::PropagationPhase::Capture);
    escape.connect_key_pressed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| match popup(&tab.view).filter(|_| key == gdk::Key::Escape) {
            Some(popup) => {
                popup.set_visible(false);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    ));
    tab.scroller.add_controller(escape);

    // Whether a popup is up is read off the widgets on each press ([`Tab::popup_shown`]), so
    // these two are only what happens either side of one: the suggestion goes when a popup takes
    // the keyboard, and the answers it was in the way of are asked for again when it lets go.
    let completion = sourceview5::prelude::ViewExt::completion(&tab.view);
    completion.connect_show(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| ghost::clear(&tab)
    ));
    completion.connect_hide(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| {
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
#[cfg(feature = "bench")]
pub(crate) fn press(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    dispatch(tab, key, state)
}

/// One key press, offered to each step of the chain in turn.
fn dispatch(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    let Some(popup) = popup(&tab.view) else {
        return beneath_popup(tab, key, state);
    };
    // The popup takes Return and Tab only to accept its selected row (`activate_nth_cb` in
    // `gtksourcecompletionlistbox.c`); with none selected it lets them through to the view bare,
    // past the list helpers below, so a list item lost its marker and Tab typed a tab after it.
    // With no row selected they are the editor's, as in VS Code and Obsidian, and the popup goes.
    let editors = matches!(
        key,
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab | gdk::Key::KP_Tab
    );
    if !editors || row_selected(&popup) {
        return glib::Propagation::Proceed;
    }
    // Blocking cancels the popup, and holding the block over the edit keeps that edit from
    // bringing it straight back: a one-character insert with a word before the caret, the tab a
    // tab-indented item gets, would otherwise start a new completion.
    let completion = sourceview5::prelude::ViewExt::completion(&tab.view);
    completion.block_interactive();
    let answer = beneath_popup(tab, key, state);
    completion.unblock_interactive();
    answer
}

/// The chain below the popup: everything a press means when no popup takes it.
fn beneath_popup(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
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
    glib::Propagation::Proceed
}

/// Whether the completion popup is on screen right now, asked of the widgets every time.
///
/// `GtkSourceCompletion` has no getter for this, so it is read the way GtkSourceView 5.20 builds
/// the popup rather than guessed (`gtksourcecompletion.c`, `gtksourceview-assistants.c`): the
/// popup is one `GtkSourceCompletionList`, a `GtkPopover` that `gtk_widget_set_parent` hangs off
/// the view, and showing and hiding it is `gtk_widget_set_visible` on that widget. So it is a
/// child of the view. The hover assistant and the signature popover are children of the view too,
/// which is why the type is read and not merely "some popover is up".
///
/// Mapped rather than visible: only a popup on screen can hold the keyboard, and mapped is what
/// the widget hierarchy itself says about being on screen rather than a flag someone set and may
/// not have taken back. The two agree in everything `ACCENT_BENCH_KEYS` can stage — GTK pops the
/// popover down when the view goes out from under it, and the drill prints both flags either side
/// of that — so this is only the one of them that cannot be left behind. A handful of widgets,
/// walked once per press, and the cheap flag is read before the type name.
pub(super) fn popup_visible(view: &sourceview5::View) -> bool {
    popup(view).is_some()
}

/// The completion popup, while it is on screen: see [`popup_visible`].
fn popup(view: &sourceview5::View) -> Option<gtk::Widget> {
    children(view.upcast_ref())
        .find(|w| w.is_mapped() && w.type_().name().starts_with("GtkSourceCompletion"))
}

/// Whether `popup` shows a row selected, which is what Return and Tab accept. None is until an
/// arrow key or the pointer picks one (`select-on-show` is off). The popup has no getter for it,
/// and its list's `proposal` property raises a critical when nothing is selected (5.20 reads the
/// item at -1), so it is read off the row the list paints selected: the list sets that state from
/// its selection on the next frame, which is sooner than a second key press.
fn row_selected(popup: &gtk::Widget) -> bool {
    fn any_selected(widget: &gtk::Widget) -> bool {
        children(widget).any(|w| {
            (w.type_().name() == "GtkSourceCompletionListBoxRow"
                && w.state_flags().contains(gtk::StateFlags::SELECTED))
                || any_selected(&w)
        })
    }
    any_selected(popup)
}

/// A widget's children, internal ones included.
fn children(widget: &gtk::Widget) -> impl Iterator<Item = gtk::Widget> {
    std::iter::successors(widget.first_child(), |w| w.next_sibling())
}
