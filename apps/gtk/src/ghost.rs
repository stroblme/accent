//! Inline suggestions accepted with Tab or one word at a time with Ctrl+Right.
//!
//! The vault's `inline_completion` supplies suggestions; `multicaret::View` paints them
//! outside the buffer, avoiding undo, save and reparse work. They appear only at line ends
//! without a selection or secondary carets.
//!
//! The completion popup suppresses suggestions so Tab has one owner. Typing a matching prefix
//! or accepting a word preserves the remainder; other edits or caret moves clear it.
//! Suggestions occupy one line.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::editor::{Tab, caret};
use crate::lang;

/// Tab-local ghost-text state; the suggestion itself lives on the view that paints it.
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
        self.on.get() && !tab.popup_shown() && tab.lang.support().is_some_and(|s| s.inline)
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
                .filter(|_| !tab.popup_shown() && !tab.is_loading())
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

/// Handle a key for painted ghost text, or return `None` to continue dispatch.
/// Without a suggestion, Tab remains available for list indentation.
pub fn on_key(tab: &Rc<Tab>, key: gdk::Key, state: gdk::ModifierType) -> Option<glib::Propagation> {
    let text = tab.ghost_view().and_then(|v| v.ghost())?;
    Some(match key {
        // Ctrl+Right writes one word of the suggestion; the rest stays painted, because the
        // insert goes through the buffer like a keystroke. Shift means a selection and Alt
        // belongs to the compositor, so both are left alone.
        gdk::Key::Right | gdk::Key::KP_Right
            if state.contains(crate::platform::PRIMARY_MODIFIER)
                && !state
                    .intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SHIFT_MASK) =>
        {
            accept(tab, word_of(&text));
            glib::Propagation::Stop
        }
        _ if state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::META_MASK,
        ) =>
        {
            return None;
        }
        gdk::Key::Tab | gdk::Key::KP_Tab => {
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

/// Return `ghost` after removing prefix `typed`, or `None` when it differs.
/// `Some("")` means the suggestion was fully typed.
fn remainder<'a>(ghost: &'a str, typed: &str) -> Option<&'a str> {
    ghost.strip_prefix(typed)
}

/// Keep a suggestion only when the caret reaches the position predicted by matching typed text.
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

/// Paint the new suggestion and discard pending typed-through state from the previous answer.
fn set(tab: &Tab, text: Option<String>) {
    *tab.lang.ghost.typed.borrow_mut() = None;
    if let Some(view) = tab.ghost_view() {
        view.set_ghost(text);
    }
}

/// Request and paint the rest of the line after `lang::refresh` flushes current text.
/// The next keystroke drops the request and cancels it at the server.
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
