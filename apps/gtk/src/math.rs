//! Formulas typeset natively: TeX through core's MathML into `accent_math`'s layout, painted as
//! render nodes wherever a snapshot goes, a canvas, a thumbnail or an export alike.
//!
//! The font is the first of [`FAMILIES`] Pango has that is a math font, its glyphs drawn as text
//! in that Pango font. Without one it is the bundled STIX Two Math, which no font map knows (and
//! CoreText cannot be given), its glyphs drawn from their outlines. A character the math font
//! lacks, a CJK `\text{}` say, is set by Pango in [`FALLBACK`] and the fonts it falls back on.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::rc::Rc;

use accent_math::{Color, Item, Layout, MathFont, PathOp};
use gtk::glib::translate::{ToGlibPtr, ToGlibPtrMut};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

/// The system math fonts, in the order an exported page names them: Latin Modern's look is
/// closest to draw.io's MathJax.
const FAMILIES: [&str; 3] = ["Latin Modern Math", "STIX Two Math", "DejaVu Math TeX Gyre"];

/// The family characters the math font lacks are set in: MathJax's `unknownFamily`, as draw.io
/// and Jupyter set them.
const FALLBACK: &str = "serif";

/// The math font formulas are laid out in, and how its glyphs are drawn.
struct Fonts {
    math: MathFont,
    draw: Draw,
    /// The widget's own, so glyphs are rendered with GTK's font options as text is.
    context: pango::Context,
}

enum Draw {
    /// A system font, as text: Pango's font of `family` at each size, by 1/64 px.
    Text {
        family: &'static str,
        sized: RefCell<HashMap<u32, pango::Font>>,
    },
    /// The bundled font, as paths: each glyph's outline, in em units.
    Outlines(RefCell<HashMap<u16, Rc<[PathOp]>>>),
}

/// A formula's TeX, whether it is a display, and its size's bits.
type Key = (String, bool, u32);

thread_local! {
    static FONTS: OnceCell<Rc<Fonts>> = const { OnceCell::new() };
    static FORMULAS: RefCell<Kept> = RefCell::default();
}

/// What the formulas laid out or asked for lately may weigh before the ones asked for before
/// them are let go of: about 4,800 formulas of some 870 bytes, a long note's at one zoom with room
/// to spare, where laying 2,000 out again takes about 45 ms (2026-11-18).
const GENERATION: usize = 4 << 20;

/// The formulas laid out, `None` for one that does not, in two generations by weight: the newer
/// takes each formula laid out or asked for again, and once it weighs [`GENERATION`] it becomes
/// the older, the older let go of. So a formula asked for once a generation stays, and what edits
/// and zooms leave behind goes. A view holds its own layout, which going from here does not take.
#[derive(Default)]
struct Kept {
    new: HashMap<Key, Option<Rc<Layout>>>,
    old: HashMap<Key, Option<Rc<Layout>>>,
    /// What `new` weighs.
    bytes: usize,
}

impl Kept {
    fn get(&mut self, key: &Key) -> Option<Option<Rc<Layout>>> {
        if let Some(laid) = self.new.get(key) {
            return Some(laid.clone());
        }
        let (key, laid) = self.old.remove_entry(key)?;
        self.insert(key, laid.clone());
        Some(laid)
    }

    fn insert(&mut self, key: Key, laid: Option<Rc<Layout>>) {
        self.bytes += weight(&key, laid.as_deref());
        self.new.insert(key, laid);
        if self.bytes > GENERATION {
            self.old = std::mem::take(&mut self.new);
            self.bytes = 0;
        }
    }
}

/// About the bytes a kept formula takes: its entry, its key's text, and its layout's items.
fn weight(key: &Key, laid: Option<&Layout>) -> usize {
    let items = laid.map_or(0, |laid| {
        let text = |item: &Item| match item {
            Item::Text { text, .. } => text.len(),
            _ => 0,
        };
        let items = laid.items.iter();
        items.map(|item| size_of::<Item>() + text(item)).sum()
    });
    size_of::<(Key, Option<Rc<Layout>>, Layout)>() + key.0.len() + items
}

/// The math font, found the first time a formula is laid out, through `widget`'s font map.
fn fonts(widget: &gtk::Widget) -> Rc<Fonts> {
    FONTS.with(|fonts| fonts.get_or_init(|| Rc::new(find(widget))).clone())
}

fn find(widget: &gtk::Widget) -> Fonts {
    let context = widget.pango_context();
    let system = FAMILIES.into_iter().find_map(|family| {
        let font = context.load_font(&description(family, 16.0))?;
        // Pango answers with another family for one it does not have.
        let named = font.describe().family()?;
        if !named.eq_ignore_ascii_case(family) {
            return None;
        }
        // SAFETY: the font is alive for the call, and the HarfBuzz font is its own;
        // `from_hb_font` takes a reference of its own to the face.
        let math = unsafe {
            let hb = pango::ffi::pango_font_get_hb_font(font.to_glib_none().0);
            MathFont::from_hb_font(NonNull::new(hb.cast_mut())?)
        };
        let sized = RefCell::default();
        Some((math.ok()?, Draw::Text { family, sized }))
    });
    let (mut math, draw) = system.unwrap_or_else(|| {
        let math = MathFont::from_static(accent_math::STIX_TWO_MATH)
            .expect("the bundled STIX Two Math has a MATH table");
        (math, Draw::Outlines(RefCell::default()))
    });
    match &draw {
        Draw::Text { family, .. } => tracing::debug!("formulas in {family}"),
        Draw::Outlines(_) => tracing::debug!("formulas in the bundled STIX Two Math"),
    }
    let measure = context.clone();
    math.set_fallback(move |text, size| {
        let layout = fallback(&measure, text, size);
        let (ink, logical) = layout.extents();
        let baseline = layout.baseline();
        let px = |v: i32| v as f32 / pango::SCALE as f32;
        Some((
            px(logical.width()),
            px(baseline - ink.y()),
            px(ink.y() + ink.height() - baseline),
        ))
    });
    Fonts {
        math,
        draw,
        context,
    }
}

/// `text`, characters the math font lacks, as Pango sets it at `px` pixels.
fn fallback(context: &pango::Context, text: &str, px: f32) -> pango::Layout {
    let layout = pango::Layout::new(context);
    layout.set_font_description(Some(&description(FALLBACK, f64::from(px))));
    layout.set_text(text);
    layout
}

fn description(family: &str, px: f64) -> pango::FontDescription {
    let mut desc = pango::FontDescription::new();
    desc.set_family(family);
    desc.set_absolute_size(px * f64::from(pango::SCALE));
    desc
}

/// `tex` laid out at `size` pixels, as a display formula where `display`; `None` for TeX that
/// does not parse, whose source the caller shows instead.
pub fn formula(
    widget: &impl IsA<gtk::Widget>,
    tex: &str,
    display: bool,
    size: f32,
) -> Option<Rc<Layout>> {
    lay_out(widget, tex, display, size, || {
        accent_core::markdown::mathml(tex, display)
    })
}

/// [`formula`] from the MathML core already wrote, as a note's page carries it, so its TeX is not
/// read a second time. It is kept by the MathML, which one TeX need not have twice: the operators
/// a page declares change what it reads.
pub fn formula_of(
    widget: &impl IsA<gtk::Widget>,
    mathml: &str,
    display: bool,
    size: f32,
) -> Option<Rc<Layout>> {
    lay_out(widget, mathml, display, size, || Some(mathml.to_string()))
}

/// How tall the math font's capitals are, in ems: an upright `H`'s ink, what an inline formula is
/// scaled by to stand as tall as the prose's capitals.
pub fn cap_height(widget: &impl IsA<gtk::Widget>) -> f32 {
    thread_local! {
        static CAP: OnceCell<f32> = const { OnceCell::new() };
    }
    CAP.with(|cap| {
        *cap.get_or_init(|| {
            formula(widget, "\\mathrm{H}", false, 100.0).map_or(0.7, |f| f.ascent / 100.0)
        })
    })
}

/// [`formula`] laid out from the MathML `mathml` makes, unless it is kept by `source`, its TeX or
/// its MathML.
fn lay_out(
    widget: &impl IsA<gtk::Widget>,
    source: &str,
    display: bool,
    size: f32,
    mathml: impl FnOnce() -> Option<String>,
) -> Option<Rc<Layout>> {
    let key = (source.to_string(), display, size.to_bits());
    if let Some(kept) = FORMULAS.with_borrow_mut(|kept| kept.get(&key)) {
        return kept;
    }
    let fonts = fonts(widget.as_ref());
    // A panic here would abort the app from inside a GTK callback: the source is shown instead.
    let laid = catch_unwind(AssertUnwindSafe(|| {
        let root = accent_math::parse(&mathml()?)
            .inspect_err(|e| tracing::debug!("{source:?}: {e}"))
            .ok()?;
        if !root.warnings.is_empty() {
            tracing::debug!("{source:?}: {:?}", root.warnings);
        }
        Some(Rc::new(accent_math::layout(&root, &fonts.math, size)))
    }))
    .unwrap_or_else(|_| {
        tracing::warn!("laying out {source:?} panicked; its source is shown");
        None
    });
    FORMULAS.with_borrow_mut(|kept| kept.insert(key, laid.clone()));
    laid
}

/// Paint `formula` with the left end of its baseline at `origin`, `scale` content units to a
/// layout unit, in `ink` but where `\color` says otherwise, through `colour`. Nothing is clipped
/// to its box: a stretched accent may overhang it, as in TeX.
pub fn paint(
    snapshot: &gtk::Snapshot,
    formula: &Layout,
    origin: (f32, f32),
    scale: f32,
    ink: &gdk::RGBA,
    colour: impl Fn(Color) -> gdk::RGBA,
) {
    draw(snapshot, formula, origin, scale, ink, colour, false);
}

/// [`paint`] with each rule on whole device pixels, so a fraction bar is one row of pixels, not
/// two grey ones, wherever the snapshot puts `origin` ([`rule_snapped`]).
pub fn paint_snapped(
    snapshot: &gtk::Snapshot,
    formula: &Layout,
    origin: (f32, f32),
    scale: f32,
    ink: &gdk::RGBA,
    colour: impl Fn(Color) -> gdk::RGBA,
) {
    draw(snapshot, formula, origin, scale, ink, colour, true);
}

fn draw(
    snapshot: &gtk::Snapshot,
    formula: &Layout,
    origin: (f32, f32),
    scale: f32,
    ink: &gdk::RGBA,
    colour: impl Fn(Color) -> gdk::RGBA,
    snap: bool,
) {
    let Some(fonts) = FONTS.with(|fonts| fonts.get().cloned()) else {
        return;
    };
    let rgba = |c: Option<Color>| c.map_or(*ink, &colour);
    let mut items = formula.items.as_slice();
    while let Some(first) = items.first() {
        let n = match *first {
            Item::Rule { x, y, w, h, color } => {
                let (w, h) = (w * scale, h * scale);
                // At least a pixel thick, so a fraction bar never vanishes at a small zoom.
                let (w, h) = match w < h {
                    true => (w.max(1.0), h),
                    false => (w, h.max(1.0)),
                };
                let (x, y) = (origin.0 + x * scale, origin.1 + y * scale);
                match snap {
                    true => {
                        let (w, h) = (w.round().max(1.0), h.round().max(1.0));
                        rule_snapped(snapshot, &rgba(color), &graphene::Rect::new(x, y, w, h));
                    }
                    false => snapshot.append_color(&rgba(color), &graphene::Rect::new(x, y, w, h)),
                }
                1
            }
            Item::Line {
                x,
                y,
                x2,
                y2,
                thickness,
                color,
            } => {
                let path = gsk::PathBuilder::new();
                path.move_to(origin.0 + x * scale, origin.1 + y * scale);
                path.line_to(origin.0 + x2 * scale, origin.1 + y2 * scale);
                // At least a pixel wide, as a rule is at least a pixel thick.
                let stroke = gsk::Stroke::new((thickness * scale).max(1.0));
                snapshot.append_stroke(&path.to_path(), &stroke, &rgba(color));
                1
            }
            Item::Text {
                ref text,
                x,
                y,
                size,
                color,
            } => {
                let layout = fallback(&fonts.context, text, size * scale);
                let baseline = layout.baseline() as f32 / pango::SCALE as f32;
                let (x, y) = (origin.0 + x * scale, origin.1 + y * scale - baseline);
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x, y));
                snapshot.append_layout(&layout, &rgba(color));
                snapshot.restore();
                1
            }
            Item::Glyph { size, color, .. } => {
                let n = items
                    .iter()
                    .take_while(|i| matches!(i, Item::Glyph { size: s, color: c, .. } if *s == size && *c == color))
                    .count();
                fonts.glyphs(snapshot, &items[..n], origin, scale, &rgba(color));
                n
            }
        };
        items = &items[n..];
    }
}

/// `rect` filled on whole device pixels. A formula in the text sits on its line's baseline, which
/// a 1.6 line height puts on a fraction of a pixel, and a snapshot does not say where it is
/// (GTK 4.24's `gtk_snapshot_set_snap` would snap it). A cairo fill without antialiasing does
/// not need to know: the renderer replays it on its own device pixels, and it covers those whose
/// centres it holds, so a rule of whole pixels is that many rows at any offset.
fn rule_snapped(snapshot: &gtk::Snapshot, colour: &gdk::RGBA, rect: &graphene::Rect) {
    // A pixel more all round: the renderer clips to the node's bounds, which on a fraction of a
    // pixel would dim the rows the fill covers.
    let cr = snapshot.append_cairo(&rect.inset_r(-1.0, -1.0));
    cr.set_antialias(gtk::cairo::Antialias::None);
    cr.set_source_color(colour);
    let (x, y) = (f64::from(rect.x()), f64::from(rect.y()));
    cr.rectangle(x, y, f64::from(rect.width()), f64::from(rect.height()));
    let _ = cr.fill();
}

impl Fonts {
    /// `run`, glyphs of one size and colour, as one text node or one fill.
    fn glyphs(
        &self,
        snapshot: &gtk::Snapshot,
        run: &[Item],
        origin: (f32, f32),
        scale: f32,
        colour: &gdk::RGBA,
    ) {
        let glyphs = run.iter().filter_map(|i| match *i {
            Item::Glyph { id, x, y, size, .. } => Some((id, x * scale, y * scale, size * scale)),
            _ => None,
        });
        match &self.draw {
            Draw::Text { family, sized } => {
                let Some(Item::Glyph { size, .. }) = run.first() else {
                    return;
                };
                let px = size * scale;
                let key = (px * 64.0).round() as u32;
                let kept = sized.borrow().get(&key).cloned();
                let font = kept.or_else(|| {
                    let font = self
                        .context
                        .load_font(&description(family, f64::from(key) / 64.0))?;
                    let mut sized = sized.borrow_mut();
                    // A size for every zoom: the last few dozen are kept.
                    if sized.len() > 64 {
                        sized.clear();
                    }
                    sized.insert(key, font.clone());
                    Some(font)
                });
                let Some(font) = font else {
                    return;
                };
                let mut string = pango::GlyphString::new();
                string.set_size(run.len() as i32);
                let units = |v: f32| (v * pango::SCALE as f32).round() as i32;
                for (info, (id, x, y, _)) in string.glyph_info_mut().iter_mut().zip(glyphs) {
                    // SAFETY: a `GlyphInfo` is a `PangoGlyphInfo`, which `set_size` leaves
                    // unset; all of it is written, the pen kept where the node starts.
                    unsafe {
                        *info.to_glib_none_mut().0 = pango::ffi::PangoGlyphInfo {
                            glyph: id.into(),
                            geometry: pango::ffi::PangoGlyphGeometry {
                                width: 0,
                                x_offset: units(x),
                                y_offset: units(y),
                            },
                            attr: pango::ffi::PangoGlyphVisAttr {
                                is_cluster_start: 1,
                            },
                        }
                    };
                }
                let at = graphene::Point::new(origin.0, origin.1);
                if let Some(node) = gsk::TextNode::new(&font, &string, colour, &at) {
                    snapshot.append_node(&node);
                }
            }
            Draw::Outlines(outlines) => {
                let builder = gsk::PathBuilder::new();
                for (id, x, y, size) in glyphs {
                    let ops = outlines
                        .borrow_mut()
                        .entry(id)
                        .or_insert_with(|| self.math.outline(id).into())
                        .clone();
                    let (x, y) = (origin.0 + x, origin.1 + y);
                    let at = |px: f32, py: f32| (x + px * size, y + py * size);
                    for op in ops.iter() {
                        match *op {
                            PathOp::MoveTo(px, py) => {
                                let (x, y) = at(px, py);
                                builder.move_to(x, y);
                            }
                            PathOp::LineTo(px, py) => {
                                let (x, y) = at(px, py);
                                builder.line_to(x, y);
                            }
                            PathOp::QuadTo(cx, cy, px, py) => {
                                let ((cx, cy), (x, y)) = (at(cx, cy), at(px, py));
                                builder.quad_to(cx, cy, x, y);
                            }
                            PathOp::CubicTo(ax, ay, bx, by, px, py) => {
                                let ((ax, ay), (bx, by), (x, y)) =
                                    (at(ax, ay), at(bx, by), at(px, py));
                                builder.cubic_to(ax, ay, bx, by, x, y);
                            }
                            PathOp::Close => builder.close(),
                        }
                    }
                }
                snapshot.append_fill(&builder.to_path(), gsk::FillRule::Winding, colour);
            }
        }
    }
}

glib::wrapper! {
    /// A formula on its own, as a notebook's LaTeX output: as large as its layout at the
    /// notebook's zoom, painted in the widget's colour, so a theme change only paints it again.
    pub struct FormulaView(ObjectSubclass<imp::FormulaView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FormulaView {
    pub fn new(formula: Rc<Layout>, zoom: Rc<Cell<f64>>) -> FormulaView {
        let view: FormulaView = glib::Object::new();
        let _ = view.imp().formula.set(formula);
        let _ = view.imp().zoom.set(zoom);
        view
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FormulaView {
        pub(super) formula: OnceCell<Rc<Layout>>,
        pub(super) zoom: OnceCell<Rc<Cell<f64>>>,
    }

    impl FormulaView {
        fn scale(&self) -> f32 {
            self.zoom.get().map_or(1.0, |zoom| zoom.get() as f32)
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FormulaView {
        const NAME: &'static str = "AccentFormulaView";
        type Type = super::FormulaView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for FormulaView {}

    impl WidgetImpl for FormulaView {
        /// The layout's box, its baseline the widget's.
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let Some(f) = self.formula.get() else {
                return (0, 0, -1, -1);
            };
            let scale = self.scale();
            match orientation {
                gtk::Orientation::Horizontal => {
                    let w = (f.width * scale).ceil() as i32;
                    (w, w, -1, -1)
                }
                _ => {
                    let above = (f.ascent * scale).ceil() as i32;
                    let below = (f.descent * scale).ceil() as i32;
                    (above + below, above + below, above, above)
                }
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(f) = self.formula.get() else {
                return;
            };
            let (ink, scale) = (self.obj().color(), self.scale());
            let colour = |c: Color| crate::theme::rgba([c.r, c.g, c.b], f32::from(c.a) / 255.0);
            let baseline = (f.ascent * scale).ceil();
            paint(snapshot, f, (0.0, baseline), scale, &ink, colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Formulas laid out at zoom after zoom weigh at most about two generations, while one asked
    /// for at every zoom stays kept, and one a view holds stays whole once it has gone.
    #[test]
    fn kept_formulas_are_bounded_by_weight() {
        let laid = |i: usize| {
            let rule = Item::Rule {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                color: None,
            };
            let items = vec![rule; 1 + i % 50];
            let (width, ascent, descent) = (1.0, 1.0, 0.0);
            Some(Rc::new(Layout {
                width,
                ascent,
                descent,
                items,
            }))
        };
        let mut kept = Kept::default();
        let shown = ("x^2".to_string(), false, 16f32.to_bits());
        let held = laid(0);
        kept.insert(shown.clone(), held.clone());
        let first = ("\\frac{a}{b}".to_string(), false, 8f32.to_bits());
        let gone = laid(1);
        kept.insert(first.clone(), gone.clone());
        let (mut laid_out, mut most) = (0, 0);
        for size in 8..40 {
            for i in 0..2000 {
                let key = (
                    format!("\\frac{{a + {i}}}{{b}}"),
                    false,
                    (size as f32).to_bits(),
                );
                if kept.get(&key).is_none() {
                    let laid = laid(i);
                    laid_out += weight(&key, laid.as_deref());
                    kept.insert(key, laid);
                }
                // As a view lays its formula out again, a zoom or an edit beside it.
                if i % 100 == 0 {
                    let again = kept.get(&shown).flatten();
                    assert!(again.is_some_and(|again| Rc::ptr_eq(&again, held.as_ref().unwrap())));
                    let all = kept.new.iter().chain(&kept.old);
                    most = most.max(all.map(|(k, l)| weight(k, l.as_deref())).sum());
                }
            }
        }
        assert!(laid_out > 8 * GENERATION, "{laid_out}");
        assert!(most <= 2 * GENERATION + 2 * weight(&first, laid(49).as_deref()));
        assert!(kept.get(&first).is_none());
        assert_eq!(gone.unwrap().items.len(), 2);
    }
}
