//! Show the call signature above the caret without taking keyboard or pointer focus.
//!
//! A popover avoids competing with GtkSourceView's hover and completion assistants. Trigger
//! characters request it; `None`, Escape, leaving the line, losing focus or switching tabs
//! hide it.

use crate::editor::{Tab, caret};
use crate::hover::WIDTH;
use crate::lang;
use accent_api::Signature;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The signature popover of one tab, and the request that would fill it.
#[derive(Default)]
pub struct Help {
    popover: RefCell<Option<gtk::Popover>>,
    request: RefCell<Option<glib::JoinHandle<()>>>,
    /// The buffer line the popover was put over. The caret leaving it ends the call.
    line: Cell<i32>,
}

impl Help {
    pub fn is_shown(&self) -> bool {
        self.popover.borrow().is_some()
    }

    /// Take the popover away and drop whatever was being asked for it.
    pub fn dismiss(&self) {
        if let Some(handle) = self.request.borrow_mut().take() {
            handle.abort();
        }
        if let Some(popover) = self.popover.borrow_mut().take() {
            popover.popdown();
        }
    }
}

/// Render `sig` as escaped Pango markup, with the active parameter bold and the signature monospace.
/// Escaping prevents server text containing `<` or `&` from breaking the label.
pub fn markup(sig: &Signature) -> String {
    let chars: Vec<char> = sig.label.chars().collect();
    let piece = |range: std::ops::Range<usize>| -> String {
        glib::markup_escape_text(&chars[range].iter().collect::<String>()).to_string()
    };
    let active = sig
        .active
        .and_then(|i| sig.params.get(i as usize))
        .map(|(s, e)| {
            (
                (*s as usize).min(chars.len()),
                (*e as usize).min(chars.len()),
            )
        })
        .filter(|(s, e)| s < e);
    match active {
        Some((start, end)) => format!(
            "<tt>{}<b>{}</b>{}</tt>",
            piece(0..start),
            piece(start..end),
            piece(end..chars.len())
        ),
        None => format!("<tt>{}</tt>", piece(0..chars.len())),
    }
}

/// Wire a tab's view to ask for a signature and to know when to stop showing one. Called by
/// [`lang::attach`].
pub fn install(tab: &Rc<Tab>) {
    // A trigger character was typed: one character, or the pair `typing` writes for a bracket,
    // at the caret, as for the completion popup. A template landing with a `(` in it is not a
    // call being typed. Connected before the insertion lands, so the caret is read one idle
    // later, once it sits after the character the server is being asked about.
    tab.buffer.connect_insert_text(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |buffer, at, text| {
            let typed = text.chars().count() <= 2 && at.offset() == caret(buffer).offset();
            let Some(last) = text
                .chars()
                .next_back()
                .filter(|_| typed && !tab.is_loading())
            else {
                return;
            };
            let asks = tab
                .lang
                .support()
                .is_some_and(|s| s.signature_triggers.contains(&last));
            if asks {
                glib::idle_add_local_once(move || request(&tab));
            }
        }
    ));

    // The caret left the line the call is on: whatever is being typed now is not this call.
    tab.buffer.connect_cursor_position_notify(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |buffer| {
            let line = caret(buffer).line();
            if tab.lang.signature.is_shown() && line != tab.lang.signature.line.get() {
                tab.lang.signature.dismiss();
            }
        }
    ));

    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.lang.signature.dismiss()
    ));
    tab.view.add_controller(focus);

    // The tab was switched away from or closed with a signature up.
    tab.view.connect_unmap(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_| tab.lang.signature.dismiss()
    ));
}

/// Consume Escape while signature help is visible; return `None` for other keys.
/// `editor::keys` gives this priority over ghost text so one Escape closes one feature.
pub fn on_key(tab: &Rc<Tab>, key: gdk::Key) -> Option<glib::Propagation> {
    (key == gdk::Key::Escape && tab.lang.signature.is_shown()).then(|| {
        tab.lang.signature.dismiss();
        glib::Propagation::Stop
    })
}

/// Ask what call the caret is inside, and show or hide the popover from the answer.
pub fn request(tab: &Rc<Tab>) {
    let Some(vault) = tab.lang.vault() else {
        return;
    };
    if let Some(handle) = tab.lang.signature.request.borrow_mut().take() {
        handle.abort();
    }
    let weak = Rc::downgrade(tab);
    let handle = glib::spawn_future_local(async move {
        let Some(tab) = weak.upgrade() else { return };
        lang::flush(tab.clone()).await;
        // Read after the flush, not before: the edit that asked for this may have moved it.
        let caret = caret(&tab.buffer);
        let answer = vault.signature_help(&tab.rel(), lang::pos_of(&caret)).await;
        tracing::debug!(
            "signature for {}: {:?}",
            tab.rel(),
            answer.as_ref().map(|s| s.as_ref().map(|s| &s.label))
        );
        match answer {
            Ok(Some(sig)) => show(&tab, &sig),
            // No call here any more, or the server could not say: the popover goes.
            _ => tab.lang.signature.dismiss(),
        }
    });
    *tab.lang.signature.request.borrow_mut() = Some(handle);
}

/// Put the signature over the caret, replacing whatever was there.
fn show(tab: &Rc<Tab>, sig: &Signature) {
    tab.lang.signature.dismiss();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let code = gtk::Label::builder()
        .use_markup(true)
        .wrap(true)
        .max_width_chars(WIDTH)
        .xalign(0.0)
        .build();
    code.set_markup(&markup(sig));
    content.append(&code);
    if let Some(doc) = sig.doc.as_deref().filter(|d| !d.is_empty()) {
        // Dimmed the way the hover dims its own source line, rather than through `dim-label`:
        // the two say the same thing beside the same code and should not say it in two greys.
        let quiet = gtk::Label::builder()
            .use_markup(true)
            .wrap(true)
            .max_width_chars(WIDTH)
            .xalign(0.0)
            .build();
        quiet.set_markup(&format!(
            "<span alpha=\"60%\">{}</span>",
            glib::markup_escape_text(doc)
        ));
        content.append(&quiet);
    }

    // Never autohide and never targetable, for the reason the PDF link preview is neither: the
    // caret is still in the text under it and a grab would be the end of typing. No arrow either:
    // the hover and the completion are assistants without one, and a third shape over the same
    // text reads as a different kind of thing.
    let popover = gtk::Popover::builder()
        .autohide(false)
        .can_target(false)
        .has_arrow(false)
        .position(gtk::PositionType::Top)
        .child(&content)
        .build();
    popover.set_parent(&tab.view);
    let caret = caret(&tab.buffer);
    let at = tab.view.iter_location(&caret);
    let (x, y) = tab
        .view
        .buffer_to_window_coords(gtk::TextWindowType::Widget, at.x(), at.y());
    popover.set_pointing_to(Some(&gdk::Rectangle::new(
        x,
        y,
        at.width().max(1),
        at.height().max(1),
    )));
    // A popover parented by hand stays parented until it is unparented by hand.
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
    tab.lang.signature.line.set(caret.line());
    *tab.lang.signature.popover.borrow_mut() = Some(popover);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(active: Option<u32>) -> Signature {
        Signature {
            label: "int add(int a, int b)".to_string(),
            doc: None,
            params: vec![(8, 13), (15, 20)],
            active,
        }
    }

    #[test]
    fn the_active_parameter_is_the_bold_one() {
        assert_eq!(
            markup(&sig(Some(1))),
            "<tt>int add(int a, <b>int b</b>)</tt>"
        );
    }

    #[test]
    fn a_signature_with_no_active_parameter_is_plain_monospace() {
        assert_eq!(markup(&sig(None)), "<tt>int add(int a, int b)</tt>");
    }

    /// A server may send a range that no longer fits the label it sent with it; the label still
    /// has to render rather than panicking on the slice.
    #[test]
    fn an_out_of_range_parameter_is_clamped_away() {
        let mut wild = sig(Some(0));
        wild.params = vec![(99, 120)];
        assert_eq!(markup(&wild), "<tt>int add(int a, int b)</tt>");
    }

    #[test]
    fn markup_escapes_what_pango_would_read_as_a_tag() {
        let sig = Signature {
            label: "T& max<T>(T& a)".to_string(),
            doc: None,
            params: vec![(10, 14)],
            active: Some(0),
        };
        assert_eq!(
            markup(&sig),
            "<tt>T&amp; max&lt;T&gt;(<b>T&amp; a</b>)</tt>"
        );
    }
}
