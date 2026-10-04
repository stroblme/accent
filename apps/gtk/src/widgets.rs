//! The widgets and timers more than one pane was building the same way: an empty state, a
//! scroller, a list factory of plain labels, and the two timers — a pulsing progress bar and a
//! keystroke debounce — that every searching pane needs a copy of.

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// The shared empty state of every pane. A full-size `AdwStatusPage` is drawn for a window, not
/// for a 200 px column: `.compact` takes the icon from 128 to 96 px, drops the title a step and
/// halves the margins from 36 to 24.
///
/// ponytail: libadwaita has no smaller variant than `.compact`, so if it still crowds a narrow
/// sidebar the next dial is an app CSS rule shrinking the icon inside `statuspage.compact`.
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

/// A list's scroller: it takes the height it is given and never scrolls sideways, because
/// everything put in one of these ellipsizes instead.
pub(crate) fn scroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .build()
}

/// Give `view` its `model`, and keep the list where it is whenever the model takes away the row
/// holding the keyboard focus.
///
/// GTK finds a removed widget's focus a new home after the next paint, from the top of the window
/// (`GtkWindow`'s `TAB_FORWARD`), and in a list that is its first row, scrolled to: a Stage click,
/// whose row leaves Changes, or a file made or removed around the row last clicked put the Git
/// and Files lists back at their top. The focus goes instead to the row GTK now keeps in that
/// place, which is where the reader was, so nothing scrolls; taking it cancels the window's own
/// search.
///
/// Two handlers, one either side of the view's own, which `set_model` connects: the first sees
/// whether a row has the focus while it is still there, the second whether it is there still.
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

/// Before `row` is taken out of its list box, hand the keyboard, where the row holds it, to the
/// row showing after it, or else the one before, or else to `fallback`: GTK would give it to the
/// first thing in the window it can focus, and scroll there (see [`set_model`]).
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

/// A factory whose row `setup` builds and `bind` fills, each handed the row as its own type.
///
/// The two downcasts every list factory was writing out: `GtkSignalListItemFactory` hands its
/// closures a `GObject`, and the child comes back as a `GtkWidget`. `setup` is given the item as
/// well, for a row whose drawing reads the object bound to it.
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

/// Hang a menu off `host`, pointed at `anchor` in `host`'s coordinates, and show it.
///
/// The popover comes back so a caller can hear it close: the tree holds its row highlight for as
/// long as its menu is up (`tree::Tree::pin`).
///
/// A popover parented by hand stays parented until it is unparented by hand — but not while it is
/// closing. `closed` is emitted from inside the item's own `clicked`, and an unparented widget
/// has no path to the action group on the host, so unparenting there drops whatever the click
/// just asked for: it is what made the status bar's Fit Height do nothing. The idle runs once the
/// click is over.
pub(crate) fn popup_menu(
    host: &impl IsA<gtk::Widget>,
    menu: &gio::Menu,
    anchor: Option<gdk::Rectangle>,
) -> gtk::PopoverMenu {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.set_parent(host);
    popover.set_has_arrow(false);
    popover.set_pointing_to(anchor.as_ref());
    popover.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    popover.popup();
    popover
}

/// Add or take away a style class. Row widgets are recycled, so the branch that takes it off
/// again is never the one to leave out.
pub(crate) fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    match on {
        true => widget.add_css_class(class),
        false => widget.remove_css_class(class),
    }
}

/// Keep a press on `button` to the button, for one laid over a text view. GtkButton claims its
/// press only on release, so the view under it saw the press too, put its caret under the pointer
/// and took the keyboard. The claim is made in a group with the button's own click, which a
/// gesture claiming alone would deny; and the click leaves the keyboard where it was.
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

/// The class on a revealer of a row's hover buttons ([`hover_revealer`]): the only revealers
/// [`reveal_on_hover`] opens and shuts, so it never touches one a row holds for its own ends (an
/// `AdwEntryRow`'s apply button sits in one).
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

/// Show a row's buttons while the pointer or the keyboard is on it, and give them no width at all
/// the rest of the time, so the name beside them reads out to the whole width of the pane and is
/// cut short only where there is really something to give way to. A `GtkRevealer` does both: shut,
/// it measures nothing, and it slides them in and out at full opacity, the same way whether the
/// pointer or the focus is what let them go. The Git pane's rows and the diagram Properties
/// pane's rows share it, each holding its buttons in a [`hover_revealer`].
///
/// Watched on the row itself: that is the widget GTK marks with PRELIGHT while the pointer is
/// anywhere on it and with FOCUS_WITHIN while one of its buttons has the keyboard — and in a list
/// it is the one the keyboard lands on first, so Tab reveals the buttons it would otherwise never
/// be able to reach.
///
/// A click gives the row the focus as well, taking it back even from a button pressed inside it,
/// and that focus is the pointer's: it keeps the buttons no longer than the pointer stays, or a
/// commit clicked open would keep them out until the focus went somewhere else. So a press marks
/// the row until both the pointer and the focus have left it. GTK's FOCUS_VISIBLE cannot tell the
/// two apart: it outlasts a key by three seconds whatever is clicked meanwhile, and then drops
/// what the keyboard reached.
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
    row.connect_state_flags_changed(move |row, _| {
        let flags = row.state_flags();
        let hovered = flags.contains(gtk::StateFlags::PRELIGHT);
        let focused = flags.contains(gtk::StateFlags::FOCUS_WITHIN);
        // Not on the focus leaving alone: moving it between the row and a button inside takes it
        // off the row and puts it back, which a press does with the pointer still on the row.
        if !hovered && !focused {
            clicked.set(false);
        }
        let on = hovered || (focused && !clicked.get());
        for revealer in revealers(row) {
            revealer.set_reveal_child(on);
        }
    });
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

/// Fade `widget` in from nothing over [`FADE_MS`], easing out. It is libadwaita's animation, which
/// lands at once with `gtk-enable-animations` off or the widget unmapped. It keeps itself until it
/// is done: one let go part way would leave the widget part transparent.
pub(crate) fn fade_in(widget: &impl IsA<gtk::Widget>) {
    let target = adw::PropertyAnimationTarget::new(widget.upcast_ref::<gtk::Widget>(), "opacity");
    let fade = adw::TimedAnimation::new(widget, 0.0, 1.0, FADE_MS, target);
    fade.set_easing(adw::Easing::EaseOutCubic);
    let kept = RefCell::new(Some(fade.clone()));
    fade.connect_done(move |_| drop(kept.take()));
    fade.play();
}

/// A progress bar stepped by a timer of ours, GTK4 having no indeterminate mode (DESIGN.md,
/// Loading). Every timer it starts is removed when the work ends and when the bar's window
/// closes: a leaked `glib::timeout` keeps firing.
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

    /// Step the bar every `every`, drawing it once `after` steps have passed — held back so a
    /// wait that is over before anyone notices never draws a bar at all. Opacity rather than
    /// visibility: the bar keeps its height either way, so nothing under it jumps when a wait
    /// starts.
    ///
    /// Already running is left alone rather than restarted: a restart would hold the bar at the
    /// start of its trough for as long as the messages keep coming.
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

/// One pending call at a time, replaced on every keystroke: long enough to swallow a burst,
/// short enough to feel immediate (DESIGN.md, Motion).
pub(crate) struct Debounce {
    delay: Duration,
    pending: Rc<RefCell<Option<glib::SourceId>>>,
}

impl Drop for Debounce {
    /// A pane that goes away takes its pending call with it: a late timeout must not touch a
    /// closed window.
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

    /// Run `f` once the delay has passed, unless a call is already waiting: the first one wins.
    ///
    /// The other half of [`call`](Self::call). A restart is right where the latest input is the
    /// one to answer (a query, a render); first-wins is right where the work reads the current
    /// state whenever it runs, and a burst must not push it off indefinitely — writing the
    /// session, for one, which a steady stream of edits would otherwise never get to.
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
