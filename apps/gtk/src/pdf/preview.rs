//! The Ctrl-hover link preview: where a link goes, shown without going there.

use std::rc::Rc;

use accent_core::pdf::{self, LinkTarget};
use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use super::geometry::PT_TO_PX;
use super::protocol::Request;
use super::tab::PdfTab;

/// How tall a band is for its width: 96 pixels of 256, about a quarter of a portrait page, which
/// is a heading and the lines under it. A whole page shrunk to a popover says nothing a reader
/// can read.
const TALL: f32 = 0.375;

/// What a link preview keeps clear of its pane's edges, a PDF's or a diagram's, in logical pixels.
const MARGIN: i32 = 12;

/// The part of a page a link preview shows: drawn at `scale` logical pixels to a point, `size`
/// logical pixels large, and starting `offset` of them down the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Band {
    pub scale: f32,
    pub size: (i32, i32),
    pub offset: i32,
}

impl Band {
    /// The band of a page `page` points large around the destination `top` points down it, drawn
    /// at the reading view's `scale` so it reads as the page will once the link is followed: as
    /// wide as the page and [`TALL`] of that high. A view zoomed out past 100 % draws it at 100 %
    /// all the same, as smaller is unreadable again. A page wider than `room` allows is drawn
    /// smaller to fit, and a band taller than it is cut down.
    ///
    /// The destination is centred, and the band stays inside the page at both ends: a link to the
    /// last line shows the foot of the page rather than a strip of nothing under it.
    pub(super) fn around(page: (f32, f32), top: Option<f32>, scale: f32, room: (i32, i32)) -> Band {
        let scale = scale
            .max(PT_TO_PX)
            .min(room.0.max(1) as f32 / page.0.max(1.0));
        let (width, height) = (page.0 * scale, page.1 * scale);
        let tall = (width * TALL).min(height).min(room.1.max(0) as f32).round();
        let at = top.unwrap_or(0.0) * scale - tall / 2.0;
        Band {
            scale,
            size: (width.round() as i32, tall as i32),
            offset: at.clamp(0.0, (height - tall).max(0.0)).round() as i32,
        }
    }

    /// What is rendered for it on a display of `sf` device pixels to a logical one: device pixels
    /// per point, and the band as `(x, y, w, h)` in device pixels of the page at that scale.
    pub(super) fn pixels(&self, sf: i32) -> (f32, (i32, i32, i32, i32)) {
        let rect = (0, self.offset * sf, self.size.0 * sf, self.size.1 * sf);
        (self.scale * sf as f32, rect)
    }
}

/// The Ctrl-hover link preview currently on screen.
pub(super) struct Preview {
    popover: gtk::Popover,
    /// Where the band of the target page goes: the page's stand-in, if it has one, until the band
    /// itself lands, which for a page of another PDF is all there is.
    band: gtk::Picture,
    /// The band of a page of this document still being drawn, which is what an answer must be.
    asked: Option<(usize, Band)>,
    /// What it is showing, so a pointer still on the same link asks for nothing again.
    target: LinkTarget,
}

impl PdfTab {
    /// Ctrl-hover link preview. A separate controller retains modifier state and rejects
    /// non-Ctrl motion before link lookup or rendering.
    pub(super) fn wire_preview(self: &Rc<Self>) {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |controller, x, y| tab.hover(x, y, controller.current_event_state())
        ));
        // The pointer left the page for the sidebar, the chrome or another window.
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.hide_preview()
        ));
        self.view.add_controller(motion);
        // The tab was switched away from or closed while a preview was up.
        self.host.connect_unmap(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.hide_preview()
        ));
    }

    /// The band a preview asked for landed. One for a link the pointer has left since is dropped.
    pub(super) fn band_landed(
        &self,
        page: usize,
        rect: (i32, i32, i32, i32),
        image: pdf::RgbaImage,
    ) {
        let sf = self.view.scale_factor().max(1);
        let mut preview = self.preview.borrow_mut();
        let Some(shown) = preview.as_mut() else {
            return;
        };
        let Some((_, band)) = shown
            .asked
            .filter(|(at, band)| *at == page && band.pixels(sf).1 == rect)
        else {
            return;
        };
        shown.asked = None;
        put_band(&shown.band, band, &super::cache::texture(image), sf);
    }

    /// Ctrl over a link: show where it leads. Anything else takes the preview away.
    fn hover(self: &Rc<Self>, x: f64, y: f64, state: gtk::gdk::ModifierType) {
        if !state.contains(crate::platform::PRIMARY_MODIFIER) {
            return self.hide_preview();
        }
        let Some(target) = self.link_at(&self.view, x, y) else {
            return self.hide_preview();
        };
        // Still the same link: the popover already says what this one does.
        if self
            .preview
            .borrow()
            .as_ref()
            .is_some_and(|shown| shown.target == target)
        {
            return;
        }
        self.hide_preview();
        self.show_preview(target, x, y);
    }

    pub(super) fn hide_preview(&self) {
        if let Some(shown) = self.preview.borrow_mut().take() {
            shown.popover.popdown();
        }
    }

    /// Put a popover over the link. A page target gets a band of the page it leads to, an
    /// external one gets the address it would open, which is the thing worth knowing before
    /// clicking it, and one into another PDF the same band of that file's page under its name,
    /// read once the file is ([`PdfTab::fill_band_elsewhere`]).
    ///
    /// The popover stays inside the PDF pane: above the link where it fits, below it otherwise,
    /// and slid along to keep clear of the pane's sides.
    fn show_preview(self: &Rc<Self>, target: LinkTarget, x: f64, y: f64) {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let band = gtk::Picture::new();
        let caption = gtk::Label::builder()
            .css_classes(["caption", "dimmed"])
            .xalign(0.0)
            .build();
        let mut elsewhere = None;
        match &target {
            LinkTarget::Page { page, .. } => {
                content.append(&band);
                caption.set_label(&format!("Page {}", page + 1));
                content.append(&caption);
            }
            LinkTarget::Uri(uri) => content.append(
                &gtk::Label::builder()
                    .label(uri)
                    .css_classes(["caption"])
                    .wrap(true)
                    .max_width_chars(48)
                    .xalign(0.0)
                    .build(),
            ),
            LinkTarget::File(to) => {
                elsewhere = self.on_locate.get().and_then(|f| f(self, &to.file));
                if elsewhere.is_some() {
                    content.append(&band);
                }
                content.append(
                    &gtk::Label::builder()
                        .label(crate::doc::file_name(&to.file))
                        .css_classes(["caption"])
                        .xalign(0.0)
                        .build(),
                );
                // A named destination's page is known once the file has been read.
                if let pdf::Remote::Page { page, .. } = to.dest {
                    caption.set_label(&format!("Page {}", page + 1));
                }
                content.append(&caption);
            }
        }
        // Never autohide: an autohiding popover takes a grab, and this one is under the pointer
        // that is still reading the page. It cannot be targeted either, so it neither swallows a
        // click nor steals the crossing event that would take it away again.
        let popover = gtk::Popover::builder()
            .autohide(false)
            .can_target(false)
            .child(&content)
            .build();
        popover.set_parent(&self.host);
        let at = (x as i32, y as i32);
        // The pointer's row and one either side of it, so the popover never starts on it: under
        // X11 the pointer then goes into the popover, and its leave takes the preview away.
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(at.0, at.1 - 1, 1, 3)));
        // A popover parented by hand stays parented until it is unparented by hand.
        popover.connect_closed(|popover| popover.unparent());
        // Up before it is measured, as a hidden widget measures nothing: what follows changes it
        // before its first frame is drawn.
        popover.popup();

        // The room the band has: the pane less the popover around it, across, and half the
        // pane's height less the rest of the popover, which fits above the link or below it.
        let pane = (self.host.width(), self.host.height());
        let (_, inner, _, _) = content.measure(gtk::Orientation::Horizontal, -1);
        let (_, outer, _, _) = popover.measure(gtk::Orientation::Horizontal, -1);
        let (_, chrome, _, _) = popover.measure(gtk::Orientation::Vertical, -1);
        let room = (
            pane.0 - 2 * MARGIN - (outer - inner),
            pane.1 / 2 - MARGIN - chrome,
        );
        // Measured as a line all the same, which a named destination's page fills once read.
        caption.set_visible(!caption.label().is_empty());
        let sf = self.view.scale_factor().max(1);
        let scale = self.view.pixels_per_point();
        let asked = match target {
            LinkTarget::Page { page, top } => self
                .view
                .page_size(page)
                .map(|size| (page, size, Band::around(size, top, scale, room))),
            LinkTarget::Uri(_) | LinkTarget::File(_) => None,
        };
        // Another file's page is not read yet: its band is at most as tall as the room's width
        // makes one.
        let tall = match (&asked, &elsewhere) {
            (Some((_, _, fitted)), _) => fitted.size.1,
            (None, Some(_)) => ((room.0 as f32 * TALL) as i32).min(room.1),
            (None, None) => 0,
        };
        if let Some((_, _, fitted)) = asked {
            band.set_size_request(fitted.size.0, fitted.size.1);
        }
        popover.set_position(side(at.1, chrome + tall));
        slide(&popover, pane.0, at.0);

        if let Some((page, size, fitted)) = asked {
            // The page's stand-in under the band meanwhile, as the reading view paints one.
            let low = self
                .view
                .cache()
                .borrow_mut()
                .lowres(page as u32, self.view.theme());
            if let Some(low) = low {
                let (w, h) = (size.0 * fitted.scale, size.1 * fitted.scale);
                let at = graphene::Rect::new(0.0, -fitted.offset as f32, w, h);
                paint(&band, fitted.size, &low, at);
            }
            let (device, rect) = fitted.pixels(sf);
            self.ask(Request::Band {
                page,
                scale: device,
                rect,
                theme: self.view.theme(),
            });
        }
        if let (Some(path), LinkTarget::File(to)) = (elsewhere, &target) {
            let fit = (scale, room, sf);
            self.fill_band_elsewhere(path, to, fit, (&popover, &band, &caption), (pane.0, at.0));
        }
        *self.preview.borrow_mut() = Some(Preview {
            popover,
            band,
            asked: asked.map(|(page, _, fitted)| (page, fitted)),
            target,
        });
    }

    /// Fill `band` with the page a link into the PDF at `path` leads to, drawn as `fit` says
    /// ([`super::elsewhere::page`]), and `caption` with its number, once the thread that reads
    /// other files has drawn it; a file that does not read takes the band away. The popover,
    /// wider now, slides back inside the pane `across.0` wide over the link at `across.1`. A
    /// popover gone meanwhile has nothing left to fill.
    fn fill_band_elsewhere(
        &self,
        path: std::path::PathBuf,
        to: &pdf::Elsewhere,
        fit: super::elsewhere::Fit,
        (popover, band, caption): (&gtk::Popover, &gtk::Picture, &gtk::Label),
        across: (i32, i32),
    ) {
        let asked = super::elsewhere::page(path, to.dest.clone(), self.view.theme(), fit);
        let (popover, band, caption) = (popover.downgrade(), band.downgrade(), caption.downgrade());
        glib::spawn_future_local(async move {
            // Dropped for a newer link's: that popover has taken this one's place.
            let Ok(seen) = asked.await else { return };
            let (Some(popover), Some(band), Some(caption)) =
                (popover.upgrade(), band.upgrade(), caption.upgrade())
            else {
                return;
            };
            let Some(seen) = seen else {
                return band.set_visible(false);
            };
            caption.set_label(&format!("Page {}", seen.page + 1));
            caption.set_visible(true);
            put_band(&band, seen.band, &super::cache::texture(seen.image), fit.2);
            slide(&popover, across.0, across.1);
        });
    }

    /// One page's links, once read. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn page_links(&self, page: usize) -> Option<Vec<pdf::Link>> {
        self.links.borrow().get(&page).cloned()
    }

    /// The preview on screen against the PDF pane: the pane's size and the reading view's
    /// pixels per point, the band's size and whether it is still being drawn, and where the
    /// popover's surface is in the pane. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn preview_report(&self) -> String {
        let preview = self.preview.borrow();
        let Some(shown) = preview.as_ref() else {
            return "none".to_string();
        };
        let pane = (self.host.width(), self.host.height());
        let band = (shown.band.paintable()).map(|p| (p.intrinsic_width(), p.intrinsic_height()));
        // The pane's corner in the window's surface, which is what a popup is placed against.
        let corner = self.host.root().and_then(|root| {
            let (dx, dy) = root.surface_transform();
            let at = self.host.compute_point(&root, &graphene::Point::zero())?;
            Some((f64::from(at.x()) + dx, f64::from(at.y()) + dy))
        });
        let surface = shown.popover.surface().and_downcast::<gdk::Popup>();
        let placed = surface.zip(corner).map(|(s, (cx, cy))| {
            let (x, y) = (s.position_x() - cx as i32, s.position_y() - cy as i32);
            (x, y, s.width(), s.height())
        });
        let inside = placed
            .is_some_and(|(x, y, w, h)| x >= 0 && y >= 0 && x + w <= pane.0 && y + h <= pane.1);
        format!(
            "pane={}x{} scale={:.3} band={band:?} waiting={} side={:?} popover={placed:?} \
             inside={inside}",
            pane.0,
            pane.1,
            self.view.pixels_per_point(),
            shown.asked.is_some(),
            shown.popover.position(),
        )
    }
}

/// Which side of a link at `y` down the pane a popover `height` tall goes: above it, unless it
/// would run past the pane's top. A PDF's band is at most half the pane, so below then has the
/// room.
pub(crate) fn side(y: i32, height: i32) -> gtk::PositionType {
    if y - MARGIN >= height {
        gtk::PositionType::Top
    } else {
        gtk::PositionType::Bottom
    }
}

/// Slide `popover`, centred over a link `x` across a pane `pane_w` wide, to keep it inside the
/// pane. The arrow still points at the link.
pub(crate) fn slide(popover: &gtk::Popover, pane_w: i32, x: i32) {
    let (_, width, _, _) = popover.measure(gtk::Orientation::Horizontal, -1);
    popover.set_offset(slid(pane_w, x, width), 0);
}

/// How far a popover `width` wide, centred over `x`, has to move to stay inside a pane `pane_w`
/// wide.
fn slid(pane_w: i32, x: i32, width: i32) -> i32 {
    let left = x - width / 2;
    left.clamp(MARGIN, (pane_w - MARGIN - width).max(MARGIN)) - left
}

/// Show `band`'s rendered `texture`, `sf` device pixels to a logical one.
fn put_band(picture: &gtk::Picture, band: Band, texture: &gdk::MemoryTexture, sf: i32) {
    let (w, h) = (texture.width() as f32, texture.height() as f32);
    let at = graphene::Rect::new(0.0, 0.0, w / sf as f32, h / sf as f32);
    paint(picture, band.size, texture, at);
}

/// Paint `texture` into `picture` over `at`, cut to a band `size` logical pixels large: the band
/// itself, or a page's stand-in stretched under it.
fn paint(
    picture: &gtk::Picture,
    size: (i32, i32),
    texture: &gdk::MemoryTexture,
    at: graphene::Rect,
) {
    let (w, h) = (size.0 as f32, size.1 as f32);
    let snapshot = gtk::Snapshot::new();
    snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, w, h));
    snapshot.append_texture(texture, &at);
    snapshot.pop();
    // A paintable of the band's logical size: a texture's own is its pixels, which a display of
    // two device pixels to one would show twice as large.
    picture.set_paintable(
        snapshot
            .to_paintable(Some(&graphene::Size::new(w, h)))
            .as_ref(),
    );
}

#[cfg(test)]
mod tests {
    use super::{Band, MARGIN, PT_TO_PX, slid};

    /// At the reading view's zoom a band is as wide as the page and centred on the destination,
    /// never running off either end of the page.
    #[test]
    fn a_band_is_at_the_zoom_and_centred_on_the_destination() {
        let (page, room) = ((600.0, 800.0), (2000, 1000));
        let band = |top| Band::around(page, top, 2.0, room);
        assert_eq!(band(Some(400.0)).scale, 2.0);
        assert_eq!(band(Some(400.0)).size, (1200, 450));
        // Halfway down the page, so the band is centred on the middle of it.
        assert_eq!(band(Some(400.0)).offset, 800 - 225);
        // The top of the page, and a destination with no y at all, both start at the top.
        assert_eq!(band(Some(0.0)).offset, 0);
        assert_eq!(band(None).offset, 0);
        // The last line shows the foot of the page rather than a strip of nothing under it.
        assert_eq!(band(Some(800.0)).offset, 1600 - 450);
        // A page shorter than the band is the band.
        let slide = Band::around((600.0, 100.0), Some(50.0), 2.0, room);
        assert_eq!((slide.size, slide.offset), ((1200, 200), 0));
    }

    /// A page wider than the room is drawn smaller to fit it, and a band taller than the room is
    /// cut down to it. A view zoomed out past 100 % gets its band at 100 %, unless that is too
    /// wide for the room.
    #[test]
    fn a_band_fits_the_room() {
        let band = Band::around((600.0, 800.0), None, 2.0, (900, 1000));
        assert_eq!((band.scale, band.size), (1.5, (900, 338)));
        let band = Band::around((600.0, 800.0), None, 0.4, (2000, 1000));
        assert_eq!((band.scale, band.size), (PT_TO_PX, (800, 300)));
        let band = Band::around((600.0, 800.0), None, 0.4, (600, 1000));
        assert_eq!((band.scale, band.size), (1.0, (600, 225)));
        let band = Band::around((600.0, 800.0), None, 2.0, (2000, 300));
        assert_eq!((band.scale, band.size), (2.0, (1200, 300)));
        // Device pixels: twice as many of everything on a display of two to one.
        assert_eq!(band.pixels(2), (4.0, (0, 0, 2400, 600)));
    }

    /// A popover too close to a side of the pane slides in until it keeps the margin; one too
    /// wide for the pane at all keeps it on the left.
    #[test]
    fn a_popover_slides_inside_the_pane() {
        assert_eq!(slid(1000, 500, 400), 0);
        assert_eq!(slid(1000, 100, 400), MARGIN + 100);
        assert_eq!(slid(1000, 950, 400), 1000 - MARGIN - 400 - 750);
        assert_eq!(slid(300, 150, 400), MARGIN + 50);
    }
}
