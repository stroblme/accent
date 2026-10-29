//! The native preview, for now the note's plain text, read-only, on the editor's page: its font,
//! gutters, zoom and column. The renderer that sets the note as a page replaces it (stage 2, P3).

use super::Keys;
use crate::editor::{self, Flavour};
use accent_core::config::Config;
use accent_core::note::Syntax;
use accent_core::search::Options;
use gtk::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

pub struct Native {
    /// The editor's frame (`editor/open.rs`): an overlay over the scroller, with a zero-size area
    /// laid over it that reports the width the column is a share of.
    root: gtk::Widget,
    view: sourceview5::View,
    /// Where [`Native::scroll_to_line`] scrolls to.
    mark: gtk::TextMark,
    page: Rc<Page>,
    /// The view's font rule, at the document zoom ([`editor::install_font`]).
    font: RefCell<Option<gtk::CssProvider>>,
    /// The note and the text up: rendering them again changes nothing.
    shown: RefCell<(String, String)>,
}

/// The column the page is capped to, as the editor caps its own (`Tab::set_clamp`).
struct Page {
    clamp: adw::ClampScrollable,
    scroller: gtk::ScrolledWindow,
    zoom: Cell<f64>,
    /// The share of the view in percent, `None` under Full-Width Preview.
    column: Cell<Option<u32>>,
}

impl Page {
    fn set_clamp(&self) {
        let max = match self.column.get() {
            Some(percent) => editor::column_max(self.scroller.width(), percent, self.zoom.get()),
            None => i32::MAX / 4,
        };
        self.clamp.set_maximum_size(max);
        self.clamp.set_tightening_threshold(max * 3 / 4);
    }
}

impl Native {
    /// The arguments are [`super::Preview::new`]'s, which the rendered page will use.
    pub fn new(
        _resolve: impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync + 'static,
        _inverted: Keys,
        _web: Keys,
        _on_open: impl Fn(&str) + 'static,
        _on_tag: impl Fn(&str) + 'static,
        _on_invert: impl Fn(&str) + 'static,
    ) -> Native {
        // No language: the view is a note's page, which nothing highlights as source.
        let buffer = sourceview5::Buffer::new(None);
        buffer.set_enable_undo(false);
        buffer.set_highlight_matching_brackets(false);
        editor::sync_scheme(&buffer);
        let view = sourceview5::View::with_buffer(&buffer);
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_pixels_above_lines(editor::LINE_SPACING);
        view.set_pixels_below_lines(editor::LINE_SPACING);
        view.add_css_class("accent-doc");
        view.set_widget_name(&editor::next_view_name());

        let clamp = adw::ClampScrollable::builder()
            .child(&view)
            .unit(adw::LengthUnit::Px)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&clamp)
            .build();
        crate::widgets::smooth_wheel(&scroller);
        let width = gtk::DrawingArea::builder()
            .can_target(false)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build();
        let overlay = gtk::Overlay::builder().child(&scroller).build();
        overlay.add_overlay(&width);

        let page = Rc::new(Page {
            clamp,
            scroller,
            zoom: Cell::new(1.0),
            column: Cell::new(None),
        });
        width.connect_resize(glib::clone!(
            #[weak]
            page,
            move |_, _, _| page.set_clamp()
        ));
        let mark = buffer.create_mark(None, &buffer.start_iter(), true);
        Native {
            root: overlay.upcast(),
            view,
            mark,
            page,
            font: RefCell::default(),
            shown: RefCell::default(),
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    pub fn focus_target(&self) -> &gtk::Widget {
        self.view.upcast_ref()
    }

    /// Put `text` up, `rel` being the note it is; the same again changes nothing.
    pub fn render(&self, rel: &str, text: &str) {
        let mut shown = self.shown.borrow_mut();
        if shown.0 == rel && shown.1 == text {
            return;
        }
        *shown = (rel.to_string(), text.to_string());
        self.view.buffer().set_text(text);
    }

    /// Scroll source line `line` (1-based) to the top; the first, to the page's own top.
    ///
    /// A view not allocated yet, as a preview tab's just shown, scrolls once it is and GTK has
    /// measured its lines, as the editor's `Tab::scroll_to_caret` waits; before, the scroll stops
    /// short.
    pub fn scroll_to_line(&self, line: u32) {
        let buffer = self.view.buffer();
        let at = match line {
            0 | 1 => buffer.start_iter(),
            _ => buffer
                .iter_at_line(line as i32 - 1)
                .unwrap_or_else(|| buffer.end_iter()),
        };
        buffer.move_mark(&self.mark, &at);
        if self.view.height() > 0 {
            return scroll_to(&self.view, &self.mark);
        }
        let mark = self.mark.clone();
        self.view.add_tick_callback(move |view, _| {
            if view.height() == 0 {
                return glib::ControlFlow::Continue;
            }
            let (view, mark) = (view.downgrade(), mark.clone());
            glib::idle_add_local_once(move || {
                if let Some(view) = view.upgrade() {
                    scroll_to(&view, &mark);
                }
            });
            glib::ControlFlow::Break
        });
    }

    /// The theme's colours; there are no images whose look could have changed.
    pub fn restyle(&self) -> bool {
        if let Some(buffer) = self.view.buffer().downcast_ref::<sourceview5::Buffer>() {
            editor::sync_scheme(buffer);
        }
        false
    }

    pub fn forget_images(&self, then: impl FnOnce() + 'static) {
        then();
    }

    pub fn forget_image(&self, _key: &str) -> bool {
        false
    }

    /// Nothing is lost: there is no process to end.
    pub fn connect_lost(&self, _f: impl Fn() + 'static) {}

    /// The document zoom, shared with the editor: its font, gutters and column.
    pub fn set_zoom(&self, zoom: f64) {
        self.page.zoom.set(zoom);
        editor::set_margins(&self.view, zoom);
        let name = self.view.widget_name();
        let note = Flavour::Note(Syntax::Markdown);
        editor::install_font(&self.font, note, None, zoom, &name);
        editor::refont(&self.view);
        self.page.set_clamp();
    }

    /// The page's width as Preferences set it: the editor's Column Width, or the whole view.
    pub fn set_column(&self, config: &Config) {
        let column = crate::export::css::column(config);
        if self.page.column.replace(column) != column {
            self.page.set_clamp();
        }
    }

    // Find comes with the rendered page (stage 2, P6).

    pub fn find(&self, _text: &str, _options: Options) {}

    pub fn find_next(&self) {}

    pub fn find_previous(&self) {}

    pub fn find_clear(&self) {}

    pub fn connect_found(&self, _f: impl Fn(&str) + 'static) {}
}

/// Put `mark`'s line at the top of `view`, or the page's own top, margin included, for the first.
fn scroll_to(view: &sourceview5::View, mark: &gtk::TextMark) {
    if view.buffer().iter_at_mark(mark).offset() > 0 {
        view.scroll_to_mark(mark, 0.0, true, 0.0, 0.0);
    } else if let Some(adjustment) = view.vadjustment() {
        adjustment.set_value(0.0);
    }
}

impl Drop for Native {
    /// Take the font rule off the display and free the view, as a closed editor tab does.
    fn drop(&mut self) {
        if let (Some(display), Some(provider)) =
            (gdk::Display::default(), self.font.borrow_mut().take())
        {
            gtk::style_context_remove_provider_for_display(&display, &provider);
        }
        editor::release(&self.view);
    }
}
