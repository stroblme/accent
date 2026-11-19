//! Show connection progress over the document column while a remote vault opens, and how far a
//! print or an export being made has got.
//!
//! The bar belongs to the surface becoming usable; status-bar text still reports the step.
//! It fades in and goes without changing the layout. A local vault reserves no space for it: its
//! window shows the bar over the top of the tabs, and only for a print or an export.

use crate::widgets::Pulse;
use gtk::prelude::*;
use std::cell::Cell;
use std::time::Duration;

/// Pulse increment for unknown-duration waits; GTK4 has no indeterminate progress mode.
/// Matches Search progress (DESIGN.md, Motion).
const PULSE: Duration = Duration::from_millis(80);

/// Inset connection progress to align with the first and last tabs beneath it.
const TAB_INSET: i32 = 6;

pub struct Bar {
    bar: gtk::ProgressBar,
    pulse: Pulse,
    /// Whether a connection is coming up, which the bar speaks for first.
    connecting: Cell<bool>,
    /// How many prints and exports hold the bar, which stays up until the last lets go.
    jobs: Cell<usize>,
}

impl Bar {
    pub fn new() -> Bar {
        let bar = gtk::ProgressBar::builder()
            .opacity(0.0)
            .margin_start(TAB_INSET)
            .margin_end(TAB_INSET)
            .valign(gtk::Align::Start)
            .can_target(false)
            .build();
        Bar {
            pulse: Pulse::new(&bar),
            bar,
            connecting: Cell::new(false),
            jobs: Cell::new(0),
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.bar.upcast_ref()
    }

    /// Show connection-step progress with `fraction`, or pulse when the duration is unknown.
    pub fn show(&self, fraction: Option<f64>) {
        self.connecting.set(true);
        self.put(fraction);
    }

    /// The connection is over, however it ended: the bar goes, unless a print or an export
    /// holds it.
    pub fn hide(&self) {
        self.connecting.set(false);
        self.clear();
    }

    /// A print or an export has run long enough to be shown: it holds the bar until it lets go
    /// ([`Bar::release`]), at `fraction` or pulsing.
    pub fn hold(&self, fraction: Option<f64>) {
        self.jobs.set(self.jobs.get() + 1);
        self.advance(fraction);
    }

    /// How far a print or an export holding the bar has got.
    pub fn advance(&self, fraction: Option<f64>) {
        if !self.connecting.get() {
            self.put(fraction);
        }
    }

    /// A print or an export holding the bar is done with it.
    pub fn release(&self) {
        self.jobs.set(self.jobs.get().saturating_sub(1));
        self.clear();
    }

    fn put(&self, fraction: Option<f64>) {
        self.pulse.show();
        match fraction {
            Some(f) => {
                self.pulse.stop();
                self.bar.set_fraction(f.clamp(0.0, 1.0));
            }
            // Already pulsing leaves the timer alone rather than restarting it on every
            // message, which would hold the bar at the start of its trough.
            None => self.pulse.start(PULSE, 0),
        }
    }

    /// The bar gone, once nothing holds it.
    fn clear(&self) {
        if !self.connecting.get() && self.jobs.get() == 0 {
            self.pulse.hide();
            self.bar.set_fraction(0.0);
        }
    }

    /// What the bar shows, for the drills: `off` while it is faded out, `pulse`, or how far
    /// along it is.
    #[cfg(feature = "bench")]
    pub fn shown(&self) -> String {
        match (self.bar.opacity() > 0.0, self.pulse.running()) {
            (false, _) => "off".to_string(),
            (true, true) => "pulse".to_string(),
            (true, false) => format!("{:.2}", self.bar.fraction()),
        }
    }
}
