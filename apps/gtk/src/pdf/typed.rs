//! The Text tool: a click on the page opens the diagram's label editor there, plain and at the
//! text's size, empty or holding a text of ours to type again; finishing it types the text, types
//! it again, or erases one emptied (`accent_core::pdf`'s `typed`).

use std::rc::{Rc, Weak};
use std::time::Duration;

use accent_core::pdf::{self, NamedText, TextStyle, Theme};
use adw::prelude::*;
use gtk::glib;

use super::protocol::Request;
use super::tab::PdfTab;
use crate::diagram::label::{LabelEditor, Look, wire_finish};

/// What the editor open over the page types: on which page, which text of ours it types again
/// (`None` for a new one), and how it is set.
pub struct Typing {
    pub page: usize,
    pub id: Option<u32>,
    pub style: TextStyle,
}

impl PdfTab {
    /// The Text tool pressed `page` at `at`. An editor open finishes, and nothing more: the press
    /// was a click away from it. Else one opens over `hit`, a text of ours under the press, or at
    /// `at`, empty, in the ring's size and colour.
    pub(super) fn type_at(self: &Rc<Self>, page: usize, at: (f32, f32), hit: Option<NamedText>) {
        if self.typing.borrow().is_some() {
            return self.finish_text();
        }
        let (id, text, place, style) = match hit {
            Some((id, t)) => (Some(id), t.typed.text, t.bounds, t.typed.style),
            None => (
                None,
                String::new(),
                pdf::Rect::from_corners(at, at),
                self.view.text_style(),
            ),
        };
        let theme = self.view.theme();
        let paper = match theme {
            Theme::Recolour { paper, .. } => paper,
            Theme::Plain => [255; 3],
        };
        let look = Look::Plain {
            ink: super::view::shown(style.rgb, theme),
            paper,
        };
        let place = accent_drawio::Rect::new(
            f64::from(place.left),
            f64::from(place.top),
            f64::from(place.width()),
            f64::from(place.height()),
        );
        let target = Typing { page, id, style };
        let size = f64::from(style.size);
        let editor = LabelEditor::open(&self.overlay, target, &text, place, size, look);
        *self.typing.borrow_mut() = Some(editor.clone());
        self.place_text();
        // Only the editor that left: another may have opened by the time the idle runs.
        let later = {
            let (tab, left) = (Rc::downgrade(self), Rc::downgrade(&editor));
            move || {
                let (tab, left) = (tab.clone(), left.clone());
                glib::idle_add_local_once(move || {
                    let Some(tab) = tab.upgrade() else { return };
                    let open = tab.typing.borrow().as_ref().map(Rc::downgrade);
                    if open.is_some_and(|open| open.ptr_eq(&left)) {
                        tab.finish_text();
                    }
                });
            }
        };
        let now = glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.finish_text()
        );
        // After the press that opened it is done with: the page takes the keyboard on a press.
        glib::idle_add_local_once(move || wire_finish(&editor, now, later));
    }

    /// Put the editor back over its text: where it opens, and again whenever the page scrolls or
    /// zooms under it. Nothing while none is open.
    pub(super) fn place_text(&self) {
        let Some(editor) = self.typing.borrow().clone() else {
            return;
        };
        let p = editor.place;
        let corner = |x: f64, y: f64| (x as f32, y as f32);
        let r = pdf::Rect::from_corners(corner(p.x, p.y), corner(p.x + p.w, p.y + p.h));
        let Some(at) = self.view.widget_rect(editor.target.page, &r) else {
            return;
        };
        let to = |x: i32, y: i32| {
            let p = gtk::graphene::Point::new(x as f32, y as f32);
            let p = self.view.compute_point(&self.overlay, &p).unwrap_or(p);
            (f64::from(p.x()), f64::from(p.y()))
        };
        let (x, y) = to(at.x(), at.y());
        let at = accent_drawio::Rect::new(x, y, f64::from(at.width()), f64::from(at.height()));
        let scale = f64::from(self.view.placement().0.scale);
        editor.place_at(at, editor.font_size * scale);
    }

    /// Type what the editor holds and close it: a new text where it opened, or one of ours typed
    /// again, or erased once emptied; nothing for an empty box or an unchanged text. Reentrant-safe, as
    /// the diagram's label is: closing it moves the keyboard, which can call this again.
    pub fn finish_text(self: &Rc<Self>) {
        let Some(editor) = self.typing.borrow_mut().take() else {
            return;
        };
        let Typing { page, id, style } = editor.target;
        let at = (editor.place.x as f32, editor.place.y as f32);
        let request = match (id, editor.changed()) {
            (_, None) => None,
            (None, Some(text)) => Some(Request::Type {
                page,
                at,
                text,
                style,
            }),
            (Some(id), Some(text)) if text.is_empty() => Some(Request::Erase {
                page,
                id,
                joined: false,
                partial: None,
            }),
            (Some(id), Some(text)) => Some(Request::Retype {
                page,
                id,
                text,
                style,
            }),
        };
        match request {
            None => editor.close(&self.overlay),
            // Kept over the page as it was left until the page shows what it typed: closed at
            // once, the page would show the text as it was for as long as the render takes.
            Some(request) => {
                editor.freeze();
                self.settle(editor);
                self.ask(request);
            }
        }
        // The keyboard back to the page, unless another text opened meanwhile.
        let tab = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(tab) = tab.upgrade().filter(|t| t.typing.borrow().is_none()) {
                tab.view.grab_focus();
            }
        });
    }

    /// Keep a finished editor until a tile of its page lands after the page's change is heard,
    /// or a second at most; one kept from before goes now.
    fn settle(self: &Rc<Self>, editor: Rc<LabelEditor<Typing>>) {
        let kept = Rc::downgrade(&editor);
        if let Some((before, _)) = self.settling.replace(Some((editor, false))) {
            before.close(&self.overlay);
        }
        let tab = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(1), move || {
            if let Some(tab) = tab.upgrade() {
                tab.settled(Some(&kept));
            }
        });
    }

    /// `page` changed: the next tile of it carries what the kept editor typed.
    pub(super) fn page_heard(&self, page: usize) {
        if let Some((editor, heard)) = self.settling.borrow_mut().as_mut()
            && editor.target.page == page
        {
            *heard = true;
        }
    }

    /// A tile of `page` landed: the kept editor goes, once its page's change has been heard.
    pub(super) fn tile_landed(&self, page: usize) {
        let shown = matches!(&*self.settling.borrow(), Some((e, true)) if e.target.page == page);
        if shown {
            self.settled(None);
        }
    }

    /// Close the kept editor, or nothing if `only` names one that has gone already.
    fn settled(&self, only: Option<&Weak<LabelEditor<Typing>>>) {
        let kept = self
            .settling
            .borrow()
            .as_ref()
            .map(|(e, _)| Rc::downgrade(e));
        if only.is_some_and(|only| kept.as_ref().is_none_or(|k| !k.ptr_eq(only))) {
            return;
        }
        if let Some((editor, _)) = self.settling.take() {
            editor.close(&self.overlay);
        }
    }

    /// `Ctrl+Return` in the editor, which the window's accelerator took first: finish the text.
    /// `false` when no text is being typed here.
    pub fn commit_text(self: &Rc<Self>) -> bool {
        let typing = self.typing.borrow().as_ref().is_some_and(|e| e.has_focus());
        if typing {
            self.finish_text();
        }
        typing
    }
}
