//! The buttons a comparison lays over its views, kept for reuse from one refresh to the next.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What a press does: handed the key of the hidden run its button opens right now.
pub(super) type Act = Rc<dyn Fn(usize)>;

struct Slot {
    button: gtk::Button,
    /// The key of the hidden run the button opens.
    key: Rc<Cell<usize>>,
}

/// Which slot each of `wanted` goes to, `had` naming what each slot shows now (`None`, hidden):
/// the one showing it already, or else a spare one, shown ones before hidden ones, and new ones
/// past the end once those run out. So a button stays with what it shows while that is wanted,
/// and is shown or hidden only as the number wanted changes: a keystroke lays every button again,
/// and a scroll lays those near the view.
pub(super) fn hand_out<K: PartialEq>(had: &[Option<K>], wanted: &[K]) -> Vec<usize> {
    let kept: Vec<Option<usize>> = (wanted.iter())
        .map(|key| had.iter().position(|h| h.as_ref() == Some(key)))
        .collect();
    let spare = (0..had.len()).filter(|&i| had[i].is_some() && !kept.contains(&Some(i)));
    let hidden = (0..had.len()).filter(|&i| had[i].is_none());
    let mut spare = spare.chain(hidden).chain(had.len()..);
    kept.iter()
        .filter_map(|k| k.or_else(|| spare.next()))
        .collect()
}

/// The "⋯ N unchanged lines" buttons laid over one view, kept for reuse.
///
/// GTK 4.22 has no public way to take an overlay back off a text view — `gtk_text_view_remove`
/// knows the anchored children and the four border children, and warns for anything else — so
/// a button that is not needed is hidden, and the next refresh picks it up again. The pool is
/// owned by the pane and not by the comparison, because the editor's view outlives every
/// comparison it hosts and the buttons parented to it have to as well.
#[derive(Default)]
pub struct Pool {
    /// What the comparison laid over the view right now does with a press, asked at the press:
    /// the buttons outlive the comparison that made them.
    pub(super) act: RefCell<Option<Act>>,
    slots: RefCell<Vec<Slot>>,
}

impl Pool {
    /// A shown button for each of `runs`, `(key, rows)` the hidden run keyed `key`, `rows` long,
    /// in order, laid over `view` where the pool has too few, and every other button hidden: see
    /// [`hand_out`].
    pub(super) fn claim(
        self: &Rc<Self>,
        view: &sourceview5::View,
        runs: &[(usize, usize)],
    ) -> Vec<gtk::Widget> {
        let mut slots = self.slots.borrow_mut();
        let had: Vec<Option<usize>> = (slots.iter())
            .map(|slot| slot.button.is_visible().then(|| slot.key.get()))
            .collect();
        let keys: Vec<usize> = runs.iter().map(|&(key, _)| key).collect();
        let given = hand_out(&had, &keys);
        while slots.len() < given.iter().map(|&at| at + 1).max().unwrap_or(0) {
            slots.push(self.build(view));
        }
        for (at, slot) in slots.iter().enumerate() {
            if !given.contains(&at) {
                slot.button.set_visible(false);
            }
        }
        (given.iter().zip(runs))
            .map(|(&at, &(key, rows))| {
                let slot = &slots[at];
                slot.button.set_label(&format!("⋯ {rows} unchanged lines"));
                slot.key.set(key);
                slot.button.set_visible(true);
                slot.button.clone().upcast()
            })
            .collect()
    }

    /// How many buttons are shown.
    pub(super) fn shown(&self) -> usize {
        let slots = self.slots.borrow();
        slots.iter().filter(|slot| slot.button.is_visible()).count()
    }

    /// A new button laid over `view`. Its press runs the comparison's [`Pool::act`] as it is at
    /// the press, with the key the slot holds then; weak on the pool, because the button is a
    /// child of the view the pool's slots hold.
    fn build(self: &Rc<Self>, view: &sourceview5::View) -> Slot {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("caption");
        button.set_tooltip_text(Some("Show these lines"));
        crate::widgets::claim_press(&button);
        let (pool, key) = (Rc::downgrade(self), Rc::new(Cell::new(0)));
        let pressed = key.clone();
        button.connect_clicked(move |_| {
            if let Some(act) = pool.upgrade().and_then(|p| p.act.borrow().clone()) {
                act(pressed.get());
            }
        });
        view.add_overlay(&button, 0, 0);
        Slot { button, key }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_button_stays_with_what_it_shows() {
        let had = [Some(10), Some(20), None, Some(30)];
        assert_eq!(hand_out(&had, &[20, 30]), [1, 3], "kept, the rest hidden");
        assert_eq!(
            hand_out(&had, &[20, 30, 40]),
            [1, 3, 0],
            "a newcomer takes the one let go of, which stays shown"
        );
        assert_eq!(hand_out(&had, &[10, 20, 30, 40, 50]), [0, 1, 3, 2, 4]);
        assert!(hand_out::<usize>(&[], &[]).is_empty());
    }
}
