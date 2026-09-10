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
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ops::Range;
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

/// The mark [`Compare::reveal_first_hunk`] scrolls to, one per buffer and reused.
const MARK_FIRST_HUNK: &str = "diff-first-hunk";
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
/// Share of the tint that is the hue; the rest is the foreground.
const HUE_MIX: f32 = 0.65;
const CHANGE_ALPHA: f32 = 0.16;
/// The words that actually differ, in the same hue over the row's own background. Emphasis is
/// colour only: bold would change advance widths and pull the two panes out of alignment.
const EMPH_ALPHA: f32 = 0.35;

/// Which of the two texts a pane shows: `Old` is the left column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Old,
    New,
}

impl Side {
    fn idx(self) -> usize {
        match self {
            Side::Old => 0,
            Side::New => 1,
        }
    }

    pub fn other(self) -> Side {
        match self {
            Side::Old => Side::New,
            Side::New => Side::Old,
        }
    }

    /// The index into the diff of the line this row shows on this side.
    fn of(self, row: &Row) -> Option<usize> {
        match self {
            Side::Old => row.old,
            Side::New => row.new,
        }
    }

    /// The 1-based line number of `line` in this side's text.
    fn number(self, line: &DiffLine) -> Option<usize> {
        match self {
            Side::Old => line.old_line,
            Side::New => line.new_line,
        }
    }

    /// The row and word-emphasis tags a changed line gets on this side, or `None` for an
    /// unchanged one. `align` never puts an insertion on the old side, nor a deletion on the new.
    fn tag(self, op: Op) -> Option<(&'static str, &'static str)> {
        match (self, op) {
            (Side::Old, Op::Delete) => Some((TAG_REMOVED, TAG_REMOVED_EMPH)),
            (Side::New, Op::Insert) => Some((TAG_ADDED, TAG_ADDED_EMPH)),
            _ => None,
        }
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
                        compare.opened.borrow_mut().insert(key);
                        compare.refresh();
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
fn normalise(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Where each `\n`-separated line of `text` starts, in characters, with the text's end appended
/// so line `n` (1-based) always spans `starts[n - 1]..starts[n]`, its newline included.
///
/// Counted in characters and not taken from the buffer's own line numbers: `GtkTextBuffer`
/// breaks a line at U+2029 too, and the diff does not, so a note carrying one puts the two
/// numberings permanently out of step. A character offset means the same thing to both.
fn line_starts(text: &str) -> Vec<i32> {
    let mut starts = Vec::new();
    let mut at = 0;
    for line in text.split('\n') {
        starts.push(at);
        at += line.chars().count() as i32 + 1;
    }
    starts.push(at - 1);
    starts
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

/// What a relayout measured: every row's natural height per side (`None` where the side has no
/// visible line), the space both sides leave at a row on purpose, and where each row starts in
/// the shared column.
#[derive(Default)]
struct Grid {
    heights: [Vec<Option<i32>>; 2],
    extra: Vec<i32>,
    tops: Vec<i32>,
}

/// Blank space above each row of one column, and below its last, that keeps it in step with the
/// other column. See [`padding`].
#[derive(Debug, PartialEq, Eq)]
struct Pads {
    above: Vec<i32>,
    below_last: i32,
}

/// From the natural height of every row on each side — `None` where a side has no visible line
/// there — the space each side adds so that row `i` starts at the same height on both, and the
/// top of every row in that shared grid. `extra` is space both sides leave at a row on purpose,
/// which is where a hidden run's button goes.
///
/// A row with no line on a side has nothing to carry space, so its share is handed on to the next
/// row that has one; what is left after the last goes below it. This is the whole correctness
/// surface of the alignment, so it is a plain function over plain numbers.
fn padding(old: &[Option<i32>], new: &[Option<i32>], extra: &[i32]) -> ([Pads; 2], Vec<i32>) {
    let n = old.len();
    let mut pads = [
        Pads {
            above: vec![0; n],
            below_last: 0,
        },
        Pads {
            above: vec![0; n],
            below_last: 0,
        },
    ];
    let mut tops = Vec::with_capacity(n);
    let (mut y, mut carry) = (0, [0, 0]);
    for r in 0..n {
        tops.push(y);
        let h = old[r].unwrap_or(0).max(new[r].unwrap_or(0)) + extra[r];
        y += h;
        for (s, side) in [old, new].into_iter().enumerate() {
            match side[r] {
                Some(own) => {
                    pads[s].above[r] = carry[s] + h - own;
                    carry[s] = 0;
                }
                None => carry[s] += h,
            }
        }
    }
    pads[0].below_last = carry[0];
    pads[1].below_last = carry[1];
    (pads, tops)
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
/// left alone is not laid out again.
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
) {
    let first = buffer.iter_at_offset(from);
    let (start, end) = (
        buffer.iter_at_offset((from - 1).max(0)),
        buffer.iter_at_offset((to - 1).max(from + 1)),
    );
    let (had_above, had_below) = carried(view, &first);
    for (px, had, prefix, base) in [
        (above, had_above, PAD_ABOVE, view.pixels_above_lines()),
        (below, had_below, PAD_BELOW, view.pixels_below_lines()),
    ] {
        if px == had {
            continue;
        }
        for tag in first.tags() {
            if tag.name().is_some_and(|name| name.starts_with(prefix)) {
                buffer.remove_tag(&tag, &start, &end);
            }
        }
        if px > 0 {
            buffer.apply_tag(&pad_tag(buffer, prefix, base + px), &start, &end);
        }
    }
}

fn is_pad(tag: &gtk::TextTag) -> bool {
    tag.name()
        .is_some_and(|name| name.starts_with(PAD_ABOVE) || name.starts_with(PAD_BELOW))
}

/// Put text typed at the very start of the buffer back under the first paragraph's padding.
///
/// That paragraph has no newline before it for [`pad`] to start its tags from, so a character
/// typed ahead of it went in outside them: GTK laid the line out bare for a frame, and the
/// relayout, finding no padding on its first character, measured it at the height it had last
/// been laid out at, padding and all. Stretched back over what was typed, the tags cover the
/// first character again and say what GTK last laid the line out with. The first pad tag that
/// begins inside the line is the one the typing moved: the next paragraph's begins at the
/// line's own newline. A first line that was empty is left alone, for that reason: its tags sit
/// on that newline beside the next paragraph's.
fn reclaim_start(buffer: &sourceview5::Buffer) {
    let start = buffer.start_iter();
    if start.tags().iter().any(is_pad) {
        return;
    }
    let mut end = start;
    end.forward_to_line_end();
    let mut at = start;
    while at.forward_to_tag_toggle(None::<&gtk::TextTag>) && at < end {
        let moved: Vec<gtk::TextTag> = at.toggled_tags(true).into_iter().filter(is_pad).collect();
        if !moved.is_empty() {
            for tag in &moved {
                buffer.apply_tag(tag, &start, &at);
            }
            return;
        }
    }
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
    /// Whether the next refresh should put the first hunk on screen. Set once, when the
    /// comparison is built: a diff opens on what changed rather than on the top of a file whose
    /// first difference is four hundred lines down. Cleared by the refresh that does it, because
    /// after that where the view sits is the reader's business.
    first_view: Cell<bool>,
    handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
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
        new.scroller
            .set_vadjustment(Some(&old.scroller.vadjustment()));
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
            first_view: Cell::new(true),
            handlers: RefCell::new(Vec::new()),
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
        connect(vadj.upcast(), id);
        let w = weak.clone();
        let style = adw::StyleManager::default();
        let id = style.connect_dark_notify(move |_| {
            if let Some(c) = w.upgrade() {
                c.restyle();
            }
        });
        connect(style.upcast(), id);
        // Text typed ahead of the first line's padding goes back under it on the keystroke itself:
        // above 16 KB the editor refreshes the comparison only on its debounce, and the line
        // would be laid out bare until then.
        if let Some(mine) = editable {
            let buffer = this.pane(mine).buffer.clone();
            let id = buffer.connect_changed(reclaim_start);
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
        editor::style_companion(pane.flavour, &pane.buffer);
        self.refresh();
    }

    /// Re-read both buffers and lay the diff over them: the tints, the emphasis, the hidden runs
    /// and the buttons. Nothing in either buffer's text is touched.
    pub fn refresh(&self) {
        self.lay(false);
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
                let Some((row_tag, emph_tag)) = side.tag(line.op) else {
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
        if self.first_view.get() {
            self.reveal_first_hunk();
        }
    }

    /// Put the first hunk on screen, once. Both panes share a vertical adjustment, so scrolling
    /// either scrolls both, and the first one with a line in that hunk is the one asked.
    fn reveal_first_hunk(&self) {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let Some(hunk) = diff::hunks(&lines, &rows).into_iter().next() else {
            // Nothing has changed yet — an untouched buffer against its own index side. The next
            // refresh that finds a difference is the one that opens on it.
            return;
        };
        self.first_view.set(false);
        for side in [Side::Old, Side::New] {
            let Some(n) = hunk
                .clone()
                .find_map(|r| side.of(&rows[r]).and_then(|i| side.number(&lines[i])))
            else {
                continue;
            };
            let pane = self.pane(side);
            let at = pane.buffer.iter_at_offset(starts[side.idx()][n - 1]);
            // A mark rather than the iter: `scroll_to_iter` gives up when the line it wants has
            // not been laid out yet, which on a comparison that is opening is every line.
            let mark = match pane.buffer.mark(MARK_FIRST_HUNK) {
                Some(mark) => {
                    pane.buffer.move_mark(&mark, &at);
                    mark
                }
                None => pane.buffer.create_mark(Some(MARK_FIRST_HUNK), &at, true),
            };
            pane.view
                .scroll_to_mark(&mark, 0.0, true, 0.0, FIRST_HUNK_AT);
            return;
        }
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
        }
        self.overlays.borrow_mut().clear();
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        for (object, id) in self.handlers.borrow_mut().drain(..) {
            object.disconnect(id);
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
            reclaim_start(&self.pane(mine).buffer);
        }
        // The companion takes the editor's page margins, so the first row of each starts level.
        if let Some(mine) = self.editable {
            let (from, to) = (&self.pane(mine).view, &self.pane(mine.other()).view);
            to.set_top_margin(from.top_margin());
            to.set_bottom_margin(from.bottom_margin());
            to.set_left_margin(from.left_margin());
            to.set_right_margin(from.right_margin());
            to.set_pixels_above_lines(from.pixels_above_lines());
            to.set_pixels_below_lines(from.pixels_below_lines());
        }
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
        let (pads, tops) = padding(&heights[0], &heights[1], &extra);

        for side in [Side::Old, Side::New] {
            let pane = self.pane(side);
            let st = &starts[side.idx()];
            let now = &pads[side.idx()];
            // Below the last visible line, which is where trailing rows of the other side fall.
            let last = (0..rows.len())
                .rev()
                .find(|&r| heights[side.idx()][r].is_some());
            // Every line on this side, hidden ones included, so a row that is hidden now or was
            // the last one before an edit does not keep what it carried then.
            for (r, row) in rows.iter().enumerate() {
                let Some(n) = side.of(row).and_then(|i| side.number(&lines[i])) else {
                    continue;
                };
                let below = if last == Some(r) { now.below_last } else { 0 };
                pad(
                    &pane.view,
                    &pane.buffer,
                    st[n - 1],
                    st[n],
                    now.above[r],
                    below,
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
            heights,
            extra,
            tops,
        };
        // GTK had not laid some line out yet: ask again once it has.
        if estimated.get() && self.settling.get() > 0 && self.pending.borrow().is_none() {
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
    pub fn skew(&self) -> i32 {
        let top = |side: Side| {
            self.pane(side)
                .scroller
                .compute_point(&self.paned, &gtk::graphene::Point::zero())
                .map_or(0.0, |p| p.y())
        };
        (top(Side::New) - top(Side::Old)).round() as i32
    }

    /// Whether row `r` is in a hidden run right now.
    pub fn hides_row(&self, r: usize) -> bool {
        self.hidden.borrow().iter().any(|(gap, _)| gap.contains(&r))
    }

    /// Where the first change starts on the editor's side, in characters.
    pub fn opens_at(&self) -> Option<i32> {
        let side = self.editable?;
        let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
        first_change(&lines, &rows, &self.starts.borrow()[side.idx()], side)
    }

    /// How many rows GTK lays out at a different height than the last relayout meant them to
    /// have, on either side: the number the alignment stands or falls on, and 0 is the claim.
    pub fn misaligned(&self) -> usize {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let grid = self.grid.borrow();
        let Grid {
            heights,
            extra,
            tops,
        } = &*grid;
        if tops.len() != rows.len() {
            return rows.len();
        }
        let off = |side: Side, r: usize| -> bool {
            let pane = self.pane(side);
            let Some(own) = heights[side.idx()][r] else {
                return false;
            };
            let Some(n) = side.of(&rows[r]).and_then(|i| side.number(&lines[i])) else {
                return false;
            };
            let tallest = heights[0][r].unwrap_or(0).max(heights[1][r].unwrap_or(0)) + extra[r];
            let expected = tops[r] + pane.view.pixels_above_lines() + (tallest - own);
            let iter = pane.buffer.iter_at_offset(starts[side.idx()][n - 1]);
            pane.view.iter_location(&iter).y() != expected
        };
        (0..rows.len())
            .filter(|&r| off(Side::Old, r) || off(Side::New, r))
            .count()
    }

    /// The first row [`Compare::misaligned`] counts, spelled out: which row and side, what the
    /// relayout expected, what GTK laid out, and the line. For the bench to print.
    pub fn first_misaligned(&self) -> Option<String> {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let grid = self.grid.borrow();
        for (r, row) in rows.iter().enumerate() {
            for side in [Side::Old, Side::New] {
                let pane = self.pane(side);
                let (Some(own), Some(n)) = (
                    grid.heights
                        .get(side.idx())
                        .and_then(|h| h.get(r).copied().flatten()),
                    side.of(row).and_then(|i| side.number(&lines[i])),
                ) else {
                    continue;
                };
                let tallest = grid.heights[0][r]
                    .unwrap_or(0)
                    .max(grid.heights[1][r].unwrap_or(0))
                    + grid.extra[r];
                let expected = grid.tops[r] + pane.view.pixels_above_lines() + (tallest - own);
                let iter = pane.buffer.iter_at_offset(starts[side.idx()][n - 1]);
                let actual = pane.view.iter_location(&iter).y();
                if actual != expected {
                    let text = side
                        .of(row)
                        .map(|i| lines[i].text.clone())
                        .unwrap_or_default();
                    return Some(format!(
                        "row={r} side={side:?} expected={expected} actual={actual} own={own} tallest={tallest} text={text:?}"
                    ));
                }
            }
        }
        None
    }

    /// What the `Take` (or, with `keep_own`, the `Both`) button on the `i`th hunk does.
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
    pub fn open_gap(&self, i: usize) {
        let key = self.hidden.borrow().get(i).map(|(_, key)| *key);
        if let Some(key) = key {
            self.opened.borrow_mut().insert(key);
            self.refresh();
        }
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

/// A comparison of two texts that are not files, as a tab of its own: a staged change, a commit
/// against its parent. Both panes are companions, so the tab carries the font provider the
/// editor would otherwise have.
pub struct DiffTab {
    pub page: adw::TabPage,
    key: String,
    compare: Rc<Compare>,
    /// Both panes' views. They take the page's margins from the zoom here, having no editor
    /// beside them for [`Compare::relayout`] to copy them from.
    views: [sourceview5::View; 2],
    flavour: Flavour,
    name: String,
    font: RefCell<Option<gtk::CssProvider>>,
}

impl DiffTab {
    /// `old` and `new` are (title, text). `key` is what the tab is keyed by, see
    /// `App::open_diff`; the file name behind it picks the language and the flavour.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        tabs: &adw::TabView,
        key: &str,
        file: &str,
        title: &str,
        flavour: Flavour,
        old: (&str, &str),
        new: (&str, &str),
        font: Option<&str>,
        zoom: f64,
    ) -> Rc<DiffTab> {
        let name = editor::next_view_name();
        let language = match flavour {
            Flavour::Code => editor::language_for(file, new.1),
            _ => None,
        };
        let old = pane(old.0, flavour, old.1, &name, language.as_ref());
        let new = pane(new.0, flavour, new.1, &name, language.as_ref());
        let views = [old.view.clone(), new.view.clone()];
        let compare = Compare::new(old, new, None, false);
        let page = tabs.append(compare.widget());
        page.set_title(title);
        page.set_icon(Some(&gtk::gio::ThemedIcon::new("view-dual-symbolic")));
        let tab = Rc::new(DiffTab {
            page,
            key: key.to_string(),
            compare,
            views,
            flavour,
            name,
            font: RefCell::new(None),
        });
        tab.set_font(font, zoom);
        tab
    }

    pub fn key(&self) -> String {
        self.key.clone()
    }

    /// The font and the page at `zoom`, as `Tab::set_font` sets them for an editor.
    pub fn set_font(&self, font: Option<&str>, zoom: f64) {
        for view in &self.views {
            editor::set_margins(view, zoom);
        }
        editor::install_font(&self.font, self.flavour, font, zoom, &self.name);
        // Heading markers hang in the left margin and are measured in the font, so they are
        // measured again once the font has reached the views, as `Tab::rehang` does.
        let compare = Rc::downgrade(&self.compare);
        glib::idle_add_local_once(move || {
            if let Some(compare) = compare.upgrade() {
                compare.restyle();
            }
        });
    }

    pub fn restyle(&self) {
        self.compare.restyle();
    }

    /// Both texts again, after what they compare has moved.
    pub fn set_texts(&self, old: &str, new: &str) {
        self.compare.set_side(Side::Old, old);
        self.compare.set_side(Side::New, new);
    }

    pub fn comparison(&self) -> &Rc<Compare> {
        &self.compare
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
        // the old side; row 3 exists on both.
        let old = [Some(10), Some(20), None, Some(10)];
        let new = [Some(10), Some(10), Some(10), Some(10)];
        let (pads, tops) = padding(&old, &new, &[0; 4]);
        assert_eq!(tops, vec![0, 10, 30, 40]);
        assert_eq!(
            pads[0].above,
            vec![0, 0, 0, 10],
            "the filler's row lands on row 3"
        );
        assert_eq!(pads[1].above, vec![0, 10, 0, 0]);
        assert_eq!((pads[0].below_last, pads[1].below_last), (0, 0));
    }

    #[test]
    fn trailing_fillers_and_gap_space_go_below_the_last_line() {
        let old = [Some(10), None, None];
        let new = [Some(10), Some(10), Some(10)];
        let (pads, _) = padding(&old, &new, &[0, 0, 0]);
        assert_eq!(pads[0].below_last, 20);
        assert_eq!(pads[1].above, vec![0, 0, 0]);

        // A hidden run leaves the same blank on both sides, so the alignment is unmoved.
        let (pads, tops) = padding(
            &[Some(10), None, Some(10)],
            &[Some(10), None, Some(10)],
            &[0, 5, 0],
        );
        assert_eq!(tops, vec![0, 10, 15]);
        assert_eq!(pads[0].above, vec![0, 0, 5]);
        assert_eq!(pads[1].above, vec![0, 0, 5]);
    }
}
