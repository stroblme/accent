//! Shared empty states, scrollers, label factories, fades, progress pulses, search debounces,
//! handler slots and the focus's selected text, keeping pane construction consistent.

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
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .build();
    smooth_wheel(&scroller);
    scroller
}

/// How long a wheel notch takes to scroll: GTK's own easing of a page key or a scrollbar press
/// (`ANIMATION_DURATION` in `gtkscrolledwindow.c`), so the wheel and the keys move alike.
const WHEEL_MS: u32 = 200;

/// Ease each mouse-wheel notch on `scroller` over [`WHEEL_MS`] instead of GTK's jump, notches in
/// quick succession adding up to one target; call it once the scroller has its child.
///
/// GTK 4 moves a wheel notch's whole step at once. Only the wheel is eased: a touchpad's pixel
/// deltas already scroll smoothly, Shift scrolls sideways and Control zooms, all left to GTK.
/// On the child in bubble phase, so a scroller inside takes its own wheel first, and at an edge
/// the wheel goes on to GTK, for its overshoot and for any scroller around this one.
pub(crate) fn smooth_wheel(scroller: &gtk::ScrolledWindow) {
    let Some(child) = scroller.child() else {
        return;
    };
    // The adjustment is read at each step: a comparison's columns are given a shared one later.
    // ponytail: one animation per scroller, so a wheel crossing from one column to the other
    // within a notch's 200 ms has both easing the shared adjustment until the first ends.
    let weak = scroller.downgrade();
    let target = adw::CallbackAnimationTarget::new(glib::clone!(
        #[weak]
        scroller,
        move |value| scroller.vadjustment().set_value(value)
    ));
    let animation = adw::TimedAnimation::new(scroller, 0.0, 0.0, WHEEL_MS, target);
    animation.set_easing(adw::Easing::EaseOutCubic);
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.connect_scroll(move |wheel, _, dy| {
        let left = gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::CONTROL_MASK
            | crate::platform::PRIMARY_MODIFIER;
        let Some(adj) = weak.upgrade().map(|scroller| scroller.vadjustment()) else {
            return glib::Propagation::Proceed;
        };
        if wheel.unit() != gdk::ScrollUnit::Wheel || wheel.current_event_state().intersects(left) {
            return glib::Propagation::Proceed;
        }
        let from = match animation.state() {
            adw::AnimationState::Playing => animation.value_to(),
            _ => adj.value(),
        };
        let to = wheel_target(from, dy, adj.lower(), adj.upper(), adj.page_size());
        if to == adj.value() {
            return glib::Propagation::Proceed;
        }
        animation.set_value_from(adj.value());
        animation.set_value_to(to);
        animation.play();
        glib::Propagation::Stop
    });
    child.add_controller(wheel);
}

/// Where `dy` notches from `from` land: GTK's step of `page^(2/3)` a notch, kept in range.
fn wheel_target(from: f64, dy: f64, lower: f64, upper: f64, page: f64) -> f64 {
    (from + dy * page.powf(2.0 / 3.0))
        .min(upper - page)
        .max(lower)
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

/// The text selected in `widget`, as Ctrl+Shift+F asks the focus for it: an entry's (the
/// `GtkText` inside it), a text view's (an editor, a notebook cell, a diff) or a terminal's. A
/// hidden entry's, a password's, is not offered.
pub(crate) fn selected_text(widget: &gtk::Widget) -> Option<String> {
    if let Some(editable) = widget.dynamic_cast_ref::<gtk::Editable>() {
        if widget.is::<gtk::Text>() && !widget.property::<bool>("visibility") {
            return None;
        }
        let (start, end) = editable.selection_bounds()?;
        return Some(editable.chars(start, end).into());
    }
    if let Some(view) = widget.dynamic_cast_ref::<gtk::TextView>() {
        let buffer = view.buffer();
        let (start, end) = buffer.selection_bounds()?;
        return Some(buffer.text(&start, &end, false).into());
    }
    use vte4::TerminalExt;
    let terminal = widget.dynamic_cast_ref::<vte4::Terminal>()?;
    terminal.text_selected(vte4::Format::Text).map(Into::into)
}

/// What a selection searches for in the one-line Search box: its first line, if that is not
/// empty.
pub(crate) fn search_query(selected: &str) -> Option<String> {
    let first = selected.lines().next().unwrap_or_default();
    (!first.is_empty()).then(|| first.to_string())
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

/// The marker class on a list row that already opens a [`row_menu`].
const MENU_ROW: &str = "accent-menu-row";

/// Open a menu for a list row on a right-click, or on Menu or Shift+F10 while the row has the
/// keyboard. `build` gets the row's child and answers the menu and the widget it hangs from, whose
/// actions it resolves: a plain box, which GTK re-presents the menu from (States). Call it on the
/// GTK list row, once it is there (bound, as for [`reveal_on_hover`]); a recycled row keeps the
/// first `build`, which serves whatever it shows.
pub(crate) fn row_menu(
    row: &impl IsA<gtk::Widget>,
    build: impl Fn(&gtk::Widget) -> Option<(gtk::Widget, gio::Menu)> + 'static,
) {
    let row = row.upcast_ref::<gtk::Widget>();
    if row.has_css_class(MENU_ROW) {
        return;
    }
    row.add_css_class(MENU_ROW);
    let open = Rc::new(move |row: &gtk::Widget, at: Option<(f64, f64)>| {
        let Some((host, menu)) = row.first_child().and_then(|child| build(&child)) else {
            return;
        };
        // At the pointer, or under the whole row for the keyboard.
        let anchor = match at.and_then(|(x, y)| {
            row.compute_point(&host, &gtk::graphene::Point::new(x as f32, y as f32))
        }) {
            Some(p) => gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, 1),
            None => gdk::Rectangle::new(0, 0, host.width(), host.height()),
        };
        popup_menu(&host, &menu, Some(anchor));
    });
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let on_click = open.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        // Claimed, so the list neither activates nor selects on the right button.
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(row) = gesture.widget() {
            on_click(&row, Some((x, y)));
        }
    });
    row.add_controller(click);
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |keys, key, _, state| {
        use gdk::ModifierType as M;
        let mods = state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK);
        let shift = mods == M::SHIFT_MASK;
        if key == gdk::Key::Menu || (key == gdk::Key::F10 && shift) {
            if let Some(row) = keys.widget() {
                open(&row, None);
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    row.add_controller(keys);
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

/// How long a fade, a crossfade or a slide takes: GtkRevealer's own default, which the revealers
/// keep and the stacks, whose own is 200 ms, are set to (DESIGN.md, Motion).
pub(crate) const FADE_MS: u32 = 250;

/// [`fade_in`]'s easing as a stylesheet writes it: libadwaita's ease-out cubic, which GtkRevealer
/// and GtkStack ease with too.
pub(crate) const CSS_EASING: &str = "cubic-bezier(0.33, 1, 0.68, 1)";

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

/// One fade of a widget's opacity, kept for a widget that comes and goes faster than it fades
/// (the cell buttons under a pointer sweeping a notebook): each change turns it round from where
/// it is, where [`fade_in`] would start one more animation per call. Faded out, it is hidden.
pub(crate) struct Fade {
    widget: gtk::Widget,
    animation: adw::TimedAnimation,
}

impl Fade {
    pub(crate) fn new(widget: &impl IsA<gtk::Widget>) -> Fade {
        let widget = widget.upcast_ref::<gtk::Widget>().clone();
        let target = adw::PropertyAnimationTarget::new(&widget, "opacity");
        let animation = adw::TimedAnimation::new(&widget, 0.0, 1.0, FADE_MS, target);
        animation.set_easing(adw::Easing::EaseOutCubic);
        let weak = widget.downgrade();
        animation.connect_done(move |animation| {
            if animation.value_to() == 0.0
                && let Some(widget) = weak.upgrade()
            {
                widget.set_visible(false);
            }
        });
        Fade { widget, animation }
    }

    /// Show the widget and fade it in from nothing.
    pub(crate) fn fade_in(&self) {
        self.play(0.0, 1.0);
    }

    /// Fade towards `opacity` from wherever the widget is, unless on its way there already.
    pub(crate) fn to(&self, opacity: f64) {
        let at = match self.widget.is_visible() {
            true => self.widget.opacity(),
            false => 0.0,
        };
        let heading = match self.animation.state() {
            adw::AnimationState::Playing => self.animation.value_to(),
            _ => at,
        };
        if heading != opacity {
            self.play(at, opacity);
        }
    }

    /// Stop wherever the fade is, the widget at `opacity` at once.
    pub(crate) fn set(&self, opacity: f64) {
        self.animation.pause();
        self.widget.set_opacity(opacity);
    }

    fn play(&self, from: f64, to: f64) {
        if to > 0.0 {
            self.widget.set_visible(true);
        }
        self.animation.set_value_from(from);
        self.animation.set_value_to(to);
        self.animation.play();
    }
}

/// The class on a tree expander while its arrow turns (`build::install_chrome_css`).
const TURNING: &str = "accent-turning";

/// Turn `expander`'s arrow as its row opens or closes, and only then: a list binds its row widgets
/// to other rows as it scrolls, and an arrow that turned whenever its widget was bound to a row
/// open where the last was closed would spin under the scroll. The arrow's transition is on while
/// the class is, held twice the turn, the transition starting at the next frame.
pub(crate) fn turn_arrows(expander: &gtk::TreeExpander) {
    let bound: RefCell<Option<(gtk::TreeListRow, glib::SignalHandlerId)>> = RefCell::default();
    let held: Rc<Cell<Option<glib::SourceId>>> = Rc::default();
    fn settle(expander: &gtk::TreeExpander, held: &Cell<Option<glib::SourceId>>) {
        if let Some(id) = held.take() {
            id.remove();
        }
        expander.remove_css_class(TURNING);
    }
    expander.connect_list_row_notify(move |expander| {
        if let Some((row, handler)) = bound.take() {
            row.disconnect(handler);
        }
        settle(expander, &held);
        let Some(row) = expander.list_row() else {
            return;
        };
        let (weak, held) = (expander.downgrade(), held.clone());
        let handler = row.connect_expanded_notify(move |_| {
            let Some(expander) = weak.upgrade() else {
                return;
            };
            settle(&expander, &held);
            expander.add_css_class(TURNING);
            let (weak, slot) = (expander.downgrade(), held.clone());
            let turn = Duration::from_millis(u64::from(2 * FADE_MS));
            held.set(Some(glib::timeout_add_local_once(turn, move || {
                slot.take();
                if let Some(expander) = weak.upgrade() {
                    expander.remove_css_class(TURNING);
                }
            })));
        });
        bound.replace(Some((row, handler)));
    });
}

/// Timed progress pulse because GTK4 has no indeterminate mode; cancel timers when work or its window ends.
pub(crate) struct Pulse {
    bar: gtk::ProgressBar,
    /// Shared with the timer's own closure, so it can clear the slot when it stops itself.
    id: Rc<Cell<Option<glib::SourceId>>>,
    /// The bar fading in on its first shown step, stopped by [`hide`](Self::hide).
    fade: Rc<Fade>,
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
            fade: Rc::new(Fade::new(bar)),
        }
    }

    /// Pulse every `every`, fading the bar in after `after` steps to suppress brief waits.
    ///
    /// Use opacity to preserve layout. An active pulse is not restarted, so repeated messages
    /// cannot keep it permanently at the start of its cycle.
    pub(crate) fn start(&self, every: Duration, after: u32) {
        if self.running() {
            return;
        }
        let (bar, slot, fade) = (self.bar.clone(), self.id.clone(), self.fade.clone());
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
                fade.to(1.0);
                bar.pulse();
            }
            glib::ControlFlow::Continue
        })));
    }

    /// Stop pulsing, the bar left as it is: for a step that measures itself.
    pub(crate) fn stop(&self) {
        if let Some(id) = self.id.take() {
            id.remove();
        }
    }

    /// Fade the bar in, unless it is in or on its way.
    pub(crate) fn show(&self) {
        self.fade.to(1.0);
    }

    /// The wait is over: no pulse, and the bar gone at once, a fade still running stopped
    /// rather than left to bring it back.
    pub(crate) fn hide(&self) {
        self.stop();
        self.fade.set(0.0);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wheel_notch_lands_a_step_on_and_stays_in_range() {
        // A 1000 px page steps 100 px a notch, as GTK's does.
        assert!((wheel_target(0.0, 1.0, 0.0, 5000.0, 1000.0) - 100.0).abs() < 1e-9);
        assert_eq!(wheel_target(3950.0, 1.0, 0.0, 5000.0, 1000.0), 4000.0);
        assert_eq!(wheel_target(50.0, -1.0, 0.0, 5000.0, 1000.0), 0.0);
        // Shorter than its page: nowhere to go.
        assert_eq!(wheel_target(0.0, 1.0, 0.0, 500.0, 1000.0), 0.0);
    }

    #[test]
    fn a_selection_searches_for_its_first_line() {
        assert_eq!(
            search_query("alpha beta\ngamma").as_deref(),
            Some("alpha beta")
        );
        assert_eq!(search_query("alpha\r\n").as_deref(), Some("alpha"));
        assert_eq!(search_query("\nalpha"), None);
        assert_eq!(search_query(""), None);
    }
}
