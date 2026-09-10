//! Coming back after a dropped link without being asked.
//!
//! A remote window that was connected and lost it tries again on its own, after 1, 2, 4, 8 and
//! 16 seconds and then every 30, each attempt a quiet one that cannot raise a prompt. The banner
//! counts down to the next ("Lost the connection to host · Reconnecting in 8 s"), and its
//! Reconnect Now is the attempt that may ask for a passphrase. A first connection that failed,
//! and an outage that has outlasted ten minutes, keep the plain Reconnect button: there was
//! nothing to come back to, or it is not coming back by itself.

use super::*;

/// How long after the drop the automatic attempts stop.
const GIVE_UP: Duration = Duration::from_secs(10 * 60);

/// The wait before the `n`th automatic attempt after a drop, counting from zero.
pub fn backoff(n: u32) -> Duration {
    Duration::from_secs(match n {
        0..=4 => 1 << n,
        _ => 30,
    })
}

/// Where a window's automatic reconnect has got to.
#[derive(Default)]
pub struct Retry {
    /// Whether the vault has ever answered: only a link that was up is worth waiting for.
    was_up: Cell<bool>,
    /// When the link went, for [`GIVE_UP`]. Kept until the vault answers again, so once the
    /// attempts have stopped a failed Reconnect does not start them over.
    since: Cell<Option<Instant>>,
    /// What the banner says was lost, across the failed attempts that follow it.
    lost: RefCell<String>,
    /// The attempts so far, which is what the next wait grows with.
    attempts: Cell<u32>,
    /// The once-a-second tick counting down to the next attempt.
    tick: RefCell<Option<glib::SourceId>>,
}

impl Retry {
    fn stop(&self) {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
    }
}

impl App {
    /// The vault answers: nothing left to count down to, and the next drop starts from one
    /// second again.
    pub fn connection_up(&self) {
        let retry = &self.retry;
        retry.stop();
        retry.was_up.set(true);
        retry.since.set(None);
        retry.attempts.set(0);
        self.hide_connection_banner();
    }

    /// The link went, or an attempt to bring it back failed.
    pub fn connection_down(self: &Rc<Self>, why: &str) {
        let retry = &self.retry;
        retry.stop();
        if !retry.was_up.get() {
            return self.show_connection_banner(why);
        }
        let since = match retry.since.get() {
            Some(since) => {
                tracing::info!("reconnecting to {}: {why}", self.host());
                since
            }
            None => {
                retry.lost.replace(why.to_string());
                retry.attempts.set(0);
                let now = Instant::now();
                retry.since.set(Some(now));
                now
            }
        };
        if since.elapsed() >= GIVE_UP {
            return self.show_connection_banner(why);
        }
        let n = retry.attempts.get();
        retry.attempts.set(n + 1);
        self.count_down(Instant::now() + backoff(n));
    }

    /// The banner's button: an attempt that may prompt, whatever was counting down.
    pub fn reconnect_now(&self) {
        self.retry.stop();
        self.connection.set_title("Reconnecting…");
        self.connection.set_sensitive(false);
        if let Some(vault) = self.vault() {
            vault.reconnect();
        }
    }

    /// Say when the next attempt is, every second until it is made. The tick holds the window
    /// weakly, so a window closed mid-count stops it within the second.
    fn count_down(self: &Rc<Self>, at: Instant) {
        self.say_count(at);
        self.connection.set_button_label(Some("Reconnect Now"));
        self.connection.set_sensitive(true);
        self.connection.set_revealed(true);
        let app = Rc::downgrade(self);
        let tick = glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(app) = app.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if Instant::now() < at {
                app.say_count(at);
                return glib::ControlFlow::Continue;
            }
            // Gone from the slot before it returns, so nothing removes it a second time.
            app.retry.tick.take();
            app.connection.set_title("Reconnecting…");
            app.connection.set_sensitive(false);
            if let Some(remote) = app.vault().and_then(|v| v.remote()) {
                remote.reconnect_quietly();
            }
            glib::ControlFlow::Break
        });
        self.retry.tick.replace(Some(tick));
    }

    fn say_count(&self, at: Instant) {
        let left = at.saturating_duration_since(Instant::now()).as_secs_f64();
        self.connection.set_title(&format!(
            "{} · Reconnecting in {} s",
            self.retry.lost.borrow(),
            left.ceil().max(1.0) as u64
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_doubles_to_sixteen_seconds_and_then_holds_at_thirty() {
        let waits: Vec<u64> = (0..8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(backoff(u32::MAX), Duration::from_secs(30));
    }
}
