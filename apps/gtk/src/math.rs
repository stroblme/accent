//! Formulas typeset natively: TeX through core's MathML into `accent_math`'s layout, painted as
//! render nodes wherever a snapshot goes, a canvas, a thumbnail or an export alike.
//!
//! The font is the first of [`FAMILIES`] Pango has that is a math font, its glyphs drawn as text
//! in that Pango font. Without one it is the bundled STIX Two Math, which no font map knows (and
//! CoreText cannot be given), its glyphs drawn from their outlines. A character the math font
//! lacks, a CJK `\text{}` say, is set by Pango in [`FALLBACK`] and the fonts it falls back on.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::rc::Rc;

use accent_math::{Color, Item, Layout, MathFont, PathOp};
use gtk::glib::translate::{ToGlibPtr, ToGlibPtrMut};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

/// The system math fonts, in the order WebKit was given them: Latin Modern's look is closest to
/// draw.io's MathJax.
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
    // ponytail: every formula laid out is kept for the app's life, by TeX, display and size;
    // forgetting the ones nothing has painted for a while is the upgrade if that ever weighs.
    static FORMULAS: RefCell<HashMap<Key, Option<Rc<Layout>>>> = RefCell::default();
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
    let key = (tex.to_string(), display, size.to_bits());
    if let Some(kept) = FORMULAS.with_borrow(|laid| laid.get(&key).cloned()) {
        return kept;
    }
    let fonts = fonts(widget.as_ref());
    // A panic here would abort the app from inside a GTK callback: the source is shown instead.
    let laid = catch_unwind(AssertUnwindSafe(|| {
        let root = accent_math::parse(&accent_core::markdown::mathml(tex, display)?)
            .inspect_err(|e| tracing::debug!("{tex:?}: {e}"))
            .ok()?;
        if !root.warnings.is_empty() {
            tracing::debug!("{tex:?}: {:?}", root.warnings);
        }
        Some(Rc::new(accent_math::layout(&root, &fonts.math, size)))
    }))
    .unwrap_or_else(|_| {
        tracing::warn!("laying out {tex:?} panicked; its source is shown");
        None
    });
    FORMULAS.with_borrow_mut(|laid_out| laid_out.insert(key, laid.clone()));
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
                snapshot.append_color(&rgba(color), &graphene::Rect::new(x, y, w, h));
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
    /// A formula on its own, as a notebook's LaTeX output: as large as its layout, painted in the
    /// widget's colour, so a theme change only paints it again.
    pub struct FormulaView(ObjectSubclass<imp::FormulaView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FormulaView {
    pub fn new(formula: Rc<Layout>) -> FormulaView {
        let view: FormulaView = glib::Object::new();
        let _ = view.imp().formula.set(formula);
        view
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FormulaView {
        pub(super) formula: OnceCell<Rc<Layout>>,
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
            match orientation {
                gtk::Orientation::Horizontal => {
                    let w = f.width.ceil() as i32;
                    (w, w, -1, -1)
                }
                _ => {
                    let (above, below) = (f.ascent.ceil() as i32, f.descent.ceil() as i32);
                    (above + below, above + below, above, above)
                }
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(f) = self.formula.get() else {
                return;
            };
            let ink = self.obj().color();
            let colour = |c: Color| crate::theme::rgba([c.r, c.g, c.b], f32::from(c.a) / 255.0);
            paint(snapshot, f, (0.0, f.ascent.ceil()), 1.0, &ink, colour);
        }
    }
}
