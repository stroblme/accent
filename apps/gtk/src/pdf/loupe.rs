//! The loupe over a finger or a pen selecting text: the line under the hand, twice as large in a
//! popover above it, as GTK's text views show one (`gtk_text_view_show_magnifier`).

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use super::preview::CORNER;
use super::selection::nearest;
use super::tab::PdfTab;

/// How much larger the loupe shows the page: GTK's text views' and entries' two.
const MAGNIFICATION: f32 = 2.0;

/// The narrowest a loupe is, in logical pixels, however short the line: GTK's text views'.
const NARROWEST: f32 = 80.0;

/// The loupe on screen while a finger or a pen selects text.
pub(super) struct Loupe {
    popover: gtk::Popover,
    picture: gtk::Picture,
    /// The reading view as it was last painted, selection and all, which the loupe enlarges: a
    /// frame painted already, so no page is drawn again for it.
    source: gtk::WidgetPaintable,
    /// Where the hand is in the reading view.
    hand: Cell<(f64, f64)>,
}

impl Loupe {
    /// A loupe over `view` in a popover parented by `host`: GTK's, which libadwaita draws as an
    /// on-screen display. It never autohides, being up under a hand that is still selecting, and
    /// takes no input, so it takes nothing from that hand either.
    fn new(view: &super::PdfView, host: &gtk::Box) -> Loupe {
        let picture = gtk::Picture::new();
        let popover = gtk::Popover::builder()
            .autohide(false)
            .can_target(false)
            .position(gtk::PositionType::Top)
            .child(&picture)
            .build();
        popover.add_css_class("magnifier");
        popover.set_parent(host);
        Loupe {
            popover,
            picture,
            source: gtk::WidgetPaintable::new(Some(view)),
            hand: Cell::new((0.0, 0.0)),
        }
    }
}

impl PdfTab {
    /// Follow a hand selecting text with the loupe, and take it away when the hand lets go or the
    /// tab goes.
    pub(super) fn wire_loupe(self: &Rc<Self>) {
        self.view.connect_loupe(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |hand| match hand {
                Some(at) => tab.loupe_to(at),
                None => tab.hide_loupe(),
            }
        ));
        self.host.connect_unmap(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.hide_loupe()
        ));
    }

    /// The hand selecting text is at `at` in the reading view.
    fn loupe_to(self: &Rc<Self>, at: (f64, f64)) {
        if self.loupe.borrow().is_none() {
            let loupe = Loupe::new(&self.view, &self.host);
            // What the view paints next is what the loupe shows: the selection the hand has just
            // made, a tile landing, or a page whose glyphs were read only now.
            loupe.source.connect_invalidate_contents(glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move |_| tab.place_loupe()
            ));
            self.loupe.replace(Some(loupe));
        }
        if let Some(loupe) = self.loupe.borrow().as_ref() {
            loupe.hand.set(at);
        }
        self.place_loupe();
    }

    fn hide_loupe(&self) {
        if let Some(loupe) = self.loupe.take() {
            loupe.popover.popdown();
            loupe.popover.unparent();
        }
    }

    /// Put the loupe over the line under the hand, as [`fitted`] says. Nothing shows until the
    /// page's glyphs are read, as nothing is selected before then either.
    fn place_loupe(&self) {
        let loupe = self.loupe.borrow();
        let Some(loupe) = loupe.as_ref() else {
            return;
        };
        let (x, y) = loupe.hand.get();
        let Some(line) = self.line_under(x, y) else {
            return loupe.popover.popdown();
        };
        let (middle, size, at) = fitted(x, line);
        paint(&loupe.picture, &loupe.source, middle, size);
        let corner = graphene::Point::new(at.x() as f32, at.y() as f32);
        let Some(corner) = self.view.compute_point(&self.host, &corner) else {
            return;
        };
        let (x, y) = (corner.x() as i32, corner.y() as i32);
        let at = gdk::Rectangle::new(x, y, at.width(), at.height());
        loupe.popover.set_pointing_to(Some(&at));
        loupe.popover.popup();
    }

    /// The box, in the reading view, of the line of text under `(x, y)`: the glyph the end of a
    /// selection there takes, or for a space between words the glyph before it.
    fn line_under(&self, x: f64, y: f64) -> Option<gdk::Rectangle> {
        let (page, at) = self.view.nearest_page_point(x, y)?;
        let glyphs = self.glyphs.borrow();
        let on = glyphs.get(&page)?;
        let end = nearest(on, at)?;
        let boxed = |g: &&accent_core::pdf::Glyph| g.rect.width() > 0.0 && g.rect.height() > 0.0;
        let glyph = on[..=end].iter().rev().find(boxed)?;
        self.view.widget_rect(page, &glyph.rect)
    }

    /// The loupe's popover while there is one. Only drills ask, to picture it.
    #[cfg(feature = "bench")]
    pub fn loupe_popover(&self) -> Option<gtk::Popover> {
        self.loupe.borrow().as_ref().map(|l| l.popover.clone())
    }

    /// The loupe against the screen: `none`, or whether it shows, the hand, the part of the
    /// reading view it enlarges as `(x, y, w, h)`, its size, whether it is above the line, and
    /// where its popover is on the screen and whether it is all on it, the drill's window being
    /// at the screen's corner. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn loupe_report(&self) -> String {
        let loupe = self.loupe.borrow();
        let Some(loupe) = loupe.as_ref() else {
            return "none".to_string();
        };
        let (x, y) = loupe.hand.get();
        let shown = loupe.popover.is_visible();
        let Some(line) = self.line_under(x, y) else {
            return format!("shown={shown} hand=({x:.0},{y:.0}) line=none");
        };
        let ((mx, my), (w, h), at) = fitted(x, line);
        let (sw, sh) = (w / MAGNIFICATION, h / MAGNIFICATION);
        // The line's top in the window's surface, which a popup is placed against.
        let top = self.view.root().and_then(|root| {
            let (_, dy) = root.surface_transform();
            let p = graphene::Point::new(0.0, at.y() as f32);
            Some(f64::from(self.view.compute_point(&root, &p)?.y()) + dy)
        });
        let surface = loupe.popover.surface().and_downcast::<gdk::Popup>();
        let placed =
            (surface.as_ref()).map(|s| (s.position_x(), s.position_y(), s.width(), s.height()));
        let monitor = (surface.as_ref()).and_then(|s| s.display().monitor_at_surface(s));
        let inside = |(x, y, w, h): (i32, i32, i32, i32), m: gdk::Rectangle| {
            x >= m.x() && y >= m.y() && x + w <= m.x() + m.width() && y + h <= m.y() + m.height()
        };
        let on_screen = (placed.zip(monitor)).is_some_and(|(p, m)| inside(p, m.geometry()));
        let above = placed
            .zip(top)
            .is_some_and(|((_, y, _, h), top)| f64::from(y + h) <= top + 1.0);
        format!(
            "shown={shown} hand=({x:.0},{y:.0}) source=({:.0},{:.0},{sw:.0},{sh:.0}) size={w}x{h} \
             above={above} popover={placed:?} on_screen={on_screen}",
            mx - sw / 2.0,
            my - sh / 2.0,
        )
    }
}

/// What GTK's text view makes of a hand at `x` across the reading view over `line`, a box in it
/// (`gtk_text_view_show_magnifier`): the point of the view at the loupe's middle, at the hand's x
/// and the line's middle; the loupe's size, the line twice as tall and a third wider than that,
/// but never narrower than [`NARROWEST`]; and the rectangle its popover points at from above, the
/// line from a quarter of the way down.
fn fitted(x: f64, line: gdk::Rectangle) -> ((f32, f32), (f32, f32), gdk::Rectangle) {
    let tall = (line.height() as f32 * MAGNIFICATION).round();
    let size = ((tall * 4.0 / 3.0).round().max(NARROWEST), tall);
    let middle = (x as f32, line.y() as f32 + line.height() as f32 / 2.0);
    let quarter = line.height() / 4;
    let at = gdk::Rectangle::new(x as i32, line.y() + quarter, 1, line.height() - quarter);
    (middle, size, at)
}

/// Paint into `picture` the reading view as `source` last saw it, around `middle` and
/// [`MAGNIFICATION`] times as large, into a picture `size` large with [`CORNER`]s: what GTK's
/// magnifier draws, its middle kept on the view as it keeps it (`gtk_magnifier_snapshot`).
fn paint(
    picture: &gtk::Picture,
    source: &gtk::WidgetPaintable,
    middle: (f32, f32),
    size: (f32, f32),
) {
    let w = source.intrinsic_width() as f32;
    let h = source.intrinsic_height() as f32;
    let snapshot = gtk::Snapshot::new();
    let bounds = graphene::Rect::new(0.0, 0.0, size.0, size.1);
    snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, CORNER));
    snapshot.translate(&graphene::Point::new(size.0 / 2.0, size.1 / 2.0));
    snapshot.scale(MAGNIFICATION, MAGNIFICATION);
    let x = middle.0.clamp(0.0, w.max(0.0));
    let y = middle.1.clamp(0.0, h.max(0.0));
    snapshot.translate(&graphene::Point::new(-x, -y));
    source.snapshot(&snapshot, f64::from(w), f64::from(h));
    snapshot.pop();
    let size = graphene::Size::new(size.0, size.1);
    picture.set_paintable(snapshot.to_paintable(Some(&size)).as_ref());
}

#[cfg(test)]
mod tests {
    use super::{NARROWEST, fitted};
    use gtk::gdk::Rectangle;

    /// A body line's loupe is GTK's narrowest, twice the line tall, around the hand and the line's
    /// middle; a heading's is a third wider than it is tall.
    #[test]
    fn a_loupe_is_the_line_twice_over() {
        let (middle, size, at) = fitted(30.0, Rectangle::new(10, 100, 50, 16));
        assert_eq!(middle, (30.0, 108.0));
        assert_eq!(size, (NARROWEST, 32.0));
        assert_eq!(at, Rectangle::new(30, 104, 1, 12));
        let (_, size, _) = fitted(30.0, Rectangle::new(10, 100, 200, 60));
        assert_eq!(size, (160.0, 120.0));
    }
}
