//! Editing a label: the note editor, floated over the cell, holding the label as Markdown.
//!
//! A draw.io label is an HTML fragment; the crate turns it into Markdown and back
//! (`accent_drawio::label`), so a label is written the way a note is, with the same styling,
//! spellcheck and font. Per-run colours and sizes a label had are given up once it is edited:
//! the cell's own font colour and size then apply, which is what the Properties pane sets.

use std::cell::RefCell;
use std::rc::Rc;

use accent_drawio::{CellId, Rect};
use adw::prelude::*;
use gtk::{gdk, glib, pango};

use crate::editor::{self, Flavour};

/// The smallest box the editor is given, in pixels: a label on a hairline edge still needs room
/// for a word.
const MIN_SIZE: (f64, f64) = (160.0, 40.0);

pub struct LabelEditor {
    pub cell: CellId,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    frame: gtk::ScrolledWindow,
    font: RefCell<Option<gtk::CssProvider>>,
    original: String,
}

/// The zoom that makes the note font, `doc_pt` points, show a label's `font_px` page pixels at
/// `scale` on screen. Kept to a range a label can still be read and typed in at.
pub fn label_zoom(font_px: f64, scale: f64, doc_pt: f64) -> f64 {
    let doc_px = doc_pt * 96.0 / 72.0;
    (font_px * scale / doc_px.max(1.0)).clamp(0.6, 3.0)
}

impl LabelEditor {
    /// The editor over `at` (widget coordinates) in `overlay`, holding `markdown`. `done` gets
    /// the new Markdown, or `None` when nothing changed.
    /// `px` is the label's font size on screen, which the editor's text is made to match.
    pub fn open(
        overlay: &gtk::Overlay,
        cell: CellId,
        markdown: &str,
        at: Rect,
        px: f64,
        font: Option<&str>,
        spellcheck: bool,
    ) -> Rc<LabelEditor> {
        let (view, buffer) = editor::overlay_view(markdown);
        view.set_left_margin(6);
        view.set_right_margin(6);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        let frame = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .css_classes(["accent-label-editor", "view"])
            .build();
        let (w, h) = (at.w.max(MIN_SIZE.0), at.h.max(MIN_SIZE.1));
        let (x, y) = (at.x - (w - at.w) / 2.0, at.y - (h - at.h) / 2.0);
        frame.set_margin_start(x.max(0.0) as i32);
        frame.set_margin_top(y.max(0.0) as i32);
        frame.set_size_request(w as i32, h as i32);
        overlay.add_overlay(&frame);
        // Placed by its margins over the page: measured, an editor near the far edge would
        // widen the overlay and with it the canvas under the label being edited.
        overlay.set_measure_overlay(&frame, false);
        overlay.set_clip_overlay(&frame, true);
        let editor = Rc::new(LabelEditor {
            cell,
            view,
            buffer,
            frame,
            font: RefCell::new(None),
            original: markdown.to_string(),
        });
        editor.set_font(font, px);
        if spellcheck {
            let adapter = libspelling::TextBufferAdapter::new(
                &editor.buffer,
                &libspelling::Checker::default(),
            );
            editor.view.insert_action_group("spelling", Some(&adapter));
            editor.view.set_extra_menu(Some(&adapter.menu_model()));
            adapter.set_enabled(true);
        }
        editor.buffer.connect_changed(|buffer| {
            crate::highlight::apply(buffer);
        });
        let (view, buffer) = (editor.view.clone(), editor.buffer.clone());
        editor.view.connect_realize(move |_| {
            crate::highlight::restyle(&buffer, &view);
        });
        editor
            .buffer
            .select_range(&editor.buffer.start_iter(), &editor.buffer.end_iter());
        editor.view.grab_focus();
        editor
    }

    /// The note font, zoomed to `px` on screen.
    fn set_font(&self, font: Option<&str>, px: f64) {
        self.view.set_widget_name(&editor::next_view_name());
        let desc = pango::FontDescription::from_string(
            &font
                .map(str::to_string)
                .unwrap_or_else(editor::default_font),
        );
        let doc_pt = f64::from(desc.size()) / f64::from(pango::SCALE);
        let zoom = label_zoom(px, 1.0, doc_pt);
        editor::install_font(
            &self.font,
            Flavour::Note,
            font,
            zoom,
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

    /// Take the editor off the canvas and drop its font.
    pub fn close(&self, overlay: &gtk::Overlay) {
        overlay.remove_overlay(&self.frame);
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

#[cfg(test)]
mod tests {
    #[test]
    fn the_editor_shows_a_label_at_its_size_on_screen() {
        // 12 px at 2× is 24 px on screen; an 11 pt note font is 14.67 px.
        let zoom = super::label_zoom(12.0, 2.0, 11.0);
        assert!((zoom - 24.0 / (11.0 * 96.0 / 72.0)).abs() < 1e-9);
        assert_eq!(super::label_zoom(1.0, 0.1, 11.0), 0.6);
        assert_eq!(super::label_zoom(100.0, 8.0, 11.0), 3.0);
    }
}
