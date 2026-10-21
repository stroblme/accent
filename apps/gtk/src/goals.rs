//! The Code pane's goals: what a proof assistant says holds at the caret of the tab in front.

use super::*;

/// DESIGN.md, Motion: the goals follow the caret by 300 ms, as the References section does.
const GOALS: Duration = Duration::from_millis(300);

impl App {
    /// Show Code while the tab in front has goals or can be debugged or run, a notebook's kernel,
    /// or a debug session runs, and request goals. Window-owned visibility keeps the debugger's
    /// sections up over any tab.
    pub fn sync_code_pane(self: &Rc<Self>) {
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let goals = self
            .active()
            .and_then(|tab| tab.lang.support())
            .is_some_and(|support| support.goals);
        let running = self.debugger.running();
        let debuggable = self.debuggable_in_front().is_some();
        let runnable = self.runnable_in_front();
        sidebar.set_code_visible(goals || running || debuggable || runnable);
        self.sync_kernel();
        if !running {
            sidebar.clear_debug(debuggable);
            self.refill_watches();
        }
        if running || debuggable {
            self.refill_breakpoints();
            self.refill_exceptions();
        }
        self.refresh_goals();
    }

    /// Request active-caret goals only while Code is visible, with debounce and cancellation.
    /// Retain previous answers during requests to avoid flashing; clear on tab switches.
    pub fn refresh_goals(self: &Rc<Self>) {
        if let Some(handle) = self.goals.borrow_mut().take() {
            handle.abort();
        }
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let key = self.active_key().unwrap_or_default();
        let checking = self
            .active()
            .is_some_and(|tab| tab.lang.checking.get().is_some());
        sidebar.sync_code(&key, checking);
        if !self.sidebar_column.is_visible() || !sidebar.is_showing("code") {
            return;
        }
        let Some(tab) = self
            .active()
            .filter(|tab| tab.lang.support().is_some_and(|s| s.goals))
        else {
            return;
        };
        let Some(vault) = tab.lang.vault() else {
            return;
        };
        let pos = lang::pos_of(&tab.buffer.iter_at_mark(&tab.buffer.get_insert()));
        let weak = Rc::downgrade(self);
        let handle = glib::spawn_future_local(async move {
            glib::timeout_future(GOALS).await;
            lang::flush(tab.clone()).await;
            let answer = vault.goals(&key, pos).await;
            let Some(app) = weak.upgrade() else { return };
            if app.active_key().as_deref() != Some(&key) {
                return;
            }
            // A position the server has not checked yet can outlast the request; the end of the
            // check asks again (`Event::Checking`), so the pane keeps what it has until then.
            let goals = match answer {
                Err(_) if tab.lang.checking.get().is_some() => return,
                answer => crate::work::or_empty("asking for the goals", answer),
            };
            if let Some(sidebar) = app.sidebar.get() {
                let font = tab.view.pango_context().font_description();
                sidebar.set_goals(&key, goals.as_ref(), font.as_ref());
            }
        });
        *self.goals.borrow_mut() = Some(handle);
    }
}
