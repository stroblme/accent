//! Show how far a remote vault's connection, or a print or an export being made, has got, in one
//! bar right above the status bar, whatever pane started it.
//!
//! The status bar's text still says what the step is. The bar lies over the bottom of the
//! document column, so it reserves no height, and fades in and goes without changing the layout.

use crate::widgets::Pulse;
use gtk::prelude::*;
use std::cell::Cell;
use std::time::Duration;

/// Pulse increment for unknown-duration waits; GTK4 has no indeterminate progress mode.
/// Matches Search progress (DESIGN.md, Motion).
const PULSE: Duration = Duration::from_millis(80);

/// Inset the bar as the status bar's padding insets its line beneath it (`.accent-statusbar`).
const INSET: i32 = 12;

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
            .margin_start(INSET)
            .margin_end(INSET)
            .valign(gtk::Align::End)
            .can_target(false)
            .build();
        // Lifted with the toasts while presentation's status bar comes up over the document.
        bar.add_css_class("accent-progress");
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
