//! Focus mode over a notebook (DESIGN.md, Chrome auto-hide): typing in a cell hides the chrome,
//! and at High one fade runs over the notebook's lines, through the borders of its cells, from
//! the selected cell's carets. Each cell's editor veils its own lines (`crate::fade`), told how
//! far its lines are from them; a cell's outputs fade as its last line does. Only the cells near
//! the view are kept up to date, so the cost does not grow with the notebook.

use std::cell::{Cell, OnceCell};
use std::ops::RangeInclusive;
use std::rc::Rc;

use accent_core::notebook::CellId;
use adw::prelude::*;

use super::cell::CellView;
use super::{NotebookTab, TabHook};
use crate::{fade, multicaret};

#[derive(Default)]
pub(super) struct State {
    /// Whether the chrome asked for the line fade.
    on: Cell<bool>,
    /// How far the outputs' fade is in, 0 to 1, on the ramp each editor runs for its own lines.
    ramp: OnceCell<adw::TimedAnimation>,
    /// What the fade was last measured from: the selected cell, its carets' first and last line,
    /// and how many lines it has, which moves every cell below it.
    from: Cell<Option<(CellId, i32, i32, i32)>>,
    /// Told when a cell is typed in, which hides the chrome.
    typed: TabHook,
}

impl NotebookTab {
    pub fn connect_typed(&self, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        self.focus.typed.set(Rc::new(f));
    }

    /// The cell buttons and the add strip, which fade with the chrome.
    pub fn chrome(&self) -> [gtk::Widget; 2] {
        [self.tools.bar.clone().upcast(), self.gap.clone().upcast()]
    }

    /// Whether `widget` is one of the cells' editors, and if so whether its completion popup is
    /// up, whose arrows pick a row rather than step through the text.
    pub fn popup_in(&self, widget: &gtk::Widget) -> Option<bool> {
        let views = self.views.borrow();
        let view = views.iter().find(|v| {
            v.editor()
                .is_some_and(|e| e.view.upcast_ref::<gtk::Widget>() == widget)
        })?;
        Some(self.popup_shown(view.id))
    }

    /// Turn the line fade on or off, ramping as the editors do.
    pub fn set_fade(self: &Rc<Self>, on: bool) {
        if self.focus.on.replace(on) == on {
            return;
        }
        self.focus.from.set(None);
        // Off, every editor that took it; on, the ones near the view, the rest as they near it.
        let views = match on {
            true => self.near.borrow().clone(),
            false => self.views.borrow().clone(),
        };
        for view in &views {
            if let Some(editor) = fader(view) {
                editor.set_fade(on);
            }
        }
        let ramp = self.focus.ramp.get_or_init(|| {
            let weak = Rc::downgrade(self);
            let target = adw::CallbackAnimationTarget::new(move |strength| {
                if let Some(tab) = weak.upgrade() {
                    tab.shade_outputs(strength);
                }
            });
            let ramp = adw::TimedAnimation::new(&self.scroller, 0.0, 1.0, fade::RAMP_MS, target);
            ramp.set_easing(adw::Easing::Linear);
            ramp
        });
        ramp.set_value_from(ramp.value());
        ramp.set_value_to(if on { 1.0 } else { 0.0 });
        ramp.play();
    }

    /// Cell `id`'s editor measures its fade from the selected cell's carets, through the cells
    /// between, rather than from its own.
    pub(super) fn fade_from_selected(self: &Rc<Self>, id: CellId, editor: &gtk::Widget) {
        let Some(editor) = editor.downcast_ref::<multicaret::View>() else {
            return;
        };
        let weak = Rc::downgrade(self);
        editor.fade_from(move || {
            let tab = weak.upgrade()?;
            // The selected cell's own carets, which the others are measured from.
            if tab.selected.get() == Some(id) {
                return None;
            }
            tab.measured(id)
        });
    }

    /// A cell was typed in: the fade follows, and the chrome hides if the keyboard is there.
    pub(super) fn typed(self: &Rc<Self>, id: CellId) {
        self.refade();
        let focused = self
            .view_of(Some(id))
            .and_then(|v| v.editor().map(|e| e.view.has_focus()));
        if focused == Some(true) {
            self.focus.typed.emit(self);
        }
    }

    /// The carets or the selected cell moved: the cells near the view are faded afresh, if what
    /// the fade is measured from changed.
    pub(super) fn refade(&self) {
        if !self.focus.on.get() {
            return;
        }
        let from = self.view_of(self.selected.get()).and_then(|view| {
            let span = fade::span(fader(&view)?);
            Some((view.id, *span.start(), *span.end(), view.line_count()))
        });
        if self.focus.from.replace(from) == from {
            return;
        }
        for view in self.near.borrow().iter() {
            if let Some(editor) = view.editor() {
                editor.view.queue_draw();
            }
        }
        self.shade_outputs(self.strength());
    }

    /// The cells near the view are now `near`, after `was`: those new to it take the fade, which
    /// moved on while they were away.
    pub(super) fn fade_near(&self, was: &[Rc<CellView>]) {
        if !self.focus.on.get() {
            return;
        }
        let strength = self.strength();
        for view in self.near.borrow().iter() {
            if was.iter().any(|w| Rc::ptr_eq(w, view)) {
                continue;
            }
            if let Some(editor) = fader(view) {
                editor.set_fade(true);
                editor.queue_draw();
            }
            self.shade(view, strength);
        }
    }

    /// The selected cell's carets, as lines of cell `id`.
    fn measured(&self, id: CellId) -> Option<RangeInclusive<i32>> {
        let views = self.views.borrow();
        let from = views
            .iter()
            .position(|v| Some(v.id) == self.selected.get())?;
        let to = views.iter().position(|v| v.id == id)?;
        // Its own carets: its editor's measure answers nothing.
        let span = fade::span(fader(&views[from])?);
        Some(across(span, from, to, |i| views[i].line_count()))
    }

    fn strength(&self) -> f64 {
        self.focus.ramp.get().map_or(0.0, |ramp| ramp.value())
    }

    /// The outputs of the cells near the view at `strength` of the fade; with none left, every
    /// cell's back to whole, those faded while near and since gone from it too.
    fn shade_outputs(&self, strength: f64) {
        if strength == 0.0 {
            for view in self.views.borrow().iter() {
                view.outputs.widget.set_opacity(1.0);
            }
            return;
        }
        for view in self.near.borrow().iter() {
            self.shade(view, strength);
        }
    }

    /// `view`'s outputs at `strength` of the fade, which they take from the cell's last line.
    fn shade(&self, view: &CellView, strength: f64) {
        let alpha = self.measured(view.id).map_or(1.0, |span| {
            fade::alpha(fade::distance(view.line_count() - 1, &span))
        });
        let opacity = 1.0 - strength * (1.0 - f64::from(alpha));
        view.outputs.widget.set_opacity(opacity);
    }
}

/// `view`'s editor, which draws the fade, once built.
fn fader(view: &CellView) -> Option<&multicaret::View> {
    view.editor()?.view.downcast_ref::<multicaret::View>()
}

/// `span`, lines of cell `from`, as lines of cell `to`, counted on through the cells between,
/// cell `i` being `lines(i)` lines: the notebook's lines are its cells' in order.
fn across(
    span: RangeInclusive<i32>,
    from: usize,
    to: usize,
    lines: impl Fn(usize) -> i32,
) -> RangeInclusive<i32> {
    let shift = match to >= from {
        true => -(from..to).map(&lines).sum::<i32>(),
        false => (to..from).map(&lines).sum(),
    };
    span.start() + shift..=span.end() + shift
}

#[cfg(test)]
mod tests {
    use super::across;
    use crate::fade::distance;

    /// Cells of 3, 2 and 4 lines, the carets on the middle cell's last line: the cell below is
    /// measured on from it, its first line one line further, and the cell above back from it.
    #[test]
    fn the_fade_runs_on_through_the_cells() {
        let lines = |i: usize| [3, 2, 4][i];
        assert_eq!(across(1..=1, 1, 1, lines), 1..=1);
        let below = across(1..=1, 1, 2, lines);
        assert_eq!(distance(0, &below), 1);
        assert_eq!(distance(3, &below), 4);
        let above = across(1..=1, 1, 0, lines);
        assert_eq!(distance(2, &above), 2);
        assert_eq!(distance(0, &above), 4);
    }
}
