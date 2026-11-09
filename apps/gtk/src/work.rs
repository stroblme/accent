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
    use super::panic_message;

    #[test]
    fn a_panic_says_what_it_said() {
        let text = String::from("é");
        let sliced = std::panic::catch_unwind(|| text[..1].len()).unwrap_err();
        assert!(panic_message(sliced.as_ref()).contains("not a char boundary"));
        let plain = std::panic::catch_unwind(|| panic!("plain")).unwrap_err();
        assert_eq!(panic_message(plain.as_ref()), "plain");
    }
}
