//! Editing a label: the note editor, floated over the cell, holding the label as Markdown.
//!
//! A draw.io label is an HTML fragment; the crate turns it into Markdown and back
//! (`accent_drawio::label`), so a label is written the way a note is, with the same styling,
//! spellcheck and font. Per-run colours and sizes a label had are given up once it is edited:
//! the cell's own font colour and size then apply, which is what the Properties pane sets.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_drawio::{CellId, Rect};
use adw::prelude::*;
use gtk::{gdk, glib, graphene, pango};

use crate::editor::{self, Flavour};

/// The smallest box the editor is given, in pixels: a label on a hairline edge still needs room
/// for a word.
const MIN_SIZE: (f64, f64) = (160.0, 40.0);

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

/// The box the editor is given over a label `at` on screen: the label's own, grown to the
/// smallest one a word can be typed in and centred on it. The corner may be off the canvas,
/// where the overlay clips it — a label scrolled past the edge is gone, not pinned to it.
fn box_at(at: Rect) -> (f64, f64, f64, f64) {
    let (w, h) = (at.w.max(MIN_SIZE.0), at.h.max(MIN_SIZE.1));
    (at.x - (w - at.w) / 2.0, at.y - (h - at.h) / 2.0, w, h)
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
        view.set_left_margin(6);
        view.set_right_margin(6);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        view.set_widget_name(&editor::next_view_name());
        let frame = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
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
        overlay.set_clip_overlay(&layer, true);
        // What the overlay clips from sight the layer keeps from the pointer too: an editor
        // hanging over the banner above the canvas, which is drawn over it, must not take the
        // press meant for the banner's button.
        layer.set_overflow(gtk::Overflow::Hidden);
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
        let (x, y, w, h) = box_at(at);
        self.frame.set_size_request(w as i32, h as i32);
        self.layer.move_(&self.frame, x, y);
        // The font only when the zoom moved: it is a provider on the whole display, far too much
        // to install again at every step of a scroll.
        if self.px.replace(px) != px {
            self.set_font(px);
        }
    }

    /// Where the editor is on the canvas and how big its text is there: what the drills watch
    /// while the page moves under it.
    #[cfg(feature = "bench")]
    pub fn at(&self) -> (f64, f64, f64) {
        let (x, y) = self.layer.child_position(&self.frame);
        (x, y, self.px.get())
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

#[cfg(test)]
mod tests {
    use accent_drawio::Rect;

    #[test]
    fn a_label_scrolled_off_the_edge_keeps_its_box() {
        // A 40x20 label scrolled just past the canvas's left edge: a box big enough to type in,
        // centred on the label, and left at the negative corner the overlay clips.
        let (x, y, w, h) = super::box_at(Rect::new(-30.0, 100.0, 40.0, 20.0));
        assert_eq!((w, h), super::MIN_SIZE);
        assert_eq!(x, -30.0 - (super::MIN_SIZE.0 - 40.0) / 2.0);
        assert_eq!(y, 100.0 - (super::MIN_SIZE.1 - 20.0) / 2.0);
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
