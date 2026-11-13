//! A shape's link, draw.io's `link`: followed by a click on a presented page and by Ctrl+click
//! while editing, and shown where it leads while Ctrl is held over it, as a PDF's links are.

use std::cell::RefCell;
use std::rc::Rc;

use accent_drawio::File;
use accent_drawio::model::attr;
use adw::prelude::*;
use gtk::glib;

use super::DiagramTab;
use crate::organize::Thumbnails;

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

impl DiagramTab {
    /// Go where `link` leads: its page as a jump, or its address opened by the window.
    pub(super) fn follow(self: &Rc<Self>, link: &str) {
        let to = target(self.editor.borrow().file(), link);
        match to {
            Some(Link::Page(page)) => self.goto_page(page),
            Some(Link::Uri(uri)) => {
                if let Some(f) = self.on_uri.get() {
                    f(&uri);
                }
            }
            None => {}
        }
    }

    pub fn connect_uri(&self, f: impl Fn(&str) + 'static) {
        self.on_uri.set(Rc::new(f));
    }

    /// Ctrl over a shape with a link: a popover says where it leads, the page in thumbnail and
    /// by name, or the address. Anything else takes it away.
    pub(super) fn wire_link_preview(self: &Rc<Self>) {
        let shown: Rc<RefCell<Option<(gtk::Popover, Link)>>> = Rc::default();
        let hide = |shown: &RefCell<Option<(gtk::Popover, Link)>>| {
            if let Some((popover, _)) = shown.take() {
                popover.popdown();
            }
        };
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[strong]
            shown,
            move |controller, x, y| {
                let ctrl = controller.current_event_state();
                let held = ctrl.contains(crate::platform::PRIMARY_MODIFIER);
                let to = held
                    .then(|| tab.view.link_at(x, y))
                    .flatten()
                    .and_then(|(_, link)| target(tab.editor.borrow().file(), &link));
                // Still the same link: the popover already says where it leads.
                if shown.borrow().as_ref().map(|(_, at)| at) == to.as_ref() {
                    return;
                }
                hide(&shown);
                if let Some(to) = to {
                    let popover = tab.link_popover(&to, x, y);
                    *shown.borrow_mut() = popover.map(|p| (p, to));
                }
            }
        ));
        // The pointer left the page, or the tab went with the popover up.
        motion.connect_leave(glib::clone!(
            #[strong]
            shown,
            move |_| hide(&shown)
        ));
        self.view.add_controller(motion);
        self.view.connect_unmap(move |_| hide(&shown));
    }

    /// The popover over widget point `(x, y)` saying where `to` leads, up.
    fn link_popover(&self, to: &Link, x: f64, y: f64) -> Option<gtk::Popover> {
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
        // Parented to the tab's box, not the canvas, which allocates itself (`menu_at`). Never
        // autohiding and not targeted, as a PDF's: it is under a pointer still reading the page.
        let host = self.overlay.parent().and_downcast::<gtk::Box>()?;
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = self.view.compute_point(&host, &at).unwrap_or(at);
        let popover = gtk::Popover::builder()
            .autohide(false)
            .can_target(false)
            .position(gtk::PositionType::Top)
            .child(&content)
            .build();
        popover.set_parent(&host);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            at.x() as i32,
            at.y() as i32,
            1,
            1,
        )));
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
