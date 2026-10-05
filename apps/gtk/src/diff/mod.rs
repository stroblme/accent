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
use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::{Range, RangeInclusive};
use std::rc::{Rc, Weak};

use crate::editor::{self, Flavour};

#[cfg(feature = "bench")]
mod bench;
mod columns;
pub mod merge;
mod pad;
mod pool;

use columns::{Anchor, Columns, Rows};
pub use merge::Merge;
use pad::UNMEASURED;
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

/// What an entry [`Compare::offer`] puts on the panes' menus does with a selection, and what a
/// button [`Compare::offer_hunks`] puts on each hunk does with that hunk's lines.
pub type OnLines = Rc<dyn Fn(Side, RangeInclusive<usize>, &str, &str)>;

/// What a button on a hunk does, handed the hunk's rows.
type OnHunk = Rc<dyn Fn(&Compare, Range<usize>)>;

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
/// into a buffer except the hunk buttons, which edit the user's side as the user would. The rows
/// are laid out level, the scroll is shared and the reader's place is kept by [`Columns`].
pub struct Compare {
    weak: Weak<Compare>,
    columns: Rc<Columns>,
    /// Which side is the user's own editor, if either. Its text is read, never set, and the hunk
    /// buttons write into it.
    editable: Option<Side>,
    /// The buttons each hunk carries on the pane beside the editor: a label, a tooltip, and what
    /// the button does.
    hunk_buttons: RefCell<Vec<(&'static str, &'static str, OnHunk)>>,
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
    handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    /// Each pane's context menu as [`Compare::offer`] left it, and the one it replaced, to put
    /// back when the comparison goes. Empty until something is offered.
    offered: RefCell<Vec<(gio::Menu, Option<gio::MenuModel>)>>,
    /// What to run once the rows have been laid again. The hidden runs move with every lay — a
    /// keystroke, a side re-read, a run opened — and what is drawn per line rather than per
    /// character has to follow them: see [`Compare::on_laid`].
    laid: RefCell<Option<Box<dyn Fn()>>>,
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

        let this = Rc::new_cyclic(|weak| Compare {
            weak: weak.clone(),
            columns: Columns::new(vec![old, new], editable.map(Side::idx)),
            editable,
            hunk_buttons: RefCell::new(takes),
            lines: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            starts: RefCell::new([Vec::new(), Vec::new()]),
            hidden: RefCell::new(Vec::new()),
            opened: RefCell::new(HashSet::new()),
            unfold,
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
        for pane in &this.columns.panes {
            // `view.color()` only resolves the theme foreground once the widget is mapped, and
            // the layout only measures true once it has a font.
            let w = weak.clone();
            pane.view.connect_map(move |_| {
                if let Some(c) = w.upgrade() {
                    c.restyle();
                    c.columns.schedule_relayout();
                }
            });
        }
        let w = weak.clone();
        let style = adw::StyleManager::default();
        let id = style.connect_dark_notify(move |_| {
            if let Some(c) = w.upgrade() {
                c.restyle();
            }
        });
        connect(style.upcast(), id);
        // Focus mode's line fade on the other column is measured from the lines facing the
        // editor's carets, so stepping through the changes keeps the two columns' focus level,
        // and drawn again as those carets move: its own caret nobody moves.
        if let Some(mine) = editable
            && let Some(theirs) = this
                .pane(mine.other())
                .view
                .downcast_ref::<crate::multicaret::View>()
        {
            let w = weak.clone();
            theirs.fade_from(move || w.upgrade()?.facing(mine));
            let theirs = theirs.downgrade();
            let buffer = this.pane(mine).buffer.clone();
            let id = buffer.connect_mark_set(move |buffer, _, mark| {
                let caret = [buffer.get_insert(), buffer.selection_bound()].contains(mark);
                if let Some(theirs) = theirs.upgrade().filter(|t| caret && t.fade_shown()) {
                    theirs.queue_draw();
                }
            });
            connect(buffer.upcast(), id);
        }

        let w = weak.clone();
        this.unfold.connect_toggled(move |_| {
            if let Some(c) = w.upgrade() {
                c.toggle_all();
            }
        });

        for pane in &this.columns.panes {
            let w = this.weak.clone();
            *pane.pool.act.borrow_mut() = Some(Rc::new(move |role, i| {
                let Some(c) = w.upgrade() else { return };
                match role {
                    Role::Gap { key, .. } => c.open_run(key),
                    Role::Hunk(hunk) => {
                        let on = c.hunk_buttons.borrow().get(i).map(|(.., on)| on.clone());
                        if let Some(on) = on {
                            on(&c, hunk);
                        }
                    }
                    Role::Block(_) => {}
                }
            }));
        }
        this.lay(true);
        this
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.columns.paned.upcast_ref()
    }

    fn pane(&self, side: Side) -> &Pane {
        &self.columns.panes[side.idx()]
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
        if !self.columns.opening() {
            let top = self.columns.top_line(side.other().idx());
            self.columns.keep.set(top);
        }
        let pane = self.pane(side);
        pane.buffer.set_text(&text);
        editor::style_companion(pane.flavour, &pane.buffer, &pane.view);
        self.refresh();
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

        for pane in &self.columns.panes {
            pane.pool.unclaim();
        }
        let mut overlays = Vec::new();
        if !self.hunk_buttons.borrow().is_empty()
            && let Some(mine) = self.editable
        {
            let (theirs, pane) = (mine.other(), self.pane(mine.other()));
            *pane.pool.buttons.borrow_mut() = (self.hunk_buttons.borrow().iter())
                .map(|(label, tip, _)| (*label, *tip))
                .collect();
            for hunk in diff::hunks(&lines, &rows) {
                let row = hunk.start;
                let widget = pane.pool.claim(&pane.view, Role::Hunk(hunk));
                overlays.push((theirs.idx(), widget, Anchor::Hunk(row)));
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
                overlays.push((side.idx(), widget, Anchor::Gap(gap.start)));
            }
        }
        for pane in &self.columns.panes {
            pane.pool.hide_unclaimed();
        }

        let number = |side: Side, row: &Row| side.of(row).and_then(|i| side.number(&lines[i]));
        let laid = Rows {
            lines: [Side::Old, Side::New]
                .map(|side| rows.iter().map(|row| number(side, row)).collect())
                .to_vec(),
            starts: starts.to_vec(),
            // A row is a change where either side's line is: a changed pair, or a line the other
            // side has none for.
            changed: rows
                .iter()
                .map(|row| {
                    [Side::Old, Side::New]
                        .iter()
                        .any(|side| side.of(row).is_some_and(|i| lines[i].op != Op::Equal))
                })
                .collect(),
            hidden: hidden.iter().map(|(gap, _)| gap.clone()).collect(),
            extra: vec![0; rows.len()],
            // The blank of a hunk that only adds or only deletes, in the hue of the lines it
            // faces: green on the left facing an addition, red on the right facing a deletion.
            hues: vec![ADDED_HUE, REMOVED_HUE],
            first: diff::hunks(&lines, &rows).first().map(|hunk| hunk.start),
            overlays,
        };
        *self.lines.borrow_mut() = lines;
        *self.rows.borrow_mut() = rows;
        *self.starts.borrow_mut() = starts;
        *self.hidden.borrow_mut() = hidden;
        self.columns.lay(laid);
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

    /// Open the hidden run keyed `key`, as its button does, with the scroll held where it was
    /// until the rows are laid: the rows above stay, and the run opens downwards from the button's
    /// row. Left to GTK, each view keeps its own top line in place as the lines above it grow,
    /// both on the one scroll they share, so a run opened at the top of the view scrolled it by
    /// twice its height, and by twice the height laid so far while GTK caught up.
    fn open_run(&self, key: usize) {
        self.columns.hold_scroll();
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
        if !self.columns.opening() {
            let top = || {
                self.columns
                    .top_line(self.editable.unwrap_or(Side::New).idx())
            };
            self.columns.hold(self.columns.caret_line().or_else(top));
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
        }
        self.columns.leave();
        for (object, id) in self.handlers.borrow_mut().drain(..) {
            object.disconnect(id);
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
        for (i, pane) in self.columns.panes.iter().enumerate() {
            if self.editable != Some([Side::Old, Side::New][i]) {
                editor::restyle_companion(pane.flavour, &pane.buffer, &pane.view);
            }
            restyle_tags(&pane.buffer, &pane.view);
        }
    }

    /// Put the editor's page on the companion beside it: see [`Columns::follow_editor`].
    pub fn follow_editor(&self, refont: bool) {
        self.columns.follow_editor(refont);
    }

    // --- for the bench and the tests ----------------------------------------------------------

    /// (rows, hunks, hidden runs, buttons) on screen right now.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
        (
            rows.len(),
            diff::hunks(&lines, &rows).len(),
            self.hidden.borrow().len(),
            self.columns.rows.borrow().overlays.len(),
        )
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
