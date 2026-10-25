//! Shared empty states, scrollers, label factories, progress pulses, search debounces and
//! handler slots, keeping pane construction consistent.

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// Build a compact empty state for narrow panes.
///
/// AdwStatusPage's `.compact` reduces icon, title and margins compared with a window-sized
/// status page.
pub(crate) fn status_page(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    let page = adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(description)
        .vexpand(true)
        .build();
    page.add_css_class("compact");
    page
}

/// Height-filling list scroller without horizontal scrolling; rows ellipsize instead.
pub(crate) fn scroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .build()
}

/// Set `view`'s model while preserving scroll when its focused row disappears.
///
/// GTK otherwise moves focus to the first row after painting, scrolling lists to the top.
/// Handlers before and after `set_model`'s handler retain focus at the removed row's position
/// and cancel that fallback search.
pub(crate) fn set_model(view: &gtk::ListView, model: &impl IsA<gtk::SelectionModel>) {
    let (model, held) = (
        model.upcast_ref::<gtk::SelectionModel>(),
        Rc::new(Cell::new(false)),
    );
    let weak = view.downgrade();
    model.connect_items_changed({
        let (weak, held) = (weak.clone(), held.clone());
        move |_, _, removed, _| {
            held.set(removed > 0 && weak.upgrade().is_some_and(|view| has_focus(&view)));
        }
    });
    view.set_model(Some(model));
    model.connect_items_changed(move |_, _, _, _| {
        if held.take()
            && let Some(view) = weak.upgrade()
            && !has_focus(&view)
        {
            view.grab_focus();
        }
    });
}

/// Move focus before removing a focused row: next visible row, previous or `fallback`.
/// Avoid GTK's fallback to the window's first focusable widget and resulting scroll jump.
pub(crate) fn hand_on_focus(row: &gtk::ListBoxRow, fallback: &impl IsA<gtk::Widget>) {
    if !row.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN) {
        return;
    }
    let shown =
        |w: &gtk::Widget| w.is::<gtk::ListBoxRow>() && w.is_child_visible() && w.is_sensitive();
    let beside = std::iter::successors(row.next_sibling(), |w| w.next_sibling())
        .find(shown)
        .or_else(|| std::iter::successors(row.prev_sibling(), |w| w.prev_sibling()).find(shown));
    if !beside.is_some_and(|w| w.grab_focus()) {
        fallback.grab_focus();
    }
}

/// Whether the keyboard focus is on one of `view`'s rows.
fn has_focus(view: &gtk::ListView) -> bool {
    view.root()
        .and_then(|root| root.focus())
        .is_some_and(|focus| focus.is_ancestor(view))
}

/// Typed row setup/binding for list factories, centralizing GObject/widget downcasts.
/// Pass the list item to setup for rows whose drawing uses the bound object.
pub(crate) fn factory<W: IsA<gtk::Widget>>(
    setup: impl Fn(&gtk::ListItem) -> W + 'static,
    bind: impl Fn(&W, &gtk::ListItem) + 'static,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        item.set_child(Some(&setup(item)));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if let Some(row) = item.child().and_downcast::<W>() {
            bind(&row, item);
        }
    });
    factory
}

/// [`factory`] for the commonest row of all: one ellipsized label.
pub(crate) fn label_factory(
    ellipsize: pango::EllipsizeMode,
    bind: impl Fn(&gtk::Label, &gtk::ListItem) + 'static,
) -> gtk::SignalListItemFactory {
    factory(
        move |_| {
            gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(ellipsize)
                .build()
        },
        bind,
    )
}

/// The text a `GtkStringList` row carries, which is what every plain-label list is a list of.
pub(crate) fn row_text(item: &gtk::ListItem) -> Option<String> {
    item.item()
        .and_downcast::<gtk::StringObject>()
        .map(|s| s.string().to_string())
}

/// Show a menu on `host` at `anchor` and return its popover for close notifications.
///
/// Unparent on an idle after `closed`: closing occurs inside the item's click, and removing
/// the parent sooner loses the host action group before dispatch. Restore prior focus on
/// close so document keys work again; actions may then move focus elsewhere.
pub(crate) fn popup_menu(
    host: &impl IsA<gtk::Widget>,
    menu: &gio::Menu,
    anchor: Option<gdk::Rectangle>,
) -> gtk::PopoverMenu {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.set_parent(host);
    popover.set_has_arrow(false);
    popover.set_pointing_to(anchor.as_ref());
    let had = host.root().and_then(|r| r.focus()).map(|w| w.downgrade());
    popover.connect_closed(move |p| {
        if let Some(had) = had.as_ref().and_then(|w| w.upgrade()) {
            had.grab_focus();
        }
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    popover.popup();
    popover
}

/// Set or clear a style class so recycled rows never retain stale state.
pub(crate) fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    match on {
        true => widget.add_css_class(class),
        false => widget.remove_css_class(class),
    }
}

/// Claim overlaid button presses with its click gesture before release, preventing
/// the underlying text view from moving the caret or taking focus. Group the gestures
/// so the claim does not deny the button's own click.
pub(crate) fn claim_press(button: &gtk::Button) {
    let claim = gtk::GestureClick::new();
    claim.set_propagation_phase(gtk::PropagationPhase::Capture);
    claim.connect_pressed(|gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    button.add_controller(claim.clone());
    let controllers = button.observe_controllers();
    let own = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
        .find(|gesture| *gesture != claim);
    if let Some(own) = own {
        claim.group_with(&own);
    }
    button.set_focus_on_click(false);
}

/// Mark button revealers for [`reveal_on_hover`] so unrelated revealers remain untouched.
const HOVER_ACTIONS: &str = "accent-hover-actions";

/// The marker class on a row whose buttons already follow its hover ([`reveal_on_hover`]).
const WATCHED: &str = "accent-hover-row";

/// A revealer for a row's hover buttons, which slide in from the side ([`reveal_on_hover`]).
pub(crate) fn hover_revealer() -> gtk::Revealer {
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideLeft)
        .build();
    revealer.add_css_class(HOVER_ACTIONS);
    revealer
}

/// Reveal row buttons for pointer hover or keyboard focus, reserving no width when hidden.
///
/// A [`hover_revealer`] slides full-opacity buttons while names use available space. Watch
/// row PRELIGHT and FOCUS_WITHIN so keyboard navigation reveals controls before Tab needs
/// them. Pointer-created focus retains buttons only while hovered; remember presses until
/// both focus and pointer leave. FOCUS_VISIBLE alone cannot distinguish these cases reliably.
pub(crate) fn reveal_on_hover(row: &impl IsA<gtk::Widget>) {
    let row = row.upcast_ref::<gtk::Widget>();
    // Once per row widget, which in a list is recycled and bound again and again. The class is
    // the marker, there being nowhere else to keep one bit on a widget GTK made for itself.
    if row.has_css_class(WATCHED) {
        return;
    }
    row.add_css_class(WATCHED);
    let clicked = Rc::new(Cell::new(false));
    let press = gtk::GestureClick::new();
    // Ahead of the row's own gesture and of any button's, which are what move the focus.
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let mark = clicked.clone();
    press.connect_pressed(move |_, _, _, _| mark.set(true));
    row.add_controller(press);
    let update = Rc::new(move |row: &gtk::Widget| {
        let flags = row.state_flags();
        let hovered = flags.contains(gtk::StateFlags::PRELIGHT);
        let focused = flags.contains(gtk::StateFlags::FOCUS_WITHIN);
        let revealers = revealers(row);
        // An open menu keeps the button it hangs from on screen, or it would close under the
        // pointer that left the row for it; GTK does not count the menu's focus as the row's.
        let open = revealers.iter().flat_map(menus).any(|m| m.is_active());
        // Forget the press only once the focus stays away: GTK takes it off the row and puts it
        // back when it moves between the row and a button inside, and when a menu closes.
        if !hovered && !focused && !open {
            let (row, clicked) = (row.downgrade(), clicked.clone());
            glib::idle_add_local_once(move || {
                let away = gtk::StateFlags::PRELIGHT | gtk::StateFlags::FOCUS_WITHIN;
                if row
                    .upgrade()
                    .is_some_and(|row| !row.state_flags().intersects(away))
                {
                    clicked.set(false);
                }
            });
        }
        let on = hovered || (focused && !clicked.get()) || open;
        for revealer in revealers {
            revealer.set_reveal_child(on);
        }
    });
    // A menu closing changes no state on the row, the pointer having left it already.
    for menu in revealers(row).iter().flat_map(menus) {
        let (row, update) = (row.downgrade(), update.clone());
        menu.connect_active_notify(move |_| {
            if let Some(row) = row.upgrade() {
                update(&row);
            }
        });
    }
    row.connect_state_flags_changed(move |row, _| update(row));
}

/// The menu buttons under `revealer`.
fn menus(revealer: &gtk::Revealer) -> Vec<gtk::MenuButton> {
    let mut found = Vec::new();
    let mut todo = vec![revealer.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = todo.pop() {
        if let Ok(menu) = widget.clone().downcast::<gtk::MenuButton>() {
            found.push(menu);
            continue;
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
    found
}

/// Every [`hover_revealer`] under `row` — in a list row, one per layout its stack can show.
fn revealers(row: &gtk::Widget) -> Vec<gtk::Revealer> {
    let mut found = Vec::new();
    let mut todo = vec![row.clone()];
    while let Some(widget) = todo.pop() {
        if widget.has_css_class(HOVER_ACTIONS)
            && let Ok(revealer) = widget.clone().downcast::<gtk::Revealer>()
        {
            found.push(revealer);
            continue;
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
    found
}

/// A flat icon button, centred in its row, named by its tooltip.
pub(crate) fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    // The tooltip's words are its name for a screen reader too, an icon having none of its own.
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

/// How long a switch crossfades and a page fades in (DESIGN.md, Motion).
pub(crate) const FADE_MS: u32 = 150;

/// Fade in over [`FADE_MS`] with libadwaita easing; finish immediately when animations
/// are disabled or unmapped. Retain the animation until completion to avoid partial opacity.
pub(crate) fn fade_in(widget: &impl IsA<gtk::Widget>) {
    let target = adw::PropertyAnimationTarget::new(widget.upcast_ref::<gtk::Widget>(), "opacity");
    let fade = adw::TimedAnimation::new(widget, 0.0, 1.0, FADE_MS, target);
    fade.set_easing(adw::Easing::EaseOutCubic);
    let kept = RefCell::new(Some(fade.clone()));
    fade.connect_done(move |_| drop(kept.take()));
    fade.play();
}

/// Timed progress pulse because GTK4 has no indeterminate mode; cancel timers when work or its window ends.
pub(crate) struct Pulse {
    bar: gtk::ProgressBar,
    /// Shared with the timer's own closure, so it can clear the slot when it stops itself.
    id: Rc<Cell<Option<glib::SourceId>>>,
}

impl Drop for Pulse {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Pulse {
    pub(crate) fn new(bar: &gtk::ProgressBar) -> Pulse {
        Pulse {
            bar: bar.clone(),
            id: Rc::new(Cell::new(None)),
        }
    }

    /// Pulse every `every`, showing the bar after `after` steps to suppress brief waits.
    ///
    /// Use opacity to preserve layout. An active pulse is not restarted, so repeated messages
    /// cannot keep it permanently at the start of its cycle.
    pub(crate) fn start(&self, every: Duration, after: u32) {
        if self.running() {
            return;
        }
        let (bar, slot) = (self.bar.clone(), self.id.clone());
        let mut steps = 0;
        self.id.set(Some(glib::timeout_add_local(every, move || {
            // The owner can outlive its window — a query still on a worker thread holds the search
            // pane's — so `Drop` may come late. An unrooted bar means the window closed under the
            // work; that is the timer's cue to stop on its own.
            if bar.root().is_none() {
                slot.set(None);
                return glib::ControlFlow::Break;
            }
            steps += 1;
            if steps >= after {
                bar.set_opacity(1.0);
                bar.pulse();
            }
            glib::ControlFlow::Continue
        })));
    }

    pub(crate) fn stop(&self) {
        if let Some(id) = self.id.take() {
            id.remove();
        }
    }

    pub(crate) fn running(&self) -> bool {
        let id = self.id.take();
        let running = id.is_some();
        self.id.set(id);
        running
    }
}

/// Debounce repeated requests into one pending call after activity settles.
pub(crate) struct Debounce {
    delay: Duration,
    pending: Rc<RefCell<Option<glib::SourceId>>>,
}

impl Drop for Debounce {
    /// Cancel pending timeouts when the pane drops so they cannot access a closed window.
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Debounce {
    pub(crate) fn new(delay: Duration) -> Debounce {
        Debounce {
            delay,
            pending: Rc::new(RefCell::new(None)),
        }
    }

    /// Run `f` once the delay has passed with nothing else asked for.
    pub(crate) fn call(&self, f: impl FnOnce() + 'static) {
        self.cancel();
        let pending = self.pending.clone();
        let id = glib::timeout_add_local_once(self.delay, move || {
            *pending.borrow_mut() = None;
            f();
        });
        *self.pending.borrow_mut() = Some(id);
    }

    /// Run `f` after the delay unless already pending, preserving the first request.
    ///
    /// Unlike [`call`](Self::call), this prevents repeated requests from postponing state-based
    /// work such as session saving indefinitely.
    pub(crate) fn call_once(&self, f: impl FnOnce() + 'static) {
        if self.pending.borrow().is_none() {
            self.call(f);
        }
    }

    /// Drop whatever is pending, for the keystroke that is answered on the spot instead.
    pub(crate) fn cancel(&self) {
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
    }
}

/// A tab callback slot with signature `F`, usually `dyn Fn(&Rc<Tab>)`.
///
/// Clone the handler before invoking it ([`Hook::get`]) so callbacks can re-enter and update
/// hooks without borrowing the cell recursively.
pub(crate) struct Hook<F: ?Sized>(RefCell<Option<Rc<F>>>);

impl<F: ?Sized> Default for Hook<F> {
    fn default() -> Self {
        Hook(RefCell::new(None))
    }
}

impl<F: ?Sized> Hook<F> {
    pub(crate) fn set(&self, f: Rc<F>) {
        *self.0.borrow_mut() = Some(f);
    }

    /// The handler, to call once the cell is let go of.
    pub(crate) fn get(&self) -> Option<Rc<F>> {
        self.0.borrow().clone()
    }
}

impl<T> Hook<dyn Fn(&Rc<T>)> {
    /// Call the handler, if one is set, with the tab it is about.
    pub(crate) fn emit(&self, on: &Rc<T>) {
        if let Some(f) = self.get() {
            f(on);
        }
    }
}
