//! Run vault work off the main loop so remote round trips cannot block the UI.
//!
//! These helpers centralise worker dispatch, awaiting results and reporting worker panics, and
//! cut what has to run on the main loop, drawing with GTK, into slices ([`Slices`]).

use std::cell::Cell;
use std::time::{Duration, Instant};

use accent_api::link::Stop;
use gtk::{gio, glib};

/// How long a slice of work on the main loop runs before the window draws a frame and takes its
/// input again: three frames at 60 Hz, a pause nobody reads as a freeze.
const SLICE: Duration = Duration::from_millis(50);

/// What a stop ends work in slices with, which the toast saying how it went keeps quiet about.
pub(crate) const STOPPED: &str = "stopped";

/// Work that has to run on the main loop, as drawing with GTK does, cut into slices of about
/// [`SLICE`]: between two of its steps, [`Slices::pause`] lets the window draw a frame and take
/// its input once the slice is used up, and ends the work once its [`Stop`] is pressed.
pub(crate) struct Slices {
    /// When the slice running began.
    since: Cell<Instant>,
    stop: Stop,
}

impl Slices {
    /// Slices that nothing stops.
    pub(crate) fn new() -> Slices {
        Slices::stopped_by(Stop::default())
    }

    pub(crate) fn stopped_by(stop: Stop) -> Slices {
        Slices {
            since: Cell::new(Instant::now()),
            stop,
        }
    }

    /// Give the main loop back once the slice is used up; [`STOPPED`] once the stop is pressed.
    pub(crate) async fn pause(&self) -> Result<(), String> {
        match self.since.get().elapsed() >= SLICE {
            true => self.yield_now().await,
            false => self.stopped(),
        }
    }

    /// Give the main loop back now, before a step that cannot be cut and so takes a slice of its
    /// own, at a priority below GTK's redraw, so a frame that is due is drawn first.
    pub(crate) async fn yield_now(&self) -> Result<(), String> {
        glib::timeout_future_with_priority(glib::Priority::DEFAULT_IDLE, Duration::ZERO).await;
        self.since.set(Instant::now());
        self.stopped()
    }

    fn stopped(&self) -> Result<(), String> {
        match self.stop.stopped() {
            true => Err(STOPPED.to_string()),
            false => Ok(()),
        }
    }
}

/// Run `work` on a worker thread; log panics under `what` and return `None`.
///
/// Callers choose how to present failures. [`attempt`] combines worker and operation failures
/// into the normal user-facing error format.
pub(crate) async fn off_thread<T: Send + 'static>(
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    caught(what, work).await.ok()
}

/// [`off_thread`], handing back what a panic said for a caller that shows it.
pub(crate) async fn caught<T: Send + 'static>(
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    gio::spawn_blocking(work).await.map_err(|payload| {
        let why = panic_message(payload.as_ref());
        tracing::warn!("the {what} worker panicked: {why}");
        why
    })
}

/// A panic's message: `panic!` and the standard library's own panics give a `&str` or a `String`.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    let text = payload.downcast_ref::<&str>().copied();
    text.or(payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic without a message")
        .to_string()
}

/// Run fallible work through [`off_thread`], formatting operation failures and worker panics
/// as `Cannot {what}: {why}` (`App::cannot`). This prevents worker panics from failing silently.
pub(crate) async fn attempt<T: Send + 'static, E: std::fmt::Display + Send + 'static>(
    what: &str,
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, String> {
    match off_thread(what, work).await {
        Some(Ok(answer)) => Ok(answer),
        Some(Err(e)) => Err(format!("Cannot {what}: {e:#}")),
        None => Err(format!("Cannot {what}: the worker stopped")),
    }
}

/// Return optional reader data, logging failures instead of confusing them with empty results.
/// Superseded calls need no failure report.
pub fn or_empty<T: Default>(what: &str, answer: accent_api::Result<T>) -> T {
    answer.unwrap_or_else(|e| {
        if e != accent_api::Error::Core(accent_core::Error::Cancelled) {
            tracing::warn!("{what}: {e}");
        }
        T::default()
    })
}

#[cfg(test)]
mod tests {
    use super::{STOPPED, Slices, Stop, panic_message};

    #[test]
    fn a_stop_ends_work_in_slices_at_its_next_step() {
        let stop = Stop::default();
        let slices = Slices::stopped_by(stop.clone());
        let context = gtk::glib::MainContext::new();
        assert_eq!(context.block_on(slices.pause()), Ok(()));
        stop.stop();
        assert_eq!(context.block_on(slices.pause()), Err(STOPPED.to_string()));
    }

    #[test]
    fn a_panic_says_what_it_said() {
        let text = String::from("é");
        let sliced = std::panic::catch_unwind(|| text[..1].len()).unwrap_err();
        assert!(panic_message(sliced.as_ref()).contains("not a char boundary"));
        let plain = std::panic::catch_unwind(|| panic!("plain")).unwrap_err();
        assert_eq!(panic_message(plain.as_ref()), "plain");
    }
}
