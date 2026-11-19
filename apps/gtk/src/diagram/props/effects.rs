//! The Shape group's switches that read or write more than one key, each shown only where it
//! changes something: Fixed Corner Size, draw.io's Abs. Arc Size (`absoluteArcSize`), Fixed
//! Size, its `fixedSize`, and Sketch, its comic drawing.

use std::rc::Rc;

use accent_drawio::shapes::{Corner, Inset};
use accent_drawio::{Fixed, Scene};
use adw::prelude::*;

use super::{Cells, Change, Check, Read};

/// What the switches read on the cells shown, each over the cells it can change, `None` where
/// there is none.
pub struct Shown {
    /// The shapes whose corner grows with them ([`Corner`]): whether it is fixed.
    corners: Option<Read<bool>>,
    /// The shapes whose `size` is a share of their width until fixed ([`Inset`]): whether it is.
    sizes: Option<Read<bool>>,
    /// The cells that paint a straight piece a hand would wobble ([`Scene::wobbles`]): whether
    /// they are drawn by hand.
    sketch: Option<Read<bool>>,
}

impl Shown {
    /// What the switches read on `cells`, as `scene` draws them.
    pub fn of(cells: &[&accent_drawio::Cell], scene: Option<&Scene>) -> Shown {
        let shapes = || {
            let vertices = cells.iter().filter(|c| c.vertex);
            vertices.map(|c| c.style.resolve(false))
        };
        let wobbles = |c: &&&accent_drawio::Cell| scene.is_some_and(|scene| scene.wobbles(&c.id));
        Shown {
            corners: Read::of(
                shapes()
                    .filter(|s| Corner::of(s.shape()).is_some())
                    .map(|s| s.flag("absoluteArcSize", false)),
            ),
            sizes: Read::of(
                shapes()
                    .filter(|s| Inset::of(s.shape()).is_some())
                    .map(|s| s.flag("fixedSize", false)),
            ),
            sketch: Read::of(
                (cells.iter().filter(wobbles))
                    .map(|c| accent_drawio::is_comic(&c.style.resolve(c.edge))),
            ),
        }
    }
}

pub(super) struct Effects {
    /// A corner of a size of its own, not a share of the shape's.
    fixed: Check,
    /// A `size` of a length of its own, not a share of the shape's width.
    sized: Check,
    /// Whether Fixed Corner Size can change some shape shown.
    corners: std::cell::Cell<bool>,
    /// Drawn by hand as draw.io's comic drawing, written `sketch=1;sketchStyle=comic`, and on
    /// for `comic=1` and a rough sketch too.
    pub sketch: Check,
}

impl Effects {
    pub fn new() -> Effects {
        Effects {
            fixed: Check::new("Fixed Corner Size"),
            sized: Check::new("Fixed Size"),
            corners: std::cell::Cell::new(false),
            sketch: Check::new("Sketch"),
        }
    }

    /// Put Fixed Corner Size and Fixed Size into `group`, after the corners' size.
    pub fn add_fixed(&self, group: &adw::PreferencesGroup) {
        group.add(&self.fixed.row);
        group.add(&self.sized.row);
    }

    /// Send each switch's change; `send` drops what the pane's own filling sets.
    pub fn wire(&self, send: Rc<dyn Fn(Change)>) {
        for (row, what) in [(&self.fixed, Fixed::Corner), (&self.sized, Fixed::Size)] {
            let send = send.clone();
            row.connect(move |on| send(Change::Fixed(what, on)));
        }
        self.sketch.connect(move |on| {
            let mut pairs = vec![
                ("sketch", on.then(|| "1".to_string())),
                ("sketchStyle", on.then(|| "comic".to_string())),
            ];
            if !on {
                pairs.push(("comic", None));
            }
            send(Change::Style(Cells::All, pairs))
        });
    }

    /// Show what `shown` reads, each switch only where it changes something.
    pub fn fill(&self, shown: &Shown) {
        self.fixed.fill(shown.corners.unwrap_or_default());
        self.corners.set(shown.corners.is_some());
        self.sized.fill(shown.sizes.unwrap_or_default());
        self.sized.row.set_visible(shown.sizes.is_some());
        self.sketch.fill(shown.sketch.unwrap_or_default());
        self.sketch.row.set_visible(shown.sketch.is_some());
    }

    /// Fixed Corner Size shows with the corners' size, while they are rounded, where a corner
    /// grows with its shape.
    pub fn sync(&self, rounded: bool) {
        self.fixed.row.set_visible(rounded && self.corners.get());
    }
}
