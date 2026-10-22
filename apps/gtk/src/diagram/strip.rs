//! The diagram's pages as thumbnails: what the Outline pane shows under the page names, and what
//! `crate::organize` moves, deletes and inserts pages on.
//!
//! One canvas in a viewport, laid out as a PDF's strip is (`pdf::geometry::layout`). A page is
//! painted when it first comes on screen, into a render node at a pixel a page unit that is then
//! scaled into its thumbnail, and again only once an edit changed it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_drawio::{Color, File, Page, Rect, Scene};
use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use super::geometry::Frame;
use super::paint::{self, Cache, Tint};
use crate::pdf::geometry::{Layout, PageRect, PdfZoom, fit_scale, layout, page_at};
use crate::theme;
use crate::widgets::Hook;

/// One page's thumbnail.
struct Thumb {
    /// The page as it was painted, with its place and the page count then, which a label with
    /// placeholders may show.
    page: Page,
    at: (usize, usize),
    /// What of the page shows, in page units: its sheet, or with none its drawing.
    area: Rect,
    /// The page's display list, made when it is first painted (or at once for a page with no
    /// sheet, whose drawing is its size).
    scene: Option<Scene>,
    /// Painted since the page last changed, into this node (`None` for nothing drawn).
    painted: bool,
    node: Option<gsk::RenderNode>,
}

impl Thumb {
    fn new(page: &Page, at: usize, count: usize) -> Thumb {
        let (w, h) = page.size();
        let mut thumb = Thumb {
            page: page.clone(),
            at: (at, count),
            area: Rect::new(0.0, 0.0, w, h),
            scene: None,
            painted: false,
            node: None,
        };
        if !page.page_view() {
            let drawing = thumb
                .scene()
                .prims
                .iter()
                .map(|p| p.bounds())
                .reduce(|a, b| a.union(&b));
            if let Some(d) = drawing.filter(|d| d.w > 0.0 && d.h > 0.0) {
                thumb.area = d;
            }
        }
        thumb
    }

    fn scene(&mut self) -> &Scene {
        let (page, (at, count)) = (&self.page, self.at);
        self.scene
            .get_or_insert_with(|| accent_drawio::scene_with(page, &super::shown(at, count)))
    }
}

glib::wrapper! {
    pub struct PageStrip(ObjectSubclass<imp::PageStrip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PageStrip {
    /// A strip of no pages yet, and the scrolled window it is shown in.
    pub fn new() -> (PageStrip, gtk::ScrolledWindow) {
        let strip: PageStrip = glib::Object::new();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&strip)
            .build();
        // Only the pages on screen are painted, so a scroll paints again.
        let adjustment = scroller.vadjustment();
        adjustment.connect_value_changed(glib::clone!(
            #[weak]
            strip,
            move |_| strip.queue_draw()
        ));
        strip.imp().adjustment.replace(Some(adjustment));
        // A thumbnail is a button: a click goes to its page, on the release, because a press is
        // also how a drag that moves the page begins. A drag takes the sequence, so it never gets
        // here.
        let click = gtk::GestureClick::builder().button(1).build();
        click.connect_released(glib::clone!(
            #[weak]
            strip,
            move |_, _, x, y| {
                let page = strip.page_at(x as f32, y as f32);
                if let (Some(page), Some(f)) = (page, strip.imp().on_goto.get()) {
                    f(page);
                }
            }
        ));
        strip.add_controller(click);
        (strip, scroller)
    }

    /// Show `file`'s pages, painting again only those an edit changed.
    pub fn set_pages(&self, file: &File) {
        let count = file.pages.len();
        let mut old = std::mem::take(&mut *self.imp().thumbs.borrow_mut());
        let thumbs = file
            .pages
            .iter()
            .enumerate()
            .map(|(at, page)| {
                match old
                    .iter()
                    .position(|t| t.at == (at, count) && t.page == *page)
                {
                    Some(i) => old.swap_remove(i),
                    None => Thumb::new(page, at, count),
                }
            })
            .collect();
        *self.imp().thumbs.borrow_mut() = thumbs;
        self.queue_resize();
        self.queue_draw();
    }

    /// Paint every page again: the colours, or the pictures it may draw, changed.
    pub fn forget(&self) {
        for thumb in self.imp().thumbs.borrow_mut().iter_mut() {
            thumb.painted = false;
            thumb.node = None;
        }
        self.queue_draw();
    }

    /// Paint in `tint`, as the canvas does.
    pub fn set_tint(&self, tint: Tint) {
        if self.imp().tint.replace(tint) != tint {
            self.forget();
        }
    }

    /// Draw the pictures the diagram links from the web, as the canvas does once allowed.
    pub fn set_web(&self, on: bool) {
        self.imp().cache.set_web(on);
        self.forget();
    }

    /// Frame `page`, the one the canvas shows.
    pub fn set_framed(&self, page: usize) {
        if self.imp().framed.replace(page) != page {
            self.queue_draw();
        }
    }

    /// Called with a page when its thumbnail is clicked.
    pub fn connect_goto(&self, f: impl Fn(usize) + 'static) {
        self.imp().on_goto.set(Rc::new(f));
    }

    /// Where the thumbnails are, in the strip's own coordinates, which are the scrolled content's.
    pub fn layout(&self) -> Layout {
        self.imp().layout.borrow().clone()
    }

    /// Page `page` as its thumbnail shows it, for a drag to hold.
    pub fn picture(&self, page: usize) -> Option<gdk::Texture> {
        let rect = *self.imp().layout.borrow().pages.get(page)?;
        let snapshot = gtk::Snapshot::new();
        snapshot.translate(&graphene::Point::new(-rect.x, -rect.y));
        let mut thumbs = self.imp().thumbs.borrow_mut();
        self.paint_page(&snapshot, thumbs.get_mut(page)?, rect, false);
        let node = snapshot.to_node()?;
        let viewport = graphene::Rect::new(0.0, 0.0, rect.w, rect.h);
        Some(
            self.native()?
                .renderer()?
                .render_texture(&node, Some(&viewport)),
        )
    }

    /// The page whose thumbnail holds a point of the strip.
    fn page_at(&self, x: f32, y: f32) -> Option<usize> {
        let layout = self.imp().layout.borrow();
        layout
            .pages
            .iter()
            .position(|r| (r.x..=r.x + r.w).contains(&x) && (r.y..=r.y + r.h).contains(&y))
    }

    /// The thumbnails laid out `width` wide, each page as wide as the strip allows.
    fn laid_out(&self, width: f32) -> Layout {
        let thumbs = self.imp().thumbs.borrow();
        let sizes: Vec<(f32, f32)> = thumbs
            .iter()
            .map(|t| (t.area.w as f32, t.area.h as f32))
            .collect();
        layout(
            &sizes,
            fit_scale(&sizes, PdfZoom::FitWidth, width, 0.0),
            width,
        )
    }

    /// What `snapshot` draws: the pages on screen, the one the canvas shows framed in the accent.
    fn paint(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let layout = imp.layout.borrow();
        if layout.pages.is_empty() {
            return;
        }
        let (top, height) = imp
            .adjustment
            .borrow()
            .as_ref()
            .map_or((0.0, f64::from(self.height())), |a| {
                (a.value(), a.page_size())
            });
        let (first, last) = (page_at(&layout, top), page_at(&layout, top + height));
        let mut thumbs = imp.thumbs.borrow_mut();
        for (i, rect) in layout.pages.iter().enumerate().take(last + 1).skip(first) {
            let Some(thumb) = thumbs.get_mut(i) else {
                break;
            };
            self.paint_page(snapshot, thumb, *rect, i == imp.framed.get());
        }
    }

    /// One page into `rect`, painted first if it has not been since it changed.
    fn paint_page(
        &self,
        snapshot: &gtk::Snapshot,
        thumb: &mut Thumb,
        rect: PageRect,
        framed: bool,
    ) {
        let imp = self.imp();
        let tint = imp.tint.get();
        if !thumb.painted {
            imp.cache.forget();
            let drawn = gtk::Snapshot::new();
            let frame = Frame::default();
            for (j, prim) in thumb.scene().prims.iter().enumerate() {
                paint::prim(&drawn, self.upcast_ref(), j, prim, &frame, &imp.cache, tint);
            }
            thumb.node = drawn.to_node();
            thumb.painted = true;
        }
        let bounds = graphene::Rect::new(rect.x, rect.y, rect.w, rect.h);
        let paper = thumb.scene().background.unwrap_or(Color::WHITE);
        snapshot.append_color(&tint.rgba(paper), &bounds);
        if let Some(node) = &thumb.node {
            let scale = rect.w / thumb.area.w.max(1.0) as f32;
            snapshot.push_clip(&bounds);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(rect.x, rect.y));
            snapshot.scale(scale, scale);
            let origin = (-thumb.area.x as f32, -thumb.area.y as f32);
            snapshot.translate(&graphene::Point::new(origin.0, origin.1));
            snapshot.append_node(node);
            snapshot.restore();
            snapshot.pop();
        }
        // A hairline, so a white page on a light background still reads as a page.
        let edge = theme::at(self.color(), theme::PAGE_EDGE_ALPHA);
        let outline = gsk::RoundedRect::from_rect(bounds, 0.0);
        snapshot.append_border(&outline, &[1.0; 4], &[edge; 4]);
        if framed {
            snapshot.append_border(&outline, &[2.0; 4], &[theme::accent(); 4]);
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PageStrip {
        pub(super) thumbs: RefCell<Vec<Thumb>>,
        pub(super) layout: RefCell<Layout>,
        /// The scrolled window's, which says which pages are on screen.
        pub(super) adjustment: RefCell<Option<gtk::Adjustment>>,
        pub(super) framed: Cell<usize>,
        pub(super) tint: Cell<Tint>,
        /// Label layouts and decoded pictures, kept across the pages painted.
        pub(super) cache: Cache,
        pub(super) on_goto: Hook<dyn Fn(usize)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PageStrip {
        const NAME: &'static str = "AccentDiagramStrip";
        type Type = super::PageStrip;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for PageStrip {}

    impl WidgetImpl for PageStrip {
        /// As tall as its pages are at the width it is given.
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (0, 0, -1, -1),
                _ => {
                    let width = for_size.max(0) as f32;
                    let height = self.obj().laid_out(width).height.ceil() as i32;
                    (height, height, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            *self.layout.borrow_mut() = self.obj().laid_out(width as f32);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().paint(snapshot);
        }
    }
}
