//! Ghost text: the rest of the line, suggested where the caret is, accepted with Tab or one
//! word at a time with Ctrl+Right.
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
//! Typing what is painted does not chase it away: `connect_insert_text` sees the characters
//! before they land, and what is left of the suggestion is painted again the moment the caret
//! reaches them. Ctrl+Right does the same thing on purpose, writing one word of the suggestion
//! and leaving the rest standing.
//!
//! ponytail: end of line only, one line at a time. A mid-line ghost is the obvious extension.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::editor::{Tab, caret};
use crate::lang;

/// What this tab knows about its ghost text. The suggestion itself lives on the view, which is
/// what paints it.
#[derive(Default)]
pub struct State {
    /// The preference. Off means nothing is asked for and nothing is shown, at once.
    pub on: Cell<bool>,
    /// A suggestion being typed through: what is left of it, and the caret offset it belongs
    /// at once the insert that was announced has landed.
    typed: RefCell<Option<(String, i32)>>,
}

impl State {
    /// Whether a suggestion may be asked for at all, before looking at where the caret is.
    fn armed(&self, tab: &Tab) -> bool {
        self.on.get() && !tab.popup_shown.get() && tab.lang.support().is_some_and(|s| s.inline)
    }
}

/// Watch the tab for everything that ends a suggestion. Which keys reach [`on_key`] is
/// `editor::keys`'s decision, and so is the popup's own show and hide.
pub fn install(tab: &Rc<Tab>) {
    tab.lang.ghost.on.set(tab.ghost_text_wanted());

    // Before the insert lands, so the suggestion is still up and the location iter still says
    // where the characters go: what is typed on top of a suggestion only shortens it.
    tab.buffer.connect_insert_text(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_, at, text| {
            let showing = tab
                .ghost_view()
                .and_then(|v| v.ghost())
                .filter(|_| !tab.popup_shown.get())
                .filter(|_| at.offset() == caret(&tab.buffer).offset());
            *tab.lang.ghost.typed.borrow_mut() = showing.and_then(|ghost| {
                let rest = remainder(&ghost, text)?.to_string();
                Some((rest, at.offset() + text.chars().count() as i32))
            });
        }
    ));

    // Any edit and any move of the caret makes the suggestion about the wrong place, unless the
    // move is the one the typing above just announced. The next suggestion arrives with the
    // post-edit refresh, 100 ms after the typing stops.
    tab.buffer.connect_changed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| settle(&tab)
    ));
    tab.buffer.connect_cursor_position_notify(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| settle(&tab)
    ));
}

/// What a painted suggestion does with a key, or `None` for a key it does not want. `indenting`
/// is the one case where Tab is not the suggestion's: a line holding only a list marker, where
/// the key belongs to the item's indent.
pub fn on_key(
    tab: &Rc<Tab>,
    key: gdk::Key,
    state: gdk::ModifierType,
    indenting: bool,
) -> Option<glib::Propagation> {
    let text = tab.ghost_view().and_then(|v| v.ghost())?;
    Some(match key {
        // Ctrl+Right writes one word of the suggestion; the rest stays painted, because the
        // insert goes through the buffer like a keystroke. Shift means a selection and Alt
        // belongs to the compositor, so both are left alone.
        gdk::Key::Right | gdk::Key::KP_Right
            if state.contains(gdk::ModifierType::CONTROL_MASK)
                && !state
                    .intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SHIFT_MASK) =>
        {
            accept(tab, word_of(&text));
            glib::Propagation::Stop
        }
        _ if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) => {
            return None;
        }
        gdk::Key::Tab | gdk::Key::KP_Tab if !indenting => {
            accept(tab, &text);
            glib::Propagation::Stop
        }
        gdk::Key::Escape => {
            clear(tab);
            glib::Propagation::Stop
        }
        _ => return None,
    })
}

/// Write `text` where the suggestion was painted, as one undo step. Whatever is left of the
/// suggestion is kept by the same path a keystroke takes: the insert is one.
fn accept(tab: &Rc<Tab>, text: &str) {
    tab.buffer.begin_user_action();
    tab.buffer.insert_at_cursor(text);
    tab.buffer.end_user_action();
}

/// The first word of a suggestion, through the whitespace after it: what Ctrl+Right writes, so
/// that the next press starts on a word rather than on the space before it.
fn word_of(text: &str) -> &str {
    let after = text
        .trim_start()
        .trim_start_matches(|c: char| !c.is_whitespace());
    &text[..text.len() - after.trim_start().len()]
}

/// What is left of `ghost` once `typed` has been written at its head, or `None` when the two
/// have parted company. `Some("")` means the suggestion has been typed out in full.
fn remainder<'a>(ghost: &'a str, typed: &str) -> Option<&'a str> {
    ghost.strip_prefix(typed)
}

/// The buffer changed or the caret moved. A suggestion survives exactly one thing: the caret
/// arriving where the characters typed into it said it would.
fn settle(tab: &Tab) {
    let typed = tab.lang.ghost.typed.borrow().clone();
    match typed {
        Some((rest, at)) if at == caret(&tab.buffer).offset() => {
            if let Some(view) = tab.ghost_view() {
                view.set_ghost((!rest.is_empty()).then_some(rest));
            }
        }
        _ => clear(tab),
    }
}

/// Take the suggestion off the screen. Cheap enough to call from every signal that could
/// invalidate one.
pub fn clear(tab: &Tab) {
    set(tab, None);
}

/// Paint a suggestion, or none, and forget the one being typed through: this is the suggestion
/// now, and what was left of the old one is about the text before this answer.
fn set(tab: &Tab, text: Option<String>) {
    *tab.lang.ghost.typed.borrow_mut() = None;
    if let Some(view) = tab.ghost_view() {
        view.set_ghost(text);
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
    let at = caret(&tab.buffer);
    // Mid-line, a selection, or a column of carets: not a place a suggestion can be drawn. Nor is
    // a line holding only its indent and a list marker, where Tab is the item's indent and a
    // suggestion would be in the way of writing the list at all.
    if !at.ends_line()
        || tab.buffer.has_selection()
        || tab.ghost_view().is_some_and(|v| v.has_carets())
        || crate::typing::marker_only(&crate::editor::line_prefix(&tab.buffer, &at))
    {
        return;
    }
    let (rel, pos) = (tab.rel(), lang::pos_of(&at));
    match vault.inline_completion(&rel, pos).await {
        Ok(Some(text)) => {
            tracing::debug!("ghost for {rel} at {pos:?}: {text:?}");
            // The answer took a round trip; the caret may have moved on since.
            if tab.lang.ghost.armed(tab) && lang::pos_of(&caret(&tab.buffer)) == pos {
                set(tab, Some(text));
            }
        }
        Ok(None) => clear(tab),
        Err(e) => tracing::debug!("ghost for {rel}: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Typing what is painted leaves the rest of it; anything else ends the suggestion.
    #[test]
    fn typing_through_a_suggestion_leaves_what_is_left_of_it() {
        assert_eq!(remainder("hello", "h"), Some("ello"));
        assert_eq!(remainder("hello", "hello"), Some(""));
        assert_eq!(remainder("hello", "j"), None);
    }

    /// One word and the space after it, wherever the suggestion starts.
    #[test]
    fn a_word_is_taken_with_the_space_behind_it() {
        assert_eq!(word_of("hello world"), "hello ");
        assert_eq!(word_of(" the rest"), " the ");
        assert_eq!(word_of("hello"), "hello");
    }
}
