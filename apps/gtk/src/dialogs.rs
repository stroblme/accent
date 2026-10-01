//! The dialog shapes DESIGN.md allows, built once: an `AdwAlertDialog` for a choice that can lose
//! data, and the name dialog every "give this a name" question shares. Cancel, one verb, no OK
//! button; header capitalisation on the buttons.

use adw::prelude::*;
use gtk::glib;
use std::cell::Cell;

/// The response id the name dialogs confirm with.
pub(crate) const CONFIRM: &str = "confirm";

/// Present `dialog` over `parent` and call `callback` once, with the response it closes on: what
/// `AlertDialogExtManual::choose` does, without its leak.
///
/// libadwaita-rs 0.9.2's `choose` hands `adw_alert_dialog_choose` a full reference where the C side
/// takes `self` as `transfer none`, so every dialog shown with it outlived its close, and with it
/// whatever its handlers held: New File's path field completes from the vault, which kept a closed
/// window's vault open. Once a release of the bindings passes `self` borrowed, this can go and the
/// callers can call `choose` again.
pub(crate) fn choose(
    dialog: &adw::AlertDialog,
    parent: Option<&impl IsA<gtk::Widget>>,
    callback: impl FnOnce(glib::GString) + 'static,
) {
    let callback = Cell::new(Some(callback));
    dialog.connect_response(None, move |_, response| {
        if let Some(callback) = callback.take() {
            callback(response.into());
        }
    });
    dialog.present(parent);
}

/// Close `dialog` on a primary press outside it, as Escape closes it: what a palette and
/// Preferences do, never an alert, whose question waits for its answer.
///
/// libadwaita closes a dialog so only as a bottom sheet: a floating one's dimming is a
/// `GtkWindowHandle`, which drags or maximises the window instead (adw-floating-sheet.c). The
/// dimming is part of the dialog, which spans the window, and while a dialog is up no press
/// reaches the window's own controllers, so the gesture is the dialog's, in the capture phase
/// ahead of the dimming. A press is outside where the widget under it is not the dialog's child
/// or inside it; a press in a popover comes from a surface of its own and never is. The press is
/// claimed, so nothing under the dimming acts on it.
pub(crate) fn close_on_outside_press(dialog: &adw::Dialog) {
    let gesture = gtk::GestureClick::new();
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    gesture.connect_pressed(|gesture, _, x, y| {
        let Some(dialog) = gesture.widget().and_downcast::<adw::Dialog>() else {
            return;
        };
        let on_dialog = gesture
            .current_event()
            .and_then(|event| event.surface())
            .is_some_and(|surface| Some(surface) == dialog.native().and_then(|n| n.surface()));
        let outside = on_dialog
            && dialog
                .pick(x, y, gtk::PickFlags::DEFAULT)
                .zip(dialog.child())
                .is_some_and(|(hit, child)| hit != child && !hit.is_ancestor(&child));
        if !outside {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        dialog.close();
    });
    dialog.add_controller(gesture);
}

/// Put the keyboard in `entry`, and then let `place` set its caret or its selection. Called after
/// `choose` has presented the dialog, so the entry is in a window that can focus it.
///
/// Every grab of the entry selects its whole text (`gtk-entry-select-on-focus`), and the dialog
/// grabs it once more on its second frame, when it opens its sheet (libadwaita's `map_tick_cb`).
/// So `place` waits for the frame after that, or what it did would be thrown away.
pub(crate) fn focus_entry(entry: &gtk::Entry, place: impl Fn(&gtk::Entry) + 'static) {
    entry.grab_focus();
    let frames = Cell::new(0);
    entry.add_tick_callback(move |entry, _| {
        frames.set(frames.get() + 1);
        if frames.get() < 3 {
            return glib::ControlFlow::Continue;
        }
        place(entry);
        glib::ControlFlow::Break
    });
}

/// An alert with its responses in one call. `responses` are `(id, label, appearance)` in the
/// order they are shown; the first one is also what closing the dialog answers, which is why it
/// is Cancel everywhere this is called. `default` is the response Return activates.
///
/// An empty `body` is no body at all, which is what a dialog whose question is entirely in its
/// extra child wants.
pub(crate) fn alert(
    heading: &str,
    body: &str,
    responses: &[(&str, &str, adw::ResponseAppearance)],
    default: &str,
) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(heading), (!body.is_empty()).then_some(body));
    for (id, label, appearance) in responses {
        dialog.add_response(id, label);
        dialog.set_response_appearance(id, *appearance);
    }
    dialog.set_default_response(Some(default));
    if let Some((close, _, _)) = responses.first() {
        dialog.set_close_response(close);
    }
    dialog
}

/// The question every "are you sure" takes: Cancel, one verb, Return and Escape on Cancel.
/// `then` runs on the verb and on nothing else.
///
/// `destructive` paints the verb in the destructive colour, which is what marks the answers that
/// lose something (DESIGN.md, States). A question with a third answer, or one whose verb is the
/// safe one, builds its own with [`alert`] and [`choose`].
pub(crate) fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    verb: &str,
    destructive: bool,
    then: impl FnOnce() + 'static,
) {
    let appearance = match destructive {
        true => adw::ResponseAppearance::Destructive,
        false => adw::ResponseAppearance::Default,
    };
    let dialog = alert(
        heading,
        body,
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            (CONFIRM, verb, appearance),
        ],
        "cancel",
    );
    choose(&dialog, Some(parent), move |response| {
        if response == CONFIRM {
            then();
        }
    });
}

/// The shared shape of the name dialogs: Cancel, one verb, and `form` as the extra child. Also
/// what the Git pane's Create Branch uses.
pub(crate) fn name_dialog(title: &str, verb: &str, form: &gtk::Box) -> adw::AlertDialog {
    name_dialog_with(title, CONFIRM, verb, form)
}

/// [`name_dialog`] under a response id of the caller's own, for the connect dialog: it answers
/// with the address rather than with a name, and says so in the id its handler reads.
pub(crate) fn name_dialog_with(
    title: &str,
    confirm: &str,
    verb: &str,
    form: &gtk::Box,
) -> adw::AlertDialog {
    let dialog = alert(
        title,
        "",
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            (confirm, verb, adw::ResponseAppearance::Suggested),
        ],
        confirm,
    );
    dialog.set_extra_child(Some(form));
    dialog
}

pub(crate) fn name_entry(placeholder: &str, text: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .text(text)
        .activates_default(true)
        .build()
}

/// 12 px between related widgets, per DESIGN.md's spacing scale.
pub(crate) fn form() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build()
}

pub(crate) fn labelled(text: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.append(
        &gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    row.append(child);
    row
}
