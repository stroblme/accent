//! The dialog shapes DESIGN.md allows, built once: an `AdwAlertDialog` for a choice that can lose
//! data, and the name dialog every "give this a name" question shares. Cancel, one verb, no OK
//! button; header capitalisation on the buttons.

use adw::prelude::*;
use gtk::glib;
use std::cell::Cell;

/// The response id the name dialogs confirm with.
pub(crate) const CONFIRM: &str = "confirm";

/// Present `dialog` over `parent` and call `callback` once with its closing response.
///
/// libadwaita-rs 0.9.2's `choose` passes a full reference where C expects `transfer none`,
/// leaking dialogs and captured resources. Remove this workaround once the binding passes
/// a borrowed reference.
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

/// Dismiss non-alert dialogs on a primary press outside, matching Escape.
///
/// Libadwaita only does this for bottom sheets; floating dimming handles window drags.
/// A dialog-local capture gesture claims outside presses before the dimming can act.
/// Popover presses come from another surface and are excluded. Alerts still require answers.
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

/// Focus `entry` after presentation, then apply `place` to its caret or selection.
///
/// Entry focus selects all text, and libadwaita grabs focus again on its second frame.
/// Waiting until the following frame prevents that grab from overwriting `place`.
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

/// Build an alert with `(id, label, appearance)` responses in display order.
///
/// The first response is also the close response, so callers put Cancel first. Return
/// activates `default`. An empty `body` leaves room for a question in the extra child.
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

/// Ask with Cancel and one verb; Return and Escape cancel, and only the verb runs `then`.
///
/// `destructive` marks answers that lose data. Use [`alert`] and [`choose`] for three answers
/// or a safe default verb.
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
