//! The bar a remote vault draws while it is still coming up.
//!
//! A first connection to a host uploads the 6.4 MB server binary before anything else can happen,
//! which is seconds of waiting with a number attached to it. DESIGN.md's Loading rule keeps
//! progress in the status bar as text, and the text stays there; the one bar the app draws
//! belongs to the surface it is about to fill, which for the search pane is the results list and
//! here is the whole document column — a connection is not replacing a list, it is the window
//! becoming usable. So the bar spans that column instead of sitting in the status bar, where a
//! 29 px caption row is no place for a measure and a bar beside the branch readout would shove it
//! across the window every time a vault connects.
//!
//! Faded rather than hidden, the same way the search bar is, so nothing shifts when it comes and
//! goes. A local vault is connected from the moment it opens, so it never gets the widget at all
//! and does not even reserve its height.

use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// One step of the bar while it is showing a wait of unknown length. Same value and the same
/// reason as the search pane's pulse (DESIGN.md, Motion): GTK4 has no indeterminate mode.
const PULSE: Duration = Duration::from_millis(80);

pub struct Bar {
    bar: gtk::ProgressBar,
    /// The timer pulsing [`Bar::bar`], shared with the timer's own closure so it can clear the
    /// slot when it stops itself.
    pulse: Rc<Cell<Option<glib::SourceId>>>,
}

impl Drop for Bar {
    /// A window that goes away takes its pulse timer with it, as the search pane does.
    fn drop(&mut self) {
        self.stop();
    }
}

impl Bar {
    pub fn new() -> Bar {
        Bar {
            bar: gtk::ProgressBar::builder().opacity(0.0).build(),
            pulse: Rc::new(Cell::new(None)),
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.bar.upcast_ref()
    }

    /// A step of the connection is running. `fraction` is how far it has got where the step knows;
    /// where it does not, the bar pulses instead.
    pub fn show(&self, fraction: Option<f64>) {
        self.bar.set_opacity(1.0);
        match fraction {
            Some(f) => {
                self.stop();
                self.bar.set_fraction(f.clamp(0.0, 1.0));
            }
            // Already pulsing: leave the timer alone rather than restarting it on every message,
            // which would hold the bar at the start of its trough.
            None if self.pulsing() => {}
            None => self.start(),
        }
    }

    /// The connection is over, however it ended.
    pub fn hide(&self) {
        self.stop();
        self.bar.set_opacity(0.0);
        self.bar.set_fraction(0.0);
    }

    fn pulsing(&self) -> bool {
        let id = self.pulse.take();
        let running = id.is_some();
        self.pulse.set(id);
        running
    }

    fn start(&self) {
        let (bar, slot) = (self.bar.clone(), self.pulse.clone());
        self.pulse.set(Some(glib::timeout_add_local(PULSE, move || {
            // `Bar` lives in the `App`, which the window's own handlers keep alive, so `Drop` is
            // not guaranteed to run. An unrooted bar means the window closed mid-connection; that
            // is the timer's cue to stop on its own.
            if bar.root().is_none() {
                slot.set(None);
                return glib::ControlFlow::Break;
            }
            bar.pulse();
            glib::ControlFlow::Continue
        })));
    }

    fn stop(&self) {
        if let Some(id) = self.pulse.take() {
            id.remove();
        }
    }
}
