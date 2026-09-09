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

use crate::widgets::Pulse;
use gtk::prelude::*;
use std::time::Duration;

/// One step of the bar while it is showing a wait of unknown length. Same value and the same
/// reason as the search pane's pulse (DESIGN.md, Motion): GTK4 has no indeterminate mode.
const PULSE: Duration = Duration::from_millis(80);

pub struct Bar {
    bar: gtk::ProgressBar,
    pulse: Pulse,
}

impl Bar {
    pub fn new() -> Bar {
        let bar = gtk::ProgressBar::builder().opacity(0.0).build();
        Bar {
            pulse: Pulse::new(&bar),
            bar,
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
                self.pulse.stop();
                self.bar.set_fraction(f.clamp(0.0, 1.0));
            }
            // Already pulsing leaves the timer alone rather than restarting it on every
            // message, which would hold the bar at the start of its trough.
            None => self.pulse.start(PULSE, 0),
        }
    }

    /// The connection is over, however it ended.
    pub fn hide(&self) {
        self.pulse.stop();
        self.bar.set_opacity(0.0);
        self.bar.set_fraction(0.0);
    }
}
