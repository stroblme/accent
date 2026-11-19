//! Form fields on the page (DESIGN.md, PDF Forms): the widgets the render thread reads with a
//! page's links, tinted in the accent where a reader can fill them in, the pointer and the
//! tooltip over them, a click that ticks a box or presses a button, the editor a text field is
//! typed into and the list a choice field picks from, and Tab through them all in pdf.js's order.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use accent_core::pdf::{self, FieldAction, FieldKind, FieldValue, FormField, Theme};
use adw::prelude::*;
use gtk::glib;

use super::protocol::Request;
use super::tab::PdfTab;
use super::typed::Kept;
use super::{Mode, PdfView};
use crate::diagram::label::{LabelEditor, Look, wire_finish};

/// The editor of a text field being filled in: the diagram's label editor, over the field.
pub(super) type FieldEditor = LabelEditor<Filling>;

/// What the editor over a field fills in: the terminal field, and the page and the box of the
/// widget it is over.
pub struct Filling {
    pub page: usize,
    pub field: pdf::Ref,
    pub rect: pdf::Rect,
}

impl Kept for FieldEditor {
    fn page(&self) -> usize {
        self.target.page
    }

    fn shut(&self, overlay: &gtk::Overlay) {
        self.close(overlay);
    }
}

/// What a password field says over it: accent writes none into a file (user decision 9).
pub(super) const NO_PASSWORDS: &str = "Accent does not store passwords";
/// What a form whose fields run scripts says once, and an XFA form without fields of its own:
/// accent runs no JavaScript (user decision 7) and reads no XFA (decision 8).
const SCRIPTS: &str = "This form's scripts, which check and calculate its fields, are not run";
const XFA: &str = "This is an XFA form, which Accent shows but cannot fill";

impl PdfTab {
    /// A page's fields landed: the tint follows them where they changed.
    pub(super) fn fields_landed(&self, page: usize, fields: Vec<FormField>) {
        let was = self.fields.borrow_mut().insert(page, fields.clone());
        if was.unwrap_or_default() != fields {
            self.tint_fields();
        }
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

impl PdfTab {
    /// A click at a point of the reading view: a field a reader fills in under it takes it, and
    /// is filled in as its kind is, a check box toggled, a radio button turned on, or off again
    /// where its group lets it. Whether a field took it.
    pub(super) fn click_field(self: &Rc<Self>, view: &PdfView, x: f64, y: f64) -> bool {
        let Some((page, field)) = self.over_field(view, (x, y), |page, f| (page, f.clone())) else {
            return false;
        };
        if !takes_clicks(&field) {
            return false;
        }
        self.press_field(page, &field);
        true
    }

    /// `field` of `page` clicked, or pressed with Space or Return while the keyboard is on it:
    /// a check box toggled, a radio button turned on, or off again where its group lets it, the
    /// editor opened over a text field, the list under a choice field, a push button's action
    /// carried out.
    fn press_field(self: &Rc<Self>, page: usize, field: &FormField) {
        match &field.kind {
            FieldKind::Check { on, checked } => {
                self.fill_state(field, (!checked).then(|| on.clone()))
            }
            FieldKind::Radio {
                on,
                checked,
                no_toggle_off,
            } => match (checked, no_toggle_off) {
                (true, true) => {}
                (true, false) => self.fill_state(field, None),
                (false, _) => self.fill_state(field, Some(on.clone())),
            },
            FieldKind::Text { .. } => self.edit_field(page, field),
            FieldKind::Choice { .. } => self.choose(page, field),
            FieldKind::Button => self.press_button(field),
            FieldKind::Signature => {}
        }
    }

    /// What a push button does: a Reset Form put back as one step, a link to a page of this
    /// document or a URI followed, a named page turn; a script it carries is not run.
    fn press_button(self: &Rc<Self>, field: &FormField) {
        match &field.action {
            Some(FieldAction::Reset(reset)) => {
                self.finish_field();
                self.ask(Request::Reset(reset.clone()));
            }
            Some(FieldAction::Uri(uri)) => {
                if let Some(f) = self.on_uri.get() {
                    f(uri);
                }
            }
            Some(FieldAction::Page { page, top }) => {
                self.jumping();
                self.view.goto_page(*page, *top);
            }
            Some(FieldAction::Named(name)) => match name.as_str() {
                "NextPage" => self.next_page(),
                "PrevPage" => self.previous_page(),
                "FirstPage" => self.goto_page(0),
                "LastPage" => self.goto_page(self.page_count().saturating_sub(1)),
                _ => {}
            },
            None => {}
        }
    }

    /// Put every field of the form back to its default, as one step: the palette's Reset Form.
    pub fn reset_form(self: &Rc<Self>) {
        self.finish_field();
        self.ask(Request::Reset(pdf::Reset::all()));
    }

    /// Whether the document has a form with fields: what Reset Form is offered over.
    pub fn has_form(&self) -> bool {
        self.form.borrow().as_ref().is_some_and(|form| form.fields)
    }

    /// What accent cannot do of the form, said once a document: run its scripts, or fill an XFA
    /// form that has no fields of its own. `None` once said, or for a form it does all of.
    pub fn form_notice(&self) -> Option<&'static str> {
        let form = self.form.borrow();
        let said = match form.as_ref()? {
            form if form.xfa && !form.fields => XFA,
            form if form.scripted => SCRIPTS,
            _ => return None,
        };
        (!self.told_form.replace(true)).then_some(said)
    }

    /// Called when what the form says as a whole is known.
    pub fn connect_form(&self, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_form.set(Rc::new(f));
    }

    /// Move the keyboard to the next field, or with `forward` false the one before, in pdf.js's
    /// order across the pages (a text field's editor opening, another kind showing the focus
    /// ring), brought into view: from the field it is on, else from the page being read. `false`
    /// past the last or the first, where the keyboard leaves the page as it would without fields.
    pub(super) fn tab_field(self: &Rc<Self>, forward: bool) -> bool {
        let order = self.tab_order();
        let page = self.current_page();
        let at =
            (self.focused.get()).and_then(|(_, w)| order.iter().position(|(_, f)| f.widget == w));
        let next = match (at, forward) {
            (Some(i), true) => Some(i + 1),
            (Some(i), false) => i.checked_sub(1),
            (None, true) => order.iter().position(|(p, _)| *p >= page),
            (None, false) => order.iter().rposition(|(p, _)| *p <= page),
        };
        match next.and_then(|i| order.get(i)) {
            Some((page, field)) => {
                self.focus_field(*page, field);
                true
            }
            None => {
                self.finish_field();
                self.unfocus_field();
                false
            }
        }
    }

    /// The keyboard on `field` of `page`, brought into view: its editor open for a text field, the
    /// focus ring round another.
    fn focus_field(self: &Rc<Self>, page: usize, field: &FormField) {
        self.focused.set(Some((page, field.widget)));
        self.view.reveal(page, field.rect);
        match field.kind {
            FieldKind::Text { .. } => {
                self.view.set_focus_field(None);
                self.edit_field(page, field);
            }
            _ => {
                self.finish_field();
                self.view.set_focus_field(Some((page, field.rect)));
            }
        }
    }

    /// The keyboard off the fields, its ring gone.
    pub(super) fn unfocus_field(&self) {
        self.focused.set(None);
        self.view.set_focus_field(None);
    }

    /// A key on the page while the keyboard is on a field other than a text field: Space or
    /// Return presses it, an arrow on a radio button turns its neighbour in the group on, and
    /// Escape takes the keyboard off it. Whether the key was the field's.
    pub(super) fn field_key(self: &Rc<Self>, key: gtk::gdk::Key) -> bool {
        use gtk::gdk::Key;
        let Some((page, widget)) = self.focused.get() else {
            return false;
        };
        let order = self.tab_order();
        let Some(at) = order.iter().position(|(_, f)| f.widget == widget) else {
            self.unfocus_field();
            return false;
        };
        let (_, field) = order[at].clone();
        match key {
            Key::Escape => self.unfocus_field(),
            Key::space | Key::Return | Key::KP_Enter => self.press_field(page, &field),
            Key::Left | Key::Up | Key::Right | Key::Down
                if matches!(field.kind, FieldKind::Radio { .. }) =>
            {
                let group: Vec<&(usize, FormField)> = order
                    .iter()
                    .filter(|(_, f)| f.field == field.field)
                    .collect();
                let here = group
                    .iter()
                    .position(|(_, f)| f.widget == widget)
                    .unwrap_or(0);
                let next = match key {
                    Key::Left | Key::Up => here.checked_sub(1).unwrap_or(group.len() - 1),
                    _ => (here + 1) % group.len(),
                };
                let (page, next) = group[next].clone();
                self.focus_field(page, &next);
                if let FieldKind::Radio {
                    on, checked: false, ..
                } = &next.kind
                {
                    self.fill_state(&next, Some(on.clone()));
                }
            }
            _ => return false,
        }
        true
    }

    /// The fields the keyboard goes through, in pdf.js's order: page by page, and on a page row
    /// by row, a field starting a row below another when its middle is under that one's bottom and
    /// that one's middle above its top, then across a row by the fields' middles.
    fn tab_order(&self) -> Vec<(usize, FormField)> {
        let fields = self.fields.borrow();
        let mut pages: Vec<usize> = fields.keys().copied().collect();
        pages.sort_unstable();
        let mut out = Vec::new();
        for page in pages {
            let mut on: Vec<&FormField> =
                fields[&page].iter().filter(|f| takes_clicks(f)).collect();
            on.sort_by(|a, b| a.rect.top.total_cmp(&b.rect.top));
            let mut rows: Vec<Vec<&FormField>> = Vec::new();
            for f in on {
                match rows.last_mut() {
                    Some(row) if !above(row[0], f) => row.push(f),
                    _ => rows.push(vec![f]),
                }
            }
            for mut row in rows {
                row.sort_by(|a, b| {
                    middle_x(a)
                        .partial_cmp(&middle_x(b))
                        .unwrap_or(Ordering::Equal)
                });
                out.extend(row.into_iter().map(|f| (page, f.clone())));
            }
        }
        out
    }

    /// Open the list a choice field picks from under its widget, its picks marked: a click on an
    /// option of a combo box or a single list box picks it and closes the list, and a list box
    /// that picks several ticks and unticks its options, picked as they stand when the list
    /// closes. An editable combo box's own text goes in above, picked on Return.
    fn choose(self: &Rc<Self>, page: usize, field: &FormField) {
        let FieldKind::Choice {
            edit,
            multi,
            options,
            picked,
            ..
        } = &field.kind
        else {
            return;
        };
        self.finish_field();
        if let Some(open) = self.choosing.borrow_mut().take() {
            open.popdown();
        }
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["navigation-sidebar"])
            .build();
        let ticks: Vec<gtk::CheckButton> = (options.iter())
            .map(|(export, shown)| {
                let label = gtk::Label::builder()
                    .label(shown)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(40)
                    .build();
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let tick = gtk::CheckButton::builder()
                    .active(picked.contains(export))
                    .can_focus(false)
                    .can_target(false)
                    .build();
                // A pick of one is said by the row's mark, as a menu's radio items say it.
                if !multi {
                    tick.set_opacity(if tick.is_active() { 1.0 } else { 0.0 });
                }
                row.append(&tick);
                row.append(&label);
                list.append(&row);
                tick
            })
            .collect();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(320)
            .child(&list)
            .build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let popover = gtk::Popover::builder()
            .position(gtk::PositionType::Bottom)
            .child(&column)
            .build();
        let target = field.field;
        let exports: Vec<String> = options.iter().map(|(e, _)| e.clone()).collect();
        if *edit {
            let entry = gtk::Entry::builder()
                .text(picked.first().map(String::as_str).unwrap_or_default())
                .build();
            entry.connect_activate(glib::clone!(
                #[weak(rename_to = tab)]
                self,
                #[weak]
                popover,
                move |entry| {
                    tab.fill_choice(target, vec![entry.text().to_string()]);
                    popover.popdown();
                }
            ));
            column.append(&entry);
        }
        column.append(&scroller);
        let was = picked.clone();
        let multi = *multi;
        let selected = picked
            .first()
            .and_then(|p| exports.iter().position(|e| e == p));
        list.connect_row_activated(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[weak]
            popover,
            #[strong]
            ticks,
            #[strong]
            exports,
            move |_, row| {
                let at = row.index().max(0) as usize;
                if multi {
                    if let Some(tick) = ticks.get(at) {
                        tick.set_active(!tick.is_active());
                    }
                    return;
                }
                if let Some(export) = exports.get(at) {
                    tab.fill_choice(target, vec![export.clone()]);
                }
                popover.popdown();
            }
        ));
        popover.connect_closed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |popover| {
                if multi {
                    let now: Vec<String> = (ticks.iter().zip(&exports))
                        .filter(|(tick, _)| tick.is_active())
                        .map(|(_, export)| export.clone())
                        .collect();
                    if now != was {
                        tab.fill_choice(target, now);
                    }
                }
                popover.unparent();
                tab.choosing.borrow_mut().take_if(|open| *open == *popover);
                tab.page_takes_keys();
            }
        ));
        // On the box, at its own coordinates, as the page's menu is (`selection_menu`).
        popover.set_parent(&self.host);
        if let Some(r) = self.view.widget_rect(page, &field.rect) {
            let p = gtk::graphene::Point::new(r.x() as f32, r.y() as f32);
            let p = self.view.compute_point(&self.host, &p).unwrap_or(p);
            let at = gtk::gdk::Rectangle::new(p.x() as i32, p.y() as i32, r.width(), r.height());
            popover.set_pointing_to(Some(&at));
        }
        popover.popup();
        if let Some(row) = list.row_at_index(selected.unwrap_or(0) as i32) {
            row.grab_focus();
        }
        *self.choosing.borrow_mut() = Some(popover);
    }

    /// Pick `picked` in the choice field `field`, what the tab knows of it saying so at once.
    fn fill_choice(&self, field: pdf::Ref, picked: Vec<String>) {
        for fields in self.fields.borrow_mut().values_mut() {
            for f in fields.iter_mut().filter(|f| f.field == field) {
                if let FieldKind::Choice { picked: now, .. } = &mut f.kind {
                    now.clone_from(&picked);
                }
            }
        }
        let value = FieldValue::Choice(picked);
        self.ask(Request::Fill { field, value });
    }

    /// Open the editor over the text field `field` of `page`, holding what it says, in its face,
    /// size and colour as the page shows them, the caret at its end; one open already is finished
    /// first. Escape leaves the field as it was; Return on one line, `Ctrl+Return`, a click
    /// elsewhere or the keyboard going elsewhere fills it in.
    fn edit_field(self: &Rc<Self>, page: usize, field: &FormField) {
        let FieldKind::Text {
            value,
            multiline,
            max_len,
            align,
            da,
            ..
        } = &field.kind
        else {
            return;
        };
        self.finish_field();
        self.focused.set(Some((page, field.widget)));
        let theme = self.view.theme();
        let paper = match theme {
            Theme::Recolour { paper, .. } => paper,
            Theme::Plain => [255; 3],
        };
        let look = Look::Field {
            ink: super::view::shown(da.rgb, theme),
            paper,
            align: match align {
                1 => gtk::Justification::Center,
                2 => gtk::Justification::Right,
                _ => gtk::Justification::Left,
            },
            multiline: *multiline,
            max_len: *max_len,
        };
        let r = field.rect;
        let place = accent_drawio::Rect::new(
            f64::from(r.left),
            f64::from(r.top),
            f64::from(r.width()),
            f64::from(r.height()),
        );
        let target = Filling {
            page,
            field: field.field,
            rect: r,
        };
        let size = f64::from(field.editor_size());
        let editor = LabelEditor::open(&self.overlay, target, value, place, size, look);
        *self.filling.borrow_mut() = Some(editor.clone());
        self.place_field();
        // Only the editor that left: another may have opened by the time the idle runs.
        let later = {
            let (tab, left) = (Rc::downgrade(self), Rc::downgrade(&editor));
            move || {
                let (tab, left) = (tab.clone(), left.clone());
                glib::idle_add_local_once(move || {
                    let Some(tab) = tab.upgrade() else { return };
                    let open = tab.filling.borrow().as_ref().map(Rc::downgrade);
                    if open.is_some_and(|open| open.ptr_eq(&left)) {
                        tab.finish_field();
                    }
                });
            }
        };
        let cancel = glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.cancel_field()
        );
        // Return fills a field of one line in, Tab and Shift+Tab move on to the next field or
        // the one before, and past the last or the first fill it in and leave the page.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let multiline = *multiline;
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| match key {
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter if !multiline => {
                    tab.finish_field();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Tab | gtk::gdk::Key::ISO_Left_Tab => {
                    if !tab.tab_field(key == gtk::gdk::Key::Tab) {
                        tab.view.grab_focus();
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        ));
        editor.view().add_controller(keys);
        // After the press that opened it is done with: the page takes the keyboard on a press.
        glib::idle_add_local_once(move || wire_finish(&editor, cancel, later));
    }

    /// Put the field's editor back over its widget: where it opens, and again whenever the page
    /// scrolls or zooms under it.
    pub(super) fn place_field(&self) {
        let Some(editor) = self.filling.borrow().clone() else {
            return;
        };
        let Some(at) = self
            .view
            .widget_rect(editor.target.page, &editor.target.rect)
        else {
            return;
        };
        let p = gtk::graphene::Point::new(at.x() as f32, at.y() as f32);
        let p = self.view.compute_point(&self.overlay, &p).unwrap_or(p);
        let at = accent_drawio::Rect::new(
            f64::from(p.x()),
            f64::from(p.y()),
            f64::from(at.width()),
            f64::from(at.height()),
        );
        let scale = f64::from(self.view.placement().0.scale);
        editor.place_at(at, editor.font_size * scale);
    }

    /// Fill the field in with what its editor holds and close it, kept over the page until a
    /// tile shows the value, or at once for one unchanged. Reentrant-safe, as the Text tool's is.
    pub fn finish_field(self: &Rc<Self>) {
        let Some(editor) = self.filling.borrow_mut().take() else {
            return;
        };
        match editor.changed() {
            None => editor.close(&self.overlay),
            Some(text) => {
                self.know_text(editor.target.field, &text);
                editor.freeze();
                self.ask(Request::Fill {
                    field: editor.target.field,
                    value: FieldValue::Text(text),
                });
                self.settle(editor);
            }
        }
        self.page_takes_keys();
    }

    /// Close the field's editor and leave the field as it was.
    fn cancel_field(self: &Rc<Self>) {
        if let Some(editor) = self.filling.borrow_mut().take() {
            editor.close(&self.overlay);
        }
        self.page_takes_keys();
    }

    /// Fill the field in at once and close its editor, as a copy of the document asks: the
    /// copy comes after the fill.
    pub(super) fn fill_now(&self) {
        let Some(editor) = self.filling.borrow_mut().take() else {
            return;
        };
        if let Some(text) = editor.changed() {
            self.know_text(editor.target.field, &text);
            let (field, value) = (editor.target.field, FieldValue::Text(text));
            self.ask(Request::Fill { field, value });
        }
        editor.close(&self.overlay);
    }

    /// `Ctrl+Return` in the field's editor: fill it in. `false` when none has the keyboard.
    pub(super) fn commit_field(self: &Rc<Self>) -> bool {
        let filling = self
            .filling
            .borrow()
            .as_ref()
            .is_some_and(|e| e.has_focus());
        if filling {
            self.finish_field();
        }
        filling
    }

    /// The document was read again from its file: an editor open is of a field that may be gone.
    pub(super) fn drop_field(&self) {
        if let Some(editor) = self.filling.borrow_mut().take() {
            editor.close(&self.overlay);
        }
    }

    /// The keyboard back to the page once an editor has closed, unless another opened meanwhile.
    fn page_takes_keys(self: &Rc<Self>) {
        let tab = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let idle = |t: &Rc<PdfTab>| {
                let open = t.choosing.borrow().is_some() || t.filling.borrow().is_some();
                !open && t.typing.borrow().is_none()
            };
            if let Some(tab) = tab.upgrade().filter(idle) {
                tab.view.grab_focus();
            }
        });
    }

    /// What the tab knows of the field `field` says `text` at once, the page being read again
    /// once the fill has landed.
    fn know_text(&self, field: pdf::Ref, text: &str) {
        for fields in self.fields.borrow_mut().values_mut() {
            for f in fields.iter_mut().filter(|f| f.field == field) {
                if let FieldKind::Text { value, .. } = &mut f.kind {
                    *value = text.to_string();
                }
            }
        }
    }

    /// Turn `field`'s check box or radio group on in the state `on`, or off. What the tab knows of
    /// its widgets says so at once, the pages being read again once the fill has landed, so a
    /// second click goes on from this one.
    fn fill_state(&self, field: &FormField, on: Option<Vec<u8>>) {
        for fields in self.fields.borrow_mut().values_mut() {
            for f in fields.iter_mut().filter(|f| f.field == field.field) {
                if let FieldKind::Check { on: state, checked }
                | FieldKind::Radio {
                    on: state, checked, ..
                } = &mut f.kind
                {
                    *checked = on.as_ref() == Some(state);
                }
            }
        }
        let value = FieldValue::On(on);
        self.ask(Request::Fill {
            field: field.field,
            value,
        });
    }
}

/// Whether `field` takes a click: one a reader fills in, or a push button that does something.
pub(super) fn takes_clicks(field: &FormField) -> bool {
    let button = matches!(field.kind, FieldKind::Button) && field.action.is_some();
    fillable(field) || (button && !field.hidden && !field.read_only)
}

/// Whether `b` starts a row below `a`'s on the page: its middle under `a`'s bottom, and `a`'s
/// middle above its top.
fn above(a: &FormField, b: &FormField) -> bool {
    let middle = |f: &FormField| (f.rect.top + f.rect.bottom) / 2.0;
    middle(a) <= b.rect.top && middle(b) >= a.rect.bottom
}

fn middle_x(f: &FormField) -> f32 {
    (f.rect.left + f.rect.right) / 2.0
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
    match (&field.kind, takes_clicks(field)) {
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

    /// Where the field's editor stands over the page, how big its text is, and whether it holds
    /// the keyboard.
    pub fn filling_at(&self) -> Option<(accent_drawio::Rect, f64, bool)> {
        let editor = self.filling.borrow().clone()?;
        let (at, _, px) = editor.at();
        Some((at, px, editor.has_focus()))
    }

    /// The name of the field the keyboard is on.
    pub fn focused_field(&self) -> Option<String> {
        let (page, widget) = self.focused.get()?;
        let fields = self.fields.borrow();
        let field = fields.get(&page)?.iter().find(|f| f.widget == widget)?;
        Some(field.name.clone())
    }

    /// The list a choice field picks from, while it is open.
    pub fn choosing(&self) -> Option<gtk::Popover> {
        self.choosing.borrow().clone()
    }
}
