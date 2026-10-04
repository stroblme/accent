//! The Properties pane's Layers group: the page's layers, the topmost first, each with its name,
//! whether it shows and whether it is locked, and the current one — where new cells go — marked.
//!
//! Each row writes one change through the tab, which is one undo step, as the pane's other rows
//! do. Picking the current layer is no edit at all: it is kept for the session, as draw.io keeps
//! its default parent (`Editor::set_current_layer`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_drawio::CellId;
use adw::prelude::*;

use crate::widgets::{hover_revealer, icon_button, reveal_on_hover, set_class};

/// One layer as the list shows it.
pub struct Layer {
    pub id: CellId,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
}

/// What the group asks for.
pub enum LayerChange {
    Add,
    /// Make it the layer new cells go into.
    Pick(CellId),
    Rename(CellId, String),
    Show(CellId, bool),
    Lock(CellId, bool),
    /// Up the list, which is in front of the layers below it, or down it.
    Raise(CellId, bool),
    Delete(CellId),
}

/// What draw.io's Layers dialog calls a layer with no name.
const UNNAMED: &str = "Background";

/// The class that marks the current layer's row with an accent bar.
const CURRENT: &str = "accent-current-layer";

struct Row {
    row: adw::EntryRow,
    /// Whether the layer shows and whether it is locked, as last filled: what a press flips.
    visible: Rc<Cell<bool>>,
    locked: Rc<Cell<bool>>,
    eye: gtk::Button,
    lock: gtk::Button,
    up: gtk::Button,
    down: gtk::Button,
    delete: gtk::Button,
}

pub struct Layers {
    pub group: adw::PreferencesGroup,
    rows: RefCell<Vec<(CellId, Row)>>,
    send: Rc<dyn Fn(LayerChange)>,
}

impl Layers {
    pub fn new(send: impl Fn(LayerChange) + 'static) -> Layers {
        let group = adw::PreferencesGroup::builder().title("Layers").build();
        let send: Rc<dyn Fn(LayerChange)> = Rc::new(send);
        let add = icon_button("list-add-symbolic", "Add Layer");
        let on_add = send.clone();
        add.connect_clicked(move |_| on_add(LayerChange::Add));
        group.set_header_suffix(Some(&add));
        Layers {
            group,
            rows: RefCell::new(Vec::new()),
            send,
        }
    }

    /// Show `layers`, the topmost first, marking `current`. The rows are kept while the same
    /// layers stand in the same order, so a toggle pressed or a name being typed stays where it
    /// is; a layer added, removed or moved builds them again.
    pub fn fill(&self, layers: &[Layer], current: Option<&str>) {
        let same = {
            let rows = self.rows.borrow();
            let ids = rows.iter().map(|(id, _)| id);
            ids.eq(layers.iter().map(|l| &l.id))
        };
        if !same {
            for (_, row) in self.rows.take() {
                self.group.remove(&row.row);
            }
            let rows: Vec<(CellId, Row)> = layers
                .iter()
                .map(|l| (l.id.clone(), self.row(&l.id)))
                .collect();
            for (_, row) in &rows {
                self.group.add(&row.row);
            }
            *self.rows.borrow_mut() = rows;
        }
        let rows = self.rows.borrow();
        let n = rows.len();
        for (i, (layer, (_, row))) in layers.iter().zip(rows.iter()).enumerate() {
            // An unnamed layer reads "Background", which typing a name floats away.
            row.row
                .set_title(if layer.name.is_empty() { UNNAMED } else { "" });
            let typing = row
                .row
                .state_flags()
                .contains(gtk::StateFlags::FOCUS_WITHIN);
            if !typing && row.row.text() != layer.name {
                row.row.set_text(&layer.name);
            }
            row.visible.set(layer.visible);
            row.locked.set(layer.locked);
            match layer.visible {
                true => name_button(&row.eye, "view-reveal-symbolic", "Hide Layer"),
                false => name_button(&row.eye, "view-conceal-symbolic", "Show Layer"),
            }
            match layer.locked {
                true => name_button(&row.lock, "changes-prevent-symbolic", "Unlock Layer"),
                false => name_button(&row.lock, "changes-allow-symbolic", "Lock Layer"),
            }
            let picked = current == Some(layer.id.as_str());
            set_class(&row.row, CURRENT, picked);
            row.row
                .upcast_ref::<gtk::Widget>()
                .update_state(&[gtk::accessible::State::Selected(Some(picked))]);
            row.up.set_sensitive(i > 0);
            row.down.set_sensitive(i + 1 < n);
            row.delete.set_sensitive(n > 1);
        }
    }

    /// A row for layer `id`: its name to edit, Move Up, Move Down and Delete sliding out while the
    /// pointer or the keyboard is on it, and whether it shows and is locked always there.
    fn row(&self, id: &CellId) -> Row {
        let row = adw::EntryRow::builder().show_apply_button(true).build();
        let send = |change: fn(CellId) -> LayerChange| {
            let (send, id) = (self.send.clone(), id.clone());
            move |_: &gtk::Button| send(change(id.clone()))
        };
        let up = icon_button("go-up-symbolic", "Move Layer Up");
        up.connect_clicked(send(|id| LayerChange::Raise(id, true)));
        let down = icon_button("go-down-symbolic", "Move Layer Down");
        down.connect_clicked(send(|id| LayerChange::Raise(id, false)));
        let delete = icon_button("user-trash-symbolic", "Delete Layer");
        delete.connect_clicked(send(LayerChange::Delete));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        for button in [&up, &down, &delete] {
            actions.append(button);
        }
        let revealer = hover_revealer();
        revealer.set_child(Some(&actions));
        row.add_suffix(&revealer);
        let (visible, locked) = (Rc::new(Cell::new(true)), Rc::new(Cell::new(false)));
        let eye = icon_button("view-reveal-symbolic", "Hide Layer");
        let lock = icon_button("changes-allow-symbolic", "Lock Layer");
        for (button, state, flip) in [
            (
                &eye,
                &visible,
                LayerChange::Show as fn(CellId, bool) -> LayerChange,
            ),
            (&lock, &locked, LayerChange::Lock),
        ] {
            let (send, id, state) = (self.send.clone(), id.clone(), state.clone());
            button.connect_clicked(move |_| send(flip(id.clone(), !state.get())));
            row.add_suffix(button);
        }
        reveal_on_hover(&row);
        let (send, layer) = (self.send.clone(), id.clone());
        row.connect_apply(move |row| send(LayerChange::Rename(layer.clone(), row.text().into())));
        // A press anywhere on the row but its buttons picks the layer, as a click on a row of
        // draw.io's Layers dialog does; Return in its name does too, for the keyboard.
        let press = gtk::GestureClick::new();
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (send, layer) = (self.send.clone(), id.clone());
        press.connect_pressed(move |gesture, _, x, y| {
            let Some(row) = gesture.widget() else { return };
            let on = row.pick(x, y, gtk::PickFlags::DEFAULT);
            if on
                .and_then(|w| w.ancestor(gtk::Button::static_type()))
                .is_none()
            {
                send(LayerChange::Pick(layer.clone()));
            }
        });
        row.add_controller(press);
        let (send, layer) = (self.send.clone(), id.clone());
        row.connect_entry_activated(move |_| send(LayerChange::Pick(layer.clone())));
        Row {
            row,
            visible,
            locked,
            eye,
            lock,
            up,
            down,
            delete,
        }
    }

    /// Each row as a drill reads it: its name, whether it shows, is locked and is current.
    #[cfg(feature = "bench")]
    pub fn describe(&self) -> Vec<String> {
        let rows = self.rows.borrow();
        let state = |(id, row): &(CellId, Row)| {
            let name = match row.row.text().as_str() {
                "" => row.row.title().to_string(),
                name => name.to_string(),
            };
            let flags = [
                (!row.visible.get(), " hidden"),
                (row.locked.get(), " locked"),
                (row.row.has_css_class(CURRENT), " current"),
            ];
            let flags: String = flags.iter().filter(|f| f.0).map(|f| f.1).collect();
            format!("{id}:{name}{flags}")
        };
        rows.iter().map(state).collect()
    }

    /// The row of layer `id`, for a drill to press its buttons and type its name.
    #[cfg(feature = "bench")]
    pub fn row_of(&self, id: &str) -> Option<adw::EntryRow> {
        let rows = self.rows.borrow();
        rows.iter()
            .find(|(l, _)| l == id)
            .map(|(_, r)| r.row.clone())
    }
}

/// Give a toggle-like button the icon and the words of what a press on it does now.
fn name_button(button: &gtk::Button, icon: &str, tooltip: &str) {
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
}
