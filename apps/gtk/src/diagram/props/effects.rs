//! The Shape group's switches beside Rounded that read more than one key: Fixed Corner Size,
//! draw.io's Abs. Arc Size (`absoluteArcSize`).

use std::cell::Cell;
use std::rc::Rc;

use accent_drawio::Resolved;
use adw::prelude::*;

use super::Change;

pub(super) struct Effects {
    /// A corner of its own size rather than a share of the shape's.
    pub fixed: adw::SwitchRow,
    /// Whether some shape shown has a corner that grows with it
    /// ([`accent_drawio::shapes::Corner`]), which is all Fixed Corner Size can change.
    corners: Cell<bool>,
}

impl Effects {
    pub fn new() -> Effects {
        Effects {
            fixed: adw::SwitchRow::builder().title("Fixed Corner Size").build(),
            corners: Cell::new(false),
        }
    }

    /// Send each switch's change; `send` drops what the pane's own filling sets.
    pub fn wire(&self, send: Rc<dyn Fn(Change)>) {
        self.fixed
            .connect_active_notify(move |row| send(Change::FixedCorners(row.is_active())));
    }

    /// Show the first cell's `style`, `corners` saying whether any shape shown has a corner
    /// that grows with it.
    pub fn fill(&self, style: &Resolved, corners: bool) {
        self.fixed.set_active(style.flag("absoluteArcSize", false));
        self.corners.set(corners);
    }

    /// Fixed Corner Size shows with the corners' size, while they are rounded, where a corner
    /// grows with its shape.
    pub fn sync(&self, rounded: bool) {
        self.fixed.set_visible(rounded && self.corners.get());
    }
}
