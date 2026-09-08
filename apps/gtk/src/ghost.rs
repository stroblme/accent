//! Ghost text: the rest of the line, suggested where the caret is and accepted with Tab.
//!
//! The suggestion comes from the vault's own notes, through the language layer's
//! `inline_completion` (merl answers it; see `crates/api/src/language.rs`). It is never in the
//! buffer — `multicaret::View` paints it — so it costs no undo step, no save and no reparse, and
//! it disappears the moment anything moves.
//!
//! Two rules keep it out of the way of everything else:
//!
//! * **The popup wins.** While the completion popup is up nothing is asked for and nothing is
//!   painted, so Tab always means one thing: the selected row if the popup is open, the
//!   suggestion if it is not. Which of the two key controllers GTK reaches first is not
//!   something `typing.rs` relies on either, and for the same reason.
//! * **Only at the end of a line**, with no selection and no secondary carets. A suggestion
//!   painted mid-line would sit on top of the text after the caret, and moving that text out of
//!   the way is a widget of its own.
//!
//! ponytail: end of line only, one line at a time, and the whole suggestion or none of it. Word
//! by word (Ctrl+Right in VS Code) and a mid-line ghost are the two obvious extensions.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::editor::Tab;
use crate::lang;

/// What this tab knows about its ghost text. The suggestion itself lives on the view, which is
/// what paints it.
#[derive(Default)]
pub struct State {
    /// The preference. Off means nothing is asked for and nothing is shown, at once.
    pub on: Cell<bool>,
    /// The completion popup is up, so the ghost path stands aside.
    popup: Cell<bool>,
}

impl State {
    /// Whether a suggestion may be asked for at all, before looking at where the caret is.
    fn armed(&self, tab: &Tab) -> bool {
        self.on.get() && !self.popup.get() && tab.lang.support().is_some_and(|s| s.inline)
    }
}

/// Watch the tab for everything that ends a suggestion, and take Tab and Escape while one is up.
pub fn install(tab: &Rc<Tab>) {
    tab.lang.ghost.on.set(tab.ghost_text_wanted());

    let completion = sourceview5::prelude::ViewExt::completion(&tab.view);
    completion.connect_show(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| {
            tab.lang.ghost.popup.set(true);
            clear(&tab);
        }
    ));
    completion.connect_hide(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.lang.ghost.popup.set(false)
    ));

    // Any edit and any move of the caret makes the suggestion about the wrong place. The next
    // one arrives with the post-edit refresh, 300 ms after the typing stops.
    tab.buffer.connect_changed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| clear(&tab)
    ));
    tab.buffer.connect_cursor_position_notify(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| clear(&tab)
    ));

    // Capture, so Tab is taken before the view turns it into an indent. Everything else is left
    // alone, including Tab with no suggestion up.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, state| on_key(&tab, key, state)
    ));
    tab.view.add_controller(keys);
}

fn on_key(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
    let showing = tab.ghost_view().and_then(|v| v.ghost());
    let Some(text) = showing.filter(|_| !tab.lang.ghost.popup.get()) else {
        return glib::Propagation::Proceed;
    };
    if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
        return glib::Propagation::Proceed;
    }
    match key {
        gdk::Key::Tab | gdk::Key::KP_Tab => {
            accept(tab, &text);
            glib::Propagation::Stop
        }
        gdk::Key::Escape => {
            clear(tab);
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    }
}

/// Write the suggestion where it was painted, as one undo step.
fn accept(tab: &Rc<Tab>, text: &str) {
    clear(tab);
    tab.buffer.begin_user_action();
    tab.buffer.insert_at_cursor(text);
    tab.buffer.end_user_action();
}

/// Take the suggestion off the screen. Cheap enough to call from every signal that could
/// invalidate one.
pub fn clear(tab: &Tab) {
    if let Some(view) = tab.ghost_view() {
        view.set_ghost(None);
    }
}

/// Ask for the rest of the line, and paint what comes back.
///
/// Awaited from `lang::refresh`, so the text has already been flushed to the provider and the
/// request is dropped — and cancelled at the server — on the next keystroke, like every other
/// request that follows the caret.
pub async fn request(tab: &Rc<Tab>) {
    if !tab.lang.ghost.armed(tab) {
        return;
    }
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
    // Mid-line, a selection, or a column of carets: not a place a suggestion can be drawn.
    if !caret.ends_line()
        || tab.buffer.has_selection()
        || tab.ghost_view().is_some_and(|v| v.has_carets())
    {
        return;
    }
    let (rel, pos) = (tab.rel(), lang::pos_of(&caret));
    match vault.inline_completion(&rel, pos).await {
        Ok(Some(text)) => {
            tracing::debug!("ghost for {rel} at {pos:?}: {text:?}");
            // The answer took a round trip; the caret may have moved on since.
            if tab.lang.ghost.armed(tab)
                && lang::pos_of(&caret_of(tab)) == pos
                && let Some(view) = tab.ghost_view()
            {
                view.set_ghost(Some(text));
            }
        }
        Ok(None) => clear(tab),
        Err(e) => tracing::debug!("ghost for {rel}: {e:#}"),
    }
}

fn caret_of(tab: &Tab) -> gtk::TextIter {
    tab.buffer.iter_at_mark(&tab.buffer.get_insert())
}
