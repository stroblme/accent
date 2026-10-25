//! The Code pane's goals: what a proof assistant says holds at the caret of the tab in front.

use super::*;

/// DESIGN.md, Motion: the goals follow the caret by 300 ms, as the References section does.
const GOALS: Duration = Duration::from_millis(300);

impl App {
    /// The tab the Code pane is about: the one in front where the pane has something to say for
    /// it — goals, a debugger, a kernel — else the last such tab in front, while it is open, so
    /// a terminal or a note brought forward leaves the pane as it was (user decision 2026-10-25).
    pub(crate) fn code_doc(&self) -> Option<Doc> {
        if let Some(doc) = self.active_doc().filter(|doc| self.code_is_for(doc)) {
            self.code_tab.replace(Some(doc.key()));
            return Some(doc);
        }
        let key = self.code_tab.borrow().clone()?;
        let doc = self.doc_for(&key).filter(|doc| self.code_is_for(doc));
        if doc.is_none() {
            self.code_tab.replace(None);
        }
        doc
    }

    /// Whether the Code pane has something to say for `doc`: goals, a debugger or a kernel.
    fn code_is_for(&self, doc: &Doc) -> bool {
        match doc {
            Doc::Text(tab) => {
                tab.lang.support().is_some_and(|s| s.goals) || self.program_of(tab).is_some()
            }
            Doc::Notebook(tab) => self.why_not_run(tab).is_none(),
            _ => false,
        }
    }

    /// Show Code while the window has a tab it is for (`code_doc`) or a debug session runs, and
    /// request goals. Window-owned visibility keeps the debugger's sections up over any tab.
    pub fn sync_code_pane(self: &Rc<Self>) {
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let running = self.debugger.running();
        let debuggable = self.code_program().is_some();
        sidebar.set_code_visible(self.code_doc().is_some() || running);
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

    /// Request goals at the caret of the Code pane's tab only while Code is visible, with
    /// debounce and cancellation. Retain previous answers during requests to avoid flashing;
    /// clear when the pane is about another tab.
    pub fn refresh_goals(self: &Rc<Self>) {
        if let Some(handle) = self.goals.borrow_mut().take() {
            handle.abort();
        }
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        let tab = match self.code_doc() {
            Some(Doc::Text(tab)) => Some(tab),
            _ => None,
        };
        let key = self.code_tab.borrow().clone().unwrap_or_default();
        let checking = tab
            .as_ref()
            .is_some_and(|tab| tab.lang.checking.get().is_some());
        sidebar.sync_code(&key, checking);
        if !self.sidebar_column.is_visible() || !sidebar.is_showing("code") {
            return;
        }
        let Some(tab) = tab.filter(|tab| tab.lang.support().is_some_and(|s| s.goals)) else {
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
            if app.code_tab.borrow().as_deref() != Some(&key) {
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
