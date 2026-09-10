//! The Outline pane: a document's headings, or a PDF's bookmarks, as rows that jump.

use crate::widgets::{label_factory, row_text, scroller, status_page};
use adw::prelude::*;
use gtk::pango;
use std::cell::RefCell;
use std::rc::Rc;

/// The pane's icon, and the one its empty states are drawn with.
pub(super) const ICON: &str = "view-list-bullet-symbolic";

/// How far each heading level is indented in the Outline pane, on the 6/12/18 spacing scale.
const INDENT: i32 = 12;

/// What the Outline pane says with nothing to outline.
pub(super) fn empty() -> gtk::Widget {
    status_page(
        ICON,
        "No Outline",
        "Open a file to see its headings, symbols or bookmarks.",
    )
    .upcast()
}

/// A sentence in the Outline pane's own shape, for a tab that has no outline to give.
pub fn outline_note(title: &str, body: &str) -> gtk::Widget {
    status_page(ICON, title, body).upcast()
}

/// An outline as rows that jump: `(level, text, where a click goes)`, for a document whose
/// outline does not change while it is read. [`List`] is the one that can be refilled.
pub fn outline_list<T: Copy + 'static>(
    rows: &[(u8, String, T)],
    on_jump: impl Fn(T) + 'static,
) -> gtk::Widget {
    let list = List::new("");
    list.fill(rows, on_jump);
    list.scroller.upcast()
}

/// The one run of rows a refill changes: where it starts, how many old rows it covers and how
/// many new ones replace them. Everything before and after it is left alone.
fn changed_run<T: PartialEq>(old: &[T], new: &[T]) -> (usize, usize, usize) {
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    (head, old.len() - head - tail, new.len() - head - tail)
}

/// What activating a row does, by the row's position. Shared with the list's `activate` handler,
/// which clones it out before it runs: a jump moves the focus, and what that sets off may refill
/// the list.
type Jump = Rc<RefCell<Rc<dyn Fn(u32)>>>;

/// An outline list that is refilled in place, so a document that changes under the reader keeps
/// the pane where it was scrolled to: a list built afresh is a new scrolled window, at the top.
///
/// Generic in what a row jumps to, because the two callers mean different things by it: a text
/// tab's symbols carry a position in the buffer and a PDF's bookmarks carry a page number.
///
/// Indented by level rather than nested in a tree: an outline is read top to bottom, and an
/// expander per row would hide exactly what the pane exists to show.
pub(super) struct List {
    /// The document the rows are of.
    pub(super) key: String,
    pub(super) scroller: gtk::ScrolledWindow,
    model: gtk::StringList,
    /// Each row's level and text as shown: what a refill is compared with, and where a row being
    /// bound reads its indent.
    rows: Rc<RefCell<Vec<(u8, String)>>>,
    /// Replaced on every refill, because an edit moves where the rows jump to even when they read
    /// the same.
    jump: Jump,
}

impl List {
    pub(super) fn new(key: &str) -> List {
        let model = gtk::StringList::new(&[]);
        let rows: Rc<RefCell<Vec<(u8, String)>>> = Rc::default();
        let jump: Jump = Rc::new(RefCell::new(Rc::new(|_| {})));

        let factory = label_factory(pango::EllipsizeMode::End, {
            let rows = rows.clone();
            move |label, item| {
                if let Some(text) = row_text(item) {
                    label.set_text(&text);
                    let level = rows
                        .borrow()
                        .get(item.position() as usize)
                        .map_or(1, |(level, _)| *level);
                    label.set_margin_start(INDENT * i32::from(level.saturating_sub(1)));
                }
            }
        });

        let view = gtk::ListView::new(
            Some(gtk::SingleSelection::new(Some(model.clone()))),
            Some(factory),
        );
        view.add_css_class("navigation-sidebar");
        view.set_single_click_activate(true);
        view.connect_activate({
            let jump = jump.clone();
            move |_, row| {
                let jump = jump.borrow().clone();
                jump(row);
            }
        });
        List {
            key: key.to_string(),
            scroller: scroller(&view),
            model,
            rows,
            jump,
        }
    }

    /// Show `rows`, splicing in only the run that differs from what is shown.
    pub(super) fn fill<T: Copy + 'static>(
        &self,
        rows: &[(u8, String, T)],
        on_jump: impl Fn(T) + 'static,
    ) {
        let shown: Vec<(u8, String)> = rows
            .iter()
            .map(|(level, text, _)| (*level, text.clone()))
            .collect();
        let (at, removed, added) = changed_run(&self.rows.borrow(), &shown);
        // Before the splice, because binding the new rows reads their levels from here.
        *self.rows.borrow_mut() = shown;
        let shown = self.rows.borrow();
        let texts: Vec<&str> = shown[at..at + added]
            .iter()
            .map(|(_, text)| text.as_str())
            .collect();
        self.model.splice(at as u32, removed as u32, &texts);

        let targets: Vec<T> = rows.iter().map(|(_, _, target)| *target).collect();
        *self.jump.borrow_mut() = Rc::new(move |row| {
            if let Some(target) = targets.get(row as usize) {
                on_jump(*target);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::changed_run;

    #[test]
    fn a_refill_touches_only_the_rows_that_changed() {
        assert_eq!(changed_run(&[1, 2, 3], &[1, 2, 3]), (3, 0, 0), "nothing");
        assert_eq!(changed_run(&[1, 2, 3], &[1, 9, 3]), (1, 1, 1), "one row");
        assert_eq!(changed_run(&[1, 3], &[1, 2, 3]), (1, 0, 1), "one added");
        assert_eq!(changed_run(&[1, 2, 3], &[1, 3]), (1, 1, 0), "one removed");
        assert_eq!(changed_run(&[1, 1], &[1, 1, 1]), (2, 0, 1), "repeats");
        assert_eq!(changed_run(&[], &[1, 2]), (0, 0, 2), "first fill");
    }
}
