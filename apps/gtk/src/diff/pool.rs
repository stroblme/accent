//! The buttons a comparison lays over its views, kept for reuse from one refresh to the next.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

/// What one overlaid button is for right now.
#[derive(Clone)]
pub(super) enum Role {
    /// The Take / Keep Both pair of the hunk over these rows.
    Hunk(Range<usize>),
    /// Opens the hidden run keyed `key`, `rows` long.
    Gap { key: usize, rows: usize },
    /// Takes a side of the `i`th conflict block in a merge.
    Block(usize),
}

/// What a press does: handed the role its button's slot holds right now, and which of the slot's
/// buttons it was.
pub(super) type Act = Rc<dyn Fn(Role, usize)>;

struct Slot {
    widget: gtk::Widget,
    /// The gap button itself, to relabel.
    label: Option<gtk::Button>,
    /// A hunk's or a block's buttons. A block's are dressed again at each claim: the arrow on a
    /// merge's column takes the side the column shows.
    buttons: Vec<gtk::Button>,
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
    /// What the comparison laid over the view right now does with a press, asked at the press:
    /// the buttons outlive the comparison that made them.
    pub(super) act: RefCell<Option<Act>>,
    /// The labels or icon names, and the tooltips, of a hunk's or a block's buttons: whichever
    /// this view's comparison asks for, which a row is made with and dressed in at every claim.
    pub(super) buttons: RefCell<Vec<(&'static str, &'static str)>>,
    slots: RefCell<Vec<Slot>>,
}

impl Pool {
    /// A shown button for `role`: the first one of the same kind this refresh has not handed out
    /// yet, or a new one laid over `view`.
    pub(super) fn claim(self: &Rc<Self>, view: &sourceview5::View, role: Role) -> gtk::Widget {
        let same_kind = |slot: &Slot| {
            matches!(
                (&*slot.role.borrow(), &role),
                (Role::Hunk(_), Role::Hunk(_))
                    | (Role::Gap { .. }, Role::Gap { .. })
                    | (Role::Block(_), Role::Block(_))
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
        if matches!(role, Role::Block(_)) {
            for (button, (name, tip)) in slot.buttons.iter().zip(self.buttons.borrow().iter()) {
                dress(button, name, tip);
            }
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
        let (widget, label, buttons) = match &*role.borrow() {
            Role::Gap { .. } => {
                let button = gtk::Button::new();
                button.add_css_class("flat");
                button.add_css_class("caption");
                button.set_tooltip_text(Some("Show these lines"));
                crate::widgets::claim_press(&button);
                button.connect_clicked(self.act(&role, 0));
                (button.clone().upcast(), Some(button), Vec::new())
            }
            // The comparison's buttons as they are now, which is once and for all: a hunk is only
            // ever claimed on the pane beside the editor, a companion that goes with its
            // comparison, and a block's buttons on a merge's column are those of the column.
            Role::Hunk(_) | Role::Block(_) => {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                row.add_css_class("linked");
                row.add_css_class("osd");
                let mut buttons = Vec::new();
                for (i, (name, tip)) in self.buttons.borrow().iter().enumerate() {
                    let button = gtk::Button::new();
                    button.add_css_class("caption");
                    dress(&button, name, tip);
                    crate::widgets::claim_press(&button);
                    button.connect_clicked(self.act(&role, i));
                    row.append(&button);
                    buttons.push(button);
                }
                (row.upcast(), None, buttons)
            }
        };
        view.add_overlay(&widget, 0, 0);
        Slot {
            widget,
            label,
            buttons,
            role,
            claimed: Cell::new(false),
        }
    }

    /// What the `i`th button of a slot does: the comparison's [`Pool::act`] as it is at the press,
    /// with the role the slot holds then. Weak on the pool, because the button is a child of the
    /// view the pool's slots hold.
    fn act(self: &Rc<Self>, role: &Rc<RefCell<Role>>, i: usize) -> impl Fn(&gtk::Button) + use<> {
        let (pool, role) = (Rc::downgrade(self), role.clone());
        move |_| {
            let Some(act) = pool.upgrade().and_then(|p| p.act.borrow().clone()) else {
                return;
            };
            let role = role.borrow().clone();
            act(role, i);
        }
    }
}

/// Give `button` its label, or its icon where `name` is a symbolic icon's, and its tooltip, which
/// is its accessible description too.
fn dress(button: &gtk::Button, name: &str, tip: &str) {
    match name.ends_with("-symbolic") {
        true => button.set_icon_name(name),
        false => button.set_label(name),
    }
    button.set_tooltip_text(Some(tip));
    button.update_property(&[gtk::accessible::Property::Description(tip)]);
}
