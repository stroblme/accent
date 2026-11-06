//! Move, delete, insert, copy and paste pages through a thumbnail strip: a PDF's or a diagram's.
//!
//! The strip is one canvas in a scrolled window, so its page layout determines thumbnail targets.
//! Overlay buttons provide Copy Page, Delete Page, Insert Page Here and Paste Page Here, fading in
//! like Git row actions. A copied page is held for the whole app until the next copy, so it pastes
//! into any strip of its kind.
//! Points are taken in the scrolled window's coordinates, which are the same for a canvas that
//! scrolls itself (`PdfView`) and one a viewport scrolls (the diagram's strip).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use accent_core::page_edit::PageEdit;
use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use crate::doc::Kind;
use crate::pdf::geometry::{Layout, gap_at, gap_middle, move_to, page_at};
use crate::widgets::Hook;

/// How near the strip's top or bottom edge a drag scrolls it, and how fast at the very edge, in
/// pixels a frame.
const EDGE: f64 = 40.0;
const EDGE_SPEED: f64 = 12.0;

/// How far the trash and copy buttons sit inside their thumbnail's corners, and how far apart
/// the insert and paste buttons stand.
const INSET: f32 = 6.0;

/// A page Copy Page holds for Paste Page Here.
pub enum Copied {
    /// The diagram's page as it was.
    Diagram(accent_drawio::Page),
    /// Page `page` of `file`, a copy of the PDF called `name` as it was.
    Pdf {
        file: PathBuf,
        page: usize,
        name: String,
    },
}

impl Copied {
    fn kind(&self) -> Kind {
        match self {
            Copied::Diagram(_) => Kind::Diagram,
            Copied::Pdf { .. } => Kind::Pdf,
        }
    }
}

thread_local! {
    /// What Copy Page holds, one page for every strip of every window.
    static COPIED: RefCell<Option<Rc<Copied>>> = const { RefCell::new(None) };
}

/// Hold `copied` for Paste Page Here, or nothing.
pub fn hold(copied: Option<Copied>) {
    COPIED.set(copied.map(Rc::new));
}

/// What Copy Page holds, if it is a page of `kind`.
fn held(kind: Kind) -> Option<Rc<Copied>> {
    COPIED.with_borrow(|c| c.clone().filter(|c| c.kind() == kind))
}

/// Called with the gap a paste goes into and the page held.
type PasteHook = Hook<dyn Fn(usize, &Copied)>;

/// The strip's overlay and what it knows about the pointer over it.
pub struct Organize {
    /// The strip with the buttons and the drop bar over it: what the Outline pane is handed.
    pub pane: gtk::Overlay,
    strip: gtk::ScrolledWindow,
    /// What the thumbnails are painted on, which the drag starts from and drops onto.
    canvas: gtk::Widget,
    /// Where the pages are, in the scrolled content.
    layout: Box<dyn Fn() -> Layout>,
    /// The picture a drag of a page holds.
    icon: Box<dyn Fn(usize) -> Option<gdk::Texture>>,
    /// The pages a paste here takes.
    kind: Kind,
    on_edit: Hook<dyn Fn(PageEdit)>,
    on_copy: Hook<dyn Fn(usize)>,
    on_paste: PasteHook,
    trash: gtk::Revealer,
    copy: gtk::Revealer,
    /// Insert Page Here, with Paste Page Here beside it while a page of this kind is held.
    insert: gtk::Revealer,
    paste: gtk::Button,
    /// The line in the gap a dragged page would be dropped into.
    bar: gtk::Box,
    /// The page the buttons are over: the one under the pointer, or above the gap it is in.
    hovered: Cell<Option<usize>>,
    /// Where the pointer is over the strip, in the scrolled window's coordinates.
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

/// Round buttons that float over a thumbnail, in a revealer that fades them in and out.
fn floating(child: &impl IsA<gtk::Widget>) -> gtk::Revealer {
    gtk::Revealer::builder()
        .child(child)
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

impl Organize {
    /// The organizer of `canvas`, the pages of `kind` that `layout` places, in `strip`. Every
    /// handler holds it weakly: the widgets they are on outlive nothing it does not.
    pub fn new(
        strip: &gtk::ScrolledWindow,
        canvas: &impl IsA<gtk::Widget>,
        layout: impl Fn() -> Layout + 'static,
        icon: impl Fn(usize) -> Option<gdk::Texture> + 'static,
        kind: Kind,
    ) -> Rc<Organize> {
        // Clipped, so a button over a thumbnail scrolled half out of the strip stays inside it.
        let pane = gtk::Overlay::builder()
            .child(strip)
            .overflow(gtk::Overflow::Hidden)
            .build();
        let o = Rc::new_cyclic(|weak: &Weak<Organize>| {
            // A button that does `act` to the page the buttons are over.
            let button = |icon: &str, tooltip: &str, act: fn(&Organize, usize)| {
                let button = gtk::Button::builder()
                    .icon_name(icon)
                    .tooltip_text(tooltip)
                    .css_classes(["opaque", "accent-floating", "circular"])
                    .build();
                let weak = weak.clone();
                button.connect_clicked(move |_| {
                    if let Some(o) = weak.upgrade()
                        && let Some(page) = o.hovered.get()
                    {
                        act(&o, page);
                    }
                });
                button
            };
            let delete = |o: &Organize, page| o.edit(PageEdit::delete(page));
            let trash = floating(&button("user-trash-symbolic", "Delete Page", delete));
            let copy = floating(&button("edit-copy-symbolic", "Copy Page", Organize::copy));
            let add = |o: &Organize, page| o.edit(PageEdit::insert(page + 1));
            let gap = gtk::Box::new(gtk::Orientation::Horizontal, INSET as i32);
            gap.append(&button("list-add-symbolic", "Insert Page Here", add));
            let paste = button("edit-paste-symbolic", "Paste Page Here", Organize::paste);
            paste.set_visible(false);
            gap.append(&paste);
            let insert = floating(&gap);
            let bar = gtk::Box::builder()
                .height_request(3)
                .can_target(false)
                .visible(false)
                .css_classes(["accent-drop-bar"])
                .build();
            pane.add_overlay(&trash);
            pane.add_overlay(&copy);
            pane.add_overlay(&insert);
            pane.add_overlay(&bar);
            Organize {
                pane,
                strip: strip.clone(),
                canvas: canvas.clone().upcast(),
                layout: Box::new(layout),
                icon: Box::new(icon),
                kind,
                on_edit: Hook::default(),
                on_copy: Hook::default(),
                on_paste: Hook::default(),
                trash,
                copy,
                insert,
                paste,
                bar,
                hovered: Cell::new(None),
                pointer: Cell::new(None),
                dragged: Cell::new(None),
                gap: Cell::new(None),
                speed: Cell::new(0.0),
                ticking: Cell::new(false),
            }
        });
        o.wire();
        o
    }

    /// Called with what a button or a drop asks of the document.
    pub fn connect_edit(&self, f: impl Fn(PageEdit) + 'static) {
        self.on_edit.set(Rc::new(f));
    }

    /// Called with the page Copy Page was pressed on, for the document to [`hold`] it.
    pub fn connect_copy(&self, f: impl Fn(usize) + 'static) {
        self.on_copy.set(Rc::new(f));
    }

    /// Called with the gap Paste Page Here was pressed on, gap `g` being before page `g`, and the
    /// page held, which is of this strip's kind.
    pub fn connect_paste(&self, f: impl Fn(usize, &Copied) + 'static) {
        self.on_paste.set(Rc::new(f));
    }

    /// Copy Page on `page`: the document holds it, and a page held here shows Paste at once.
    fn copy(&self, page: usize) {
        if let Some(f) = self.on_copy.get() {
            f(page);
        }
        self.hover();
    }

    /// Paste Page Here on the gap below `page`.
    fn paste(&self, page: usize) {
        if let (Some(f), Some(copied)) = (self.on_paste.get(), held(self.kind)) {
            f(page + 1, &copied);
        }
    }

    fn edit(&self, edit: PageEdit) {
        if let Some(f) = self.on_edit.get() {
            f(edit);
        }
    }

    /// How far the strip is scrolled, across and down.
    fn scrolled(&self) -> (f64, f64) {
        (
            self.strip.hadjustment().value(),
            self.strip.vadjustment().value(),
        )
    }

    /// A point on `widget` in the scrolled window's coordinates.
    fn on_strip(&self, widget: &impl IsA<gtk::Widget>, x: f64, y: f64) -> (f64, f64) {
        let at = graphene::Point::new(x as f32, y as f32);
        let at = widget.compute_point(&self.strip, &at).unwrap_or(at);
        (f64::from(at.x()), f64::from(at.y()))
    }

    /// Hook the buttons, the pointer and the drag up.
    fn wire(self: &Rc<Self>) {
        // The drop bar says where a page lands; the outline Adwaita draws round a drop target
        // would say only that the whole strip is one.
        self.canvas.add_css_class("accent-page-strip");
        let weak = Rc::downgrade(self);
        self.pane.connect_get_child_position(move |_, child| {
            weak.upgrade().and_then(|o| o.place_on_strip(child))
        });

        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_motion(move |_, x, y| {
            let Some(o) = weak.upgrade() else { return };
            // On one of the buttons, they stay where they are: the one on the gap reaches onto
            // the next page, which would otherwise take them from under the pointer.
            let on_button = (o.pane.pick(x, y, gtk::PickFlags::DEFAULT)).is_some_and(|w| {
                [&o.trash, &o.copy, &o.insert]
                    .into_iter()
                    .any(|buttons| w.is_ancestor(buttons))
            });
            if !on_button {
                o.pointer.set(Some(o.on_strip(&o.pane, x, y).1));
                o.hover();
            }
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(o) = weak.upgrade() {
                o.pointer.set(None);
                o.hover();
            }
        });
        self.pane.add_controller(motion);
        // A wheel under a pointer that stays put brings another page under it.
        let weak = Rc::downgrade(self);
        self.strip.vadjustment().connect_value_changed(move |_| {
            if let Some(o) = weak.upgrade() {
                o.hover();
            }
        });
        self.wire_page_drag();
    }

    /// Move a page dropped into a strip gap. Accept only this strip's drag because page numbers
    /// belong to its document. Indicate no-op drops beside the original position during the
    /// drag.
    fn wire_page_drag(self: &Rc<Self>) {
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        let weak = Rc::downgrade(self);
        source.connect_prepare(move |source, x, y| {
            let o = weak.upgrade()?;
            let (x, y) = o.on_strip(&o.canvas, x, y);
            let (left, top) = o.scrolled();
            let (cx, cy) = ((x + left) as f32, (y + top) as f32);
            let layout = (o.layout)();
            let (page, rect) = layout.pages.iter().enumerate().find(|(_, r)| {
                (r.x..=r.x + r.w).contains(&cx) && (r.y..=r.y + r.h).contains(&cy)
            })?;
            // The page itself under the pointer, held where it was taken hold of.
            if let Some(icon) = (o.icon)(page) {
                let grip = |at: f32, of: f32, size: i32| (at / of * size as f32) as i32;
                let gx = grip(cx - rect.x, rect.w, icon.width());
                let gy = grip(cy - rect.y, rect.h, icon.height());
                source.set_icon(Some(&icon), gx, gy);
            }
            o.dragged.set(Some(page));
            o.hover();
            Some(gdk::ContentProvider::for_value(&(page as u32).to_value()))
        });
        let weak = Rc::downgrade(self);
        source.connect_drag_end(move |_, _, _| {
            if let Some(o) = weak.upgrade() {
                o.dragged.set(None);
                o.aim(None);
                o.hover();
            }
        });
        self.canvas.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = Rc::downgrade(self);
        target
            .connect_accept(move |_, _| weak.upgrade().is_some_and(|o| o.dragged.get().is_some()));
        let aim = |weak: Weak<Organize>| {
            move |_: &gtk::DropTarget, x: f64, y: f64| match weak.upgrade() {
                Some(o) => o.aim(Some(o.on_strip(&o.canvas, x, y).1)),
                None => gdk::DragAction::empty(),
            }
        };
        target.connect_enter(aim(Rc::downgrade(self)));
        target.connect_motion(aim(Rc::downgrade(self)));
        let weak = Rc::downgrade(self);
        target.connect_leave(move |_| {
            if let Some(o) = weak.upgrade() {
                o.aim(None);
            }
        });
        let weak = Rc::downgrade(self);
        target.connect_drop(move |_, value, x, y| {
            let (Some(o), Ok(from)) = (weak.upgrade(), value.get::<u32>()) else {
                return false;
            };
            let y = o.on_strip(&o.canvas, x, y).1 + o.scrolled().1;
            let from = from as usize;
            let Some(to) = move_to(from, gap_at(&(o.layout)(), y)) else {
                return false;
            };
            o.aim(None);
            o.edit(PageEdit::Move { from, to });
            true
        });
        self.canvas.add_controller(target);
    }

    /// Where the strip and its insert, copy and paste buttons are in `window`, and whether each
    /// button is out: what the strip drills print for XTEST to aim at.
    #[cfg(feature = "bench")]
    pub fn buttons(&self, window: &gtk::Widget) -> String {
        let at = |w: &gtk::Widget| {
            w.compute_bounds(window).map(|r| {
                let (x, y) = (r.x().round(), r.y().round());
                format!("{x},{y},{},{}", r.width().round(), r.height().round())
            })
        };
        format!(
            "strip={:?} insert={:?} copy={:?} paste={:?} out=trash:{},insert:{},copy:{},paste:{}",
            at(self.pane.upcast_ref()),
            at(self.insert.upcast_ref()),
            at(self.copy.upcast_ref()),
            at(self.paste.upcast_ref()),
            self.trash.reveals_child(),
            self.insert.reveals_child(),
            self.copy.reveals_child(),
            self.paste.is_visible()
        )
    }

    /// Show the buttons over the page under the pointer, or put them away: with the pointer gone,
    /// while a page is being dragged, the trash on a document of one page, which keeps it, and
    /// Paste while no page of this kind is held.
    pub fn hover(&self) {
        let layout = (self.layout)();
        let top = self.scrolled().1;
        let page = self
            .pointer
            .get()
            .filter(|_| self.dragged.get().is_none())
            .map(|y| y + top)
            .filter(|y| *y < f64::from(layout.height))
            .map(|y| page_at(&layout, y))
            .filter(|page| *page < layout.pages.len());
        if page.is_some() {
            self.hovered.set(page);
        }
        reveal(&self.trash, page.is_some() && layout.pages.len() > 1);
        reveal(&self.copy, page.is_some());
        reveal(&self.insert, page.is_some());
        self.paste.set_visible(held(self.kind).is_some());
        self.pane.queue_allocate();
    }

    /// A drag of one of the strip's pages is at `y` over it, or has left it: show where the page
    /// would land, scroll while the drag is near an edge, and say whether a drop here moves it.
    fn aim(self: &Rc<Self>, y: Option<f64>) -> gdk::DragAction {
        let layout = (self.layout)();
        let top = self.scrolled().1;
        let gap = y.zip(self.dragged.get()).and_then(|(y, from)| {
            let gap = gap_at(&layout, y + top);
            move_to(from, gap).map(|_| gap)
        });
        self.pointer.set(y);
        self.gap.set(gap);
        self.bar.set_visible(gap.is_some());
        self.pane.queue_allocate();
        let height = f64::from(self.strip.height());
        self.speed.set(y.map_or(0.0, |y| edge_speed(y, height)));
        if self.speed.get() != 0.0 {
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
        if self.ticking.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.canvas.add_tick_callback(move |_, _| {
            let Some(o) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if o.speed.get() == 0.0 || o.dragged.get().is_none() {
                o.ticking.set(false);
                return glib::ControlFlow::Break;
            }
            let adjustment = o.strip.vadjustment();
            adjustment.set_value(adjustment.value() + o.speed.get());
            // The pointer stayed put and the pages moved under it.
            o.aim(o.pointer.get());
            glib::ControlFlow::Continue
        });
    }

    /// Where one of the overlay's floating children goes, in the overlay's coordinates: the trash
    /// in the hovered thumbnail's top-right corner and Copy in its top-left, the insert and paste
    /// buttons centred on the gap below it, and the bar across the gap a drop would land in.
    fn place_on_strip(&self, child: &gtk::Widget) -> Option<gdk::Rectangle> {
        let layout = (self.layout)();
        let origin = graphene::Point::zero();
        let origin = self.strip.compute_point(&self.pane, &origin)?;
        let (left, top) = self.scrolled();
        // From the scrolled content into the overlay.
        let (left, top) = (left as f32 - origin.x(), top as f32 - origin.y());
        let (_, natural) = child.preferred_size();
        let (w, h) = (natural.width() as f32, natural.height() as f32);
        let (x, y, w) = if child == self.bar.upcast_ref::<gtk::Widget>() {
            let gap = self.gap.get()?;
            let beside = layout
                .pages
                .get(gap)
                .or_else(|| layout.pages.get(gap.checked_sub(1)?))?;
            (beside.x, gap_middle(&layout, gap) - top - h / 2.0, beside.w)
        } else {
            let page = self.hovered.get()?;
            let rect = layout.pages.get(page)?;
            let left = child == self.copy.upcast_ref::<gtk::Widget>();
            match left || child == self.trash.upcast_ref::<gtk::Widget>() {
                // In the corner of what shows of the thumbnail, so a page scrolled half out of
                // the strip keeps its trash and its copy button in reach.
                true => {
                    let shown = (rect.y - top).max(origin.y());
                    let y = (shown + INSET).min(rect.y + rect.h - top - h - INSET);
                    let x = match left {
                        true => rect.x + INSET,
                        false => rect.x + rect.w - w - INSET,
                    };
                    (x, y, w)
                }
                false => (
                    rect.x + (rect.w - w) / 2.0,
                    gap_middle(&layout, page + 1) - top - h / 2.0,
                    w,
                ),
            }
        };
        let x = x - left;
        Some(gdk::Rectangle::new(
            x.round() as i32,
            y.round() as i32,
            w.round() as i32,
            h.round() as i32,
        ))
    }
}
