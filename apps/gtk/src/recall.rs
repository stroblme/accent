//! Up and Down in a search field recall what was searched for before, the way a shell recalls
//! commands. One list of queries and one of replacements for the whole run, shared by every find
//! bar and Search pane, newest first, and kept in memory only.
//!
//! What goes into a list is what was used — a step, a replace, a result opened — not every
//! keystroke of a live search, which would fill it with the prefixes of each query.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::RefCell;
use std::thread::LocalKey;

/// How many entries a list keeps.
const CAP: usize = 50;

/// A list of what was used, newest first.
pub type List = LocalKey<RefCell<Vec<String>>>;

thread_local! {
    pub static QUERIES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    pub static REPLACEMENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Put `text` at the front of `list`, since it has just been used.
pub fn remember(list: &'static List, text: &str) {
    list.with_borrow_mut(|entries| push(entries, text));
}

/// Let Up and Down in `field` walk `list`. Plain arrows only, and taken before the field sees
/// them: a one-line text box binds them to nothing but Meta+Up and Meta+Down, so until now they
/// only moved the focus out of the box.
pub fn attach(field: &(impl IsA<gtk::Editable> + IsA<gtk::Widget>), list: &'static List) {
    let walk = RefCell::new(Walk::default());
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |keys, key, _, state| {
        let older = match key {
            gdk::Key::Up | gdk::Key::KP_Up => true,
            gdk::Key::Down | gdk::Key::KP_Down => false,
            _ => return glib::Propagation::Proceed,
        };
        if state.intersects(gtk::accelerator_get_default_mod_mask()) {
            return glib::Propagation::Proceed;
        }
        // From the controller rather than captured: the field owns this handler.
        let Some(field) = keys.widget().and_downcast::<gtk::Editable>() else {
            return glib::Propagation::Proceed;
        };
        let text =
            list.with_borrow(|entries| walk.borrow_mut().step(entries, &field.text(), older));
        if let Some(text) = text {
            field.set_text(&text);
            field.set_position(-1);
        }
        glib::Propagation::Stop
    });
    field.add_controller(keys);
}

fn push(entries: &mut Vec<String>, text: &str) {
    if text.is_empty() {
        return;
    }
    entries.retain(|entry| entry != text);
    entries.insert(0, text.to_string());
    entries.truncate(CAP);
}

/// One field's place in a list while Up and Down walk it, and what the field said when the walk
/// began: the draft, which counts as the newest entry of all, so Down comes back to it.
#[derive(Default)]
struct Walk {
    /// The entry the field is showing, or `None` for the draft.
    at: Option<usize>,
    draft: String,
}

impl Walk {
    /// What the field says after Up (`older`) or Down, or `None` where there is nothing further
    /// that way. A field typed in since the last step starts a fresh walk from what it says now.
    fn step(&mut self, entries: &[String], current: &str, older: bool) -> Option<String> {
        if self
            .at
            .is_none_or(|i| entries.get(i).is_none_or(|entry| entry != current))
        {
            self.at = None;
            self.draft = current.to_string();
        }
        // The draft first, then every entry that is not the draft over again: a query just used
        // is at the front of the list and still in the box, and Up has to reach the one before.
        let places: Vec<Option<usize>> = std::iter::once(None)
            .chain(
                (0..entries.len())
                    .filter(|&i| entries[i] != self.draft)
                    .map(Some),
            )
            .collect();
        let here = places.iter().position(|place| *place == self.at)?;
        let next = match older {
            true => here + 1,
            false => here.checked_sub(1)?,
        };
        self.at = *places.get(next)?;
        Some(match self.at {
            Some(i) => entries[i].clone(),
            None => self.draft.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_used_query_goes_to_the_front_once_and_the_list_is_capped() {
        let mut entries = list(&["b", "a"]);
        push(&mut entries, "a");
        assert_eq!(entries, list(&["a", "b"]), "moved, not repeated");
        push(&mut entries, "");
        assert_eq!(entries.len(), 2, "nothing typed is nothing to recall");
        for i in 0..CAP + 5 {
            push(&mut entries, &i.to_string());
        }
        assert_eq!(entries.len(), CAP);
        assert_eq!(entries[0], (CAP + 4).to_string(), "the newest is kept");
    }

    #[test]
    fn up_walks_back_and_down_returns_to_what_was_typed() {
        let entries = list(&["c", "b", "a"]);
        let mut walk = Walk::default();
        let mut field = "dra".to_string();
        let mut press = |older| {
            let next = walk.step(&entries, &field, older);
            if let Some(text) = &next {
                field = text.clone();
            }
            next
        };
        assert_eq!(press(false), None, "nothing is newer than the draft");
        assert_eq!(press(true).as_deref(), Some("c"));
        assert_eq!(press(true).as_deref(), Some("b"));
        assert_eq!(press(true).as_deref(), Some("a"));
        assert_eq!(press(true), None, "the oldest stays");
        assert_eq!(press(false).as_deref(), Some("b"));
        assert_eq!(press(false).as_deref(), Some("c"));
        assert_eq!(press(false).as_deref(), Some("dra"), "the draft comes back");
        assert_eq!(press(false), None);
    }

    #[test]
    fn a_query_still_in_the_box_is_the_draft_and_an_edit_starts_over() {
        let entries = list(&["foo", "bar"]);
        let mut walk = Walk::default();
        assert_eq!(walk.step(&entries, "foo", true).as_deref(), Some("bar"));
        assert_eq!(walk.step(&entries, "bar", false).as_deref(), Some("foo"));
        assert_eq!(walk.step(&entries, "foo", true).as_deref(), Some("bar"));
        // Typed into after the walk: that is the new draft, and Up starts from the newest again.
        assert_eq!(walk.step(&entries, "barx", true).as_deref(), Some("foo"));
        assert_eq!(walk.step(&entries, "foo", false).as_deref(), Some("barx"));
    }
}
