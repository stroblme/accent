//! The dialog shapes DESIGN.md allows, built once: an `AdwAlertDialog` for a choice that can lose
//! data, and the name dialog every "give this a name" question shares. Cancel, one verb, no OK
//! button; header capitalisation on the buttons.

use adw::prelude::*;

/// The response id the name dialogs confirm with.
pub(crate) const CONFIRM: &str = "confirm";

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
