//! Editing a label: the note editor, floated over the cell, holding the label as Markdown.
//!
//! A draw.io label is an HTML fragment; the crate turns it into Markdown and back
//! (`accent_drawio::label`), so a label is written the way a note is, with the same styling,
//! spellcheck and font. Per-run colours and sizes a label had are given up once it is edited:
//! the cell's own font colour and size then apply, which is what the Properties pane sets.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_drawio::{CellId, Rect, VAlign};
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

pub struct LabelEditor {
    pub cell: CellId,
    /// Where the label sits on the page and how big its font is there, both in document units.
    /// The editor is put back over them whenever the canvas moves under it, so a scroll or a
    /// zoom carries the edit along instead of finishing it.
    pub place: Rect,
    pub font_size: f64,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    frame: gtk::ScrolledWindow,
    /// The layer the editor is placed in: a `GtkFixed` takes the negative offsets a margin
    /// cannot, so a label scrolled off the top or the left is clipped by the overlay rather than
    /// pinned to the edge.
    layer: gtk::Fixed,
    /// The note font preference, and the provider carrying it at the size last placed at.
    family: Option<String>,
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

/// The box the editor is given over a label `at` on screen, its text `px` tall: the label's own,
/// grown evenly round it to a word's width and a line's height. The corner may be off the
/// canvas, where the overlay clips it — a label scrolled past the edge is gone, not pinned to it.
fn box_at(at: Rect, px: f64) -> (f64, f64, f64, f64) {
    let (w, h) = (at.w.max(MIN_WIDTH), at.h.max(px * LINE_HEIGHT));
    (at.x - (w - at.w) / 2.0, at.y - (h - at.h) / 2.0, w, h)
}

/// Where the editor goes over a label laid out at `rect`, aligned `valign`: a label inside its
/// shape is edited in the box within the shape's spacing. draw.io lowers a top-aligned label 5
/// units past it and lifts a bottom-aligned one 1 (`mxText.baseSpacingTop`/`Bottom`), which
/// would hang the editor over the shape's bottom edge.
pub fn label_box(rect: Rect, valign: VAlign, inside: bool) -> Rect {
    use accent_drawio::scene::{BASE_SPACING_BOTTOM, BASE_SPACING_TOP};
    let dy = match (inside, valign) {
        (true, VAlign::Top) => -BASE_SPACING_TOP,
        (true, VAlign::Bottom) => BASE_SPACING_BOTTOM,
        _ => 0.0,
    };
    rect.translate(0.0, dy)
}

impl LabelEditor {
    /// The editor for the label at `place` on the page (document units, its font `font_size`
    /// points there), in `overlay`, holding `markdown`, all of it selected.
    /// [`place_at`](Self::place_at) puts it on screen and [`focus`](Self::focus) gives it the
    /// keyboard.
    pub fn open(
        overlay: &gtk::Overlay,
        cell: CellId,
        markdown: &str,
        place: Rect,
        font_size: f64,
        font: Option<&str>,
        spellcheck: bool,
    ) -> Rc<LabelEditor> {
        let (view, buffer) = editor::overlay_view(markdown);
        // Lines as close as draw.io sets them, so a label that fits its shape fits the editor.
        view.set_left_margin(2);
        view.set_right_margin(2);
        view.set_top_margin(0);
        view.set_bottom_margin(0);
        view.set_pixels_above_lines(0);
        view.set_pixels_below_lines(0);
        view.set_widget_name(&editor::next_view_name());
        // No scrollbar: one would make the box as tall as its trough, past a short label. Text
        // longer than the box still scrolls to the caret.
        let frame = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .css_classes(["accent-label-editor", "view"])
            .build();
        let layer = gtk::Fixed::builder()
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
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
            cell,
            place,
            font_size,
            view,
            buffer,
            frame,
            layer,
            family: font.map(str::to_string),
            font: RefCell::new(None),
            px: Cell::new(0.0),
            original: markdown.to_string(),
            press: RefCell::new(None),
        });
        if spellcheck {
            editor::spell_adapter(&editor.buffer, &editor.view).set_enabled(true);
        }
        // Styled as the note editor's companions are, on each change and in the theme's colours.
        // Weak: the view holds the buffer, which holds this closure.
        let weak = editor.view.downgrade();
        editor.buffer.connect_changed(move |buffer| {
            if let Some(view) = weak.upgrade() {
                editor::style_companion(Flavour::Note, buffer, &view);
            }
        });
        let (view, buffer) = (editor.view.clone(), editor.buffer.clone());
        editor.view.connect_realize(move |_| {
            editor::restyle_companion(Flavour::Note, &buffer, &view);
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

    /// Put the editor over `at` (widget coordinates) with its text `px` tall: where it opens,
    /// and where it goes again on every scroll and zoom of the canvas. Nothing here touches the
    /// focus, so the keyboard and what was typed stay where they are.
    pub fn place_at(&self, at: Rect, px: f64) {
        let (x, y, w, h) = box_at(at, px);
        self.frame.set_size_request(w as i32, h as i32);
        self.layer.move_(&self.frame, x, y);
        // The font only when the zoom moved: it is a provider on the whole display, far too much
        // to install again at every step of a scroll.
        if self.px.replace(px) != px {
            self.set_font(px);
        }
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

    /// The note font, zoomed to `px` on screen.
    fn set_font(&self, px: f64) {
        let family = self.family.clone().unwrap_or_else(editor::default_font);
        let doc_pt = f64::from(pango::FontDescription::from_string(&family).size())
            / f64::from(pango::SCALE);
        editor::install_font(
            &self.font,
            Flavour::Note,
            self.family.as_deref(),
            label_zoom(px, doc_pt),
            &self.view.widget_name(),
        );
    }

    /// The Markdown typed, or `None` when it is what the editor opened with.
    pub fn changed(&self) -> Option<String> {
        let text = self
            .buffer
            .text(&self.buffer.start_iter(), &self.buffer.end_iter(), false)
            .to_string();
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

/// The editor's own keys: Escape finishes, as leaving it does.
pub fn wire_keys(editor: &Rc<LabelEditor>, finish: impl Fn() + 'static) {
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

/// Finish the label on a press over space that takes no keyboard — the header bar's background,
/// the status bar — which moves no focus and so tells the editor nothing. In the capture phase,
/// so it hears the press wherever it lands, and claiming no sequence, so the click still does
/// whatever it was for.
pub fn wire_press(editor: &Rc<LabelEditor>, finish: impl Fn() + 'static) {
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
        let (markdown, inside) = {
            let editor = self.editor.borrow();
            let Ok(page) = editor.page(self.page_index.get()) else {
                return;
            };
            let Some(cell) = page.cell(&id) else { return };
            // A label inside its shape, rather than beside it or on an edge.
            let style = cell.style.resolve(cell.edge);
            let at = |key, middle| style.get(key).is_none_or(|v| v == middle);
            (
                accent_drawio::label::to_markdown(cell.label(), cell.is_html()),
                cell.vertex
                    && at("labelPosition", "center")
                    && at("verticalLabelPosition", "middle"),
            )
        };
        // Where the label is drawn, or where it will be for a cell that has none yet: halfway
        // along an edge, over the whole of a shape.
        let (mut place, size) = sheet
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
                } if *cell == id => Some((label_box(*rect, *valign, inside), font.size)),
                _ => None,
            })
            .or_else(|| {
                let m = sheet.edge_middle(&id)?;
                Some((accent_drawio::Rect::new(m.x, m.y, 0.0, 0.0), 11.0))
            })
            .unwrap_or_else(|| (sheet.frame_of(&id).unwrap_or_default(), 12.0));
        if let Some(r) = sheet.rect(&id).filter(|_| place.w < 1.0 || place.h < 1.0) {
            place = r;
        }
        self.view.reveal(&place);
        let editor = LabelEditor::open(
            &self.overlay,
            id,
            &markdown,
            place,
            size,
            self.font.borrow().as_deref(),
            self.spellcheck.get(),
        );
        *self.label.borrow_mut() = Some(editor.clone());
        self.place_label();
        // A click outside finishes the label, from an idle rather than the handler: leaving is
        // GTK moving the focus, and taking the editor off the canvas meanwhile left GTK walking
        // up from a widget that was gone, over and over (the freeze). Only the editor that left:
        // another may have opened by the time the idle runs.
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
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave({
            let later = later.clone();
            move |_| later()
        });
        editor.view().add_controller(focus);
        wire_keys(
            &editor,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || tab.finish_label()
            ),
        );
        // Space that takes no keyboard moves no focus, so the press on it is heard on the window
        // instead. Wired now that the editor is in place, and taken off again when it closes.
        wire_press(&editor, later);
        // Only now: a focus controller hears the focus leave only if it saw it come in, so a
        // click outside finishes the label only when the grab comes after the wiring.
        editor.focus();
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

    /// Put the label editor away, writing what was typed into the cell. Safe to call twice:
    /// taking the editor off the canvas moves the focus, which calls it again.
    pub fn finish_label(self: &Rc<Self>) {
        let Some(editor) = self.label.borrow_mut().take() else {
            return;
        };
        let typed = editor.changed();
        editor.close(&self.overlay);
        if let Some(markdown) = typed {
            let id = editor.cell.clone();
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
        self.label.borrow().as_ref().map(|e| e.cell.clone())
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
