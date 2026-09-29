//! The Ctrl-hover link preview: where a link goes, shown without going there.

use std::rc::Rc;

use accent_core::pdf::LinkTarget;
use adw::prelude::*;
use gtk::glib;

use super::protocol::{Asker, Request};
use super::tab::PdfTab;
use super::tab::theme_of;
use super::{LOWRES_W, Want};

/// How tall a link preview's band is, in the low-resolution page's own pixels. Roughly a quarter
/// of a portrait page at [`LOWRES_W`], which is a heading and the lines under it: a
/// whole page shrunk to a popover says nothing a reader can read.
const BAND: i32 = 96;

/// The Ctrl-hover link preview currently on screen.
pub(super) struct Preview {
    popover: gtk::Popover,
    /// Where the band of the target page goes. Empty until the render lands, which for a page
    /// nobody has looked at yet is a moment after the popover is up.
    band: adw::Bin,
    /// What it is showing, so a pointer still on the same link asks for nothing again.
    target: LinkTarget,
}

impl PdfTab {
    /// The Ctrl-hover link preview: where a link goes, without going there.
    ///
    /// A controller of its own rather than the cursor hook next door, for two reasons: that hook's
    /// signature drops the controller, and so the modifier state with it, and this one can refuse
    /// the event on the modifier alone. A pointer crossing a page without Ctrl held therefore
    /// costs one bit test and never looks a link up, let alone asks for a render.
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

    /// A page's stand-in landed. The band arrives after the popover does, for any page nobody has
    /// read yet, so a preview up for that page fills in now.
    pub(super) fn band_landed(self: &Rc<Self>, ready: u32) {
        let waiting = match self.preview.borrow().as_ref().map(|p| p.target.clone()) {
            Some(LinkTarget::Page { page, top }) if page as u32 == ready => Some((page, top)),
            _ => None,
        };
        if let Some((page, top)) = waiting {
            self.fill_band(page, top);
        }
    }

    /// Ctrl over a link: show where it leads. Anything else takes the preview away.
    fn hover(self: &Rc<Self>, x: f64, y: f64, state: gtk::gdk::ModifierType) {
        if !state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
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
    /// clicking it.
    fn show_preview(self: &Rc<Self>, target: LinkTarget, x: f64, y: f64) {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let band = adw::Bin::new();
        match &target {
            LinkTarget::Page { page, .. } => {
                band.set_size_request(LOWRES_W, BAND);
                content.append(&band);
                content.append(
                    &gtk::Label::builder()
                        .label(format!("Page {}", page + 1))
                        .css_classes(["caption", "dim-label"])
                        .xalign(0.0)
                        .build(),
                );
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
        }
        // Never autohide: an autohiding popover takes a grab, and this one is under the pointer
        // that is still reading the page. It cannot be targeted either, so it neither swallows a
        // click nor steals the crossing event that would take it away again.
        let popover = gtk::Popover::builder()
            .autohide(false)
            .can_target(false)
            .position(gtk::PositionType::Top)
            .child(&content)
            .build();
        popover.set_parent(&self.host);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        // A popover parented by hand stays parented until it is unparented by hand.
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
        let page = match target {
            LinkTarget::Page { page, top } => Some((page, top)),
            LinkTarget::Uri(_) => None,
        };
        *self.preview.borrow_mut() = Some(Preview {
            popover,
            band,
            target,
        });
        if let Some((page, top)) = page {
            self.fill_band(page, top);
        }
    }

    /// Slide the target page behind the band, rendering its low-resolution stand-in first if this
    /// is a page nobody has looked at.
    ///
    /// Only ever reached from a Ctrl-hover that landed on a link the popover is not already
    /// showing, so the pdfium lock is taken for one 256 px page render at most, and never once
    /// per pointer event.
    fn fill_band(self: &Rc<Self>, page: usize, top: Option<f32>) {
        let dark = self.view.dark();
        let ready = self.view.cache().borrow_mut().lowres(page as u32, dark);
        let Some(texture) = ready else {
            return self.ask(Request::Tiles {
                from: Asker::Preview,
                scale: 1.0,
                dark,
                theme: theme_of(dark),
                wants: vec![Want {
                    page: page as u32,
                    tx: u16::MAX,
                    ty: u16::MAX,
                }],
            });
        };
        let Some((_, page_h)) = self.view.page_size(page) else {
            return;
        };
        let preview = self.preview.borrow();
        let Some(band) = preview.as_ref().map(|shown| shown.band.clone()) else {
            return;
        };
        drop(preview);
        if band.child().is_some() {
            return;
        }
        let offset = band_offset(top, page_h, texture.height() as f32);
        let strip = crop(&texture, offset, BAND);
        let picture = gtk::Picture::for_paintable(&strip);
        picture.set_size_request(strip.width(), strip.height());
        band.set_child(Some(&picture));
    }
}

/// How far to slide a low-resolution page up so the band shows what a link points at.
///
/// `top` is points down the target page, `page_h` its height in points and `texture_h` the
/// stand-in's height in pixels. The destination is centred in the band, and the band stays inside
/// the page at both ends: a link to the last line shows the foot of the page rather than a strip
/// of nothing under it.
fn band_offset(top: Option<f32>, page_h: f32, texture_h: f32) -> i32 {
    let at = top.unwrap_or(0.0) / page_h.max(1.0) * texture_h;
    let band = BAND as f32;
    (at - band / 2.0).clamp(0.0, (texture_h - band).max(0.0)) as i32
}

/// One horizontal band of a texture, as a texture of its own.
///
/// The crop is of the pixels and not of the layout: a clipped widget still asks for the whole
/// page's height, and a popover is as big as what it holds asks to be.
fn crop(texture: &gtk::gdk::MemoryTexture, top: i32, height: i32) -> gtk::gdk::MemoryTexture {
    let (w, h) = (texture.width(), texture.height());
    let height = height.min(h);
    let top = top.clamp(0, h - height);
    let stride = w as usize * 4;
    let mut pixels = vec![0u8; stride * h as usize];
    texture.download(&mut pixels, stride);
    let from = top as usize * stride;
    let bytes = glib::Bytes::from(&pixels[from..from + stride * height as usize]);
    // The layout `GdkTexture::download` writes, on every platform GTK builds for.
    gtk::gdk::MemoryTexture::new(
        w,
        height,
        gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        stride,
    )
}

#[cfg(test)]
mod tests {
    use super::{BAND, band_offset};

    /// The band follows the destination but never runs off either end of the page.
    #[test]
    fn a_band_is_centred_on_the_destination_and_stays_on_the_page() {
        let (page_h, texture_h) = (800.0, 400.0);
        // Halfway down the page, so the band is centred on the middle of the stand-in.
        assert_eq!(band_offset(Some(400.0), page_h, texture_h), 200 - BAND / 2);
        // The top of the page, and a destination with no y at all, both start at the top.
        assert_eq!(band_offset(Some(0.0), page_h, texture_h), 0);
        assert_eq!(band_offset(None, page_h, texture_h), 0);
        // The last line shows the foot of the page rather than a strip of nothing under it.
        assert_eq!(band_offset(Some(800.0), page_h, texture_h), 400 - BAND);
        // A page shorter than the band does not scroll at all.
        assert_eq!(band_offset(Some(400.0), page_h, 50.0), 0);
    }
}
