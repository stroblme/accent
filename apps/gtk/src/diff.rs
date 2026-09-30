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

const TAG_ADDED: &str = "diff-added";
const TAG_REMOVED: &str = "diff-removed";
const TAG_ADDED_EMPH: &str = "diff-added-emph";
const TAG_REMOVED_EMPH: &str = "diff-removed-emph";
/// The runs of unchanged lines a changes-only view hides. Its own tag rather than `fold.rs`'s,
/// so the editor's fold bookkeeping never mistakes a hidden run for a block it folded.
pub(crate) const TAG_GAP: &str = "diff-gap";
/// The blank space above a paragraph, and below the last, that keeps the two columns level: one
/// tag per pixel count, named this plus the count, so a paragraph's padding can be read back off
/// the buffer. See [`carried`].
const PAD_ABOVE: &str = "diff-pad-above-";
const PAD_BELOW: &str = "diff-pad-below-";
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
    pub header: gtk::Widget,
    pub view: sourceview5::View,
    pub buffer: sourceview5::Buffer,
    pub scroller: gtk::ScrolledWindow,
    pub flavour: Flavour,
    /// The buttons laid over this view. Handed in rather than made here because they outlive
    /// a comparison on a view that does: see [`Pool`].
    pub pool: Rc<Pool>,
}

/// A pane's title bar, with room for one control beside it.
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
        header: header.upcast(),
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

/// What one overlaid button is for right now.
#[derive(Clone)]
enum Role {
    /// The Take / Keep Both pair of the hunk over these rows.
    Hunk(Range<usize>),
    /// Opens the hidden run keyed `key`, `rows` long.
    Gap { key: usize, rows: usize },
}

struct Slot {
    widget: gtk::Widget,
    /// The gap button itself, to relabel; a hunk row has nothing to relabel.
    label: Option<gtk::Button>,
    role: Rc<RefCell<Role>>,
    /// Whether the refresh under way has handed this slot out.
    claimed: Cell<bool>,
}

/// The buttons laid over one view, kept for reuse.
///
/// GTK 4.22 has no public way to take an overlay back off a text view — `gtk_text_view_remove`
/// knows the anchored children and the four border children, and warns for anything else — so
/// a button that is not needed is hidden, and the next refresh picks it up again. The pool is
/// owned by the pane and not by the comparison, because the editor's view outlives every
/// comparison it hosts and the buttons parented to it have to as well.
///
/// A refresh hands the buttons out again in order, [`Pool::unclaim`] to [`Pool::hide_unclaimed`],
/// so the button a hunk or a run had is the one it gets back, and one still wanted is never
/// hidden in between: a keystroke re-diffs, and must not take every button off and put it back.
#[derive(Default)]
pub struct Pool {
    /// The comparison the buttons act on right now.
    owner: RefCell<Weak<Compare>>,
    slots: RefCell<Vec<Slot>>,
}

impl Pool {
    /// A shown button for `role`: the first one of the same kind this refresh has not handed out
    /// yet, or a new one laid over `view`.
    fn claim(self: &Rc<Self>, view: &sourceview5::View, role: Role) -> gtk::Widget {
        let same_kind = |slot: &Slot| {
            matches!(
                (&*slot.role.borrow(), &role),
                (Role::Hunk(_), Role::Hunk(_)) | (Role::Gap { .. }, Role::Gap { .. })
            )
        };
        let mut slots = self.slots.borrow_mut();
        let at = match slots
            .iter()
            .position(|slot| !slot.claimed.get() && same_kind(slot))
        {
            Some(at) => at,
            None => {
                slots.push(self.build(view, &role));
                slots.len() - 1
            }
        };
        let slot = &slots[at];
        if let (Some(button), Role::Gap { rows, .. }) = (&slot.label, &role) {
            button.set_label(&format!("⋯ {rows} unchanged lines"));
        }
        *slot.role.borrow_mut() = role;
        slot.claimed.set(true);
        slot.widget.set_visible(true);
        slot.widget.clone()
    }

    /// Every button up for claiming again. None is hidden yet.
    fn unclaim(&self) {
        for slot in self.slots.borrow().iter() {
            slot.claimed.set(false);
        }
    }

    /// Hide every button nothing has claimed since [`Pool::unclaim`].
    fn hide_unclaimed(&self) {
        for slot in self.slots.borrow().iter() {
            if !slot.claimed.get() {
                slot.widget.set_visible(false);
            }
        }
    }

    fn build(self: &Rc<Self>, view: &sourceview5::View, role: &Role) -> Slot {
        let role = Rc::new(RefCell::new(role.clone()));
        let (widget, label) = match &*role.borrow() {
            Role::Gap { .. } => {
                let button = gtk::Button::new();
                button.add_css_class("flat");
                button.add_css_class("caption");
                button.set_tooltip_text(Some("Show these lines"));
                button.connect_clicked(self.act(&role, |compare, role| {
                    if let Role::Gap { key, .. } = role {
                        compare.open_run(key);
                    }
                }));
                (button.clone().upcast(), Some(button))
            }
            Role::Hunk(_) => {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                row.add_css_class("linked");
                row.add_css_class("osd");
                for (label, tip, keep_own) in [
                    ("Take", "Replace this hunk in Mine with Theirs", false),
                    ("Both", "Keep both versions of this hunk", true),
                ] {
                    let button = gtk::Button::with_label(label);
                    button.add_css_class("caption");
                    button.set_tooltip_text(Some(tip));
                    button.connect_clicked(self.act(&role, move |compare, role| {
                        if let Role::Hunk(hunk) = role {
                            compare.take(hunk, keep_own);
                        }
                    }));
                    row.append(&button);
                }
                (row.upcast(), None)
            }
        };
        view.add_overlay(&widget, 0, 0);
        Slot {
            widget,
            label,
            role,
            claimed: Cell::new(false),
        }
    }

    /// What a button does: `f`, on the comparison the pool serves right now, with the role the
    /// slot holds right now. Weak on the pool, because the button is a child of the view the
    /// pool's slots hold.
    fn act<F: Fn(&Compare, Role) + 'static>(
        self: &Rc<Self>,
        role: &Rc<RefCell<Role>>,
        f: F,
    ) -> impl Fn(&gtk::Button) + use<F> {
        let (pool, role) = (Rc::downgrade(self), role.clone());
        move |_| {
            let Some(compare) = pool.upgrade().and_then(|p| p.owner.borrow().upgrade()) else {
                return;
            };
            let role = role.borrow().clone();
            f(&compare, role);
        }
    }
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

/// Where the overlaid buttons sit, in rows.
#[derive(Clone, Copy)]
enum Anchor {
    /// At the right end of the row's top: the Take / Keep Both pair of the hunk starting there.
    Hunk(usize),
    /// Centred in the blank space a hidden run left at this row.
    Gap(usize),
}

/// Where the view is kept until the rows are laid: see [`Compare::keep`].
#[derive(Clone, Copy)]
enum Keep {
    /// The first hunk at [`FIRST_HUNK_AT`] of the page, which is where a comparison opens.
    FirstHunk,
    /// The scroll a hidden run was opened at, held there: see [`Compare::open_run`].
    Scroll(f64),
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

/// Blank space above and below each row's line in one column, that keeps it in step with the
/// other column. See [`padding`].
#[derive(Debug, PartialEq, Eq)]
struct Pads {
    above: Vec<i32>,
    below: Vec<i32>,
    /// What no line of this column can carry because it has none at all — a file a commit added,
    /// seen from before it — which is the whole of the other column's height.
    rest: i32,
}

/// From the natural height of every row on each side — `None` where a side has no visible line
/// there — the space each side adds so that row `i` starts at the same height on both, and the
/// top of every row in that shared grid. `extra` is space both sides leave at a row on purpose,
/// which is where a hidden run's button goes.
///
/// A line shorter than its partner leaves the difference under it, so two paired lines start on
/// the same row; a row with no line on a side leaves all of it. The space goes below the side's
/// line before it where that line is a change (`changed`, per row), so a change and the blank
/// that levels it are one tinted block, and above the side's next line otherwise; what is left
/// after the last line goes below it, or to `rest` on a side with no line. This is the whole
/// correctness surface of the alignment, so it is a plain function over plain numbers.
fn padding(
    old: &[Option<i32>],
    new: &[Option<i32>],
    extra: &[i32],
    changed: &[bool],
) -> ([Pads; 2], Vec<i32>) {
    let n = old.len();
    let mut pads = [(); 2].map(|_| Pads {
        above: vec![0; n],
        below: vec![0; n],
        rest: 0,
    });
    let mut tops = Vec::with_capacity(n);
    let (mut y, mut carry, mut last) = (0, [0, 0], [None::<usize>; 2]);
    for r in 0..n {
        tops.push(y);
        let h = old[r].unwrap_or(0).max(new[r].unwrap_or(0)) + extra[r];
        y += h;
        for (s, side) in [old, new].into_iter().enumerate() {
            let Some(own) = side[r] else {
                carry[s] += h;
                continue;
            };
            match last[s] {
                Some(l) if changed[l] => pads[s].below[l] += carry[s],
                _ => pads[s].above[r] = carry[s],
            }
            (carry[s], last[s]) = (h - own, Some(r));
        }
    }
    for s in 0..2 {
        match last[s] {
            Some(l) => pads[s].below[l] += carry[s],
            None => pads[s].rest = carry[s],
        }
    }
    (pads, tops)
}

/// Where `side` has no line in a hunk at all — the other side only adds, or only deletes — the
/// blank that levels it, as `(y, height, hue)` rows for `multicaret::View::set_bands`, in the hue
/// of the lines it faces. A run of rows it has no line in that touches a changed line of its own
/// is part of a change already, and that line's tint covers it (see [`padding`]).
fn bands(
    heights: &[Vec<Option<i32>>; 2],
    extra: &[i32],
    changed: &[bool],
    tops: &[i32],
    side: Side,
) -> Vec<Band> {
    let (n, own) = (changed.len(), &heights[side.idx()]);
    let hue = match side {
        Side::Old => ADDED_HUE,
        Side::New => REMOVED_HUE,
    };
    let bottom =
        |r: usize| tops[r] + heights[0][r].unwrap_or(0).max(heights[1][r].unwrap_or(0)) + extra[r];
    let lacks = |r: usize| changed[r] && own[r].is_none();
    let mut out = Vec::new();
    let mut r = 0;
    while r < n {
        if !lacks(r) {
            r += 1;
            continue;
        }
        let start = r;
        while r < n && lacks(r) {
            r += 1;
        }
        if (start == 0 || !changed[start - 1]) && (r == n || !changed[r]) {
            out.push((tops[start], bottom(r - 1) - tops[start], hue));
        }
    }
    out
}

/// The natural height of one line of `buffer` as `view` lays it out, wrapping and all, with
/// `padded` — the pixels this module has put above and below it — taken back off. The flag
/// says the figure is an estimate.
///
/// GTK's own figure is used once it has one: a validated line's height is the paragraph's Pango
/// extent rounded once, margins on. GTK validates lazily, though, and reports 0 for a line it
/// has not reached, so until then the height is measured inside the paragraph — the first and
/// last character's positions come from the same layout, so their difference is right to
/// within the pixel a wrapped paragraph's per-line rounding can add — and the caller asks again
/// once GTK has caught up. A paragraph the buffer counts as several lines (U+2029) is measured
/// that way too, since GTK's figure would be for its first line alone.
fn measure(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    from: i32,
    to: i32,
    padded: i32,
) -> (i32, bool) {
    let first = buffer.iter_at_offset(from);
    let last = buffer.iter_at_offset((to - 1).max(from));
    if first.line() == last.line() {
        let (_, height) = view.line_yrange(&first);
        if height > 0 {
            return (height - padded, false);
        }
    }
    let (a, b) = (view.iter_location(&first), view.iter_location(&last));
    let margins = view.pixels_above_lines() + view.pixels_below_lines();
    (b.y() + b.height() - a.y() + margins, true)
}

/// The padding the paragraph starting at `at` carries, above and below, over the view's own
/// margins.
///
/// Read off the buffer rather than remembered, because it is what GTK's figure for the line
/// includes: a line keeps the height it was last laid out at until GTK lays it out again, even
/// after an edit or a tag change, so taking off anything but the padding it really carries
/// measures it wrong. Rows renumber after an edit, but the tags move with the text.
fn carried(view: &sourceview5::View, at: &gtk::TextIter) -> (i32, i32) {
    let (mut above, mut below) = (0, 0);
    // Lowest priority first, so where a paste has left two, the one GTK uses is read last.
    for tag in at.tags() {
        let Some(name) = tag.name() else { continue };
        if name.starts_with(PAD_ABOVE) {
            above = tag.pixels_above_lines() - view.pixels_above_lines();
        } else if name.starts_with(PAD_BELOW) {
            below = tag.pixels_below_lines() - view.pixels_below_lines();
        }
    }
    (above, below)
}

/// Give the paragraph `from..to` (its newline included) `above` pixels of padding above it and
/// `below` under it, where its first character does not carry exactly that already: a paragraph
/// left alone is not laid out again. `true` when it was not left alone.
///
/// GTK reads a paragraph's spacing off its first character alone, but the tags cover the newline
/// before the paragraph and the paragraph itself up to its own newline. Text typed at its start
/// lands inside them — text inserted where a tag begins does not take the tag — and so does the
/// character left when the first is deleted, where tags on the first character alone were lost
/// to either edit and the line was laid out bare for a frame. The paragraph's own newline is left
/// out because a tag ending at the next line's start is taken by text typed there. The first
/// paragraph has no newline before it: [`reclaim_start`] makes up for that.
fn pad(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    from: i32,
    to: i32,
    above: i32,
    below: i32,
) -> bool {
    let first = buffer.iter_at_offset(from);
    let (start, end) = (
        buffer.iter_at_offset((from - 1).max(0)),
        buffer.iter_at_offset((to - 1).max(from + 1)),
    );
    let (had_above, had_below) = carried(view, &first);
    let mut changed = false;
    for (px, had, prefix, base) in [
        (above, had_above, PAD_ABOVE, view.pixels_above_lines()),
        (below, had_below, PAD_BELOW, view.pixels_below_lines()),
    ] {
        if px == had {
            continue;
        }
        changed = true;
        for tag in first.tags() {
            if tag.name().is_some_and(|name| name.starts_with(prefix)) {
                buffer.remove_tag(&tag, &start, &end);
            }
        }
        if px > 0 {
            buffer.apply_tag(&pad_tag(buffer, prefix, base + px), &start, &end);
        }
    }
    changed
}

fn is_pad(tag: &gtk::TextTag) -> bool {
    tag.name()
        .is_some_and(|name| name.starts_with(PAD_ABOVE) || name.starts_with(PAD_BELOW))
}

/// The lines a change can have pushed a padding tag out of: the one the caret is in, which is
/// where typing lands, and the first, which has no newline before it whoever wrote into it.
fn reclaim(buffer: &sourceview5::Buffer) {
    reclaim_line(buffer, &buffer.start_iter());
    reclaim_line(buffer, &buffer.iter_at_mark(&buffer.get_insert()));
}

/// Put text typed at the start of `line` back under the paragraph's padding.
///
/// [`pad`] starts a paragraph's tags at the newline before it, so text typed at its start lands
/// inside them. Two paragraphs cannot both own that newline, though: the first line of all has
/// none, and a padded blank line's own padding sits on the newline the paragraph under it would
/// start from, which [`pad`] takes back for the blank line. There the tags begin at the
/// paragraph's own first character and a character typed ahead of them goes in outside: GTK lays
/// the line out bare for a frame, and the relayout, finding no padding on its first character,
/// measures it at the height it had last been laid out at, padding and all. Stretched back over
/// what was typed, the tags cover the first character again and say what GTK last used.
///
/// The line's own tags are the ones that end with it: the next paragraph's begins at this line's
/// newline as well, and runs on past it.
fn reclaim_line(buffer: &sourceview5::Buffer, line: &gtk::TextIter) {
    let mut start = *line;
    start.set_line_offset(0);
    if start.tags().iter().any(is_pad) {
        return;
    }
    // The start of the next line, which is where a tag of this one's ends at the latest.
    let mut to = start;
    to.forward_line();
    let mut at = start;
    while at.forward_to_tag_toggle(None::<&gtk::TextTag>) && at < to {
        let moved: Vec<gtk::TextTag> = at
            .toggled_tags(true)
            .into_iter()
            .filter(|tag| is_pad(tag) && ends_by(&at, tag, &to))
            .collect();
        if !moved.is_empty() {
            for tag in &moved {
                buffer.apply_tag(tag, &start, &at);
            }
            return;
        }
    }
}

/// Whether `tag`, which begins at `at`, is over by `to`.
fn ends_by(at: &gtk::TextIter, tag: &gtk::TextTag, to: &gtk::TextIter) -> bool {
    let mut end = *at;
    end.forward_to_tag_toggle(Some(tag));
    end <= *to
}

/// The tag named `prefix` plus `px`, which sets that many pixels above or below a paragraph.
/// `pixels-above-lines` on a tag replaces the view's default rather than adding to it, so `px`
/// is the base margin plus the pad.
fn pad_tag(buffer: &sourceview5::Buffer, prefix: &str, px: i32) -> gtk::TextTag {
    let name = format!("{prefix}{px}");
    let table = buffer.tag_table();
    table.lookup(&name).unwrap_or_else(|| {
        let tag = gtk::TextTag::new(Some(&name));
        match prefix {
            PAD_BELOW => tag.set_pixels_below_lines(px),
            _ => tag.set_pixels_above_lines(px),
        }
        table.add(&tag);
        tag
    })
}

fn install_tags(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    for name in [TAG_ADDED, TAG_REMOVED, TAG_ADDED_EMPH, TAG_REMOVED_EMPH] {
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
    hunk_buttons: bool,
    paned: gtk::Paned,
    lines: RefCell<Vec<DiffLine>>,
    rows: RefCell<Vec<Row>>,
    /// [`line_starts`] of each side's text.
    starts: RefCell<[Vec<i32>; 2]>,
    /// The row ranges hidden right now, each with the key it can be opened by.
    hidden: RefCell<Vec<(Range<usize>, usize)>>,
    /// Lines the user asked to see, by their number on the side that is not typed into, which
    /// survives the edits that move everything else. A run holding one stays open.
    opened: RefCell<HashSet<usize>>,
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

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&old.root));
        paned.set_end_child(Some(&new.root));
        paned.set_resize_start_child(true);
        paned.set_shrink_start_child(false);
        paned.set_resize_end_child(true);
        paned.set_shrink_end_child(false);
        // Even split. The widget cannot know how wide its host will be, so the position is set
        // once from an idle, which runs after the first layout pass has given the paned a width.
        paned.connect_map(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || {
                if p.width() > 0 {
                    p.set_position(p.width() / 2);
                }
            });
        });

        let this = Rc::new_cyclic(|weak| Compare {
            weak: weak.clone(),
            panes: [old, new],
            editable,
            hunk_buttons: hunk_buttons && editable.is_some(),
            paned,
            lines: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            starts: RefCell::new([Vec::new(), Vec::new()]),
            hidden: RefCell::new(Vec::new()),
            opened: RefCell::new(HashSet::new()),
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
            // window resize lands here and re-measures the rows.
            let w = weak.clone();
            let hadj = pane.scroller.hadjustment();
            let id = hadj.connect_page_size_notify(move |_| {
                if let Some(c) = w.upgrade() {
                    c.schedule_relayout();
                }
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
        // A scroll held by `Compare::open_run` goes back to where it is held.
        let w = weak.clone();
        let id = vadj.connect_value_changed(move |adj| {
            if let Some(Keep::Scroll(value)) = w.upgrade().and_then(|c| c.keep.get())
                && adj.value() != value
            {
                adj.set_value(value);
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
            let id = buffer.connect_changed(reclaim);
            connect(buffer.upcast(), id);
        }

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
        let pane = self.pane(side);
        pane.buffer.set_text(&text);
        editor::style_companion(pane.flavour, &pane.buffer, &pane.view);
        self.refresh();
    }

    /// Re-read both buffers and lay the diff over them: the tints, the emphasis, the hidden runs
    /// and the buttons. Nothing in either buffer's text is touched.
    pub fn refresh(&self) {
        self.lay(false);
    }

    /// Put `label` on both panes' context menus, there while the pane has a selection. `act` is
    /// handed that pane's side, the lines of its text the selection covers (1-based, inclusive)
    /// and both texts as they stand. Once per comparison: [`Compare::leave`] takes it off again.
    ///
    /// The entry joins whatever menu the view had, which on the editor is the spell checker's
    /// suggestions, and hides rather than greys out: `hidden-when` follows the action, and the
    /// action follows the selection, so a menu opened from the keyboard is right too.
    pub fn offer(
        &self,
        label: &str,
        act: impl Fn(Side, RangeInclusive<usize>, &str, &str) + 'static,
    ) {
        if !self.offered.borrow().is_empty() {
            return;
        }
        let act = Rc::new(act);
        for side in [Side::Old, Side::New] {
            let pane = self.pane(side);
            let action = gio::SimpleAction::new("selection", None);
            action.set_enabled(pane.buffer.has_selection());
            let (weak, act) = (self.weak.clone(), act.clone());
            action.connect_activate(move |_, _| {
                let Some(compare) = weak.upgrade() else {
                    return;
                };
                let texts = [compare.text(Side::Old), compare.text(Side::New)];
                if let Some((from, to)) = compare.pane(side).buffer.selection_bounds() {
                    let lines =
                        lines_between(&line_starts(&texts[side.idx()]), from.offset(), to.offset());
                    act(side, lines, &texts[0], &texts[1]);
                }
            });
            let group = gio::SimpleActionGroup::new();
            group.add_action(&action);
            pane.view.insert_action_group("diff", Some(&group));
            let id = pane
                .buffer
                .connect_has_selection_notify(move |b| action.set_enabled(b.has_selection()));
            self.handlers
                .borrow_mut()
                .push((pane.buffer.clone().upcast(), id));

            let item = gio::MenuItem::new(Some(label), Some("diff.selection"));
            item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
            let section = gio::Menu::new();
            section.append_item(&item);
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
        for gap in diff::gaps(&lines, &rows, CONTEXT) {
            let number = |r: usize| {
                keyed
                    .of(&rows[r])
                    .and_then(|i| keyed.number(&lines[i]))
                    .unwrap_or(0)
            };
            let (key, last) = (number(gap.start), number(gap.end - 1));
            // A run the user opened stays open, and so does one it has since merged into: the
            // line they asked to see is still in it.
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
        if self.hunk_buttons
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

    /// Put the first hunk at [`FIRST_HUNK_AT`] of the page, once, from the rows the last relayout
    /// measured. Both panes share the vertical adjustment, so setting it scrolls both.
    ///
    /// The grid rather than GTK's own figures, and not before the relayout: GTK lays lines out
    /// lazily and the padding just laid is not in its figures yet, so a `scroll_to_mark` made as
    /// the comparison opened landed wherever the estimates put the line, which in a long file
    /// with its unchanged runs folded away was nowhere near it.
    fn reveal_first_hunk(&self) {
        let row = {
            let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
            diff::hunks(&lines, &rows).first().map(|hunk| hunk.start)
        };
        // Nothing has changed yet — an untouched buffer against its own index side. The next
        // relayout that finds a difference is the one that opens on it.
        let Some(top) = row.and_then(|r| self.grid.borrow().tops.get(r).copied()) else {
            return;
        };
        self.keep.set(None);
        // `visible_rect` is in buffer coordinates and the adjustment is not — the top margin
        // lies between them — so the scroll moves by the distance from what is on screen now.
        let (adj, seen) = (
            self.panes[0].scroller.vadjustment(),
            self.panes[0].view.visible_rect(),
        );
        adj.set_value(adj.value() + f64::from(top - seen.y()) - FIRST_HUNK_AT * adj.page_size());
    }

    /// Open the hidden run keyed `key`, as its button does, with the scroll held where it was
    /// until the rows are laid: the rows above stay, and the run opens downwards from the button's
    /// row. Left to GTK, each view keeps its own top line in place as the lines above it grow,
    /// both on the one scroll they share, so a run opened at the top of the view scrolled it by
    /// twice its height, and by twice the height laid so far while GTK caught up.
    fn open_run(&self, key: usize) {
        let value = self.panes[0].scroller.vadjustment().value();
        self.keep.set(Some(Keep::Scroll(value)));
        self.opened.borrow_mut().insert(key);
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
                if is_pad(tag) {
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
    /// A jump that moves the caret needs none of this — [`Compare::lay`] leaves the run the caret
    /// is in open — but Go to Line's preview moves no caret, and taking the tag off that side's
    /// buffer alone would show the lines under the other side's "unchanged lines" button.
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
        self.opened.borrow_mut().insert(key);
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
    fn padding_keeps_every_row_level_and_hands_a_fillers_share_on() {
        // Row 1 is a two-line paragraph on the old side facing one line; row 2 is a filler on
        // the old side; row 3 exists on both. Rows 1 and 2 are one change.
        let old = [Some(10), Some(20), None, Some(10)];
        let new = [Some(10), Some(10), Some(10), Some(10)];
        let changed = [false, true, true, false];
        let (pads, tops) = padding(&old, &new, &[0; 4], &changed);
        assert_eq!(tops, vec![0, 10, 30, 40]);
        assert_eq!(pads[0].above, vec![0; 4]);
        assert_eq!(
            pads[0].below,
            vec![0, 10, 0, 0],
            "the filler's row goes under the change it belongs to, and takes its tint"
        );
        assert_eq!(pads[1].above, vec![0; 4]);
        assert_eq!(
            pads[1].below,
            vec![0, 10, 0, 0],
            "the shorter line of row 1 starts with its partner, and the blank under it is its own"
        );

        // A deletion with no line of its own on the new side: the blank has no changed line to
        // go under, so it waits above the next one.
        let (pads, _) = padding(
            &[Some(10), Some(10), Some(10)],
            &[Some(10), None, Some(10)],
            &[0; 3],
            &[false, true, false],
        );
        assert_eq!(
            (pads[1].above.clone(), pads[1].below.clone()),
            (vec![0, 0, 10], vec![0; 3])
        );
    }

    #[test]
    fn a_hunk_with_no_line_on_one_side_leaves_a_band_there_in_the_other_sides_hue() {
        // Rows 1 and 2 only delete; the new side has nothing there.
        let heights = [
            vec![Some(10), Some(20), Some(10), Some(10)],
            vec![Some(10), None, None, Some(10)],
        ];
        let (extra, changed) = ([0; 4], [false, true, true, false]);
        let tops = [0, 10, 30, 40];
        assert_eq!(
            bands(&heights, &extra, &changed, &tops, Side::New),
            vec![(10, 30, REMOVED_HUE)]
        );
        assert_eq!(bands(&heights, &extra, &changed, &tops, Side::Old), vec![]);

        // A deletion under a changed pair is that change's blank, which its own tint covers.
        let heights = [
            vec![Some(10), Some(20), Some(10), Some(10)],
            vec![Some(10), Some(10), None, Some(10)],
        ];
        assert_eq!(bands(&heights, &extra, &changed, &tops, Side::New), vec![]);
    }

    #[test]
    fn trailing_fillers_and_gap_space_go_below_the_last_line() {
        let old = [Some(10), None, None];
        let new = [Some(10), Some(10), Some(10)];
        let (pads, _) = padding(&old, &new, &[0, 0, 0], &[false, true, true]);
        assert_eq!(pads[0].below, vec![20, 0, 0]);
        assert_eq!(pads[1].above, vec![0, 0, 0]);

        // A hidden run leaves the same blank on both sides, so the alignment is unmoved.
        let (pads, tops) = padding(
            &[Some(10), None, Some(10)],
            &[Some(10), None, Some(10)],
            &[0, 5, 0],
            &[false; 3],
        );
        assert_eq!(tops, vec![0, 10, 15]);
        assert_eq!(pads[0].above, vec![0, 0, 5]);
        assert_eq!(pads[1].above, vec![0, 0, 5]);
    }

    #[test]
    fn a_side_with_no_line_at_all_leaves_the_whole_column_under_its_text() {
        // A file a commit added: nothing on the old side to pad.
        let (pads, _) = padding(&[None, None], &[Some(20), Some(30)], &[0, 0], &[true, true]);
        assert_eq!((pads[0].rest, pads[1].rest), (50, 0));
        assert_eq!(pads[0].below, vec![0, 0]);
    }
}
