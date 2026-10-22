// Derived from formulary 0.3.0 `src/font.rs` (MIT OR Apache-2.0, Copyright (c) 2026 Jeffrey T.
// Peckham (Ophymx)), modified for accent; see crates/math/NOTICE.
//! The math font: OpenType MATH metrics, shaping and outlines, all from the system HarfBuzz.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_int, c_uint, c_void};
use std::ptr::{self, NonNull};

use crate::Error;
use crate::hb;

/// HarfBuzz answers in integers at the font's scale. At this many per design unit, the half
/// advance it falls back to for a top accent, and the height a MathKern step is looked up at,
/// keep their fraction.
const SUBUNITS: i32 = 64;

/// The MATH constants the layout reads, numbered as `hb_ot_math_constant_t`.
#[derive(Clone, Copy)]
#[repr(i32)]
pub(crate) enum Constant {
    ScriptPercentScaleDown = 0,
    ScriptScriptPercentScaleDown = 1,
    DisplayOperatorMinHeight = 3,
    AxisHeight = 5,
    AccentBaseHeight = 6,
    SubscriptShiftDown = 8,
    SubscriptTopMax = 9,
    SubscriptBaselineDropMin = 10,
    SuperscriptShiftUp = 11,
    SuperscriptShiftUpCramped = 12,
    SuperscriptBottomMin = 13,
    SuperscriptBaselineDropMax = 14,
    SubSuperscriptGapMin = 15,
    SuperscriptBottomMaxWithSubscript = 16,
    SpaceAfterScript = 17,
    UpperLimitGapMin = 18,
    UpperLimitBaselineRiseMin = 19,
    LowerLimitGapMin = 20,
    LowerLimitBaselineDropMin = 21,
    StackTopShiftUp = 22,
    StackTopDisplayStyleShiftUp = 23,
    StackBottomShiftDown = 24,
    StackBottomDisplayStyleShiftDown = 25,
    StackGapMin = 26,
    StackDisplayStyleGapMin = 27,
    FractionNumeratorShiftUp = 32,
    FractionNumeratorDisplayStyleShiftUp = 33,
    FractionDenominatorShiftDown = 34,
    FractionDenominatorDisplayStyleShiftDown = 35,
    FractionNumeratorGapMin = 36,
    FractionNumDisplayStyleGapMin = 37,
    FractionRuleThickness = 38,
    FractionDenominatorGapMin = 39,
    FractionDenomDisplayStyleGapMin = 40,
    OverbarVerticalGap = 43,
    OverbarRuleThickness = 44,
    OverbarExtraAscender = 45,
    UnderbarVerticalGap = 46,
    UnderbarRuleThickness = 47,
    UnderbarExtraDescender = 48,
    RadicalVerticalGap = 49,
    RadicalDisplayStyleVerticalGap = 50,
    RadicalRuleThickness = 51,
    RadicalExtraAscender = 52,
    RadicalKernBeforeDegree = 53,
    RadicalKernAfterDegree = 54,
    RadicalDegreeBottomRaisePercent = 55,
}

/// A corner of a glyph for MathKern, numbered as `hb_ot_math_kern_t`.
#[derive(Clone, Copy)]
#[repr(i32)]
pub(crate) enum Corner {
    TopRight = 0,
    TopLeft = 1,
    BottomRight = 2,
    BottomLeft = 3,
}

/// A glyph's ink box in design units, y up.
#[derive(Clone, Copy)]
pub(crate) struct Ink {
    pub y_min: f32,
    pub y_max: f32,
}

/// A stretched glyph: one glyph, a variant or the base, or a stack of assembly parts.
pub(crate) enum Stretched {
    Glyph(u16),
    /// `parts` are `(glyph, offset along the stretch axis)` in design units from the
    /// assembly's start (its bottom when vertical, its left when horizontal); `extent` is the
    /// assembled size along that axis.
    Assembly {
        parts: Vec<(u16, f32)>,
        extent: f32,
    },
}

/// One glyph of a shaped run, in design units.
pub(crate) struct Shaped {
    pub id: u16,
    /// The byte in the run's text the glyph's cluster starts at.
    pub cluster: usize,
    pub x_advance: f32,
    pub x_offset: f32,
    /// Up, as HarfBuzz has it.
    pub y_offset: f32,
}

/// One step of a glyph outline, in em units with y down: scaled by a glyph item's `size` and
/// moved to its `(x, y)`, the path covers the glyph's ink.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathOp {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    /// Control point, then end point.
    QuadTo(f32, f32, f32, f32),
    /// Two control points, then end point.
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

/// A font with an OpenType MATH table, read through HarfBuzz. Not `Send`: it owns HarfBuzz
/// objects and a cache of ink boxes, and lives on the thread that draws with it.
pub struct MathFont {
    font: NonNull<hb::hb_font_t>,
    face: NonNull<hb::hb_face_t>,
    draw: NonNull<hb::hb_draw_funcs_t>,
    units_per_em: f32,
    /// The GSUB lookups behind `ssty`, the font's script-style alternates.
    ssty: Vec<c_uint>,
    /// HarfBuzz walks a CFF charstring for every extents call; layout asks per glyph placed.
    ink: RefCell<HashMap<u16, Option<Ink>>>,
    fallback: Option<Box<Fallback>>,
}

/// The caller's measure of text in a font of its own: a run's advance, and its ink above and
/// below the baseline, at a size; see [`MathFont::set_fallback`].
type Fallback = dyn Fn(&str, f32) -> Option<(f32, f32, f32)>;

impl MathFont {
    /// The math font behind a HarfBuzz font, such as the one a Pango font draws with. Only its
    /// face is kept, with a reference of its own; the font's scale and size do not matter.
    ///
    /// # Safety
    ///
    /// `font` is a live `hb_font_t`, as `pango_font_get_hb_font` returns.
    pub unsafe fn from_hb_font(font: NonNull<c_void>) -> Result<Self, Error> {
        // SAFETY: the caller hands a live font, and `hb_font_create` takes its own reference
        // to the face.
        unsafe {
            Self::new(hb::hb_font_create(hb::hb_font_get_face(
                font.as_ptr().cast(),
            )))
        }
    }

    /// The math font in a font file's bytes, such as the bundled STIX Two Math.
    pub fn from_static(data: &'static [u8]) -> Result<Self, Error> {
        let len = c_uint::try_from(data.len()).map_err(|_| Error::NoMathTable)?;
        // SAFETY: the bytes live as long as the program, so the read-only blob may borrow
        // them; face and font each hold their own reference to what they are made of.
        unsafe {
            let blob = hb::hb_blob_create(
                data.as_ptr().cast(),
                len,
                hb::HB_MEMORY_MODE_READONLY,
                ptr::null_mut(),
                None,
            );
            let face = hb::hb_face_create(blob, 0);
            hb::hb_blob_destroy(blob);
            let font = hb::hb_font_create(face);
            hb::hb_face_destroy(face);
            Self::new(font)
        }
    }

    /// Takes over `font`, a fresh one of our own.
    unsafe fn new(font: *mut hb::hb_font_t) -> Result<Self, Error> {
        // SAFETY: HarfBuzz returns its inert empty objects rather than null, so every pointer
        // here is valid, and `font` is ours to set up or destroy.
        unsafe {
            let face = hb::hb_font_get_face(font);
            if hb::hb_ot_math_has_data(face) == 0 {
                hb::hb_font_destroy(font);
                return Err(Error::NoMathTable);
            }
            hb::hb_ot_font_set_funcs(font);
            let upem = hb::hb_face_get_upem(face);
            let scale = c_int::try_from(upem).unwrap_or(1000) * SUBUNITS;
            hb::hb_font_set_scale(font, scale, scale);

            let features = [hb::tag(b"ssty"), 0];
            let set = hb::hb_set_create();
            hb::hb_ot_layout_collect_lookups(
                face,
                hb::HB_OT_TAG_GSUB,
                ptr::null(),
                ptr::null(),
                features.as_ptr(),
                set,
            );
            let mut ssty = Vec::new();
            let mut lookup = hb::HB_SET_VALUE_INVALID;
            while hb::hb_set_next(set, &mut lookup) != 0 {
                ssty.push(lookup);
            }
            hb::hb_set_destroy(set);

            let draw = hb::hb_draw_funcs_create();
            hb::hb_draw_funcs_set_move_to_func(draw, move_to, ptr::null_mut(), None);
            hb::hb_draw_funcs_set_line_to_func(draw, line_to, ptr::null_mut(), None);
            hb::hb_draw_funcs_set_quadratic_to_func(draw, quad_to, ptr::null_mut(), None);
            hb::hb_draw_funcs_set_cubic_to_func(draw, cubic_to, ptr::null_mut(), None);
            hb::hb_draw_funcs_set_close_path_func(draw, close_path, ptr::null_mut(), None);
            hb::hb_draw_funcs_make_immutable(draw);

            Ok(MathFont {
                font: NonNull::new_unchecked(font),
                face: NonNull::new_unchecked(face),
                draw: NonNull::new_unchecked(draw),
                units_per_em: upem as f32,
                ssty,
                ink: RefCell::default(),
                fallback: None,
            })
        }
    }

    /// Have the characters this font lacks laid out by `measure`, the caller's own text stack,
    /// which answers a run's advance and its ink above and below the baseline at a size. The run
    /// becomes an [`Item::Text`](crate::Item::Text) the caller draws in the font it measured with.
    /// Without a fallback such characters are left out.
    pub fn set_fallback(
        &mut self,
        measure: impl Fn(&str, f32) -> Option<(f32, f32, f32)> + 'static,
    ) {
        self.fallback = Some(Box::new(measure));
    }

    /// `text`, characters this font lacks, measured at `size` by the caller's fallback.
    pub(crate) fn fallback(&self, text: &str, size: f32) -> Option<(f32, f32, f32)> {
        self.fallback.as_ref()?(text, size)
    }

    fn hb(&self) -> *mut hb::hb_font_t {
        self.font.as_ptr()
    }

    pub(crate) fn units_per_em(&self) -> f32 {
        self.units_per_em
    }

    /// A MATH constant in design units. Not for the three percentages: [`Self::percent`].
    pub(crate) fn constant(&self, c: Constant) -> f32 {
        // SAFETY: a live font; HarfBuzz answers 0 for a constant the font lacks.
        unscale(unsafe { hb::hb_ot_math_get_constant(self.hb(), c as c_int) })
    }

    /// One of the MATH constants that are percentages, as a fraction.
    pub(crate) fn percent(&self, c: Constant) -> f32 {
        // SAFETY: as in `constant`; HarfBuzz does not scale the percentages.
        unsafe { hb::hb_ot_math_get_constant(self.hb(), c as c_int) as f32 / 100.0 }
    }

    pub(crate) fn glyph_index(&self, c: char) -> Option<u16> {
        let mut glyph = 0;
        // SAFETY: a live font and a valid out pointer.
        let found = unsafe { hb::hb_font_get_nominal_glyph(self.hb(), c.into(), &mut glyph) };
        (found != 0).then(|| u16::try_from(glyph).ok()).flatten()
    }

    /// Horizontal advance in design units.
    pub(crate) fn advance(&self, glyph: u16) -> f32 {
        // SAFETY: a live font; an id past the font's glyphs reads as 0.
        unscale(unsafe { hb::hb_font_get_glyph_h_advance(self.hb(), glyph.into()) })
    }

    /// The ink box, if the glyph has ink.
    pub(crate) fn ink_box(&self, glyph: u16) -> Option<Ink> {
        *self.ink.borrow_mut().entry(glyph).or_insert_with(|| {
            let mut e = hb::hb_glyph_extents_t::default();
            // SAFETY: a live font and a valid out pointer.
            let found = unsafe { hb::hb_font_get_glyph_extents(self.hb(), glyph.into(), &mut e) };
            // HarfBuzz reports a glyph with no contours as an empty box at the origin.
            (found != 0 && (e.width != 0 || e.height != 0)).then(|| Ink {
                y_max: unscale(e.y_bearing),
                y_min: unscale(e.y_bearing + e.height),
            })
        })
    }

    /// Italic correction in design units, 0 when absent: how far a slanted glyph's ink runs
    /// past its advance, which a superscript is moved right and a subscript left by.
    pub(crate) fn italic_correction(&self, glyph: u16) -> f32 {
        // SAFETY: a live font.
        unscale(unsafe { hb::hb_ot_math_get_glyph_italics_correction(self.hb(), glyph.into()) })
    }

    /// Where an accent attaches above `glyph`, in design units from its origin: the font's
    /// point, or half the advance when it has none.
    pub(crate) fn top_accent_attachment(&self, glyph: u16) -> f32 {
        // SAFETY: a live font.
        unscale(unsafe { hb::hb_ot_math_get_glyph_top_accent_attachment(self.hb(), glyph.into()) })
    }

    /// The MathKern cut-in at one corner of `glyph`, `height` design units above the baseline
    /// (below when negative); 0 when the font has none.
    pub(crate) fn math_kern(&self, glyph: u16, corner: Corner, height: f32) -> f32 {
        let height = (height * SUBUNITS as f32).round() as hb::hb_position_t;
        // SAFETY: a live font.
        unscale(unsafe {
            hb::hb_ot_math_get_glyph_kerning(self.hb(), glyph.into(), corner as c_int, height)
        })
    }

    /// x-height in design units, half an em when the font does not say.
    pub(crate) fn x_height(&self) -> f32 {
        let mut pos = 0;
        // SAFETY: a live font and a valid out pointer.
        let found = unsafe {
            hb::hb_ot_metrics_get_position(self.hb(), hb::HB_OT_METRICS_TAG_X_HEIGHT, &mut pos)
        };
        if found != 0 {
            unscale(pos)
        } else {
            self.units_per_em * 0.5
        }
    }

    /// Font-wide ascent and descent in design units, the descent positive.
    pub(crate) fn line_metrics(&self) -> (f32, f32) {
        let mut e = hb::hb_font_extents_t::default();
        // SAFETY: a live font and a valid out pointer.
        unsafe { hb::hb_font_get_h_extents(self.hb(), &mut e) };
        (unscale(e.ascender), -unscale(e.descender))
    }

    /// The font's `ssty` alternate for `glyph` at script depth `level`, 1 or 2: glyphs tuned
    /// for scripts, primes the classic case. Applied by hand, as runs are shaped under DFLT,
    /// where math fonts do not register `ssty`, and single glyphs (stretchy operators, accents)
    /// are not shaped at all.
    pub(crate) fn script_alternate(&self, glyph: u16, level: u8) -> Option<u16> {
        debug_assert!(level >= 1);
        for &lookup in &self.ssty {
            let face = self.face.as_ptr();
            let mut none = 0;
            // SAFETY: a live face; with a count of 0 nothing is written, the total comes back.
            let total = unsafe {
                hb::hb_ot_layout_lookup_get_glyph_alternates(
                    face,
                    lookup,
                    glyph.into(),
                    0,
                    &mut none,
                    ptr::null_mut(),
                )
            };
            let Some(last) = total.checked_sub(1) else {
                continue;
            };
            // Deeper nesting takes the furthest alternate the font has.
            let (mut count, mut alternate) = (1, 0);
            // SAFETY: room for the one alternate asked for.
            unsafe {
                hb::hb_ot_layout_lookup_get_glyph_alternates(
                    face,
                    lookup,
                    glyph.into(),
                    c_uint::from(level - 1).min(last),
                    &mut count,
                    &mut alternate,
                )
            };
            return u16::try_from(alternate).ok().filter(|_| count == 1);
        }
        None
    }

    /// A glyph stretched vertically to at least `target` design units.
    pub(crate) fn stretch_vertical(&self, glyph: u16, target: f32) -> Stretched {
        self.stretch(glyph, target, hb::HB_DIRECTION_TTB)
    }

    /// A glyph stretched horizontally to at least `target` design units.
    pub(crate) fn stretch_horizontal(&self, glyph: u16, target: f32) -> Stretched {
        self.stretch(glyph, target, hb::HB_DIRECTION_LTR)
    }

    /// The smallest variant that reaches `target`, else an assembly of the glyph's parts, else
    /// the largest variant (or the glyph itself) when neither reaches it.
    fn stretch(&self, glyph: u16, target: f32, direction: c_int) -> Stretched {
        let font = self.hb();
        // SAFETY (both lists): a live font, and room for as many items as asked for.
        let variants = list(|start, count, out| unsafe {
            hb::hb_ot_math_get_glyph_variants(font, glyph.into(), direction, start, count, out)
        });
        let mut best = glyph;
        for v in &variants {
            best = u16::try_from(v.glyph).unwrap_or(glyph);
            if unscale(v.advance) >= target {
                return Stretched::Glyph(best);
            }
        }
        let parts = list(|start, count, out| unsafe {
            let italic = ptr::null_mut();
            hb::hb_ot_math_get_glyph_assembly(
                font,
                glyph.into(),
                direction,
                start,
                count,
                out,
                italic,
            )
        });
        // SAFETY: a live font.
        let overlap = unscale(unsafe { hb::hb_ot_math_get_min_connector_overlap(font, direction) });
        assemble(&parts, overlap, target).unwrap_or(Stretched::Glyph(best))
    }

    /// `text` shaped as one left-to-right run, with `ssty` at script `level` 1 or 2.
    ///
    /// Shaped under DFLT, not the OpenType `math` script: math fonts register `ssty` and
    /// `dtls` under `math`, but STIX Two Math and fonts built like it leave `math`'s GPOS
    /// language system empty, so selecting it would drop `kern` from every run.
    pub(crate) fn shape(&self, text: &str, level: u8) -> Vec<Shaped> {
        let Ok(len) = c_int::try_from(text.len()) else {
            return Vec::new();
        };
        let ssty = hb::hb_feature_t {
            tag: hb::tag(b"ssty"),
            value: level.min(2).into(),
            start: 0,
            end: c_uint::MAX,
        };
        // SAFETY: a live font and a buffer of our own, read before it is destroyed.
        unsafe {
            let buffer = hb::hb_buffer_create();
            hb::hb_buffer_add_utf8(buffer, text.as_ptr().cast(), len, 0, len);
            hb::hb_buffer_set_direction(buffer, hb::HB_DIRECTION_LTR);
            hb::hb_buffer_set_script(buffer, hb::HB_SCRIPT_COMMON);
            hb::hb_shape(self.hb(), buffer, &ssty, c_uint::from(level > 0));
            let mut n = 0;
            let infos = hb::hb_buffer_get_glyph_infos(buffer, &mut n);
            let positions = hb::hb_buffer_get_glyph_positions(buffer, &mut n);
            let shaped = (0..n as usize)
                .map(|i| {
                    let (info, pos) = (&*infos.add(i), &*positions.add(i));
                    Shaped {
                        id: u16::try_from(info.codepoint).unwrap_or(0),
                        cluster: info.cluster as usize,
                        x_advance: unscale(pos.x_advance),
                        x_offset: unscale(pos.x_offset),
                        y_offset: unscale(pos.y_offset),
                    }
                })
                .collect();
            hb::hb_buffer_destroy(buffer);
            shaped
        }
    }

    /// `glyph`'s outline, for a renderer that draws the font as paths (the bundled font,
    /// which no font map knows). Empty for a glyph without ink.
    pub fn outline(&self, glyph: u16) -> Vec<PathOp> {
        let mut pen = Pen {
            ops: Vec::new(),
            scale: 1.0 / (self.units_per_em * SUBUNITS as f32),
        };
        // SAFETY: a live font and draw functions that only push onto `pen`, which outlives
        // the call.
        unsafe {
            hb::hb_font_draw_glyph(
                self.hb(),
                glyph.into(),
                self.draw.as_ptr(),
                (&raw mut pen).cast(),
            )
        };
        pen.ops
    }
}

impl Drop for MathFont {
    fn drop(&mut self) {
        // SAFETY: both are ours; the font releases the face it holds.
        unsafe {
            hb::hb_draw_funcs_destroy(self.draw.as_ptr());
            hb::hb_font_destroy(self.font.as_ptr());
        }
    }
}

fn unscale(v: hb::hb_position_t) -> f32 {
    v as f32 / SUBUNITS as f32
}

/// The whole of a HarfBuzz list: asked once for its length, once for its items.
fn list<T: Default + Clone>(get: impl Fn(c_uint, *mut c_uint, *mut T) -> c_uint) -> Vec<T> {
    let mut none = 0;
    let total = get(0, &mut none, ptr::null_mut());
    let mut items = vec![T::default(); total as usize];
    let mut count = total;
    get(0, &mut count, items.as_mut_ptr());
    items.truncate(count as usize);
    items
}

/// Stacks assembly parts, listed start to end along the stretch axis, to reach `target`
/// design units: the extenders repeated as needed, with one overlap at every joint.
fn assemble(
    parts: &[hb::hb_ot_math_glyph_part_t],
    min_overlap: f32,
    target: f32,
) -> Option<Stretched> {
    let extender =
        |p: &hb::hb_ot_math_glyph_part_t| p.flags & hb::HB_OT_MATH_GLYPH_PART_FLAG_EXTENDER != 0;
    for repeats in 1..=32 {
        let seq: Vec<_> = parts
            .iter()
            .flat_map(|p| std::iter::repeat_n(p, if extender(p) { repeats } else { 1 }))
            .collect();
        if seq.len() < 2 {
            return None;
        }
        let sum: f32 = seq.iter().map(|p| unscale(p.full_advance)).sum();
        let joints = (seq.len() - 1) as f32;
        if sum - min_overlap * joints < target {
            continue; // not enough parts yet, even at the least overlap
        }
        // The overlap that lands on target, within what the connectors allow.
        let max_overlap = seq
            .windows(2)
            .map(|w| unscale(w[0].end_connector_length.min(w[1].start_connector_length)))
            .fold(f32::INFINITY, f32::min);
        let overlap = ((sum - target) / joints).clamp(min_overlap, max_overlap.max(min_overlap));

        let mut placed = Vec::with_capacity(seq.len());
        let mut start = 0.0;
        for p in &seq {
            placed.push((u16::try_from(p.glyph).unwrap_or(0), start));
            start += unscale(p.full_advance) - overlap;
        }
        return Some(Stretched::Assembly {
            parts: placed,
            extent: start + overlap,
        });
    }
    None
}

/// Where `MathFont::outline`'s draw functions write.
struct Pen {
    ops: Vec<PathOp>,
    /// Font scale to em.
    scale: f32,
}

/// The pen behind a draw function's `draw_data`, and `(x, y)` in em with y down.
///
/// # Safety
///
/// `data` is the `Pen` `MathFont::outline` passed.
unsafe fn pen<'a>(data: *mut c_void, x: f32, y: f32) -> (&'a mut Pen, f32, f32) {
    // SAFETY: the caller's promise.
    let pen = unsafe { &mut *data.cast::<Pen>() };
    let (x, y) = (x * pen.scale, -y * pen.scale);
    (pen, x, y)
}

unsafe extern "C" fn move_to(
    _: *mut hb::hb_draw_funcs_t,
    data: *mut c_void,
    _: *mut hb::hb_draw_state_t,
    x: f32,
    y: f32,
    _: *mut c_void,
) {
    // SAFETY: HarfBuzz hands back `outline`'s pen.
    let (pen, x, y) = unsafe { pen(data, x, y) };
    pen.ops.push(PathOp::MoveTo(x, y));
}

unsafe extern "C" fn line_to(
    _: *mut hb::hb_draw_funcs_t,
    data: *mut c_void,
    _: *mut hb::hb_draw_state_t,
    x: f32,
    y: f32,
    _: *mut c_void,
) {
    // SAFETY: as in `move_to`.
    let (pen, x, y) = unsafe { pen(data, x, y) };
    pen.ops.push(PathOp::LineTo(x, y));
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn quad_to(
    _: *mut hb::hb_draw_funcs_t,
    data: *mut c_void,
    _: *mut hb::hb_draw_state_t,
    cx: f32,
    cy: f32,
    x: f32,
    y: f32,
    _: *mut c_void,
) {
    // SAFETY: as in `move_to`.
    let (pen, x, y) = unsafe { pen(data, x, y) };
    let (cx, cy) = (cx * pen.scale, -cy * pen.scale);
    pen.ops.push(PathOp::QuadTo(cx, cy, x, y));
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn cubic_to(
    _: *mut hb::hb_draw_funcs_t,
    data: *mut c_void,
    _: *mut hb::hb_draw_state_t,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x: f32,
    y: f32,
    _: *mut c_void,
) {
    // SAFETY: as in `move_to`.
    let (pen, x, y) = unsafe { pen(data, x, y) };
    let s = pen.scale;
    pen.ops
        .push(PathOp::CubicTo(c1x * s, -c1y * s, c2x * s, -c2y * s, x, y));
}

unsafe extern "C" fn close_path(
    _: *mut hb::hb_draw_funcs_t,
    data: *mut c_void,
    _: *mut hb::hb_draw_state_t,
    _: *mut c_void,
) {
    // SAFETY: as in `move_to`.
    let (pen, _, _) = unsafe { pen(data, 0.0, 0.0) };
    pen.ops.push(PathOp::Close);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::STIX_TWO_MATH as STIX;

    /// A HarfBuzz font, as Pango hands one over, gives the same font as the bytes behind it.
    #[test]
    fn a_harfbuzz_font_is_its_face() {
        let bytes = MathFont::from_static(STIX).unwrap();
        // A second font over the same face, at a scale of its own, as Pango's would be.
        let theirs = unsafe {
            let font = hb::hb_font_create(bytes.face.as_ptr());
            hb::hb_font_set_scale(font, 12, 12);
            font
        };
        let font = unsafe { MathFont::from_hb_font(NonNull::new(theirs.cast()).unwrap()) };
        unsafe { hb::hb_font_destroy(theirs) };
        let font = font.unwrap();
        let x = font.glyph_index('x').unwrap();
        assert_eq!(Some(x), bytes.glyph_index('x'));
        assert_eq!(font.advance(x), bytes.advance(x));
        assert_eq!(
            font.constant(Constant::AxisHeight),
            bytes.constant(Constant::AxisHeight)
        );
    }

    #[test]
    fn a_font_without_a_math_table_is_refused() {
        assert!(matches!(
            MathFont::from_static(b"not a font"),
            Err(Error::NoMathTable)
        ));
    }

    /// The outline is in em, y down: a glyph sitting on the baseline reaches up to negative y.
    #[test]
    fn outlines_are_in_em_with_y_down() {
        let font = MathFont::from_static(STIX).unwrap();
        let ops = font.outline(font.glyph_index('2').unwrap());
        assert!(matches!(ops.first(), Some(PathOp::MoveTo(..))));
        assert!(matches!(ops.last(), Some(PathOp::Close)));
        let ys = ops.iter().filter_map(|op| match *op {
            PathOp::MoveTo(_, y) | PathOp::LineTo(_, y) => Some(y),
            PathOp::QuadTo(.., y) | PathOp::CubicTo(.., y) => Some(y),
            PathOp::Close => None,
        });
        let top = ys.fold(0.0_f32, f32::min);
        assert!((-0.7..-0.6).contains(&top), "{top}");
        assert!(font.outline(font.glyph_index(' ').unwrap()).is_empty());
    }
}
