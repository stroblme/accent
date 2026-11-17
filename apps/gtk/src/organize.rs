//! Move, delete, insert, copy and paste pages through a thumbnail strip: a PDF's or a diagram's.
//!
//! The strip is one canvas in a scrolled window, so its page layout determines thumbnail targets.
//! Overlay buttons provide Copy Page, Delete Page, Insert Page Here and Paste Page Here, fading in
//! like Git row actions, beside the page's number. A copied page is held for the whole app until
//! the next copy, so it pastes into any strip of its kind.
//! Ctrl+click and Shift+click pick several pages, which the buttons and a drag on one of them
//! take together, as one edit.
//! Points are taken in the scrolled window's coordinates, which are the same for a canvas that
//! scrolls itself (`PdfView`) and one a viewport scrolls (the diagram's strip).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use accent_core::page_edit::PageEdit;
use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use crate::doc::Kind;
use crate::pdf::geometry::{Layout, PageRect, gap_at, gap_middle, page_at};
use crate::widgets::Hook;

/// How near the strip's top or bottom edge a drag scrolls it, and how fast at the very edge, in
/// pixels a frame.
const EDGE: f64 = 40.0;
const EDGE_SPEED: f64 = 12.0;

/// How far apart the insert and paste buttons stand.
const INSET: f32 = 6.0;

/// How round a thumbnail's corners are: a round button's radius (libadwaita's `.circular` is
/// 34 px), so the copy and trash buttons each sit in their corner, centred where its arc is.
pub const CORNER: f32 = 17.0;

/// The pages Copy Page holds for Paste Page Here, in their order.
pub enum Copied {
    /// The diagram's pages as they were.
    Diagram(Vec<accent_drawio::Page>),
    /// The pages `pages` of `file`, a copy of the PDF called `name` as it was.
    Pdf {
        file: PathBuf,
        pages: Vec<usize>,
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

    fn count(&self) -> usize {
        match self {
            Copied::Diagram(pages) => pages.len(),
            Copied::Pdf { pages, .. } => pages.len(),
        }
    }
}

/// What the organizer asks of the canvas a strip's thumbnails are painted on, and tells it.
pub trait Thumbnails {
    /// Where the pages are, in the scrolled content.
    fn layout(&self) -> Layout;
    /// The picture a drag of `page` holds.
    fn picture(&self, page: usize) -> Option<gdk::Texture>;
    /// The page the document shows, which the strip frames while no page is picked.
    fn shown(&self) -> usize;
    /// Frame `pages`, in order, instead, or the page shown again for none.
    fn set_picked(&self, pages: &[usize]);
}

/// Whether a strip frames `page`: it is one of the pages `picked`, or with none picked the page
/// `shown`.
pub fn framed(picked: &[usize], shown: usize, page: usize) -> bool {
    match picked.is_empty() {
        true => page == shown,
        false => picked.contains(&page),
    }
}

/// The pages Ctrl+click and Shift+click picked in a strip, in order: none while the page shown
/// stands alone, as a plain click leaves it.
#[derive(Debug, Default, PartialEq)]
struct Picked {
    pages: Vec<usize>,
    /// Where Shift+click picks from: the page last Ctrl+clicked, else the page shown.
    anchor: Option<usize>,
}

impl Picked {
    /// Ctrl+click on `page`, the document showing `shown`: the page clicked joins the pages
    /// picked, the page shown first among them, or leaves them.
    fn toggle(&mut self, page: usize, shown: usize) {
        if self.pages.is_empty() {
            self.pages.push(shown);
        }
        match self.pages.binary_search(&page) {
            Ok(i) => {
                self.pages.remove(i);
            }
            Err(i) => self.pages.insert(i, page),
        }
        self.anchor = Some(page);
        if self.pages.is_empty() {
            *self = Picked::default();
        }
    }

    /// Shift+click on `page`: every page from the anchor to it is picked, and only those.
    fn extend(&mut self, page: usize, shown: usize) {
        let anchor = *self.anchor.get_or_insert(shown);
        self.pages = (anchor.min(page)..=anchor.max(page)).collect();
    }

    /// The pages a button or a drag on `page` takes: the pages picked if it is one of them, else
    /// it alone.
    fn taking(&self, page: usize) -> Vec<usize> {
        match self.pages.contains(&page) {
            true => self.pages.clone(),
            false => vec![page],
        }
    }

    /// Follow a page edit, `went` saying where each page went, `None` for one taken out.
    fn repage(&mut self, went: impl Fn(usize) -> Option<usize>) {
        self.pages = self.pages.iter().filter_map(|&page| went(page)).collect();
        self.pages.sort_unstable();
        self.anchor = self.anchor.and_then(&went);
        if self.pages.is_empty() {
            *self = Picked::default();
        }
    }
}

/// A button's tooltip for `count` pages: "Delete Page", or "Delete 3 Pages".
fn tooltip(verb: &str, count: usize, after: &str) -> String {
    match count {
        1 => format!("{verb} Page{after}"),
        n => format!("{verb} {n} Pages{after}"),
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
    /// What the thumbnails are painted on, which a click lands on and a drag starts from and
    /// drops onto.
    canvas: gtk::Widget,
    /// The same canvas, which places the thumbnails and frames them.
    thumbs: Box<dyn Thumbnails>,
    /// The pages a paste here takes.
    kind: Kind,
    /// What the document's keys reach, which a button or a drop hands the keyboard to, so
    /// `Ctrl+Z` takes back what the strip just did.
    keys: gtk::Widget,
    on_goto: Hook<dyn Fn(usize)>,
    on_edit: Hook<dyn Fn(Vec<PageEdit>)>,
    on_copy: Hook<dyn Fn(Vec<usize>)>,
    on_paste: PasteHook,
    trash: gtk::Revealer,
    copy: gtk::Revealer,
    /// Insert Page Here, with Paste Page Here before it while a page of this kind is held.
    insert: gtk::Revealer,
    paste: gtk::Button,
    /// The page's number, which takes no click: the thumbnail under it does.
    number: gtk::Revealer,
    /// The line in the gap a dragged page would be dropped into.
    bar: gtk::Box,
    /// The page the buttons are over: the one under the pointer, or above the gap it is in.
    hovered: Cell<Option<usize>>,
    /// Where the pointer is over the strip, in the scrolled window's coordinates.
    pointer: Cell<Option<f64>>,
    picked: RefCell<Picked>,
    /// The pages being dragged out of this strip, which is the only drag it takes a drop from;
    /// none while no drag is.
    dragged: RefCell<Vec<usize>>,
    /// The gap a drop would put the dragged pages into, where the bar shows. `None` where the
    /// drop would leave them where they are.
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
    /// The organizer of `canvas`, the thumbnails of pages of `kind`, in `strip`, for the document
    /// whose keys `keys` takes. Every handler holds it weakly: the widgets they are on outlive
    /// nothing it does not.
    pub fn new<T: IsA<gtk::Widget> + Thumbnails>(
        strip: &gtk::ScrolledWindow,
        canvas: &T,
        kind: Kind,
        keys: &impl IsA<gtk::Widget>,
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
                        o.keys.grab_focus();
                    }
                });
                button
            };
            let delete = |o: &Organize, page| o.edit(PageEdit::deletes(&o.taking(page)));
            let trash = floating(&button("user-trash-symbolic", "Delete Page", delete));
            let copy = floating(&button("edit-copy-symbolic", "Copy Page", Organize::copy));
            let add = |o: &Organize, page| o.edit(vec![PageEdit::insert(page + 1)]);
            let gap = gtk::Box::new(gtk::Orientation::Horizontal, INSET as i32);
            let paste = button("edit-paste-symbolic", "Paste Page Here", Organize::paste);
            paste.set_visible(false);
            gap.append(&paste);
            gap.append(&button("list-add-symbolic", "Insert Page Here", add));
            let insert = floating(&gap);
            let number = floating(
                &gtk::Label::builder()
                    .css_classes([
                        "caption",
                        "numeric",
                        "accent-floating",
                        "accent-page-number",
                    ])
                    .build(),
            );
            let bar = gtk::Box::builder()
                .height_request(3)
                .can_target(false)
                .visible(false)
                .css_classes(["accent-drop-bar"])
                .build();
            pane.add_overlay(&trash);
            pane.add_overlay(&copy);
            pane.add_overlay(&insert);
            pane.add_overlay(&number);
            pane.add_overlay(&bar);
            Organize {
                pane,
                strip: strip.clone(),
                canvas: canvas.clone().upcast(),
                thumbs: Box::new(canvas.clone()),
                kind,
                keys: keys.clone().upcast(),
                on_goto: Hook::default(),
                on_edit: Hook::default(),
                on_copy: Hook::default(),
                on_paste: Hook::default(),
                trash,
                copy,
                insert,
                paste,
                number,
                bar,
                hovered: Cell::new(None),
                pointer: Cell::new(None),
                picked: RefCell::default(),
                dragged: RefCell::default(),
                gap: Cell::new(None),
                speed: Cell::new(0.0),
                ticking: Cell::new(false),
            }
        });
        o.wire();
        o
    }

    /// Called with the page a plain click on its thumbnail goes to.
    pub fn connect_goto(&self, f: impl Fn(usize) + 'static) {
        self.on_goto.set(Rc::new(f));
    }

    /// Called with what a button or a drop asks of the document: edits made one after another,
    /// as one step.
    pub fn connect_edit(&self, f: impl Fn(Vec<PageEdit>) + 'static) {
        self.on_edit.set(Rc::new(f));
    }

    /// Called with the pages Copy Page was pressed on, in order, for the document to [`hold`]
    /// them.
    pub fn connect_copy(&self, f: impl Fn(Vec<usize>) + 'static) {
        self.on_copy.set(Rc::new(f));
    }

    /// Called with the gap Paste Page Here was pressed on, gap `g` being before page `g`, and the
    /// pages held, which are of this strip's kind.
    pub fn connect_paste(&self, f: impl Fn(usize, &Copied) + 'static) {
        self.on_paste.set(Rc::new(f));
    }

    /// The pages picked follow a page edit, `went` saying where each page went, `None` for one
    /// taken out.
    pub fn repage(&self, went: impl Fn(usize) -> Option<usize>) {
        self.picked.borrow_mut().repage(went);
        self.thumbs.set_picked(&self.picked.borrow().pages);
    }

    /// Let go of the pages picked: the document was read again.
    pub fn unpick(&self) {
        self.repage(|_| None);
    }

    /// The pages a button or a drag on `page` takes.
    fn taking(&self, page: usize) -> Vec<usize> {
        self.picked.borrow().taking(page)
    }

    /// A click on the thumbnail of `page` with `state`'s modifiers: Shift picks the pages from the
    /// anchor to it, Ctrl ([`crate::platform::PRIMARY_MODIFIER`]) picks it or lets it go, and a
    /// plain click lets every page go and goes to it.
    fn click(&self, page: usize, state: gdk::ModifierType) {
        let shown = self.thumbs.shown();
        if state.contains(gdk::ModifierType::SHIFT_MASK) {
            self.picked.borrow_mut().extend(page, shown);
        } else if state.contains(crate::platform::PRIMARY_MODIFIER) {
            self.picked.borrow_mut().toggle(page, shown);
        } else {
            *self.picked.borrow_mut() = Picked::default();
            if let Some(f) = self.on_goto.get() {
                f(page);
            }
        }
        self.thumbs.set_picked(&self.picked.borrow().pages);
        self.hover();
    }

    /// Copy Page on `page`: the document holds the pages it takes, and pages held here show Paste
    /// at once.
    fn copy(&self, page: usize) {
        if let Some(f) = self.on_copy.get() {
            f(self.taking(page));
        }
        self.hover();
    }

    /// Paste Page Here on the gap below `page`.
    fn paste(&self, page: usize) {
        if let (Some(f), Some(copied)) = (self.on_paste.get(), held(self.kind)) {
            f(page + 1, &copied);
        }
    }

    fn edit(&self, edits: Vec<PageEdit>) {
        if let Some(f) = self.on_edit.get() {
            f(edits);
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

    /// The page whose thumbnail holds a point of `widget`, where that thumbnail is, and the
    /// point, all in the scrolled content.
    fn page_under(
        &self,
        widget: &gtk::Widget,
        x: f64,
        y: f64,
    ) -> Option<(usize, PageRect, f32, f32)> {
        let (x, y) = self.on_strip(widget, x, y);
        let (left, top) = self.scrolled();
        let (x, y) = ((x + left) as f32, (y + top) as f32);
        let layout = self.thumbs.layout();
        let found = layout
            .pages
            .iter()
            .enumerate()
            .find(|(_, r)| (r.x..=r.x + r.w).contains(&x) && (r.y..=r.y + r.h).contains(&y));
        found.map(|(page, rect)| (page, *rect, x, y))
    }

    /// Hook the buttons, the pointer, a click and the drag up.
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
        // A thumbnail is a button: a click goes to its page or picks it, on the release, because a
        // press is also how a drag that moves the page begins. A drag takes the sequence, so it
        // never gets here.
        let click = gtk::GestureClick::builder().button(1).build();
        let weak = Rc::downgrade(self);
        click.connect_released(move |click, _, x, y| {
            if let Some(o) = weak.upgrade()
                && let Some((page, ..)) = o.page_under(&o.canvas, x, y)
            {
                o.click(page, click.current_event_state());
            }
        });
        self.canvas.add_controller(click);
        self.wire_wheel();
        // A wheel under a pointer that stays put brings another page under it.
        let weak = Rc::downgrade(self);
        self.strip.vadjustment().connect_value_changed(move |_| {
            if let Some(o) = weak.upgrade() {
                o.hover();
            }
        });
        self.wire_page_drag();
    }

    /// Ease a wheel's notches over the strip, and scroll it by a wheel over the buttons too, as
    /// beside them: they float over the strip, outside its scrolled window, which hears nothing of
    /// a scroll on them. A touchpad's pixels move it as they come.
    fn wire_wheel(&self) {
        let wheel = crate::widgets::smooth_wheel(&self.strip);
        for buttons in [&self.trash, &self.copy, &self.insert] {
            let flags = gtk::EventControllerScrollFlags::VERTICAL;
            let scroll = gtk::EventControllerScroll::new(flags);
            let (wheel, strip) = (wheel.clone(), self.strip.downgrade());
            scroll.connect_scroll(move |scroll, _, dy| {
                if scroll.unit() == gdk::ScrollUnit::Wheel {
                    wheel.notch(dy);
                } else if let Some(strip) = strip.upgrade() {
                    let adjustment = strip.vadjustment();
                    adjustment.set_value(adjustment.value() + dy);
                }
                glib::Propagation::Stop
            });
            buttons.add_controller(scroll);
        }
    }

    /// Move the pages dropped into a strip gap: the page taken hold of, or the pages picked with
    /// it, as one block. Accept only this strip's drag because page numbers belong to its
    /// document. Indicate no-op drops beside the original position during the drag.
    fn wire_page_drag(self: &Rc<Self>) {
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::MOVE)
            .build();
        let weak = Rc::downgrade(self);
        source.connect_prepare(move |source, x, y| {
            let o = weak.upgrade()?;
            let (page, rect, cx, cy) = o.page_under(&o.canvas, x, y)?;
            // The page itself under the pointer, held where it was taken hold of.
            if let Some(icon) = o.thumbs.picture(page) {
                let grip = |at: f32, of: f32, size: i32| (at / of * size as f32) as i32;
                let gx = grip(cx - rect.x, rect.w, icon.width());
                let gy = grip(cy - rect.y, rect.h, icon.height());
                source.set_icon(Some(&icon), gx, gy);
            }
            *o.dragged.borrow_mut() = o.taking(page);
            o.hover();
            Some(gdk::ContentProvider::for_value(&(page as u32).to_value()))
        });
        let weak = Rc::downgrade(self);
        source.connect_drag_end(move |_, _, _| {
            if let Some(o) = weak.upgrade() {
                o.dragged.borrow_mut().clear();
                o.aim(None);
                o.hover();
            }
        });
        self.canvas.add_controller(source);

        let target = gtk::DropTarget::new(u32::static_type(), gdk::DragAction::MOVE);
        let weak = Rc::downgrade(self);
        target.connect_accept(move |_, _| {
            weak.upgrade()
                .is_some_and(|o| !o.dragged.borrow().is_empty())
        });
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
        target.connect_drop(move |_, _, x, y| {
            let Some(o) = weak.upgrade() else {
                return false;
            };
            let y = o.on_strip(&o.canvas, x, y).1 + o.scrolled().1;
            let gap = gap_at(&o.thumbs.layout(), y);
            let moves = PageEdit::moves(&o.dragged.borrow(), gap);
            if moves.is_empty() {
                return false;
            }
            o.aim(None);
            o.edit(moves);
            o.keys.grab_focus();
            true
        });
        self.canvas.add_controller(target);
    }

    /// Where the strip, its insert, copy and paste buttons and its thumbnails' middles are in
    /// `window`, whether each button is out and which pages are picked: what the strip drills
    /// print for XTEST to aim at.
    #[cfg(feature = "bench")]
    pub fn buttons(&self, window: &gtk::Widget) -> String {
        let at = |w: &gtk::Widget| {
            w.compute_bounds(window).map(|r| {
                let (x, y) = (r.x().round(), r.y().round());
                format!("{x},{y},{},{}", r.width().round(), r.height().round())
            })
        };
        let origin = self.strip.compute_point(window, &graphene::Point::zero());
        let (left, top) = self.scrolled();
        let middles: Vec<String> = (self.thumbs.layout().pages.iter())
            .filter_map(|r| {
                let x = origin?.x() + r.x + r.w / 2.0 - left as f32;
                let y = origin?.y() + r.y + r.h / 2.0 - top as f32;
                Some(format!("{},{}", x.round(), y.round()))
            })
            .collect();
        let number = (self.number.child().and_downcast::<gtk::Label>())
            .filter(|_| self.number.reveals_child())
            .map(|label| label.text());
        format!(
            "strip={:?} insert={:?} copy={:?} paste={:?} number={number:?}@{:?} \
             out=trash:{},insert:{},copy:{},paste:{} picked={:?} shown={} thumbs={}",
            at(self.pane.upcast_ref()),
            at(self.insert.upcast_ref()),
            at(self.copy.upcast_ref()),
            at(self.paste.upcast_ref()),
            at(self.number.upcast_ref()),
            self.trash.reveals_child(),
            self.insert.reveals_child(),
            self.copy.reveals_child(),
            self.paste.is_visible(),
            self.picked.borrow().pages,
            self.thumbs.shown(),
            middles.join(";")
        )
    }

    /// Show the buttons and the number of the page under the pointer, or put them away: with the
    /// pointer gone, while pages are being dragged, the trash where it would take every page,
    /// which a document keeps one of, and Paste while no page of this kind is held. Each button
    /// says how many pages it takes.
    pub fn hover(&self) {
        let layout = self.thumbs.layout();
        let top = self.scrolled().1;
        let page = self
            .pointer
            .get()
            .filter(|_| self.dragged.borrow().is_empty())
            .map(|y| y + top)
            .filter(|y| *y < f64::from(layout.height))
            .map(|y| page_at(&layout, y))
            .filter(|page| *page < layout.pages.len());
        if let Some(page) = page {
            self.hovered.set(Some(page));
            if let Some(label) = self.number.child().and_downcast::<gtk::Label>() {
                label.set_text(&(page + 1).to_string());
            }
        }
        // Never targetable, unlike the buttons: a click on it is one on the thumbnail.
        self.number.set_reveal_child(page.is_some());
        let taken = page.map_or(1, |page| self.taking(page).len());
        reveal(&self.trash, page.is_some() && taken < layout.pages.len());
        reveal(&self.copy, page.is_some());
        reveal(&self.insert, page.is_some());
        for (buttons, verb) in [(&self.trash, "Delete"), (&self.copy, "Copy")] {
            if let Some(button) = buttons.child() {
                button.set_tooltip_text(Some(&tooltip(verb, taken, "")));
            }
        }
        let held = held(self.kind);
        let pasted = held.as_ref().map_or(1, |held| held.count());
        self.paste
            .set_tooltip_text(Some(&tooltip("Paste", pasted, " Here")));
        self.paste.set_visible(held.is_some());
        self.pane.queue_allocate();
    }

    /// A drag of the strip's pages is at `y` over it, or has left it: show where they would land,
    /// scroll while the drag is near an edge, and say whether a drop here moves them.
    fn aim(self: &Rc<Self>, y: Option<f64>) -> gdk::DragAction {
        let layout = self.thumbs.layout();
        let top = self.scrolled().1;
        let dragged = self.dragged.borrow();
        let gap = y
            .filter(|_| !dragged.is_empty())
            .map(|y| gap_at(&layout, y + top))
            .filter(|gap| !PageEdit::moves(&dragged, *gap).is_empty());
        drop(dragged);
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
            if o.speed.get() == 0.0 || o.dragged.borrow().is_empty() {
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
    /// in the hovered thumbnail's top-right corner, Copy in its top-left and its number in its
    /// bottom-left, the insert and paste buttons centred on the gap below it, and the bar across
    /// the gap a drop would land in.
    fn place_on_strip(&self, child: &gtk::Widget) -> Option<gdk::Rectangle> {
        let layout = self.thumbs.layout();
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
        } else if child == self.number.upcast_ref::<gtk::Widget>() {
            let rect = layout.pages.get(self.hovered.get()?)?;
            // Its round end on the bottom-left corner's arc, as the buttons are on theirs, kept on
            // what shows of a page scrolled half out of the strip.
            let shown = (rect.y + rect.h - top).min(origin.y() + self.strip.height() as f32);
            let y = (shown - CORNER).max(rect.y - top + CORNER) - h / 2.0;
            (rect.x + CORNER - h / 2.0, y, w)
        } else {
            let page = self.hovered.get()?;
            let rect = layout.pages.get(page)?;
            let left = child == self.copy.upcast_ref::<gtk::Widget>();
            match left || child == self.trash.upcast_ref::<gtk::Widget>() {
                // Centred on the arc of a corner of what shows of the thumbnail, so a page
                // scrolled half out of the strip keeps its trash and its copy button in reach.
                true => {
                    let shown = (rect.y - top).max(origin.y());
                    let y = (shown + CORNER).min(rect.y + rect.h - top - CORNER) - h / 2.0;
                    let x = match left {
                        true => rect.x + CORNER,
                        false => rect.x + rect.w - CORNER,
                    };
                    (x - w / 2.0, y, w)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_and_shift_pick_pages_that_a_button_then_takes_together() {
        let mut picked = Picked::default();
        // Ctrl+click picks the page clicked beside the page shown.
        picked.toggle(4, 1);
        assert_eq!(picked.pages, [1, 4]);
        // Shift+click picks from the page last Ctrl+clicked, and only those.
        picked.extend(2, 1);
        assert_eq!(picked.pages, [2, 3, 4]);
        picked.toggle(3, 1);
        assert_eq!(picked.pages, [2, 4]);
        // A button on a picked page takes them all, on another that page alone.
        assert_eq!((picked.taking(4), picked.taking(1)), (vec![2, 4], vec![1]));
        assert!(framed(&picked.pages, 1, 4) && !framed(&picked.pages, 1, 1));
        // They go with their pages, and once taken out the page shown stands alone again.
        picked.repage(|page| PageEdit::Move { from: 4, to: 0 }.map(page));
        assert_eq!(picked.pages, [0, 3]);
        picked.repage(|_| None);
        assert_eq!(picked, Picked::default());
        assert!(framed(&picked.pages, 1, 1));
        // With nothing picked, Shift+click picks from the page shown, and Ctrl+click on that page
        // leaves nothing picked.
        picked.extend(5, 7);
        assert_eq!(picked.pages, [5, 6, 7]);
        let mut alone = Picked::default();
        alone.toggle(2, 2);
        assert_eq!(alone, Picked::default());
    }
}
