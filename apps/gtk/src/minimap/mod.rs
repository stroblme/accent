//! The minimap beside a text tab: the document drawn small, a bar per word, under a band over
//! the lines the view shows. It stands in for the vertical scrollbar (`Tab::set_minimap`): a
//! press puts the line under it in the middle of the view, a press on the band takes hold of it,
//! and either then scrolls the view with the pointer; the wheel scrolls the view. The caret never
//! moves.
//!
//! Monochrome but for the accent: words in the foreground, a dimmer tone of it where the editor
//! dims the text, and the accent where the editor writes in it or, in code, writes a keyword;
//! never a syntax colour. A heading's text as a label to read, sized and indented by its level, as
//! VS Code draws a section header; a comparison's or a conflict's row tints as the page has them.
//!
//! Ours rather than GtkSourceMap, which laid the whole document out a second time in a view of
//! its own, painted no band on GTK 4.22 and followed the adjustment its view had when it was set
//! (ISSUES.md). Nothing here asks GTK's layout where a row is, which is where its hidden-text
//! aborts live: the rows are [`model`]'s estimate from the characters on each line, and the band
//! is the lines the view shows, placed on those rows.
//!
//! Drawn in chunks of [`CHUNK`] lines, each kept as a texture and put back at its row until
//! something in it changes: an edit, a tag that changes how a line looks, a new width, ink, accent
//! or screen scale. A texture rather than the rectangles themselves, which cairo paints one by
//! one, thousands to a frame. The buffer is followed only while the map is on screen; one that
//! changed while it was not is read whole again when it comes back.

mod model;

use crate::{highlight, theme};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use model::{Line, Model};
use sourceview5::prelude::{BufferExt as _, ViewExt as _};
use std::ops::Range;

/// The map's width, in logical pixels.
const WIDTH: i32 = 100;
/// A row's height in logical pixels; a character is one pixel wide.
const ROW: f64 = 2.0;
/// Lines per cached chunk. An edit draws its chunk again and a scroll the chunks coming onto the
/// map: 32 drew a keystroke's in half the time 64 did and scrolled as cheaply, where 16 gained
/// nothing more (`ACCENT_BENCH_MINIMAP=typing:` and `=frames:`, `map_us`).
const CHUNK: usize = 32;
/// How many chunks above and below the map's window are kept for a scroll to come back to.
const KEEP: usize = 4;
/// A heading label's size in logical pixels by level, h1 first, and how much further in each
/// level starts: h1 largest and leftmost, as an outline reads.
const LABEL_PX: [f64; 6] = [11.0, 10.0, 9.0, 8.5, 8.0, 8.0];
const LABEL_INDENT: f64 = 4.0;
/// GtkSourceView's tag over a comment, which the language spec names; the style tags that colour
/// code have no names.
const COMMENT: &str = "gtksourceview:context-classes:comment";

/// The rows a heading label of `level` takes: its size, with a line's leading.
fn label_rows(level: u8) -> u32 {
    (LABEL_PX[usize::from(level.clamp(1, 6) - 1)] * 1.35 / ROW).ceil() as u32
}

/// A word's tone: the foreground, a dimmer one where the editor dims the text (a note's markup,
/// quotes and done tasks, a comment in code), and the accent where the editor writes in it (a
/// note's links and tags) or, in code, where it writes a keyword.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Ink,
    Dim,
    Accent,
}

impl Tone {
    /// The tone of the text at `at`, from its highest-priority tag that has one, as the editor
    /// paints the foreground of the highest-priority tag that sets one. `keywords` are the looks
    /// a keyword has in the style scheme ([`keyword_looks`]).
    fn at(at: &gtk::TextIter, keywords: &[gtk::TextTag]) -> Tone {
        for tag in at.tags().iter().rev() {
            let Some(name) = tag.name() else {
                if keywords.iter().any(|look| same_look(look, tag)) {
                    return Tone::Accent;
                }
                continue;
            };
            let name = name.as_str();
            if highlight::ACCENT_TAGS.contains(&name) {
                return Tone::Accent;
            }
            if name == COMMENT
                || highlight::MARKUP_TAGS.contains(&name)
                || highlight::FADED_TAGS.contains(&name)
            {
                return Tone::Dim;
            }
        }
        Tone::Ink
    }
}

/// What every chunk of a frame is drawn with.
struct Paint {
    /// The foreground itself, which a label is written in.
    text: gdk::RGBA,
    ink: gdk::RGBA,
    dim: gdk::RGBA,
    accent: gdk::RGBA,
    renderer: gsk::Renderer,
    scale: f64,
    context: pango::Context,
    /// The labels' face, bold; each level sets its own size.
    label: pango::FontDescription,
    /// The looks of a keyword ([`keyword_looks`]), found when a chunk is first drawn.
    keywords: std::cell::OnceCell<Vec<gtk::TextTag>>,
}

/// Probe tags with the looks a keyword and a statement have in `buffer`'s style scheme. The
/// highlighter's tags have no names, only the style it gave them, so a keyword's tag is the one
/// that looks exactly like this. The scheme names the two styles, or the `def` language maps them
/// to one it does: Adwaita's keywords are its statements.
fn keyword_looks(buffer: &gtk::TextBuffer) -> Vec<gtk::TextTag> {
    let Some(buffer) = buffer
        .downcast_ref::<sourceview5::Buffer>()
        .filter(|buffer| buffer.is_highlight_syntax())
    else {
        return Vec::new();
    };
    let manager = sourceview5::LanguageManager::default();
    let (Some(scheme), Some(def)) = (buffer.style_scheme(), manager.language("def")) else {
        return Vec::new();
    };
    ["def:keyword", "def:statement"]
        .into_iter()
        .filter_map(|id| {
            let mut id = glib::GString::from(id);
            let style = loop {
                match scheme.style(&id) {
                    Some(style) => break style,
                    None => id = def.style_fallback(&id)?,
                }
            };
            let look = gtk::TextTag::new(None);
            style.apply(&look);
            Some(look)
        })
        .collect()
}

/// Whether `tag` writes its text exactly as `look` does: the same colour, weight and slant.
fn same_look(look: &gtk::TextTag, tag: &gtk::TextTag) -> bool {
    look.is_foreground_set() == tag.is_foreground_set()
        && look.foreground_rgba() == tag.foreground_rgba()
        && look.is_weight_set() == tag.is_weight_set()
        && (!look.is_weight_set() || look.weight() == tag.weight())
        && look.is_style_set() == tag.is_style_set()
        && (!look.is_style_set() || look.style() == tag.style())
}

glib::wrapper! {
    pub struct Minimap(ObjectSubclass<imp::Minimap>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Minimap {
    /// A map of `view`, following its buffer and whichever vertical adjustment it has.
    pub fn new(view: &sourceview5::View) -> Self {
        let map: Self = glib::Object::builder()
            .property("accessible-role", gtk::AccessibleRole::Presentation)
            .build();
        map.imp().attach(view);
        map
    }

    /// Measure the page again from the next frame: the font or the zoom changed.
    pub fn relayout(&self) {
        self.imp().char_width.set(0.0);
        self.restyle();
    }

    /// Draw again from scratch, in the theme's colours.
    pub fn restyle(&self) {
        self.imp().chunks.borrow_mut().clear();
        self.queue_draw();
    }

    fn install_input(&self) {
        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = map)]
            self,
            move |gesture, _, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                map.imp().press(y);
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = map)]
            self,
            move |_, _, dy| map.imp().drag(dy)
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = map)]
            self,
            move |_, _, _| map.imp().release()
        ));
        self.add_controller(drag);
        // The band is stronger while the pointer is over the map, as a scrollbar's slider is.
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter(glib::clone!(
            #[weak(rename_to = map)]
            self,
            move |_, _, _| map.imp().set_hover(true)
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = map)]
            self,
            move |_| map.imp().set_hover(false)
        ));
        self.add_controller(motion);
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.connect_scroll(glib::clone!(
            #[weak(rename_to = map)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |wheel, _, dy| map.imp().wheel(wheel, dy)
        ));
        self.add_controller(wheel);
    }
}

/// What the drills read and do.
#[cfg(feature = "bench")]
impl Minimap {
    /// The lines under the band's top and bottom edges, as last drawn.
    pub fn band_lines(&self) -> Option<(usize, usize)> {
        let frame = self.imp().frame.get()?;
        let model = self.imp().model.borrow();
        let bottom = frame.top + frame.band - 1e-3;
        Some((model.line_at_row(frame.top).0, model.line_at_row(bottom).0))
    }

    /// The band's top and height, in the map's pixels, as last drawn.
    pub fn band_px(&self) -> Option<(f64, f64)> {
        let frame = self.imp().frame.get()?;
        Some(((frame.top - frame.offset) * ROW, frame.band * ROW))
    }

    /// The line drawn at `y`.
    pub fn line_at(&self, y: f64) -> Option<usize> {
        let frame = self.imp().frame.get()?;
        Some(
            self.imp()
                .model
                .borrow()
                .line_at_row(y / ROW + frame.offset)
                .0,
        )
    }

    /// A press at `y`, a drag `dy` on from it and the release, as the gesture hands them over.
    pub fn press_at(&self, y: f64) {
        self.imp().press(y);
    }

    pub fn drag_by(&self, dy: f64) {
        self.imp().drag(dy);
    }

    pub fn let_go(&self) {
        self.imp().release();
    }

    /// The time the map has spent drawing since this was last asked: its model brought up to the
    /// buffer, its chunks drawn and the frame put together.
    pub fn take_spent(&self) -> std::time::Duration {
        self.imp().spent.take()
    }
}

impl imp::Minimap {
    fn attach(&self, view: &sourceview5::View) {
        self.view.set(Some(view));
        let map = self.obj();
        let buffer = view.buffer();
        // Each handler holds the map weakly and is left on the buffer and the view, which live as
        // long as the tab the map is part of.
        buffer.connect_insert_text(glib::clone!(
            #[weak]
            map,
            move |_, at, _| map.imp().edit.set(Some(at.line().max(0) as usize))
        ));
        buffer.connect_delete_range(glib::clone!(
            #[weak]
            map,
            move |_, start, _| map.imp().edit.set(Some(start.line().max(0) as usize))
        ));
        buffer.connect_changed(glib::clone!(
            #[weak]
            map,
            move |buffer| map.imp().changed(buffer)
        ));
        buffer.connect_apply_tag(glib::clone!(
            #[weak]
            map,
            move |_, tag, start, end| map.imp().tagged(tag, start, end)
        ));
        buffer.connect_remove_tag(glib::clone!(
            #[weak]
            map,
            move |_, tag, start, end| map.imp().tagged(tag, start, end)
        ));
        // A comparison puts the view on the adjustment its columns share, and gives it its own
        // back when it goes.
        view.connect_notify_local(
            Some("vadjustment"),
            glib::clone!(
                #[weak]
                map,
                move |view, _| map.imp().follow(view.vadjustment())
            ),
        );
        self.follow(view.vadjustment());
    }

    /// Redraw whenever `adjustment` scrolls or resizes, and stop hearing the one before it.
    fn follow(&self, adjustment: Option<gtk::Adjustment>) {
        if let Some((old, handlers)) = self.adjustment.take() {
            for handler in handlers {
                old.disconnect(handler);
            }
        }
        let Some(adjustment) = adjustment else {
            return;
        };
        let map = self.obj();
        let handlers = vec![
            adjustment.connect_value_changed(glib::clone!(
                #[weak]
                map,
                move |_| map.queue_draw()
            )),
            adjustment.connect_changed(glib::clone!(
                #[weak]
                map,
                move |_| map.queue_draw()
            )),
        ];
        self.adjustment.replace(Some((adjustment, handlers)));
        map.queue_draw();
    }

    /// The buffer's text changed at the line `insert-text` or `delete-range` said.
    fn changed(&self, buffer: &gtk::TextBuffer) {
        let edit = self.edit.take();
        let map = self.obj();
        if !map.is_mapped() {
            self.stale.set(true);
            return;
        }
        self.highlighted.set(None);
        let mut model = self.model.borrow_mut();
        let mut chunks = self.chunks.borrow_mut();
        match edit.filter(|_| model.len() > 0) {
            Some(at) => {
                let delta = buffer.line_count() as isize - model.len() as isize;
                model.splice(at, delta);
                // A line more or fewer moves every chunk after this one onto other lines.
                match delta {
                    0 => drop_chunk(&mut chunks, at / CHUNK),
                    _ => chunks.truncate(at / CHUNK),
                }
            }
            None => self.stale.set(true),
        }
        map.queue_draw();
    }

    /// A tag went on or came off between `start` and `end`.
    fn tagged(&self, tag: &gtk::TextTag, start: &gtk::TextIter, end: &gtk::TextIter) {
        if !matters(tag) {
            return;
        }
        let map = self.obj();
        if !map.is_mapped() {
            self.stale.set(true);
            return;
        }
        let (from, to) = (start.line().max(0) as usize, end.line().max(0) as usize);
        self.model.borrow_mut().unmeasure(from..to + 1);
        let mut chunks = self.chunks.borrow_mut();
        for chunk in from / CHUNK..=to / CHUNK {
            drop_chunk(&mut chunks, chunk);
        }
        map.queue_draw();
    }

    /// The view's text width in characters of its font, which is how many the page wraps at.
    fn char_width(&self, view: &sourceview5::View) -> f64 {
        if self.char_width.get() <= 0.0 {
            let metrics = view.pango_context().metrics(None, None);
            let width = f64::from(metrics.approximate_char_width()) / f64::from(pango::SCALE);
            self.char_width.set(width.max(1.0));
        }
        self.char_width.get()
    }

    /// Bring the model up to the buffer and the view's width: `false` while the view has none.
    fn prepare(&self, view: &sourceview5::View) -> bool {
        let width = view.visible_rect().width() - view.left_margin() - view.right_margin();
        if width <= 0 {
            return false;
        }
        let buffer = view.buffer();
        let count = buffer.line_count().max(0) as usize;
        let mut model = self.model.borrow_mut();
        let mut chunks = self.chunks.borrow_mut();
        // A count that disagrees means an edit the handlers missed: read it all again.
        if self.stale.take() || model.len() != count {
            model.reset(count);
            chunks.clear();
        }
        let cols = match view.wrap_mode() {
            gtk::WrapMode::None => None,
            _ => Some((f64::from(width) / self.char_width(view)).max(1.0) as u32),
        };
        if model.set_cols(cols) {
            chunks.clear();
        }
        for at in model.take_unmeasured() {
            model.set(at, measure(&buffer, at));
            drop_chunk(&mut chunks, at / CHUNK);
        }
        model.sum();
        true
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let obj = self.obj();
        let Some(view) = self.view.upgrade() else {
            return;
        };
        if !self.prepare(&view) {
            return;
        }
        let Some(native) = obj.native() else {
            return;
        };
        let Some(renderer) = native.renderer() else {
            return;
        };
        let ink = obj.color();
        let accent = highlight::text_accent();
        let scale = native.surface().map_or(1.0, |surface| surface.scale());
        if self.drawn.replace(Some((ink, accent, scale))) != Some((ink, accent, scale)) {
            self.chunks.borrow_mut().clear();
        }
        let context = obj.pango_context();
        let mut label = context.font_description().unwrap_or_default();
        label.set_weight(pango::Weight::Bold);
        let paint = Paint {
            text: ink,
            ink: theme::at(ink, theme::MAP_INK_ALPHA),
            dim: theme::at(ink, theme::MAP_DIM_ALPHA),
            accent: theme::at(accent, theme::MAP_ACCENT_ALPHA),
            renderer,
            scale,
            context,
            label,
            keywords: std::cell::OnceCell::new(),
        };
        let model = self.model.borrow();
        let (width, height) = (f64::from(obj.width()), f64::from(obj.height()));
        let total = f64::from(model.total());
        let (top, bottom) = band(&view, &model);
        let band = (bottom - top).max(1.0);
        let rows = height / ROW;
        // The rows a screen holds on average: the document's rows over its pixels, a screen high.
        let usual = view
            .vadjustment()
            .filter(|adjustment| adjustment.upper() > 0.0)
            .map_or(band, |adjustment| {
                total * adjustment.page_size() / adjustment.upper()
            });
        let pace = model::Pace {
            total,
            height: rows,
            usual,
        };
        let offset = pace.offset(top, band);
        self.frame.set(Some(imp::Frame {
            offset,
            top,
            band,
            pace,
        }));
        snapshot.push_clip(&rect(0.0, 0.0, width, height));
        if total > 0.0 {
            let lines = model.line_at_row(offset).0..model.line_at_row(offset + rows).0 + 1;
            self.highlight(&view, lines.clone());
            let (first, last) = (lines.start / CHUNK, lines.end / CHUNK);
            let mut chunks = self.chunks.borrow_mut();
            for k in first..=last {
                if chunks.len() <= k {
                    chunks.resize(k + 1, None);
                }
                let node = chunks[k].get_or_insert_with(|| render(k, &model, &view, &paint));
                let y = (f64::from(model.start(k * CHUNK)) - offset) * ROW;
                snapshot.save();
                snapshot.translate(&graphene::Point::new(0.0, y as f32));
                snapshot.append_node(&*node);
                snapshot.restore();
            }
            for (k, chunk) in chunks.iter_mut().enumerate() {
                if k + KEEP < first || k > last + KEEP {
                    *chunk = None;
                }
            }
        }
        let alpha = match self.hover.get() || self.grab.get().is_some() {
            true => theme::MAP_BAND_HOVER_ALPHA,
            false => theme::MAP_BAND_ALPHA,
        };
        snapshot.append_color(
            &theme::at(ink, alpha),
            &rect(0.0, (top - offset) * ROW, width, band * ROW),
        );
        snapshot.pop();
    }

    /// Have GtkSourceView colour the `lines` the map shows, from an idle. It colours only what its
    /// view shows, and the map's comments are dimmed by what it colours; never from inside a
    /// frame, since the tags it applies come back here as changes.
    fn highlight(&self, view: &sourceview5::View, lines: Range<usize>) {
        let Some(buffer) = view
            .buffer()
            .downcast::<sourceview5::Buffer>()
            .ok()
            .filter(|buffer| buffer.is_highlight_syntax())
        else {
            return;
        };
        let done = self.highlighted.get();
        if done.is_some_and(|(start, end)| start <= lines.start && lines.end <= end) {
            return;
        }
        self.highlighted.set(Some((lines.start, lines.end)));
        glib::idle_add_local_once(move || {
            let at = |line: usize| buffer.iter_at_line(line as i32);
            if let Some(start) = at(lines.start) {
                buffer.ensure_highlight(&start, &at(lines.end).unwrap_or(buffer.end_iter()));
            }
        });
    }

    /// A press at `y`: on the band it takes hold of it, anywhere else it first puts the row
    /// pressed in the middle of the view, and either way the drag after it moves the band.
    fn press(&self, y: f64) {
        let (Some(frame), Some(view)) = (self.frame.get(), self.view.upgrade()) else {
            return;
        };
        let row = y / ROW + frame.offset;
        let screen = match (frame.top..=frame.top + frame.band).contains(&row) {
            true => frame.top - frame.offset,
            false => {
                let (at, frac) = self.line_at_row(row);
                scroll_line_to(&view, at, frac, 0.5);
                let (top, band) = self.band_now(&view);
                top - frame.pace.offset(top, band)
            }
        };
        self.grab.set(Some(imp::Grab { screen }));
        self.obj().queue_draw();
    }

    /// The pointer `dy` below where it pressed: the band's top that far below where it was on
    /// the map.
    fn drag(&self, dy: f64) {
        let (Some(grab), Some(frame), Some(view)) =
            (self.grab.get(), self.frame.get(), self.view.upgrade())
        else {
            return;
        };
        let last = (frame.pace.total - frame.band).max(0.0);
        let top = ((grab.screen + dy / ROW) * frame.pace.ratio()).clamp(0.0, last);
        let (at, frac) = self.line_at_row(top);
        scroll_line_to(&view, at, frac, 0.0);
    }

    fn release(&self) {
        self.grab.set(None);
        self.obj().queue_draw();
    }

    fn set_hover(&self, hover: bool) {
        self.hover.set(hover);
        self.obj().queue_draw();
    }

    /// The line drawn on `row`, and how far down it.
    fn line_at_row(&self, row: f64) -> (usize, f64) {
        let mut model = self.model.borrow_mut();
        model.sum();
        model.line_at_row(row)
    }

    /// The band's top and height in rows as `view` stands now, between frames.
    fn band_now(&self, view: &sourceview5::View) -> (f64, f64) {
        let (top, bottom) = band(view, &self.model.borrow());
        (top, (bottom - top).max(1.0))
    }

    fn wheel(&self, wheel: &gtk::EventControllerScroll, dy: f64) -> glib::Propagation {
        // With Control the wheel zooms, further up (`zoom::zoom_on_wheel`).
        if wheel
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let Some(adjustment) = self.view.upgrade().and_then(|view| view.vadjustment()) else {
            return glib::Propagation::Proceed;
        };
        // A wheel's click goes as far as GtkScrolledWindow's does; a touchpad moves by pixels.
        let step = match wheel.unit() {
            gdk::ScrollUnit::Wheel => adjustment.page_size().powf(2.0 / 3.0),
            _ => 1.0,
        };
        adjustment.set_value(adjustment.value() + dy * step);
        glib::Propagation::Stop
    }
}

/// Whether `tag` changes what the map draws of a line: it hides it (a fold, a collapsed run),
/// scales it or labels it (a heading), tints its row (a comparison, a merge, a conflict) or tones
/// its words, a highlighter's tag among them.
fn matters(tag: &gtk::TextTag) -> bool {
    if tag.is_invisible_set() || tag.is_scale_set() || tag.is_paragraph_background_set() {
        return true;
    }
    let Some(name) = tag.name() else {
        return tag.is_foreground_set();
    };
    let name = name.as_str();
    name == COMMENT
        || [
            &highlight::HEADING_TAGS[..],
            &highlight::ACCENT_TAGS,
            &highlight::MARKUP_TAGS,
            &highlight::FADED_TAGS,
        ]
        .iter()
        .any(|names| names.contains(&name))
}

fn drop_chunk(chunks: &mut [Option<gsk::RenderNode>], chunk: usize) {
    if let Some(node) = chunks.get_mut(chunk) {
        *node = None;
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> graphene::Rect {
    graphene::Rect::new(x as f32, y as f32, width as f32, height as f32)
}

/// Line `at` as the map sees it. One whose start is hidden is hidden whole: hidden text keeps to
/// whole lines (`fold::whole_lines`).
fn measure(buffer: &gtk::TextBuffer, at: usize) -> Line {
    let Some(start) = buffer.iter_at_line(at as i32) else {
        return Line::default();
    };
    let end = line_end(start);
    let mut line = Line {
        chars: end.offset().saturating_sub(start.offset()) as u32,
        ..Line::default()
    };
    let mut level = 0;
    for tag in start.tags() {
        line.hidden |= tag.is_invisible_set() && tag.is_invisible();
        if tag.is_scale_set() {
            line.scale = line.scale.max(tag.scale() as f32);
        }
        let name = tag.name();
        if let Some(at) = highlight::HEADING_TAGS
            .iter()
            .position(|heading| name.as_deref() == Some(*heading))
        {
            level = at as u8 + 1;
        }
    }
    if level > 0 && model::label(&buffer.text(&start, &end, false)).is_some() {
        line.heading = level;
        line.min_rows = label_rows(level);
    }
    line
}

/// The row tint of the line starting at `start`: the paragraph background of its highest-priority
/// tag that sets one, which is a comparison's, a merge's or a conflict block's.
fn tint(start: &gtk::TextIter) -> Option<gdk::RGBA> {
    start
        .tags()
        .iter()
        .rev()
        .filter(|tag| tag.is_paragraph_background_set())
        .find_map(|tag| tag.paragraph_background_rgba())
}

/// The characters of `text`, the line from `start` to `end`, each with its tone. Tags change only
/// at their toggles, so the tone is looked up once per run between two.
fn toned<'a>(
    text: &'a str,
    start: gtk::TextIter,
    end: gtk::TextIter,
    keywords: &[gtk::TextTag],
) -> impl Iterator<Item = (char, Tone)> + 'a {
    let mut runs = Vec::new();
    let mut at = start;
    while at < end {
        let mut next = at;
        if !next.forward_to_tag_toggle(None::<&gtk::TextTag>) || next > end {
            next = end;
        }
        runs.push((
            (next.offset() - at.offset()) as usize,
            Tone::at(&at, keywords),
        ));
        at = next;
    }
    let tones = runs
        .into_iter()
        .flat_map(|(chars, tone)| std::iter::repeat_n(tone, chars));
    text.chars().zip(tones)
}

fn line_end(start: gtk::TextIter) -> gtk::TextIter {
    let mut end = start;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    end
}

/// The rows of the view's first and last lines on screen, each placed by how far down it the
/// screen's edge falls. `line_at_y` and `line_yrange` read the layout's heights and never walk a
/// line's bytes, which is where GTK's hidden-text aborts are (ISSUES.md).
fn band(view: &sourceview5::View, model: &Model) -> (f64, f64) {
    let shown = view.visible_rect();
    let row = |y: i32| {
        let (iter, top) = view.line_at_y(y);
        let (_, height) = view.line_yrange(&iter);
        let frac = match height {
            h if h > 0 => f64::from(y - top) / f64::from(h),
            _ => 0.0,
        };
        model.row_of(iter.line().max(0) as usize, frac)
    };
    (row(shown.y()), row(shown.y() + shown.height()))
}

/// Scroll `view` to have the point `frac` of the way down line `at` `place` of the way down the
/// screen: at its top for 0, in its middle for 0.5. Nothing while the whole document fits.
fn scroll_line_to(view: &sourceview5::View, at: usize, frac: f64, place: f64) {
    let (Some(adjustment), Some(iter)) =
        (view.vadjustment(), view.buffer().iter_at_line(at as i32))
    else {
        return;
    };
    if adjustment.upper() <= adjustment.page_size() {
        return;
    }
    let (y, height) = view.line_yrange(&iter);
    let target = f64::from(y) + frac * f64::from(height);
    let shown = view.visible_rect();
    let at = f64::from(shown.y()) + place * f64::from(shown.height());
    adjustment.set_value(adjustment.value() + target - at);
}

/// Chunk `k`, drawn from its own first row down into a texture at the screen's scale: each
/// line's row tint, then its label if it is a heading, or else its words.
fn render(k: usize, model: &Model, view: &sourceview5::View, paint: &Paint) -> gsk::RenderNode {
    let snapshot = gtk::Snapshot::new();
    snapshot.scale(paint.scale as f32, paint.scale as f32);
    let buffer = view.buffer();
    let tab = view.tab_width().max(1);
    let width = f64::from(WIDTH);
    let (first, end) = (k * CHUNK, ((k + 1) * CHUNK).min(model.len()));
    let base = f64::from(model.start(first));
    let height = (f64::from(model.start(end)) - base) * ROW;
    for at in first..end {
        let line = model.line(at);
        let rows = line.rows(model.cols());
        let Some(start) = buffer.iter_at_line(at as i32).filter(|_| rows > 0) else {
            continue;
        };
        let stop = line_end(start);
        let text = buffer.text(&start, &stop, true);
        let y = (f64::from(model.start(at)) - base) * ROW;
        if let Some(tint) = tint(&start) {
            snapshot.append_color(&tint, &rect(0.0, y, width, f64::from(rows) * ROW));
        }
        if let Some(label) = model::label(&text).filter(|_| line.heading > 0) {
            let level = line.heading;
            let mut font = paint.label.clone();
            font.set_absolute_size(LABEL_PX[usize::from(level - 1)] * f64::from(pango::SCALE));
            let indent = f64::from(level - 1) * LABEL_INDENT;
            let layout = pango::Layout::new(&paint.context);
            layout.set_font_description(Some(&font));
            layout.set_text(label);
            layout.set_width(((width - indent) * f64::from(pango::SCALE)) as i32);
            layout.set_ellipsize(pango::EllipsizeMode::End);
            let room = f64::from(label_rows(level)) * ROW - f64::from(layout.pixel_size().1);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                indent as f32,
                (y + room / 2.0) as f32,
            ));
            snapshot.append_layout(&layout, &paint.text);
            snapshot.restore();
            continue;
        }
        let scale = f64::from(line.scale);
        let keywords = paint.keywords.get_or_init(|| keyword_looks(&buffer));
        let chars = toned(&text, start, stop, keywords);
        for bar in model::bars(chars, line.per_row(model.cols()), rows, tab) {
            let colour = match bar.tone {
                Tone::Ink => &paint.ink,
                Tone::Dim => &paint.dim,
                Tone::Accent => &paint.accent,
            };
            snapshot.append_color(
                colour,
                &rect(
                    f64::from(bar.col) * scale,
                    y + f64::from(bar.row) * ROW,
                    f64::from(bar.len) * scale,
                    ROW,
                ),
            );
        }
    }
    match snapshot.to_node().filter(|_| height > 0.0) {
        Some(node) => {
            let pixels = rect(0.0, 0.0, width * paint.scale, height * paint.scale);
            let texture = paint.renderer.render_texture(&node, Some(&pixels));
            gsk::TextureNode::new(&texture, &rect(0.0, 0.0, width, height)).upcast()
        }
        None => gsk::ContainerNode::new(&[]).upcast(),
    }
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// What the last frame drew, in rows: what a press and a drag are measured against.
    #[derive(Clone, Copy)]
    pub struct Frame {
        /// How far the map has scrolled itself.
        pub offset: f64,
        /// The band's top and height.
        pub top: f64,
        pub band: f64,
        pub pace: model::Pace,
    }

    /// A hold on the band: the row of the map its top was drawn on as the press began.
    #[derive(Clone, Copy)]
    pub struct Grab {
        pub screen: f64,
    }

    #[derive(Default)]
    pub struct Minimap {
        pub view: glib::WeakRef<sourceview5::View>,
        pub model: RefCell<Model>,
        /// A texture per [`CHUNK`] lines, `None` until it is drawn again.
        pub chunks: RefCell<Vec<Option<gsk::RenderNode>>>,
        /// The foreground, the accent and the screen scale the chunks were drawn at.
        pub drawn: Cell<Option<(gdk::RGBA, gdk::RGBA, f64)>>,
        /// The lines last handed to GtkSourceView to colour, until the next edit.
        pub highlighted: Cell<Option<(usize, usize)>>,
        /// The buffer changed while the map was off screen: read it whole again.
        pub stale: Cell<bool>,
        /// The line the edit in progress starts on, from `insert-text` or `delete-range` to
        /// `changed`.
        pub edit: Cell<Option<usize>>,
        pub adjustment: RefCell<Option<(gtk::Adjustment, Vec<glib::SignalHandlerId>)>>,
        /// The view's font's average character width, 0 until measured.
        pub char_width: Cell<f64>,
        pub frame: Cell<Option<Frame>>,
        pub hover: Cell<bool>,
        pub grab: Cell<Option<Grab>>,
        /// What drawing has cost, for the drills.
        #[cfg(feature = "bench")]
        pub spent: Cell<std::time::Duration>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Minimap {
        const NAME: &'static str = "AccentMinimap";
        type Type = super::Minimap;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("minimap");
        }
    }

    impl ObjectImpl for Minimap {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().install_input();
        }

        fn dispose(&self) {
            self.follow(None);
        }
    }

    impl WidgetImpl for Minimap {
        /// A fixed width, and no height of its own: it is as tall as the view beside it.
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (WIDTH, WIDTH, -1, -1),
                _ => (0, 0, -1, -1),
            }
        }

        /// Off screen nothing is drawn, so nothing drawn is kept.
        fn unmap(&self) {
            self.parent_unmap();
            self.chunks.borrow_mut().clear();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            #[cfg(feature = "bench")]
            let start = std::time::Instant::now();
            self.draw(snapshot);
            #[cfg(feature = "bench")]
            self.spent.set(self.spent.get() + start.elapsed());
        }
    }
}
