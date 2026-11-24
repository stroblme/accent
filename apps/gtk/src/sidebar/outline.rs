//! The Outline pane: a document's headings, or a PDF's bookmarks and comments, as rows that jump.

use crate::widgets::{factory, scroller, status_page};
use adw::prelude::*;
use gtk::{glib, pango};
use std::cell::{Cell, RefCell};
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

/// `top` over `below` — a PDF's bookmarks over its thumbnail strip — split where the reader drags
/// it.
fn above(top: &gtk::Widget, below: &gtk::Widget) -> gtk::Widget {
    detach(below);
    let paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Vertical)
        .resize_start_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .start_child(top)
        .end_child(below)
        .build();
    paned.upcast()
}

/// Take `strip`, a thumbnail strip kept for its document's whole life, out of wherever it was,
/// under rows or in the pane alone, so it can go elsewhere: a widget with two parents is a GTK
/// critical.
pub(super) fn detach(strip: &gtk::Widget) {
    let Some(parent) = strip.parent() else {
        return;
    };
    if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        paned.set_end_child(gtk::Widget::NONE);
    } else if let Some(bin) = parent.downcast_ref::<adw::Bin>() {
        bin.set_child(gtk::Widget::NONE);
    } else {
        strip.unparent();
    }
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

/// Where `row` is once the `removed` rows at `at` are replaced by `added` new ones: moved along
/// when the change is above it, kept when its own text changed, and gone when it was removed.
fn spliced(row: u32, at: usize, removed: usize, added: usize) -> u32 {
    let r = row as usize;
    if row == gtk::INVALID_LIST_POSITION || r < at {
        row
    } else if r >= at + removed {
        (r + added - removed) as u32
    } else if r < at + added {
        row
    } else {
        gtk::INVALID_LIST_POSITION
    }
}

/// What a row shows: how far in it is, its text, and a dim word before the text, a comment's
/// author. Level 0 is a heading over the rows after it, dim as the Search pane's, which opens
/// nothing.
#[derive(Clone, Default, PartialEq)]
pub struct Line {
    pub level: u8,
    pub lead: String,
    pub text: String,
}

impl Line {
    pub fn new(level: u8, text: String) -> Line {
        Line {
            level,
            text,
            lead: String::new(),
        }
    }
}

/// The line a text file's row shows, its heading's or its symbol's, counted from one.
pub fn line_number(at: accent_api::Pos) -> Option<usize> {
    Some(at.line as usize + 1)
}

/// What activating a row does, by the row's position. Shared with the list's `activate` handler,
/// which clones it out before it runs: a jump moves the focus, and what that sets off may refill
/// the list.
type Jump = Rc<RefCell<Rc<dyn Fn(u32)>>>;

/// The page or line number a row shows while the pointer is on it, by the row's position, counted
/// from one, or `None` for a row without. Replaced on every refill, as [`Jump`] is.
type Number = Rc<RefCell<Rc<dyn Fn(u32) -> Option<usize>>>>;

/// Each row widget's list item and number label, which a refill numbers again.
type Numbered = Rc<RefCell<Vec<(glib::WeakRef<gtk::ListItem>, glib::WeakRef<gtk::Label>)>>>;

/// Outline list refilled in place to retain scroll position. Generic jump targets support text
/// positions, PDF bookmarks and diagram pages. Use level indentation rather than expanders so
/// the full outline remains visible.
pub(super) struct List {
    /// The document the rows are of.
    pub(super) key: String,
    /// PDF thumbnail strip shown below the rows and shared with the document.
    pub(super) below: Option<gtk::Widget>,
    /// What the pane shows: the rows, over `below` when there is one.
    pub(super) root: gtk::Widget,
    model: gtk::StringList,
    /// Each row as shown: what a refill is compared with, and where a row being bound reads its
    /// indent and its lead.
    rows: Rc<RefCell<Vec<Line>>>,
    /// Replaced on every refill, because an edit moves where the rows jump to even when they read
    /// the same.
    jump: Jump,
    number: Number,
    /// The row widgets made so far, which a refill whose text left them unbound numbers again.
    numbered: Numbered,
    view: gtk::ListView,
    selection: gtk::SingleSelection,
    /// The row the caret is in, which the selection shows. Shared with the pointer-leave handler.
    followed: Rc<Cell<u32>>,
    /// Row to bring into view; it may differ from the selection (see [`List::follow`]).
    shown: Rc<Cell<u32>>,
    /// A scroll is waiting for the list's first layout.
    waiting: Rc<Cell<bool>>,
    /// The pointer is over the list, which is then the reader's: see [`List::follow`].
    hovered: Rc<Cell<bool>>,
}

impl List {
    /// The outline of the document `key`, its rows dragged as references to it through
    /// `dragging` ([`drag`]).
    pub(super) fn new(key: &str, below: Option<gtk::Widget>, dragging: &Rc<dyn Fn(bool)>) -> List {
        let model = gtk::StringList::new(&[]);
        let rows: Rc<RefCell<Vec<Line>>> = Rc::default();
        let jump: Jump = Rc::new(RefCell::new(Rc::new(|_| {})));
        let number: Number = Rc::new(RefCell::new(Rc::new(|_| None)));
        let numbered: Numbered = Rc::default();
        let followed = Rc::new(Cell::new(gtk::INVALID_LIST_POSITION));
        let drag = drag(key, &rows, dragging);

        let factory = factory(
            {
                let numbered = numbered.clone();
                move |item| {
                    let lead = gtk::Label::builder()
                        .xalign(0.0)
                        .max_width_chars(16)
                        .ellipsize(pango::EllipsizeMode::End)
                        .css_classes(["dimmed"])
                        .build();
                    let text = gtk::Label::builder()
                        .xalign(0.0)
                        .hexpand(true)
                        .ellipsize(pango::EllipsizeMode::End)
                        .build();
                    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    line.append(&lead);
                    line.append(&text);
                    // Laid over the end of the title, on a backing of the row's own tint, and
                    // shown only while the pointer is on the row (`.accent-row-number`): the
                    // title keeps its room, so nothing moves.
                    let numeral = gtk::Label::builder()
                        .css_classes(["caption", "dimmed", "numeric"])
                        .build();
                    let backing = gtk::Box::builder()
                        .halign(gtk::Align::End)
                        .can_target(false)
                        .css_classes(["accent-row-number"])
                        .build();
                    backing.append(&numeral);
                    let row = gtk::Overlay::builder().child(&line).build();
                    row.add_overlay(&backing);
                    numbered
                        .borrow_mut()
                        .push((item.downgrade(), numeral.downgrade()));
                    if let Some(drag) = &drag {
                        drag.attach(&row, item);
                    }
                    row
                }
            },
            {
                let (rows, number) = (rows.clone(), number.clone());
                move |row: &gtk::Overlay, item| {
                    let label = |w: Option<gtk::Widget>| w.and_downcast::<gtk::Label>();
                    let content = row.child();
                    let (Some(lead), Some(text), Some(numeral)) = (
                        label(content.as_ref().and_then(|c| c.first_child())),
                        label(content.as_ref().and_then(|c| c.last_child())),
                        label(row.last_child().and_then(|b| b.first_child())),
                    ) else {
                        return;
                    };
                    let number_of = number.borrow().clone();
                    show_number(&numeral, number_of(item.position()));
                    let rows = rows.borrow();
                    let Some(line) = rows.get(item.position() as usize) else {
                        return;
                    };
                    text.set_text(&line.text);
                    lead.set_text(&line.lead);
                    lead.set_visible(!line.lead.is_empty());
                    row.set_margin_start(INDENT * i32::from(line.level.saturating_sub(1)));
                    // A recycled row may have been a heading, so every row sets all four. A
                    // heading starts a group, so it stands off the rows above it.
                    let heading = line.level == 0;
                    item.set_activatable(!heading);
                    item.set_selectable(!heading);
                    row.set_margin_top(if heading { 12 } else { 0 });
                    text.set_css_classes(match heading {
                        true => &["heading", "dimmed"],
                        false => &[],
                    });
                }
            },
        );

        // Nothing selected until the caret is in a section: the selection says where it is.
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let view = gtk::ListView::new(None::<gtk::SingleSelection>, Some(factory));
        crate::widgets::set_model(&view, &selection);
        view.add_css_class("navigation-sidebar");
        view.set_single_click_activate(true);
        view.connect_activate({
            let jump = jump.clone();
            move |_, row| {
                let jump = jump.borrow().clone();
                jump(row);
            }
        });
        // Single-click activate also selects on hover, so the caret's row is put back once the
        // pointer leaves, as the file tree does with the open file's.
        let hovered: Rc<Cell<bool>> = Rc::default();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter({
            let hovered = hovered.clone();
            move |_, _, _| hovered.set(true)
        });
        motion.connect_leave({
            let (selection, followed, hovered) =
                (selection.clone(), followed.clone(), hovered.clone());
            move |_| {
                hovered.set(false);
                select(&selection, followed.get());
            }
        });
        view.add_controller(motion);
        let rows_widget: gtk::Widget = scroller(&view).upcast();
        List {
            key: key.to_string(),
            root: match &below {
                Some(below) => above(&rows_widget, below),
                None => rows_widget,
            },
            below,
            model,
            rows,
            jump,
            number,
            numbered,
            view,
            selection,
            followed,
            shown: Rc::new(Cell::new(gtk::INVALID_LIST_POSITION)),
            waiting: Rc::default(),
            hovered,
        }
    }

    /// Select `row` and minimally reveal `shown` without activating or moving keyboard focus;
    /// `None` clears selection or requests no scroll. Defer both while the pointer is over the
    /// list so navigation cannot move a clicked row away, then follow again on leave.
    pub(super) fn follow(&self, row: Option<usize>, shown: Option<usize>) {
        let position = |row: Option<usize>| row.map_or(gtk::INVALID_LIST_POSITION, |r| r as u32);
        self.followed.set(position(row));
        self.shown.set(position(shown));
        if self.hovered.get() {
            return;
        }
        select(&self.selection, self.followed.get());
        if self.view.height() > 0 {
            reveal(&self.view, self.shown.get());
        } else if !self.waiting.replace(true) {
            // Not laid out yet — the list of a tab just switched to — so there is no viewport to
            // scroll against, and GTK would leave the list at the top. The first frame that has
            // one scrolls to wherever the caret is by then.
            let (shown, waiting) = (self.shown.clone(), self.waiting.clone());
            self.view.add_tick_callback(move |view, _| {
                if view.height() == 0 {
                    return glib::ControlFlow::Continue;
                }
                waiting.set(false);
                reveal(view, shown.get());
                glib::ControlFlow::Break
            });
        }
    }

    /// Show `rows`, splicing in only the run that differs from what is shown, each numbered by
    /// `number_of` while the pointer is on it.
    /// What activating a row does, as the list's `activate` handler clones it out.
    #[cfg(feature = "bench")]
    pub(super) fn jump(&self) -> Rc<dyn Fn(u32)> {
        self.jump.borrow().clone()
    }

    pub(super) fn fill<T: Copy + 'static>(
        &self,
        rows: &[(Line, T)],
        on_jump: impl Fn(T) + 'static,
        number_of: impl Fn(T) -> Option<usize> + 'static,
    ) {
        let shown: Vec<Line> = rows.iter().map(|(line, _)| line.clone()).collect();
        let (at, removed, added) = changed_run(&self.rows.borrow(), &shown);
        let targets: Rc<[T]> = rows.iter().map(|(_, target)| *target).collect();
        let number: Rc<dyn Fn(u32) -> Option<usize>> = {
            let targets = targets.clone();
            Rc::new(move |row: u32| targets.get(row as usize).copied().and_then(&number_of))
        };
        // Before the splice, because binding the new rows reads them from here.
        *self.rows.borrow_mut() = shown;
        *self.number.borrow_mut() = number.clone();
        let shown = self.rows.borrow();
        let texts: Vec<&str> = shown[at..at + added]
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        self.model.splice(at as u32, removed as u32, &texts);
        // An edit leaves the selection on the row it was on, which the splice would drop when
        // that row's own text changed; only a caret move re-follows.
        self.followed
            .set(spliced(self.followed.get(), at, removed, added));
        select(&self.selection, self.followed.get());
        // A row whose text is unchanged is not bound again, but its number may have moved with a
        // line typed above it. Only the rows on screen have widgets, so this is a few rows.
        self.numbered.borrow_mut().retain(|(item, numeral)| {
            let (Some(item), Some(numeral)) = (item.upgrade(), numeral.upgrade()) else {
                return false;
            };
            show_number(&numeral, number(item.position()));
            true
        });

        *self.jump.borrow_mut() = Rc::new(move |row| {
            if let Some(target) = targets.get(row as usize) {
                on_jump(*target);
            }
        });
    }
}

#[cfg(feature = "bench")]
impl List {
    /// The rows as they read, indented by level, a lead before its text and a heading in
    /// capitals, and the list, to activate one of them on.
    pub(super) fn lines(&self) -> (Vec<String>, gtk::ListView) {
        let lines = self.rows.borrow();
        let lines = lines
            .iter()
            .map(|l| match l.level {
                0 => l.text.to_uppercase(),
                level => {
                    let indent = "  ".repeat(usize::from(level - 1));
                    let lead = (!l.lead.is_empty()).then(|| format!("[{}] ", l.lead));
                    format!("{indent}{}{}", lead.unwrap_or_default(), l.text)
                }
            })
            .collect();
        (lines, self.view.clone())
    }
}

/// An Outline row dragged as a reference: a link to its heading where `key` is a note of the
/// vault, to the document itself where it is anything else of the vault (a code symbol, a
/// bookmark), and none from a group's title or a file outside it.
fn drag(
    key: &str,
    rows: &Rc<RefCell<Vec<Line>>>,
    dragging: &Rc<dyn Fn(bool)>,
) -> Option<crate::refer::RowDrag> {
    if crate::doc::is_loose_key(key) {
        return None;
    }
    let (key, rows) = (key.to_string(), rows.clone());
    let note = accent_core::path::is_note(&key);
    Some(crate::refer::RowDrag {
        of: Rc::new(move |item| {
            let rows = rows.borrow();
            let line = rows.get(item.position() as usize).filter(|l| l.level > 0)?;
            Some(crate::refer::Dragged {
                rel: key.clone(),
                heading: note.then(|| line.text.clone()),
            })
        }),
        dragging: dragging.clone(),
    })
}

/// Show `number` on a row's number label, or hide the label and its backing where there is none.
fn show_number(numeral: &gtk::Label, number: Option<usize>) {
    numeral.set_text(&number.map_or_else(String::new, |n| n.to_string()));
    if let Some(backing) = numeral.parent() {
        backing.set_visible(number.is_some());
    }
}

/// Put the selection on `row`, leaving it alone when it is already there.
fn select(selection: &gtk::SingleSelection, row: u32) {
    if selection.selected() != row {
        selection.set_selected(row);
    }
}

/// Scroll `row` into view, as little as it takes, without selecting or focusing it.
fn reveal(view: &gtk::ListView, row: u32) {
    if row != gtk::INVALID_LIST_POSITION {
        view.scroll_to(row, gtk::ListScrollFlags::NONE, None);
    }
}

#[cfg(test)]
mod tests {
    use super::{changed_run, spliced};

    #[test]
    fn a_refill_touches_only_the_rows_that_changed() {
        assert_eq!(changed_run(&[1, 2, 3], &[1, 2, 3]), (3, 0, 0), "nothing");
        assert_eq!(changed_run(&[1, 2, 3], &[1, 9, 3]), (1, 1, 1), "one row");
        assert_eq!(changed_run(&[1, 3], &[1, 2, 3]), (1, 0, 1), "one added");
        assert_eq!(changed_run(&[1, 2, 3], &[1, 3]), (1, 1, 0), "one removed");
        assert_eq!(changed_run(&[1, 1], &[1, 1, 1]), (2, 0, 1), "repeats");
        assert_eq!(changed_run(&[], &[1, 2]), (0, 0, 2), "first fill");
    }

    #[test]
    fn a_refill_keeps_the_selected_row() {
        let none = gtk::INVALID_LIST_POSITION;
        assert_eq!(spliced(1, 3, 1, 2), 1, "a change below");
        assert_eq!(spliced(4, 1, 0, 2), 6, "two headings typed above");
        assert_eq!(spliced(4, 1, 1, 0), 3, "one deleted above");
        assert_eq!(spliced(2, 2, 1, 1), 2, "its own heading retyped");
        assert_eq!(spliced(2, 2, 1, 0), none, "its heading deleted");
        assert_eq!(spliced(none, 0, 1, 1), none, "nothing selected");
    }
}
