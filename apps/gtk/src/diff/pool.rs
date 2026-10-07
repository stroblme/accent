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
    /// Whether the refresh under way has handed this slot out.
    claimed: Cell<bool>,
}

/// The "⋯ N unchanged lines" buttons laid over one view, kept for reuse.
///
/// GTK 4.22 has no public way to take an overlay back off a text view — `gtk_text_view_remove`
/// knows the anchored children and the four border children, and warns for anything else — so
/// a button that is not needed is hidden, and the next refresh picks it up again. The pool is
/// owned by the pane and not by the comparison, because the editor's view outlives every
/// comparison it hosts and the buttons parented to it have to as well.
///
/// A refresh hands the buttons out again in order, [`Pool::unclaim`] to [`Pool::hide_unclaimed`],
/// so the button a run had is the one it gets back, and one still wanted is never hidden in
/// between: a keystroke re-diffs, and must not take every button off and put it back.
#[derive(Default)]
pub struct Pool {
    /// What the comparison laid over the view right now does with a press, asked at the press:
    /// the buttons outlive the comparison that made them.
    pub(super) act: RefCell<Option<Act>>,
    slots: RefCell<Vec<Slot>>,
}

impl Pool {
    /// A shown button for the hidden run keyed `key`, `rows` long: the first one this refresh has
    /// not handed out yet, or a new one laid over `view`.
    pub(super) fn claim(
        self: &Rc<Self>,
        view: &sourceview5::View,
        key: usize,
        rows: usize,
    ) -> gtk::Widget {
        let mut slots = self.slots.borrow_mut();
        let at = match slots.iter().position(|slot| !slot.claimed.get()) {
            Some(at) => at,
            None => {
                slots.push(self.build(view));
                slots.len() - 1
            }
        };
        let slot = &slots[at];
        slot.button.set_label(&format!("⋯ {rows} unchanged lines"));
        slot.key.set(key);
        slot.claimed.set(true);
        slot.button.set_visible(true);
        slot.button.clone().upcast()
    }

    /// Every button up for claiming again. None is hidden yet.
    pub(super) fn unclaim(&self) {
        for slot in self.slots.borrow().iter() {
            slot.claimed.set(false);
        }
    }

    /// Hide every button nothing has claimed since [`Pool::unclaim`].
    pub(super) fn hide_unclaimed(&self) {
        for slot in self.slots.borrow().iter() {
            if !slot.claimed.get() {
                slot.button.set_visible(false);
            }
        }
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
        Slot {
            button,
            key,
            claimed: Cell::new(false),
        }
    }
}
