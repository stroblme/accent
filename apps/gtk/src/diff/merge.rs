//! A file git left unmerged, in three columns: a side of the conflict, the file itself with git's
//! markers in it, and the other side. The middle column is the tab's own editor, where the merge
//! is made; the side columns are read-only, each showing one of the conflict's stages — Current,
//! Base or Incoming, from the title row — and tinted where it differs from the file. Above each
//! conflict block every column leaves a strip of room: an arrow on a side's strip takes that side
//! (`Conflicts::accept`), Both in the middle takes both.
//!
//! The rows are [`diff::align3`]'s, laid out by [`Columns`] as a comparison's are: one scroll,
//! padding that keeps each row level, unchanged runs hidden behind a button, the reader's line
//! kept where it is.

use accent_core::conflict::{self, Take};
use accent_core::diff::{self, DiffLine, Op};
use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ops::Range;
use std::rc::{Rc, Weak};

use super::columns::{Anchor, Columns, GAP_PX, Rows};
use super::links::run_of;
use super::pool::Role;
use super::{CONTEXT, Pane, TAG_GAP, install_tags, line_starts, normalise};
use crate::conflict::Conflicts;
use crate::editor;

/// A side column's lines that differ from the file, in the tint of the stage it shows.
const TAG_SIDE: &str = "merge-side";

/// What a side column can show: git's stages `:1`, `:2` and `:3`, by index.
pub const BASE: usize = 0;
pub const CURRENT: usize = 1;
pub const INCOMING: usize = 2;

/// The editor's column, between the two sides.
const MID: usize = 1;

pub struct Merge {
    weak: Weak<Merge>,
    columns: Rc<Columns>,
    /// The three stages' texts, [`BASE`], [`CURRENT`], [`INCOMING`].
    stages: [String; 3],
    /// The stage each side column shows, left and right.
    shown: [Cell<usize>; 2],
    /// The title row's choice of stage, left and right.
    pickers: [gtk::DropDown; 2],
    /// Per row, the line each column shows there: [`Three`]'s rows with a row of room before each
    /// conflict block.
    lines: RefCell<Vec<[Option<usize>; 3]>>,
    /// Per column, where each line of its text starts, in characters.
    starts: RefCell<[Vec<i32>; 3]>,
    /// The rows hidden right now, each with the key it can be opened by: its first line in the
    /// left column, which typing in the middle does not renumber.
    hidden: RefCell<Vec<(Range<usize>, usize)>>,
    /// Lines of the left column the reader asked to see, as a comparison keeps them.
    opened: RefCell<HashSet<usize>>,
    /// Show All Unchanged Lines, in the middle column's title row.
    unfold: gtk::ToggleButton,
    /// Mark Resolved, beside it.
    resolved: gtk::Button,
    /// The tab's conflict blocks, which the arrows resolve.
    conflicts: Rc<Conflicts>,
    handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    laid: RefCell<Option<Box<dyn Fn()>>>,
}

impl Merge {
    /// `left` and `right` read-only columns over the stages `CURRENT` and `INCOMING` of
    /// `stages`, `mid` the editor's column; `titles` name each stage in the pickers. `conflicts` is
    /// the editor's, its band off while the merge is up.
    pub fn new(
        [left, mid, right]: [Pane; 3],
        stages: [String; 3],
        titles: [String; 3],
        conflicts: Rc<Conflicts>,
    ) -> Rc<Self> {
        for pane in [&left, &mid, &right] {
            install_tags(&pane.buffer);
        }
        for pane in [&left, &right] {
            let table = pane.buffer.tag_table();
            if table.lookup(TAG_SIDE).is_none() {
                table.add(&gtk::TextTag::new(Some(TAG_SIDE)));
            }
        }
        // The title of each side column is its choice of stage.
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        let pickers = [(&left, CURRENT), (&right, INCOMING)].map(|(pane, stage)| {
            let picker = gtk::DropDown::from_strings(&titles);
            picker.set_selected(stage as u32);
            picker.add_css_class("flat");
            picker.set_hexpand(true);
            picker.set_halign(gtk::Align::Start);
            if let Some(label) = pane.header.first_child() {
                label.set_visible(false);
            }
            pane.header.prepend(&picker);
            picker
        });
        let unfold = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text("Show All Unchanged Lines")
            .css_classes(["flat"])
            .focus_on_click(false)
            .build();
        mid.header
            .insert_child_after(&unfold, mid.header.first_child().as_ref());
        let resolved = gtk::Button::builder()
            .icon_name("object-select-symbolic")
            .tooltip_text("Mark Resolved")
            .css_classes(["flat"])
            .focus_on_click(false)
            .build();
        mid.header
            .insert_child_after(&resolved, mid.header.first_child().as_ref());

        let columns = Columns::new(vec![left, mid, right], Some(MID));
        let this = Rc::new_cyclic(|weak| Merge {
            weak: weak.clone(),
            columns,
            stages,
            shown: [Cell::new(CURRENT), Cell::new(INCOMING)],
            pickers,
            lines: RefCell::default(),
            starts: RefCell::default(),
            hidden: RefCell::default(),
            opened: RefCell::default(),
            unfold,
            resolved,
            conflicts,
            handlers: RefCell::default(),
            laid: RefCell::default(),
        });

        let weak = this.weak.clone();
        let connect = |object: glib::Object, id: glib::SignalHandlerId| {
            this.handlers.borrow_mut().push((object, id));
        };
        for (column, pane) in this.columns.panes.iter().enumerate() {
            // As a comparison's: the theme foreground and the font are there once it is mapped.
            let w = weak.clone();
            pane.view.connect_map(move |_| {
                if let Some(m) = w.upgrade() {
                    m.restyle();
                    m.columns.schedule_relayout();
                }
            });
            let w = weak.clone();
            *pane.pool.act.borrow_mut() = Some(Rc::new(move |role, _| {
                let Some(m) = w.upgrade() else { return };
                match role {
                    Role::Gap { key, .. } => m.open_run(key),
                    Role::Block(i) => m.accept(i, column),
                }
            }));
        }
        let w = weak.clone();
        let style = adw::StyleManager::default();
        let id = style.connect_dark_notify(move |_| {
            if let Some(m) = w.upgrade() {
                m.restyle();
            }
        });
        connect(style.upcast(), id);
        for (side, picker) in this.pickers.iter().enumerate() {
            let w = weak.clone();
            let id = picker.connect_selected_notify(move |picker| {
                if let Some(m) = w.upgrade() {
                    m.show_stage(side, picker.selected() as usize);
                }
            });
            connect(picker.clone().upcast(), id);
        }
        let w = weak.clone();
        this.unfold.connect_toggled(move |_| {
            if let Some(m) = w.upgrade() {
                m.toggle_all();
            }
        });
        this.lay(true);
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.columns.widget()
    }

    fn pane(&self, column: usize) -> &Pane {
        &self.columns.panes[column]
    }

    fn text(&self, column: usize) -> String {
        let buffer = &self.pane(column).buffer;
        let (s, e) = buffer.bounds();
        buffer.text(&s, &e, true).to_string()
    }

    /// Re-read the file and lay the columns over it again: on every edit of the middle column.
    pub fn refresh(&self) {
        self.lay(false);
    }

    /// Run `f` on Mark Resolved in the title row.
    pub fn connect_resolved(&self, f: impl Fn() + 'static) {
        self.resolved.connect_clicked(move |_| f());
    }

    /// Run `f` whenever the rows have been laid again, and once now: see `Compare::on_laid`.
    pub fn on_laid(&self, f: impl Fn() + 'static) {
        f();
        *self.laid.borrow_mut() = Some(Box::new(f));
    }

    /// Show `stage` in side column `side` (0 left, 1 right), its line at the top kept there.
    pub fn show_stage(&self, side: usize, stage: usize) {
        let Some(text) = self.stages.get(stage).map(|text| normalise(text)) else {
            return;
        };
        self.shown[side].set(stage);
        self.pickers[side].set_selected(stage as u32);
        let column = side * 2;
        if self.text(column) == text {
            return;
        }
        if !self.columns.opening() {
            let top = self.columns.top_line(MID);
            self.columns.keep.set(top);
        }
        // The runs the reader opened are keyed by the left column's lines, which are others now.
        if side == 0 {
            self.opened.borrow_mut().clear();
        }
        let pane = self.pane(column);
        pane.buffer.set_text(&text);
        editor::style_companion(pane.flavour, &pane.buffer, &pane.view);
        self.restyle();
        self.refresh();
    }

    /// Take a side of the `i`th conflict block, from an arrow on `column`'s strip: the side that
    /// column shows, or both from the middle.
    fn accept(&self, i: usize, column: usize) {
        let take = match column {
            MID => Take::Both,
            _ => match self.shown[column / 2].get() {
                CURRENT => Take::Current,
                INCOMING => Take::Incoming,
                _ => return,
            },
        };
        self.conflicts.accept(i, take);
    }

    /// Open the hidden run keyed `key`, the scroll held where it was: see `Compare::open_run`.
    fn open_run(&self, key: usize) {
        self.columns.hold_scroll();
        let rows = self
            .hidden
            .borrow()
            .iter()
            .find(|(_, k)| *k == key)
            .map(|(gap, _)| gap.len());
        self.opened
            .borrow_mut()
            .extend(key..key + rows.unwrap_or(1));
        self.refresh();
    }

    /// The run hiding `offset` in the middle column opened, as a jump into it opens it: `true`
    /// when one was. See `Compare::open_hiding`.
    pub fn open_hiding(&self, offset: i32) -> bool {
        let key = {
            let (lines, starts, hidden) = (
                self.lines.borrow(),
                self.starts.borrow(),
                self.hidden.borrow(),
            );
            let number = |r: usize| lines[r][MID].unwrap_or(1);
            let st = &starts[MID];
            hidden
                .iter()
                .find(|(gap, _)| {
                    (st[number(gap.start) - 1]..st[number(gap.end - 1)]).contains(&offset)
                })
                .map(|(_, key)| *key)
        };
        let Some(key) = key else {
            return false;
        };
        self.open_run(key);
        true
    }

    /// Every run opened, as Show All Unchanged Lines goes down, or hidden again as it comes up,
    /// the caret's line kept where it is: see `Compare::toggle_all`.
    fn toggle_all(&self) {
        if !self.columns.opening() {
            let top = || self.columns.top_line(MID);
            self.columns.hold(self.columns.caret_line().or_else(top));
        }
        if !self.unfold.is_active() {
            self.opened.borrow_mut().clear();
        }
        self.refresh();
    }

    /// Diff the side columns against the file and lay the rows. With `opening`, the caret goes to
    /// the first conflict block, or the first change where there is none.
    fn lay(&self, opening: bool) {
        let texts = [0, 1, 2].map(|c| self.text(c));
        let three = diff::align3(&texts[0], &texts[1], &texts[2]);
        let starts = texts.each_ref().map(|text| line_starts(text));
        for (c, pane) in self.columns.panes.iter().enumerate() {
            let (start, end) = pane.buffer.bounds();
            pane.buffer.remove_tag_by_name(TAG_GAP, &start, &end);
            if c != MID {
                pane.buffer.remove_tag_by_name(TAG_SIDE, &start, &end);
            }
        }
        let tint = |c: usize, n: usize| {
            let buffer = &self.pane(c).buffer;
            let at = |offset| buffer.iter_at_offset(offset);
            buffer.apply_tag_by_name(TAG_SIDE, &at(starts[c][n - 1]), &at(starts[c][n]));
        };
        // A side's line that is not the file's.
        for (c, side) in [(0, &three.left), (2, &three.right)] {
            for line in side.iter().filter(|line| line.op == Op::Delete) {
                if let Some(n) = line.old_line {
                    tint(c, n);
                }
            }
        }

        // The rows, with one of room before each block's `<<<<<<<` line. A side's lines beside a
        // block are tinted too, where they are the file's: the block reads across all three
        // columns until it is taken.
        let number = |lines: &[DiffLine], i: Option<usize>| i.and_then(|i| lines[i].old_line);
        let blocks = conflict::blocks(&texts[MID]);
        let line_at = |byte: usize| {
            let at = texts[MID][..byte].chars().count() as i32;
            starts[MID][..starts[MID].len() - 1].partition_point(|&s| s <= at)
        };
        let block_lines: Vec<(usize, usize)> = blocks
            .iter()
            .map(|block| (line_at(block.range.start), line_at(block.theirs.end)))
            .collect();
        let (mut lines, mut changed, mut rooms) = (Vec::new(), Vec::new(), Vec::new());
        // Per side, whether the row is one the side and the file differ in, a block's included:
        // what the connectors between them are drawn over.
        let mut differ: [Vec<bool>; 2] = Default::default();
        let same =
            |lines: &[DiffLine], i: Option<usize>| i.is_some_and(|i| lines[i].op == Op::Equal);
        // The `>>>>>>>` line of the block the rows are in.
        let mut inside = None;
        for (row, differs) in three.rows.iter().zip(three.changed()) {
            if let Some(&(_, end)) = block_lines
                .iter()
                .find(|(start, _)| row.mid == Some(*start))
            {
                rooms.push(lines.len());
                lines.push([None; 3]);
                changed.push(true);
                differ.iter_mut().for_each(|d| d.push(false));
                inside = Some(end);
            }
            let line = [
                number(&three.left, row.left),
                row.mid,
                number(&three.right, row.right),
            ];
            if inside.is_some() {
                for c in [0, 2] {
                    if let Some(n) = line[c] {
                        tint(c, n);
                    }
                }
            }
            for (d, (side, i)) in differ
                .iter_mut()
                .zip([(&three.left, row.left), (&three.right, row.right)])
            {
                let any = i.is_some() || row.mid.is_some();
                d.push(inside.is_some() || (any && !same(side, i)));
            }
            if row.mid.is_some() && row.mid == inside {
                inside = None;
            }
            lines.push(line);
            changed.push(differs);
        }

        if opening {
            let at = match blocks.first() {
                Some(block) => Some(texts[MID][..block.range.start].chars().count() as i32),
                None => diff::hunks_of(&changed)
                    .first()
                    .and_then(|hunk| lines[hunk.clone()].iter().find_map(|l| l[MID]))
                    .map(|n| starts[MID][n - 1]),
            };
            if let Some(at) = at {
                let buffer = &self.pane(MID).buffer;
                buffer.place_cursor(&buffer.iter_at_offset(at));
            }
        }
        let caret = {
            let buffer = &self.pane(MID).buffer;
            buffer.iter_at_mark(&buffer.get_insert()).offset()
        };

        // Unchanged runs, hidden in every column as a comparison hides them.
        let span = |c: usize, gap: &Range<usize>| -> (i32, i32) {
            let n = |r: usize| lines[r][c].unwrap_or(1);
            (starts[c][n(gap.start) - 1], starts[c][n(gap.end - 1)])
        };
        let gaps = diff::gaps_of(&changed, CONTEXT);
        self.unfold.set_sensitive(!gaps.is_empty());
        let mut hidden = Vec::new();
        for gap in gaps.into_iter().filter(|_| !self.unfold.is_active()) {
            let key = lines[gap.start][0].unwrap_or(0);
            let last = lines[gap.end - 1][0].unwrap_or(0);
            if self
                .opened
                .borrow()
                .iter()
                .any(|k| (key..=last).contains(k))
            {
                continue;
            }
            let (from, to) = span(MID, &gap);
            if caret >= from && caret < to {
                continue;
            }
            for (c, pane) in self.columns.panes.iter().enumerate() {
                let (from, to) = span(c, &gap);
                let at = |offset| pane.buffer.iter_at_offset(offset);
                pane.buffer.apply_tag_by_name(TAG_GAP, &at(from), &at(to));
            }
            hidden.push((gap, key));
        }

        // The buttons: a hidden run's in every column, and the arrows on each block's strip.
        for pane in &self.columns.panes {
            pane.pool.unclaim();
        }
        let mut overlays = Vec::new();
        for (c, pane) in self.columns.panes.iter().enumerate() {
            for (gap, key) in &hidden {
                let role = Role::Gap {
                    key: *key,
                    rows: gap.len(),
                };
                let widget = pane.pool.claim(&pane.view, role);
                overlays.push((c, widget, Anchor::Gap(gap.start)));
            }
            let (button, align) = match c {
                MID => (
                    (
                        "Both",
                        "Accept Both: the current change, then the incoming one",
                    ),
                    gtk::Align::Center,
                ),
                _ => {
                    let icon = match c {
                        0 => "go-next-symbolic",
                        _ => "go-previous-symbolic",
                    };
                    let tip = match self.shown[c / 2].get() {
                        CURRENT => "Accept Current",
                        INCOMING => "Accept Incoming",
                        _ => continue,
                    };
                    let align = match c {
                        0 => gtk::Align::End,
                        _ => gtk::Align::Start,
                    };
                    ((icon, tip), align)
                }
            };
            *pane.pool.buttons.borrow_mut() = vec![button];
            for (i, &room) in rooms.iter().enumerate() {
                let widget = pane.pool.claim(&pane.view, Role::Block(i));
                overlays.push((c, widget, Anchor::Room(room, align)));
            }
        }
        for pane in &self.columns.panes {
            pane.pool.hide_unclaimed();
        }

        // Each connector joins the rows of its run the side has lines in to those the file has.
        let runs = [0, 1].map(|s| {
            let has = |k: usize, r: usize| lines[r][s + k].is_some();
            (diff::hunks_of(&differ[s]).iter())
                .map(|run| run_of(run, has))
                .collect()
        });
        self.columns.links.set_runs(runs.to_vec());

        let mut extra = vec![0; lines.len()];
        for &room in &rooms {
            extra[room] = GAP_PX;
        }
        let laid = Rows {
            lines: (0..3)
                .map(|c| lines.iter().map(|line| line[c]).collect())
                .collect(),
            starts: starts.to_vec(),
            first: diff::hunks_of(&changed).first().map(|hunk| hunk.start),
            changed,
            hidden: hidden.iter().map(|(gap, _)| gap.clone()).collect(),
            extra,
            overlays,
        };
        *self.lines.borrow_mut() = lines;
        *self.starts.borrow_mut() = starts;
        *self.hidden.borrow_mut() = hidden;
        self.columns.lay(laid);
        if let Some(laid) = self.laid.borrow().as_ref() {
            laid();
        }
    }

    /// The side columns' tints from the resolved theme, as the conflict blocks' in the middle
    /// column are (`conflict::tints`), each in the tint of the stage it shows.
    pub fn restyle(&self) {
        let page = crate::highlight::page(adw::StyleManager::default().is_dark());
        for (side, column) in [0, 2].into_iter().enumerate() {
            let pane = self.pane(column);
            editor::restyle_companion(pane.flavour, &pane.buffer, &pane.view);
            let tints = crate::conflict::tints(pane.view.color(), page);
            let (tint, marker) = match self.shown[side].get() {
                CURRENT => tints[0],
                BASE => tints[1],
                _ => tints[2],
            };
            if let Some(tag) = pane.buffer.tag_table().lookup(TAG_SIDE) {
                tag.set_paragraph_background_rgba(Some(&tint));
            }
            self.columns.links.set_tints(side, [tint; 2], [marker; 2]);
        }
    }

    /// Put the editor's page on the side columns: see `Columns::follow_editor`.
    pub fn follow_editor(&self, refont: bool) {
        self.columns.follow_editor(refont);
    }

    /// Everything off the columns: what leaving the merge view does before they part.
    pub fn leave(&self) {
        for pane in &self.columns.panes {
            let (start, end) = pane.buffer.bounds();
            pane.buffer.remove_tag_by_name(TAG_GAP, &start, &end);
        }
        self.columns.leave();
        for (object, id) in self.handlers.borrow_mut().drain(..) {
            object.disconnect(id);
        }
    }
}

// --- for the bench ------------------------------------------------------------------------

#[cfg(feature = "bench")]
impl Merge {
    /// How many rows some column lays out off the grid: 0 is the claim.
    pub fn misaligned(&self) -> usize {
        self.columns.misaligned()
    }

    pub fn settled(&self) -> bool {
        self.columns.settled()
    }

    /// (rows, conflict blocks, hidden runs, buttons) on screen right now.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let rows = self.columns.rows.borrow();
        let blocks = rows.extra.iter().filter(|&&px| px > 0).count();
        (
            rows.changed.len(),
            blocks,
            rows.hidden.len(),
            rows.overlays.len(),
        )
    }

    /// The middle of the `i`th block's buttons in each column, in the window, `None` where a
    /// column has none: the three are one strip, so the claim is one height.
    pub fn block_centres(&self, i: usize) -> [Option<i32>; 3] {
        let rows = self.columns.rows.borrow();
        let room = rows
            .extra
            .iter()
            .enumerate()
            .filter(|(_, px)| **px > 0)
            .nth(i)
            .map(|(r, _)| r);
        [0, 1, 2].map(|c| {
            let (_, widget, _) = rows.overlays.iter().find(|(column, _, anchor)| {
                *column == c && matches!(anchor, Anchor::Room(r, _) if Some(*r) == room)
            })?;
            let bounds = widget.compute_bounds(&self.columns.paned)?;
            Some((bounds.y() + bounds.height() / 2.0).round() as i32)
        })
    }

    /// Each connector's ends, as `(top, bottom)` on its strip's left and on its right, by strip,
    /// in the drawing's pixels, and how many of those ends are not where GTK draws the line of the
    /// row they stand on: 0 is the claim, what [`Merge::misaligned`] is to the rows.
    pub fn links(&self) -> (Vec<Vec<super::links::Ends>>, usize) {
        self.columns.links_check()
    }

    /// How wide each column and each strip between them is.
    pub fn widths(&self) -> (Vec<i32>, Vec<i32>) {
        self.columns.links.widths()
    }

    /// How many lines of each side column carry its tint.
    pub fn tinted(&self) -> [usize; 2] {
        [0, 2].map(|c| {
            let buffer = &self.pane(c).buffer;
            let tag = buffer.tag_table().lookup(TAG_SIDE);
            (0..buffer.line_count())
                .filter_map(|n| buffer.iter_at_line(n))
                .filter(|at| tag.as_ref().is_some_and(|tag| at.has_tag(tag)))
                .count()
        })
    }

    /// What the button on block `i`'s strip in `column` does.
    pub fn press(&self, i: usize, column: usize) {
        self.accept(i, column);
    }

    /// What each side column's title row says it shows.
    pub fn titles(&self) -> [String; 2] {
        self.pickers.each_ref().map(|picker| {
            picker
                .selected_item()
                .and_downcast::<gtk::StringObject>()
                .map(|s| s.string().to_string())
                .unwrap_or_default()
        })
    }

    /// The view of `column`, 0 to 2.
    pub fn view(&self, column: usize) -> gtk::TextView {
        self.columns.panes[column].view.clone().upcast()
    }

    pub fn unfold(&self) -> &gtk::ToggleButton {
        &self.unfold
    }

    pub fn paned(&self) -> &gtk::Paned {
        &self.columns.paned
    }
}
