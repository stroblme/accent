//! The buttons a comparison lays over its views, kept for reuse from one refresh to the next.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::{Rc, Weak};

use super::Compare;

/// What one overlaid button is for right now.
#[derive(Clone)]
pub(super) enum Role {
    /// The Take / Keep Both pair of the hunk over these rows.
    Hunk(Range<usize>),
    /// Opens the hidden run keyed `key`, `rows` long.
    Gap { key: usize, rows: usize },
}

struct Slot {
    widget: gtk::Widget,
    /// The gap button itself, to relabel; a hunk row has nothing to relabel.
    label: Option<gtk::Button>,
    role: Rc<RefCell<Role>>,
    /// Whether the refresh under way has handed this slot out.
    claimed: Cell<bool>,
}

/// The buttons laid over one view, kept for reuse.
///
/// GTK 4.22 has no public way to take an overlay back off a text view — `gtk_text_view_remove`
/// knows the anchored children and the four border children, and warns for anything else — so
/// a button that is not needed is hidden, and the next refresh picks it up again. The pool is
/// owned by the pane and not by the comparison, because the editor's view outlives every
/// comparison it hosts and the buttons parented to it have to as well.
///
/// A refresh hands the buttons out again in order, [`Pool::unclaim`] to [`Pool::hide_unclaimed`],
/// so the button a hunk or a run had is the one it gets back, and one still wanted is never
/// hidden in between: a keystroke re-diffs, and must not take every button off and put it back.
#[derive(Default)]
pub struct Pool {
    /// The comparison the buttons act on right now.
    pub(super) owner: RefCell<Weak<Compare>>,
    slots: RefCell<Vec<Slot>>,
}

impl Pool {
    /// A shown button for `role`: the first one of the same kind this refresh has not handed out
    /// yet, or a new one laid over `view`.
    pub(super) fn claim(self: &Rc<Self>, view: &sourceview5::View, role: Role) -> gtk::Widget {
        let same_kind = |slot: &Slot| {
            matches!(
                (&*slot.role.borrow(), &role),
                (Role::Hunk(_), Role::Hunk(_)) | (Role::Gap { .. }, Role::Gap { .. })
            )
        };
        let mut slots = self.slots.borrow_mut();
        let at = match slots
            .iter()
            .position(|slot| !slot.claimed.get() && same_kind(slot))
        {
            Some(at) => at,
            None => {
                slots.push(self.build(view, &role));
                slots.len() - 1
            }
        };
        let slot = &slots[at];
        if let (Some(button), Role::Gap { rows, .. }) = (&slot.label, &role) {
            button.set_label(&format!("⋯ {rows} unchanged lines"));
        }
        *slot.role.borrow_mut() = role;
        slot.claimed.set(true);
        slot.widget.set_visible(true);
        slot.widget.clone()
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
                slot.widget.set_visible(false);
            }
        }
    }

    fn build(self: &Rc<Self>, view: &sourceview5::View, role: &Role) -> Slot {
        let role = Rc::new(RefCell::new(role.clone()));
        let (widget, label) = match &*role.borrow() {
            Role::Gap { .. } => {
                let button = gtk::Button::new();
                button.add_css_class("flat");
                button.add_css_class("caption");
                button.set_tooltip_text(Some("Show these lines"));
                crate::widgets::claim_press(&button);
                button.connect_clicked(self.act(&role, |compare, role| {
                    if let Role::Gap { key, .. } = role {
                        compare.open_run(key);
                    }
                }));
                (button.clone().upcast(), Some(button))
            }
            Role::Hunk(_) => {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                row.add_css_class("linked");
                row.add_css_class("osd");
                // The comparison's buttons as they are now, which is once and for all: a hunk is
                // only ever claimed on the pane beside the editor, a companion that goes with its
                // comparison.
                let buttons = match self.owner.borrow().upgrade() {
                    Some(compare) => compare.hunk_buttons.borrow().clone(),
                    None => Vec::new(),
                };
                for (label, tip, on) in buttons {
                    let button = gtk::Button::with_label(label);
                    button.add_css_class("caption");
                    button.set_tooltip_text(Some(tip));
                    button.update_property(&[gtk::accessible::Property::Description(tip)]);
                    crate::widgets::claim_press(&button);
                    button.connect_clicked(self.act(&role, move |compare, role| {
                        if let Role::Hunk(hunk) = role {
                            on(compare, hunk);
                        }
                    }));
                    row.append(&button);
                }
                (row.upcast(), None)
            }
        };
        view.add_overlay(&widget, 0, 0);
        Slot {
            widget,
            label,
            role,
            claimed: Cell::new(false),
        }
    }

    /// What a button does: `f`, on the comparison the pool serves right now, with the role the
    /// slot holds right now. Weak on the pool, because the button is a child of the view the
    /// pool's slots hold.
    fn act<F: Fn(&Compare, Role) + 'static>(
        self: &Rc<Self>,
        role: &Rc<RefCell<Role>>,
        f: F,
    ) -> impl Fn(&gtk::Button) + use<F> {
        let (pool, role) = (Rc::downgrade(self), role.clone());
        move |_| {
            let Some(compare) = pool.upgrade().and_then(|p| p.owner.borrow().upgrade()) else {
                return;
            };
            let role = role.borrow().clone();
            f(&compare, role);
        }
    }
}
