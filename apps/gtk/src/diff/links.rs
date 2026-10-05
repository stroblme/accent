//! Meld's connectors between a merge's columns: a strip of its own between each side column and
//! the file, with a band across it for every run of rows where the two differ, from the rows the
//! side has lines in to the rows the file has lines in. The rows are level, so a band runs straight
//! where both cover the same rows and curves where one covers fewer: a side's one line fanning out
//! to the whole conflict block it is part of in the file.
//!
//! Drawn from the grid the last relayout laid ([`Columns`]), again on every relayout and scroll.

use adw::prelude::*;
use gtk::{gdk, graphene};
use std::cell::RefCell;
use std::ops::Range;

use super::Pane;
use super::columns::Columns;

/// The room each column gives up for the strips: a side column all of it towards the middle, the
/// middle one half on either side, so a strip is a share and a half and a divider wide.
const SHARE: i32 = 22;

/// The rows a band joins in the column on the strip's left and in the one on its right. An empty
/// range is a column with no line in the run, which the band narrows to a point at.
pub(super) type Run = [Range<usize>; 2];

/// Where a band starts and ends, `(top, bottom)`, on the strip's left and on its right.
pub type Ends = [(i32, i32); 2];

pub(super) struct Links {
    /// The columns, once they are laid side by side, with the strips drawn over the room between.
    pub(super) overlay: gtk::Overlay,
    pub(super) area: gtk::DrawingArea,
    /// Each column as it was before it gave up its share: where its title and text are.
    roots: [gtk::Widget; 3],
    runs: RefCell<[Vec<Run>; 2]>,
    /// Each strip's fill and outline: the tint of the stage its side column shows.
    tints: RefCell<[(gdk::RGBA, gdk::RGBA); 2]>,
}

impl Links {
    /// Room for a strip between each two of `panes`, the left column's on its right, the right
    /// one's on its left and the middle one's half on either side. The paned splits the columns in
    /// the ratio of their widths, which `Columns` makes one, so each giving up the same share keeps
    /// their text equally wide. A strip straddles the divider between its columns, and is drawn
    /// over it.
    pub(super) fn new(panes: &mut [Pane; 3]) -> Links {
        let roots = std::array::from_fn(|c| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            let root = std::mem::replace(&mut panes[c].root, row.clone().upcast());
            root.set_hexpand(true);
            let room = |width| gtk::Box::builder().width_request(width).build();
            let (before, after) = [(0, SHARE), (SHARE / 2, SHARE / 2), (SHARE, 0)][c];
            if before > 0 {
                row.append(&room(before));
            }
            row.append(&root);
            if after > 0 {
                row.append(&room(after));
            }
            root
        });
        let area = gtk::DrawingArea::builder().can_target(false).build();
        let overlay = gtk::Overlay::new();
        overlay.add_overlay(&area);
        Links {
            overlay,
            area,
            roots,
            runs: RefCell::default(),
            tints: RefCell::new([(gdk::RGBA::TRANSPARENT, gdk::RGBA::TRANSPARENT); 2]),
        }
    }

    /// The bands of the strip right of the left column and of the one left of the right column.
    pub(super) fn set_runs(&self, runs: [Vec<Run>; 2]) {
        *self.runs.borrow_mut() = runs;
        self.queue_draw();
    }

    /// Strip `i`'s fill and outline.
    pub(super) fn set_tints(&self, i: usize, fill: gdk::RGBA, outline: gdk::RGBA) {
        self.tints.borrow_mut()[i] = (fill, outline);
        self.queue_draw();
    }

    pub(super) fn queue_draw(&self) {
        self.area.queue_draw();
    }

    /// Where strip `i` is across: from the right edge of the column on its left to the left edge
    /// of the one on its right.
    fn span(&self, i: usize) -> Option<(f64, f64)> {
        let left = self.roots[i].compute_bounds(&self.area)?;
        let right = self.roots[i + 1].compute_bounds(&self.area)?;
        Some((f64::from(left.x() + left.width()), f64::from(right.x())))
    }

    /// Both strips: the page's ground over the room and the divider in it, then below the titles
    /// each band, a curve from the top of its rows on the left to theirs on the right, down, and
    /// back by the bottoms.
    pub(super) fn draw(&self, columns: &Columns, cr: &gtk::cairo::Context, height: i32) {
        let page = crate::highlight::page(adw::StyleManager::default().is_dark());
        let height = f64::from(height);
        cr.set_line_width(1.0);
        for i in 0..2 {
            let Some((x0, x1)) = self.span(i) else {
                continue;
            };
            cr.rectangle(x0, 0.0, x1 - x0, height);
            cr.set_source_color(&page);
            let _ = cr.fill();
            let origin = graphene::Point::zero();
            let scroller = &columns.panes[i].scroller;
            let top = scroller
                .compute_point(&self.area, &origin)
                .map_or(0.0, |p| p.y());
            let _ = cr.save();
            cr.rectangle(x0, f64::from(top), x1 - x0, height);
            cr.clip();
            let (fill, outline) = self.tints.borrow()[i];
            let mid = (x0 + x1) / 2.0;
            for [(a0, a1), (b0, b1)] in self.ends(i, columns) {
                let [a0, a1, b0, b1] = [a0, a1, b0, b1].map(f64::from);
                cr.move_to(x0, a0);
                cr.curve_to(mid, a0, mid, b0, x1, b0);
                cr.line_to(x1, b1);
                cr.curve_to(mid, b1, mid, a1, x0, a1);
                cr.close_path();
                cr.set_source_color(&fill);
                let _ = cr.fill_preserve();
                cr.set_source_color(&outline);
                let _ = cr.stroke();
            }
            let _ = cr.restore();
        }
    }

    /// Strip `i`'s bands as `[(top, bottom); 2]`, on its left and on its right, in the drawing's
    /// pixels: where the grid has the rows, through the view of the column each end faces.
    pub(super) fn ends(&self, i: usize, columns: &Columns) -> Vec<Ends> {
        let grid = columns.grid.borrow();
        let strip = &self.area;
        let y = |c: usize, row: usize| -> Option<i32> {
            let top = match grid.tops.get(row) {
                Some(&top) => top,
                None => (row == grid.tops.len()).then_some(grid.end)?,
            };
            let view = &columns.panes[c].view;
            let (_, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, top);
            let at = view.compute_point(strip, &graphene::Point::new(0.0, y as f32))?;
            Some(at.y().round() as i32)
        };
        self.runs.borrow()[i]
            .iter()
            .filter_map(|run| {
                let end = |k: usize| Some((y(i + k, run[k].start)?, y(i + k, run[k].end)?));
                Some([end(0)?, end(1)?])
            })
            .collect()
    }

    /// Strip `i`'s runs, for the bench.
    #[cfg(feature = "bench")]
    pub(super) fn runs(&self, i: usize) -> Vec<Run> {
        self.runs.borrow()[i].clone()
    }

    /// How wide each column and each strip is, for the bench.
    #[cfg(feature = "bench")]
    pub(super) fn widths(&self) -> ([i32; 3], [i32; 2]) {
        let strip = |i| self.span(i).map_or(0, |(x0, x1)| (x1 - x0).round() as i32);
        (
            self.roots.each_ref().map(|r| r.width()),
            [strip(0), strip(1)],
        )
    }
}
