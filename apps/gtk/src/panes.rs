//! Split panes with an `AdwTabBar` and `AdwTabView` each, nested in `GtkPaned`s.
//!
//! The widget tree is the layout model: splitting replaces a pane with a paned; closing
//! reverses that swap. No parallel layout state needs synchronisation.
//!
//! Libadwaita transfers `AdwTabPage`s between panes. This module adds edge drop zones to
//! split panes, which libadwaita does not provide.

use crate::find;
use crate::pdfview::Anchor;
use crate::switcher::Switcher;
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The class `main::install_chrome_css` paints the drop hint with.
const ZONE: &str = "accent-drop-zone";
/// Share of pane width or height reserved for an edge drop zone, leaving the middle easy to target.
const EDGE: f64 = 0.25;

/// Preview indicator whose eye action keeps the tab. Loose-file indicators take priority.
const PREVIEW_ICON: &str = "view-reveal-symbolic";
const PREVIEW_TIP: &str = "Preview — Click to Keep";

/// Pinned indicator, with unpinning kept in the menu to avoid stray clicks.
/// Loose-file indicators take priority because save location matters more than pin state.
const PINNED_ICON: &str = "view-pin-symbolic";
const PINNED_TIP: &str = "Pinned";

/// AdwTabView shortcuts retained after reserving window and editor bindings.
///
/// Ctrl+Tab/Shift+Tab belong to MRU actions. Home/End pairs must reach GtkSourceView for
/// document navigation and selection, since the tab controller runs in capture phase.
/// PageUp/PageDown, their Shift variants and Alt+0..9 remain with libadwaita; [`CHORDS`]
/// records them for shortcut discovery.
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

/// Complete AdwTabView shortcut catalogue with flags, action names and accelerator spellings.
/// Tests require every ALL_SHORTCUTS flag so palette and rebinding cannot miss a shortcut.
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

/// Return live history pages followed by never-selected pages in bar order.
///
/// Both Ctrl+Tab and close fallback use this order, with no display needed to test it.
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

/// Advance `at` through `len` tabs with wrapping.
/// The [`recent_order`] stays fixed while Ctrl is held, making successive steps predictable.
pub fn cycle_to(len: usize, at: usize, forward: bool) -> usize {
    match len {
        0 => 0,
        len if forward => (at + 1) % len,
        len => (at + len - 1) % len,
    }
}

/// Move `item` to the front of the most recently used order.
pub fn to_front<T: Clone + PartialEq>(order: &mut Vec<T>, item: &T) {
    order.retain(|p| p != item);
    order.insert(0, item.clone());
}

/// Clamp a tab position to the boundary of the leading `pinned` group.
/// Pin/unpin and drag reordering use the same rule, keeping pinned tabs before all others.
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

/// Maximum pane navigation history, shared with the PDF reader.
const HISTORY: usize = 100;
/// Line distance separating navigation entries, coalescing nearby typing into one place.
const NEARBY: i32 = 10;
/// Pause separating repeat visits to nearby text, while allowing mid-sentence pauses to coalesce.
const COALESCE: Duration = Duration::from_secs(10);

/// Location within a document: text caret, PDF [`Anchor`] or diagram page.
/// Other document kinds have only a whole-document position.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Spot {
    /// A 1-based line and column, as [`crate::editor::Tab::goto_line`] takes them.
    Caret(i32, i32),
    Page(Anchor),
    /// A diagram's page, by its index.
    Sheet(usize),
    Whole,
}

/// A document key and location for navigation history.
/// Keys survive tab reordering without retaining the page.
#[derive(Clone, PartialEq, Debug)]
pub struct Place {
    pub key: String,
    pub at: Spot,
    /// Shared text mark following edits so Back returns to the original character.
    /// Use recorded `at` when [`Held::iter_in`] cannot resolve the mark.
    pub mark: Option<Rc<Held>>,
}

/// Navigation mark removed from its buffer when the last history entry releases it.
/// Closing its tab removes entries through [`Nav::forget`].
#[derive(Debug, PartialEq)]
pub struct Held {
    mark: gtk::TextMark,
    /// The tab's count of whole-text replacements when the mark was set (`Tab::replaced`).
    replaced: u32,
}

impl Held {
    /// Create a left-gravity mark at `iter`, keeping later typing after the recorded place.
    pub fn at(buffer: &gtk::TextBuffer, iter: &gtk::TextIter, replaced: u32) -> Held {
        Held {
            mark: buffer.create_mark(None, iter, true),
            replaced,
        }
    }

    /// Resolve the mark in `buffer`, or `None` after removal or whole-text replacement.
    /// `replaced` is the tab's current replacement count; replacements move old marks to the start.
    pub fn iter_in(&self, buffer: &gtk::TextBuffer, replaced: u32) -> Option<gtk::TextIter> {
        let ours = !self.mark.is_deleted() && self.mark.buffer().as_ref() == Some(buffer);
        (ours && replaced == self.replaced).then(|| buffer.iter_at_mark(&self.mark))
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(buffer) = self.mark.buffer()
            && !self.mark.is_deleted()
        {
            buffer.delete_mark(&self.mark);
        }
    }
}

/// Whether `next` coalesces with `last`: same document, nearby position and age within [`COALESCE`].
/// Exact repeats always coalesce so jumps cannot record their starting place twice.
/// `apart` is the age of the last entry.
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

    /// Back/forward entry counts for checking that session restoration adds no navigation steps.
    #[cfg(feature = "bench")]
    pub fn depth(&self) -> (usize, usize) {
        (self.back.len(), self.forward.len())
    }

    /// A tab has closed: its places go with it, because there is nothing left to go back into.
    pub fn forget(&mut self, key: &str) {
        self.back.retain(|p| p.key != key);
        self.forward.retain(|p| p.key != key);
    }

    /// Remap the pages of `key`'s history places (a PDF's or a diagram's) through `map`, keeping
    /// Back attached to pages after reordering.
    pub fn repage(&mut self, key: &str, map: impl Fn(usize) -> usize) {
        for place in self.back.iter_mut().chain(&mut self.forward) {
            match &mut place.at {
                _ if place.key != key => {}
                Spot::Page(anchor) => anchor.page = map(anchor.page),
                Spot::Sheet(page) => *page = map(*page),
                Spot::Caret(..) | Spot::Whole => {}
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

/// Move a divider one [`DIVIDER_STEP`] towards the end (`forward`) or start.
///
/// Stops are anchored at the centre so reset remains reachable and off-grid positions snap
/// back to the grid. Positions within one pixel count as the stop because ratios round to
/// pixels. Callers clamp the result to the split's bounds.
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

/// Find the nearest pane on `side` of `from`, as an index into `others`.
///
/// Candidates start beyond the facing edge and overlap on the other axis; ties prefer more
/// overlap. Geometry reflects visual neighbours better than split ancestry. `from` may be
/// included but cannot match; unallocated panes have no neighbours.
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
    /// Pane-local find/replace/go-to-line bar, revealed between tabs and document for independent split searches.
    pub find: Rc<find::Bar>,
    pub tabs: adw::TabView,
    overlay: gtk::Overlay,
    /// Drag drop-sheet overlay, picked before AdwTabView's plain-transfer target so edges can split panes.
    hint: gtk::Box,
    /// Highlight rectangle inside `hint`, keeping its coordinate widget fixed as the zone changes.
    shade: gtk::Box,
    pub drop: gtk::DropTarget,
    /// Most-recent-first tab history, pruned against live pages on every read.
    /// Transferred tabs leave automatically and enter their destination history on selection.
    history: RefCell<Vec<adw::TabPage>>,
    /// MRU order before libadwaita selects a neighbour while a dragged tab leaves.
    transfer_order: RefCell<Option<Vec<adw::TabPage>>>,
    /// How deep into `history` a held `Ctrl+Tab` has walked, `None` when no chord is in flight.
    /// See [`Pane::step`].
    cycling: Cell<Option<usize>>,
    /// Pane-local switcher overlay listing `history` with `cycling` highlighted, leaving other panes clear.
    pub switcher: Switcher,
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

        // Expanding of its own, not only through the tab view, which presentation mode hides
        // under the rendered note it lays over it (`Pane::cover`).
        let overlay = gtk::Overlay::builder()
            .child(&tabs)
            .hexpand(true)
            .vexpand(true)
            .build();
        overlay.add_overlay(&hint);
        let switcher = Switcher::new();
        overlay.add_overlay(switcher.widget());

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
            transfer_order: RefCell::new(None),
            cycling: Cell::new(None),
            switcher,
            nav: RefCell::new(Nav::default()),
            preview: RefCell::new(None),
        });
        pane.wire_preview();
        let weak = Rc::downgrade(&pane);
        pane.switcher.connect_pick(move |at| {
            if let Some(pane) = weak.upgrade() {
                pane.pick(at);
            }
        });
        pane
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.column.upcast_ref()
    }

    /// Overlay `widget` above tabs and below drag hints and the switcher for presentation.
    /// Transfer it from another pane if needed; other parents must release it first. Skip if already here.
    pub fn cover(&self, widget: &gtk::Widget) {
        if widget.parent().as_ref() == Some(self.overlay.upcast_ref()) {
            return;
        }
        widget.unparent();
        self.overlay.add_overlay(widget);
        widget.insert_after(&self.overlay, Some(&self.tabs));
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

    /// Record selection of `page` and prune pages that have left the pane.
    ///
    /// Ctrl+Tab steps preserve the order they are walking. Other selections end cycling, so a
    /// lost modifier release cannot leave the pane stuck in that state.
    pub fn touch(&self, page: &adw::TabPage) {
        if let Some(at) = self.cycling.get() {
            if self.recent().get(at) == Some(page) {
                return;
            }
            self.cycling.set(None);
            self.switcher.hide();
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
        let order = self.transfer_order.borrow();
        let recent = order
            .as_ref()
            .map(|before| recent_order(before, &self.pages()))
            .unwrap_or_else(|| self.recent());
        recent.into_iter().find(|p| p != page)
    }

    /// The previous selection order is needed after libadwaita auto-selects a neighbour.
    pub fn dragged_survivor(&self, page: &adw::TabPage) -> Option<adw::TabPage> {
        if !self.tabs.is_transferring_page() {
            return None;
        }
        let order = self.transfer_order.borrow();
        recent_order(order.as_ref()?, &self.pages())
            .into_iter()
            .find(|p| p != page)
    }

    /// Step the fixed MRU order while Ctrl is held; Shift reverses direction.
    /// [`Pane::switcher`] displays [`Pane::recent`] and highlights the cycling cursor.
    pub fn step(&self, forward: bool) -> Option<adw::TabPage> {
        let order = self.recent();
        let at = cycle_to(order.len(), self.cycling.get().unwrap_or(0), forward);
        self.cycling.set(Some(at));
        order.get(at).cloned()
    }

    /// Where a held `Ctrl+Tab` has walked to in [`Pane::recent`], `None` with no chord in flight.
    pub fn cycling(&self) -> Option<usize> {
        self.cycling.get()
    }

    /// Finish cycling on Ctrl release and record the selected tab as most recent. No-op without a cycle.
    pub fn end_cycle(&self) {
        self.switcher.hide();
        if self.cycling.take().is_none() {
            return;
        }
        if let Some(page) = self.tabs.selected_page() {
            self.touch(&page);
        }
    }

    /// End the chord on the `at`th tab in [`Pane::recent`], as if Ctrl had come up there: a
    /// click on the card's row. Stepped to rather than picked, so it does not fade in either.
    pub fn pick(&self, at: usize) {
        let Some(page) = self.recent().get(at).cloned() else {
            return;
        };
        if self.cycling.get().is_none() {
            return;
        }
        self.cycling.set(Some(at));
        self.tabs.set_selected_page(&page);
        self.end_cycle();
    }

    /// Escape while the chord is held: back to the tab it started from, the order as it was.
    /// Says whether there was a chord to call off.
    pub fn cancel_cycle(&self) -> bool {
        let held = self.cycling.get().is_some();
        if held {
            self.pick(0);
        }
        held
    }

    // --- preview tabs ----------------------------------------------------------------------

    /// Return the live preview tab. Leaving the pane keeps it through [`Pane::wire_preview`].
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

    /// Keep `page` after editing, tab double-click, eye click or leaving its pane.
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

    /// Wire tab-view preview promotion on eye clicks and page transfers.
    fn wire_preview(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.tabs.connect_is_transferring_page_notify(move |tabs| {
            let Some(pane) = weak.upgrade() else { return };
            if tabs.is_transferring_page() {
                *pane.transfer_order.borrow_mut() = Some(pane.recent());
            } else {
                let weak = Rc::downgrade(&pane);
                glib::idle_add_local_once(move || {
                    if let Some(pane) = weak.upgrade() {
                        pane.transfer_order.borrow_mut().take();
                    }
                });
            }
        });
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

    /// Call `f` with the page whose tab was double-clicked.
    ///
    /// AdwTabBox claims presses, cancelling gestures but not raw controllers. A capture-phase
    /// legacy controller observes without consuming them. The first press already selected
    /// the page, so the selected page identifies the clicked tab.
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

    /// Enable or disable the mapped drop sheet without remapping during a drag.
    /// Some backends do not pick newly mapped targets; disabled sheets draw nothing and pass input through.
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

/// Split `pane` with `new` on `side`, replacing it with a paned whose handle starts at the old midpoint.
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

/// The column that takes `pane`'s place when it is detached: its sibling, or where the sibling is
/// split itself, the sibling's column that borders `pane`.
pub fn heir(pane: &Pane) -> Option<gtk::Widget> {
    let paned = pane.column.parent().and_downcast::<gtk::Paned>()?;
    let after = paned.end_child().as_ref() == Some(pane.widget());
    let mut heir = match after {
        true => paned.start_child(),
        false => paned.end_child(),
    }?;
    // Where `pane` comes after the sibling, the columns facing it end the sibling's splits that
    // run the same way.
    while let Ok(split) = heir.clone().downcast::<gtk::Paned>() {
        let facing = after && split.orientation() == paned.orientation();
        heir = match facing {
            true => split.end_child(),
            false => split.start_child(),
        }?;
    }
    Some(heir)
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
    // A paned that loses the child it had the keyboard in looks for the focused widget among its
    // own, and warns once per paned up the tree where it is not there (ISSUES.md): gone with a
    // shell's tab, since GTK hands the keyboard on only at the next frame, on `pane` itself, or in
    // `sibling` while it is between its two paneds, whose keyboard GTK would then hand to the
    // paned it lands in. So the focus is unset while the tree changes, and a widget still in the
    // window gets it back; one that was `pane`'s went on as its last tab closed
    // (`App::hand_on_keyboard`).
    let root = pane.column.root();
    let held = root
        .as_ref()
        .and_then(|root| root.focus())
        .filter(|w| w.root().is_none() || w.is_ancestor(&paned));
    if let (Some(root), Some(_)) = (&root, &held) {
        root.set_focus(gtk::Widget::NONE);
    }
    paned.set_start_child(gtk::Widget::NONE);
    paned.set_end_child(gtk::Widget::NONE);
    replace(&grandparent, paned.upcast_ref(), &sibling);
    if let Some(widget) = held.filter(|w| w.root().is_some()) {
        widget.grab_focus();
    }
}

/// Show only `only` in-place by hiding sibling branches, or restore all panes with `None`.
/// GtkPaned preserves divider positions while its sole visible child fills the split.
/// Change visibility only where needed to avoid remapping unchanged siblings.
pub fn isolate(panes: &[Rc<Pane>], only: Option<&Pane>) {
    let mut path = Vec::new();
    let mut at = only.map(|pane| pane.widget().clone());
    while let Some(node) = at {
        at = node.parent().filter(|parent| parent.is::<gtk::Paned>());
        path.push(node);
    }
    for pane in panes {
        let mut node = pane.widget().clone();
        while let Some(paned) = node.parent().and_downcast::<gtk::Paned>() {
            let beside = !path.contains(&node) && path.contains(paned.upcast_ref());
            node.set_visible(!beside);
            node = paned.upcast();
        }
    }
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
            mark: None,
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
            mark: None,
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
            mark: None,
            at: Spot::Page(Anchor {
                page: n,
                u: 0.0,
                v: 0.5,
            }),
        };
        let mut nav = Nav::default();
        nav.record(page("p.pdf", 0), now);
        nav.record(page("q.pdf", 0), now);
        let sheet = |n| Place {
            key: "d.drawio".to_string(),
            mark: None,
            at: Spot::Sheet(n),
        };
        nav.record(sheet(1), now);
        nav.repage("p.pdf", |_| 2);
        nav.repage("d.drawio", |n| n + 1);
        assert_eq!(nav.back(caret("c.md", 1)), Some(sheet(2)));
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

    /// Require catalogue coverage for future libadwaita flags so rebinding and reserved-key tests see every chord.
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
