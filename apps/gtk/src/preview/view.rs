//! The preview's text view: a source view with no language, so the editor's margins, scheme and
//! release apply to it unchanged. Below the text it paints what a tag cannot: the wash behind a
//! code block, the bar beside a quote, a callout's box and a rule. Copy writes each picture,
//! formula and table back as the markdown it came from.

use super::text::{Href, PAD};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};

glib::wrapper! {
    pub struct PreviewView(ObjectSubclass<imp::PreviewView>)
        @extends sourceview5::View, gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

/// What the view paints under the lines tagged with one of its tags, and in which colours.
pub struct Decor {
    pub code: gtk::TextTag,
    pub quote: gtk::TextTag,
    pub callout: gtk::TextTag,
    pub rule: gtk::TextTag,
    /// A block's last line, the gap below it no part of a box.
    pub gap: gtk::TextTag,
    /// A code block's first and last lines, whose room above and below holds its box's padding.
    pub fence_top: gtk::TextTag,
    pub fence_bottom: gtk::TextTag,
    /// A callout's last line, whose room below holds its box's padding and then the gap.
    pub callout_end: gtk::TextTag,
    /// The code wash and the callout tint, the accent bar, the rule line.
    pub surface: gdk::RGBA,
    pub accent: gdk::RGBA,
    pub line: gdk::RGBA,
}

/// What Copy writes for the selection, `None` for GTK's own.
type Copy = Box<dyn Fn() -> Option<String>>;
/// Told the view's new width.
type Width = Box<dyn Fn(i32)>;
/// Follows a link.
type Follow = Box<dyn Fn(&Href)>;

/// The bar beside a quote or a callout, and the rounding of a code block's and a callout's box.
const BAR: f32 = 3.0;
const ROUND: f32 = 6.0;

impl PreviewView {
    pub(super) fn new() -> PreviewView {
        let buffer = sourceview5::Buffer::new(None);
        buffer.set_enable_undo(false);
        buffer.set_highlight_matching_brackets(false);
        glib::Object::builder()
            .property("buffer", &buffer)
            .property("editable", false)
            .property("cursor-visible", false)
            .property("wrap-mode", gtk::WrapMode::WordChar)
            .build()
    }

    pub(super) fn set_decor(&self, decor: Decor) {
        *self.imp().decor.borrow_mut() = Some(decor);
        self.queue_draw();
    }

    /// What Copy writes for the selection in place of GTK's text, `None` for GTK's own.
    pub(super) fn on_copy(&self, copy: impl Fn() -> Option<String> + 'static) {
        *self.imp().copy.borrow_mut() = Some(Box::new(copy));
    }

    /// What follows a link in a table's cell, which is a label of its own.
    pub(super) fn on_follow(&self, follow: impl Fn(&Href) + 'static) {
        *self.imp().follow.borrow_mut() = Some(Box::new(follow));
    }

    pub(super) fn follow(&self, href: &Href) {
        if let Some(follow) = self.imp().follow.borrow().as_ref() {
            follow(href);
        }
    }

    /// Told the view's width on an idle after an allocation that changed it, when a table can
    /// be sized again without resizing inside a layout.
    pub(super) fn on_width(&self, width: impl Fn(i32) + 'static) {
        *self.imp().on_width.borrow_mut() = Some(Box::new(width));
    }

    /// Paint the decorations of the lines on screen: a block's run of lines under one tag is one
    /// box.
    fn decorate(&self, snapshot: &gtk::Snapshot) {
        let decor = self.imp().decor.borrow();
        let Some(decor) = decor.as_ref() else {
            return;
        };
        let visible = self.visible_rect();
        let (left, right) = (
            self.left_margin() as f32,
            (visible.width() - self.right_margin()) as f32,
        );
        let (mut line, _) = self.line_at_y(visible.y());
        line.set_line_offset(0);
        // The run being gathered: which tag, from which y to which.
        let mut run: Option<(usize, i32, i32)> = None;
        let paint = |(kind, top, bottom): (usize, i32, i32)| {
            let (top, height) = (top as f32, (bottom - top) as f32);
            match kind {
                0 => {
                    let rect = graphene::Rect::new(left, top, right - left, height);
                    rounded(snapshot, &rect, [ROUND; 4], &decor.surface);
                }
                1 => {
                    let rect = graphene::Rect::new(left, top, BAR, height);
                    snapshot.append_color(&decor.accent, &rect);
                }
                2 => {
                    let rect = graphene::Rect::new(left, top, right - left, height);
                    rounded(snapshot, &rect, [0.0, ROUND, ROUND, 0.0], &decor.surface);
                    let bar = graphene::Rect::new(left, top, BAR, height);
                    snapshot.append_color(&decor.accent, &bar);
                }
                _ => {
                    let y = top + height / 2.0;
                    let rect = graphene::Rect::new(left, y.round(), right - left, 1.0);
                    snapshot.append_color(&decor.line, &rect);
                }
            }
        };
        let tags = [&decor.code, &decor.quote, &decor.callout, &decor.rule];
        // Whether the run ended with its block: two code blocks a blank line apart are two boxes.
        let mut ended = false;
        loop {
            let (y, height) = self.line_yrange(&line);
            let kind = tags.iter().position(|tag| line.has_tag(*tag));
            let has = |tag: &gtk::TextTag| line.has_tag(tag);
            // A code block's box holds PAD of its first and last lines' room, a callout's all of its
            // title's room above and its last line's but the gap below; other boxes end at their
            // text, the gap below their block outside them; a rule's line is all its own.
            let top = match has(&decor.fence_top) {
                true => y + decor.fence_top.pixels_above_lines() - PAD,
                false => y,
            };
            let bottom = if has(&decor.fence_bottom) {
                y + height - decor.fence_bottom.pixels_below_lines() + PAD
            } else if has(&decor.callout_end) || (has(&decor.gap) && kind.is_some_and(|k| k < 3)) {
                y + height - decor.gap.pixels_below_lines()
            } else {
                y + height
            };
            run = match (run, kind) {
                (Some((k, start, _)), Some(now)) if k == now && !ended => Some((k, start, bottom)),
                (run, kind) => {
                    if let Some(run) = run {
                        paint(run);
                    }
                    kind.map(|k| (k, top, bottom))
                }
            };
            ended = has(&decor.gap) || has(&decor.fence_bottom) || has(&decor.callout_end);
            if y > visible.y() + visible.height() || !line.forward_line() {
                break;
            }
        }
        if let Some(run) = run {
            paint(run);
        }
    }
}

/// `rect` filled in `colour` with corners rounded by `radii` (top left, top right, bottom right,
/// bottom left).
fn rounded(snapshot: &gtk::Snapshot, rect: &graphene::Rect, radii: [f32; 4], colour: &gdk::RGBA) {
    let corner = |r: f32| graphene::Size::new(r, r);
    let shape = gsk::RoundedRect::new(
        *rect,
        corner(radii[0]),
        corner(radii[1]),
        corner(radii[2]),
        corner(radii[3]),
    );
    snapshot.push_rounded_clip(&shape);
    snapshot.append_color(colour, rect);
    snapshot.pop();
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PreviewView {
        pub decor: RefCell<Option<Decor>>,
        pub copy: RefCell<Option<Copy>>,
        pub on_width: RefCell<Option<Width>>,
        pub(super) follow: RefCell<Option<Follow>>,
        pub width: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreviewView {
        const NAME: &'static str = "AccentPreviewView";
        type Type = super::PreviewView;
        type ParentType = sourceview5::View;
    }

    impl ObjectImpl for PreviewView {}

    impl WidgetImpl for PreviewView {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if self.width.replace(width) != width {
                let view = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(view) = view.upgrade()
                        && let Some(told) = view.imp().on_width.borrow().as_ref()
                    {
                        told(view.imp().width.get());
                    }
                });
            }
        }
    }

    impl TextViewImpl for PreviewView {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            if layer == gtk::TextViewLayer::BelowText {
                self.obj().decorate(&snapshot);
            }
            self.parent_snapshot_layer(layer, snapshot);
        }

        fn copy_clipboard(&self) {
            let text = self.copy.borrow().as_ref().and_then(|copy| copy());
            match text {
                Some(text) => self.obj().clipboard().set_text(&text),
                None => self.parent_copy_clipboard(),
            }
        }
    }

    impl sourceview5::subclass::prelude::ViewImpl for PreviewView {}
}
