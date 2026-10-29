//! Formulas in the page: a [`Formula`] paintable drawing `math::formula`'s layout, its baseline
//! the line's through the writer's rise tag; a display formula centred on a line of its own. Stage
//! 2 P5 sizes them to the prose's cap height, a display one 1.15× (refresh N9), lays them out
//! from the worker's MathML (N10) and again on a zoom.

use super::text::{Ink, Object, Writer};
use accent_math::Layout;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use std::cell::OnceCell;
use std::rc::Rc;

glib::wrapper! {
    pub struct Formula(ObjectSubclass<imp::Formula>) @implements gdk::Paintable;
}

impl Formula {
    fn new(layout: Rc<Layout>, ink: Rc<Ink>) -> Formula {
        let formula: Formula = glib::Object::new();
        let _ = formula.imp().layout.set(layout);
        let _ = formula.imp().ink.set(ink);
        formula
    }
}

/// `tex` at the writer, or its source where it does not lay out.
pub(super) fn insert(w: &mut Writer, tex: &str, display: bool) {
    let source = match display {
        true => format!("$${tex}$$"),
        false => format!("${tex}$"),
    };
    let page = w.page;
    let laid = match w.in_cell() {
        true => None,
        false => crate::math::formula(&page.view, tex, display, page.px.get() as f32),
    };
    let Some(laid) = laid else {
        w.push(&page.tags.math);
        w.text(&source);
        w.pop();
        return;
    };
    if display {
        w.newline();
        w.push(&page.tags.display);
    }
    let descent = laid.descent.ceil() as i32;
    let object = Object::new("formula".into(), source);
    w.paintable(&Formula::new(laid, page.ink.clone()), descent, object);
    if display {
        w.newline();
        w.pop();
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Formula {
        pub layout: OnceCell<Rc<Layout>>,
        pub ink: OnceCell<Rc<Ink>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Formula {
        const NAME: &'static str = "AccentPreviewFormula";
        type Type = super::Formula;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for Formula {}

    impl PaintableImpl for Formula {
        fn intrinsic_width(&self) -> i32 {
            self.layout.get().map_or(0, |f| f.width.ceil() as i32)
        }

        /// Whole pixels above and below the baseline, so the rise lowering it by its descent
        /// puts its baseline on the line's.
        fn intrinsic_height(&self) -> i32 {
            self.layout
                .get()
                .map_or(0, |f| f.ascent.ceil() as i32 + f.descent.ceil() as i32)
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, _width: f64, _height: f64) {
            let (Some(snapshot), Some(f), Some(ink)) = (
                snapshot.downcast_ref::<gtk::Snapshot>(),
                self.layout.get(),
                self.ink.get(),
            ) else {
                return;
            };
            let colour =
                |c: accent_math::Color| crate::theme::rgba([c.r, c.g, c.b], f32::from(c.a) / 255.0);
            let baseline = f.ascent.ceil();
            crate::math::paint(snapshot, f, (0.0, baseline), 1.0, &ink.fg.get(), colour);
        }
    }
}
