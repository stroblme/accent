//! What other readers wrote on a page — an annotation's `/Contents` — as the reading view's
//! tooltip while the pointer is over it, in a popover a click pins, where it can be selected and
//! copied, and as the Outline pane's list of every comment in the document.

use std::rc::Rc;

use accent_core::pdf;
use adw::prelude::*;
use gtk::{gdk, glib, pango};

use super::Mode;
use super::tab::PdfTab;

/// How many lines of a comment a tooltip shows: it is a glance, and the pinned popover has it all.
const TIP_LINES: i32 = 12;

impl PdfTab {
    /// The reading view's tooltip: the comments under the pointer.
    pub(super) fn wire_comments(self: &Rc<Self>) {
        self.view.set_has_tooltip(true);
        self.view.connect_query_tooltip(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            false,
            move |_, x, y, keyboard, tooltip| {
                // A comment is somewhere on the page, not where the keyboard is.
                if keyboard {
                    return false;
                }
                let Some((content, area)) = tab.tip(x.into(), y.into()) else {
                    return false;
                };
                tooltip.set_custom(Some(&content));
                // Off it the tooltip goes, and comes back for the next comment where it is.
                tooltip.set_tip_area(&area);
                true
            }
        ));
    }

    /// What the tooltip shows at a point of the reading view, and the area of the page it holds
    /// over. Nothing while a popover is pinned: it says the same, and a tooltip would cover it.
    ///
    /// GTK asks again on every motion, so the content is built once for the comments it shows
    /// and handed back while the pointer stays on them.
    fn tip(&self, x: f64, y: f64) -> Option<(gtk::Widget, gdk::Rectangle)> {
        if self.pinned.borrow().is_some() {
            return None;
        }
        let (page, hit) = self.comments_at(x, y)?;
        let area = hit
            .iter()
            .flat_map(|c| c.areas.iter().copied())
            .reduce(pdf::Rect::union)?;
        let area = self.view.widget_rect(page, &area)?;
        let mut tip = self.tip.borrow_mut();
        let content = match tip.as_ref() {
            Some((shown, content)) if *shown == hit => content.clone(),
            _ => {
                let content = card(&hit, false);
                *tip = Some((hit, content.clone()));
                content
            }
        };
        Some((content, area))
    }

    /// The comments under a point of the reading view, and their page. None with a tool in hand,
    /// which the page belongs to, or over a link, which a click follows.
    fn comments_at(&self, x: f64, y: f64) -> Option<(usize, Vec<pdf::Comment>)> {
        if self.view.mode() != Mode::Select || self.link_at(&self.view, x, y).is_some() {
            return None;
        }
        let (page, px, py) = self.view.page_point(x, y)?;
        let hit: Vec<pdf::Comment> = self
            .comments
            .borrow()
            .get(&page)?
            .iter()
            .filter(|c| c.areas.iter().any(|a| a.contains((px, py))))
            .cloned()
            .collect();
        (!hit.is_empty()).then_some((page, hit))
    }

    /// A click that was not a drag, on no link and no note's highlight: pin the comments under it
    /// in a popover. It closes as a popover does, on `Escape` or a click elsewhere.
    pub(super) fn pin_comments(self: &Rc<Self>, x: f64, y: f64) {
        if let Some((_, hit)) = self.comments_at(x, y) {
            self.pin(&hit, x, y);
        }
    }

    /// The Outline pane's rows for the comments of every page read so far, under a "Comments"
    /// heading and their page's row, each its author and its first line; none for a document
    /// without. Each row jumps to its page, and to a comment by its place on the page.
    pub fn comment_rows(&self) -> Vec<(crate::sidebar::Line, (usize, Option<usize>))> {
        use crate::sidebar::Line;
        let comments = self.comments.borrow();
        let mut pages: Vec<usize> = comments
            .iter()
            .filter(|(_, on)| !on.is_empty())
            .map(|(page, _)| *page)
            .collect();
        if pages.is_empty() {
            return Vec::new();
        }
        pages.sort_unstable();
        let mut rows = vec![(Line::new(0, "Comments".to_string()), (0, None))];
        for page in pages {
            rows.push((Line::new(1, format!("Page {}", page + 1)), (page, None)));
            for (at, comment) in comments[&page].iter().enumerate() {
                let first = comment.text.lines().map(str::trim).find(|l| !l.is_empty());
                let line = Line {
                    level: 2,
                    lead: comment.author.clone().unwrap_or_default(),
                    text: first.unwrap_or_default().to_string(),
                };
                rows.push((line, (page, Some(at))));
            }
        }
        rows
    }

    /// The `at`th comment on `page`, brought into view and pinned as a click on it pins it: the
    /// Outline pane's row for it.
    pub fn show_comment(self: &Rc<Self>, page: usize, at: usize) {
        let comment = self
            .comments
            .borrow()
            .get(&page)
            .and_then(|c| c.get(at))
            .cloned();
        let Some(comment) = comment else {
            return;
        };
        let Some(area) = comment.areas.iter().copied().reduce(pdf::Rect::union) else {
            return;
        };
        self.jumping();
        self.view.reveal(page, area);
        if let Some(r) = self.view.widget_rect(page, &area) {
            let (x, y) = (r.x() + r.width() / 2, r.y() + r.height() / 2);
            self.pin(&[comment], x.into(), y.into());
        }
    }

    /// Pin `comments` in a popover pointing at a point of the reading view, in place of any
    /// pinned before.
    fn pin(self: &Rc<Self>, comments: &[pdf::Comment], x: f64, y: f64) {
        let old = self.pinned.borrow_mut().take();
        if let Some(old) = old {
            old.popdown();
        }
        let popover = gtk::Popover::builder()
            .position(gtk::PositionType::Top)
            .child(&card(comments, true))
            .build();
        // On the box, at its own coordinates, as the page's menu is (`selection_menu`).
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = self.view.compute_point(&self.host, &at).unwrap_or(at);
        popover.set_parent(&self.host);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(
            at.x() as i32,
            at.y() as i32,
            1,
            1,
        )));
        popover.connect_closed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |popover| {
                popover.unparent();
                tab.pinned
                    .borrow_mut()
                    .take_if(|pinned| *pinned == *popover);
            }
        ));
        popover.popup();
        // The text takes the focus as the popover opens, and a label taking it selects all of
        // itself: the reader selects what they want.
        let focus = popover.root().and_then(|root| root.focus());
        if let Some(label) = focus.and_downcast::<gtk::Label>() {
            label.select_region(0, 0);
        }
        *self.pinned.borrow_mut() = Some(popover);
    }

    /// One page's comments, once read. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn page_comments(&self, page: usize) -> Option<Vec<pdf::Comment>> {
        self.comments.borrow().get(&page).cloned()
    }

    /// How many pages' comments have been read, and the popover pinned. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn comments_read(&self) -> (usize, Option<gtk::Popover>) {
        (self.comments.borrow().len(), self.pinned.borrow().clone())
    }

    /// What the tooltip shows at a point of the reading view. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn hover_at(&self, x: f64, y: f64) -> Option<gtk::Widget> {
        self.tip(x, y).map(|(content, _)| content)
    }

    /// A click at a point of the reading view that was not a drag, and the popover it left
    /// pinned. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn click_at(self: &Rc<Self>, x: f64, y: f64) -> Option<gtk::Popover> {
        self.clicked_highlight(&self.view, x, y, gdk::ModifierType::empty());
        self.pinned.borrow().clone()
    }
}

/// The comments one above the other, each its author in small dim type over its text. A tooltip's
/// text stops after [`TIP_LINES`]; the pinned popover's is whole, and can be selected.
fn card(comments: &[pdf::Comment], pinned: bool) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    for comment in comments {
        let one = gtk::Box::new(gtk::Orientation::Vertical, 2);
        if let Some(author) = &comment.author {
            one.append(
                &gtk::Label::builder()
                    .label(author)
                    .css_classes(["caption", "dim-label"])
                    .xalign(0.0)
                    .build(),
            );
        }
        let text = gtk::Label::builder()
            .label(&comment.text)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(48)
            .xalign(0.0)
            .selectable(pinned)
            .build();
        if !pinned {
            text.set_lines(TIP_LINES);
            text.set_ellipsize(pango::EllipsizeMode::End);
        }
        one.append(&text);
        column.append(&one);
    }
    column.upcast()
}
