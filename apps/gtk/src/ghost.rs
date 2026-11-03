//! Inline suggestions accepted with Tab or one word at a time with Ctrl+Right, in any
//! [`Editor`] that keeps a [`State`]: a text tab, and a notebook's markdown cell.
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

use crate::editor::caret;
use crate::lang::{self, Editor};
use crate::multicaret;

/// An editor's ghost-text state; the suggestion itself lives on the view that paints it.
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
    fn armed(&self, editor: &dyn Editor) -> bool {
        self.on.get()
            && !crate::completion::is_shown(editor)
            && editor.document().support().is_some_and(|s| s.inline)
    }
}

/// The view as the subclass that paints a suggestion.
fn painter(editor: &dyn Editor) -> Option<&multicaret::View> {
    editor.view().downcast_ref::<multicaret::View>()
}

/// Watch `editor`, which has a [`State`] ([`Editor::ghost`]), for everything that ends a
/// suggestion. Which keys reach [`on_key`] is `editor::keys`'s decision, and so is the popup's
/// own show and hide.
pub fn install(editor: &Rc<dyn Editor>) {
    let buffer = editor.view().buffer();
    // Before the insert lands, so the suggestion is still up and the location iter still says
    // where the characters go: what is typed on top of a suggestion only shortens it.
    // Weak: the editor holds the buffer the handlers hang off.
    let weak = Rc::downgrade(editor);
    buffer.connect_insert_text(move |buffer, at, text| {
        let Some(editor) = weak.upgrade() else { return };
        let Some(state) = editor.ghost() else { return };
        let showing = painter(editor.as_ref())
            .and_then(|v| v.ghost())
            .filter(|_| !crate::completion::is_shown(editor.as_ref()) && !editor.is_loading())
            .filter(|_| at.offset() == caret(buffer).offset());
        *state.typed.borrow_mut() = showing.and_then(|ghost| {
            let rest = remainder(&ghost, text)?.to_string();
            Some((rest, at.offset() + text.chars().count() as i32))
        });
    });

    // Any edit and any move of the caret makes the suggestion about the wrong place, unless the
    // move is the one the typing above just announced. The next suggestion arrives with the
    // post-edit refresh, 100 ms after the typing stops.
    let weak = Rc::downgrade(editor);
    buffer.connect_changed(move |_| {
        if let Some(editor) = weak.upgrade() {
            settle(editor.as_ref());
        }
    });
    let weak = Rc::downgrade(editor);
    buffer.connect_cursor_position_notify(move |_| {
        if let Some(editor) = weak.upgrade() {
            settle(editor.as_ref());
        }
    });
}

/// Handle a key for painted ghost text, or return `None` to continue dispatch.
/// Without a suggestion, Tab remains available for list indentation.
pub fn on_key(
    editor: &dyn Editor,
    key: gdk::Key,
    state: gdk::ModifierType,
) -> Option<glib::Propagation> {
    let text = painter(editor).and_then(|v| v.ghost())?;
    Some(match key {
        // Ctrl+Right writes one word of the suggestion; the rest stays painted, because the
        // insert goes through the buffer like a keystroke. Shift means a selection and Alt
        // belongs to the compositor, so both are left alone.
        gdk::Key::Right | gdk::Key::KP_Right
            if state.contains(crate::platform::PRIMARY_MODIFIER)
                && !state
                    .intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SHIFT_MASK) =>
        {
            accept(editor, word_of(&text));
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
            accept(editor, &text);
            glib::Propagation::Stop
        }
        gdk::Key::Escape => {
            clear(editor);
            glib::Propagation::Stop
        }
        _ => return None,
    })
}

/// Write `text` where the suggestion was painted, as one undo step. Whatever is left of the
/// suggestion is kept by the same path a keystroke takes: the insert is one.
fn accept(editor: &dyn Editor, text: &str) {
    let buffer = editor.view().buffer();
    buffer.begin_user_action();
    buffer.insert_at_cursor(text);
    buffer.end_user_action();
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
fn settle(editor: &dyn Editor) {
    let typed = editor.ghost().and_then(|s| s.typed.borrow().clone());
    match typed {
        Some((rest, at)) if at == caret(&editor.view().buffer()).offset() => {
            if let Some(view) = painter(editor) {
                view.set_ghost((!rest.is_empty()).then_some(rest));
            }
        }
        _ => clear(editor),
    }
}

/// Take the suggestion off the screen. Cheap enough to call from every signal that could
/// invalidate one.
pub fn clear(editor: &dyn Editor) {
    set(editor, None);
}

/// Paint the new suggestion and discard pending typed-through state from the previous answer.
fn set(editor: &dyn Editor, text: Option<String>) {
    if let Some(state) = editor.ghost() {
        *state.typed.borrow_mut() = None;
    }
    if let Some(view) = painter(editor) {
        view.set_ghost(text);
    }
}

/// Request and paint the rest of the line, once the editor has flushed its text: after
/// `lang::refresh` in a text tab, after the notebook's own in a markdown cell. The next
/// keystroke drops the request and cancels it at the server.
pub async fn request(editor: &dyn Editor) {
    if !editor.ghost().is_some_and(|s| s.armed(editor)) {
        return;
    }
    let doc = editor.document();
    let Some(vault) = doc.vault() else {
        return;
    };
    let buffer = editor.view().buffer();
    let at = caret(&buffer);
    // Mid-line, a selection, or a column of carets: not a place a suggestion can be drawn. Nor is
    // a line holding only its indent and a list marker, where Tab is the item's indent and a
    // suggestion would be in the way of writing the list at all.
    if !at.ends_line()
        || buffer.has_selection()
        || painter(editor).is_some_and(|v| v.has_carets())
        || crate::typing::marker_only(&crate::editor::line_prefix(&buffer, &at))
    {
        return;
    }
    let here = lang::pos_of(&at);
    let (key, Some(pos)) = (doc.key(), editor.to_document(here)) else {
        return;
    };
    match vault.inline_completion(&key, pos).await {
        Ok(Some(text)) => {
            tracing::debug!("ghost for {key} at {pos:?}: {text:?}");
            // The answer took a round trip; the caret may have moved on since.
            let armed = editor.ghost().is_some_and(|s| s.armed(editor));
            if armed && lang::pos_of(&caret(&buffer)) == here {
                set(editor, Some(text));
            }
        }
        Ok(None) => clear(editor),
        Err(e) => tracing::debug!("ghost for {key}: {e:#}"),
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
