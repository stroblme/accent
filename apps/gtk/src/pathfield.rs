//! A path entry with the folders it could go into listed under it as it is typed.
//!
//! Not file operations: the connect dialog completes a path on another machine with the same
//! widget, and the only thing the two callers share is the shape of the list.

use gtk::prelude::*;
use gtk::{gdk, glib};

/// How many rows Page Up and Page Down move by. A completion list is at most
/// [`COMPLETIONS`] long and shows five or six of them at a time, so a page is a screenful.
const PAGE: usize = 5;
/// How many folders a path entry offers at once before the list stops.
const COMPLETIONS: usize = 12;

/// The whole texts a half-typed path could be completed to: every name in `folders` that carries
/// on from the last segment, with the rest of the path kept in front of it and a `/` on the end so
/// the next segment can be typed straight away.
///
/// Folders only. The last segment is the file's own name, which is being invented rather than
/// looked up, so nothing can complete it and a file would only be a name to collide with.
pub(crate) fn completions(typed: &str, folders: &[String]) -> Vec<String> {
    let typed = typed.trim();
    let (head, leaf) = match typed.rsplit_once('/') {
        Some((_, leaf)) => (&typed[..typed.len() - leaf.len()], leaf),
        None => ("", typed),
    };
    let leaf = leaf.to_lowercase();
    folders
        .iter()
        .filter(|name| name.to_lowercase().starts_with(&leaf))
        .take(COMPLETIONS)
        .map(|name| format!("{head}{name}/"))
        .collect()
}

/// What a key pressed in a path entry asks of its completion list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Show a list that is built but put away.
    Open,
    /// Move the highlight, or take it off and go back to what was typed.
    Select(Option<usize>),
    /// Put a candidate in the entry.
    Apply(usize),
    /// Put the list away without touching the entry.
    Close,
    /// Not the list's key: the entry, the dialog or the window has it.
    Pass,
}

/// Which of those a key press asks for, from the list's state alone.
///
/// Return applies only where a row has been arrowed to; with nothing selected it falls through to
/// the dialog's default response, so an entry holding a finished path still confirms on Return
/// rather than growing a trailing slash. Tab is the accept that needs no aim, Escape closes the
/// list before it closes the dialog, and anything held with a modifier is somebody else's chord.
fn completion_key(
    key: gdk::Key,
    mods: gdk::ModifierType,
    offers: usize,
    open: bool,
    selected: Option<usize>,
) -> Step {
    let chord = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK;
    if offers == 0 || mods.intersects(chord) {
        return Step::Pass;
    }
    let last = offers - 1;
    match key {
        gdk::Key::Down | gdk::Key::KP_Down if !open => Step::Open,
        gdk::Key::Down | gdk::Key::KP_Down => {
            Step::Select(Some(selected.map_or(0, |i| (i + 1).min(last))))
        }
        gdk::Key::Up | gdk::Key::KP_Up => Step::Select(selected.and_then(|i| i.checked_sub(1))),
        // A page and the two ends, on a list that is open. They walk within the offers rather
        // than off the top of them: the way back to what was typed is Up from the first row.
        gdk::Key::Page_Down | gdk::Key::KP_Page_Down if open => {
            Step::Select(Some(selected.map_or(0, |i| (i + PAGE).min(last))))
        }
        gdk::Key::Page_Up | gdk::Key::KP_Page_Up if open => {
            Step::Select(Some(selected.map_or(0, |i| i.saturating_sub(PAGE))))
        }
        gdk::Key::Home | gdk::Key::KP_Home if open => Step::Select(Some(0)),
        gdk::Key::End | gdk::Key::KP_End if open => Step::Select(Some(last)),
        gdk::Key::Return | gdk::Key::KP_Enter => selected.map_or(Step::Pass, Step::Apply),
        gdk::Key::Tab | gdk::Key::KP_Tab | gdk::Key::ISO_Left_Tab if open => {
            Step::Apply(selected.unwrap_or(0))
        }
        gdk::Key::Escape if open => Step::Close,
        _ => Step::Pass,
    }
}

/// The scroll offset that brings a row at `top..top + height` into a `page`-tall view showing
/// `value` onwards: as little movement as shows the row, and nothing at all when it is in view.
fn scroll_to(value: f64, page: f64, top: f64, height: f64) -> f64 {
    if top < value {
        top
    } else if top + height > value + page {
        top + height - page
    } else {
        value
    }
}

/// A path entry with the folders it could go into listed under it as it is typed.
///
/// GTK4 deprecated `GtkEntryCompletion` and shipped nothing in its place. A popover is the shape
/// `start::host_field` reaches for, but not here: a completion list stays up while the keyboard is
/// still in the entry, and in a dialog this small every popover GTK will fit lands on top of
/// Cancel and Rename. So the list is a revealer inside the form — it pushes the buttons down
/// instead of covering them, and it needs no grab, no hand-parenting and no guessing about where
/// there is room. The folder button beside the entry is the same list on demand, for an entry
/// nothing is typed in yet.
///
/// The keyboard stays in the entry and drives the list from there: Down opens it and steps into
/// it, the arrows walk it, Tab takes the selected row or else the first, Return takes a row that
/// was arrowed to and otherwise leaves the dialog to confirm itself, and Escape puts the list away
/// before it closes anything. [`completion_key`] is that rule, on its own and tested.
///
/// The entry expands because the list is what decides how wide the dialog is: the candidates set
/// the scroller's minimum width and it reaches the row, so an entry that did not take that width
/// would sit at its natural size with the surplus dead beside it.
///
/// `complete` answers with whole texts the entry could hold, so every bit of path arithmetic stays
/// with the caller. It may answer with nothing while it is still finding out — a listing on a
/// worker thread, or a host that has not replied — and [`look_again`] is how it comes back once it
/// knows.
pub(crate) fn path_field(
    entry: &gtk::Entry,
    tooltip: &str,
    complete: impl Fn(&str) -> Vec<String> + 'static,
) -> gtk::Widget {
    // A `GtkListBox` rather than a column of buttons: it paints the selected row itself, where a
    // button would want a stylesheet the app does not otherwise have, and it is one tab stop
    // instead of twelve. The rows are the only record of what is on offer.
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    // A deep vault is a list that scrolls rather than a dialog taller than the window.
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(160)
        .child(&list)
        .build();
    scroller.add_css_class("card");
    let revealer = gtk::Revealer::builder().child(&scroller).build();
    let button = gtk::ToggleButton::builder()
        .icon_name("folder-symbolic")
        .tooltip_text(tooltip)
        .build();
    button
        .bind_property("active", &revealer, "reveal-child")
        .bidirectional()
        .sync_create()
        .build();

    entry.connect_changed({
        let (list, button) = (list.clone(), button.clone());
        move |entry| {
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            let offers = complete(&entry.text());
            for candidate in &offers {
                list.append(
                    &gtk::ListBoxRow::builder()
                        .child(&gtk::Label::builder().label(candidate).xalign(0.0).build())
                        .build(),
                );
            }
            show_completions(&button, entry, offers.is_empty());
        }
    });
    list.connect_row_activated({
        // Weak: the entry owns this list through its own handler, so a row holding it back would
        // be a cycle that outlives the dialog.
        let asked = entry.downgrade();
        move |_, row| {
            let Some(candidate) = candidate(row) else {
                return;
            };
            // Setting the text rebuilds this very list, so it happens once the click is over —
            // the same reason `popup` unparents its menu from an idle. The focus goes back first,
            // so the rebuilt list is the new folder's.
            let asked = asked.clone();
            glib::idle_add_local_once(move || {
                if let Some(entry) = asked.upgrade() {
                    entry.grab_focus_without_selecting();
                    entry.set_text(&candidate);
                    entry.set_position(-1);
                }
            });
        }
    });

    // Capture, so Return arrives before `GtkText`'s own binding turns it into the dialog's
    // default response: that binding sits below this controller, which is why `ghost` captures
    // Tab and `palette` captures Escape. Nothing here fires while the keyboard is elsewhere.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        entry,
        #[weak]
        list,
        #[weak]
        scroller,
        #[weak]
        button,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, mods| on_key(&entry, &list, &scroller, &button, key, mods)
    ));
    entry.add_controller(keys);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("linked");
    entry.set_hexpand(true);
    row.append(entry);
    row.append(&button);
    // 6 px inside a control group, per DESIGN.md's spacing scale: the list belongs to the entry.
    let field = gtk::Box::new(gtk::Orientation::Vertical, 6);
    field.append(&row);
    field.append(&revealer);
    field.upcast()
}

/// The whole text a completion row stands for: the label it was built from, which is the only
/// place the candidate is kept.
fn candidate(row: &gtk::ListBoxRow) -> Option<String> {
    row.child()
        .and_downcast::<gtk::Label>()
        .map(|label| label.label().into())
}

/// One key pressed with the keyboard in a path entry, answered by [`completion_key`].
fn on_key(
    entry: &gtk::Entry,
    list: &gtk::ListBox,
    scroller: &gtk::ScrolledWindow,
    button: &gtk::ToggleButton,
    key: gdk::Key,
    mods: gdk::ModifierType,
) -> glib::Propagation {
    let offers = list.observe_children().n_items() as usize;
    let selected = list.selected_row().map(|row| row.index() as usize);
    match completion_key(key, mods, offers, button.is_active(), selected) {
        Step::Pass => return glib::Propagation::Proceed,
        Step::Open => {
            button.set_active(true);
            aim(list, scroller, Some(0));
        }
        Step::Select(to) => aim(list, scroller, to),
        Step::Apply(i) => {
            let Some(text) = list.row_at_index(i as i32).as_ref().and_then(candidate) else {
                return glib::Propagation::Proceed;
            };
            // Not deferred the way a click is: this is nobody's signal emission, so the rebuild
            // that setting the text sets off is safe to let happen here and now.
            entry.set_text(&text);
            entry.set_position(-1);
        }
        Step::Close => {
            button.set_active(false);
            aim(list, scroller, None);
        }
    }
    glib::Propagation::Stop
}

/// Move the highlight to `to`, and scroll only as far as it takes to see it.
fn aim(list: &gtk::ListBox, scroller: &gtk::ScrolledWindow, to: Option<usize>) {
    let row = to.and_then(|i| list.row_at_index(i as i32));
    list.select_row(row.as_ref());
    // Bounds in the list's own coordinates, which is what the scroller's adjustment counts in.
    let Some(seen) = row.and_then(|row| row.compute_bounds(list)) else {
        return;
    };
    let adjustment = scroller.vadjustment();
    adjustment.set_value(scroll_to(
        adjustment.value(),
        adjustment.page_size(),
        f64::from(seen.y()),
        f64::from(seen.height()),
    ));
}

/// Run a path entry's completion again, for an answer that arrived after the keystroke that asked
/// for it. The entry's own `changed` is the one path everything watching it already takes — the
/// list, and in the connect dialog the check that enables Connect — so a late answer needs no
/// second channel, and nothing has to hold a closure that would hold it back.
pub(crate) fn look_again(entry: &gtk::Entry) {
    entry.emit_by_name::<()>("changed", &[]);
}

/// `FOCUS_WITHIN` rather than `has_focus`, which is always false here: a GTK4 `GtkEntry` is a
/// wrapper whose inner `GtkText` is the widget that actually takes the keyboard.
pub(crate) fn typing_here(entry: &gtk::Entry) -> bool {
    entry.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN)
}

/// The list shows itself in answer to typing, not over a dialog nobody has touched: it opens only
/// while the entry has the keyboard and holds something to complete. An entry that is still empty
/// keeps its folders behind the button, which is insensitive when there are none.
fn show_completions(button: &gtk::ToggleButton, entry: &gtk::Entry, empty: bool) {
    button.set_sensitive(!empty);
    button.set_active(!empty && typing_here(entry) && !entry.text().trim().is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_offer_folders_and_keep_the_path_in_front_of_them() {
        let folders: Vec<String> = ["Archive", "Attachments", "Notes", "notes-old"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // The last segment is what is being completed; a `/` says the next one is starting.
        assert_eq!(completions("Arc", &folders), ["Archive/".to_string()]);
        assert_eq!(completions("At", &folders), ["Attachments/".to_string()]);
        // Case-insensitive, and every match is offered.
        assert_eq!(
            completions("NOT", &folders),
            ["Notes/".to_string(), "notes-old/".to_string()]
        );
        // The path already typed is kept, so a click leaves a whole path in the entry.
        assert_eq!(
            completions("../Deep/Arc", &folders),
            ["../Deep/Archive/".to_string()]
        );
        assert_eq!(completions("Deep/", &folders).len(), folders.len());
        // Nothing typed offers every folder; a name nothing starts with offers none.
        assert_eq!(completions("", &folders).len(), folders.len());
        assert!(completions("zzz", &folders).is_empty());
        // A folder whose name was typed in full still earns its trailing slash, beside anything
        // else that carries on from it.
        assert_eq!(
            completions("Notes", &folders),
            ["Notes/".to_string(), "notes-old/".to_string()]
        );
    }

    #[test]
    fn completion_key_walks_the_offers_and_stops_at_the_ends() {
        let key = |k, open, sel| completion_key(k, gdk::ModifierType::empty(), 3, open, sel);
        // Down opens a list that is closed; after that it walks and stops on the last row.
        assert_eq!(key(gdk::Key::Down, false, None), Step::Open);
        assert_eq!(key(gdk::Key::Down, true, None), Step::Select(Some(0)));
        assert_eq!(key(gdk::Key::Down, true, Some(1)), Step::Select(Some(2)));
        assert_eq!(key(gdk::Key::Down, true, Some(2)), Step::Select(Some(2)));
        // Up walks back off the top, to the text that was typed.
        assert_eq!(key(gdk::Key::KP_Up, true, Some(1)), Step::Select(Some(0)));
        assert_eq!(key(gdk::Key::Up, true, Some(0)), Step::Select(None));
        assert_eq!(key(gdk::Key::Up, true, None), Step::Select(None));
    }

    #[test]
    fn a_page_and_the_two_ends_walk_the_open_list() {
        let key = |k, open, sel| completion_key(k, gdk::ModifierType::empty(), 12, open, sel);
        assert_eq!(key(gdk::Key::Page_Down, true, None), Step::Select(Some(0)));
        assert_eq!(
            key(gdk::Key::Page_Down, true, Some(0)),
            Step::Select(Some(5))
        );
        // Neither end is ever walked past.
        assert_eq!(
            key(gdk::Key::Page_Down, true, Some(9)),
            Step::Select(Some(11))
        );
        assert_eq!(key(gdk::Key::Page_Up, true, Some(2)), Step::Select(Some(0)));
        assert_eq!(key(gdk::Key::Home, true, Some(7)), Step::Select(Some(0)));
        assert_eq!(key(gdk::Key::KP_End, true, None), Step::Select(Some(11)));
        // With the list away they are the entry's own: Home and End move the caret.
        assert_eq!(key(gdk::Key::Home, false, None), Step::Pass);
        assert_eq!(key(gdk::Key::Page_Down, false, None), Step::Pass);
    }

    #[test]
    fn return_applies_only_a_row_that_was_aimed_at() {
        let key = |k, sel| completion_key(k, gdk::ModifierType::empty(), 3, true, sel);
        // The rule the dialog depends on: with nothing selected, Return is the dialog's.
        assert_eq!(key(gdk::Key::Return, None), Step::Pass);
        assert_eq!(key(gdk::Key::KP_Enter, None), Step::Pass);
        assert_eq!(key(gdk::Key::Return, Some(1)), Step::Apply(1));
        assert_eq!(key(gdk::Key::KP_Enter, Some(0)), Step::Apply(0));
    }

    #[test]
    fn tab_takes_the_first_offer_and_escape_closes_the_list() {
        let key = |k, open, sel| completion_key(k, gdk::ModifierType::empty(), 3, open, sel);
        assert_eq!(key(gdk::Key::Tab, true, None), Step::Apply(0));
        assert_eq!(key(gdk::Key::ISO_Left_Tab, true, Some(2)), Step::Apply(2));
        assert_eq!(key(gdk::Key::Escape, true, Some(0)), Step::Close);
        // With the list away both belong to the dialog: focus moves, and Escape closes it.
        assert_eq!(key(gdk::Key::Tab, false, None), Step::Pass);
        assert_eq!(key(gdk::Key::Escape, false, None), Step::Pass);
    }

    #[test]
    fn completion_key_leaves_chords_typing_and_an_empty_list_alone() {
        let keys = [
            gdk::Key::Down,
            gdk::Key::Up,
            gdk::Key::Return,
            gdk::Key::Tab,
            gdk::Key::Escape,
        ];
        for mods in [
            gdk::ModifierType::CONTROL_MASK,
            gdk::ModifierType::ALT_MASK,
            gdk::ModifierType::SUPER_MASK,
        ] {
            for key in keys {
                assert_eq!(completion_key(key, mods, 3, true, Some(0)), Step::Pass);
            }
        }
        let none = gdk::ModifierType::empty();
        for key in keys {
            assert_eq!(completion_key(key, none, 0, true, Some(0)), Step::Pass);
        }
        // Every other key is someone typing.
        assert_eq!(
            completion_key(gdk::Key::a, none, 3, true, Some(0)),
            Step::Pass
        );
    }

    #[test]
    fn scroll_to_moves_only_as_far_as_it_has_to() {
        // A row already in view leaves the scroll where it is.
        assert_eq!(scroll_to(0.0, 100.0, 20.0, 25.0), 0.0);
        assert_eq!(scroll_to(50.0, 100.0, 60.0, 25.0), 50.0);
        // Above the view, its top; below it, just enough to show its bottom.
        assert_eq!(scroll_to(50.0, 100.0, 20.0, 25.0), 20.0);
        assert_eq!(scroll_to(0.0, 100.0, 90.0, 25.0), 15.0);
    }
}
