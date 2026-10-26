//! A cell's source before its editor is built: its text laid out as the editor will lay it out at
//! the width it is given, wrapped and spaced alike, so a cell is as tall before its editor is
//! built as after and the scroll's range keeps still as editors are built near the view. A code
//! cell's lines come out as tall as its editor's; a markdown cell's headings are set at their
//! scale, its other styling not measured. Laid out a line at a time in Pango, once a width.

use std::cell::{Cell, RefCell};

use accent_core::notebook::Kind;
use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, pango};

use crate::{highlight, typing};

glib::wrapper! {
    pub struct Placeholder(ObjectSubclass<imp::Placeholder>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// The room around the text inside the editor, as `CellView::build_editor` sets it.
#[derive(Default)]
pub struct Margins {
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
}

impl Placeholder {
    /// `source` of a `kind` cell, in its editor's font (the `accent-doc` or `accent-code` rule)
    /// and `margins`.
    pub fn new(source: &str, kind: Kind, margins: Margins) -> Placeholder {
        let placeholder: Placeholder = glib::Object::new();
        placeholder.add_css_class(match kind {
            Kind::Markdown => "accent-doc",
            Kind::Code | Kind::Raw => "accent-code",
        });
        let imp = placeholder.imp();
        imp.text.replace(source.to_string());
        imp.markdown.set(kind == Kind::Markdown);
        imp.margins.replace(margins);
        placeholder
    }

    pub fn text(&self) -> String {
        self.imp().text.borrow().clone()
    }

    pub fn set_text(&self, source: &str) {
        self.imp().text.replace(source.to_string());
        self.relayout();
    }

    pub fn set_left_margin(&self, left: i32) {
        self.imp().margins.borrow_mut().left = left;
        self.relayout();
    }

    fn relayout(&self) {
        self.imp().measured.take();
        self.queue_resize();
    }

    /// Lay the text out at `width` as its editor would, each line handed to `line` with where its
    /// top is; the height it all takes.
    fn lay_out(&self, width: i32, mut line: impl FnMut(&pango::Layout, i32)) -> i32 {
        let imp = self.imp();
        let margins = imp.margins.borrow();
        let markdown = imp.markdown.get();
        let layout = self.create_pango_layout(None);
        // A text view wraps a pixel short of its width, which it keeps for the cursor; with no
        // width given, nothing wraps.
        if width > 0 {
            let fit = (width - 1 - margins.left - margins.right).max(1);
            layout.set_width(fit * pango::SCALE);
        }
        layout.set_wrap(pango::WrapMode::WordChar);
        // GtkSourceView's tab stops: as wide as the tab width's spaces (`editor::build`).
        let space = self.create_pango_layout(Some(" ")).size().0;
        let tab = self
            .create_pango_layout(Some(&" ".repeat(TAB_WIDTH)))
            .pixel_size()
            .0;
        let mut tabs = pango::TabArray::new(1, true);
        tabs.set_tab(0, pango::TabAlign::Left, tab);
        layout.set_tabs(Some(&tabs));
        let mut top = margins.top;
        for text in imp.text.borrow().split('\n') {
            // A heading's marks hang in the margin, its text set at the heading's scale.
            let heading = markdown.then(|| heading(text)).flatten();
            let (shown, indent) = match heading {
                Some((level, rest)) => {
                    let attrs = pango::AttrList::new();
                    attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
                    attrs.insert(pango::AttrFloat::new_scale(
                        highlight::HEADING_SCALES[level - 1],
                    ));
                    layout.set_attributes(Some(&attrs));
                    (rest, 0)
                }
                None => {
                    layout.set_attributes(None);
                    // Wrapped rows hang behind the line's indent and marker (`wrap::retag`).
                    let column = typing::wrap_column(text, TAB_WIDTH, TAB_WIDTH, markdown);
                    let column = column.min(crate::wrap::COLUMNS) as i32;
                    (
                        text,
                        (f64::from(column * space) / f64::from(pango::SCALE)).round() as i32,
                    )
                }
            };
            layout.set_indent(-indent * pango::SCALE);
            layout.set_text(shown);
            top += LINE_SPACING;
            line(&layout, top);
            // As a text view rounds a line's height.
            top += (layout.size().1 + pango::SCALE / 2) / pango::SCALE + LINE_SPACING;
        }
        top + margins.bottom
    }

    /// How tall the text is at `width`, laid out once a width and font.
    fn height(&self, width: i32) -> i32 {
        let font = self.pango_context().font_description();
        if let Some((_, _, height)) = self
            .imp()
            .measured
            .borrow()
            .as_ref()
            .filter(|(at, was, _)| *at == width && *was == font)
        {
            return *height;
        }
        let height = self.lay_out(width, |_, _| {});
        self.imp().measured.replace(Some((width, font, height)));
        height
    }
}

/// The tab width a cell's editor counts its columns in (`editor::build`).
const TAB_WIDTH: usize = 4;

/// The room above and below each line of an editor (`editor::build`).
const LINE_SPACING: i32 = crate::editor::LINE_SPACING;

/// An ATX heading's level and its text after the marks.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    let rest = line[level..].strip_prefix(' ')?;
    (1..=6).contains(&level).then_some((level, rest))
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Placeholder {
        pub(super) text: RefCell<String>,
        pub(super) markdown: Cell<bool>,
        pub(super) margins: RefCell<Margins>,
        /// The last width and font laid out at, and the height they took.
        pub(super) measured: RefCell<Option<(i32, Option<pango::FontDescription>, i32)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Placeholder {
        const NAME: &'static str = "AccentCellPlaceholder";
        type Type = super::Placeholder;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Placeholder {}

    impl WidgetImpl for Placeholder {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                // It wraps at whatever width it is given, as the editor does.
                gtk::Orientation::Horizontal => (0, 0, -1, -1),
                _ => {
                    let size = self.obj().height(for_size);
                    (size, size, -1, -1)
                }
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (x, colour) = (self.margins.borrow().left, obj.color());
            obj.lay_out(obj.width(), |layout, top| {
                snapshot.save();
                snapshot.translate(&graphene::Point::new(x as f32, top as f32));
                snapshot.append_layout(layout, &colour);
                snapshot.restore();
            });
        }
    }
}
