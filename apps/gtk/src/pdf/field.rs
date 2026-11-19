//! Form fields on the page (DESIGN.md, PDF Forms): the widgets the render thread reads with a
//! page's links, tinted in the accent where a reader can fill them in, and the pointer and the
//! tooltip over them.

use std::collections::HashMap;

use accent_core::pdf::{self, FieldKind, FormField};

use super::tab::PdfTab;
use super::{Mode, PdfView};

/// What a password field says over it: accent writes none into a file (user decision 9).
pub(super) const NO_PASSWORDS: &str = "Accent does not store passwords";

impl PdfTab {
    /// A page's fields landed.
    pub(super) fn fields_landed(&self, page: usize, fields: Vec<FormField>) {
        self.fields.borrow_mut().insert(page, fields);
        self.tint_fields();
    }

    /// Tint every field a reader can fill in, on every page whose fields are known, and none while
    /// the document is presented (user decision 5).
    pub(super) fn tint_fields(&self) {
        let tint: HashMap<usize, Vec<pdf::Rect>> = match self.presenting.get() {
            Some(_) => HashMap::new(),
            None => (self.fields.borrow().iter())
                .map(|(page, fields)| {
                    let rects = fields.iter().filter(|f| fillable(f)).map(|f| f.rect);
                    (*page, rects.collect())
                })
                .collect(),
        };
        self.view.set_fields(tint);
    }

    /// What `f` makes of the field under a point of the reading view and its page: the field
    /// drawn last where widgets overlap, as the topmost takes a click. None with a tool in hand,
    /// which the page belongs to.
    pub(super) fn over_field<T>(
        &self,
        view: &PdfView,
        (x, y): (f64, f64),
        f: impl FnOnce(usize, &FormField) -> T,
    ) -> Option<T> {
        if view.mode() != Mode::Read {
            return None;
        }
        let (page, px, py) = view.page_point(x, y)?;
        let fields = self.fields.borrow();
        let hit =
            (fields.get(&page)?.iter().rev()).find(|f| !f.hidden && f.rect.contains((px, py)));
        hit.map(|field| f(page, field))
    }

    /// The tooltip over a point of the reading view, a field's, and where it holds.
    pub(super) fn field_tip(&self, x: f64, y: f64) -> Option<(String, gtk::gdk::Rectangle)> {
        let (page, tip, rect) = self.over_field(&self.view, (x, y), |page, field| {
            Some((page, tip_of(field)?, field.rect))
        })??;
        Some((tip, self.view.widget_rect(page, &rect)?))
    }
}

/// Whether a reader fills `field` in here: shown, not read-only, and holding a value of a kind
/// accent writes, which a push button, a password (user decision 9) and a signature (decision 11)
/// are not.
pub(super) fn fillable(field: &FormField) -> bool {
    let writes = match &field.kind {
        FieldKind::Text { password, .. } => !password,
        FieldKind::Check { .. } | FieldKind::Radio { .. } | FieldKind::Choice { .. } => true,
        FieldKind::Button | FieldKind::Signature => false,
    };
    writes && !field.hidden && !field.read_only
}

/// The pointer over `field`: the I-beam over text to type, the hand over a box or a list a click
/// fills in, the arrow over one that takes nothing.
pub(super) fn cursor_of(field: &FormField) -> &'static str {
    match (&field.kind, fillable(field)) {
        (_, false) => "default",
        (FieldKind::Text { .. }, true) => "text",
        (_, true) => "pointer",
    }
}

/// What the tooltip over `field` says: the name its form gives it for a reader, and over a
/// password field why it takes nothing.
pub(super) fn tip_of(field: &FormField) -> Option<String> {
    let password = matches!(field.kind, FieldKind::Text { password: true, .. });
    match (field.tip.as_deref(), password) {
        (Some(tip), true) => Some(format!("{tip}\n{NO_PASSWORDS}")),
        (None, true) => Some(NO_PASSWORDS.to_string()),
        (tip, false) => tip.map(str::to_string),
    }
}

/// What the drills ask of the fields.
#[cfg(feature = "bench")]
impl PdfTab {
    /// The fields of `page`, once the render thread has read them.
    pub fn fields_heard(&self, page: usize) -> Option<Vec<FormField>> {
        self.fields.borrow().get(&page).cloned()
    }

    /// The tooltip at a point of the reading view, as the view would show it over a field.
    pub fn field_tip_at(&self, x: f64, y: f64) -> Option<String> {
        self.field_tip(x, y).map(|(tip, _)| tip)
    }
}
