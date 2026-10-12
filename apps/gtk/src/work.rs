//! Run vault work off the main loop so remote round trips cannot block the UI.
//!
//! These helpers centralise worker dispatch, awaiting results and reporting worker panics.

use gtk::gio;

/// Run `work` on a worker thread; log panics under `what` and return `None`.
///
/// Callers choose how to present failures. [`attempt`] combines worker and operation failures
/// into the normal user-facing error format.
pub(crate) async fn off_thread<T: Send + 'static>(
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    match gio::spawn_blocking(work).await {
        Ok(answer) => Some(answer),
        Err(_) => {
            tracing::warn!("the {what} worker panicked");
            None
        }
    }
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
