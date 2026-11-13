//! How the cells look. The editor font, the Preview Font and the zoom make one rule for all of
//! them, a Ctrl+scroll zooms about the pointer, and the editors and outputs follow the theme.

use std::rc::Rc;

use accent_core::note::Syntax;
use accent_core::notebook::Kind;
use adw::prelude::*;
use gtk::{gdk, glib};

use super::NotebookTab;
use super::cell;

impl NotebookTab {
    /// The theme, or Preferences' Images switch, changed: the editors' schemes, the markdown's
    /// colours, the pictures recoloured.
    pub fn restyle(&self) {
        for view in self.views.borrow().iter() {
            if let Some(editor) = view.editor() {
                crate::editor::sync_scheme(&editor.buffer);
                crate::editor::mute(&editor.buffer, &editor.occurrence);
                if view.kind == Kind::Markdown {
                    crate::editor::restyle_note(&editor.view, &editor.buffer, Syntax::Markdown);
                }
            }
            view.outputs.restyle();
        }
        self.restyle_language();
    }

    /// Preferences' Preview Font, which a rendered cell's prose is set in.
    pub fn set_preview_font(&self, font: Option<&str>) {
        if self.fonts.borrow().1.as_deref() == font {
            return;
        }
        self.fonts.borrow_mut().1 = font.map(str::to_string);
        let editor = self.fonts.borrow().0.clone();
        self.set_font(editor.as_deref(), self.zoom.get());
    }

    /// The editor font and the document zoom, one rule for every cell.
    pub fn set_font(&self, font: Option<&str>, zoom: f64) {
        // What shows stays where it is as the cells take the heights of a new zoom, read while
        // they still have the old ones.
        let at = self.zoom_at.take();
        if zoom != self.zoom.get() {
            let anchor = self.zoom_anchor_at(at);
            self.zoom_anchor.replace(anchor);
        }
        let Some(display) = gdk::Display::default() else {
            return;
        };
        if let Some(old) = self.font.borrow_mut().take() {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        let name = self.column.widget_name();
        self.fonts.borrow_mut().0 = font.map(str::to_string);
        let doc = font
            .filter(|f| !f.is_empty())
            .map_or_else(crate::editor::default_font, str::to_string);
        let mono = crate::platform::monospace_font();
        // A rendered cell's prose is in the Preview Font, as a note's preview is.
        let prose = self.fonts.borrow().1.clone();
        let prose = prose
            .filter(|f| !f.is_empty())
            .unwrap_or_else(crate::platform::document_font);
        let css = [
            crate::editor::font_css(&doc, &format!("#{name} .accent-doc"), zoom),
            crate::editor::font_css(&prose, &format!("#{name} .accent-doc.accent-preview"), zoom),
            crate::editor::font_css(&mono, &format!("#{name} .accent-code"), zoom),
            crate::editor::font_css(&mono, &format!("#{name} .notebook-output"), zoom),
            // What outputs draw in the interface font, a table and the lines about it, scaled too.
            format!(
                "#{name} .notebook-outputs label {{ font-size: {:.0}%; }}",
                zoom * 100.0
            ),
        ]
        .join(" ");
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&css);
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        *self.font.borrow_mut() = Some(provider);
        self.gutter.set(cell::count_width(&self.scroller));
        let prose_font = self.fonts.borrow().1.clone();
        let rezoomed = self.zoom.replace(zoom) != zoom;
        for view in self.views.borrow().iter() {
            view.set_gutter(self.gutter.get());
            if rezoomed {
                view.outputs.rezoom();
            }
            if let Some(editor) = view.editor() {
                crate::editor::refont(&editor.view);
            }
            view.with_rendered(|page| {
                page.set_font(prose_font.as_deref());
                page.set_zoom(zoom);
            });
        }
        self.refit_after_layout.set(true);
        // What is measured in the cells' fonts, once the new rule has reached their views.
        let views: Vec<_> = self.views.borrow().iter().map(Rc::downgrade).collect();
        glib::idle_add_local_once(move || {
            for view in views.iter().filter_map(std::rc::Weak::upgrade) {
                if let Some(editor) = view.editor() {
                    crate::editor::remeasure(&editor.view, cell::flavour(view.kind));
                }
            }
        });
    }

    /// Zoom as a Ctrl+scroll notch with the pointer at `at` in the view does, keeping what is
    /// under the pointer there.
    pub(super) fn wheel_zoom(&self, at: Option<(f64, f64)>, out: bool) {
        if let Some(f) = self.on_zoom.get() {
            self.zoom_at.set(at);
            f(out);
        }
    }
}
