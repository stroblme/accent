//! Work that leaves the main loop, in one shape.
//!
//! Every call into the vault costs a round trip on a remote one, so it goes to a worker thread
//! and its answer comes back through the main context. That is three things at each call site —
//! the worker, the `await`, and what to do when the worker itself panicked — and they were
//! written out at fifty-odd of them, in as many wordings. Here they are written once.

use gtk::gio;

/// Run `work` on a worker thread and hand back its answer; `None` once the worker panicked,
/// which is said in the log under `what` and nowhere else.
///
/// A panic on a worker is a bug rather than a condition, so it has no user-facing wording of its
/// own: a caller that must tell the user something says it in the same breath as the failure it
/// was already prepared for. [`attempt`] is that caller's shorter way.
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

/// [`off_thread`] for work that can fail, with both failures worded the way every failure toast
/// in the window is worded (`App::cannot`): "Cannot {what}: {why}".
///
/// A panicked worker reads as one more way for the call to fail, which is what keeps a caller
/// from having an arm that reports nothing — the shape that left Replace in Files and the git
/// commands silent when their worker died.
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

/// A reader's answer, or nothing where it can do without one: a list the palette fills, the tail
/// of a search. A failure is said in the log rather than read as an empty answer nobody could tell
/// from a real one, except a call nobody was waiting for any more.
pub fn or_empty<T: Default>(what: &str, answer: accent_api::Result<T>) -> T {
    answer.unwrap_or_else(|e| {
        if e != accent_api::Error::Core(accent_core::Error::Cancelled) {
            tracing::warn!("{what}: {e}");
        }
        T::default()
    })
}
