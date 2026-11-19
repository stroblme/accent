//! What `ACCENT_BENCH_NOTEBOOK` drives and reads.

use std::rc::Rc;

use accent_core::notebook::{CellId, Kind};
use adw::prelude::*;

use super::cell::{self, CellView};
use super::selection;
use super::{Counts, NotebookTab};

impl NotebookTab {
    /// What the drill counts: code cells, markdown cells, stored outputs, and how they show.
    pub fn counts(&self) -> (usize, usize, usize, Counts) {
        let mut sum = Counts::default();
        let (mut code, mut markdown) = (0, 0);
        let outputs = self
            .model
            .borrow()
            .cells()
            .iter()
            .map(|c| c.outputs().len())
            .sum();
        for view in self.views.borrow().iter() {
            match view.kind {
                Kind::Code => code += 1,
                Kind::Markdown => markdown += 1,
                Kind::Raw => {}
            }
            let c = view.outputs.counts();
            sum.pictures += c.pictures;
            sum.recoloured += c.recoloured;
            sum.tables += c.tables;
            sum.latex += c.latex;
            sum.typeset += c.typeset;
            sum.ansi += c.ansi;
            sum.placeholders += c.placeholders;
        }
        (code, markdown, outputs, sum)
    }

    /// Type `text` at the end of cell `n`, a key at a time.
    pub fn bench_type(self: &Rc<Self>, n: usize, text: &str) {
        let Some(id) = self.views.borrow().get(n).map(|v| v.id) else {
            return;
        };
        self.focus_cell(id);
        let Some((_, editor)) = self.editor(id) else {
            return;
        };
        let buffer = &editor.buffer;
        for c in text.chars() {
            buffer.begin_user_action();
            buffer.insert(&mut buffer.end_iter(), &c.to_string());
            buffer.end_user_action();
        }
    }

    /// Select cell `n` and give it the keyboard.
    pub fn bench_select(self: &Rc<Self>, n: usize) {
        let id = self.views.borrow().get(n).map(|v| v.id);
        if let Some(id) = id {
            self.focus_cell(id);
        }
    }

    pub fn bench_source(&self, n: usize) -> String {
        let view = self.views.borrow().get(n).cloned();
        view.map(|v| self.source_of(&v)).unwrap_or_default()
    }

    /// The text of `view`'s cell: its editor's, or the model's while it has none.
    fn source_of(&self, view: &CellView) -> String {
        view.text().unwrap_or_else(|| {
            let model = self.model.borrow();
            let at = model.position(view.id);
            at.map(|at| model.cells()[at].source().to_string())
                .unwrap_or_default()
        })
    }

    /// How many cells have their editor built.
    pub fn bench_built(&self) -> usize {
        self.views
            .borrow()
            .iter()
            .filter(|v| v.editor().is_some())
            .count()
    }

    /// Down on cell `n`'s last line, or Up on its first, as the arrow key's controller sees it;
    /// answers where the selection is then.
    pub fn bench_step(self: &Rc<Self>, n: usize, down: bool) -> Option<usize> {
        let id = self.views.borrow().get(n)?.id;
        self.focus_cell(id);
        let (_, editor) = self.editor(id)?;
        let buffer = &editor.buffer;
        buffer.place_cursor(&match down {
            true => buffer.end_iter(),
            false => buffer.start_iter(),
        });
        self.leave_cell(id, down);
        self.selected_at()
    }

    /// Cell `n`'s first line scrolled half under the view's bottom edge and pressed there, as a
    /// click goes: the press, the keyboard, the caret put down. Answers the scroll before it.
    pub fn bench_click(self: &Rc<Self>, n: usize) -> Option<f64> {
        let id = self.views.borrow().get(n)?.id;
        self.press(id);
        let (_, editor) = self.editor(id)?;
        let buffer = &editor.buffer;
        buffer.place_cursor(&buffer.start_iter());
        let (top, bottom) = self.caret_at(&editor)?;
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value((top + bottom) / 2.0 - adjustment.page_size());
        let before = adjustment.value();
        editor.view.grab_focus();
        Some(before)
    }

    /// Down on cell `n`'s last line with that line just inside the view's bottom edge, so the
    /// caret goes on into the next cell out of sight.
    pub fn bench_down_at_edge(self: &Rc<Self>, n: usize) {
        let Some(id) = self.views.borrow().get(n).map(|v| v.id) else {
            return;
        };
        self.focus_cell(id);
        let Some((_, editor)) = self.editor(id) else {
            return;
        };
        editor.buffer.place_cursor(&editor.buffer.end_iter());
        if let Some((_, bottom)) = self.caret_at(&editor) {
            let adjustment = self.scroller.vadjustment();
            adjustment.set_value(bottom - adjustment.page_size());
        }
        self.leave_cell(id, true);
    }

    /// How far above the view's bottom edge the selected cell's caret is.
    pub fn bench_caret_above_edge(&self) -> Option<f64> {
        let editor = self.view_of(self.selected.get())?.editor()?.clone();
        let (_, bottom) = self.caret_at(&editor)?;
        let adjustment = self.scroller.vadjustment();
        Some(adjustment.value() + adjustment.page_size() - bottom)
    }

    pub fn bench_scroller(&self) -> gtk::ScrolledWindow {
        self.scroller.clone()
    }

    pub fn bench_scrolled(&self) -> f64 {
        self.scroller.vadjustment().value()
    }

    /// Scroll by `pages` of the view, up for a negative count; answers how far it went up.
    pub fn bench_scroll_by(&self, pages: f64) -> f64 {
        let adjustment = self.scroller.vadjustment();
        let before = adjustment.value();
        adjustment.set_value(before + pages * adjustment.page_size());
        before - adjustment.value()
    }

    /// The first cell reaching into the view, and where its top is in the view.
    pub fn bench_top_cell(&self) -> Option<(usize, f64)> {
        let value = self.scroller.vadjustment().value();
        let content = self.content()?;
        let views = self.views.borrow();
        views.iter().enumerate().find_map(|(n, v)| {
            let bounds = v.root.compute_bounds(&content)?;
            let top = f64::from(bounds.y());
            (top + f64::from(bounds.height()) > value).then_some((n, top - value))
        })
    }

    /// Where the display line holding character `offset` of cell `n`'s source is in the view: its
    /// top, and its height.
    pub fn bench_line(self: &Rc<Self>, n: usize, offset: i32) -> Option<(f64, f64)> {
        let view = self.bench_view(n)?;
        let iter = view.buffer().iter_at_offset(offset);
        let (top, height) = selection::line_in(view.upcast_ref(), &iter, &self.content()?)?;
        Some((top - self.scroller.vadjustment().value(), height))
    }

    /// Where `widget` is in the view: its top, and its height.
    pub fn bench_bounds(&self, widget: &gtk::Widget) -> Option<(f64, f64)> {
        let bounds = widget.compute_bounds(&self.content()?)?;
        let top = f64::from(bounds.y()) - self.scroller.vadjustment().value();
        Some((top, f64::from(bounds.height())))
    }

    /// A Ctrl+scroll notch with the pointer over the middle of the view's width, `y` down it.
    pub fn bench_wheel_zoom(&self, y: f64, out: bool) {
        self.wheel_zoom(Some((f64::from(self.scroller.width()) / 2.0, y)), out);
    }

    /// Where cell `n`'s top is in the view.
    pub fn bench_cell_y(&self, n: usize) -> Option<f64> {
        let bounds = self
            .views
            .borrow()
            .get(n)?
            .root
            .compute_bounds(&self.content()?)?;
        Some(f64::from(bounds.y()) - self.scroller.vadjustment().value())
    }

    /// Scroll to the `n`th screen of the notebook.
    pub fn bench_scroll(&self, n: usize) {
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value(n as f64 * adjustment.page_size() * 0.9);
    }

    /// How tall the notebook is laid out, and how much of it the view shows.
    pub fn bench_range(&self) -> (f64, f64) {
        let adjustment = self.scroller.vadjustment();
        (adjustment.upper(), adjustment.page_size())
    }

    /// Each cell's outputs: whether they are still held by a placeholder, and how tall they ask
    /// to be at their width, which a text view's allocation may not have caught up with yet.
    pub fn bench_outputs(&self) -> Vec<(bool, i32)> {
        let views = self.views.borrow();
        let tall = |w: &gtk::Box| w.measure(gtk::Orientation::Vertical, w.width()).1;
        views
            .iter()
            .map(|v| (v.outputs.is_held(), tall(&v.outputs.widget)))
            .collect()
    }

    /// Each cell's kind, whether its editor is built, and how tall its source asks to be at its
    /// width: an editor's request, which its allocation can lag behind (ISSUES.md).
    pub fn bench_sources(&self) -> Vec<(Kind, bool, i32)> {
        let views = self.views.borrow();
        views
            .iter()
            .map(|v| {
                let (built, source) = v.source_widget();
                let tall = source.measure(gtk::Orientation::Vertical, source.width()).1;
                (v.kind, built, tall)
            })
            .collect()
    }

    /// Select cell `n` as its buttons do, the keyboard and the view left where they are.
    pub fn bench_pick(self: &Rc<Self>, n: usize) {
        let id = self.views.borrow().get(n).map(|v| v.id);
        if let Some(id) = id {
            self.select(id);
        }
    }

    /// The sizes every cell's built pictures show at, and the width of the outputs they are in.
    pub fn bench_pictures(&self) -> Vec<(i32, i32, i32)> {
        let views = self.views.borrow();
        views
            .iter()
            .flat_map(|v| v.outputs.picture_sizes())
            .collect()
    }

    /// How tall cell `n`'s card is laid out.
    pub fn bench_card_height(&self, n: usize) -> i32 {
        self.views.borrow().get(n).map_or(0, |v| v.card.height())
    }

    /// Which cell the buttons serve.
    pub fn bench_tools_cell(&self) -> Option<usize> {
        let id = self.tools.cell.get()?;
        self.views.borrow().iter().position(|v| v.id == id)
    }

    /// Points of `window` on cell `n`: on its card, in its rail by its top, and in its rail `below`
    /// px under its top.
    pub fn bench_rail_points(
        &self,
        n: usize,
        window: &gtk::Widget,
        below: f32,
    ) -> Option<[(f32, f32); 3]> {
        let view = self.views.borrow().get(n).cloned()?;
        let at = |widget: &gtk::Box, x: f32, y: f32| {
            let point = widget.compute_point(window, &gtk::graphene::Point::new(x, y))?;
            Some((point.x(), point.y()))
        };
        Some([
            at(&view.card, 60.0, 20.0)?,
            at(&view.rail, 17.0, 40.0)?,
            at(&view.rail, 17.0, below)?,
        ])
    }

    /// Show the add strip in cell `n`'s gap, as the pointer coming into it does.
    pub fn bench_gap(self: &Rc<Self>, n: usize) {
        let id = self.views.borrow().get(n).map(|v| v.id);
        if let Some(id) = id {
            self.hover_gap(id, true);
        }
    }

    /// Where the selected cell's caret is in the view.
    pub fn bench_caret_y(&self) -> Option<f64> {
        let editor = self.view_of(self.selected.get())?.editor()?.clone();
        Some(self.caret_at(&editor)?.0 - self.scroller.vadjustment().value())
    }

    /// How many markdown cells show their rendered page.
    pub fn bench_rendered(&self) -> usize {
        self.views
            .borrow()
            .iter()
            .filter(|v| v.shows_rendered())
            .count()
    }

    /// The text of each markdown output's rendered page, once it has settled.
    pub fn bench_output_pages(&self) -> Vec<String> {
        let mut texts = Vec::new();
        for view in self.views.borrow().iter() {
            view.outputs.each_page(&mut |page| {
                if page.settled() {
                    let buffer = page.buffer();
                    texts.push(
                        buffer
                            .text(&buffer.start_iter(), &buffer.end_iter(), false)
                            .into(),
                    );
                }
            });
        }
        texts
    }

    /// The TeX of each formula typeset on cell `n`'s rendered page, once it has settled.
    pub fn bench_typeset(&self, n: usize) -> Option<Vec<String>> {
        let view = self.views.borrow().get(n).cloned()?;
        let formulas = view.with_rendered(|page| page.settled().then(|| page.formulas()))??;
        Some(formulas.into_iter().map(|(tex, ..)| tex).collect())
    }

    /// How many rendered pages, the cells' and the outputs', have a render yet to land, and how
    /// many wrote a block in their last patch.
    pub fn bench_pages_busy(&self) -> (usize, usize) {
        let (mut unsettled, mut written) = (0, 0);
        self.each_page(&mut |page| {
            unsettled += usize::from(!page.settled());
            written += usize::from(page.patched().1 > 0);
        });
        (unsettled, written)
    }

    /// Cell `n`'s id.
    pub fn bench_id(&self, n: usize) -> CellId {
        self.views.borrow()[n].id
    }

    /// Whether no cell can be typed into and no cell button or add strip is out.
    pub fn bench_read_only(&self) -> bool {
        let views = self.views.borrow();
        views
            .iter()
            .all(|v| v.editor().is_none_or(|e| !e.view.is_editable()))
            && !self.tools.bar.is_visible()
            && self.gap.parent().is_none()
            && !self.head.is_visible()
    }

    /// What `ACCENT_BENCH_MOTION` samples: the cell buttons, Run Cell, Stop, the strip over the
    /// first cell and the gaps' strip.
    pub fn bench_motion_parts(&self) -> [gtk::Widget; 5] {
        let t = &self.tools;
        [
            t.bar.clone().upcast(),
            t.run.clone().upcast(),
            t.stop.clone().upcast(),
            self.head.clone().upcast(),
            self.gap.clone().upcast(),
        ]
    }

    /// The pointer onto cell `n`'s card, or off every cell with none selected.
    pub fn bench_hover(self: &Rc<Self>, n: Option<usize>) {
        let id = n.and_then(|n| self.views.borrow().get(n).map(|v| v.id));
        if let Some(id) = id {
            return self.hover(id, true);
        }
        if let Some(id) = self.hovered.get() {
            self.hover(id, false);
        }
        self.deselect();
    }

    /// Cell `n`'s whole view and its outputs.
    pub fn bench_cell_parts(&self, n: usize) -> Option<[gtk::Widget; 2]> {
        let view = self.views.borrow().get(n).cloned()?;
        Some([
            view.root.clone().upcast(),
            view.outputs.widget.clone().upcast(),
        ])
    }

    /// Cell `n`'s outputs put stale, as a run starting does, or back.
    pub fn bench_set_stale(&self, n: usize, on: bool) {
        if let Some(view) = self.views.borrow().get(n) {
            view.outputs.set_stale(on);
        }
    }

    /// The row `input()` asks in under cell `n`, which a run's end takes away.
    pub fn bench_ask(&self, n: usize) -> Option<gtk::Widget> {
        let view = self.views.borrow().get(n).cloned()?;
        view.ask("name? ", false, |_| {}).parent()
    }

    /// Stop shown in Run Cell's place (`running`), or Run Cell in Stop's, as the kernel turns.
    pub fn bench_turn_to(&self, running: bool) {
        let turn = match running {
            true => cell::Turn::Running,
            false => cell::Turn::Free,
        };
        self.tools.show_turn(turn);
    }
}
