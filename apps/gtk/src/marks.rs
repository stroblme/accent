//! Mark changes from committed text and pending language checks in the editor gutter.
//!
//! Git marks use the comparison's line diff (`accent_core::diff::line_ops`) and colours:
//! green for additions, red for deletions and accent for modifications. Full opacity keeps
//! 3 px bars legible; pending checks (`Event::Checking`) use half-opacity accent.

use std::ops::Range;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use sourceview5::subclass::prelude::*;

use accent_core::diff::{DiffLine, Op};

/// Width of the mark column, and of the bar inside it. The checking bar takes the rest.
const WIDTH: i32 = 6;
const BAR: f32 = 3.0;
/// The checking bar's share of the accent: a rewritten line's bar is the accent itself, and the
/// two touch where a line is both.
const CHECKING_ALPHA: f32 = 0.5;
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

/// Map each new line to its change, with `None` for unchanged lines.
///
/// [`accent_core::diff::align`] pairs rewrites and additions; deletions mark the next surviving
/// line. [`accent_core::diff::line_ops`] avoids unnecessary word-level refinement.
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

    /// Mark sorted, disjoint `lines` as pending checks; clear them when finished.
    /// Keep the column after its first use to avoid shifting text on every check.
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

    /// Resolve diff green/red, modification accent and [`CHECKING_ALPHA`] pending-check accent from the theme.
    pub fn restyle(&self, view: &impl IsA<gtk::Widget>) {
        let fg = view.as_ref().color();
        let accent = adw::StyleManager::default().accent_color_rgba();
        self.imp().colours.set([
            crate::diff::tint(crate::diff::ADDED_HUE, fg, 1.0),
            crate::diff::tint(crate::diff::REMOVED_HUE, fg, 1.0),
            accent,
            crate::theme::at(accent, CHECKING_ALPHA),
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
        /// Theme colours for added, removed, modified and checking marks, with a pre-restyle fallback.
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
