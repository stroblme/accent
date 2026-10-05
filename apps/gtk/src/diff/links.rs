//! Meld's connectors between a merge's columns: a strip between each side column and the file,
//! with a band across it for every run of rows where the two differ, from the rows the side has
//! lines in to the rows the file has lines in. The rows are level, so a band runs straight where
//! both cover the same rows and curves where one covers fewer: a side's one line fanning out to
//! the whole conflict block it is part of in the file.
//!
//! Drawn from the grid the last relayout laid ([`Columns`]), again on every relayout and scroll.

use adw::prelude::*;
use gtk::{gdk, graphene};
use std::cell::RefCell;
use std::ops::Range;

use super::Pane;
use super::columns::Columns;

/// How wide a strip is.
const WIDTH: i32 = 32;

/// The rows a band joins in the column on the strip's left and in the one on its right. An empty
/// range is a column with no line in the run, which the band narrows to a point at.
pub(super) type Run = [Range<usize>; 2];

/// Where a band starts and ends, `(top, bottom)`, on the strip's left and on its right.
pub type Ends = [(i32, i32); 2];

pub(super) struct Links {
    /// Right of the left column's text, and left of the right column's.
    pub(super) strips: [gtk::DrawingArea; 2],
    runs: RefCell<[Vec<Run>; 2]>,
    /// Each strip's fill and outline: the tint of the stage its side column shows.
    tints: RefCell<[(gdk::RGBA, gdk::RGBA); 2]>,
}

impl Links {
    /// A strip beside the text of `left` and of `right`, on the side facing the middle column,
    /// under the column's title.
    pub(super) fn new(left: &Pane, right: &Pane) -> Links {
        let strips = [(left, false), (right, true)].map(|(pane, before)| {
            let strip = gtk::DrawingArea::builder()
                .content_width(WIDTH)
                .css_classes(["view"])
                .build();
            let root: &gtk::Box = pane
                .root
                .downcast_ref()
                .expect("diff::pane's root is a box");
            root.remove(&pane.scroller);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.append(&pane.scroller);
            match before {
                true => row.prepend(&strip),
                false => row.append(&strip),
            }
            root.append(&row);
            strip
        });
        Links {
            strips,
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
        self.strips[i].queue_draw();
    }

    pub(super) fn queue_draw(&self) {
        for strip in &self.strips {
            strip.queue_draw();
        }
    }

    /// Strip `i`'s bands, each a curve from the top of its rows on the left to theirs on the
    /// right, down, and back by the bottoms.
    pub(super) fn draw(&self, i: usize, columns: &Columns, cr: &gtk::cairo::Context, width: i32) {
        let (fill, outline) = self.tints.borrow()[i];
        let (w, mid) = (f64::from(width), f64::from(width) / 2.0);
        cr.set_line_width(1.0);
        for [(a0, a1), (b0, b1)] in self.ends(i, columns) {
            let [a0, a1, b0, b1] = [a0, a1, b0, b1].map(f64::from);
            cr.move_to(0.0, a0);
            cr.curve_to(mid, a0, mid, b0, w, b0);
            cr.line_to(w, b1);
            cr.curve_to(mid, b1, mid, a1, 0.0, a1);
            cr.close_path();
            cr.set_source_color(&fill);
            let _ = cr.fill_preserve();
            cr.set_source_color(&outline);
            let _ = cr.stroke();
        }
    }

    /// Strip `i`'s bands as `[(top, bottom); 2]`, on its left and on its right, in the strip's own
    /// pixels: where the grid has the rows, through the view of the column each end faces.
    pub(super) fn ends(&self, i: usize, columns: &Columns) -> Vec<Ends> {
        let grid = columns.grid.borrow();
        let strip = &self.strips[i];
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
}
