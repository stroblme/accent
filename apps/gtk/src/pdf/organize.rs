//! Organising a document's pages from the thumbnail strip: a thumbnail dragged along it moves its
//! page, and the thumbnail under the pointer carries two buttons — Delete Page at its corner and
//! Insert Page Here on the gap below it.
//!
//! They float over the strip in an overlay, which is what the Outline pane is handed. The strip
//! is one canvas (`PdfView`), so a thumbnail is a rectangle of it rather than a widget, and what
//! is over which page is worked out from its layout (`PdfView::placement`). The buttons fade in
//! the way the Git pane's rows reveal theirs.

use std::cell::Cell;
use std::rc::Rc;

use accent_core::pdf::PageEdit;
use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use super::geometry::{gap_at, gap_middle, move_to, page_at};
use super::tab::PdfTab;

/// How near the strip's top or bottom edge a drag scrolls it, and how fast at the very edge, in
/// pixels a frame.
const EDGE: f64 = 40.0;
const EDGE_SPEED: f64 = 12.0;

/// How far the trash button sits inside its thumbnail's corner.
const INSET: f32 = 6.0;

/// The strip's overlay and what it knows about the pointer over it.
pub(super) struct Organize {
    /// The strip with the buttons and the drop bar over it.
    pub(super) pane: gtk::Overlay,
    trash: gtk::Revealer,
    insert: gtk::Revealer,
    /// The line in the gap a dragged page would be dropped into.
    bar: gtk::Box,
    /// The page the buttons are over: the one under the pointer, or above the gap it is in.
    hovered: Cell<Option<usize>>,
    /// Where the pointer is over the strip, in the strip's own coordinates.
    pointer: Cell<Option<f64>>,
    /// The page being dragged out of this strip, which is the only drag it takes a drop from.
    dragged: Cell<Option<usize>>,
    /// The gap a drop would put the dragged page into, where the bar shows. `None` where the
    /// drop would leave the page where it is.
    gap: Cell<Option<usize>>,
    /// How fast a drag near an edge is scrolling the strip, and whether a tick is doing it.
    speed: Cell<f64>,
    ticking: Cell<bool>,
}

impl Organize {
    pub(super) fn new(strip: &gtk::ScrolledWindow) -> Organize {
        // Clipped, so a button over a thumbnail scrolled half out of the strip stays inside it.
        let pane = gtk::Overlay::builder()
            .child(strip)
            .overflow(gtk::Overflow::Hidden)
            .build();
        let trash = floating("user-trash-symbolic", "Delete Page");
        let insert = floating("list-add-symbolic", "Insert Page Here");
        let bar = gtk::Box::builder()
            .height_request(3)
            .can_target(false)
            .visible(false)
            .css_classes(["accent-drop-bar"])
            .build();
        pane.add_overlay(&trash);
        pane.add_overlay(&insert);
        pane.add_overlay(&bar);
        Organize {
            pane,
            trash,
            insert,
            bar,
            hovered: Cell::new(None),
            pointer: Cell::new(None),
            dragged: Cell::new(None),
            gap: Cell::new(None),
            speed: Cell::new(0.0),
            ticking: Cell::new(false),
        }
    }
}

/// A round button that floats over a thumbnail, in a revealer that fades it in and out.
fn floating(icon: &str, tooltip: &str) -> gtk::Revealer {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .css_classes(["osd", "circular"])
        .build();
    gtk::Revealer::builder()
        .child(&button)
        .transition_type(gtk::RevealerTransitionType::Crossfade)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .can_target(false)
        .build()
}

/// Show a floating button or put it away. A revealer takes the pointer over its whole box whether
/// its child shows or not, so one going away lets the pointer through to the strip under it.
fn reveal(revealer: &gtk::Revealer, on: bool) {
    revealer.set_reveal_child(on);
    revealer.set_can_target(on);
}

/// How fast a drag at `y` over a strip `height` tall scrolls it, in pixels a frame: nothing away
/// from the edges, up to [`EDGE_SPEED`] at them, negative towards the top.
fn edge_speed(y: f64, height: f64) -> f64 {
    let into = |edge: f64| (EDGE - edge).clamp(0.0, EDGE) / EDGE * EDGE_SPEED;
    into(height - y) - into(y)
}

impl PdfTab {
    /// Hook the buttons, the pointer and the drag up. Every handler holds the tab weakly: the
    /// widgets they are on are the tab's.
    pub(super) fn wire_organize(self: &Rc<Self>) {
        let o = &self.organize;
        // The drop bar says where a page lands; the outline Adwaita draws round a drop target
        // would say only that the whole strip is one.
        self.thumbs.add_css_class("accent-page-strip");
        for (revealer, delete) in [(&o.trash, true), (&o.insert, false)] {
            let Some(button) = revealer.child().and_downcast::<gtk::Button>() else {
                continue;
            };
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move |_| {
                    let Some(page) = tab.organize.hovered.get() else {
                        return;
                    };
                    match delete {
                        true => tab.edit_pages(PageEdit::Delete(page)),
                        false => tab.edit_pages(PageEdit::Insert(page + 1)),
                    }
                }
            ));
        }
        o.pane.connect_get_child_position(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            None,
            move |_, child| tab.place_on_strip(child)
        ));

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, x, y| {
                // On one of the buttons, they stay where they are: the one on the gap reaches
                // onto the next page, which would otherwise take them from under the pointer.
                let o = &tab.organize;
                let on_button = o
                    .pane
                    .pick(x, y, gtk::PickFlags::DEFAULT)
                    .is_some_and(|w| w.is_ancestor(&o.trash) || w.is_ancestor(&o.insert));
                if !on_button {
                    let at = graphene::Point::new(x as f32, y as f32);
                    let at = o.pane.compute_point(&tab.thumbs, &at).unwrap_or(at);
                    o.pointer.set(Some(f64::from(at.y())));
                    tab.hover_thumbnail();
                }
            }
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| {
                tab.organize.pointer.set(None);
                tab.hover_thumbnail();
            }
        ));
        o.pane.add_controller(motion);
        // A wheel under a pointer that stays put brings another page under it.
        self.thumb_strip
            .vadjustment()
            .connect_value_changed(glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move |_| tab.hover_thumbnail()
            ));
        self.wire_page_drag();
    }

    /// A thumbnail dragged along the strip and dropped into a gap moves its page there.
    ///
    /// The drag carries the page's number, and the strip takes a drop only of its own drag: the
    /// number is nothing to another document. A drop beside where the page already is moves
    /// nothing, and says so while the drag is still over it.
    fn wire_page_drag(self: &Rc<Self>) {
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        source.connect_prepare(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            None,
            move |source, x, y| {
                let (page, px, py) = tab.thumbs.page_point(x, y)?;
                let (w, h) = tab.thumbs.page_size(page)?;
                // The page itself under the pointer, held where it was taken hold of.
                let low = tab
                    .thumbs
                    .cache()
                    .borrow_mut()
                    .lowres(page as u32, tab.thumbs.dark());
                if let Some(low) = low {
                    let grip = |at: f32, of: f32, size: i32| (at / of * size as f32) as i32;
                    let (gx, gy) = (grip(px, w, low.width()), grip(py, h, low.height()));
                    source.set_icon(Some(&low), gx, gy);
                }
                tab.organize.dragged.set(Some(page));
                tab.hover_thumbnail();
                Some(gdk::ContentProvider::for_value(&(page as u32).to_value()))
            }
        ));
        source.connect_drag_end(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, _, _| {
                tab.organize.dragged.set(None);
                tab.aim(None);
                tab.hover_thumbnail();
            }
        ));
        self.thumbs.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        target.connect_accept(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            false,
            move |_, _| tab.organize.dragged.get().is_some()
        ));
        let aim = glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |_: &gtk::DropTarget, _: f64, y: f64| tab.aim(Some(y))
        );
        target.connect_enter(aim.clone());
        target.connect_motion(aim);
        target.connect_leave(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| {
                tab.aim(None);
            }
        ));
        target.connect_drop(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, y| {
                let Ok(from) = value.get::<u32>() else {
                    return false;
                };
                let (layout, top) = tab.thumbs.placement();
                let from = from as usize;
                let Some(to) = move_to(from, gap_at(&layout, y + top)) else {
                    return false;
                };
                tab.aim(None);
                tab.edit_pages(PageEdit::Move { from, to });
                true
            }
        ));
        self.thumbs.add_controller(target);
    }

    /// Show the buttons over the page under the pointer, or put them away: with the pointer gone,
    /// while a page is being dragged, and the trash on a document of one page, which keeps it.
    pub(super) fn hover_thumbnail(&self) {
        let o = &self.organize;
        let (layout, top) = self.thumbs.placement();
        let page = o
            .pointer
            .get()
            .filter(|_| o.dragged.get().is_none())
            .map(|y| y + top)
            .filter(|y| *y < f64::from(layout.height))
            .map(|y| page_at(&layout, y))
            .filter(|page| *page < layout.pages.len());
        if page.is_some() {
            o.hovered.set(page);
        }
        reveal(&o.trash, page.is_some() && layout.pages.len() > 1);
        reveal(&o.insert, page.is_some());
        o.pane.queue_allocate();
    }

    /// A drag of one of the strip's pages is at `y` over it, or has left it: show where the page
    /// would land, scroll while the drag is near an edge, and say whether a drop here moves it.
    fn aim(self: &Rc<Self>, y: Option<f64>) -> gdk::DragAction {
        let o = &self.organize;
        let (layout, top) = self.thumbs.placement();
        let gap = y.zip(o.dragged.get()).and_then(|(y, from)| {
            let gap = gap_at(&layout, y + top);
            move_to(from, gap).map(|_| gap)
        });
        o.pointer.set(y);
        o.gap.set(gap);
        o.bar.set_visible(gap.is_some());
        o.pane.queue_allocate();
        let height = f64::from(self.thumbs.height());
        o.speed.set(y.map_or(0.0, |y| edge_speed(y, height)));
        if o.speed.get() != 0.0 {
            self.autoscroll();
        }
        match gap {
            Some(_) => gdk::DragAction::MOVE,
            None => gdk::DragAction::empty(),
        }
    }

    /// Scroll the strip a frame at a time while a drag rests near one of its edges: GTK does not,
    /// and a page is otherwise only ever dropped where the strip already shows.
    fn autoscroll(self: &Rc<Self>) {
        if self.organize.ticking.replace(true) {
            return;
        }
        self.thumbs.add_tick_callback(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move |_, _| {
                let o = &tab.organize;
                if o.speed.get() == 0.0 || o.dragged.get().is_none() {
                    o.ticking.set(false);
                    return glib::ControlFlow::Break;
                }
                let adjustment = tab.thumb_strip.vadjustment();
                adjustment.set_value(adjustment.value() + o.speed.get());
                // The pointer stayed put and the pages moved under it.
                tab.aim(o.pointer.get());
                glib::ControlFlow::Continue
            }
        ));
    }

    /// Where one of the overlay's floating children goes, in the overlay's coordinates: the trash
    /// in the hovered thumbnail's top-right corner, the insert button centred on the gap below it,
    /// and the bar across the gap a drop would land in.
    fn place_on_strip(&self, child: &gtk::Widget) -> Option<gdk::Rectangle> {
        let o = &self.organize;
        let (layout, top) = self.thumbs.placement();
        let origin = graphene::Point::zero();
        let origin = self.thumbs.compute_point(&o.pane, &origin)?;
        let top = top as f32 - origin.y();
        let (_, natural) = child.preferred_size();
        let (w, h) = (natural.width() as f32, natural.height() as f32);
        let (x, y, w) = if child == o.bar.upcast_ref::<gtk::Widget>() {
            let gap = o.gap.get()?;
            let beside = layout
                .pages
                .get(gap)
                .or_else(|| layout.pages.get(gap.checked_sub(1)?))?;
            (beside.x, gap_middle(&layout, gap) - top - h / 2.0, beside.w)
        } else {
            let page = o.hovered.get()?;
            let rect = layout.pages.get(page)?;
            match child == o.trash.upcast_ref::<gtk::Widget>() {
                // In the corner of what shows of the thumbnail, so a page scrolled half out of
                // the strip keeps its trash in reach.
                true => {
                    let shown = (rect.y - top).max(origin.y());
                    let y = (shown + INSET).min(rect.y + rect.h - top - h - INSET);
                    (rect.x + rect.w - w - INSET, y, w)
                }
                false => (
                    rect.x + (rect.w - w) / 2.0,
                    gap_middle(&layout, page + 1) - top - h / 2.0,
                    w,
                ),
            }
        };
        let x = x + origin.x();
        Some(gdk::Rectangle::new(
            x.round() as i32,
            y.round() as i32,
            w.round() as i32,
            h.round() as i32,
        ))
    }
}
