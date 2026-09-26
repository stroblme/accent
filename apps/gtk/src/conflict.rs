//! Git's conflict markers in an editor, decorated as VS Code decorates them: each block's current
//! side tinted green and its incoming side blue, with Accept Current, Accept Incoming and Accept
//! Both above it.
//!
//! Knows nothing about tabs: it is handed a view and its buffer, and finds the blocks again
//! whenever the host analyses the text — on the keystroke up to 16 KB, on the debounce above. In
//! between, the tints move with the text as tags do. The buttons are overlays in buffer
//! coordinates, as a comparison's are (`diff.rs`), so they scroll with the text; they sit in a
//! band the block's first line is given above itself, and are laid again after each find and
//! whenever GTK reflows the lines.

use crate::{diff, highlight::Offsets, theme};
use accent_core::conflict::{self, Block, Take};
use gtk::{gdk, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::{Rc, Weak};

/// A side's tint, and its marker line's (`-head`) stronger one.
const CURRENT_HEAD: &str = "conflict-current-head";
const CURRENT: &str = "conflict-current";
const BASE_HEAD: &str = "conflict-base-head";
const BASE: &str = "conflict-base";
const INCOMING: &str = "conflict-incoming";
const INCOMING_HEAD: &str = "conflict-incoming-head";
const TINTS: [&str; 6] = [
    CURRENT_HEAD,
    CURRENT,
    BASE_HEAD,
    BASE,
    INCOMING,
    INCOMING_HEAD,
];
/// The room above a block's `<<<<<<<` line that its buttons sit in.
const BAND: &str = "conflict-band";
/// Space between the buttons and the band's edges.
const GAP: i32 = 3;
/// A side's lines and its marker line: the alphas a comparison gives a changed row and the words
/// that changed in it. The base, which neither side keeps, is the foreground at half of each.
const BODY_ALPHA: f32 = 0.16;
const HEAD_ALPHA: f32 = 0.35;

/// The current, base and incoming sides' tints, each with its marker line's, from the resolved
/// foreground: the editor's tags and the preview's boxes (`preview.rs`) alike.
pub(crate) fn tints(fg: gdk::RGBA) -> [(gdk::RGBA, gdk::RGBA); 3] {
    let side = |hue| {
        (
            diff::tint(hue, fg, BODY_ALPHA),
            diff::tint(hue, fg, HEAD_ALPHA),
        )
    };
    [
        side(diff::ADDED_HUE),
        (
            theme::at(fg, BODY_ALPHA / 2.0),
            theme::at(fg, HEAD_ALPHA / 2.0),
        ),
        side(diff::INCOMING_HUE),
    ]
}

pub struct Conflicts {
    weak: Weak<Conflicts>,
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    /// Off while a comparison is up, which lays its own tints and buttons over the same lines.
    enabled: Cell<bool>,
    /// Where each block the last find saw starts, in characters.
    starts: RefCell<Vec<i32>>,
    /// One row of buttons per block, the `i`th serving the `i`th block. Kept, as a comparison
    /// keeps its own (`diff::Pool`): GTK has no way to take an overlay back off a text view, so a
    /// row no block needs is hidden.
    rows: RefCell<Vec<gtk::Widget>>,
    pending: RefCell<Option<glib::SourceId>>,
}

impl Conflicts {
    /// `scroller` is the view's own, whose adjustments say when GTK has reflowed the lines.
    pub fn new(
        view: &sourceview5::View,
        buffer: &sourceview5::Buffer,
        scroller: &gtk::ScrolledWindow,
    ) -> Rc<Conflicts> {
        let table = buffer.tag_table();
        for name in TINTS.into_iter().chain([BAND]) {
            table.add(&gtk::TextTag::new(Some(name)));
        }
        let this = Rc::new_cyclic(|weak| Conflicts {
            weak: weak.clone(),
            view: view.clone(),
            buffer: buffer.clone(),
            enabled: Cell::new(true),
            starts: RefCell::default(),
            rows: RefCell::default(),
            pending: RefCell::default(),
        });
        // A new height below or above a block (lines laid out, a zoom) and a new width (wrapping)
        // both move its first line.
        let weak = this.weak.clone();
        scroller.vadjustment().connect_upper_notify(move |_| {
            if let Some(c) = weak.upgrade() {
                c.schedule_lay();
            }
        });
        let weak = this.weak.clone();
        scroller.hadjustment().connect_page_size_notify(move |_| {
            if let Some(c) = weak.upgrade() {
                c.schedule_lay();
            }
        });
        this
    }

    /// Find the blocks in the text as it stands and tint them, their buttons laid once GTK has
    /// laid the lines. Nothing to do for a text that had none and has none: the find is a search
    /// for `<<<<<<<` then.
    pub fn find(&self) {
        if !self.enabled.get() {
            return;
        }
        let (start, end) = self.buffer.bounds();
        let text = self.buffer.text(&start, &end, true);
        let blocks = conflict::blocks(&text);
        if blocks.is_empty() && self.starts.borrow().is_empty() {
            return;
        }
        self.unmark();
        let offsets = Offsets::new(&text);
        let tag = |name: &str, range: Range<usize>| {
            let at = |byte| self.buffer.iter_at_offset(offsets.char_of(byte));
            self.buffer
                .apply_tag_by_name(name, &at(range.start), &at(range.end));
        };
        for block in &blocks {
            let markers = block.markers();
            let (head, tail) = (&markers[0], &markers[markers.len() - 1]);
            tag(CURRENT_HEAD, head.clone());
            tag(CURRENT, block.ours.clone());
            if let Some(base) = &block.base {
                tag(BASE_HEAD, markers[1].clone());
                tag(BASE, base.clone());
            }
            tag(INCOMING, block.theirs.clone());
            tag(INCOMING_HEAD, tail.clone());
            // The line's own characters: a tag that ends at the next line's start is taken by
            // text typed there, which would give that line a band of its own.
            let len = text[head.clone()].trim_end_matches(['\n', '\r']).len();
            tag(BAND, head.start..head.start + len);
        }
        *self.starts.borrow_mut() = blocks
            .iter()
            .map(|block| offsets.char_of(block.range.start))
            .collect();
        self.schedule_lay();
    }

    /// Off while a comparison is up, and found again once it has gone.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.set(on);
        if on {
            return self.find();
        }
        self.unmark();
        self.starts.borrow_mut().clear();
        self.lay();
    }

    /// Take every tint off, and the band from the lines that have one. Only from those: a band is
    /// a height, and taking a tag like that off the whole buffer has GTK lay every line out again.
    fn unmark(&self) {
        let (start, end) = self.buffer.bounds();
        for name in TINTS {
            self.buffer.remove_tag_by_name(name, &start, &end);
        }
        let band = self.tag(BAND);
        for (from, to) in ranges(&self.buffer, &band) {
            let at = |offset| self.buffer.iter_at_offset(offset);
            self.buffer.remove_tag(&band, &at(from), &at(to));
        }
    }

    fn tag(&self, name: &str) -> gtk::TextTag {
        self.buffer
            .tag_table()
            .lookup(name)
            .expect("installed by Conflicts::new")
    }

    /// The tints from the resolved foreground, as a comparison's are: once the view is mapped
    /// and on every theme change.
    pub fn restyle(&self) {
        let tags = [
            (CURRENT, CURRENT_HEAD),
            (BASE, BASE_HEAD),
            (INCOMING, INCOMING_HEAD),
        ];
        for ((body, head), (tint, head_tint)) in tags.into_iter().zip(tints(self.view.color())) {
            self.tag(body).set_paragraph_background_rgba(Some(&tint));
            self.tag(head)
                .set_paragraph_background_rgba(Some(&head_tint));
        }
    }

    fn schedule_lay(&self) {
        if self.pending.borrow().is_some() {
            return;
        }
        let weak = self.weak.clone();
        let id = glib::idle_add_local_once(move || {
            if let Some(c) = weak.upgrade() {
                *c.pending.borrow_mut() = None;
                c.lay();
            }
        });
        *self.pending.borrow_mut() = Some(id);
    }

    /// Put each block's buttons at the top of its band, and give the band the buttons' height. A
    /// block folded away, or in a view that takes no edits, shows none.
    fn lay(&self) {
        let starts = self.starts.borrow();
        let editable = self.view.is_editable();
        let mut rows = self.rows.borrow_mut();
        while editable && rows.len() < starts.len() {
            let row = self.build(rows.len());
            rows.push(row);
        }
        let mut shown = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let at = starts
                .get(i)
                .filter(|_| editable)
                .map(|&at| self.buffer.iter_at_offset(at))
                .filter(|at| !at.tags().iter().any(|tag| tag.is_invisible()));
            row.set_visible(at.is_some());
            shown.extend(at.map(|at| (row, at)));
        }
        let Some((first, _)) = shown.first() else {
            return;
        };
        // A tag's spacing replaces the view's rather than adding to it, so the band is on top of
        // the view's own, which the zoom scales.
        let (_, height, _, _) = first.measure(gtk::Orientation::Vertical, -1);
        let above = self.view.pixels_above_lines() + height + 2 * GAP;
        let band = self.tag(BAND);
        if band.pixels_above_lines() != above {
            band.set_pixels_above_lines(above);
        }
        for (row, at) in shown {
            let (top, _) = self.view.line_yrange(&at);
            let x = self.view.iter_location(&at).x();
            self.view.move_overlay(row, x, top + GAP);
        }
    }

    /// The `i`th block's row, laid over the view.
    fn build(&self, i: usize) -> gtk::Widget {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("linked");
        row.add_css_class("osd");
        for (label, tip, take) in [
            ("Accept Current", "Keep the current change", Take::Current),
            (
                "Accept Incoming",
                "Keep the incoming change",
                Take::Incoming,
            ),
            (
                "Accept Both",
                "Keep both changes, the current one first",
                Take::Both,
            ),
        ] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("caption");
            button.set_tooltip_text(Some(tip));
            // The editor keeps the keyboard, so typing goes on where it was.
            button.set_focus_on_click(false);
            let weak = self.weak.clone();
            button.connect_clicked(move |_| {
                let Some(c) = weak.upgrade() else { return };
                let at = c.starts.borrow().get(i).copied();
                if let Some(at) = at {
                    c.resolve(|span| span.start == at, take);
                }
            });
            row.append(&button);
        }
        self.view.add_overlay(&row, 0, 0);
        row.upcast()
    }

    /// Resolve the block holding the caret with `take`. `false` when the caret is in none.
    pub fn accept_at_caret(&self, take: Take) -> bool {
        let caret = self.buffer.iter_at_mark(&self.buffer.get_insert()).offset();
        self.resolve(|span| span.contains(&caret), take)
    }

    /// Replace the first block `pick` accepts — handed each one's characters — with what `take`
    /// keeps of it, as one undo step, from the text as it stands rather than as the last find saw
    /// it. `false` when `pick` accepts none, or the view takes no edits.
    fn resolve(&self, pick: impl Fn(Range<i32>) -> bool, take: Take) -> bool {
        if !self.view.is_editable() {
            return false;
        }
        let (text, offsets, blocks) = self.read();
        let chars =
            |block: &Block| offsets.char_of(block.range.start)..offsets.char_of(block.range.end);
        let Some(block) = blocks.iter().find(|block| pick(chars(block))) else {
            return false;
        };
        let (kept, span) = (block.resolve(&text, take), chars(block));
        self.buffer.begin_user_action();
        let (mut from, mut to) = (
            self.buffer.iter_at_offset(span.start),
            self.buffer.iter_at_offset(span.end),
        );
        self.buffer.delete(&mut from, &mut to);
        self.buffer.insert(&mut from, &kept);
        self.buffer.end_user_action();
        // Above 16 KB the host finds the blocks again only on its debounce.
        self.find();
        true
    }

    /// Put the caret on the start of the next block after it, or the one before it with
    /// `forward` false, going round at the ends, and scroll there. `false` when there is none.
    pub fn step(&self, forward: bool) -> bool {
        let (_, offsets, blocks) = self.read();
        let starts: Vec<i32> = blocks
            .iter()
            .map(|block| offsets.char_of(block.range.start))
            .collect();
        let caret = self.buffer.iter_at_mark(&self.buffer.get_insert()).offset();
        let next = match forward {
            true => starts.iter().find(|&&at| at > caret).or(starts.first()),
            false => starts
                .iter()
                .rev()
                .find(|&&at| at < caret)
                .or(starts.last()),
        };
        let Some(&at) = next else {
            return false;
        };
        self.buffer.place_cursor(&self.buffer.iter_at_offset(at));
        self.view
            .scroll_to_mark(&self.buffer.get_insert(), 0.0, true, 0.0, 0.25);
        true
    }

    /// The text, its byte to character table and the blocks in it.
    fn read(&self) -> (String, Offsets, Vec<Block>) {
        let (start, end) = self.buffer.bounds();
        let text = self.buffer.text(&start, &end, true).to_string();
        let (offsets, blocks) = (Offsets::new(&text), conflict::blocks(&text));
        (text, offsets, blocks)
    }

    // --- for the bench ------------------------------------------------------------------------

    /// Each block's row as laid, in the view's own coordinates: the top of its line, the row's
    /// top and bottom, and where the line's text starts. `None` for a block showing no row.
    pub fn laid(&self) -> Vec<Option<[i32; 4]>> {
        let (starts, rows) = (self.starts.borrow(), self.rows.borrow());
        let window = |y: i32| {
            self.view
                .buffer_to_window_coords(gtk::TextWindowType::Widget, 0, y)
                .1
        };
        starts
            .iter()
            .enumerate()
            .map(|(i, &at)| {
                let row = rows.get(i).filter(|row| row.is_visible())?;
                let bounds = row.compute_bounds(&self.view)?;
                let at = self.buffer.iter_at_offset(at);
                Some([
                    window(self.view.line_yrange(&at).0),
                    bounds.y() as i32,
                    (bounds.y() + bounds.height()) as i32,
                    window(self.view.iter_location(&at).y()),
                ])
            })
            .collect()
    }

    /// The `i`th block's row of buttons.
    pub fn row(&self, i: usize) -> Option<gtk::Widget> {
        self.rows.borrow().get(i).cloned()
    }
}

/// Where `tag` is on in `buffer`, in characters. Read whole before anything is changed: taking a
/// tag off invalidates every iterator.
fn ranges(buffer: &sourceview5::Buffer, tag: &gtk::TextTag) -> Vec<(i32, i32)> {
    let mut found = Vec::new();
    let mut at = buffer.start_iter();
    loop {
        if at.starts_tag(Some(tag)) {
            let from = at.offset();
            at.forward_to_tag_toggle(Some(tag));
            found.push((from, at.offset()));
        }
        if !at.forward_to_tag_toggle(Some(tag)) {
            return found;
        }
    }
}
