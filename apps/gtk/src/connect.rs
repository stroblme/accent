//! Show connection progress over the document column while a remote vault opens.
//!
//! The bar belongs to the surface becoming usable; status-bar text still reports the step.
//! It fades without changing the layout. Local vaults need no bar or reserved space.

use crate::widgets::Pulse;
use gtk::prelude::*;
use std::time::Duration;

/// Pulse increment for unknown-duration waits; GTK4 has no indeterminate progress mode.
/// Matches Search progress (DESIGN.md, Motion).
const PULSE: Duration = Duration::from_millis(80);

/// Inset connection progress to align with the first and last tabs beneath it.
const TAB_INSET: i32 = 6;

pub struct Bar {
    bar: gtk::ProgressBar,
    pulse: Pulse,
}

impl Bar {
    pub fn new() -> Bar {
        let bar = gtk::ProgressBar::builder()
            .opacity(0.0)
            .margin_start(TAB_INSET)
            .margin_end(TAB_INSET)
            .build();
        Bar {
            pulse: Pulse::new(&bar),
            bar,
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.bar.upcast_ref()
    }

    /// Show connection-step progress with `fraction`, or pulse when the duration is unknown.
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
