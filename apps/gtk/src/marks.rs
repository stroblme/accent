//! Change marks in the editor gutter: which lines differ from what git has committed.
//!
//! The bars are the same green and red the diff view uses, at full alpha because a 3 px column
//! has no room to be subtle. A modified line is the accent instead: it is neither an addition nor
//! a removal, and the accent is the colour this app already means "something here" with.
//!
//! The marks come from the same `accent_core::diff::lines` the comparison tabs are built from, so
//! a line the diff paints green is a line the gutter marks green.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};
use sourceview5::subclass::prelude::*;

use accent_core::diff::{DiffLine, Op};

/// Width of the mark column, and of the bar inside it.
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
/// `diff::lines` is a flat list: a change comes out as a run of deletions followed by a run of
/// insertions. Pairing those runs is what tells a rewritten line from a brand new one — the first
/// `min(deleted, inserted)` lines of the pair are modifications and the rest are additions, which
/// is the same pairing `diff.rs::align` does to lay the two panes side by side. A deletion run
/// with no insertions after it leaves no line to colour, so it marks the line that closed over it.
pub fn marks(lines: &[DiffLine], line_count: usize) -> Vec<Option<Mark>> {
    let mut marks = vec![None; line_count];
    let mut set = |line: usize, mark: Mark| {
        if let Some(slot) = marks.get_mut(line) {
            *slot = Some(mark);
        }
    };

    let mut i = 0;
    while i < lines.len() {
        if lines[i].op == Op::Equal {
            i += 1;
            continue;
        }
        // One run of deletions, then one run of insertions: that pair is a single change.
        let start = i;
        while i < lines.len() && lines[i].op == Op::Delete {
            i += 1;
        }
        let deleted = i - start;
        let inserted_at = i;
        while i < lines.len() && lines[i].op == Op::Insert {
            i += 1;
        }
        let inserted = i - inserted_at;

        for n in 0..inserted {
            // `new_line` is 1-based and only an insertion carries one.
            let Some(line) = lines[inserted_at + n].new_line else {
                continue;
            };
            set(
                line - 1,
                match n < deleted {
                    true => Mark::Modified,
                    false => Mark::Added,
                },
            );
        }
        if deleted > inserted {
            // The surplus removals left no line behind. Mark where they were: the line that now
            // follows them, or the end of the buffer if they ran off it.
            match lines[i..].iter().find_map(|l| l.new_line) {
                Some(line) => set(line - 1, Mark::DeletedAbove),
                None => set(line_count.saturating_sub(1), Mark::DeletedBelow),
            }
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

    /// Take the colours from the theme, the way every other painted thing here does: the diff's
    /// own green and red mixed with the resolved foreground, and the accent for a rewrite.
    pub fn restyle(&self, view: &impl IsA<gtk::Widget>) {
        let fg = view.as_ref().color();
        self.imp().colours.set([
            crate::diff::tint(crate::diff::ADDED_HUE, fg, 1.0),
            crate::diff::tint(crate::diff::REMOVED_HUE, fg, 1.0),
            adw::StyleManager::default().accent_color_rgba(),
        ]);
        self.queue_draw();
    }
}

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    pub struct Renderer {
        pub marks: RefCell<Vec<Option<Mark>>>,
        /// Added, removed, modified. Set from the theme; the fallback is only ever seen if a
        /// draw beats the first `restyle`, which one `connect_map` away cannot happen.
        pub colours: Cell<[gdk::RGBA; 3]>,
    }

    impl Default for Renderer {
        fn default() -> Self {
            Renderer {
                marks: RefCell::new(Vec::new()),
                colours: Cell::new([gdk::RGBA::TRANSPARENT; 3]),
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
            let marks = self.marks.borrow();
            let Some(Some(mark)) = marks.get(line as usize) else {
                return;
            };
            let colours = self.colours.get();
            let (colour, rect) = {
                // `y` is already in the renderer's own coordinates, so nothing to translate.
                let (y, height) =
                    lines.line_yrange(line, sourceview5::GutterRendererAlignmentMode::Cell);
                // A line hidden inside a fold is laid out with no height; its bar or wedge would
                // land on the header's own row.
                if height <= 0 {
                    return;
                }
                let (y, height) = (y as f32, height as f32);
                match mark {
                    Mark::Added => (colours[0], graphene::Rect::new(0.0, y, BAR, height)),
                    Mark::Modified => (colours[2], graphene::Rect::new(0.0, y, BAR, height)),
                    Mark::DeletedAbove => {
                        (colours[1], graphene::Rect::new(0.0, y, WIDTH as f32, WEDGE))
                    }
                    Mark::DeletedBelow => (
                        colours[1],
                        graphene::Rect::new(0.0, y + height - WEDGE, WIDTH as f32, WEDGE),
                    ),
                }
            };
            snapshot.append_color(&colour, &rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks_of(old: &str, new: &str) -> Vec<Option<Mark>> {
        let lines = accent_core::diff::lines(old, new);
        marks(&lines, new.lines().count())
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
