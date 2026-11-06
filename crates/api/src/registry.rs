//! The bookkeeping a vault's debug sessions and kernels share: each is kept by the number its
//! caller picked once its start is done, and the vault's shutdown stops them all. How one stops,
//! and when it leaves the registry, is its protocol's and stays with its owner.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Error, Result, locked};

/// Running sessions by number, none kept after [`Registry::shutdown`].
pub(crate) struct Registry<S> {
    /// What a session is called when a number names none: `kernel`.
    called: &'static str,
    sessions: Mutex<HashMap<u64, Arc<S>>>,
    /// Raised by [`Registry::shutdown`] before it takes the sessions: a start still under way
    /// holds the registry's owner, so it outlives the vault's end and must not register after it.
    closed: AtomicBool,
}

impl<S> Registry<S> {
    pub(crate) fn new(called: &'static str) -> Registry<S> {
        Registry {
            called,
            sessions: Mutex::default(),
            closed: AtomicBool::new(false),
        }
    }

    /// Keep `session` as `id`; false once the vault has shut down, the caller then stopping it.
    pub(crate) fn register(&self, id: u64, session: &Arc<S>) -> bool {
        let mut sessions = locked(&self.sessions);
        // Read under the lock `shutdown` drains through, so a session is either drained or refused.
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        sessions.insert(id, session.clone());
        true
    }

    pub(crate) fn get(&self, id: u64) -> Result<Arc<S>> {
        locked(&self.sessions)
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::Language(format!("no {} {id}", self.called)))
    }

    pub(crate) fn remove(&self, id: u64) -> Option<Arc<S>> {
        locked(&self.sessions).remove(&id)
    }

    /// Refuse every later registration and `stop` each session in turn, within 2 s in all,
    /// past which the rest are only dropped. Blocks on the runtime, as `Languages::shutdown`
    /// does, and starts none when nothing runs.
    pub(crate) fn shutdown(&self, stop: impl AsyncFn(&S)) {
        self.closed.store(true, Ordering::SeqCst);
        let sessions: Vec<Arc<S>> = locked(&self.sessions).drain().map(|(_, s)| s).collect();
        if sessions.is_empty() {
            return;
        }
        accent_lsp::runtime().block_on(async {
            let all = async {
                for session in &sessions {
                    stop(session).await;
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(2), all).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shutdown_stops_what_it_holds_and_refuses_the_rest() {
        let registry = Registry::new("flag");
        let held = Arc::new(AtomicBool::new(false));
        assert!(registry.register(1, &held));
        registry.shutdown(async |flag: &AtomicBool| flag.store(true, Ordering::SeqCst));
        assert!(held.load(Ordering::SeqCst), "the held session was stopped");
        assert!(registry.get(1).is_err());
        assert!(!registry.register(2, &Arc::new(AtomicBool::new(false))));
        assert!(registry.get(2).is_err());
    }
}
