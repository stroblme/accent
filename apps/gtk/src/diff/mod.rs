//! Side-by-side comparison: the diff as highlighting laid over two buffers.
//!
//! Knows nothing about vaults, tabs or git. It is handed two panes, each a view over a buffer,
//! and one of the buffers may be the user's own editor. The buffers are the source of truth on
//! both sides — a pane that is typed into is never rewritten, only re-tagged — and everything
//! the comparison shows is a tag or an overlay: the row tints and word emphasis, the unchanged
//! runs hidden behind a "⋯ N lines" button, the blank space that keeps row `i` beside row `i`
//! once one side has grown, and the buttons that take a hunk across.
//!
//! Alignment used to be empty filler lines in the text, which had to be stripped back out of an
//! edited pane and meant rewriting the buffer under the caret whenever the columns drifted. It is
//! `pixels-above-lines` now, so an edit costs a re-diff and a pass of tags and nothing the undo
//! stack can see.

use accent_core::diff::{self, DiffLine, Op, Row};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ops::{Range, RangeInclusive};
use std::rc::{Rc, Weak};

use crate::editor::{self, Flavour};

mod pad;
mod pool;

use pad::{UNMEASURED, bands, carried, is_pad, measure, pad, padding, reclaim, unmeasured};
pub use pool::Pool;
use pool::Role;

const TAG_ADDED: &str = "diff-added";
const TAG_REMOVED: &str = "diff-removed";
const TAG_ADDED_EMPH: &str = "diff-added-emph";
const TAG_REMOVED_EMPH: &str = "diff-removed-emph";
/// The runs of unchanged lines a changes-only view hides. Its own tag rather than `fold.rs`'s,
/// so the editor's fold bookkeeping never mistakes a hidden run for a block it folded.
pub(crate) const TAG_GAP: &str = "diff-gap";
/// Unchanged lines kept on each side of a change, as `git diff` keeps them.
const CONTEXT: usize = 3;

/// Where the first hunk lands when a comparison opens, as a fraction of the view's height. A
/// quarter down rather than at the top, so the lines that lead up to the change are visible too.
const FIRST_HUNK_AT: f64 = 0.25;

/// Blank space a hidden run leaves behind, for the button that opens it to sit in.
const GAP_PX: i32 = 28;
/// Inset of the hunk buttons from the pane's right edge.
const INSET: i32 = 8;
/// How often a relayout asks again for the heights GTK had not validated yet, and how long it
/// waits between asking: GTK validates a screenful per idle, so a few are enough for any note.
const SETTLE: u8 = 10;
const SETTLE_AFTER: std::time::Duration = std::time::Duration::from_millis(100);

/// Row backgrounds. This is the one place DESIGN.md's "only accent, foreground and is_dark" rule
/// bends: a diff has to read as green and red, and libadwaita publishes its success/error colours
/// as CSS variables only, which GTK will resolve in a stylesheet but not hand back to Rust. So the
/// hue is a fixed weight and everything else is derived: it is mixed with the theme foreground,
/// which pulls the tint dark on a light theme and light on a dark one, then laid down at a low
/// alpha over the view background so the text on top keeps its contrast either way.
pub(crate) const ADDED_HUE: (f32, f32, f32) = (0.15, 0.70, 0.35);
pub(crate) const REMOVED_HUE: (f32, f32, f32) = (0.80, 0.20, 0.25);
/// Not a diff colour: the amber a warning is underlined in (`diagnostics.rs`). It lives beside
/// the other two because they are one palette and are mixed by the same [`tint`].
pub(crate) const WARNING_HUE: (f32, f32, f32) = (0.85, 0.60, 0.10);
/// Nor this: the blue a conflict block's incoming side is tinted in (`conflict.rs`), its current
/// side taking the green, as VS Code tints the two.
pub(crate) const INCOMING_HUE: (f32, f32, f32) = (0.20, 0.50, 0.90);
/// Share of the tint that is the hue; the rest is the foreground.
const HUE_MIX: f32 = 0.65;
const CHANGE_ALPHA: f32 = 0.16;
/// The words that actually differ, in the same hue over the row's own background. Emphasis is
/// colour only: bold would change advance widths and pull the two panes out of alignment.
const EMPH_ALPHA: f32 = 0.35;
/// The blank a hunk that only adds or only deletes leaves on the other side, in the hue of the
/// lines it faces: half a row's tint, so it reads as the same block without passing for lines.
const BAND_ALPHA: f32 = 0.08;

/// Which of the two texts a pane shows: `Old` is the left column.
pub use accent_core::diff::Side;

/// The row and word-emphasis tags a changed line gets on `side`, or `None` for an unchanged one.
/// `align` never puts an insertion on the old side, nor a deletion on the new.
fn tags(side: Side, op: Op) -> Option<(&'static str, &'static str)> {
    match (side, op) {
        (Side::Old, Op::Delete) => Some((TAG_REMOVED, TAG_REMOVED_EMPH)),
        (Side::New, Op::Insert) => Some((TAG_ADDED, TAG_ADDED_EMPH)),
        _ => None,
    }
}

/// One column of the comparison. `root` is what the paned shows, `header` its title row; the
/// scroller is named so the two columns can share one vertical adjustment.
pub struct Pane {
    pub root: gtk::Widget,
    pub header: gtk::Box,
    pub view: sourceview5::View,
    pub buffer: sourceview5::Buffer,
    pub scroller: gtk::ScrolledWindow,
    pub flavour: Flavour,
    /// The buttons laid over this view. Handed in rather than made here because they outlive
    /// a comparison on a view that does: see [`Pool`].
    pub pool: Rc<Pool>,
}

/// A pane's title bar, with room for one control beside it. The comparison puts its Show All
/// Unchanged Lines in front of that control on one of the two.
pub fn header(title: &str, trailing: Option<&gtk::Widget>) -> gtk::Box {
    let label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    label.add_css_class("heading");
    label.add_css_class("dim-label");
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_start(12)
        .margin_end(6)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    row.append(&label);
    if let Some(widget) = trailing {
        row.append(widget);
    }
    row
}

/// A read-only column over `text`, titled: every side that is not the user's own editor. `name`
/// is the widget name the font and zoom CSS is written for, see [`editor::companion`].
pub fn pane(
    title: &str,
    flavour: Flavour,
    text: &str,
    name: &str,
    language: Option<&sourceview5::Language>,
) -> Pane {
    let (view, buffer) = editor::companion(flavour, &normalise(text), name, language);
    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = header(title, None);
    root.append(&header);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&scroller);
    Pane {
        root: root.upcast(),
        header,
        view,
        buffer,
        scroller,
        flavour,
        pool: Rc::default(),
    }
}

/// Put `scroller` on `adjustment`. GTK 4.22's `set_vadjustment` leaves the overlay scrollbar's
/// fade handler, which a realized scroller connects to its adjustment, on the one it leaves; once
/// the scroller is freed, the next scroll of that adjustment runs the handler on freed memory.
/// Overlay scrolling switched off and back on around the swap takes the handler along.
fn swap_vadjustment(scroller: &gtk::ScrolledWindow, adjustment: &gtk::Adjustment) {
    let overlay = scroller.is_overlay_scrolling();
    scroller.set_overlay_scrolling(false);
    scroller.set_vadjustment(Some(adjustment));
    scroller.set_overlay_scrolling(overlay);
}

/// What a companion holds: `\n` line endings, as the editor's own buffer does, so a line's
/// character count is the same to the diff and to the buffer.
pub(crate) fn normalise(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Where each line of `text` starts, in characters, with the text's end appended so line `n`
/// (1-based) always spans `starts[n - 1]..starts[n]`, its line ending included. A line ends where
/// `similar` ends one: at `\n`, at `\r\n`, and at a `\r` on its own, so the diff's line numbers
/// index this.
///
/// Counted in characters and not taken from the buffer's own line numbers: `GtkTextBuffer`
/// breaks a line at U+2029 too, and the diff does not, so a note carrying one puts the two
/// numberings permanently out of step. A character offset means the same thing to both.
fn line_starts(text: &str) -> Vec<i32> {
    let mut starts = vec![0];
    let mut chars = text.chars().peekable();
    let mut at = 0;
    while let Some(c) = chars.next() {
        at += 1;
        if c == '\r' && chars.next_if_eq(&'\n').is_some() {
            at += 1;
        }
        if matches!(c, '\n' | '\r') {
            starts.push(at);
        }
    }
    starts.push(at);
    starts
}

/// Where `a` and `b` differ, in characters: how much they share at the start, and where in each
/// what they share at the end begins.
fn differing(a: &str, b: &str) -> (usize, usize, usize) {
    let start = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let (na, nb) = (a.chars().count(), b.chars().count());
    let end = a
        .chars()
        .rev()
        .zip(b.chars().rev())
        .take(na.min(nb) - start)
        .take_while(|(x, y)| x == y)
        .count();
    (start, na - end, nb - end)
}

/// The lines, 1-based and inclusive, a selection of the characters `from..to` covers, `starts`
/// being [`line_starts`]: the one it begins in to the one holding its last character, so a
/// selection taken to the start of the next line does not take that line too.
fn lines_between(starts: &[i32], from: i32, to: i32) -> RangeInclusive<usize> {
    let line = |at: i32| starts[..starts.len() - 1].partition_point(|&s| s <= at);
    line(from)..=line((to - 1).max(from))
}

/// Where the first change starts on `side`, in characters: its first line of the first hunk, or,
/// where that hunk only takes lines away from `side`, its next line after it. `starts` is `side`'s
/// [`line_starts`]. `None` when nothing differs.
fn first_change(lines: &[DiffLine], rows: &[Row], starts: &[i32], side: Side) -> Option<i32> {
    let hunk = diff::hunks(lines, rows).into_iter().next()?;
    let next = rows[hunk.start..]
        .iter()
        .find_map(|r| side.of(r).and_then(|i| side.number(&lines[i])));
    Some(next.map_or(*starts.last()?, |n| starts[n - 1]))
}

/// The lines a selection of exactly `hunk` covers: `side`'s lines in it, or the other side's where
/// it only takes lines out of `side`'s. A selection reads whole rows ([`diff::apply_lines`]), so
/// either covers the hunk whole.
fn hunk_lines(
    lines: &[DiffLine],
    rows: &[Row],
    hunk: &Range<usize>,
    side: Side,
) -> Option<(Side, RangeInclusive<usize>)> {
    [side, side.other()].into_iter().find_map(|side| {
        let mut numbers = rows[hunk.clone()]
            .iter()
            .filter_map(|r| side.number(&lines[side.of(r)?]));
        let first = numbers.next()?;
        Some((side, first..=numbers.next_back().unwrap_or(first)))
    })
}

/// Where the overlaid buttons sit, in rows.
#[derive(Clone, Copy)]
enum Anchor {
    /// At the right end of the row's top: the Take / Keep Both pair of the hunk starting there.
    Hunk(usize),
    /// Centred in the blank space a hidden run left at this row.
    Gap(usize),
}

/// What an entry [`Compare::offer`] puts on the panes' menus does with a selection, and what a
/// button [`Compare::offer_hunks`] puts on each hunk does with that hunk's lines.
pub type OnLines = Rc<dyn Fn(Side, RangeInclusive<usize>, &str, &str)>;

/// What a button on a hunk does, handed the hunk's rows.
type OnHunk = Rc<dyn Fn(&Compare, Range<usize>)>;

/// Where the view is kept until the rows are laid: see [`Compare::keep`].
#[derive(Clone, Copy)]
enum Keep {
    /// The first hunk at [`FIRST_HUNK_AT`] of the page, which is where a comparison opens.
    FirstHunk,
    /// The scroll a hidden run was opened at, held there: see [`Compare::open_run`].
    Scroll(f64),
    /// Line `.1` of side `.0`, its row `.2` pixels below the top of the view: see
    /// [`Compare::set_side`] and [`Compare::hold_line`].
    Line(Side, usize, i32),
}

/// What a relayout measured: every row's natural height per side (`None` where the side has no
/// visible line), the space both sides leave at a row on purpose, and where each row starts in
/// the shared column.
#[derive(Default)]
struct Grid {
    #[cfg(feature = "bench")]
    heights: [Vec<Option<i32>>; 2],
    #[cfg(feature = "bench")]
    extra: Vec<i32>,
    tops: Vec<i32>,
}

fn install_tags(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    for name in [
        TAG_ADDED,
        TAG_REMOVED,
        TAG_ADDED_EMPH,
        TAG_REMOVED_EMPH,
        UNMEASURED,
    ] {
        if table.lookup(name).is_none() {
            table.add(&gtk::TextTag::new(Some(name)));
        }
    }
    if table.lookup(TAG_GAP).is_none() {
        table.add(
            &gtk::TextTag::builder()
                .name(TAG_GAP)
                .invisible(true)
                .build(),
        );
    }
}

pub(crate) fn tint(hue: (f32, f32, f32), fg: gdk::RGBA, alpha: f32) -> gdk::RGBA {
    let mix = |h: f32, f: f32| h * HUE_MIX + f * (1.0 - HUE_MIX);
    gdk::RGBA::new(
        mix(hue.0, fg.red()),
        mix(hue.1, fg.green()),
        mix(hue.2, fg.blue()),
        alpha,
    )
}

/// A run of blank rows a comparison fills under the text: `y` and height in buffer coordinates,
/// and the hue of the lines it faces. See [`bands`].
pub(crate) type Band = (i32, i32, (f32, f32, f32));

/// The colour of a band of blank facing lines of `hue`, over a view whose text is `fg`: what
/// `multicaret::View` fills [`Compare`]'s bands with.
pub(crate) fn band(hue: (f32, f32, f32), fg: gdk::RGBA) -> gdk::RGBA {
    tint(hue, fg, BAND_ALPHA)
}

/// Re-derive the row backgrounds from the resolved theme foreground. Once the view is mapped
/// and again on every `notify::dark`, exactly as `highlight::restyle` does for the editor.
fn restyle_tags(buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    let fg = view.color();
    let table = buffer.tag_table();
    let set = |name: &str, colour: gdk::RGBA| {
        if let Some(t) = table.lookup(name) {
            t.set_paragraph_background_rgba(Some(&colour));
        }
    };
    set(TAG_ADDED, tint(ADDED_HUE, fg, CHANGE_ALPHA));
    set(TAG_REMOVED, tint(REMOVED_HUE, fg, CHANGE_ALPHA));
    // A character background, not a paragraph one, so it paints the words on top of the row.
    let emph = |name: &str, colour: gdk::RGBA| {
        if let Some(t) = table.lookup(name) {
            t.set_background_rgba(Some(&colour));
        }
    };
    emph(TAG_ADDED_EMPH, tint(ADDED_HUE, fg, EMPH_ALPHA));
    emph(TAG_REMOVED_EMPH, tint(REMOVED_HUE, fg, EMPH_ALPHA));
}

/// Two columns with a diff laid over them. Built once over two buffers and kept in step with
/// them from then on: [`Compare::refresh`] re-reads both and re-tags, and nothing here writes
/// into a buffer except the hunk buttons, which edit the user's side as the user would.
pub struct Compare {
    weak: Weak<Compare>,
    panes: [Pane; 2],
    /// Which side is the user's own editor, if either. Its text is read, never set, and the hunk
    /// buttons write into it.
    editable: Option<Side>,
    /// The buttons each hunk carries on the pane beside the editor: a label, a tooltip, and what
    /// the button does.
    hunk_buttons: RefCell<Vec<(&'static str, &'static str, OnHunk)>>,
    paned: gtk::Paned,
    lines: RefCell<Vec<DiffLine>>,
    rows: RefCell<Vec<Row>>,
    /// [`line_starts`] of each side's text.
    starts: RefCell<[Vec<i32>; 2]>,
    /// The row ranges hidden right now, each with the key it can be opened by.
    hidden: RefCell<Vec<(Range<usize>, usize)>>,
    /// Lines the user asked to see, every line of each run they opened, by their number on the
    /// side that is not typed into, which survives the edits that move everything else. A run
    /// holding one stays open, so an edit that splits a run or merges it with another hides
    /// nothing that was shown.
    opened: RefCell<HashSet<usize>>,
    /// Show All Unchanged Lines, in the title row: while it is down nothing is hidden.
    unfold: gtk::ToggleButton,
    overlays: RefCell<Vec<(Side, gtk::Widget, Anchor)>>,
    /// The grid the last relayout laid down, kept for [`Compare::misaligned`].
    grid: RefCell<Grid>,
    pending: RefCell<Option<glib::SourceId>>,
    /// How many more times a relayout that had to estimate a height may ask GTK again. Reset
    /// by every refresh; a bound, because a line GTK never validates would otherwise be asked
    /// about forever.
    settling: Cell<u8>,
    /// The right column's own vertical adjustment, given up for the left one's while the
    /// comparison lasts and handed back by [`Compare::leave`].
    own_vadjustment: gtk::Adjustment,
    /// The bottom margin each view was last given here, and how much of it is the blank under a
    /// side with no line: see [`Compare::page_bottom`].
    bottoms: [Cell<(i32, i32)>; 2],
    /// Where the view is kept until a relayout has laid every row, which clears it. The first
    /// hunk, as the comparison is built: a diff opens on what changed rather than on the top of a
    /// file whose first difference is four hundred lines down. The scroll a run was opened at (see
    /// [`Compare::open_run`]). After that where the view sits is the reader's business.
    keep: Cell<Option<Keep>>,
    handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    /// Each pane's context menu as [`Compare::offer`] left it, and the one it replaced, to put
    /// back when the comparison goes. Empty until something is offered.
    offered: RefCell<Vec<(gio::Menu, Option<gio::MenuModel>)>>,
    /// What to run once the rows have been laid again. The hidden runs move with every lay — a
    /// keystroke, a side re-read, a run opened — and what is drawn per line rather than per
    /// character has to follow them: see [`Compare::on_laid`].
    laid: RefCell<Option<Box<dyn Fn()>>>,
}

impl Drop for Compare {
    /// The read-only columns go with the comparison: see [`editor::release`]. The editor's view
    /// is its tab's.
    fn drop(&mut self) {
        for (pane, side) in self.panes.iter().zip([Side::Old, Side::New]) {
            if self.editable != Some(side) {
                editor::release(&pane.view);
            }
        }
    }
}

impl Compare {
    /// `editable` names the pane whose buffer is the user's; `hunk_buttons` puts Take / Keep Both
    /// on the other pane, which only means something when there is an editable side.
    pub fn new(old: Pane, new: Pane, editable: Option<Side>, hunk_buttons: bool) -> Rc<Self> {
        let take =
            |keep_own| -> OnHunk { Rc::new(move |c: &Compare, hunk| c.take(hunk, keep_own)) };
        let takes = match hunk_buttons && editable.is_some() {
            true => vec![
                ("Take", "Replace this hunk in Mine with Theirs", take(false)),
                ("Both", "Keep both versions of this hunk", take(true)),
            ],
            false => Vec::new(),
        };
        for pane in [&old, &new] {
            install_tags(&pane.buffer);
        }
        // Vertical is shared, so two views of the same rows cannot drift apart. Horizontal
        // stays per pane: everything wraps, so there is nothing to scroll sideways anyway.
        let own_vadjustment = new.scroller.vadjustment();
        swap_vadjustment(&new.scroller, &old.scroller.vadjustment());
        // One height for both title rows: the editor's carries Stop Comparing and would stand
        // taller, starting its column, and every row in it, that much lower. The group lives as
        // long as the rows do.
        let titles = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
        titles.add_widget(&old.header);
        titles.add_widget(&new.header);
        // And one minimum width for both columns, which is what splits them evenly: a paned whose
        // position was never set divides its width in the ratio of the two, on every allocation
        // until a drag sets one. A position set from an idle once the paned was mapped was lost
        // whenever the idle ran before the first allocation, and the columns stayed split by
        // their own minimums, the editor's the wider for its Stop Comparing.
        let columns = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        columns.add_widget(&old.root);
        columns.add_widget(&new.root);

        // Beside Stop Comparing on the editor's title row, or at the end of the right one's. A
        // click leaves the keyboard in the text.
        let unfold = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text("Show All Unchanged Lines")
            .css_classes(["flat"])
            .focus_on_click(false)
            .build();
        let host = match editable {
            Some(Side::Old) => &old.header,
            _ => &new.header,
        };
        host.insert_child_after(&unfold, host.first_child().as_ref());

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&old.root));
        paned.set_end_child(Some(&new.root));
        paned.set_resize_start_child(true);
        paned.set_shrink_start_child(false);
        paned.set_resize_end_child(true);
        paned.set_shrink_end_child(false);

        let this = Rc::new_cyclic(|weak| Compare {
            weak: weak.clone(),
            panes: [old, new],
            editable,
            hunk_buttons: RefCell::new(takes),
            paned,
            lines: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            starts: RefCell::new([Vec::new(), Vec::new()]),
            hidden: RefCell::new(Vec::new()),
            opened: RefCell::new(HashSet::new()),
            unfold,
            overlays: RefCell::new(Vec::new()),
            grid: RefCell::new(Grid::default()),
            pending: RefCell::new(None),
            settling: Cell::new(0),
            own_vadjustment,
            bottoms: Default::default(),
            keep: Cell::new(Some(Keep::FirstHunk)),
            handlers: RefCell::new(Vec::new()),
            offered: RefCell::new(Vec::new()),
            laid: RefCell::new(None),
        });

        // Weak throughout: every handler below is connected to something the comparison owns or
        // to the process-wide style manager, and a strong capture is a cycle either way.
        let weak = this.weak.clone();
        let connect = |object: glib::Object, id: glib::SignalHandlerId| {
            this.handlers.borrow_mut().push((object, id));
        };
        for pane in &this.panes {
            // `view.color()` only resolves the theme foreground once the widget is mapped, and
            // the layout only measures true once it has a font.
            let w = weak.clone();
            pane.view.connect_map(move |_| {
                if let Some(c) = w.upgrade() {
                    c.restyle();
                    c.schedule_relayout();
                }
            });
            // Everything wraps, so a pane's width is its horizontal page size: a paned drag or a
            // window resize lands here and re-measures the rows, the line at the top staying.
            let w = weak.clone();
            let hadj = pane.scroller.hadjustment();
            // GTK notifies on every allocation, the same width or not.
            let width = Cell::new(hadj.page_size());
            let id = hadj.connect_page_size_notify(move |hadj| {
                let Some(c) = w.upgrade() else { return };
                if width.replace(hadj.page_size()) != hadj.page_size() {
                    c.rewrapped();
                }
                c.schedule_relayout();
            });
            connect(hadj.upcast(), id);
        }
        // GTK lays lines out lazily and the total height moves as it reaches them, as it does
        // on a font change; the rows are re-measured each time it settles.
        let w = weak.clone();
        let vadj = this.panes[0].scroller.vadjustment();
        let id = vadj.connect_upper_notify(move |_| {
            if let Some(c) = w.upgrade() {
                c.schedule_relayout();
            }
        });
        connect(vadj.clone().upcast(), id);
        // A scroll held by `Compare::open_run`, or a line kept by `set_side`, goes back to where
        // it is held.
        let w = weak.clone();
        let id = vadj.connect_value_changed(move |adj| {
            let Some(c) = w.upgrade() else { return };
            match c.keep.get() {
                Some(Keep::Scroll(value)) if adj.value() != value => adj.set_value(value),
                Some(Keep::Line(side, n, at)) => c.hold_line(side, n, at),
                _ => {}
            }
        });
        connect(vadj.upcast(), id);
        let w = weak.clone();
        let style = adw::StyleManager::default();
        let id = style.connect_dark_notify(move |_| {
            if let Some(c) = w.upgrade() {
                c.restyle();
            }
        });
        connect(style.upcast(), id);
        // Text typed ahead of a line's padding goes back under it on the keystroke itself: above
        // 16 KB the editor refreshes the comparison only on its debounce, and the line would be
        // laid out bare until then.
        if let Some(mine) = editable {
            let buffer = this.pane(mine).buffer.clone();
            // Focus mode's line fade on the other column is measured from the lines facing the
            // editor's carets, so stepping through the changes keeps the two columns' focus level,
            // and drawn again as those carets move: its own caret nobody moves.
            if let Some(theirs) = this
                .pane(mine.other())
                .view
                .downcast_ref::<crate::multicaret::View>()
            {
                let w = weak.clone();
                theirs.fade_from(move || w.upgrade()?.facing(mine));
                let theirs = theirs.downgrade();
                let id = buffer.connect_mark_set(move |buffer, _, mark| {
                    let caret = [buffer.get_insert(), buffer.selection_bound()].contains(mark);
                    if let Some(theirs) = theirs.upgrade().filter(|t| caret && t.fade_shown()) {
                        theirs.queue_draw();
                    }
                });
                connect(buffer.clone().upcast(), id);
            }
            let id = buffer.connect_changed(reclaim);
            connect(buffer.clone().upcast(), id);
            // And the lines an edit touches are marked as GTK's to lay out again, before the
            // buffer's `changed` lays the comparison over them: see `pad::measure`.
            let id = buffer.connect_insert_text(|buffer, at, _| unmeasured(buffer, at, at));
            connect(buffer.clone().upcast(), id);
            let id = buffer.connect_delete_range(unmeasured);
            connect(buffer.upcast(), id);
        }

        let w = weak.clone();
        this.unfold.connect_toggled(move |_| {
            if let Some(c) = w.upgrade() {
                c.toggle_all();
            }
        });

        for pane in &this.panes {
            *pane.pool.owner.borrow_mut() = this.weak.clone();
        }
        this.lay(true);
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.paned.upcast_ref()
    }

    fn pane(&self, side: Side) -> &Pane {
        &self.panes[side.idx()]
    }

    fn text(&self, side: Side) -> String {
        let buffer = &self.pane(side).buffer;
        let (s, e) = buffer.bounds();
        buffer.text(&s, &e, true).to_string()
    }

    /// Replace the text of a pane that is not the user's, and lay the diff over both again. The
    /// same text again is nothing to do: a refresh lands on every save.
    pub fn set_side(&self, side: Side, text: &str) {
        debug_assert_ne!(
            self.editable,
            Some(side),
            "the editor's buffer is read, never set"
        );
        let text = normalise(text);
        if self.text(side) == text {
            return;
        }
        // The rows are numbered anew under the reader, so their place is kept as a line of the
        // side that stays.
        if !matches!(self.keep.get(), Some(Keep::FirstHunk)) {
            self.keep.set(self.top_line(side.other()));
        }
        let pane = self.pane(side);
        pane.buffer.set_text(&text);
        editor::style_companion(pane.flavour, &pane.buffer, &pane.view);
        self.refresh();
    }

    /// The editor's caret line, and how far below the top of the view its row starts, while that
    /// row starts on screen.
    fn caret_line(&self) -> Option<Keep> {
        let mine = self.editable?;
        let buffer = &self.pane(mine).buffer;
        let n = buffer.iter_at_mark(&buffer.get_insert()).line() as usize + 1;
        let (lines, rows, grid) = (self.lines.borrow(), self.rows.borrow(), self.grid.borrow());
        let row = rows
            .iter()
            .position(|row| mine.of(row).and_then(|i| mine.number(&lines[i])) == Some(n))?;
        let seen = self.panes[0].view.visible_rect();
        let at = grid.tops.get(row)? - seen.y();
        (0..seen.height())
            .contains(&at)
            .then_some(Keep::Line(mine, n, at))
    }

    /// A new width rewraps every line, and each view keeps its own top line in place as GTK lays
    /// them out again, both on the one scroll they share: the line at the top is held instead,
    /// until the rows have settled at the new width.
    fn rewrapped(&self) {
        if self.keep.get().is_none() {
            self.hold(self.top_line(self.editable.unwrap_or(Side::New)));
            self.settling.set(SETTLE);
        }
    }

    /// Keep `keep` until the rows are laid, a [`Keep::Line`] held again after each layout GTK
    /// makes meanwhile, before it is painted: the lines GTK lays out in the frame itself move a
    /// kept line without moving the scroll.
    fn hold(&self, keep: Option<Keep>) {
        let held = matches!(self.keep.replace(keep), Some(Keep::Line(..)));
        let clock = self.panes[0].view.frame_clock();
        let (false, Some(Keep::Line(..)), Some(clock)) = (held, keep, clock) else {
            return;
        };
        let id: Rc<Cell<Option<glib::SignalHandlerId>>> = Rc::default();
        let (w, own) = (self.weak.clone(), id.clone());
        id.set(Some(clock.connect_layout(
            move |clock| match w.upgrade().map(|c| (c.keep.get(), c)) {
                Some((Some(Keep::Line(side, n, at)), c)) => c.hold_line(side, n, at),
                _ => {
                    if let Some(id) = own.take() {
                        clock.disconnect(id);
                    }
                }
            },
        )));
    }

    /// Where the reader is, as a line of `side`: the one in the row at the top of the view, or in
    /// the nearest row above it that has one, and how far below the top of the view its row starts.
    fn top_line(&self, side: Side) -> Option<Keep> {
        let (lines, rows, grid) = (self.lines.borrow(), self.rows.borrow(), self.grid.borrow());
        let seen = self.panes[0].view.visible_rect().y();
        let top = grid.tops.partition_point(|&y| y <= seen).max(1);
        let (row, n) = rows[..top.min(rows.len())]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(r, row)| Some((r, side.number(&lines[side.of(row)?])?)))?;
        Some(Keep::Line(side, n, grid.tops[row] - seen))
    }

    /// Scroll so line `n` of `side` starts `at` pixels below the top of the view where GTK has it
    /// now, which is where it is drawn: what keeps a [`Keep::Line`] on screen while the rows are
    /// laid. GTK lays out the lines that opened, hid or grew above it a few at a time, and keeps
    /// each view's own top line in place as it does, both on the one scroll they share; a scroll
    /// held where it was showed the lines above for as long as that took.
    fn hold_line(&self, side: Side, n: usize, at: i32) {
        let pane = self.pane(side);
        let Some(line) = pane.buffer.iter_at_line(n as i32 - 1) else {
            return;
        };
        let y = pane.view.line_yrange(&line).0;
        let adj = self.panes[0].scroller.vadjustment();
        let value = adj.value() + f64::from(y - pane.view.visible_rect().y() - at);
        if value != adj.value() {
            adj.set_value(value);
        }
    }

    /// Focus mode's line fade on the column beside the editor, which comes and goes with the
    /// editor's own.
    pub fn set_fade(&self, on: bool) {
        let theirs = self.editable.map(|mine| &self.pane(mine.other()).view);
        if let Some(theirs) = theirs.and_then(|v| v.downcast_ref::<crate::multicaret::View>()) {
            theirs.set_fade(on);
        }
    }

    /// The lines of the column beside the editor that face the editor's carets, 0-based: what
    /// focus mode's line fade is measured from there.
    fn facing(&self, mine: Side) -> Option<RangeInclusive<i32>> {
        let view = self
            .pane(mine)
            .view
            .downcast_ref::<crate::multicaret::View>()?;
        let span = crate::fade::span(view);
        let span = *span.start() as usize + 1..=*span.end() as usize + 1;
        let facing = diff::facing(&self.lines.borrow(), &self.rows.borrow(), mine, &span);
        Some(*facing.start() as i32 - 1..=*facing.end() as i32 - 1)
    }

    /// Re-read both buffers and lay the diff over them: the tints, the emphasis, the hidden runs
    /// and the buttons. Nothing in either buffer's text is touched.
    pub fn refresh(&self) {
        self.lay(false);
    }

    /// Put `entries` on both panes' context menus, there while the pane has a selection: each an
    /// action `diff.<name>`, its label, and what it does, which is handed that pane's side, the
    /// lines of its text the selection covers (1-based, inclusive) and both texts as they stand.
    /// Once per comparison: [`Compare::leave`] takes them off again.
    ///
    /// The entries join whatever menu the view had, which on the editor is the spell checker's
    /// suggestions, and hide rather than grey out: `hidden-when` follows the action, and the
    /// action follows the selection, so a menu opened from the keyboard is right too.
    pub fn offer(&self, entries: Vec<(&str, &str, OnLines)>) {
        if !self.offered.borrow().is_empty() {
            return;
        }
        for side in [Side::Old, Side::New] {
            let pane = self.pane(side);
            let (group, section) = (gio::SimpleActionGroup::new(), gio::Menu::new());
            let mut actions = Vec::new();
            for (name, label, act) in &entries {
                let action = gio::SimpleAction::new(name, None);
                action.set_enabled(pane.buffer.has_selection());
                let (weak, act) = (self.weak.clone(), act.clone());
                action.connect_activate(move |_, _| {
                    let Some(compare) = weak.upgrade() else {
                        return;
                    };
                    let texts = [compare.text(Side::Old), compare.text(Side::New)];
                    if let Some((from, to)) = compare.pane(side).buffer.selection_bounds() {
                        let starts = line_starts(&texts[side.idx()]);
                        let lines = lines_between(&starts, from.offset(), to.offset());
                        act(side, lines, &texts[0], &texts[1]);
                    }
                });
                group.add_action(&action);
                let item = gio::MenuItem::new(Some(label), Some(&format!("diff.{name}")));
                item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
                section.append_item(&item);
                actions.push(action);
            }
            pane.view.insert_action_group("diff", Some(&group));
            let id = pane.buffer.connect_has_selection_notify(move |b| {
                for action in &actions {
                    action.set_enabled(b.has_selection());
                }
            });
            self.handlers
                .borrow_mut()
                .push((pane.buffer.clone().upcast(), id));

            let menu = gio::Menu::new();
            menu.append_section(None, &section);
            let previous = pane.view.extra_menu();
            if let Some(previous) = &previous {
                menu.append_section(None, previous);
            }
            pane.view.set_extra_menu(Some(&menu));
            self.offered.borrow_mut().push((menu, previous));
        }
    }

    /// Put `buttons` on each hunk, on the pane beside the editor: each a label, its tooltip, and
    /// what it does, which is handed the hunk's lines as [`Compare::offer`]'s entries are handed a
    /// selection's. Asked once, before a hunk has had buttons: a hunk's are made with its first.
    pub fn offer_hunks(&self, buttons: Vec<(&'static str, &'static str, OnLines)>) {
        *self.hunk_buttons.borrow_mut() = buttons
            .into_iter()
            .map(|(label, tip, act)| {
                let on: OnHunk = Rc::new(move |c: &Compare, hunk| c.act_on_hunk(&hunk, &act));
                (label, tip, on)
            })
            .collect();
        self.refresh();
    }

    /// Run `act` over the lines of `hunk`, as over a selection of exactly them.
    fn act_on_hunk(&self, hunk: &Range<usize>, act: &OnLines) {
        let Some(mine) = self.editable else {
            return;
        };
        let picked = hunk_lines(&self.lines.borrow(), &self.rows.borrow(), hunk, mine);
        if let Some((side, lines)) = picked {
            act(side, lines, &self.text(Side::Old), &self.text(Side::New));
        }
    }

    /// [`Compare::refresh`], and with `opening`, the one [`Compare::new`] makes: that one also
    /// puts the editor's caret on the first change, so the comparison opens on what changed
    /// with every unchanged run folded, the one the caret was in included.
    fn lay(&self, opening: bool) {
        let (old, new) = (self.text(Side::Old), self.text(Side::New));
        let lines = diff::lines(&old, &new);
        let rows = diff::align(&lines);
        let starts = [line_starts(&old), line_starts(&new)];
        for side in [Side::Old, Side::New] {
            self.clear_marks(side);
        }

        for side in [Side::Old, Side::New] {
            let buffer = &self.pane(side).buffer;
            let st = &starts[side.idx()];
            for row in &rows {
                let Some(line) = side.of(row).map(|i| &lines[i]) else {
                    continue;
                };
                let Some((row_tag, emph_tag)) = tags(side, line.op) else {
                    continue;
                };
                let n = side.number(line).unwrap_or(1);
                let (from, to) = (st[n - 1], st[n]);
                buffer.apply_tag_by_name(
                    row_tag,
                    &buffer.iter_at_offset(from),
                    &buffer.iter_at_offset(to),
                );
                // The diff's ranges are byte offsets into the line; the buffer counts characters.
                for range in &line.emphasis {
                    let at = |byte: usize| {
                        buffer.iter_at_offset(from + line.text[..byte].chars().count() as i32)
                    };
                    buffer.apply_tag_by_name(emph_tag, &at(range.start), &at(range.end));
                }
            }
        }

        // The span of rows `range` on `side`, in characters: every row in a gap has both sides.
        let span = |side: Side, range: &Range<usize>| -> (i32, i32) {
            let st = &starts[side.idx()];
            let number = |r: usize| {
                side.of(&rows[r])
                    .and_then(|i| side.number(&lines[i]))
                    .unwrap_or(1)
            };
            (st[number(range.start) - 1], st[number(range.end - 1)])
        };
        let keyed = self.editable.map(Side::other).unwrap_or(Side::Old);
        if opening
            && let Some(side) = self.editable
            && let Some(at) = first_change(&lines, &rows, &starts[side.idx()], side)
        {
            let buffer = &self.pane(side).buffer;
            buffer.place_cursor(&buffer.iter_at_offset(at));
        }
        let caret = self.editable.map(|side| {
            let buffer = &self.pane(side).buffer;
            (side, buffer.iter_at_mark(&buffer.get_insert()).offset())
        });
        let mut hidden = Vec::new();
        let gaps = diff::gaps(&lines, &rows, CONTEXT);
        self.unfold.set_sensitive(!gaps.is_empty());
        let all = self.unfold.is_active();
        for gap in gaps {
            // Show All Unchanged Lines is down.
            if all {
                continue;
            }
            let number = |r: usize| {
                keyed
                    .of(&rows[r])
                    .and_then(|i| keyed.number(&lines[i]))
                    .unwrap_or(0)
            };
            let (key, last) = (number(gap.start), number(gap.end - 1));
            // What the user opened stays open: the part of it on either side of a line they
            // changed since, and whatever it has since merged with.
            if self
                .opened
                .borrow()
                .iter()
                .any(|k| (key..=last).contains(k))
            {
                continue;
            }
            // Nothing hides under the caret: a run that just became equal around it stays open
            // until the caret has moved on.
            if let Some((side, at)) = caret {
                let (from, to) = span(side, &gap);
                if at >= from && at < to {
                    continue;
                }
            }
            for side in [Side::Old, Side::New] {
                let buffer = &self.pane(side).buffer;
                let (from, to) = span(side, &gap);
                buffer.apply_tag_by_name(
                    TAG_GAP,
                    &buffer.iter_at_offset(from),
                    &buffer.iter_at_offset(to),
                );
            }
            hidden.push((gap, key));
        }

        for pane in &self.panes {
            pane.pool.unclaim();
        }
        let mut overlays = Vec::new();
        if !self.hunk_buttons.borrow().is_empty()
            && let Some(mine) = self.editable
        {
            let (theirs, pane) = (mine.other(), self.pane(mine.other()));
            for hunk in diff::hunks(&lines, &rows) {
                let row = hunk.start;
                let widget = pane.pool.claim(&pane.view, Role::Hunk(hunk));
                overlays.push((theirs, widget, Anchor::Hunk(row)));
            }
        }
        for (gap, key) in &hidden {
            for side in [Side::Old, Side::New] {
                let pane = self.pane(side);
                let role = Role::Gap {
                    key: *key,
                    rows: gap.len(),
                };
                let widget = pane.pool.claim(&pane.view, role);
                overlays.push((side, widget, Anchor::Gap(gap.start)));
            }
        }
        for pane in &self.panes {
            pane.pool.hide_unclaimed();
        }

        *self.lines.borrow_mut() = lines;
        *self.rows.borrow_mut() = rows;
        *self.starts.borrow_mut() = starts;
        *self.hidden.borrow_mut() = hidden;
        *self.overlays.borrow_mut() = overlays;
        self.settling.set(SETTLE);
        self.relayout();
        if let Some(laid) = self.laid.borrow().as_ref() {
            laid();
        }
    }

    /// Run `f` whenever the rows have been laid again, and once now: the hidden runs have just
    /// moved, and anything drawn per line — the editor's end-of-line diagnostics — has to be laid
    /// again over the lines that are left. One at a time; asking again replaces it.
    pub fn on_laid(&self, f: impl Fn() + 'static) {
        f();
        *self.laid.borrow_mut() = Some(Box::new(f));
    }

    /// Put the first hunk at [`FIRST_HUNK_AT`] of the page, once.
    fn reveal_first_hunk(&self) {
        let row = {
            let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
            diff::hunks(&lines, &rows).first().map(|hunk| hunk.start)
        };
        // Nothing has changed yet — an untouched buffer against its own index side. The next
        // relayout that finds a difference is the one that opens on it.
        if let Some(row) = row {
            let page = self.panes[0].scroller.vadjustment().page_size();
            self.reveal(row, FIRST_HUNK_AT * page);
        }
    }

    /// Put row `row` `at` pixels below the top of the view, from the rows the last relayout
    /// measured, and let the view go. Both panes share the vertical adjustment, so setting it
    /// scrolls both.
    ///
    /// The grid rather than GTK's own figures, and not before the relayout: GTK lays lines out
    /// lazily and the padding just laid is not in its figures yet, so a `scroll_to_mark` made as
    /// the comparison opened landed wherever the estimates put the line, which in a long file
    /// with its unchanged runs folded away was nowhere near it.
    fn reveal(&self, row: usize, at: f64) {
        let Some(top) = self.grid.borrow().tops.get(row).copied() else {
            return;
        };
        self.keep.set(None);
        // `visible_rect` is in buffer coordinates and the adjustment is not — the top margin
        // lies between them — so the scroll moves by the distance from what is on screen now.
        let (adj, seen) = (
            self.panes[0].scroller.vadjustment(),
            self.panes[0].view.visible_rect(),
        );
        adj.set_value(adj.value() + f64::from(top - seen.y()) - at);
    }

    /// Open the hidden run keyed `key`, as its button does, with the scroll held where it was
    /// until the rows are laid: the rows above stay, and the run opens downwards from the button's
    /// row. Left to GTK, each view keeps its own top line in place as the lines above it grow,
    /// both on the one scroll they share, so a run opened at the top of the view scrolled it by
    /// twice its height, and by twice the height laid so far while GTK caught up.
    fn open_run(&self, key: usize) {
        let value = self.panes[0].scroller.vadjustment().value();
        self.keep.set(Some(Keep::Scroll(value)));
        self.open_lines(key);
        self.refresh();
    }

    /// Note every line of the hidden run keyed `key` as one the user asked to see: the run's rows
    /// are unchanged lines, so its lines on the side not typed into count up from the key.
    fn open_lines(&self, key: usize) {
        let rows = self
            .hidden
            .borrow()
            .iter()
            .find(|(_, k)| *k == key)
            .map(|(gap, _)| gap.len());
        self.opened
            .borrow_mut()
            .extend(key..key + rows.unwrap_or(1));
    }

    /// Every run opened, as Show All Unchanged Lines goes down, or every one hidden again as it
    /// comes up, those opened one by one too, but for the one holding the caret. The caret's line
    /// stays where it is on screen; with the caret off screen, the line at the top of the view
    /// stays there, as a re-read side's does ([`Compare::set_side`]).
    fn toggle_all(&self) {
        if !matches!(self.keep.get(), Some(Keep::FirstHunk)) {
            let top = || self.top_line(self.editable.unwrap_or(Side::New));
            self.hold(self.caret_line().or_else(top));
        }
        if !self.unfold.is_active() {
            self.opened.borrow_mut().clear();
        }
        self.refresh();
    }

    /// Make the editor's side read `text`, as one undo step that rewrites only the stretch between
    /// what the two share at either end, so the caret and the folds outside it stay put. Nothing
    /// without an editor's side.
    pub fn rewrite_mine(&self, text: &str) {
        let Some(mine) = self.editable else {
            return;
        };
        let (start, end, new_end) = differing(&self.text(mine), text);
        let insert: String = text.chars().skip(start).take(new_end - start).collect();
        let buffer = &self.pane(mine).buffer;
        buffer.begin_user_action();
        let (mut a, mut b) = (
            buffer.iter_at_offset(start as i32),
            buffer.iter_at_offset(end as i32),
        );
        buffer.delete(&mut a, &mut b);
        buffer.insert(&mut a, &insert);
        buffer.end_user_action();
        self.refresh();
    }

    /// Take the tints, the emphasis and the hidden runs off `side`, which a refresh lays down
    /// again. The padding stays, and the relayout changes only the rows whose padding moved:
    /// taking it all off made GTK lay every padded row out without it for a frame, which is what
    /// flashed on every keystroke and nudged the scroll range.
    fn clear_marks(&self, side: Side) {
        let buffer = &self.pane(side).buffer;
        let (start, end) = buffer.bounds();
        for tag in [
            TAG_ADDED,
            TAG_REMOVED,
            TAG_ADDED_EMPH,
            TAG_REMOVED_EMPH,
            TAG_GAP,
        ] {
            buffer.remove_tag_by_name(tag, &start, &end);
        }
    }

    /// Everything off both panes: what leaving a comparison does before the panes part.
    pub fn leave(&self) {
        for side in [Side::Old, Side::New] {
            self.clear_marks(side);
            let pane = self.pane(side);
            let (start, end) = pane.buffer.bounds();
            let mut pads = Vec::new();
            pane.buffer.tag_table().foreach(|tag| {
                if is_pad(tag) || tag.name().as_deref() == Some(UNMEASURED) {
                    pads.push(tag.clone());
                }
            });
            for tag in pads {
                pane.buffer.remove_tag(&tag, &start, &end);
            }
            pane.pool.unclaim();
            pane.pool.hide_unclaimed();
            if let Some(view) = pane.view.downcast_ref::<crate::multicaret::View>() {
                view.set_bands(Vec::new());
            }
        }
        self.overlays.borrow_mut().clear();
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        for (object, id) in self.handlers.borrow_mut().drain(..) {
            object.disconnect(id);
        }
        // Each column scrolls on its own again before the companion can go. A GtkTextView that is
        // freed stays connected to its adjustment, so one left on the adjustment the editor keeps
        // was called into after it was gone: a comparison left the moment it opened — a Changes
        // row git had outgrown — crashed the window on the editor's next scroll.
        swap_vadjustment(&self.panes[1].scroller, &self.own_vadjustment);
        // And the editor its page's bottom margin, should it have been the side with no line.
        if let Some(mine) = self.editable {
            self.set_bottom(mine, self.page_bottom(mine), 0);
        }
        for (side, (menu, previous)) in [Side::Old, Side::New].into_iter().zip(self.offered.take())
        {
            let view = &self.pane(side).view;
            view.insert_action_group("diff", None::<&gio::ActionGroup>);
            // Unless a menu of someone else's has replaced it since: a spell checker switched on
            // for the first time mid-comparison.
            if view.extra_menu().as_ref() == Some(menu.upcast_ref()) {
                view.set_extra_menu(previous.as_ref());
            }
        }
    }

    pub fn restyle(&self) {
        for (i, pane) in self.panes.iter().enumerate() {
            if self.editable != Some([Side::Old, Side::New][i]) {
                editor::restyle_companion(pane.flavour, &pane.buffer, &pane.view);
            }
            restyle_tags(&pane.buffer, &pane.view);
        }
    }

    /// Put the editor's page on the companion beside it: the margins, so the first row of each
    /// starts level, the line spacing, and the tab width the Indent Width preference sets. The
    /// heading markers hang in the left margin and are measured in the font, so they are hung
    /// again where the margin moves and, with `refont`, after a font change, which only the
    /// editor's tab hears of. The bottom margin is [`Compare::relayout`]'s.
    pub fn follow_editor(&self, refont: bool) {
        let Some(mine) = self.editable else {
            return;
        };
        let (from, companion) = (&self.pane(mine).view, self.pane(mine.other()));
        let to = &companion.view;
        let rehang = refont || to.left_margin() != from.left_margin();
        to.set_top_margin(from.top_margin());
        to.set_left_margin(from.left_margin());
        to.set_right_margin(from.right_margin());
        to.set_pixels_above_lines(from.pixels_above_lines());
        to.set_pixels_below_lines(from.pixels_below_lines());
        to.set_tab_width(from.tab_width());
        if rehang {
            editor::rehang_companion(companion.flavour, &companion.buffer, to);
        }
    }

    /// The bottom margin the page gives `side`, without the blank a relayout left under a side
    /// with no line: the editor's own, which a companion beside it takes as well. A margin
    /// someone else has set since — the zoom — is the page's whole.
    fn page_bottom(&self, side: Side) -> i32 {
        let side = self.editable.unwrap_or(side);
        let (set, blank) = self.bottoms[side.idx()].get();
        let now = self.pane(side).view.bottom_margin();
        if now == set { now - blank } else { now }
    }

    /// `side`'s bottom margin: the page's, and `blank` pixels more under a side with no line.
    fn set_bottom(&self, side: Side, page: i32, blank: i32) {
        self.pane(side).view.set_bottom_margin(page + blank);
        self.bottoms[side.idx()].set((page + blank, blank));
    }

    fn schedule_relayout(&self) {
        if self.pending.borrow().is_some() {
            return;
        }
        let weak = self.weak.clone();
        let id = glib::idle_add_local_once(move || {
            if let Some(c) = weak.upgrade() {
                *c.pending.borrow_mut() = None;
                c.relayout();
            }
        });
        *self.pending.borrow_mut() = Some(id);
    }

    /// Measure every row on both sides and pad whichever is shorter, then put the buttons where
    /// the rows now are. Idempotent: a row whose padding has not changed is left alone, so a pass
    /// that finds nothing to do invalidates nothing and the layout settles.
    ///
    /// ponytail: every visible row is laid out on every pass, which is a Pango layout per
    /// paragraph — fine at the few hundred rows a note has, and debounced by the editor above
    /// that. Measuring only the rows an edit touched is the upgrade if a long note shows it.
    fn relayout(&self) {
        let views = [&self.panes[0].view, &self.panes[1].view];
        if !views.iter().all(|v| v.is_mapped()) {
            return;
        }
        // Before anything is measured: a keystroke's refresh gets here ahead of the buffer's own
        // `changed` handler.
        if let Some(mine) = self.editable {
            reclaim(&self.pane(mine).buffer);
        }
        self.follow_editor(false);
        let lines = self.lines.borrow();
        let rows = self.rows.borrow();
        let starts = self.starts.borrow();
        let hidden = self.hidden.borrow();
        let is_hidden = |r: usize| hidden.iter().any(|(gap, _)| gap.contains(&r));
        let estimated = Cell::new(false);
        let heights: [Vec<Option<i32>>; 2] = [Side::Old, Side::New].map(|side| {
            let pane = self.pane(side);
            let st = &starts[side.idx()];
            rows.iter()
                .enumerate()
                .map(|(r, row)| {
                    if is_hidden(r) {
                        return None;
                    }
                    let n = side.of(row).and_then(|i| side.number(&lines[i]))?;
                    let (above, below) =
                        carried(&pane.view, &pane.buffer.iter_at_offset(st[n - 1]));
                    let (height, estimate) =
                        measure(&pane.view, &pane.buffer, st[n - 1], st[n], above + below);
                    estimated.set(estimated.get() || estimate);
                    Some(height)
                })
                .collect()
        });
        let mut extra = vec![0; rows.len()];
        for (gap, _) in hidden.iter() {
            extra[gap.start] += GAP_PX;
        }
        // A row is a change where either side's line is: a changed pair, or a line the other
        // side has none for.
        let changed: Vec<bool> = rows
            .iter()
            .map(|row| {
                [Side::Old, Side::New]
                    .iter()
                    .any(|side| side.of(row).is_some_and(|i| lines[i].op != Op::Equal))
            })
            .collect();
        let (pads, tops) = padding(&heights[0], &heights[1], &extra, &changed);
        // A side with no line at all has no paragraph to pad, so the blank that keeps it as tall
        // as the other goes under its text, less the one empty line it shows. Left shorter, its
        // view pulled the scroll the two share back into its own range whenever it was laid out,
        // and a file a commit added could not be scrolled at all.
        let pages = [Side::Old, Side::New].map(|side| self.page_bottom(side));
        for side in [Side::Old, Side::New] {
            let pane = self.pane(side);
            let blank = match pads[side.idx()].rest {
                0 => 0,
                rest => rest - pane.view.line_yrange(&pane.buffer.start_iter()).1,
            };
            self.set_bottom(side, pages[side.idx()], blank.max(0));
        }
        for side in [Side::Old, Side::New] {
            if let Some(view) = self
                .pane(side)
                .view
                .downcast_ref::<crate::multicaret::View>()
            {
                view.set_bands(bands(&heights, &extra, &changed, &tops, side));
            }
        }

        let mut repadded = false;
        for side in [Side::Old, Side::New] {
            let pane = self.pane(side);
            let st = &starts[side.idx()];
            let now = &pads[side.idx()];
            // Every line on this side, hidden ones included, so a row that is hidden now or was
            // the last one before an edit does not keep what it carried then.
            for (r, row) in rows.iter().enumerate() {
                let Some(n) = side.of(row).and_then(|i| side.number(&lines[i])) else {
                    continue;
                };
                repadded |= pad(
                    &pane.view,
                    &pane.buffer,
                    st[n - 1],
                    st[n],
                    now.above[r],
                    now.below[r],
                );
            }
        }

        // Buffer coordinates, which start at the first paragraph — the view's top margin is
        // outside them — and scroll with the text.
        for (side, widget, anchor) in self.overlays.borrow().iter() {
            let view = &self.pane(*side).view;
            let width = view.visible_rect().width();
            let (_, wanted, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
            let (_, height, _, _) = widget.measure(gtk::Orientation::Vertical, -1);
            let (x, y) = match *anchor {
                Anchor::Hunk(row) => (width - wanted - INSET, tops[row]),
                Anchor::Gap(row) => ((width - wanted) / 2, tops[row] + (GAP_PX - height) / 2),
            };
            view.move_overlay(widget, x.max(0), y);
        }
        *self.grid.borrow_mut() = Grid {
            #[cfg(feature = "bench")]
            heights,
            #[cfg(feature = "bench")]
            extra,
            tops,
        };
        // Both views laid out again in the next frame, ahead of painting either. GTK lays a view
        // out from an idle of its own and repaints it once it has, so one column could reach the
        // screen a frame ahead of the other: a keystroke's own line in the editor a frame after
        // the padding that answers it beside it.
        if repadded {
            for view in views {
                view.queue_allocate();
            }
        }
        // GTK had not laid some line out yet: ask again once it has. So too while the view waits
        // to be put somewhere for a pass that moved nothing: padding GTK has not laid out yet is
        // not in where the scroll puts a line, and each view keeps its top line where it was as it
        // catches up.
        let unsettled = estimated.get() || (self.keep.get().is_some() && repadded);
        let exhausted = self.settling.get() == 0;
        if unsettled && !exhausted && self.pending.borrow().is_none() {
            self.settling.set(self.settling.get() - 1);
            let weak = self.weak.clone();
            let id = glib::timeout_add_local_once(SETTLE_AFTER, move || {
                if let Some(c) = weak.upgrade() {
                    *c.pending.borrow_mut() = None;
                    c.relayout();
                }
            });
            *self.pending.borrow_mut() = Some(id);
        }
        // Once every row is measured and laid as the grid has it, or GTK has been asked as often
        // as it will be. After the borrows: moving the scroll runs handlers that may lay the
        // comparison again.
        let keep = self.keep.get().filter(|_| !unsettled || exhausted);
        drop((lines, rows, starts, hidden));
        match keep {
            Some(Keep::FirstHunk) => self.reveal_first_hunk(),
            // Held all along, and the rows above the run have not moved.
            Some(Keep::Scroll(_)) => self.keep.set(None),
            Some(Keep::Line(side, n, at)) => {
                self.keep.set(None);
                let row = {
                    let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
                    let number = |row: &Row| side.number(&lines[side.of(row)?]);
                    rows.iter().position(|row| number(row) == Some(n))
                };
                if let Some(row) = row {
                    self.reveal(row, f64::from(at));
                }
            }
            None => {}
        }
    }

    // --- for the bench and the tests ----------------------------------------------------------

    /// (rows, hunks, hidden runs, buttons) on screen right now.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
        (
            rows.len(),
            diff::hunks(&lines, &rows).len(),
            self.hidden.borrow().len(),
            self.overlays.borrow().len(),
        )
    }

    /// How many pixels lower the right column starts than the left one in the window: 0 is the
    /// claim, and what [`Compare::misaligned`] cannot see, being in buffer coordinates.
    #[cfg(feature = "bench")]
    pub fn skew(&self) -> i32 {
        let top = |side: Side| {
            self.pane(side)
                .scroller
                .compute_point(&self.paned, &gtk::graphene::Point::zero())
                .map_or(0.0, |p| p.y())
        };
        (top(Side::New) - top(Side::Old)).round() as i32
    }

    /// Whether the rows are laid and the view is where the comparison was keeping it: what a drill
    /// waits for before it acts on a comparison just opened.
    #[cfg(feature = "bench")]
    pub fn settled(&self) -> bool {
        self.keep.get().is_none() && self.pending.borrow().is_none()
    }

    /// Whether the first hunk's first line is inside its view right now, on the first side that
    /// has a line in it: what a comparison has to open on.
    #[cfg(feature = "bench")]
    pub fn first_hunk_on_screen(&self) -> bool {
        let Some((side, at)) = self.first_hunk_line() else {
            return false;
        };
        let view = &self.pane(side).view;
        let (line, seen) = (
            view.iter_location(&self.pane(side).buffer.iter_at_offset(at)),
            view.visible_rect(),
        );
        line.y() >= seen.y() && line.y() + line.height() <= seen.y() + seen.height()
    }

    /// Where the first hunk starts, in characters, on the first side with a line in it.
    #[cfg(feature = "bench")]
    fn first_hunk_line(&self) -> Option<(Side, i32)> {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let hunk = diff::hunks(&lines, &rows).into_iter().next()?;
        [Side::Old, Side::New].into_iter().find_map(|side| {
            let n = hunk
                .clone()
                .find_map(|r| side.of(&rows[r]).and_then(|i| side.number(&lines[i])))?;
            Some((side, starts[side.idx()][n - 1]))
        })
    }

    /// Where the first hunk's lines and the ones after it start, as GTK lays them out: the row,
    /// then the old and the new side's `y`, `None` where a side has no visible line in the row.
    #[cfg(feature = "bench")]
    pub fn first_hunk_tops(&self) -> Vec<(usize, Option<i32>, Option<i32>)> {
        let hunk = {
            let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
            diff::hunks(&lines, &rows).first().cloned()
        };
        let Some(hunk) = hunk else {
            return Vec::new();
        };
        let end = (hunk.end + 1).min(self.rows.borrow().len());
        let top = |r, side| self.laid(r, side).map(|(_, actual, ..)| actual);
        (hunk.start..end)
            .map(|r| (r, top(r, Side::Old), top(r, Side::New)))
            .collect()
    }

    /// The shared vertical scrollbar, for the bench to read and to move as a reader would.
    #[cfg(feature = "bench")]
    pub fn vadjustment(&self) -> gtk::Adjustment {
        self.panes[0].scroller.vadjustment()
    }

    /// Whether row `r` is in a hidden run right now.
    #[cfg(feature = "bench")]
    pub fn hides_row(&self, r: usize) -> bool {
        self.hidden.borrow().iter().any(|(gap, _)| gap.contains(&r))
    }

    /// Where the first change starts on the editor's side, in characters.
    #[cfg(feature = "bench")]
    pub fn opens_at(&self) -> Option<i32> {
        let side = self.editable?;
        let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
        first_change(&lines, &rows, &self.starts.borrow()[side.idx()], side)
    }

    /// How many rows GTK lays out at a different height than the last relayout meant them to
    /// have, on either side: the number the alignment stands or falls on, and 0 is the claim.
    #[cfg(feature = "bench")]
    pub fn misaligned(&self) -> usize {
        let rows = self.rows.borrow().len();
        if self.grid.borrow().tops.len() != rows {
            return rows;
        }
        let off = |r, side| {
            self.laid(r, side)
                .is_some_and(|(expected, actual, ..)| expected != actual)
        };
        (0..rows)
            .filter(|&r| off(r, Side::Old) || off(r, Side::New))
            .count()
    }

    /// The rows on screen whose two lines GTK draws at different heights right now, and the first
    /// of them spelled out (`row:old_y/new_y`, below the top of the view). `None` while an edit has
    /// not been laid over yet, when the rows say nothing about the text.
    #[cfg(feature = "bench")]
    pub fn uneven(&self) -> Option<(usize, String)> {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let fresh = |side: Side| {
            starts[side.idx()].last().copied() == Some(self.pane(side).buffer.char_count())
        };
        if !fresh(Side::Old) || !fresh(Side::New) {
            return None;
        }
        let height = self.panes[0].view.visible_rect().height();
        let y = |r: usize, side: Side| {
            let n = side.number(&lines[side.of(&rows[r])?])?;
            let pane = self.pane(side);
            let at = pane.buffer.iter_at_offset(starts[side.idx()][n - 1]);
            Some(pane.view.iter_location(&at).y() - pane.view.visible_rect().y())
        };
        let (mut count, mut first) = (0, String::new());
        for r in 0..rows.len() {
            if self.hides_row(r) {
                continue;
            }
            let (Some(old), Some(new)) = (y(r, Side::Old), y(r, Side::New)) else {
                continue;
            };
            if old != new && [old, new].iter().any(|y| (0..height).contains(y)) {
                if count == 0 {
                    first = format!("{r}:{old}/{new}");
                }
                count += 1;
            }
        }
        Some((count, first))
    }

    /// The first row [`Compare::misaligned`] counts, spelled out: which row and side, what the
    /// relayout expected, what GTK laid out, and the line. For the bench to print.
    #[cfg(feature = "bench")]
    pub fn first_misaligned(&self) -> Option<String> {
        let rows = self.rows.borrow().len();
        (0..rows)
            .flat_map(|r| [Side::Old, Side::New].map(|side| (r, side)))
            .find_map(|(r, side)| {
                let (expected, actual, own, tallest) =
                    self.laid(r, side).filter(|(e, a, ..)| e != a)?;
                let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
                let text = side
                    .of(&rows[r])
                    .map(|i| lines[i].text.clone())
                    .unwrap_or_default();
                Some(format!(
                    "row={r} side={side:?} expected={expected} actual={actual} own={own} tallest={tallest} text={text:?}"
                ))
            })
    }

    /// Row `r`'s line on `side` as laid out: where the last relayout meant it to start, where GTK
    /// put it, its own height and its row's, in buffer pixels. `None` where the side has no line.
    #[cfg(feature = "bench")]
    fn laid(&self, r: usize, side: Side) -> Option<(i32, i32, i32, i32)> {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let grid = self.grid.borrow();
        let own = grid.heights[side.idx()].get(r).copied().flatten()?;
        let n = side.of(&rows[r]).and_then(|i| side.number(&lines[i]))?;
        let tallest = grid.heights[0][r]
            .unwrap_or(0)
            .max(grid.heights[1][r].unwrap_or(0))
            + grid.extra[r];
        let pane = self.pane(side);
        let expected = grid.tops[r] + pane.view.pixels_above_lines();
        let iter = pane.buffer.iter_at_offset(starts[side.idx()][n - 1]);
        Some((expected, pane.view.iter_location(&iter).y(), own, tallest))
    }

    /// What the `Take` (or, with `keep_own`, the `Both`) button on the `i`th hunk does.
    #[cfg(feature = "bench")]
    pub fn take_hunk(&self, i: usize, keep_own: bool) {
        let hunk = {
            let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
            diff::hunks(&lines, &rows).get(i).cloned()
        };
        if let Some(hunk) = hunk {
            self.take(hunk, keep_own);
        }
    }

    /// What the button on the `i`th hidden run does.
    #[cfg(feature = "bench")]
    pub fn open_gap(&self, i: usize) {
        let key = self.hidden.borrow().get(i).map(|(_, key)| *key);
        if let Some(key) = key {
            self.open_run(key);
        }
    }

    /// The same, for the run hiding `offset` in the editable pane: `true` when one was opened.
    ///
    /// What a jump into the run does, and Go to Line's preview, which moves no caret for
    /// [`Compare::lay`] to keep the run open around: taking the tag off that side's buffer alone
    /// would show the lines under the other side's "unchanged lines" button.
    pub fn open_hiding(&self, offset: i32) -> bool {
        let Some(side) = self.editable else {
            return false;
        };
        let key = {
            let (lines, rows, starts, hidden) = (
                self.lines.borrow(),
                self.rows.borrow(),
                self.starts.borrow(),
                self.hidden.borrow(),
            );
            let st = &starts[side.idx()];
            let number = |r: usize| {
                side.of(&rows[r])
                    .and_then(|i| side.number(&lines[i]))
                    .unwrap_or(1)
            };
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
        self.open_lines(key);
        self.refresh();
        true
    }

    /// Put the other side's lines of `hunk` into the editable buffer: in place of its own lines,
    /// or after them with `keep_own`. One undo step, and one edit like any the user makes, so
    /// the host's own change handling runs and lays the diff over the result.
    fn take(&self, hunk: Range<usize>, keep_own: bool) {
        let Some(mine) = self.editable else {
            return;
        };
        let theirs = mine.other();
        // Everything is worked out before the buffer is touched: the edit re-enters `refresh`
        // through the host's change handler, and that must find nothing borrowed.
        let (text, from, to, unterminated) = {
            let (lines, rows, starts) = (
                self.lines.borrow(),
                self.rows.borrow(),
                self.starts.borrow(),
            );
            let text: String = rows[hunk.clone()]
                .iter()
                .filter_map(|r| theirs.of(r))
                .map(|i| format!("{}\n", lines[i].text))
                .collect();
            let st = &starts[mine.idx()];
            let numbers: Vec<usize> = rows[hunk.clone()]
                .iter()
                .filter_map(|r| mine.of(r).and_then(|i| mine.number(&lines[i])))
                .collect();
            let (from, to) = match (numbers.first(), numbers.last()) {
                (Some(&first), Some(&last)) => (st[first - 1], st[last]),
                // Nothing of ours in the hunk: theirs goes in where our next line begins.
                _ => {
                    let next = rows[hunk.end..]
                        .iter()
                        .find_map(|r| mine.of(r).and_then(|i| mine.number(&lines[i])));
                    let at = next.map_or(*st.last().unwrap_or(&0), |n| st[n - 1]);
                    (at, at)
                }
            };
            let end = *st.last().unwrap_or(&0);
            (
                text,
                from,
                to,
                to == end && !self.text(mine).ends_with('\n'),
            )
        };
        // A last line with no newline: what goes in after it needs one first, and what replaces
        // it should not bring one the file never had.
        let mut text = text;
        if unterminated {
            if keep_own {
                text.insert(0, '\n');
            }
            text.pop();
        }
        let buffer = &self.pane(mine).buffer;
        buffer.begin_user_action();
        let (mut a, mut b) = (buffer.iter_at_offset(from), buffer.iter_at_offset(to));
        if keep_own {
            a = b;
        } else {
            buffer.delete(&mut a, &mut b);
        }
        buffer.insert(&mut a, &text);
        buffer.end_user_action();
        self.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_texts_differ_between_what_they_share_at_either_end() {
        assert_eq!(differing("a\nB\nc\n", "a\nb\nc\n"), (2, 3, 3));
        assert_eq!(differing("ä\nb\nadded\n", "ä\nb\n"), (4, 10, 4));
        assert_eq!(
            differing("aa", "aaa"),
            (2, 2, 3),
            "the shared end never overlaps the start"
        );
        assert_eq!(differing("same", "same"), (4, 4, 4));
    }

    /// U+2029 is an ordinary character to the diff and a line break to `GtkTextBuffer`, so a
    /// line number and a buffer line stop agreeing the moment a note carries one. Every line is
    /// addressed by character offset instead.
    #[test]
    fn a_line_is_found_by_counting_characters_not_by_its_number() {
        assert_eq!(
            line_starts("alpha\u{2029}one\nbravo\n"),
            vec![0, 10, 16, 16],
            "line 2 starts 10 characters in, though the buffer calls it line 3"
        );
        assert_eq!(line_starts("a\nb"), vec![0, 2, 3]);
        assert_eq!(
            line_starts(""),
            vec![0, 0],
            "an empty text still has an end"
        );
        // `similar` ends a line at a lone `\r` too, and at `\r\n` once.
        assert_eq!(line_starts("a\rb\r\nc\n"), vec![0, 2, 5, 7, 7]);
    }

    #[test]
    fn a_selection_covers_the_lines_it_has_characters_in() {
        let starts = line_starts("one\ntwo\nthree\n");
        assert_eq!(lines_between(&starts, 5, 6), 2..=2, "inside two");
        assert_eq!(lines_between(&starts, 0, 8), 1..=2, "to the start of three");
        assert_eq!(lines_between(&starts, 2, 9), 1..=3);
        assert_eq!(lines_between(&starts, 8, 14), 3..=3, "to the end");
    }

    #[test]
    fn a_comparison_opens_on_the_first_change_or_the_line_after_a_deletion() {
        let at = |old: &str, new: &str| {
            let lines = diff::lines(old, new);
            first_change(&lines, &diff::align(&lines), &line_starts(new), Side::New)
        };
        assert_eq!(at("a\nb\nc\n", "a\nB\nc\n"), Some(2), "line 2");
        assert_eq!(at("a\nb\nc\n", "a\nc\n"), Some(2), "b is gone, so c");
        assert_eq!(at("a\nb\n", "a\n"), Some(2), "gone at the end, so the end");
        assert_eq!(at("a\n", "a\n"), None);
    }

    #[test]
    fn a_hunk_is_its_lines_on_the_editors_side_or_the_ones_it_took_out() {
        let staged = |old: &str, new: &str| {
            let lines = diff::lines(old, new);
            let rows = diff::align(&lines);
            let hunk = diff::hunks(&lines, &rows).remove(0);
            let (side, picked) = hunk_lines(&lines, &rows, &hunk, Side::New).unwrap();
            (
                side,
                picked.clone(),
                diff::apply_lines(old, new, side, picked),
            )
        };
        let (old, new) = ("a\nb\nc\n", "a\nB\nadded\nc\n");
        assert_eq!(staged(old, new), (Side::New, 2..=3, new.to_string()));
        let (old, new) = ("a\nb\nc\nd\n", "a\nd\n");
        assert_eq!(
            staged(old, new),
            (Side::Old, 2..=3, new.to_string()),
            "nothing of the hunk is left on the editor's side"
        );
    }
}
