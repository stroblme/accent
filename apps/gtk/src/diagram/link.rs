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

    /// The popover over widget point `(x, y)` saying where `to` leads, up, which a click on goes
    /// there.
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
        match to {
            Link::Page(page) => {
                // The thumbnail, where the Outline pane has drawn the strip.
                if let Some(texture) = self.strip.picture(*page) {
                    let picture = gtk::Picture::for_paintable(&texture);
                    picture.set_can_shrink(false);
                    content.append(&picture);
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
        let popover = gtk::Popover::builder()
            .autohide(false)
            .position(gtk::PositionType::Top)
            .child(&button)
            .build();
        popover.set_parent(&host);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(
            at.x() as i32,
            at.y() as i32,
            1,
            1,
        )));
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
        popover.popup();
        Some(popover)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
