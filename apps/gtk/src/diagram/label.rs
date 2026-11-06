//! Edit text in an editor overlaid on a canvas: a diagram's labels as Markdown in the note
//! editor over the cell, and text typed onto a PDF page as plain text where the page will set it.
//!
//! `accent_drawio::label` converts HTML to Markdown and back, sharing note styling,
//! spellcheck and font. Editing drops per-run colours and sizes; the cell's font
//! settings from Properties then apply.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_core::note::Syntax;
use accent_drawio::{Align, CellId, Rect, VAlign};
use adw::prelude::*;
use gtk::{gdk, glib, graphene, pango};

use super::DiagramTab;
use crate::editor::{self, Flavour};

/// The narrowest the editor is, in pixels: a label on a hairline edge still needs room for a
/// word.
const MIN_WIDTH: f64 = 160.0;

/// draw.io lays a label's lines out 1.2 font sizes apart (`mxConstants.LINE_HEIGHT`): the editor
/// is never shorter than one.
const LINE_HEIGHT: f64 = 1.2;

/// What typed text is shown in: the face a PDF's Helvetica is, which fontconfig finds a match for
/// (Nimbus Sans, Liberation Sans) where there is no Helvetica.
const FACE: &str = "Helvetica";

/// What the editor shows its text as.
pub enum Look {
    /// A diagram's label: Markdown styled as a note in the note font (`None` for the default),
    /// zoomed to the label's size, spellchecked if asked, at least a word wide and a line tall,
    /// its lines aligned in the box as the label's are on the canvas.
    Note {
        font: Option<String>,
        spellcheck: bool,
        align: Align,
        valign: VAlign,
    },
    /// Text typed onto a PDF page: plain, refusing what Helvetica cannot set, in a face like it
    /// at the text's size on screen, `ink` on `paper`, its lines and the room round them as the
    /// page sets them (`accent_core::pdf::LEADING`, `PADDING`). The box grows with the text.
    Plain { ink: [u8; 3], paper: [u8; 3] },
}

/// The editor of one piece of text, `target` naming what it edits: a diagram's cell, or a PDF's
/// typed text.
pub struct LabelEditor<T = CellId> {
    pub target: T,
    /// Label position and font size in document units. Reposition the editor on
    /// scroll and zoom so editing continues as the canvas moves.
    pub place: Rect,
    pub font_size: f64,
    look: Look,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    /// What the layer holds: a label's view in a scroller, typed text's view itself.
    frame: gtk::Widget,
    /// `GtkFixed` overlay supports negative offsets, letting offscreen labels clip
    /// instead of pinning to the viewport edge.
    layer: gtk::Fixed,
    /// The provider carrying the font at the size last placed at.
    font: RefCell<Option<gtk::CssProvider>>,
    px: Cell<f64>,
    original: String,
    /// The toplevel and the press handler on it, taken off again when the editor closes.
    press: RefCell<Option<(gtk::Widget, gtk::GestureClick)>>,
}

/// The zoom that makes the note font, `doc_pt` points, show a label at `px` pixels on screen.
/// Kept to a range a label can still be read and typed in at.
pub fn label_zoom(px: f64, doc_pt: f64) -> f64 {
    let doc_px = doc_pt * 96.0 / 72.0;
    (px / doc_px.max(1.0)).clamp(0.6, 3.0)
}

/// Editor bounds around screen position `at`, expanded to a word's width and
/// line's height for text size `px`. Allow offscreen corners so the overlay clips
/// the editor rather than pinning it to the edge.
fn box_at(at: Rect, px: f64) -> (f64, f64, f64, f64) {
    let (w, h) = (at.w.max(MIN_WIDTH), at.h.max(px * LINE_HEIGHT));
    (at.x - (w - at.w) / 2.0, at.y - (h - at.h) / 2.0, w, h)
}

/// Editor bounds for a label inside its shape's spacing. Account for draw.io's
/// 5-unit top and 1-unit bottom baseline shifts (`mxText.baseSpacingTop`/`Bottom`)
/// to avoid extending the editor beyond the shape.
pub fn label_box(rect: Rect, valign: VAlign, inside: bool) -> Rect {
    use accent_drawio::scene::{BASE_SPACING_BOTTOM, BASE_SPACING_TOP};
    let dy = match (inside, valign) {
        (true, VAlign::Top) => -BASE_SPACING_TOP,
        (true, VAlign::Bottom) => BASE_SPACING_BOTTOM,
        _ => 0.0,
    };
    rect.translate(0.0, dy)
}

impl<T: 'static> LabelEditor<T> {
    /// The editor for the text at `place` on the page (document units, its font `font_size`
    /// points there), in `overlay`, holding `text` as `look` shows it: a label all selected, to
    /// type over, typed text with the caret at its end. [`place_at`](Self::place_at) puts it on
    /// screen and [`focus`](Self::focus) gives it the keyboard.
    pub fn open(
        overlay: &gtk::Overlay,
        target: T,
        text: &str,
        place: Rect,
        font_size: f64,
        look: Look,
    ) -> Rc<LabelEditor<T>> {
        let note = matches!(look, Look::Note { .. });
        let (view, buffer) = match note {
            true => editor::overlay_view(text),
            false => plain_view(text),
        };
        // Lines as close as draw.io sets them, so a label that fits its shape fits the editor;
        // typed text's padding is its size's (`set_font`).
        view.set_left_margin(2);
        view.set_right_margin(2);
        view.set_top_margin(0);
        view.set_bottom_margin(0);
        view.set_pixels_above_lines(0);
        view.set_pixels_below_lines(0);
        view.set_widget_name(&editor::next_view_name());
        // No scrollbar: one would make the box as tall as its trough, past a short label. Text
        // longer than the box still scrolls to the caret; typed text grows the box instead, and
        // is never scrolled.
        let frame: gtk::Widget = match note {
            true => {
                let scroller = gtk::ScrolledWindow::builder()
                    .child(&view)
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .vscrollbar_policy(gtk::PolicyType::External)
                    .css_classes(["accent-label-editor", "view"])
                    .build();
                crate::widgets::smooth_wheel(&scroller);
                scroller.upcast()
            }
            false => {
                view.add_css_class("accent-label-editor");
                view.clone().upcast()
            }
        };
        // Not clipped to the box, which would cut the outline off its right and bottom.
        let layer = gtk::Fixed::builder()
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .overflow(gtk::Overflow::Visible)
            .build();
        layer.put(&frame, 0.0, 0.0);
        overlay.add_overlay(&layer);
        // Placed over the page rather than measured with it: an editor near the far edge would
        // widen the overlay and with it the canvas under the label being edited.
        overlay.set_measure_overlay(&layer, false);
        // Clipped for the pointer as well as for sight: a press where the editor overhangs the
        // sidebar, the header or the banner goes to them (checked with `Window::pick`).
        overlay.set_clip_overlay(&layer, true);
        let editor = Rc::new(LabelEditor {
            target,
            place,
            font_size,
            look,
            view,
            buffer,
            frame,
            layer,
            font: RefCell::new(None),
            px: Cell::new(0.0),
            original: text.to_string(),
            press: RefCell::new(None),
        });
        if !note {
            editor.buffer.place_cursor(&editor.buffer.end_iter());
            // Weak: the buffer is the editor's.
            let weak = Rc::downgrade(&editor);
            editor.buffer.connect_changed(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.fit();
                }
            });
            return editor;
        }
        if let Look::Note {
            spellcheck,
            align,
            valign,
            ..
        } = editor.look
        {
            if spellcheck {
                editor::spell_adapter(&editor.buffer, &editor.view).set_enabled(true);
            }
            align_lines(&editor.view, align, valign);
        }
        // Styled as the note editor's companions are, on each change and in the theme's colours.
        // Weak: the view holds the buffer, which holds this closure.
        let weak = editor.view.downgrade();
        editor.buffer.connect_changed(move |buffer| {
            if let Some(view) = weak.upgrade() {
                editor::style_companion(Flavour::Note(Syntax::Markdown), buffer, &view);
            }
        });
        let (view, buffer) = (editor.view.clone(), editor.buffer.clone());
        editor.view.connect_realize(move |_| {
            editor::restyle_companion(Flavour::Note(Syntax::Markdown), &buffer, &view);
        });
        editor
            .buffer
            .select_range(&editor.buffer.start_iter(), &editor.buffer.end_iter());
        editor
    }

    /// Give the editor the keyboard, once whatever finishes it on leaving is listening.
    pub fn focus(&self) {
        self.view.grab_focus();
    }

    /// Position the editor at widget `at` with text size `px`, on open and on scroll
    /// or zoom. Preserve focus and typed text while repositioning. Typed text is put at its
    /// corner, never smaller than `at`, so none of the text it is typing again shows round it.
    pub fn place_at(&self, at: Rect, px: f64) {
        let plain = matches!(self.look, Look::Plain { .. });
        let (x, y, w, h) = match plain {
            false => box_at(at, px),
            true => (at.x, at.y, at.w, at.h),
        };
        self.frame.set_size_request(w as i32, h as i32);
        self.layer.move_(&self.frame, x, y);
        // The font only when the zoom moved: it is a provider on the whole display, far too much
        // to install again at every step of a scroll.
        if self.px.replace(px) != px {
            self.set_font(px);
        }
        if plain {
            self.fit();
        }
    }

    /// Typed text's box: its widest line across, as it is shown, and never smaller than the text
    /// it types again (`place`), so none of that shows round it. A text view asks for no more
    /// width than its margins, and the `GtkFixed` it is in gives it no more than it asks for.
    fn fit(&self) {
        let scale = self.px.get() / self.font_size;
        let text = self
            .buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false);
        let (wide, _) = self.view.create_pango_layout(Some(&text)).pixel_size();
        // A pixel more, for the caret at the end of the widest line.
        let margins = self.view.left_margin() + self.view.right_margin() + 1;
        let w = f64::from(wide + margins).max(self.place.w * scale);
        let h = self.place.h * scale;
        self.frame
            .set_size_request(w.ceil() as i32, h.ceil() as i32);
    }

    /// Where the editor is on the canvas, its size, how tall its text is laid out and how big
    /// that text is: what the drills watch while the page moves under it.
    #[cfg(feature = "bench")]
    pub fn at(&self) -> (Rect, f64, f64) {
        let (x, y) = self.layer.child_position(&self.frame);
        let (w, h) = (self.frame.width(), self.frame.height());
        let at = Rect::new(x, y, f64::from(w), f64::from(h));
        let content = self.view.measure(gtk::Orientation::Vertical, w).1;
        (at, f64::from(content), self.px.get())
    }

    /// The note font zoomed to `px` on screen, or typed text's face at `px`.
    fn set_font(&self, px: f64) {
        let (ink, paper) = match &self.look {
            Look::Note { font, .. } => {
                let family = font.clone().unwrap_or_else(editor::default_font);
                let doc_pt = f64::from(pango::FontDescription::from_string(&family).size())
                    / f64::from(pango::SCALE);
                editor::install_font(
                    &self.font,
                    Flavour::Note(Syntax::Markdown),
                    font.as_deref(),
                    label_zoom(px, doc_pt),
                    &self.view.widget_name(),
                );
                editor::refont(&self.view);
                return;
            }
            Look::Plain { ink, paper } => (*ink, *paper),
        };
        use accent_core::pdf::{BASELINE, LEADING, PADDING};
        let (pad, leading) = (f64::from(PADDING) * px, f64::from(LEADING) * px);
        // Each line's baseline where the page sets it, BASELINE sizes below the top of its line.
        // A face is laid out by its own ascent and descent, half the leading left over above
        // them (`line-height`), so the text goes down by what that leaves short, its box as tall.
        let mut face = pango::FontDescription::from_string(FACE);
        face.set_absolute_size(px * f64::from(pango::SCALE));
        let metrics = self
            .view
            .pango_context()
            .load_font(&face)
            .map(|f| f.metrics(None));
        let laid = metrics.map_or(f64::from(BASELINE) * px, |m| {
            let (ascent, descent) = (f64::from(m.ascent()), f64::from(m.descent()));
            let scale = f64::from(pango::SCALE);
            (leading - (ascent + descent) / scale) / 2.0 + ascent / scale
        });
        let down = (f64::from(BASELINE) * px - laid).clamp(-pad, pad);
        let margin = |by: f64| by.round() as i32;
        self.view.set_left_margin(margin(pad));
        self.view.set_right_margin(margin(pad));
        self.view.set_top_margin(margin(pad + down));
        self.view.set_bottom_margin(margin(pad - down));
        let Some(display) = gdk::Display::default() else {
            return;
        };
        if let Some(old) = self.font.take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        let rgb = |[r, g, b]: [u8; 3]| format!("rgb({r}, {g}, {b})");
        let name = self.view.widget_name();
        // The colour is the view's own node's, the background its text node's.
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&format!(
            "#{name} {{ font-family: {FACE}; font-size: {px}px; line-height: {leading}px; \
             color: {}; }} #{name} > text {{ background-color: {}; }}",
            rgb(ink),
            rgb(paper)
        ));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        self.font.replace(Some(provider));
        editor::refont(&self.view);
    }

    /// The text typed, or `None` when it is what the editor opened with; typed text's without
    /// the space and lines it ends in, which a page has no use for.
    pub fn changed(&self) -> Option<String> {
        let text = self
            .buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false)
            .to_string();
        let text = match self.look {
            Look::Note { .. } => text,
            Look::Plain { .. } => text.trim_end().to_string(),
        };
        (text != self.original).then_some(text)
    }

    /// Put `text` in place of the selection, as a key typed into the editor would.
    pub fn type_text(&self, text: &str) {
        self.buffer.delete_selection(true, true);
        self.buffer.insert_at_cursor(text);
    }

    pub fn has_focus(&self) -> bool {
        self.view.has_focus()
    }

    pub fn view(&self) -> &sourceview5::View {
        &self.view
    }

    /// Leave the editor showing what was typed, no longer typing: no caret, no keys, no clicks,
    /// until it [closes](Self::close).
    pub fn freeze(&self) {
        if let Some((root, press)) = self.press.borrow_mut().take() {
            root.remove_controller(&press);
        }
        self.view.set_editable(false);
        self.view.set_cursor_visible(false);
        self.view.set_can_target(false);
        self.view.set_can_focus(false);
    }

    /// Take the editor off the canvas and drop its font and its window-wide press handler.
    pub fn close(&self, overlay: &gtk::Overlay) {
        if let Some((root, press)) = self.press.borrow_mut().take() {
            root.remove_controller(&press);
        }
        overlay.remove_overlay(&self.layer);
        if let (Some(display), Some(provider)) = (gdk::Display::default(), self.font.take()) {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
    }
}

/// Set `view`'s lines in its box as `align` and `valign` set a label's: across by justification,
/// and down by a top margin of the room its lines leave, kept as they grow. Measured an idle after
/// the view's lines change height, which is during its allocation.
fn align_lines(view: &sourceview5::View, align: Align, valign: VAlign) {
    view.set_justification(match align {
        Align::Left => gtk::Justification::Left,
        Align::Center => gtk::Justification::Center,
        Align::Right => gtk::Justification::Right,
    });
    let share = match valign {
        VAlign::Top => return,
        VAlign::Middle => 0.5,
        VAlign::Bottom => 1.0,
    };
    let Some(adjustment) = view.vadjustment() else {
        return;
    };
    let view = view.downgrade();
    adjustment.connect_changed(move |adjustment| {
        let (view, room) = (view.clone(), adjustment.page_size());
        glib::idle_add_local_once(move || {
            let Some(view) = view.upgrade() else { return };
            // The foot of the last line, in the buffer's coordinates, which the margin is outside.
            let (y, h) = view.line_yrange(&view.buffer().end_iter());
            let top = ((room - f64::from(y + h)) * share).max(0.0).round() as i32;
            if view.top_margin() != top {
                view.set_top_margin(top);
            }
        });
    });
}

/// A plain view of `text` that takes only what Helvetica can set: a paste of anything else keeps
/// the rest of it.
fn plain_view(text: &str) -> (sourceview5::View, sourceview5::Buffer) {
    use accent_core::pdf::typeable;
    use sourceview5::prelude::BufferExt;
    let buffer = sourceview5::Buffer::new(None);
    // No scheme, whose colours would be the text's and the box's.
    buffer.set_style_scheme(None);
    buffer.set_highlight_matching_brackets(false);
    buffer.set_text(text);
    buffer.connect_insert_text(|buffer, at, text| {
        if text.chars().all(typeable) {
            return;
        }
        buffer.stop_signal_emission_by_name("insert-text");
        let kept: String = text.chars().filter(|&c| typeable(c)).collect();
        if !kept.is_empty() {
            buffer.insert(at, &kept);
        }
    });
    (sourceview5::View::with_buffer(&buffer), buffer)
}

/// Finish the editor as the reader leaves it: at once on Escape, and through `later`, which
/// finishes from an idle and only while this editor is still the one open, on a click anywhere
/// outside it or the keyboard going elsewhere. Then it takes the keyboard.
///
/// From an idle rather than the handler: leaving is GTK moving the focus, and taking the editor
/// off the canvas meanwhile left GTK walking up from a widget that was gone, over and over (the
/// freeze).
pub fn wire_finish<T: 'static>(
    editor: &Rc<LabelEditor<T>>,
    now: impl Fn() + 'static,
    later: impl Fn() + Clone + 'static,
) {
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave({
        let later = later.clone();
        move |_| later()
    });
    editor.view().add_controller(focus);
    wire_keys(editor, now);
    // Space that takes no keyboard moves no focus, so the press on it is heard on the window
    // instead. Wired now that the editor is in place, and taken off again when it closes.
    wire_press(editor, later);
    // Only now: a focus controller hears the focus leave only if it saw it come in, so a
    // click outside finishes the editor only when the grab comes after the wiring.
    editor.focus();
}

/// The editor's own keys: Escape finishes, as leaving it does.
fn wire_keys<T: 'static>(editor: &Rc<LabelEditor<T>>, finish: impl Fn() + 'static) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, key, _, _| match key {
        gdk::Key::Escape => {
            finish();
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    });
    editor.view.add_controller(keys);
}

/// Finish editing on clicks in non-focusable chrome, which emits no focus change.
/// Capture the press anywhere in the window without claiming it so its action continues.
fn wire_press<T: 'static>(editor: &Rc<LabelEditor<T>>, finish: impl Fn() + 'static) {
    let Some(root) = editor.frame.root() else {
        return;
    };
    let root: gtk::Widget = root.upcast();
    let press = gtk::GestureClick::new();
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (frame, window) = (editor.frame.clone(), root.clone());
    press.connect_pressed(move |gesture, _, x, y| {
        // A popover's presses reach the window too (the spelling menu's), on a surface of their
        // own: correcting a word is not a click outside the label.
        let on = gesture.current_event().and_then(|e| e.surface());
        if on != window.native().and_then(|native| native.surface()) {
            return;
        }
        // Nor is a press in the editor itself, which is the reader putting the caret somewhere.
        let point = graphene::Point::new(x as f32, y as f32);
        if frame
            .compute_bounds(&window)
            .is_some_and(|b| b.contains_point(&point))
        {
            return;
        }
        finish();
    });
    root.add_controller(press.clone());
    *editor.press.borrow_mut() = Some((root, press));
}

impl DiagramTab {
    /// Edit the label of the one selected cell, in the note editor over it.
    pub fn edit_label(self: &Rc<Self>) {
        let selection = self.selection();
        let [id] = selection.as_slice() else {
            return;
        };
        let id = id.clone();
        self.finish_label();
        let Some(sheet) = self.view.sheet() else {
            return;
        };
        let (markdown, inside, align) = {
            let editor = self.editor.borrow();
            let Ok(page) = editor.page(self.page_index.get()) else {
                return;
            };
            let Some(cell) = page.cell(&id) else { return };
            // A label inside its shape, rather than beside it or on an edge.
            let style = cell.style.resolve(cell.edge);
            let at = |key, middle| style.get(key).is_none_or(|v| v == middle);
            let align = match style.get("align") {
                Some("left") => Align::Left,
                Some("right") => Align::Right,
                _ => Align::Center,
            };
            (
                accent_drawio::label::to_markdown(cell.label(), cell.is_html()),
                cell.vertex
                    && at("labelPosition", "center")
                    && at("verticalLabelPosition", "middle"),
                align,
            )
        };
        // Where the label is drawn, or where it will be for a cell that has none yet: halfway
        // along an edge, over the whole of a shape.
        let (mut place, size, valign) = sheet
            .scene
            .prims
            .iter()
            .find_map(|p| match p {
                accent_drawio::Prim::Text {
                    cell,
                    rect,
                    font,
                    valign,
                    ..
                } if *cell == id => Some((label_box(*rect, *valign, inside), font.size, *valign)),
                _ => None,
            })
            .or_else(|| {
                let m = sheet.edge_middle(&id)?;
                Some((Rect::new(m.x, m.y, 0.0, 0.0), 11.0, VAlign::Middle))
            })
            .unwrap_or_else(|| {
                let frame = sheet.frame_of(&id).unwrap_or_default();
                (frame, 12.0, VAlign::Middle)
            });
        if let Some(r) = sheet.rect(&id).filter(|_| place.w < 1.0 || place.h < 1.0) {
            place = r;
        }
        self.view.reveal(&place);
        let look = Look::Note {
            font: self.font.borrow().clone(),
            spellcheck: self.spellcheck.get(),
            align,
            valign,
        };
        let editor = LabelEditor::open(&self.overlay, id, &markdown, place, size, look);
        *self.label.borrow_mut() = Some(editor.clone());
        self.place_label();
        // Only the editor that left: another may have opened by the time the idle runs.
        let later = {
            let (tab, left) = (Rc::downgrade(self), Rc::downgrade(&editor));
            move || {
                let (tab, left) = (tab.clone(), left.clone());
                glib::idle_add_local_once(move || {
                    let Some(tab) = tab.upgrade() else { return };
                    let open = tab.label.borrow().as_ref().map(Rc::downgrade);
                    if open.is_some_and(|open| open.ptr_eq(&left)) {
                        tab.finish_label();
                    }
                });
            }
        };
        let now = glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.finish_label()
        );
        wire_finish(&editor, now, later);
    }

    /// Put the label editor back over its cell: where it opens, and again whenever the canvas
    /// scrolls or zooms under it. Nothing when no label is being edited.
    pub(super) fn place_label(&self) {
        let Some(editor) = self.label.borrow().clone() else {
            return;
        };
        editor.place_at(
            self.view.to_widget(&editor.place),
            editor.font_size * self.view.scale(),
        );
    }

    /// Commit the label and close its editor. Reentrant-safe: removing the widget
    /// changes focus and can call this again.
    pub fn finish_label(self: &Rc<Self>) {
        let Some(editor) = self.label.borrow_mut().take() else {
            return;
        };
        let typed = editor.changed();
        editor.close(&self.overlay);
        if let Some(markdown) = typed {
            let id = editor.target.clone();
            self.edit(|e, page| e.set_label_markdown(page, &id, &markdown));
        }
        // Not when another label opened meanwhile: a double click on a second label finishes
        // the first and opens the second in one turn, and taking the focus back would finish
        // that one too.
        let tab = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(tab) = tab.upgrade().filter(|t| t.label.borrow().is_none()) {
                tab.view.grab_focus();
            }
        });
    }

    /// Typing over the one selected shape: its label is edited with `typed` in place of what it
    /// said, as draw.io does. `false` when there is nothing to type into.
    pub(super) fn type_into_label(self: &Rc<Self>, typed: Option<char>) -> bool {
        let Some(c) = typed.filter(|c| !c.is_control()) else {
            return false;
        };
        let one = match self.selection.borrow().as_slice() {
            [id] => self.view.sheet().is_some_and(|s| !s.is_pinned(id)),
            _ => false,
        };
        if !one {
            return false;
        }
        self.edit_label();
        let Some(editor) = self.label.borrow().clone() else {
            return false;
        };
        editor.type_text(&c.to_string());
        true
    }

    /// The cell whose label is being edited, if one is.
    #[cfg(feature = "bench")]
    pub fn editing_label(&self) -> Option<CellId> {
        self.label.borrow().as_ref().map(|e| e.target.clone())
    }

    /// Where the label editor sits on the canvas, how tall its text is laid out, how big that
    /// text is and whether it holds the keyboard, for the drills that move the page under it.
    #[cfg(feature = "bench")]
    pub fn label_at(&self) -> Option<(accent_drawio::Rect, f64, f64, bool)> {
        let editor = self.label.borrow().clone()?;
        let (at, content, px) = editor.at();
        Some((at, content, px, editor.has_focus()))
    }

    /// `Ctrl+Return` in the label editor, which the window's accelerator took first: finish the
    /// label. `false` when no label is being edited here.
    pub fn commit_label(self: &Rc<Self>) -> bool {
        let editing = self.label.borrow().as_ref().is_some_and(|e| e.has_focus());
        if editing {
            self.finish_label();
        }
        editing
    }

    /// A cell's label as the label editor would show it.
    #[cfg(feature = "bench")]
    pub fn label_markdown(&self, id: &str) -> Option<String> {
        let editor = self.editor.borrow();
        let cell = editor.page(self.page_index.get()).ok()?.cell(id)?;
        Some(accent_drawio::label::to_markdown(
            cell.label(),
            cell.is_html(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use accent_drawio::Rect;

    #[test]
    fn a_label_scrolled_off_the_edge_keeps_its_box() {
        // A 40x20 label of 20 px text scrolled just past the canvas's left edge: a box a word
        // wide and a line tall, centred on the label, and left at the negative corner the
        // overlay clips.
        let (x, y, w, h) = super::box_at(Rect::new(-30.0, 100.0, 40.0, 20.0), 20.0);
        assert_eq!((w, h), (super::MIN_WIDTH, 24.0));
        assert_eq!((x, y), (-30.0 - (super::MIN_WIDTH - 40.0) / 2.0, 98.0));
    }

    #[test]
    fn a_top_aligned_label_is_edited_inside_its_shape() {
        // A `text;` cell at (100, 300, 80, 30), painted 5 below its spacing.
        let painted = Rect::new(102.0, 307.0, 76.0, 26.0);
        let top = accent_drawio::VAlign::Top;
        let inside = Rect::new(102.0, 302.0, 76.0, 26.0);
        assert_eq!(super::label_box(painted, top, true), inside);
        assert_eq!(super::label_box(painted, top, false), painted);
    }

    #[test]
    fn the_editor_shows_a_label_at_its_size_on_screen() {
        // A label 24 px on screen, over an 11 pt note font of 14.67 px.
        let zoom = super::label_zoom(24.0, 11.0);
        assert!((zoom - 24.0 / (11.0 * 96.0 / 72.0)).abs() < 1e-9);
        assert_eq!(super::label_zoom(0.1, 11.0), 0.6);
        assert_eq!(super::label_zoom(800.0, 11.0), 3.0);
    }
}
