//! The call signature under the caret, over the line it is being typed on.
//!
//! A popover rather than an assistant of GtkSourceView's own: the hover and the completion own
//! that machinery between them, and a third assistant would fight both for the same space.
//! It never takes a grab and cannot be targeted, so it sits over the text while the text is
//! still being typed and neither swallows a key nor steals the pointer.
//!
//! It comes up when a trigger character is typed — `(` and `,` for most servers — and goes away
//! the moment it stops being about what the caret is inside: an answer of `None`, Escape, the
//! caret leaving the line, the focus leaving the view, or the tab being switched away from.

use crate::editor::Tab;
use crate::lang;
use accent_api::Signature;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// How wide the signature is let grow before it wraps, in characters. The same measure the hover
/// uses, for the same reason.
const WIDTH: i32 = 80;

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

/// `sig` as one line of Pango markup: the whole signature in monospace, with the parameter the
/// caret is on in bold. Everything is escaped, so a C++ signature full of `<` and `&` cannot turn
/// the label into a parse error and blank the popover.
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
    // A trigger character was typed. Connected before the insertion lands, so the caret is read
    // one idle later, once it sits after the character the server is being asked about.
    tab.buffer.connect_insert_text(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |_, _, text| {
            let Some(last) = text.chars().next_back() else {
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

    // Escape, ahead of everything else that would take it: while the popover is up it means
    // "this call is not what I want to see", and nothing else in the view should also act on it.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| match key == gdk::Key::Escape && tab.lang.signature.is_shown() {
            true => {
                tab.lang.signature.dismiss();
                glib::Propagation::Stop
            }
            false => glib::Propagation::Proceed,
        }
    ));
    tab.view.add_controller(keys);

    // The caret left the line the call is on: whatever is being typed now is not this call.
    tab.buffer.connect_cursor_position_notify(glib::clone!(
        #[weak(rename_to = tab)]
        tab,
        move |buffer| {
            let line = buffer.iter_at_mark(&buffer.get_insert()).line();
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
        let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
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
        content.append(
            &gtk::Label::builder()
                .label(doc)
                .css_classes(["caption", "dim-label"])
                .wrap(true)
                .max_width_chars(WIDTH)
                .xalign(0.0)
                .build(),
        );
    }

    // Never autohide and never targetable, for the reason the PDF link preview is neither: the
    // caret is still in the text under it and a grab would be the end of typing.
    let popover = gtk::Popover::builder()
        .autohide(false)
        .can_target(false)
        .position(gtk::PositionType::Top)
        .child(&content)
        .build();
    popover.set_parent(&tab.view);
    let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
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
