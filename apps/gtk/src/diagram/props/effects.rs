//! The Shape group's switches that read or write more than one key, each shown only where it
//! changes something: Fixed Corner Size, draw.io's Abs. Arc Size (`absoluteArcSize`), Fixed
//! Size, its `fixedSize`, and Sketch, its comic drawing.

use std::cell::Cell;
use std::rc::Rc;

use accent_drawio::shapes::{Corner, Inset};
use accent_drawio::{Fixed, Resolved, Scene};
use adw::prelude::*;

use super::Change;

/// What the switches can change on the cells shown.
pub struct Shown {
    /// Some shape's corner grows with it ([`Corner`]).
    corners: bool,
    /// Some shape's `size` is a share of its width until fixed ([`Inset`]).
    sizes: bool,
    /// Some cell paints a straight piece a hand would wobble ([`Scene::wobbles`]).
    sketch: bool,
}

impl Shown {
    /// What the switches can change on `cells`, as `scene` draws them.
    pub fn of(cells: &[&accent_drawio::Cell], scene: Option<&Scene>) -> Shown {
        let shapes = || {
            let vertices = cells.iter().filter(|c| c.vertex);
            vertices.map(|c| c.style.resolve(false))
        };
        Shown {
            corners: shapes().any(|s| Corner::of(s.shape()).is_some()),
            sizes: shapes().any(|s| Inset::of(s.shape()).is_some()),
            sketch: scene.is_some_and(|scene| cells.iter().any(|c| scene.wobbles(&c.id))),
        }
    }
}

pub(super) struct Effects {
    /// A corner of a size of its own, not a share of the shape's.
    fixed: adw::SwitchRow,
    /// A `size` of a length of its own, not a share of the shape's width.
    sized: adw::SwitchRow,
    /// Whether Fixed Corner Size can change some shape shown.
    corners: Cell<bool>,
    /// Drawn by hand as draw.io's comic drawing, written `sketch=1;sketchStyle=comic`, and on
    /// for `comic=1` and a rough sketch too.
    pub sketch: adw::SwitchRow,
}

impl Effects {
    pub fn new() -> Effects {
        let switch = |title: &str| adw::SwitchRow::builder().title(title).build();
        Effects {
            fixed: switch("Fixed Corner Size"),
            sized: switch("Fixed Size"),
            corners: Cell::new(false),
            sketch: switch("Sketch"),
        }
    }

    /// Put Fixed Corner Size and Fixed Size into `group`, after the corners' size.
    pub fn add_fixed(&self, group: &adw::PreferencesGroup) {
        group.add(&self.fixed);
        group.add(&self.sized);
    }

    /// Send each switch's change; `send` drops what the pane's own filling sets.
    pub fn wire(&self, send: Rc<dyn Fn(Change)>) {
        for (row, what) in [(&self.fixed, Fixed::Corner), (&self.sized, Fixed::Size)] {
            let send = send.clone();
            row.connect_active_notify(move |row| send(Change::Fixed(what, row.is_active())));
        }
        self.sketch.connect_active_notify(move |row| {
            let on = row.is_active();
            let mut pairs = vec![
                ("sketch", on.then(|| "1".to_string())),
                ("sketchStyle", on.then(|| "comic".to_string())),
            ];
            if !on {
                pairs.push(("comic", None));
            }
            send(Change::Style(pairs))
        });
    }

    /// Show the first cell's `style`, each switch only where `shown` says it changes something.
    pub fn fill(&self, style: &Resolved, shown: &Shown) {
        self.fixed.set_active(style.flag("absoluteArcSize", false));
        self.corners.set(shown.corners);
        self.sized.set_active(style.flag("fixedSize", false));
        self.sized.set_visible(shown.sizes);
        self.sketch.set_active(accent_drawio::is_comic(style));
        self.sketch.set_visible(shown.sketch);
    }

    /// Fixed Corner Size shows with the corners' size, while they are rounded, where a corner
    /// grows with its shape.
    pub fn sync(&self, rounded: bool) {
        self.fixed.set_visible(rounded && self.corners.get());
    }
}
