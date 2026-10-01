//! Side-by-side panes: one `AdwTabBar` and one `AdwTabView` each, nested in `GtkPaned`s.
//!
//! There is no model of the layout beside the widget tree; the widget tree *is* the tree. A pane's
//! parent is either the `AdwBin` that holds the whole arrangement or a `GtkPaned`, so a split is
//! "replace the pane in its parent with a paned holding the pane and its new neighbour", and a
//! close is the same swap backwards. Nothing else has to be kept in step.
//!
//! Moving a tab between panes needs no code of ours: libadwaita's tab drag carries the
//! `AdwTabPage` itself, every `AdwTabBox` and every `AdwTabView` already accepts that type, and
//! `AdwTabView:is-transferring-page` says when one is in flight. What is added here is the drop
//! *zones*: an edge of a pane means "split", which libadwaita has no notion of.

use crate::find;
use crate::pdfview::Anchor;
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The class `main::install_chrome_css` paints the drop hint with.
const ZONE: &str = "accent-drop-zone";
/// How much of a pane's width or height each edge zone claims. A quarter is enough to aim at
/// without swallowing the middle, which is the far commoner drop.
const EDGE: f64 = 0.25;

/// What a preview tab carries in its indicator slot: an eye, since the tab is only being looked
/// at, and clicking it keeps the tab. A file from outside the vault has the slot already
/// (`App::mark_loose`), and that says more, so it keeps it.
const PREVIEW_ICON: &str = "view-reveal-symbolic";
const PREVIEW_TIP: &str = "Preview — Click to Keep";

/// What a pinned tab carries in the same slot. A mark rather than a button, unlike the eye:
/// unpinning is the tab menu's, not something a stray click on the tab's edge should do. The
/// outside-the-vault mark keeps the slot here too, since where a save goes matters more than
/// what the tab's place at the start of the bar already says.
const PINNED_ICON: &str = "view-pin-symbolic";
const PINNED_TIP: &str = "Pinned";

/// What `AdwTabView` keeps of its own chords. Six are taken away: `Ctrl+Tab` and
/// `Ctrl+Shift+Tab` are `win.next-tab` / `win.previous-tab`, which walk the tabs in the order
/// they were last used rather than along the bar, and both Home / End pairs go back to
/// GtkSourceView, whose document start and end — with the selection and without — are on
/// DESIGN.md's never-bind list. The widget's controller runs in the capture phase, so keeping
/// `Ctrl+Shift+Home` here meant a note could not be selected to its top at all.
///
/// What is left — `Ctrl+PageUp` / `Ctrl+PageDown`, the Shift variants that move a tab along the
/// bar, and `Alt+0` to `Alt+9` — is libadwaita's and stays there, being chords no action of ours
/// wants. [`CHORDS`] is what writes them down.
fn shortcuts() -> adw::TabViewShortcuts {
    adw::TabViewShortcuts::ALL_SHORTCUTS.difference(
        adw::TabViewShortcuts::CONTROL_TAB
            | adw::TabViewShortcuts::CONTROL_SHIFT_TAB
            | adw::TabViewShortcuts::CONTROL_HOME
            | adw::TabViewShortcuts::CONTROL_END
            | adw::TabViewShortcuts::CONTROL_SHIFT_HOME
            | adw::TabViewShortcuts::CONTROL_SHIFT_END,
    )
}

/// Every chord `AdwTabView` binds, with the flag that carries it and what it does, spelled the
/// way `main::ACTIONS` spells its accelerators. Exhaustive over `ALL_SHORTCUTS` — a test says so
/// — because a chord no table names is a chord the palette and the rebind dialog cannot see.
const CHORDS: &[(adw::TabViewShortcuts, &str, &str)] = &[
    (
        adw::TabViewShortcuts::CONTROL_TAB,
        "<Control>Tab",
        "Next Tab",
    ),
    (
        adw::TabViewShortcuts::CONTROL_SHIFT_TAB,
        "<Control><Shift>Tab",
        "Previous Tab",
    ),
    (
        adw::TabViewShortcuts::CONTROL_PAGE_UP,
        "<Control>Page_Up",
        "Previous Tab",
    ),
    (
        adw::TabViewShortcuts::CONTROL_PAGE_DOWN,
        "<Control>Page_Down",
        "Next Tab",
    ),
    (
        adw::TabViewShortcuts::CONTROL_HOME,
        "<Control>Home",
        "First Tab",
    ),
    (
        adw::TabViewShortcuts::CONTROL_END,
        "<Control>End",
        "Last Tab",
    ),
    (
        adw::TabViewShortcuts::CONTROL_SHIFT_PAGE_UP,
        "<Control><Shift>Page_Up",
        "Move Tab Back",
    ),
    (
        adw::TabViewShortcuts::CONTROL_SHIFT_PAGE_DOWN,
        "<Control><Shift>Page_Down",
        "Move Tab Forward",
    ),
    (
        adw::TabViewShortcuts::CONTROL_SHIFT_HOME,
        "<Control><Shift>Home",
        "Move Tab to Start",
    ),
    (
        adw::TabViewShortcuts::CONTROL_SHIFT_END,
        "<Control><Shift>End",
        "Move Tab to End",
    ),
    // One flag, nine chords.
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>1", "Tab 1"),
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>2", "Tab 2"),
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>3", "Tab 3"),
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>4", "Tab 4"),
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>5", "Tab 5"),
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>6", "Tab 6"),
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>7", "Tab 7"),
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>8", "Tab 8"),
    (adw::TabViewShortcuts::ALT_DIGITS, "<Alt>9", "Tab 9"),
    (adw::TabViewShortcuts::ALT_ZERO, "<Alt>0", "Last Tab"),
];

/// The chords the tab bar still holds, each with the name of what it does there. The rebind
/// dialog refuses them by that name, and a test keeps `ACTIONS` and the never-bind list off them.
pub fn widget_chords() -> Vec<(&'static str, &'static str)> {
    let kept = shortcuts();
    CHORDS
        .iter()
        .filter(|(flag, _, _)| kept.contains(*flag))
        .map(|(_, accel, what)| (*accel, *what))
        .collect()
}

/// The order `Ctrl+Tab` walks and a close falls back to: `history` filtered down to the pages
/// that are still here, then any page that has never been selected, in the order the bar shows
/// them.
///
/// Pure, so both of the things it decides — which tab a close lands on and which tab a step of
/// `Ctrl+Tab` lands on — are testable with no display.
pub fn recent_order<T: Clone + PartialEq>(history: &[T], live: &[T]) -> Vec<T> {
    let mut order: Vec<T> = history
        .iter()
        .filter(|p| live.contains(p))
        .cloned()
        .collect();
    let rest: Vec<T> = live
        .iter()
        .filter(|p| !order.contains(p))
        .cloned()
        .collect();
    order.extend(rest);
    order
}

/// Where a cursor at `at` in an order of `len` tabs lands after one `Ctrl+Tab`. Wraps, so a chord
/// held past the end of the list comes round to the tab it started on.
///
/// Pure, and the whole of what a held chord decides: the order it walks is [`recent_order`]'s and
/// does not move until the chord ends.
pub fn cycle_to(len: usize, at: usize, forward: bool) -> usize {
    match len {
        0 => 0,
        len if forward => (at + 1) % len,
        len => (at + len - 1) % len,
    }
}

/// `item` to the front of `order`: what selecting a tab does to the most recently used list.
pub fn to_front<T: Clone + PartialEq>(order: &mut Vec<T>, item: &T) {
    order.retain(|p| p != item);
    order.insert(0, item.clone());
}

/// Where a tab at `at` belongs in a bar holding `pinned` pinned tabs besides it, which are always
/// its first ones: a pinned tab no further right than the end of that group, any other no further
/// left. Pure, and the whole of pinning's order — pinning or unpinning a tab is flipping its state
/// and asking this, which lands it on the boundary; so is any move that crossed it.
pub fn pinned_slot(at: usize, pinned: usize, is_pinned: bool) -> usize {
    match is_pinned {
        true => at.min(pinned),
        false => at.max(pinned),
    }
}

/// Put the pin in `page`'s indicator slot, or take it out. A slot something else holds is left to
/// it (see [`PINNED_ICON`]).
pub fn mark_pinned(page: &adw::TabPage, pinned: bool) {
    if pinned && page.indicator_icon().is_none() {
        page.set_indicator_icon(Some(&gio::ThemedIcon::new(PINNED_ICON)));
        page.set_indicator_tooltip(PINNED_TIP);
    } else if !pinned && page.indicator_tooltip() == PINNED_TIP {
        page.set_indicator_icon(None::<&gio::Icon>);
        page.set_indicator_tooltip("");
    }
}

// --- back and forward ----------------------------------------------------------------------

/// How many places back the reader can go. A navigation history is not an undo stack; a hundred
/// is far past what anyone follows in one sitting. The PDF reader's number, now the pane's.
const HISTORY: usize = 100;
/// How far apart two lines are before they are two places rather than one. A paragraph typed in
/// one sitting leaves one mark, not one per keystroke.
const NEARBY: i32 = 10;
/// How long a pause makes the same paragraph a second visit. Long enough to think mid-sentence,
/// short enough that coming back to a note tomorrow is a place of its own.
const COALESCE: Duration = Duration::from_secs(10);

/// Where in a document a [`Place`] is. The smallest thing that covers both kinds of reader: a
/// caret, and a PDF's [`Anchor`]. Everything else — an image, a diff, a shell — has one position
/// and that is the whole document.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Spot {
    /// A 1-based line and column, as [`crate::editor::Tab::goto_line`] takes them.
    Caret(i32, i32),
    Page(Anchor),
    /// A diagram's page, by its index.
    Sheet(usize),
    Whole,
}

/// Somewhere the reader has been: which document, and where in it.
///
/// The document is named by its `Doc::key` rather than held as a page, so an entry survives the
/// tab moving along the bar and costs nothing to compare.
#[derive(Clone, PartialEq, Debug)]
pub struct Place {
    pub key: String,
    pub at: Spot,
}

/// Whether `next` merges into `last` instead of being pushed behind it: the same document, near
/// enough, and recorded within [`COALESCE`] of it — or exactly the same place, however long ago,
/// which is what stops a jump recording the spot it starts from twice.
///
/// Pure, and the whole of the coalescing rule: `apart` is how long ago the last entry was made.
pub fn coalesces(last: &Place, next: &Place, apart: Duration) -> bool {
    if last.key != next.key {
        return false;
    }
    match (last.at, next.at) {
        (Spot::Caret(a, _), Spot::Caret(b, _)) => {
            (a - b).abs() <= NEARBY && (apart <= COALESCE || last.at == next.at)
        }
        // A page is as near as a PDF's places get: the fractions move with every scroll.
        (Spot::Page(a), Spot::Page(b)) => a.page == b.page,
        (Spot::Sheet(a), Spot::Sheet(b)) => a == b,
        (Spot::Whole, Spot::Whole) => true,
        _ => false,
    }
}

/// A pane's back/forward history: the places the reader has left, and the ones Back took them
/// from. One per pane, so Back may switch tabs but never moves the keyboard to another pane.
#[derive(Default)]
pub struct Nav {
    back: Vec<Place>,
    forward: Vec<Place>,
    /// When the top of `back` was recorded, which is what [`coalesces`] measures against.
    at: Option<Instant>,
}

impl Nav {
    /// Record `from` as somewhere to come back to. A place that coalesces with the last one
    /// replaces it, so typing a paragraph leaves one mark rather than one per keystroke.
    pub fn record(&mut self, from: Place, now: Instant) {
        let apart = self
            .at
            .map_or(Duration::MAX, |then| now.saturating_duration_since(then));
        match self.back.last_mut() {
            Some(last) if coalesces(last, &from, apart) => *last = from,
            _ => {
                self.back.push(from);
                if self.back.len() > HISTORY {
                    self.back.remove(0);
                }
            }
        }
        self.at = Some(now);
        // Going somewhere new is what ends the branch Back opened, as it is in a browser.
        self.forward.clear();
    }

    /// Back: where to go, given that the reader is at `here`.
    pub fn back(&mut self, here: Place) -> Option<Place> {
        let to = self.back.pop()?;
        self.forward.push(here);
        // The entry now on top is older than whatever was recorded last, so nothing may coalesce
        // into it on the strength of a timestamp that belonged to the entry just taken off.
        self.at = None;
        Some(to)
    }

    pub fn forward(&mut self, here: Place) -> Option<Place> {
        let to = self.forward.pop()?;
        self.back.push(here);
        self.at = None;
        Some(to)
    }

    /// How many places are behind and ahead. What the `ACCENT_BENCH_LAYOUT` drill reads to say
    /// that a restore wrote none of its own steps into the history.
    #[cfg(feature = "bench")]
    pub fn depth(&self) -> (usize, usize) {
        (self.back.len(), self.forward.len())
    }

    /// A tab has closed: its places go with it, because there is nothing left to go back into.
    pub fn forget(&mut self, key: &str) {
        self.back.retain(|p| p.key != key);
        self.forward.retain(|p| p.key != key);
    }

    /// The pages of the PDF `key` were edited: each place in it goes where `map` takes its anchor,
    /// so Back returns to the page it left and not to whatever took that page's number.
    pub fn repage(&mut self, key: &str, map: impl Fn(Anchor) -> Anchor) {
        for place in self.back.iter_mut().chain(&mut self.forward) {
            if let (true, Spot::Page(anchor)) = (place.key == key, &mut place.at) {
                *anchor = map(*anchor);
            }
        }
    }
}

/// Which side of a pane a new pane goes on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

impl Side {
    /// The suffix of the `win.split-*` action that asks for this side.
    pub fn action(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
            Side::Up => "up",
            Side::Down => "down",
        }
    }
}

/// Where in a pane a drop landed: an edge splits it, the middle drops into it as it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Here,
    Split(Side),
}

/// The zone a pointer at `(x, y)` is in, for a pane of `w` by `h`.
///
/// The nearest edge wins, so the corners belong to whichever side the pointer is closer to
/// rather than to neither.
pub fn zone(x: f64, y: f64, w: f64, h: f64) -> Zone {
    if w <= 0.0 || h <= 0.0 {
        return Zone::Here;
    }
    let edges = [
        (x / w, Side::Left),
        ((w - x) / w, Side::Right),
        (y / h, Side::Up),
        ((h - y) / h, Side::Down),
    ];
    let nearest = edges
        .into_iter()
        .reduce(|best, edge| if edge.0 < best.0 { edge } else { best });
    match nearest {
        Some((share, side)) if share < EDGE => Zone::Split(side),
        _ => Zone::Here,
    }
}

/// How a split arranges the two panes: the paned's orientation, and whether the new pane takes
/// the start child.
pub fn arrange(side: Side) -> (gtk::Orientation, bool) {
    match side {
        Side::Left => (gtk::Orientation::Horizontal, true),
        Side::Right => (gtk::Orientation::Horizontal, false),
        Side::Up => (gtk::Orientation::Vertical, true),
        Side::Down => (gtk::Orientation::Vertical, false),
    }
}

/// How far one Move Divider step goes, as a share of the split.
const DIVIDER_STEP: f64 = 0.05;

/// Where a divider at `position` in a split `extent` long goes one step towards the end
/// (`forward`) or the start: the next stop on a grid of [`DIVIDER_STEP`]s anchored at the centre,
/// which is where a split starts and where a double-click puts it back. So a divider the pointer
/// left off the grid lands on it, and the centre is always a stop. A position within a pixel of a
/// stop is that stop, a share of the split having been rounded to whole pixels.
///
/// Pure; keeping the answer inside what the split allows is the caller's.
pub fn divider_step(position: i32, extent: i32, forward: bool) -> i32 {
    let step = DIVIDER_STEP * f64::from(extent);
    if step <= 0.0 {
        return position;
    }
    let centre = f64::from(extent) / 2.0;
    let at = (f64::from(position) - centre) / step;
    let pixel = 1.0 / step;
    let stop = match forward {
        true => (at + pixel).floor() + 1.0,
        false => (at - pixel).ceil() - 1.0,
    };
    (centre + stop * step).round() as i32
}

/// The pane on `side` of `from`, as an index into `others`: the nearest one that starts at or
/// past `from`'s far edge on that axis and faces it on the other. Ties — two panes stacked
/// against the same edge — go to whichever faces more of `from`.
///
/// Geometry rather than a walk up the `GtkPaned` ancestors, because the widget tree says which
/// splits nest and not which pane a reader would call "the one on the left". `others` may hold
/// `from` itself: it cannot be past its own far edge, so it never matches. An unallocated pane
/// has no neighbours, as it has no edges in [`zone`].
pub fn neighbour(from: graphene::Rect, others: &[graphene::Rect], side: Side) -> Option<usize> {
    if from.width() <= 0.0 || from.height() <= 0.0 {
        return None;
    }
    // Every rect reduces to two spans: the one the side steps along, and the one it has to face.
    // Left is the mirror of right and up of down, so turning the axis around is all that
    // separates the four cases.
    let (horizontal, forward) = match side {
        Side::Left => (true, false),
        Side::Right => (true, true),
        Side::Up => (false, false),
        Side::Down => (false, true),
    };
    let across = |r: &graphene::Rect| match horizontal {
        true => (r.y(), r.y() + r.height()),
        false => (r.x(), r.x() + r.width()),
    };
    // With the axis pointing the way `side` does, the near end is the smaller of the two.
    let ends = |r: &graphene::Rect| {
        let (a, b) = match horizontal {
            true => (r.x(), r.x() + r.width()),
            false => (r.y(), r.y() + r.height()),
        };
        match forward {
            true => (a, b),
            false => (-b, -a),
        }
    };
    let far = ends(&from).1;
    let (lo, hi) = across(&from);
    others
        .iter()
        .enumerate()
        .filter(|(_, r)| r.width() > 0.0 && r.height() > 0.0)
        .filter_map(|(i, r)| {
            let near = ends(r).0;
            let (a, b) = across(r);
            let overlap = hi.min(b) - lo.max(a);
            (near >= far && overlap > 0.0).then_some((i, near, overlap))
        })
        // Nearest first; among equals, the one facing more of `from`.
        .min_by(|a, b| a.1.total_cmp(&b.1).then(b.2.total_cmp(&a.2)))
        .map(|(i, _, _)| i)
}

/// One pane: its tab bar, its tab view, and the sheet that shows where a drop would land.
pub struct Pane {
    /// Tab bar, find bar, document. This is what the paneds hold.
    column: gtk::Box,
    pub bar: adw::TabBar,
    /// Find, replace and go to line for this pane's document alone, so a split can search two
    /// notes at once. Revealed between the tab bar and the document rather than over it.
    pub find: Rc<find::Bar>,
    pub tabs: adw::TabView,
    overlay: gtk::Overlay,
    /// Covers the whole document while a drag is in flight, and carries [`Pane::drop`]. It is an
    /// overlay child, so it is picked before the tab view, which has a tab drop target of its own
    /// that would otherwise take every drop as a plain "move it here".
    hint: gtk::Box,
    /// The lit rectangle inside `hint`. A child rather than the hint itself, so highlighting a
    /// zone never moves the widget the pointer coordinates are measured against.
    shade: gtk::Box,
    pub drop: gtk::DropTarget,
    /// This pane's tabs in the order they were last selected, most recent first.
    ///
    /// Pruned against the pane's live pages on every read, which is what lets a tab dragged into
    /// another pane or another window need no bookkeeping of its own: it simply stops being one
    /// of this pane's pages, and joins the other pane's history the moment it is selected there.
    history: RefCell<Vec<adw::TabPage>>,
    /// How deep into `history` a held `Ctrl+Tab` has walked, `None` when no chord is in flight.
    /// See [`Pane::step`].
    cycling: Cell<Option<usize>>,
    /// Back and forward across this pane's documents. See [`Nav`].
    pub nav: RefCell<Nav>,
    /// The preview tab, if this pane has one: the tab a single click in the sidebar or a followed
    /// link opened, which the next such open replaces instead of piling up beside it.
    preview: RefCell<Option<adw::TabPage>>,
}

impl Pane {
    pub fn new(menu: &gio::Menu) -> Rc<Pane> {
        let tabs = adw::TabView::builder().hexpand(true).vexpand(true).build();
        tabs.set_menu_model(Some(menu));
        tabs.set_shortcuts(shortcuts());
        let bar = adw::TabBar::builder().view(&tabs).build();
        // The bar used to be one of the toolbar view's top bars, which draw flat on their own.
        // Inside the document column it needs `.inline` to stop painting a header-bar background
        // over the one flat surface (DESIGN.md, Colour).
        bar.add_css_class("inline");
        bar.add_css_class("chrome-fade");

        let shade = gtk::Box::builder().hexpand(true).vexpand(true).build();
        shade.set_visible(false);
        let hint = gtk::Box::new(gtk::Orientation::Vertical, 0);
        hint.append(&shade);
        // Mapped from the start and merely untargetable until a drag, rather than shown when one
        // begins. A widget that appears mid-drag has to be mapped and allocated before GTK can
        // pick it, and on Wayland that never happened: the drop zones took no drags at all, while
        // X11 picked the sheet on the next motion and worked. `can-target` is a flag on a widget
        // that is already laid out, so there is nothing to wait for. It draws nothing while it is
        // off, and `gtk_widget_pick` skips its children too, so clicks still reach the editor.
        hint.set_can_target(false);
        let drop = gtk::DropTarget::new(
            glib::Type::INVALID,
            gdk::DragAction::MOVE | gdk::DragAction::COPY,
        );
        drop.set_types(&[adw::TabPage::static_type(), String::static_type()]);
        hint.add_controller(drop.clone());

        let overlay = gtk::Overlay::builder().child(&tabs).build();
        overlay.add_overlay(&hint);

        let find = find::Bar::new();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&bar);
        column.append(find.widget());
        column.append(&overlay);
        // A pane that is not being written in recedes at High (`App::hide_chrome`).
        column.add_css_class("chrome-fade");

        let pane = Rc::new(Pane {
            column,
            bar,
            find,
            tabs,
            overlay,
            hint,
            shade,
            drop,
            history: RefCell::new(Vec::new()),
            cycling: Cell::new(None),
            nav: RefCell::new(Nav::default()),
            preview: RefCell::new(None),
        });
        pane.wire_preview();
        pane
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.column.upcast_ref()
    }

    /// Put the find bar back between the tab bar and the document, after presentation mode has
    /// borrowed it (`App::hoist_find`). A no-op while it is already there.
    pub fn hold_find(&self) {
        let bar = self.find.widget();
        if bar.parent().as_ref() == Some(self.widget()) {
            return;
        }
        if let Some(old) = bar.parent().and_downcast::<gtk::Box>() {
            old.remove(bar);
        }
        self.column.insert_child_after(bar, Some(&self.bar));
    }

    /// The document area, whose size the drop zones are measured in.
    pub fn size(&self) -> (f64, f64) {
        (
            f64::from(self.overlay.width()),
            f64::from(self.overlay.height()),
        )
    }

    /// This pane's pages, in the order the bar shows them.
    pub fn pages(&self) -> Vec<adw::TabPage> {
        let pages = self.tabs.pages();
        (0..pages.n_items())
            .filter_map(|i| pages.item(i))
            .filter_map(|item| item.downcast::<adw::TabPage>().ok())
            .collect()
    }

    /// Whether `page` is one of this pane's.
    pub fn has(&self, page: &adw::TabPage) -> bool {
        self.pages().contains(page)
    }

    // --- most recently used ----------------------------------------------------------------

    /// This pane's tabs, the one selected most recently first.
    pub fn recent(&self) -> Vec<adw::TabPage> {
        recent_order(&self.history.borrow(), &self.pages())
    }

    /// Remember that `page` was just selected. Pruning happens here too, so a page that has left
    /// the pane is out of the history by the next question anyone asks of it.
    ///
    /// While a `Ctrl+Tab` chord is held the order is left alone: the step this made arrives here
    /// through the selection notify and must not rewrite the list it is walking. A selection from
    /// anywhere else ends the chord, which is what stops a modifier release that never arrives —
    /// focus lost, the window unmapped mid-chord — from stranding the pane in cycling state.
    pub fn touch(&self, page: &adw::TabPage) {
        if let Some(at) = self.cycling.get() {
            if self.recent().get(at) == Some(page) {
                return;
            }
            self.cycling.set(None);
        }
        let mut order = self.recent();
        to_front(&mut order, page);
        *self.history.borrow_mut() = order;
    }

    /// Whether `page` is where a held `Ctrl+Tab` has just stepped, as opposed to a tab picked any
    /// other way.
    pub fn stepping_to(&self, page: &adw::TabPage) -> bool {
        self.cycling
            .get()
            .is_some_and(|at| self.recent().get(at) == Some(page))
    }

    /// Which tab to show once `page` goes: the most recently used one that is left.
    pub fn survivor(&self, page: &adw::TabPage) -> Option<adw::TabPage> {
        self.recent().into_iter().find(|p| p != page)
    }

    /// One step of `Ctrl+Tab`: one deeper into the order the tabs were last used in, the order
    /// itself left alone until the chord ends. Three presses are three tabs back, and
    /// `Ctrl+Shift+Tab` walks the same cursor the other way.
    ///
    /// ponytail: nothing is shown on screen while the chord is held. This is the cheap half of
    /// VS Code's idiom — the deferred reorder without the modal overlay that lists the tabs and
    /// says where the cursor is, which is a widget, a keyboard grab and a paint of its own. The
    /// overlay is the upgrade path; the order it would list is [`Pane::recent`] and the cursor it
    /// would highlight is `cycling`, so it is a view over what is already here.
    pub fn step(&self, forward: bool) -> Option<adw::TabPage> {
        let order = self.recent();
        let at = cycle_to(order.len(), self.cycling.get().unwrap_or(0), forward);
        self.cycling.set(Some(at));
        order.get(at).cloned()
    }

    /// Ctrl came up: the tab the chord landed on is the most recently used one now. A no-op when
    /// no chord is in flight, which is what every other Ctrl release is.
    pub fn end_cycle(&self) {
        if self.cycling.take().is_none() {
            return;
        }
        if let Some(page) = self.tabs.selected_page() {
            self.touch(&page);
        }
    }

    // --- preview tabs ----------------------------------------------------------------------

    /// The tab that is only being looked at, if this pane still holds it. A tab that leaves the
    /// pane is kept on its way out (`Pane::wire_preview`), and that is the whole of "moving a
    /// tab makes it a real one".
    pub fn preview(&self) -> Option<adw::TabPage> {
        let page = self.preview.borrow().clone()?;
        self.has(&page).then_some(page)
    }

    /// Make `page` this pane's preview, and hand back whichever tab it replaces.
    pub fn set_preview(&self, page: &adw::TabPage) -> Option<adw::TabPage> {
        let old = self.preview();
        *self.preview.borrow_mut() = Some(page.clone());
        if page.indicator_icon().is_none() {
            page.set_indicator_icon(Some(&gio::ThemedIcon::new(PREVIEW_ICON)));
            page.set_indicator_tooltip(PREVIEW_TIP);
            page.set_indicator_activatable(true);
        }
        old.filter(|old| old != page)
    }

    /// `page` is a real tab now: it was edited, its own tab was double-clicked, its eye was
    /// clicked, or it left the pane.
    pub fn keep(&self, page: &adw::TabPage) {
        let mut slot = self.preview.borrow_mut();
        if slot.as_ref() == Some(page) {
            *slot = None;
            // The eye and nothing else: an outside-the-vault mark stays where it is.
            if page.indicator_tooltip() == PREVIEW_TIP {
                page.set_indicator_icon(None::<&gio::Icon>);
                page.set_indicator_tooltip("");
                page.set_indicator_activatable(false);
            }
        }
    }

    /// The two ways out of being a preview that the tab view reports itself: a click on the eye,
    /// and the page leaving this pane, whether into another pane, another window or a drag.
    fn wire_preview(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.tabs.connect_indicator_activated(move |_, page| {
            if let Some(pane) = weak.upgrade() {
                pane.keep(page);
            }
        });
        let weak = Rc::downgrade(self);
        self.tabs.connect_page_detached(move |_, page, _| {
            if let Some(pane) = weak.upgrade() {
                pane.keep(page);
                // Out of the history now rather than at the next selection: a page holds its
                // whole document, and the tab a close selects is picked while the page is still
                // here, so a closed tab stayed in memory until the reader next switched tabs.
                pane.history.borrow_mut().retain(|p| p != page);
            }
        });
    }

    /// Call `f` when one of this pane's tabs is double-clicked, with the page that was clicked.
    ///
    /// `AdwTabBox` claims the press for its own selection and drag, and a claimed sequence cancels
    /// gestures but not raw event controllers — the lesson [`crate::paned`] records — so this is a
    /// capture-phase `GtkEventControllerLegacy` like that one, and it never swallows the event.
    /// The page is the pane's selected one, because the first press of the pair has already
    /// selected whichever tab it landed on.
    pub fn on_tab_double_click(self: &Rc<Self>, f: impl Fn(&adw::TabPage) + 'static) {
        let controller = gtk::EventControllerLegacy::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let last: RefCell<Option<crate::paned::Click>> = RefCell::new(None);
        controller.connect_event(glib::clone!(
            #[weak(rename_to = pane)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, event| {
                if let Some(now) = primary_press(event) {
                    let settings = pane.bar.settings();
                    let within_ms = u32::try_from(settings.gtk_double_click_time()).unwrap_or(400);
                    let within_px = f64::from(settings.gtk_double_click_distance());
                    if let Some(first) = last.replace(Some(now))
                        && crate::paned::is_double(first, now, within_ms, within_px)
                    {
                        // A third press starts a new pair rather than promoting again.
                        *last.borrow_mut() = None;
                        if let Some(page) = pane.tabs.selected_page() {
                            f(&page);
                        }
                    }
                }
                glib::Propagation::Proceed
            }
        ));
        self.bar.add_controller(controller);
    }

    /// Take the drop sheet in or out of the picture. It stays mapped either way and only stops
    /// being targetable, because a widget mapped mid-drag is not picked on every backend; while
    /// it is off it draws nothing and every click reaches the editor underneath.
    pub fn set_drop_active(&self, on: bool) {
        self.hint.set_can_target(on);
        if !on {
            self.show_zone(None);
        }
    }

    /// Light up the part of the pane a drop would land in, or nothing.
    pub fn show_zone(&self, zone: Option<Zone>) {
        let Some(zone) = zone else {
            self.shade.set_visible(false);
            return;
        };
        let (w, h) = (self.overlay.width(), self.overlay.height());
        let (start, end, top, bottom) = match zone {
            Zone::Here => (0, 0, 0, 0),
            Zone::Split(Side::Left) => (0, w / 2, 0, 0),
            Zone::Split(Side::Right) => (w / 2, 0, 0, 0),
            Zone::Split(Side::Up) => (0, 0, 0, h / 2),
            Zone::Split(Side::Down) => (0, 0, h / 2, 0),
        };
        self.shade.set_margin_start(start);
        self.shade.set_margin_end(end);
        self.shade.set_margin_top(top);
        self.shade.set_margin_bottom(bottom);
        self.shade.add_css_class(ZONE);
        self.shade.set_visible(true);
    }
}

/// A primary-button press, in the coordinates the raw event carries. Both presses of a pair are
/// measured in the same frame, so unlike [`crate::paned`] there is no surface transform to undo.
fn primary_press(event: &gdk::Event) -> Option<crate::paned::Click> {
    if event.event_type() != gdk::EventType::ButtonPress {
        return None;
    }
    let button = event.downcast_ref::<gdk::ButtonEvent>()?;
    if button.button() != gdk::BUTTON_PRIMARY {
        return None;
    }
    let (x, y) = event.position()?;
    Some(crate::paned::Click {
        x,
        y,
        time: event.time(),
    })
}

/// Put `new` beside `pane` on `side`, by replacing `pane` in its parent with a paned holding
/// both. The handle starts at the middle of what the old pane had, which is the size it still
/// has at this point.
pub fn split(pane: &Pane, new: &Pane, side: Side) {
    let Some(parent) = pane.column.parent() else {
        return tracing::warn!("splitting a pane that is not in the window");
    };
    let (orientation, new_first) = arrange(side);
    let extent = match orientation {
        gtk::Orientation::Vertical => pane.column.height(),
        _ => pane.column.width(),
    };
    let paned = gtk::Paned::builder()
        .orientation(orientation)
        .resize_start_child(true)
        .resize_end_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .position(extent / 2)
        .build();
    // The paned goes in first, which unparents the old column; then both columns go into it.
    replace(&parent, pane.widget(), paned.upcast_ref());
    let (first, second) = match new_first {
        true => (new.widget(), pane.widget()),
        false => (pane.widget(), new.widget()),
    };
    paned.set_start_child(Some(first));
    paned.set_end_child(Some(second));
}

/// Take `pane` out of the window, leaving its neighbour where the two of them were.
pub fn detach(pane: &Pane) {
    let Some(paned) = pane.column.parent().and_downcast::<gtk::Paned>() else {
        // The only pane left is held by the bin directly and has nothing to collapse into.
        return;
    };
    let sibling = match paned.start_child().as_ref() == Some(pane.widget()) {
        true => paned.end_child(),
        false => paned.start_child(),
    };
    let (Some(sibling), Some(grandparent)) = (sibling, paned.parent()) else {
        return;
    };
    paned.set_start_child(gtk::Widget::NONE);
    paned.set_end_child(gtk::Widget::NONE);
    replace(&grandparent, paned.upcast_ref(), &sibling);
}

/// Swap one child of a pane tree node for another. A node is either the bin at the root or a
/// paned; nothing else ever holds a pane column.
fn replace(parent: &gtk::Widget, old: &gtk::Widget, new: &gtk::Widget) {
    if let Some(bin) = parent.downcast_ref::<adw::Bin>() {
        bin.set_child(Some(new));
    } else if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        match paned.start_child().as_ref() == Some(old) {
            true => paned.set_start_child(Some(new)),
            false => paned.set_end_child(Some(new)),
        }
    } else {
        tracing::warn!("a pane in a {}, which holds no panes", parent.type_());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_middle_of_a_pane_is_not_a_split() {
        assert_eq!(zone(200.0, 150.0, 400.0, 300.0), Zone::Here);
        // Just inside the quarter on every side.
        assert_eq!(zone(390.0, 150.0, 400.0, 300.0), Zone::Split(Side::Right));
        assert_eq!(zone(10.0, 150.0, 400.0, 300.0), Zone::Split(Side::Left));
        assert_eq!(zone(200.0, 10.0, 400.0, 300.0), Zone::Split(Side::Up));
        assert_eq!(zone(200.0, 290.0, 400.0, 300.0), Zone::Split(Side::Down));
    }

    #[test]
    fn a_corner_belongs_to_the_edge_it_is_nearest() {
        // 10 px from the left and 20 from the top, in a pane that is wide and short: 1/80 of the
        // width beats 1/10 of the height, so the left edge is the nearer one. Turn the pane on its
        // side and the answer turns with it.
        assert_eq!(zone(10.0, 20.0, 800.0, 200.0), Zone::Split(Side::Left));
        assert_eq!(zone(10.0, 20.0, 200.0, 800.0), Zone::Split(Side::Up));
    }

    #[test]
    fn an_unallocated_pane_has_no_edges() {
        assert_eq!(zone(0.0, 0.0, 0.0, 0.0), Zone::Here);
    }

    /// What `Pane::step` does, over three tabs: one step of a chord that is not being held, so
    /// the cursor starts at the selected tab each time.
    fn step<'a>(order: &[&'a str], forward: bool) -> Option<&'a str> {
        let order = recent_order(order, &["A", "B", "C"]);
        order.get(cycle_to(order.len(), 0, forward)).copied()
    }

    /// NOTEPAD's own example: tabs A, B and C, A opened first, then a jump to C. Closing C shows
    /// A, where the reader was before it, not the neighbour B that `AdwTabView` would pick.
    #[test]
    fn a_close_falls_back_to_the_last_tab_used() {
        let history = ["C", "A", "B"];
        let order = recent_order(&history, &["A", "B", "C"]);
        assert_eq!(order, ["C", "A", "B"]);
        assert_eq!(order.iter().find(|p| **p != "C"), Some(&"A"));
    }

    #[test]
    fn a_tab_never_selected_still_cycles() {
        // Restored from a session: only the active note has ever been selected, and the rest keep
        // the order the bar shows them in rather than dropping out of the cycle.
        assert_eq!(recent_order(&["C"], &["A", "B", "C"]), ["C", "A", "B"]);
        assert_eq!(recent_order::<&str>(&[], &["A", "B"]), ["A", "B"]);
        // A tab that has left the pane is not in the order, whatever the history still says.
        assert_eq!(recent_order(&["Z", "B"], &["A", "B"]), ["B", "A"]);
    }

    #[test]
    fn ctrl_tab_steps_one_off_the_front_and_back_one_off_the_end() {
        // Forward is the tab used before this one, which is the switch between two notes.
        assert_eq!(step(&["C", "A", "B"], true), Some("A"));
        // Back wraps to the least recently used one.
        assert_eq!(step(&["C", "A", "B"], false), Some("B"));
        // With two tabs both directions are the other one, and with one there is nowhere to go.
        assert_eq!(step(&["A", "B"], true), Some("B"));
        assert_eq!(recent_order(&["A"], &["A"]).get(1), None);
    }

    /// The reorder is deferred while Ctrl is held, so each press goes one tab deeper instead of
    /// coming straight back — the whole of what `Pane::step` and `Pane::end_cycle` decide.
    #[test]
    fn a_held_chord_walks_deeper_and_commits_once() {
        let live = ["A", "B", "C", "D"];
        // D is in front, then C, then B, then A.
        let mut history = vec!["D", "C", "B", "A"];
        let order = recent_order(&history, &live);

        // Three presses, the order untouched between them: three tabs back, not a flip.
        let mut at = 0;
        for landed in ["C", "B", "A"] {
            at = cycle_to(order.len(), at, true);
            assert_eq!(order[at], landed);
        }
        // Ctrl+Shift+Tab walks the same cursor the other way.
        assert_eq!(order[cycle_to(order.len(), at, false)], "B");
        // A fourth press comes round to where the chord started rather than running out.
        assert_eq!(order[cycle_to(order.len(), at, true)], "D");

        // Ctrl up: the tab landed on goes to the front, and nothing else moves.
        to_front(&mut history, &order[at]);
        assert_eq!(history, ["A", "D", "C", "B"]);
    }

    fn caret(key: &str, line: i32) -> Place {
        Place {
            key: key.to_string(),
            at: Spot::Caret(line, 1),
        }
    }

    /// Typing a paragraph is one place to come back to, not one per keystroke; a jump away from
    /// it is not a second copy of the same place either.
    #[test]
    fn edits_in_one_paragraph_coalesce() {
        let quick = Duration::from_millis(80);
        // The same note, three lines apart, one keystroke after another.
        assert!(coalesces(&caret("a.md", 10), &caret("a.md", 13), quick));
        // Far enough down the note to be somewhere else.
        assert!(!coalesces(&caret("a.md", 10), &caret("a.md", 30), quick));
        // Another note is another place however near the line numbers are.
        assert!(!coalesces(&caret("a.md", 10), &caret("b.md", 10), quick));
        // A long pause makes the same paragraph a second visit...
        let later = Duration::from_secs(60);
        assert!(!coalesces(&caret("a.md", 10), &caret("a.md", 13), later));
        // ...but exactly the same place is never worth two entries, however long ago.
        assert!(coalesces(&caret("a.md", 10), &caret("a.md", 10), later));
        // A PDF is coarser: a page is as near as its places get.
        let page = |n| Place {
            key: "p.pdf".to_string(),
            at: Spot::Page(Anchor {
                page: n,
                u: 0.0,
                v: 0.1 * n as f32,
            }),
        };
        assert!(coalesces(&page(3), &page(3), later));
        assert!(!coalesces(&page(3), &page(4), quick));
    }

    /// The whole of Back and Forward: what is recorded, what a walk hands back, and that going
    /// somewhere new ends the branch Back opened.
    #[test]
    fn back_walks_the_places_that_were_recorded() {
        let mut nav = Nav::default();
        let now = Instant::now();
        // An edit in a.md, then a jump to b.md: the jump records where it started, which the edit
        // has already put there, so there is one entry and not two.
        nav.record(caret("a.md", 10), now);
        nav.record(caret("a.md", 10), now);
        assert_eq!(nav.back.len(), 1);

        // From b.md, Back returns to the edit and b.md becomes the way forward again.
        assert_eq!(nav.back(caret("b.md", 5)), Some(caret("a.md", 10)));
        assert_eq!(nav.forward(caret("a.md", 10)), Some(caret("b.md", 5)));
        assert_eq!(nav.back(caret("b.md", 5)), Some(caret("a.md", 10)));

        // Somewhere new from here: the forward side goes, as it does in a browser.
        nav.record(caret("a.md", 10), now);
        assert!(nav.forward(caret("c.md", 1)).is_none());

        // Nothing left to go back to is not an error, it is the start of the history.
        let mut nav = Nav::default();
        assert!(nav.back(caret("a.md", 1)).is_none());

        // A closed tab's places go with it.
        let mut nav = Nav::default();
        nav.record(caret("a.md", 1), now);
        nav.record(caret("b.md", 1), now);
        nav.forget("a.md");
        assert_eq!(nav.back(caret("c.md", 1)), Some(caret("b.md", 1)));
        assert!(nav.back(caret("c.md", 1)).is_none());

        // A page edit takes the places in its document with the pages, and no other.
        let page = |key: &str, n| Place {
            key: key.to_string(),
            at: Spot::Page(Anchor {
                page: n,
                u: 0.0,
                v: 0.5,
            }),
        };
        let mut nav = Nav::default();
        nav.record(page("p.pdf", 0), now);
        nav.record(page("q.pdf", 0), now);
        nav.repage("p.pdf", |a| Anchor { page: 2, ..a });
        assert_eq!(nav.back(caret("c.md", 1)), Some(page("q.pdf", 0)));
        assert_eq!(nav.back(caret("c.md", 1)), Some(page("p.pdf", 2)));
    }

    /// Two tabs pinned besides the one asked about: a pinned tab goes no further right than the
    /// end of that group, and any other no further left, whatever moved it there.
    #[test]
    fn pinned_tabs_stay_left_of_the_rest() {
        // Dragged to the far end, a pinned tab comes back to the end of its group.
        assert_eq!(pinned_slot(4, 2, true), 2);
        assert_eq!(pinned_slot(1, 2, true), 1);
        // Dragged in front of the pinned ones, any other comes back to just after them.
        assert_eq!(pinned_slot(0, 2, false), 2);
        assert_eq!(pinned_slot(3, 2, false), 3);
    }

    /// The grid is anchored at the centre, so a divider dragged to 47 % steps to 50 % or 45 %,
    /// never to 52 % or 42 %; and a stop rounded to whole pixels still counts as the stop.
    #[test]
    fn a_divider_steps_along_a_grid_anchored_at_the_centre() {
        assert_eq!(divider_step(470, 1000, true), 500);
        assert_eq!(divider_step(470, 1000, false), 450);
        assert_eq!(divider_step(500, 1000, true), 550);
        assert_eq!(divider_step(501, 1000, false), 450);
        assert_eq!(divider_step(499, 1000, true), 550);
        assert_eq!(
            divider_step(300, 0, true),
            300,
            "an unallocated split stays put"
        );
    }

    #[test]
    fn a_split_puts_the_new_pane_on_the_named_side() {
        assert_eq!(arrange(Side::Left), (gtk::Orientation::Horizontal, true));
        assert_eq!(arrange(Side::Right), (gtk::Orientation::Horizontal, false));
        assert_eq!(arrange(Side::Up), (gtk::Orientation::Vertical, true));
        assert_eq!(arrange(Side::Down), (gtk::Orientation::Vertical, false));
    }

    /// Nothing else stops a flag a future libadwaita adds from arriving unlisted, which would put
    /// a chord back beyond the reach of both the rebind dialog and the never-bind test.
    #[test]
    fn the_chord_table_covers_every_shortcut_the_widget_has() {
        let listed = CHORDS
            .iter()
            .fold(adw::TabViewShortcuts::NONE, |all, (flag, _, _)| all | *flag);
        assert_eq!(listed, adw::TabViewShortcuts::ALL_SHORTCUTS);
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> graphene::Rect {
        graphene::Rect::new(x, y, w, h)
    }

    #[test]
    fn two_columns_have_one_neighbour_each_and_only_sideways() {
        let left = rect(0.0, 0.0, 700.0, 900.0);
        let right = rect(700.0, 0.0, 700.0, 900.0);
        let panes = [left, right];
        assert_eq!(neighbour(left, &panes, Side::Right), Some(1));
        assert_eq!(neighbour(right, &panes, Side::Left), Some(0));
        // Nothing beyond the window's edge, and nothing above or below a full-height column.
        assert_eq!(neighbour(left, &panes, Side::Left), None);
        assert_eq!(neighbour(left, &panes, Side::Up), None);
        assert_eq!(neighbour(left, &panes, Side::Down), None);
    }

    #[test]
    fn a_nested_split_hands_over_the_pane_it_overlaps_most() {
        let left = rect(0.0, 0.0, 700.0, 900.0);
        let top = rect(700.0, 0.0, 700.0, 600.0);
        let bottom = rect(700.0, 600.0, 700.0, 300.0);
        let panes = [left, top, bottom];
        // Both start at the same edge, so the tie is broken by how much of the left column each
        // of them faces.
        assert_eq!(neighbour(left, &panes, Side::Right), Some(1));
        assert_eq!(neighbour(top, &panes, Side::Down), Some(2));
        assert_eq!(neighbour(bottom, &panes, Side::Up), Some(1));
        assert_eq!(neighbour(top, &panes, Side::Left), Some(0));
    }

    #[test]
    fn a_pane_that_is_only_diagonally_away_is_not_a_neighbour() {
        let top_left = rect(0.0, 0.0, 700.0, 450.0);
        let bottom_right = rect(700.0, 450.0, 700.0, 450.0);
        let panes = [top_left, bottom_right];
        // They touch at a corner and nowhere else, so neither is to the right of the other.
        assert_eq!(neighbour(top_left, &panes, Side::Right), None);
        assert_eq!(neighbour(top_left, &panes, Side::Down), None);
    }

    #[test]
    fn an_unallocated_pane_has_no_neighbours() {
        let panes = [rect(0.0, 0.0, 0.0, 0.0), rect(0.0, 0.0, 700.0, 900.0)];
        assert_eq!(neighbour(panes[0], &panes, Side::Right), None);
    }
}
