//! Meld's connectors between a comparison's columns: a strip of its own between each two, with a
//! band across it for every run of rows where the two differ, from the rows the left column has
//! lines in to the rows the right one has. The rows are level, so a band runs straight where both
//! cover the same rows and curves where one covers fewer: a merge side's one line fanning out to
//! the whole conflict block it is part of in the file, an added line narrowing to the point it went
//! in at. A band's buttons — a hunk's, a conflict block's — sit at its top, one to a half of the
//! strip.
//!
//! Drawn from the grid the last relayout laid ([`Columns`]), again on every relayout and scroll.

use adw::prelude::*;
use gtk::{gdk, graphene};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::{Rc, Weak};

use super::Pane;
use super::columns::Columns;

/// The room each of `n` columns gives up for the strips: an outer column all of it to its one
/// strip, a middle one half to either of its two, so their text stays equally wide. Either way a
/// strip is about 45 px and the divider wide, room for a button in each half.
fn share(n: usize) -> i32 {
    match n {
        2 => 22,
        _ => 30,
    }
}

/// The rows a band joins in the column on the strip's left and in the one on its right. An empty
/// range is a column with no line in the run, which the band narrows to a point at.
pub(super) type Run = [Range<usize>; 2];

/// Where a band starts and ends, `(top, bottom)`, on the strip's left and on its right.
pub type Ends = [(i32, i32); 2];

/// A band: the rows it joins, and the key a press on its buttons is handed, where it has any.
pub(super) type Band = (Run, Option<usize>);

/// What a strip button shows: its icon, its accessible name, and its tooltip, which is its
/// accessible description too.
pub(super) type Dress = (&'static str, &'static str, &'static str);

/// What a press on a strip button does, handed its strip, the key of the band it is on, and the
/// half of the strip it sits in, 0 the left.
pub(super) type Act = Rc<dyn Fn(usize, usize, usize)>;

/// A strip button, and where it is: `(strip, band, half, key)`.
struct Slot {
    button: gtk::Button,
    at: Rc<Cell<(usize, usize, usize, usize)>>,
    /// What it shows, dressed again only when that changes: a keystroke lays every run again.
    dress: Cell<Option<Dress>>,
}

/// The rows of `run` each column of a strip has lines in, `has(k, row)` saying whether column `k`
/// (0 on the strip's left, 1 on its right) has one: from its first to its last, or none at the
/// run's start where it has no line at all.
pub(super) fn run_of(run: &Range<usize>, has: impl Fn(usize, usize) -> bool) -> Run {
    [0, 1].map(|k| {
        let first = run.clone().find(|&r| has(k, r));
        let last = run.clone().rev().find(|&r| has(k, r));
        match (first, last) {
            (Some(first), Some(last)) => first..last + 1,
            _ => run.start..run.start,
        }
    })
}

pub(super) struct Links {
    /// The columns, once they are laid side by side, with the strips drawn over the room between.
    pub(super) overlay: gtk::Overlay,
    area: gtk::DrawingArea,
    /// Each column as it was before it gave up its share: where its title and text are.
    roots: Vec<gtk::Widget>,
    runs: RefCell<Vec<Vec<Band>>>,
    /// Each strip's fill and outline at its left edge and at its right: a band shades from the
    /// tint of the column on its left to that of the one on its right.
    tints: RefCell<Vec<[(gdk::RGBA, gdk::RGBA); 2]>>,
    /// Per strip, the button each half carries on every band with a key, if any.
    dress: RefCell<Vec<[Option<Dress>; 2]>>,
    /// The buttons, handed out to the bands in order on every lay, the ones left over hidden.
    slots: RefCell<Vec<Slot>>,
    /// What the host does with a press, asked at the press.
    act: Rc<RefCell<Option<Act>>>,
    /// Whether the buttons are waiting to be put where their bands are: see [`Links::relaid`].
    placing: Rc<Cell<bool>>,
}

/// Where a band's buttons go down its strip, `h` tall: at the band's top, held at the top of the
/// text while the band is beside it, and carried off with the band's bottom. `top` and `bottom`
/// are the band's, over both its ends; `text` is where the text starts below the title rows.
fn button_y(top: i32, bottom: i32, text: i32, h: i32) -> i32 {
    top.max(text).min(bottom.max(top + h) - h)
}

impl Links {
    /// Room for a strip between each two of `panes`. The paned splits the columns in the ratio of
    /// their widths, which `Columns` makes one, so each giving up the same room keeps their text
    /// equally wide. A strip straddles the divider between its columns, and is drawn over it.
    pub(super) fn new(panes: &mut [Pane]) -> Links {
        let n = panes.len();
        let share = |c: usize| match c == 0 || c == n - 1 {
            true => share(n),
            false => share(n) / 2,
        };
        let roots = (panes.iter_mut().enumerate())
            .map(|(c, pane)| {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                let root = std::mem::replace(&mut pane.root, row.clone().upcast());
                root.set_hexpand(true);
                let room = || gtk::Box::builder().width_request(share(c)).build();
                if c > 0 {
                    row.append(&room());
                }
                row.append(&root);
                if c < n - 1 {
                    row.append(&room());
                }
                root
            })
            .collect();
        let area = gtk::DrawingArea::builder().can_target(false).build();
        let overlay = gtk::Overlay::new();
        overlay.add_overlay(&area);
        let clear = (gdk::RGBA::TRANSPARENT, gdk::RGBA::TRANSPARENT);
        Links {
            overlay,
            area,
            roots,
            runs: RefCell::default(),
            tints: RefCell::new(vec![[clear; 2]; n - 1]),
            dress: RefCell::new(vec![[None; 2]; n - 1]),
            slots: RefCell::default(),
            act: Rc::default(),
            placing: Rc::default(),
        }
    }

    /// Draw the strips and place their buttons with `columns`' rows, from now on. Weak: the
    /// columns own this.
    pub(super) fn follow(&self, columns: Weak<Columns>) {
        let w = columns.clone();
        self.area.set_draw_func(move |_, cr, _, height| {
            if let Some(c) = w.upgrade() {
                c.links.draw(&c, cr, height);
            }
        });
        self.overlay.connect_get_child_position(move |_, widget| {
            let c = columns.upgrade()?;
            c.links.place(&c, widget)
        });
    }

    /// The buttons on every band of strip `i` with a key, by half.
    pub(super) fn set_buttons(&self, i: usize, dress: [Option<Dress>; 2]) {
        self.dress.borrow_mut()[i] = dress;
    }

    /// What a press on a strip button does.
    pub(super) fn set_act(&self, act: Act) {
        *self.act.borrow_mut() = Some(act);
    }

    /// Each strip's bands, left to right, and the buttons on those with a key.
    pub(super) fn set_runs(&self, runs: Vec<Vec<Band>>) {
        {
            let dress = self.dress.borrow();
            let mut slots = self.slots.borrow_mut();
            let mut n = 0;
            for (i, strip) in runs.iter().enumerate() {
                for (k, (_, key)) in strip.iter().enumerate() {
                    for (h, look) in dress[i].iter().enumerate() {
                        let (Some(key), Some(look)) = (*key, *look) else {
                            continue;
                        };
                        if n == slots.len() {
                            slots.push(self.slot());
                        }
                        let slot = &slots[n];
                        slot.at.set((i, k, h, key));
                        if slot.dress.replace(Some(look)) != Some(look) {
                            dress_button(&slot.button, look);
                        }
                        slot.button.set_visible(true);
                        n += 1;
                    }
                }
            }
            for slot in &slots[n..] {
                slot.button.set_visible(false);
            }
        }
        *self.runs.borrow_mut() = runs;
        self.relaid();
    }

    /// A new strip button, over the strips.
    fn slot(&self) -> Slot {
        let button = gtk::Button::builder()
            .css_classes(["flat", "accent-strip-button"])
            .focus_on_click(false)
            .build();
        let (act, at) = (self.act.clone(), Rc::new(Cell::new((0, 0, 0, 0))));
        let here = at.clone();
        button.connect_clicked(move |_| {
            let act = act.borrow().clone();
            if let Some(act) = act {
                let (i, _, h, key) = here.get();
                act(i, key, h);
            }
        });
        self.overlay.add_overlay(&button);
        self.overlay.set_clip_overlay(&button, true);
        Slot {
            button,
            at,
            dress: Cell::new(None),
        }
    }

    /// Where `widget` goes, if it is a strip button: centred in its half of its strip, down it as
    /// [`button_y`] has it, and off the overlay, which clips it away, while its band is not laid.
    /// `None` leaves the drawing over all of the overlay.
    fn place(&self, columns: &Columns, widget: &gtk::Widget) -> Option<gdk::Rectangle> {
        // ponytail: a search per button per allocation, fine at the hunks a note has.
        let slots = self.slots.borrow();
        let slot = slots
            .iter()
            .find(|s| s.button.upcast_ref::<gtk::Widget>() == widget)?;
        let (i, k, h, _) = slot.at.get();
        let w = widget.measure(gtk::Orientation::Horizontal, -1).1;
        let height = widget.measure(gtk::Orientation::Vertical, -1).1;
        let runs = self.runs.borrow();
        let laid = runs
            .get(i)
            .and_then(|strip| strip.get(k))
            .and_then(|(run, _)| {
                let [(a0, a1), (b0, b1)] = end(i, run, columns, &self.area)?;
                Some((a0.min(b0), a1.max(b1), self.span(i)?))
            });
        let Some((top, bottom, (x0, x1))) = laid else {
            return Some(gdk::Rectangle::new(-2 * w, -2 * height, w, height));
        };
        let half = (x1 - x0) / 2.0;
        let x = x0 + half * (h as f64 + 0.5) - f64::from(w) / 2.0;
        let y = button_y(top, bottom, self.text_top(i, columns), height);
        Some(gdk::Rectangle::new(x.round() as i32, y, w, height))
    }

    /// How many strip buttons are shown.
    pub(super) fn shown(&self) -> usize {
        let slots = self.slots.borrow();
        slots.iter().filter(|s| s.button.is_visible()).count()
    }

    /// Strip `i`'s fill and outline, at its left edge and at its right.
    pub(super) fn set_tints(&self, i: usize, fill: [gdk::RGBA; 2], outline: [gdk::RGBA; 2]) {
        self.tints.borrow_mut()[i] = [(fill[0], outline[0]), (fill[1], outline[1])];
        self.area.queue_draw();
    }

    /// The rows moved: the strips drawn again, and their buttons put where the bands are now
    /// from an idle, once the frames under way are done. Placing them allocates the overlay again,
    /// and asked for from GTK's own layout or painting — a view keeping its top line as it is
    /// allocated, a view's snapshot moving the scroll — that asked for another frame, in which GTK
    /// moved the scroll again: two columns trading it back and forth never came to rest.
    pub(super) fn relaid(&self) {
        self.area.queue_draw();
        if self.shown() == 0 || self.placing.replace(true) {
            return;
        }
        let (overlay, placing) = (self.overlay.downgrade(), self.placing.clone());
        gtk::glib::idle_add_local_once(move || {
            placing.set(false);
            if let Some(overlay) = overlay.upgrade() {
                overlay.queue_allocate();
            }
        });
    }

    /// Where strip `i` is across: from the right edge of the column on its left to the left edge
    /// of the one on its right.
    fn span(&self, i: usize) -> Option<(f64, f64)> {
        let left = self.roots[i].compute_bounds(&self.area)?;
        let right = self.roots[i + 1].compute_bounds(&self.area)?;
        Some((f64::from(left.x() + left.width()), f64::from(right.x())))
    }

    /// Every strip: the page's ground over the room and the divider in it, then below the titles
    /// each band, a curve from the top of its rows on the left to theirs on the right, down, and
    /// back by the bottoms.
    fn draw(&self, columns: &Columns, cr: &gtk::cairo::Context, height: i32) {
        let page = crate::highlight::page(adw::StyleManager::default().is_dark());
        let height = f64::from(height);
        cr.set_line_width(1.0);
        for i in 0..self.roots.len() - 1 {
            let Some((x0, x1)) = self.span(i) else {
                continue;
            };
            cr.rectangle(x0, 0.0, x1 - x0, height);
            cr.set_source_color(&page);
            let _ = cr.fill();
            let _ = cr.save();
            cr.rectangle(x0, f64::from(self.text_top(i, columns)), x1 - x0, height);
            cr.clip();
            let [left, right] = self.tints.borrow()[i];
            let shade = |a: gdk::RGBA, b: gdk::RGBA| {
                let gradient = gtk::cairo::LinearGradient::new(x0, 0.0, x1, 0.0);
                for (at, c) in [(0.0, a), (1.0, b)] {
                    let rgba = [c.red(), c.green(), c.blue(), c.alpha()].map(f64::from);
                    gradient.add_color_stop_rgba(at, rgba[0], rgba[1], rgba[2], rgba[3]);
                }
                gradient
            };
            let (fill, outline) = (shade(left.0, right.0), shade(left.1, right.1));
            let mid = (x0 + x1) / 2.0;
            for [(a0, a1), (b0, b1)] in self.ends(i, columns) {
                let [a0, a1, b0, b1] = [a0, a1, b0, b1].map(f64::from);
                cr.move_to(x0, a0);
                cr.curve_to(mid, a0, mid, b0, x1, b0);
                cr.line_to(x1, b1);
                cr.curve_to(mid, b1, mid, a1, x0, a1);
                cr.close_path();
                let _ = cr.set_source(&fill);
                let _ = cr.fill_preserve();
                let _ = cr.set_source(&outline);
                let _ = cr.stroke();
            }
            let _ = cr.restore();
        }
    }

    /// Where the text starts down strip `i`, below the title rows, in the drawing's pixels.
    fn text_top(&self, i: usize, columns: &Columns) -> i32 {
        let scroller = &columns.panes[i].scroller;
        (scroller.compute_point(&self.area, &graphene::Point::zero())).map_or(0, |p| p.y() as i32)
    }

    /// Strip `i`'s bands as `[(top, bottom); 2]`, on its left and on its right, in the drawing's
    /// pixels.
    pub(super) fn ends(&self, i: usize, columns: &Columns) -> Vec<Ends> {
        let runs = self.runs.borrow();
        let runs = runs.get(i).map_or(&[][..], Vec::as_slice);
        runs.iter()
            .filter_map(|(run, _)| end(i, run, columns, &self.area))
            .collect()
    }

    /// Strip `i`'s runs, for the bench.
    #[cfg(feature = "bench")]
    pub(super) fn runs(&self, i: usize) -> Vec<Run> {
        let runs = self.runs.borrow();
        runs.get(i).map_or(Vec::new(), |strip| {
            strip.iter().map(|(run, _)| run.clone()).collect()
        })
    }

    /// How wide each column and each strip is, for the bench.
    #[cfg(feature = "bench")]
    pub(super) fn widths(&self) -> (Vec<i32>, Vec<i32>) {
        let strip = |i| self.span(i).map_or(0, |(x0, x1)| (x1 - x0).round() as i32);
        (
            self.roots.iter().map(|r| r.width()).collect(),
            (0..self.roots.len() - 1).map(strip).collect(),
        )
    }

    /// The drawing, which is where [`Links::ends`] are measured from, for the bench.
    #[cfg(feature = "bench")]
    pub(super) fn area(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Where the shown strip buttons on screen are and where the columns are, as `[x, y, width,
    /// height]` on the overlay, how many of those buttons overlap a column, and how many are not
    /// where their bands have them, for the bench: 0 and 0 are the claim, buttons standing on no
    /// text, at the top of their bands.
    #[cfg(feature = "bench")]
    pub(super) fn rects(&self, columns: &Columns) -> (Vec<[i32; 4]>, Vec<[i32; 4]>, usize, usize) {
        let rect = |r: graphene::Rect| [r.x(), r.y(), r.width(), r.height()].map(|v| v as i32);
        let (w, h) = (self.overlay.width() as f32, self.overlay.height() as f32);
        let screen = graphene::Rect::new(0.0, 0.0, w, h);
        let shown: Vec<(gtk::Widget, graphene::Rect)> = (self.slots.borrow().iter())
            .filter(|s| s.button.is_visible())
            .filter_map(|s| {
                Some((
                    s.button.clone().upcast(),
                    s.button.compute_bounds(&self.overlay)?,
                ))
            })
            .filter(|(_, b)| b.intersection(&screen).is_some())
            .collect();
        let stale = (shown.iter())
            .filter(|(button, b)| {
                let at = self.place(columns, button).map(|r| (r.x(), r.y()));
                at != Some((b.x() as i32, b.y() as i32))
            })
            .count();
        let buttons: Vec<graphene::Rect> = shown.into_iter().map(|(_, b)| b).collect();
        let roots: Vec<graphene::Rect> = (self.roots.iter())
            .filter_map(|r| r.compute_bounds(&self.overlay))
            .collect();
        let over = (buttons.iter())
            .filter(|b| roots.iter().any(|c| b.intersection(c).is_some()))
            .count();
        (
            buttons.into_iter().map(rect).collect(),
            roots.into_iter().map(rect).collect(),
            over,
            stale,
        )
    }

    /// The shown strip buttons, each with the first row of its band and its accessible name, for
    /// the bench.
    #[cfg(feature = "bench")]
    pub(super) fn buttons(&self) -> Vec<(usize, &'static str, gtk::Button)> {
        let (slots, runs) = (self.slots.borrow(), self.runs.borrow());
        (slots.iter().filter(|s| s.button.is_visible()))
            .filter_map(|s| {
                let (i, k, ..) = s.at.get();
                let [left, right] = &runs.get(i)?.get(k)?.0;
                Some((
                    left.start.min(right.start),
                    s.dress.get()?.1,
                    s.button.clone(),
                ))
            })
            .collect()
    }
}

/// Give `button` its icon, name and tooltip.
fn dress_button(button: &gtk::Button, (icon, label, tip): Dress) {
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tip));
    button.update_property(&[
        gtk::accessible::Property::Label(label),
        gtk::accessible::Property::Description(tip),
    ]);
}

/// Where `run` of strip `i` starts and ends, in `to`'s pixels: where the grid has its rows,
/// through the view of the column each end faces.
fn end(i: usize, run: &Run, columns: &Columns, to: &impl IsA<gtk::Widget>) -> Option<Ends> {
    let grid = columns.grid.borrow();
    let y = |c: usize, row: usize| -> Option<i32> {
        let top = match grid.tops.get(row) {
            Some(&top) => top,
            None => (row == grid.tops.len()).then_some(grid.end)?,
        };
        let view = &columns.panes[c].view;
        let (_, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, top);
        let at = view.compute_point(to, &graphene::Point::new(0.0, y as f32))?;
        Some(at.y().round() as i32)
    };
    let end = |k: usize| Some((y(i + k, run[k].start)?, y(i + k, run[k].end)?));
    Some([end(0)?, end(1)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_joins_the_rows_each_column_has_lines_in() {
        // Rows 4..7: the left column has lines in rows 4 and 5, the right one none.
        let has = |k: usize, r: usize| k == 0 && r < 6;
        assert_eq!(
            run_of(&(4..7), has),
            [4..6, 4..4],
            "a deletion narrows to the point on the right where its lines were"
        );
        assert_eq!(run_of(&(4..7), |_, _| true), [4..7, 4..7]);
    }

    #[test]
    fn a_button_rides_its_band_and_holds_at_the_top_of_the_text() {
        let (text, h) = (30, 20);
        assert_eq!(button_y(100, 200, text, h), 100, "at the band's top");
        assert_eq!(
            button_y(10, 200, text, h),
            30,
            "held while the band is beside the text"
        );
        assert_eq!(
            button_y(-100, 40, text, h),
            20,
            "carried off with the band's bottom"
        );
        assert_eq!(
            button_y(100, 105, text, h),
            100,
            "a band shorter than the button"
        );
        assert_eq!(button_y(20, 25, text, h), 20, "which goes with its top");
    }
}
