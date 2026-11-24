//! A shape's link, draw.io's `link`: shown where it leads while Ctrl is held over it, in a popover
//! a click on which follows it, and followed by a click on a presented page, as a PDF's link is.
//! While editing a click on the shape itself selects it, Ctrl+click and Ctrl+drag included.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use accent_drawio::File;
use accent_drawio::model::attr;
use adw::prelude::*;
use gtk::{gdk, glib};

use super::DiagramTab;
use crate::organize::Thumbnails;
use crate::pdf::preview::{paint, room, side, slide};

/// How long the popover waits for the pointer on its way over to it before it goes, in ms.
const LEAVE_MS: u64 = 400;

/// Where a shape's link leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// Page `i` of the diagram, which draw.io names `data:page/id,<the page's id>`.
    Page(usize),
    /// An address another application opens, as a PDF's link to the web.
    Uri(String),
}

/// What `link` leads to in `file`: a page by its id, nowhere for draw.io's other `data:` links
/// (its actions) or a page that is gone, and an address for anything else.
pub fn target(file: &File, link: &str) -> Option<Link> {
    if let Some(id) = link.strip_prefix("data:page/id,") {
        let page = file
            .pages
            .iter()
            .position(|p| attr(&p.attrs, "id") == Some(id));
        return page.map(Link::Page);
    }
    (!link.is_empty() && !link.starts_with("data:")).then(|| Link::Uri(link.to_string()))
}

/// The popover up, with where its link leads, and the leave it waits out while the pointer goes
/// over to it.
#[derive(Default)]
struct Preview {
    shown: RefCell<Option<(gtk::Popover, Link)>>,
    leaving: RefCell<Option<glib::SourceId>>,
}

impl Preview {
    /// The pointer is on the popover or its link again: it stays.
    fn stay(&self) {
        if let Some(leave) = self.leaving.take() {
            leave.remove();
        }
    }

    fn hide(&self) {
        self.stay();
        if let Some((popover, _)) = self.shown.take() {
            popover.popdown();
        }
    }

    /// Hide in [`LEAVE_MS`], unless the pointer reaches the popover or its link first.
    fn hide_soon(self: &Rc<Self>) {
        if self.shown.borrow().is_none() || self.leaving.borrow().is_some() {
            return;
        }
        let preview = Rc::downgrade(self);
        let leave = glib::timeout_add_local_once(Duration::from_millis(LEAVE_MS), move || {
            if let Some(preview) = preview.upgrade() {
                // Gone by now, so never removed again.
                preview.leaving.take();
                preview.hide();
            }
        });
        self.leaving.replace(Some(leave));
    }
}

impl DiagramTab {
    /// Go where `link` leads (a presented page's click on it).
    pub(super) fn follow(self: &Rc<Self>, link: &str) {
        let to = target(self.editor.borrow().file(), link);
        if let Some(to) = to {
            self.go(to);
        }
    }

    /// Go to `to`: its page as a jump, or its address opened by the window.
    fn go(self: &Rc<Self>, to: Link) {
        match to {
            Link::Page(page) => self.goto_page(page),
            Link::Uri(uri) => {
                if let Some(f) = self.on_uri.get() {
                    f(&uri);
                }
            }
        }
    }

    pub fn connect_uri(&self, f: impl Fn(&str) + 'static) {
        self.on_uri.set(Rc::new(f));
    }

    /// Ctrl over a shape with a link, with no button down: a popover says where it leads, the
    /// page in thumbnail and by name or the address, and a click on it goes there. Anything else
    /// takes it away, once the pointer has had [`LEAVE_MS`] to reach it.
    pub(super) fn wire_link_preview(self: &Rc<Self>) {
        let preview = Rc::new(Preview::default());
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[strong]
            preview,
            move |controller, x, y| {
                let state = controller.current_event_state();
                let buttons = gdk::ModifierType::BUTTON1_MASK
                    | gdk::ModifierType::BUTTON2_MASK
                    | gdk::ModifierType::BUTTON3_MASK;
                let reading =
                    state.contains(crate::platform::PRIMARY_MODIFIER) && !state.intersects(buttons);
                let to = reading
                    .then(|| tab.view.link_at(x, y))
                    .flatten()
                    .and_then(|(_, link)| target(tab.editor.borrow().file(), &link));
                let same = preview.shown.borrow().as_ref().map(|(_, at)| at) == to.as_ref();
                match to {
                    Some(_) if same => preview.stay(),
                    Some(to) => {
                        preview.hide();
                        let popover = tab.link_popover(&to, x, y, &preview);
                        *preview.shown.borrow_mut() = popover.map(|p| (p, to));
                    }
                    None => preview.hide_soon(),
                }
            }
        ));
        // The pointer left the page, for the popover perhaps.
        motion.connect_leave(glib::clone!(
            #[strong]
            preview,
            move |_| preview.hide_soon()
        ));
        self.view.add_controller(motion);
        // The tab went with the popover up.
        self.view.connect_unmap(move |_| preview.hide());
    }

    /// The popover at widget point `(x, y)` saying where `to` leads, up, which a click on goes
    /// there. It stays inside the pane as a PDF's does: above the point where it fits, below it
    /// otherwise, and slid along to keep clear of the pane's sides, a page's thumbnail cut to the
    /// room the pane has as a PDF's band is.
    fn link_popover(
        self: &Rc<Self>,
        to: &Link,
        x: f64,
        y: f64,
        preview: &Rc<Preview>,
    ) -> Option<gtk::Popover> {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let caption = |text: &str| {
            gtk::Label::builder()
                .label(text)
                .css_classes(["caption"])
                .wrap(true)
                .max_width_chars(48)
                .xalign(0.0)
                .build()
        };
        let mut thumbnail = None;
        match to {
            Link::Page(page) => {
                // The thumbnail, where the Outline pane has drawn the strip, painted once the
                // popover has been measured without it.
                if let Some(texture) = self.strip.picture(*page) {
                    let picture = gtk::Picture::new();
                    picture.set_can_shrink(false);
                    // In a row of its own, centred above a wider caption, which keeps it at its
                    // own width: given the caption's, a picture grows to fill it, its aspect
                    // kept, past the room.
                    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    row.set_halign(gtk::Align::Center);
                    row.append(&picture);
                    content.append(&row);
                    thumbnail = Some((picture, texture));
                }
                let names = self.page_names();
                content.append(&caption(names.get(*page).map_or("", String::as_str)));
            }
            Link::Uri(uri) => content.append(&caption(uri)),
        }
        // Not taking the keyboard from the page, which goes on reading it.
        let button = gtk::Button::builder()
            .child(&content)
            .css_classes(["flat"])
            .focus_on_click(false)
            .build();
        let go = to.clone();
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[weak]
            preview,
            move |_| {
                preview.hide();
                tab.go(go.clone());
            }
        ));
        // Parented to the tab's box, not the canvas, which allocates itself (`menu_at`). Never
        // autohiding, which would take a grab from the page under the pointer.
        let host = self.overlay.parent().and_downcast::<gtk::Box>()?;
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = self.view.compute_point(&host, &at).unwrap_or(at);
        let at = (at.x() as i32, at.y() as i32);
        let popover = gtk::Popover::builder()
            .autohide(false)
            .child(&button)
            .build();
        popover.set_parent(&host);
        // The pointer's row and one either side of it, as a PDF's, so the popover never starts on
        // it: under X11 the pointer then goes into its edge, and the page's leave takes it away.
        popover.set_pointing_to(Some(&gdk::Rectangle::new(at.0, at.1 - 1, 1, 3)));
        // The pointer on its way over keeps it up.
        let over = gtk::EventControllerMotion::new();
        over.connect_enter(glib::clone!(
            #[weak]
            preview,
            move |_, _, _| preview.stay()
        ));
        over.connect_leave(glib::clone!(
            #[weak]
            preview,
            move |_| preview.hide_soon()
        ));
        popover.add_controller(over);
        // A popover parented by hand stays parented until it is unparented by hand.
        popover.connect_closed(|popover| popover.unparent());
        // Up before it is measured, as a hidden widget measures nothing: placed before its first
        // frame is drawn, as tall as it is at its width, for a caption that wraps.
        popover.popup();
        if let Some((picture, texture)) = thumbnail {
            let (room, _) = room((host.width(), host.height()), &popover, &content);
            let (w, h) = fitted((texture.width(), texture.height()), room);
            let at = gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
            paint(&picture, (w, h), &texture, at);
        }
        let (_, width, _, _) = popover.measure(gtk::Orientation::Horizontal, -1);
        let (_, height, _, _) = popover.measure(gtk::Orientation::Vertical, width);
        popover.set_position(side(at.1, height));
        slide(&popover, host.width(), at.0);
        // Laid out with the popover in it on the next frame, the window asks GDK for a motion
        // where the pointer is, which comes with the next events flushed. Under X11 that motion
        // is the window's even with the pointer over the popover by then, and GTK hands the
        // pointer to the page under it, the popover's enter and click with it (ISSUES.md).
        // Flushed once that frame is done, it lands on the page the pointer is still on.
        popover.add_tick_callback(|_, clock| {
            clock.request_phase(gdk::FrameClockPhase::FLUSH_EVENTS);
            glib::ControlFlow::Break
        });
        Some(popover)
    }
}

/// The size a thumbnail `size` large is shown at in `room`: its own, or smaller to fit, its
/// aspect kept.
fn fitted(size: (i32, i32), room: (i32, i32)) -> (i32, i32) {
    let scale = |room: i32, side: i32| f64::from(room.max(1)) / f64::from(side.max(1));
    let scale = scale(room.0, size.0).min(scale(room.1, size.1)).min(1.0);
    let at = |side: i32| (f64::from(side) * scale).round() as i32;
    (at(size.0), at(size.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thumbnail taller or wider than the room is shown smaller to fit it, its aspect kept, and
    /// one that fits keeps its own size.
    #[test]
    fn a_thumbnail_fits_the_room() {
        assert_eq!(fitted((200, 4000), (500, 250)), (13, 250));
        assert_eq!(fitted((400, 200), (200, 500)), (200, 100));
        assert_eq!(fitted((200, 280), (500, 300)), (200, 280));
    }

    #[test]
    fn a_link_leads_to_its_page_by_id_or_to_its_address() {
        let xml = r#"<mxfile><diagram name="One" id="p1"><mxGraphModel><root/></mxGraphModel></diagram>
            <diagram name="Two" id="p2"><mxGraphModel><root/></mxGraphModel></diagram></mxfile>"#;
        let file = File::from_bytes(xml.as_bytes()).unwrap();
        assert_eq!(target(&file, "data:page/id,p2"), Some(Link::Page(1)));
        assert_eq!(target(&file, "data:page/id,gone"), None);
        assert_eq!(target(&file, "data:action/json,{}"), None);
        let web = "https://example.org/a";
        assert_eq!(target(&file, web), Some(Link::Uri(web.to_string())));
        assert_eq!(target(&file, ""), None);
    }
}
