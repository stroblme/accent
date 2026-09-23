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

    /// Drop whatever is pending, for the keystroke that is answered on the spot instead.
    pub(crate) fn cancel(&self) {
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
    }
}
