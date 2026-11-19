//! The loupe over a finger or a pen selecting text: the line under the hand, twice as large in a
//! popover above it, as GTK's text views show one (`gtk_text_view_show_magnifier`). The line is
//! rendered again at twice the scale, once for each line the hand reaches, so its text is as
//! sharp as the page's, and the view paints its selection and marks over that.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_core::pdf;
use adw::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use super::preview::CORNER;
use super::protocol::Request;
use super::selection::nearest;
use super::tab::PdfTab;

/// How much larger the loupe shows the page: GTK's text views' and entries' two.
const MAGNIFICATION: f32 = 2.0;

/// The narrowest a loupe is, in logical pixels, however short the line: GTK's text views'.
const NARROWEST: f32 = 80.0;

/// A band of a page rendered for the loupe: `rect` in device pixels of the page drawn at `scale`
/// device pixels per point in `theme`, as the cache's `generation` had it, so a page edited since
/// is rendered again.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Band {
    page: usize,
    rect: (i32, i32, i32, i32),
    scale: f32,
    theme: pdf::Theme,
    generation: u64,
}

impl Band {
    /// Whether this band holds all of `other`: the same page, drawn alike, round all of it.
    fn holds(&self, other: &Band) -> bool {
        let ((x, y, w, h), (ox, oy, ow, oh)) = (self.rect, other.rect);
        Band {
            rect: other.rect,
            ..*self
        } == *other
            && x <= ox
            && y <= oy
            && ox + ow <= x + w
            && oy + oh <= y + h
    }

    /// What is asked for to show this one: half the line again above and below it, so the taller
    /// glyphs along the same line find it rendered already.
    fn asked(self) -> Band {
        let (x, y, w, h) = self.rect;
        let more = h / 2;
        Band {
            rect: (x, (y - more).max(0), w, h + 2 * more),
            ..self
        }
    }

    /// Where it lies on its page in the page's own logical pixels, `texture` being what was drawn
    /// of it on a display of `sf` device pixels to a logical one.
    fn placed(&self, texture: &gdk::Texture, sf: i32) -> graphene::Rect {
        let px = sf as f32 * MAGNIFICATION;
        let (x, y, _, _) = self.rect;
        let (w, h) = (texture.width() as f32, texture.height() as f32);
        graphene::Rect::new(x as f32 / px, y as f32 / px, w / px, h / px)
    }
}

/// The loupe on screen while a finger or a pen selects text.
pub(super) struct Loupe {
    popover: gtk::Popover,
    picture: gtk::Picture,
    /// The reading view as it was last painted, which the loupe enlarges until the line's band
    /// lands, and whose repaints are the loupe's too: the selection the hand has just made, a tile
    /// landing, or a page whose glyphs were read only now.
    source: gtk::WidgetPaintable,
    /// Where the hand is in the reading view.
    hand: Cell<(f64, f64)>,
    /// The band last rendered, and the one asked for since.
    band: RefCell<Option<(Band, gdk::Texture)>>,
    asked: Cell<Option<Band>>,
    /// When the band on its way was asked for, how long the last one took in milliseconds, the
    /// longest the loupe took to paint, and whether it last painted a band. Only drills ask.
    #[cfg(feature = "bench")]
    timing: Cell<(Option<std::time::Instant>, Option<f64>, f64, bool)>,
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
            band: RefCell::new(None),
            asked: Cell::new(None),
            #[cfg(feature = "bench")]
            timing: Cell::new((None, None, 0.0, false)),
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

    /// A band landed that the loupe may have asked for: what it shows from now on. One the loupe
    /// did not ask for, the link preview's or one for a line the hand has left since, is handed
    /// back.
    pub(super) fn loupe_landed(
        &self,
        page: usize,
        rect: (i32, i32, i32, i32),
        image: pdf::RgbaImage,
    ) -> Option<pdf::RgbaImage> {
        {
            let loupe = self.loupe.borrow();
            let Some(loupe) = loupe.as_ref() else {
                return Some(image);
            };
            let Some(asked) = (loupe.asked.get()).filter(|a| a.page == page && a.rect == rect)
            else {
                return Some(image);
            };
            loupe.asked.set(None);
            // As much as the page had of it: a band running past the page's edge is cut there.
            let drawn = (rect.0, rect.1, image.width as i32, image.height as i32);
            let texture = super::cache::texture(image).upcast();
            loupe.band.replace(Some((
                Band {
                    rect: drawn,
                    ..asked
                },
                texture,
            )));
            #[cfg(feature = "bench")]
            {
                let (at, _, slowest, sharp) = loupe.timing.get();
                let took = at.map(|at| at.elapsed().as_secs_f64() * 1000.0);
                loupe.timing.set((None, took, slowest, sharp));
            }
        }
        self.place_loupe();
        None
    }

    /// Put the loupe over the line under the hand, as [`fitted`] says, showing the line's band
    /// once it has landed and asking for it until it has. Nothing shows until the page's glyphs
    /// are read, as nothing is selected before then either.
    fn place_loupe(&self) {
        #[cfg(feature = "bench")]
        let started = std::time::Instant::now();
        let loupe = self.loupe.borrow();
        let Some(loupe) = loupe.as_ref() else {
            return;
        };
        let (x, y) = loupe.hand.get();
        let Some((page, line)) = self.line_under(x, y) else {
            return loupe.popover.popdown();
        };
        let (middle, size, at) = fitted(x, line, self.view.width());
        let wanted = self.band_for(page, line, size.0);
        let landed = (loupe.band.borrow().clone())
            .filter(|(band, _)| wanted.is_some_and(|w| band.holds(&w)));
        // One band on its way at a time, a page's heaviest taking the render thread a few tenths
        // of a second: the line the hand is on by the time it lands is asked for next.
        let waiting = loupe.asked.get().is_some();
        if let Some(wanted) = wanted.filter(|_| landed.is_none() && !waiting) {
            let band = wanted.asked();
            loupe.asked.set(Some(band));
            self.ask(Request::Band {
                page,
                scale: band.scale,
                rect: band.rect,
                theme: band.theme,
            });
            #[cfg(feature = "bench")]
            {
                let (_, took, slowest, sharp) = loupe.timing.get();
                let now = Some(std::time::Instant::now());
                loupe.timing.set((now, took, slowest, sharp));
            }
        }
        let sf = self.view.scale_factor().max(1);
        let (w, h) = (self.view.width() as f32, self.view.height() as f32);
        let middle = (
            middle.0.clamp(0.0, w.max(0.0)),
            middle.1.clamp(0.0, h.max(0.0)),
        );
        paint(&loupe.picture, middle, size, sf, |snapshot| {
            let sharp = landed.as_ref().is_some_and(|(band, texture)| {
                let placed = band.placed(texture, sf);
                self.view.paint_band(snapshot, page, texture, &placed)
            });
            if !sharp {
                loupe.source.snapshot(snapshot, f64::from(w), f64::from(h));
            }
            #[cfg(feature = "bench")]
            {
                let (asked, took, slowest, _) = loupe.timing.get();
                loupe.timing.set((asked, took, slowest, sharp));
            }
        });
        let corner = graphene::Point::new(at.x() as f32, at.y() as f32);
        let Some(corner) = self.view.compute_point(&self.host, &corner) else {
            return;
        };
        let (x, y) = (corner.x() as i32, corner.y() as i32);
        let at = gdk::Rectangle::new(x, y, at.width(), at.height());
        loupe.popover.set_pointing_to(Some(&at));
        loupe.popover.popup();
        #[cfg(feature = "bench")]
        {
            let (asked, took, slowest, sharp) = loupe.timing.get();
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            loupe.timing.set((asked, took, slowest.max(ms), sharp));
        }
    }

    /// The band of page `page` a loupe `width` wide needs to show `line`, a box in the reading
    /// view, at twice its scale: that line across what the view shows of the page, and as far
    /// past either side of the view as the loupe reaches, where a hand at its edge has it.
    fn band_for(&self, page: usize, line: gdk::Rectangle, width: f32) -> Option<Band> {
        let (pw, ph) = self.view.page_size(page)?;
        let sheet = pdf::Rect::from_corners((0.0, 0.0), (pw, ph));
        let sheet = self.view.widget_rect(page, &sheet)?;
        let sf = self.view.scale_factor().max(1);
        // The loupe shows a quarter of its width either side of a hand kept inside the view, and
        // a pixel more all round than that and the line's box, which is whole pixels already.
        let reach = width / MAGNIFICATION / 2.0 + 1.0;
        let (sx, sy) = (sheet.x() as f32, sheet.y() as f32);
        let (left, right) = (-reach - sx, self.view.width() as f32 + reach - sx);
        let (top, bottom) = (
            line.y() as f32 - 1.0 - sy,
            (line.y() + line.height()) as f32 + 1.0 - sy,
        );
        // In the band's own pixels, inside the page as pdfium draws it at that scale.
        let px = sf as f32 * MAGNIFICATION;
        let scale = self.view.pixels_per_point() * px;
        let (pw, ph) = ((pw * scale).round() as i32, (ph * scale).round() as i32);
        let (x, y) = (
            ((left * px).floor() as i32).max(0),
            ((top * px).floor() as i32).max(0),
        );
        let w = ((right * px).ceil() as i32).min(pw) - x;
        let h = ((bottom * px).ceil() as i32).min(ph) - y;
        (w > 0 && h > 0).then(|| Band {
            page,
            rect: (x, y, w, h),
            scale,
            theme: self.view.theme(),
            generation: self.view.cache().borrow().generation(),
        })
    }

    /// The page and the box, in the reading view, of the line of text under `(x, y)`: the glyph
    /// the end of a selection there takes, or for a space between words the glyph before it.
    fn line_under(&self, x: f64, y: f64) -> Option<(usize, gdk::Rectangle)> {
        let (page, at) = self.view.nearest_page_point(x, y)?;
        let glyphs = self.glyphs.borrow();
        let on = glyphs.get(&page)?;
        let end = nearest(on, at)?;
        let boxed = |g: &&accent_core::pdf::Glyph| g.rect.width() > 0.0 && g.rect.height() > 0.0;
        let glyph = on[..=end].iter().rev().find(boxed)?;
        Some((page, self.view.widget_rect(page, &glyph.rect)?))
    }

    /// The loupe's popover while there is one. Only drills ask, to picture it.
    #[cfg(feature = "bench")]
    pub fn loupe_popover(&self) -> Option<gtk::Popover> {
        self.loupe.borrow().as_ref().map(|l| l.popover.clone())
    }

    /// The loupe against the screen: `none`, or whether it shows, the hand, the part of the
    /// reading view it enlarges as `(x, y, w, h)`, its size, whether it is above the line, and
    /// where its popover is on the screen and whether it is all on it, the drill's window being
    /// at the screen's corner; then how many device pixels of what it shows there are to one of
    /// the view's, two once its band has landed, how long that band took in milliseconds, and the
    /// longest the loupe has taken to paint. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn loupe_report(&self) -> String {
        let loupe = self.loupe.borrow();
        let Some(loupe) = loupe.as_ref() else {
            return "none".to_string();
        };
        let (x, y) = loupe.hand.get();
        let shown = loupe.popover.is_visible();
        let Some((_, line)) = self.line_under(x, y) else {
            return format!("shown={shown} hand=({x:.0},{y:.0}) line=none");
        };
        let ((mx, my), (w, h), at) = fitted(x, line, self.view.width());
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
        let (_, took, slowest, sharp) = loupe.timing.get();
        let scale = if sharp { MAGNIFICATION } else { 1.0 };
        let took = took.map_or("none".to_string(), |ms| format!("{ms:.1}"));
        format!(
            "shown={shown} hand=({x:.0},{y:.0}) source=({:.0},{:.0},{sw:.0},{sh:.0}) size={w}x{h} \
             above={above} popover={placed:?} on_screen={on_screen} source_scale={scale} \
             band_ms={took} paint_ms={slowest:.1}",
            mx - sw / 2.0,
            my - sh / 2.0,
        )
    }
}

/// What GTK's text view makes of a hand at `x` across the reading view, `width` wide, over `line`,
/// a box in it (`gtk_text_view_show_magnifier`): the point of the view at the loupe's middle, at
/// the hand's x and the line's middle; the loupe's size, the line twice as tall and a third wider
/// than that, but never narrower than [`NARROWEST`]; and the rectangle its popover points at from
/// above, the line from a quarter of the way down, kept across the view.
fn fitted(x: f64, line: gdk::Rectangle, width: i32) -> ((f32, f32), (f32, f32), gdk::Rectangle) {
    let tall = (line.height() as f32 * MAGNIFICATION).round();
    let size = ((tall * 4.0 / 3.0).round().max(NARROWEST), tall);
    let middle = (x as f32, line.y() as f32 + line.height() as f32 / 2.0);
    let quarter = line.height() / 4;
    let across = (x as i32).clamp(0, width.max(0));
    let at = gdk::Rectangle::new(across, line.y() + quarter, 1, line.height() - quarter);
    (middle, size, at)
}

/// Paint into `picture`, `size` large with [`CORNER`]s, what `draw` paints of the reading view
/// in its own coordinates, around `middle` and [`MAGNIFICATION`] times as large: what GTK's
/// magnifier draws (`gtk_magnifier_snapshot`). Moved by whole device pixels on a display of `sf`
/// to one, so a band rendered at the loupe's scale lands on its pixels one to one.
fn paint(
    picture: &gtk::Picture,
    middle: (f32, f32),
    size: (f32, f32),
    sf: i32,
    draw: impl FnOnce(&gtk::Snapshot),
) {
    let snapshot = gtk::Snapshot::new();
    let bounds = graphene::Rect::new(0.0, 0.0, size.0, size.1);
    snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, CORNER));
    let grid = |v: f32| (v * sf as f32).round() / sf as f32;
    snapshot.translate(&graphene::Point::new(
        grid(size.0 / 2.0 - MAGNIFICATION * middle.0),
        grid(size.1 / 2.0 - MAGNIFICATION * middle.1),
    ));
    snapshot.scale(MAGNIFICATION, MAGNIFICATION);
    draw(&snapshot);
    snapshot.pop();
    let size = graphene::Size::new(size.0, size.1);
    picture.set_paintable(snapshot.to_paintable(Some(&size)).as_ref());
}

#[cfg(test)]
mod tests {
    use super::{Band, NARROWEST, fitted};
    use accent_core::pdf::Theme;
    use gtk::gdk::Rectangle;

    /// A body line's loupe is GTK's narrowest, twice the line tall, around the hand and the line's
    /// middle; a heading's is a third wider than it is tall.
    #[test]
    fn a_loupe_is_the_line_twice_over() {
        let (middle, size, at) = fitted(30.0, Rectangle::new(10, 100, 50, 16), 400);
        assert_eq!(middle, (30.0, 108.0));
        assert_eq!(size, (NARROWEST, 32.0));
        assert_eq!(at, Rectangle::new(30, 104, 1, 12));
        let (_, size, _) = fitted(30.0, Rectangle::new(10, 100, 200, 60), 400);
        assert_eq!(size, (160.0, 120.0));
        // A hand dragged past the view's side points the popover at that side.
        let (middle, _, at) = fitted(-20.0, Rectangle::new(0, 100, 50, 16), 400);
        assert_eq!((middle.0, at.x()), (-20.0, 0));
    }

    /// A line's band is asked for with half the line again above and below, so a taller glyph
    /// along it is held by the same band, while the line under it, another page, another zoom
    /// or a page edited since is not.
    #[test]
    fn a_band_serves_its_whole_line() {
        let band = |page, rect, scale, generation| Band {
            page,
            rect,
            scale,
            theme: Theme::Plain,
            generation,
        };
        let line = band(0, (0, 100, 1600, 32), 2.67, 0);
        let asked = line.asked();
        assert_eq!(asked.rect, (0, 84, 1600, 64));
        assert!(asked.holds(&band(0, (40, 90, 800, 40), 2.67, 0)));
        assert!(!asked.holds(&band(0, (0, 140, 1600, 32), 2.67, 0)));
        assert!(!asked.holds(&band(1, (0, 100, 1600, 32), 2.67, 0)));
        assert!(!asked.holds(&band(0, (0, 100, 1600, 32), 5.33, 0)));
        assert!(!asked.holds(&band(0, (0, 100, 1600, 32), 2.67, 1)));
        // A line at the page's top is asked from the page's top.
        assert_eq!(
            band(0, (0, 4, 10, 20), 2.67, 0).asked().rect,
            (0, 0, 10, 40)
        );
    }
}
