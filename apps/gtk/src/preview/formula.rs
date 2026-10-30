//! Formulas in the page: a [`Formula`] paintable drawing `math`'s layout of the MathML the page's
//! worker wrote (refresh N10), its baseline the line's through the writer's rise tag. An inline
//! formula's capitals stand as tall as the prose's (N9); a display one is 1.15 times that, centred
//! on a line of its own and shrunk to the text's width where it is wider. A zoom lays them out
//! again. Rules are drawn on whole device pixels, wherever the line puts the formula.

use super::Page;
use super::text::{Ink, Object, Writer};
use accent_math::Layout;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, pango};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

/// A display formula's size against an inline one's, as the stylesheet's `DISPLAY_MATH`.
const DISPLAY: f32 = 1.15;

glib::wrapper! {
    pub struct Formula(ObjectSubclass<imp::Formula>) @implements gdk::Paintable;
}

/// `tex` at the writer, laid out from `mathml`, or its source where it does not lay out.
pub(super) fn insert(w: &mut Writer, tex: &str, mathml: &str, display: bool) {
    let source = match display {
        true => format!("$${tex}$$"),
        false => format!("${tex}$"),
    };
    let page = w.page;
    let formula = match w.in_cell() {
        true => None,
        false => Formula::new(page, tex, mathml, display),
    };
    let Some(formula) = formula else {
        w.push(&page.tags.math);
        w.text(&source);
        w.pop();
        return;
    };
    if display {
        w.newline();
        w.push(&page.tags.display);
    }
    let line = w.at.offset();
    let solid = page.figures.solid(page);
    w.push(&solid);
    let object = Object::new("formula".into(), source);
    w.paintable(&formula, formula.descent(), object);
    w.pop();
    if display {
        w.newline();
        w.pop();
        super::text::collapse_margin(page, &page.buffer.iter_at_offset(line));
    }
}

/// Every formula laid out again for the zoom and the text's width, lowered anew where its
/// descent changed.
pub(super) fn resize(page: &Page) {
    CAPS.set(caps(page));
    let buffer = &page.buffer;
    let mut from = 0;
    loop {
        let at = buffer.iter_at_offset(from);
        let Some((start, end)) = at.forward_search("\u{fffc}", gtk::TextSearchFlags::empty(), None)
        else {
            return;
        };
        from = end.offset();
        let Some(formula) = start.paintable().and_downcast::<Formula>() else {
            continue;
        };
        let before = formula.descent();
        if formula.lay(page) != Some(true) {
            continue;
        }
        let after = formula.descent();
        if after != before {
            if before != 0 {
                buffer.remove_tag(&page.rise(before), &start, &end);
            }
            if after != 0 {
                buffer.apply_tag(&page.rise(after), &start, &end);
            }
        }
        formula.invalidate_size();
    }
}

thread_local! {
    /// The prose's capitals over the math font's, measured on a zoom, which a new font comes
    /// with; 0 before the first.
    static CAPS: Cell<f32> = const { Cell::new(0.0) };
}

/// The size an inline formula is laid out at: the prose's, times its capitals over the math
/// font's, as the stylesheet's `font-size-adjust: cap-height` scales it.
fn inline_px(page: &Page) -> f32 {
    if CAPS.get() == 0.0 {
        CAPS.set(caps(page));
    }
    page.px.get() as f32 * CAPS.get()
}

/// [`CAPS`], the prose in the page's Preview Font or else the system's document font.
fn caps(page: &Page) -> f32 {
    let font = page.prose_font.borrow().clone();
    let font = font.unwrap_or_else(crate::platform::document_font);
    let family = pango::FontDescription::from_string(&font)
        .family()
        .map_or_else(|| "Sans".to_string(), |f| f.to_string());
    let prose = crate::export::css::cap_height(&family).unwrap_or(0.7) as f32;
    prose / crate::math::cap_height(&page.view)
}

impl Formula {
    fn new(page: &Page, tex: &str, mathml: &str, display: bool) -> Option<Formula> {
        let formula: Formula = glib::Object::new();
        let imp = formula.imp();
        let _ = imp.tex.set(tex.to_string());
        let _ = imp.mathml.set(mathml.to_string());
        imp.display.set(display);
        let _ = imp.ink.set(page.ink.clone());
        formula.lay(page).map(|_| formula)
    }

    /// Lay it out for the zoom and, a display formula, the text's width; whether that changed
    /// its size, `None` where it does not lay out.
    fn lay(&self, page: &Page) -> Option<bool> {
        let imp = self.imp();
        let (tex, mathml) = (imp.tex.get()?, imp.mathml.get()?);
        let display = imp.display.get();
        let size = inline_px(page) * if display { DISPLAY } else { 1.0 };
        let laid = crate::math::formula_of(&page.view, tex, mathml, display, size)?;
        let width = page.text_width() as f32;
        let shrink = match display && laid.width > width {
            true => width / laid.width,
            false => 1.0,
        };
        let changed = (imp.size.replace(size), imp.shrink.replace(shrink)) != (size, shrink);
        imp.layout.replace(Some(laid));
        Some(changed)
    }

    /// Whole pixels below the baseline, which the writer's rise lowers it by.
    fn descent(&self) -> i32 {
        let imp = self.imp();
        let shrink = imp.shrink.get();
        imp.layout
            .borrow()
            .as_ref()
            .map_or(0, |f| (f.descent * shrink).ceil() as i32)
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Formula {
        pub(super) tex: OnceCell<String>,
        pub(super) mathml: OnceCell<String>,
        pub(super) display: Cell<bool>,
        pub(super) ink: OnceCell<Rc<Ink>>,
        pub(super) layout: RefCell<Option<Rc<Layout>>>,
        /// The size it is laid out at, in pixels.
        pub(super) size: Cell<f32>,
        /// What a display formula wider than the text is drawn at, 1 for the rest.
        pub(super) shrink: Cell<f32>,
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
            let shrink = self.shrink.get();
            (self.layout.borrow().as_ref()).map_or(0, |f| (f.width * shrink).ceil() as i32)
        }

        /// Whole pixels above and below the baseline, so the rise lowering it by its descent
        /// puts its baseline on the line's.
        fn intrinsic_height(&self) -> i32 {
            let shrink = self.shrink.get();
            (self.layout.borrow().as_ref()).map_or(0, |f| {
                (f.ascent * shrink).ceil() as i32 + (f.descent * shrink).ceil() as i32
            })
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, _width: f64, _height: f64) {
            let layout = self.layout.borrow();
            let (Some(snapshot), Some(f), Some(ink)) = (
                snapshot.downcast_ref::<gtk::Snapshot>(),
                layout.as_ref(),
                self.ink.get(),
            ) else {
                return;
            };
            let colour =
                |c: accent_math::Color| crate::theme::rgba([c.r, c.g, c.b], f32::from(c.a) / 255.0);
            let shrink = self.shrink.get();
            let baseline = (f.ascent * shrink).ceil();
            crate::math::paint_snapped(snapshot, f, (0.0, baseline), shrink, &ink.fg.get(), colour);
        }
    }
}

/// Every formula on the page, in order: its TeX, whether it is a display one, and its size
/// as drawn against an inline formula's.
#[cfg(feature = "bench")]
pub(super) fn probe(page: &Page) -> Vec<(String, bool, f32)> {
    let mut out = Vec::new();
    let inline = inline_px(page);
    let mut at = page.buffer.start_iter();
    loop {
        if let Some(f) = at.paintable().and_downcast::<Formula>() {
            let imp = f.imp();
            let tex = imp.tex.get().cloned().unwrap_or_default();
            let drawn = imp.size.get() * imp.shrink.get();
            out.push((tex, imp.display.get(), drawn / inline));
        }
        if !at.forward_find_char(|c| c == '\u{fffc}', None) {
            return out;
        }
    }
}
