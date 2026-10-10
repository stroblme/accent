//! Change marks in the editor gutter: which lines differ from what git has committed.
//!
//! The bars are the same green and red the diff view uses, at full alpha because a 3 px column
//! has no room to be subtle. A modified line is the accent instead: it is neither an addition nor
//! a removal, and the accent is the colour this app already means "something here" with.
//!
//! The marks come from the same line diff the comparison tabs are built from, less its word-level
//! pass (`accent_core::diff::line_ops`), so a line the diff paints green is a line the gutter marks
//! green.
//!
//! Beside them, in the same column, a bar in the warning's amber marks the lines a language server
//! has still to check (`Event::Checking`), as VS Code's Lean extension does in orange.

use std::ops::Range;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use sourceview5::subclass::prelude::*;

use accent_core::diff::{DiffLine, Op};

/// Width of the mark column, and of the bar inside it. The checking bar takes the rest.
const WIDTH: i32 = 6;
const BAR: f32 = 3.0;
/// A deletion has no line of its own, so it is a wedge between two lines rather than a bar.
const WEDGE: f32 = 2.0;

/// What happened to one line of the buffer, against the committed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Added,
    Modified,
    /// Something was deleted immediately above this line.
    DeletedAbove,
    /// Something was deleted after the last line, which has no line below to mark.
    DeletedBelow,
}

/// One slot per line of the new text, `None` where nothing changed.
///
/// The rows [`accent_core::diff::align`] lays the two panes out with are the same pairing a gutter
/// needs: a row with a line on both sides is a rewrite, a row with only a new line is an addition,
/// and a row with only an old one is a removal that left no line to colour, so it marks the line
/// that closed over it. What the diff view draws in a column, the gutter draws in three pixels.
///
/// `lines` needs no word-level refinement for this; [`accent_core::diff::line_ops`] is the cheap
/// way to get it.
pub fn marks(lines: &[DiffLine], line_count: usize) -> Vec<Option<Mark>> {
    let rows = accent_core::diff::align(lines);
    let mut marks = vec![None; line_count];
    let mut set = |line: usize, mark: Mark| {
        if let Some(slot) = marks.get_mut(line) {
            *slot = Some(mark);
        }
    };
    for (i, row) in rows.iter().enumerate() {
        // `new_line` is 1-based, and every row with a new side carries one.
        match row.new.map(|n| &lines[n]) {
            Some(line) if line.op == Op::Equal => {}
            Some(line) => set(
                line.new_line.unwrap_or(1) - 1,
                match row.old {
                    Some(_) => Mark::Modified,
                    None => Mark::Added,
                },
            ),
            // Mark where the removal was: the line that now follows it, or the end of the buffer
            // if it ran off it.
            None => match rows[i + 1..]
                .iter()
                .find_map(|r| r.new.and_then(|n| lines[n].new_line))
            {
                Some(line) => set(line - 1, Mark::DeletedAbove),
                None => set(line_count.saturating_sub(1), Mark::DeletedBelow),
            },
        }
    }
    marks
}

glib::wrapper! {
    pub struct Renderer(ObjectSubclass<imp::Renderer>)
        @extends sourceview5::GutterRenderer, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Renderer {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Renderer {
    pub fn new() -> Renderer {
        let renderer: Renderer = glib::Object::new();
        renderer.set_size_request(WIDTH, -1);
        renderer
    }

    pub fn set_marks(&self, marks: Vec<Option<Mark>>) {
        self.imp().marks.replace(marks);
        self.queue_draw();
    }

    /// Mark `lines` as still being checked, in order and apart; none once the check is done.
    /// The column comes up with the first check and stays, git or not, so the text does not move
    /// each time a check starts and ends.
    pub fn set_checking(&self, lines: Vec<Range<u32>>) {
        if !lines.is_empty() {
            self.imp().checked.set(true);
            self.set_visible(true);
        }
        self.imp().checking.replace(lines);
        self.queue_draw();
    }

    /// Whether a check has ever marked lines here: the column stays up for it.
    pub fn checked(&self) -> bool {
        self.imp().checked.get()
    }

    /// The lines marked as still being checked, for the drill that watches them.
    #[cfg(feature = "bench")]
    pub fn checking(&self) -> Vec<Range<u32>> {
        self.imp().checking.borrow().clone()
    }

    /// Take the colours from the theme, the way every other painted thing here does: the diff's
    /// own green and red mixed with the resolved foreground, the accent for a rewrite, and the
    /// warning's amber for a line still being checked.
    pub fn restyle(&self, view: &impl IsA<gtk::Widget>) {
        let fg = view.as_ref().color();
        self.imp().colours.set([
            crate::diff::tint(crate::diff::ADDED_HUE, fg, 1.0),
            crate::diff::tint(crate::diff::REMOVED_HUE, fg, 1.0),
            adw::StyleManager::default().accent_color_rgba(),
            crate::diff::tint(crate::diff::WARNING_HUE, fg, 1.0),
        ]);
        self.queue_draw();
    }
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    pub struct Renderer {
        pub marks: RefCell<Vec<Option<Mark>>>,
        /// The lines still being checked, in order and apart.
        pub checking: RefCell<Vec<Range<u32>>>,
        /// A check has marked lines here: the column stays up from then on.
        pub checked: Cell<bool>,
        /// Added, removed, modified, checking. Set from the theme; the fallback is only ever seen
        /// if a draw beats the first `restyle`, which one `connect_map` away cannot happen.
        pub colours: Cell<[gdk::RGBA; 4]>,
    }

    impl Default for Renderer {
        fn default() -> Self {
            Renderer {
                marks: RefCell::new(Vec::new()),
                checking: RefCell::new(Vec::new()),
                checked: Cell::new(false),
                colours: Cell::new([gdk::RGBA::TRANSPARENT; 4]),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Renderer {
        const NAME: &'static str = "AccentGitMarks";
        type Type = super::Renderer;
        type ParentType = sourceview5::GutterRenderer;
    }

    impl ObjectImpl for Renderer {}
    impl WidgetImpl for Renderer {}

    impl GutterRendererImpl for Renderer {
        fn snapshot_line(
            &self,
            snapshot: &gtk::Snapshot,
            lines: &sourceview5::GutterLines,
            line: u32,
        ) {
            let ranges = self.checking.borrow();
            let next = ranges.partition_point(|lines| lines.end <= line);
            let checking = ranges.get(next).is_some_and(|lines| lines.start <= line);
            let marks = self.marks.borrow();
            let mark = marks.get(line as usize).copied().flatten();
            if !checking && mark.is_none() {
                return;
            }
            // `y` is already in the renderer's own coordinates, so nothing to translate.
            let (y, height) =
                lines.line_yrange(line, sourceview5::GutterRendererAlignmentMode::Cell);
            // A line hidden inside a fold is laid out with no height; its bar or wedge would
            // land on the header's own row.
            if height <= 0 {
                return;
            }
            let (y, height) = (y as f32, height as f32);
            let colours = self.colours.get();
            if checking {
                let rect = graphene::Rect::new(BAR, y, WIDTH as f32 - BAR, height);
                snapshot.append_color(&colours[3], &rect);
            }
            let Some(mark) = mark else {
                return;
            };
            let (colour, rect) = match mark {
                Mark::Added => (colours[0], graphene::Rect::new(0.0, y, BAR, height)),
                Mark::Modified => (colours[2], graphene::Rect::new(0.0, y, BAR, height)),
                Mark::DeletedAbove => {
                    (colours[1], graphene::Rect::new(0.0, y, WIDTH as f32, WEDGE))
                }
                Mark::DeletedBelow => (
                    colours[1],
                    graphene::Rect::new(0.0, y + height - WEDGE, WIDTH as f32, WEDGE),
                ),
            };
            snapshot.append_color(&colour, &rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks_of(old: &str, new: &str) -> Vec<Option<Mark>> {
        let plain = accent_core::diff::line_ops(old, new);
        // The refined diff is the same rows, so the gutter reads the same either way — which is
        // what lets the editor pay for the cheap one.
        let refined = accent_core::diff::lines(old, new);
        let count = new.lines().count();
        assert_eq!(marks(&plain, count), marks(&refined, count));
        marks(&plain, count)
    }

    #[test]
    fn identical_texts_are_unmarked() {
        assert_eq!(marks_of("a\nb\n", "a\nb\n"), vec![None, None]);
    }

    #[test]
    fn a_rewritten_line_is_modified_and_an_extra_one_is_added() {
        // One line replaced by two: the first pairs with the deletion, the second is new.
        assert_eq!(
            marks_of("a\nb\nc\n", "a\nB\nB2\nc\n"),
            vec![None, Some(Mark::Modified), Some(Mark::Added), None]
        );
    }

    #[test]
    fn a_pure_insertion_is_added() {
        assert_eq!(
            marks_of("a\nc\n", "a\nb\nc\n"),
            vec![None, Some(Mark::Added), None]
        );
    }

    #[test]
    fn a_pure_deletion_marks_the_line_that_closed_over_it() {
        assert_eq!(
            marks_of("a\nb\nc\n", "a\nc\n"),
            vec![None, Some(Mark::DeletedAbove)]
        );
    }

    #[test]
    fn a_deletion_at_the_end_marks_the_last_line() {
        assert_eq!(marks_of("a\nb\n", "a\n"), vec![Some(Mark::DeletedBelow)]);
    }
}
