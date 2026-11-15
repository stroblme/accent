//! A line's free ends as numbers, draw.io's Line Start and Line End: each end's X and Y as the
//! file has them, in the line's parent, for one line selected alone and each end on no shape
//! (`ArrangePanel.addEdgeGeometry`, Format.js 1868-1894 and 3670-3812).

use std::rc::Rc;

use accent_drawio::{Cell, CellId, Page, Point};
use adw::prelude::*;

use super::{Change, Placed, Props, coordinate, typed};
use crate::diagram::DiagramTab;
use crate::diagram::view::Edit;

/// Line Start's and Line End's groups, each with an X and a Y row.
pub(super) struct Ends {
    groups: [adw::PreferencesGroup; 2],
    rows: [[adw::SpinRow; 2]; 2],
}

impl Ends {
    /// X and Y rows in `groups`, Line Start's and Line End's.
    pub(super) fn new(groups: [adw::PreferencesGroup; 2]) -> Ends {
        let rows = groups.each_ref().map(|group| {
            let rows = [coordinate("X", -1e6), coordinate("Y", -1e6)];
            rows.iter().for_each(|row| group.add(row));
            rows
        });
        Ends { groups, rows }
    }

    /// An end's two rows read together, as Position's do: a step lands as the burst settles, and
    /// digits typed show on the canvas as they are typed and land with Return or as the keyboard
    /// leaves the row.
    pub(super) fn wire(&self, props: &Rc<Props>, send: Rc<dyn Fn(Change)>) {
        for (end, slot) in ["line start", "line end"].into_iter().enumerate() {
            for (axis, row) in self.rows[end].iter().enumerate() {
                let (me, on) = (Rc::downgrade(props), send.clone());
                row.connect_value_notify(move |_| {
                    let Some(me) = me.upgrade().filter(|me| !me.filling.get()) else {
                        return;
                    };
                    let (at, on) = (me.ends.at(end), on.clone());
                    me.hold(slot, move || on(Change::End(end == 0, at)));
                    me.held(Placed::Ends(only(end, at)));
                });
                let (me, on) = (Rc::downgrade(props), send.clone());
                row.connect_changed(move |row| {
                    let Some(me) = me.upgrade().filter(|me| !me.filling.get()) else {
                        return;
                    };
                    let Some(v) = typed(row) else { return };
                    let mut at = me.ends.at(end);
                    match axis {
                        0 => at.x = v,
                        _ => at.y = v,
                    }
                    let on = on.clone();
                    me.keep(slot, move || on(Change::End(end == 0, at)));
                    me.held(Placed::Ends(only(end, at)));
                });
            }
        }
    }

    /// Where end `end`'s rows put it, the start first.
    fn at(&self, end: usize) -> Point {
        let [x, y] = &self.rows[end];
        Point::new(x.value(), y.value())
    }

    /// Show the free ends `ends`, hiding an end with none; `pinned` greys them, as a locked
    /// shape's Position is.
    fn fill(&self, ends: [Option<Point>; 2], pinned: bool) {
        for (group, at) in self.groups.iter().zip(ends) {
            group.set_visible(at.is_some());
        }
        self.show(ends);
        for row in self.rows.iter().flatten() {
            row.set_sensitive(!pinned);
        }
    }

    /// Show `ends` in their rows, an end that is `None` left as it is.
    pub(super) fn show(&self, ends: [Option<Point>; 2]) {
        for ([x, y], at) in self.rows.iter().zip(ends) {
            if let Some(at) = at {
                x.set_value(at.x);
                y.set_value(at.y);
            }
        }
    }
}

/// End `end` (the start first) at `at`, the other left as it is.
fn only(end: usize, at: Point) -> [Option<Point>; 2] {
    let mut ends = [None; 2];
    ends[end] = Some(at);
    ends
}

/// The points of `cell`'s free ends, the start first, as the file has them: none for an end on
/// a shape or with no point, as draw.io's panel shows none.
fn free(cell: &Cell) -> [Option<Point>; 2] {
    let g = cell.geometry.as_ref().filter(|_| cell.edge);
    [
        g.and_then(|g| g.source_point)
            .filter(|_| cell.source.is_none()),
        g.and_then(|g| g.target_point)
            .filter(|_| cell.target.is_none()),
    ]
}

/// What the canvas shows of the end a row holds for line `id` on `page`: that end put there.
pub(super) fn held(page: &Page, id: CellId, ends: [Option<Point>; 2]) -> Option<Edit> {
    let (source, at) = match ends {
        [Some(at), _] => (true, at),
        [None, at] => (false, at?),
    };
    let o = page.origin_of(&id);
    let end = (None, Point::new(at.x + o.x, at.y + o.y), None);
    Some(Edit::End { id, source, end })
}

impl DiagramTab {
    /// Put the one line selected's free ends into Line Start and Line End, or hide them.
    pub(super) fn fill_ends(&self) {
        let (ends, pinned) = {
            let (editor, selection) = (self.editor.borrow(), self.selection.borrow());
            match (editor.page(self.page_index.get()), selection.as_slice()) {
                (Ok(page), [id]) => (
                    page.cell(id).map(free).unwrap_or_default(),
                    self.view.sheet().is_some_and(|s| s.is_pinned(id)),
                ),
                _ => Default::default(),
            }
        };
        self.props.filling.set(true);
        self.props.ends.fill(ends, pinned);
        self.props.filling.set(false);
    }

    /// Put the one line selected's free start (else end) at `at` in its parent, as one step.
    pub(super) fn place_end(self: &Rc<Self>, source: bool, at: Point) {
        if let [id] = self.selection().as_slice() {
            self.edit(|e, page| e.place_end(page, id, source, at));
        }
    }

    /// What a drag's `edit` makes of the one line selected's free ends, as their rows show them:
    /// a move takes both along, and an end dragged off any shape is where it is dragged.
    pub(super) fn dragged_ends(&self, edit: &Edit) -> Option<Placed> {
        let selection = self.selection.borrow();
        let [id] = selection.as_slice() else {
            return None;
        };
        let editor = self.editor.borrow();
        let page = editor.page(self.page_index.get()).ok()?;
        let ends = free(page.cell(id)?);
        match edit {
            Edit::Move { ids, delta } if ids.contains(id) => {
                let moved = |at: Point| Point::new(at.x + delta.x, at.y + delta.y);
                Some(Placed::Ends(ends.map(|at| at.map(moved))))
            }
            Edit::End {
                id: dragged,
                source,
                end: (None, at, _),
            } if dragged == id => {
                let o = page.origin_of(id);
                let at = Point::new(at.x - o.x, at.y - o.y);
                Some(Placed::Ends(only(usize::from(!source), at)))
            }
            _ => None,
        }
    }
}
