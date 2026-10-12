//! Reusable buttons over comparison views, retained across refreshes.

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

/// Assign `wanted` keys to slots in `had`: preserve matching slots, then
/// reuse shown spares before hidden ones, then allocate. Stable assignment
/// keeps buttons attached to runs across keystrokes and scroll relayouts;
/// only count changes require showing or hiding them.
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

/// Pool unchanged-run buttons for reuse on one view.
///
/// GTK 4.22 cannot publicly remove text-view overlay children, so unused buttons hide.
/// Pane ownership matches the editor view's lifetime across comparisons.
#[derive(Default)]
pub struct Pool {
    /// Current comparison action, looked up on click because pooled buttons
    /// outlive the comparison that created them.
    pub(super) act: RefCell<Option<Act>>,
    slots: RefCell<Vec<Slot>>,
}

impl Pool {
    /// Show buttons for ordered `(key, rows)` runs, reuse slots via [`hand_out`],
    /// add overlays only when needed and hide unused buttons.
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

    /// Create an overlay button invoking the current [`Pool::act`] with its
    /// current slot key. Keep the pool weak to avoid a view/button/slot cycle.
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
