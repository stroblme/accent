//! The Breakpoints section's rows: each breakpoint, whose line a click opens, and under it, while
//! its Edit Breakpoint button has opened them, the fields that set what it does beside stopping.
//!
//! The rows are updated in place, never made again whole: an open editor stays open while the
//! other rows change (`sync_code_pane` refills them on every tab switch).

use crate::debug::breakpoints::{described, icon};
use crate::widgets::icon_button;
use accent_api::debug::{Breakpoint, Supports};
use adw::prelude::*;
use gtk::{glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// A file key and one of its breakpoints, as the window lists them.
pub type Row = (String, Breakpoint);

/// What a row hands on of a breakpoint edited: its file key, and it with what is now set beside
/// its line.
pub type Edited = Rc<dyn Fn(&str, Breakpoint)>;

/// What a field sets of a breakpoint.
type Setting = fn(&mut Breakpoint) -> &mut Option<String>;

/// The fields an editor offers, each where the adapter takes it: title, tooltip, setting.
const FIELDS: [(&str, &str, Setting); 3] = [
    (
        "Condition",
        "Stop only where this expression is true",
        |b| &mut b.condition,
    ),
    (
        "Hit Count",
        "Stop only on the hits this names, as 5, >= 5 or % 5",
        |b| &mut b.hit_condition,
    ),
    (
        "Log Message",
        "Print this to the Debug Console instead of stopping; {expressions} are evaluated",
        |b| &mut b.log_message,
    ),
];

pub(super) struct Rows {
    pub(super) list: gtk::ListBox,
    lines: RefCell<Vec<Line>>,
    supports: Cell<Option<Supports>>,
    editing: RefCell<Option<Editing>>,
    /// A row activated, as its `path:line`.
    open: Rc<dyn Fn(&str)>,
    edit: Edited,
}

/// A breakpoint's row and the widgets that change with it.
struct Line {
    row: gtk::ListBoxRow,
    key: String,
    set: Breakpoint,
    glyph: gtk::Image,
    detail: gtk::Label,
    button: gtk::Button,
}

/// The fields open under a row, as a row of their own.
struct Editing {
    key: String,
    /// The breakpoint as it was when the editor opened; the fields overwrite what they show.
    set: Breakpoint,
    row: gtk::ListBoxRow,
    fields: Vec<(Setting, adw::EntryRow)>,
    /// The window's focus moving out of the editor commits it.
    focus: Option<(gtk::Window, glib::SignalHandlerId)>,
}

impl Rows {
    pub(super) fn new(open: Rc<dyn Fn(&str)>, edit: Edited) -> Rc<Rows> {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        let rows = Rc::new(Rows {
            list,
            lines: RefCell::default(),
            supports: Cell::default(),
            editing: RefCell::default(),
            open,
            edit,
        });
        // One click opens the line, as a result list's row does.
        let weak = Rc::downgrade(&rows);
        rows.list.connect_row_activated(move |_, row| {
            let Some(rows) = weak.upgrade() else {
                return;
            };
            let lines = rows.lines.borrow();
            if let Some(line) = lines.iter().find(|l| l.row == *row) {
                let at = format!("{}:{}", line.key, line.set.line + 1);
                let open = rows.open.clone();
                drop(lines);
                open(&at);
            }
        });
        rows
    }

    /// Show `rows`, updating those already shown in place: an open editor stays while its
    /// breakpoint does, and goes, committing what it holds, with it.
    pub(super) fn set(self: &Rc<Self>, rows: &[Row], supports: Supports) {
        self.supports.set(Some(supports));
        let mut old = std::mem::take(&mut *self.lines.borrow_mut())
            .into_iter()
            .peekable();
        let mut lines = Vec::new();
        let place = |l: &Line| (l.key.clone(), l.set.line);
        for (key, set) in rows {
            let at = (key.clone(), set.line);
            while let Some(gone) = old.next_if(|l| place(l) < at) {
                self.forget(gone);
            }
            match old.next_if(|l| place(l) == at) {
                Some(mut line) => {
                    line.set = set.clone();
                    line.show(supports);
                    lines.push(line);
                }
                None => {
                    let line = self.line(key, set.clone());
                    line.show(supports);
                    // Before the next row kept, after the editor of the one before it.
                    let index = old.peek().map_or(-1, |next| next.row.index());
                    self.list.insert(&line.row, index);
                    lines.push(line);
                }
            }
        }
        old.for_each(|gone| self.forget(gone));
        self.lines.replace(lines);
    }

    /// Take away a row whose breakpoint is gone, and its editor with it.
    fn forget(&self, gone: Line) {
        let edited = self
            .editing
            .borrow()
            .as_ref()
            .map(|e| (e.key.clone(), e.set.line));
        if edited == Some((gone.key.clone(), gone.set.line)) {
            self.close(true);
        }
        self.list.remove(&gone.row);
    }

    /// A row for `set`, its button opening the editor, or committing the one open under it.
    fn line(self: &Rc<Self>, key: &str, set: Breakpoint) -> Line {
        let glyph = gtk::Image::new();
        let place = gtk::Label::builder()
            .label(format!("{key}:{}", set.line + 1))
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        detail.add_css_class("dimmed");
        let button = icon_button("document-edit-symbolic", "Edit Breakpoint");
        // The keyboard stays where it is, so a press is never taken for leaving an editor.
        button.set_focus_on_click(false);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        line.append(&glyph);
        line.append(&place);
        line.append(&detail);
        line.append(&button);
        let row = gtk::ListBoxRow::builder().child(&line).build();
        let weak = Rc::downgrade(self);
        let (key_, line_) = (key.to_string(), set.line);
        button.connect_clicked(move |_| {
            if let Some(rows) = weak.upgrade() {
                rows.toggle_editor(&key_, line_);
            }
        });
        Line {
            row,
            key: key.to_string(),
            set,
            glyph,
            detail,
            button,
        }
    }

    /// Open the editor under the breakpoint on `line` of `key`, committing one open elsewhere;
    /// or commit its own, open already.
    fn toggle_editor(self: &Rc<Self>, key: &str, line: u32) {
        let open = self
            .editing
            .borrow()
            .as_ref()
            .map(|e| (e.key.clone(), e.set.line));
        if open.is_some() {
            self.close(true);
        }
        if open != Some((key.to_string(), line)) {
            self.open_editor(key, line);
        }
    }

    fn open_editor(self: &Rc<Self>, key: &str, line: u32) {
        let lines = self.lines.borrow();
        let Some(at) = lines.iter().find(|l| l.key == key && l.set.line == line) else {
            return;
        };
        let Some(supports) = self.supports.get() else {
            return;
        };
        let offered = [
            supports.condition,
            supports.hit_condition,
            supports.log_message,
        ];
        let boxed = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .margin_start(6)
            .margin_end(6)
            .margin_top(3)
            .margin_bottom(6)
            .build();
        boxed.add_css_class("boxed-list");
        let mut set = at.set.clone();
        let mut fields = Vec::new();
        for ((title, tooltip, setting), _) in FIELDS.into_iter().zip(offered).filter(|f| f.1) {
            let field = adw::EntryRow::builder()
                .title(title)
                .text(setting(&mut set).as_deref().unwrap_or_default())
                .tooltip_text(tooltip)
                .build();
            let weak = Rc::downgrade(self);
            field.connect_entry_activated(move |_| later(&weak, true));
            boxed.append(&field);
            fields.push((setting, field));
        }
        let row = gtk::ListBoxRow::builder()
            .child(&boxed)
            .activatable(false)
            .selectable(false)
            .build();
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            later(&weak, false);
            glib::Propagation::Stop
        });
        row.add_controller(keys);
        self.list.insert(&row, at.row.index() + 1);
        drop(lines);
        if let Some((_, first)) = fields.first() {
            first.grab_focus();
        }
        // All of it in sight once it is laid out, not only the field with the keyboard.
        let shown = row.downgrade();
        glib::idle_add_local_once(move || {
            let shown = shown.upgrade();
            let viewport = shown
                .as_ref()
                .and_then(|r| r.ancestor(gtk::Viewport::static_type()));
            if let (Some(row), Some(viewport)) = (shown, viewport.and_downcast::<gtk::Viewport>()) {
                viewport.scroll_to(&row, None);
            }
        });
        let window = row.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(self);
        let inside = row.clone();
        let focus = window.map(|window| {
            let id = window.connect_focus_widget_notify(move |window| {
                let within = GtkWindowExt::focus(window).is_some_and(|f| f.is_ancestor(&inside));
                if !within {
                    later(&weak, true);
                }
            });
            (window, id)
        });
        self.editing.replace(Some(Editing {
            key: key.to_string(),
            set,
            row,
            fields,
            focus,
        }));
    }

    /// Close the editor, handing on what it holds when `commit`, once this call is over (the
    /// window answers with the rows again); the keyboard goes back to its breakpoint's row where
    /// the editor had it.
    fn close(&self, commit: bool) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let mut had_keyboard = false;
        if let Some((window, id)) = editing.focus {
            had_keyboard =
                GtkWindowExt::focus(&window).is_some_and(|f| f.is_ancestor(&editing.row));
            window.disconnect(id);
        }
        self.list.remove(&editing.row);
        let line = editing.set.line;
        if had_keyboard
            && let Some(at) = self
                .lines
                .borrow()
                .iter()
                .find(|l| l.key == editing.key && l.set.line == line)
        {
            at.row.grab_focus();
        }
        if commit {
            let mut edited = editing.set;
            for (setting, field) in &editing.fields {
                let text = field.text().trim().to_string();
                *setting(&mut edited) = (!text.is_empty()).then_some(text);
            }
            let edit = self.edit.clone();
            glib::idle_add_local_once(move || edit(&editing.key, edited));
        }
    }

    /// The rows as a drill reads them: `path:line`, then what is set beside it.
    #[cfg(feature = "bench")]
    pub(super) fn shown(&self) -> Vec<String> {
        let lines = self.lines.borrow();
        let shown = lines.iter().map(|l| {
            let place = format!("{}:{}", l.key, l.set.line + 1);
            match l.detail.text() {
                detail if detail.is_empty() => place,
                detail => format!("{place} {detail}"),
            }
        });
        shown.collect()
    }

    /// Edit the breakpoint on 0-based `line` of `key` as a reader does: its button, `values`
    /// typed into the fields with those titles, and Enter in the first.
    #[cfg(feature = "bench")]
    pub(super) fn edit_as_typed(self: &Rc<Self>, key: &str, line: u32, values: &[(&str, &str)]) {
        let button = self
            .lines
            .borrow()
            .iter()
            .find_map(|l| (l.key == key && l.set.line == line).then(|| l.button.clone()));
        let Some(button) = button else {
            return;
        };
        button.emit_clicked();
        let fields: Vec<adw::EntryRow> = match &*self.editing.borrow() {
            Some(editing) => editing.fields.iter().map(|(_, f)| f.clone()).collect(),
            None => return,
        };
        for field in &fields {
            if let Some((_, value)) = values.iter().find(|(title, _)| field.title() == *title) {
                field.set_text(value);
            }
        }
        if let Some(first) = fields.first() {
            first.emit_by_name::<()>("entry-activated", &[]);
        }
    }
}

/// Close the editor once the signal that asks for it is over, which a field inside it may be
/// emitting.
fn later(rows: &Weak<Rows>, commit: bool) {
    let rows = rows.clone();
    glib::idle_add_local_once(move || {
        if let Some(rows) = rows.upgrade() {
            rows.close(commit);
        }
    });
}

impl Line {
    /// Show what the breakpoint is: its glyph, dimmed what it does beside stopping (its log
    /// message, else its condition, else its hit count), all of it in the tooltip, and the edit
    /// button while the adapter takes any of it.
    fn show(&self, supports: Supports) {
        let set = &self.set;
        self.glyph.set_icon_name(Some(icon(set)));
        let detail = match set {
            Breakpoint {
                log_message: Some(said),
                ..
            }
            | Breakpoint {
                condition: Some(said),
                ..
            } => said.clone(),
            Breakpoint {
                hit_condition: Some(hits),
                ..
            } => format!("hits {hits}"),
            _ => String::new(),
        };
        self.detail.set_text(&detail);
        let described = described(set).join("\n");
        self.row
            .set_tooltip_text((!described.is_empty()).then_some(described.as_str()));
        let any = supports.condition || supports.hit_condition || supports.log_message;
        self.button.set_visible(any);
    }
}
