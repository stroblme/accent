//! Properties for selected cells, or the page when nothing is selected.
//!
//! Each change uses the tab's undo path; spin rows debounce bursts, entry rows hold what is typed
//! until Return or the keyboard leaves them, and a change still held lands before any other
//! (`Props::flush`). `filling` prevents model-to-widget notifications
//! from becoming edits. Geometry and style groups reveal Copy/Paste buttons on hover or focus
//! (`widgets::reveal_on_hover`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use accent_drawio::{Color, Point, Rect, Resolved};
use adw::prelude::*;
use gtk::glib;

use super::Tool;
use crate::widgets::{Debounce, hover_revealer, icon_button, reveal_on_hover};

pub(super) mod effects;
mod ends;
mod line;
mod tab;
mod text;

/// What the pane shows.
pub enum Target {
    /// Cells: the first one's resolved style and its style string, and what kinds are selected.
    Cells {
        style: Resolved,
        raw: String,
        count: usize,
        vertices: bool,
        edges: bool,
        /// Some cell takes a fill: a shape, or a flex arrow ([`accent_drawio::Cell::takes_fill`]).
        fills: bool,
        /// Some cell is a swimlane, whose divider draw.io offers (`ss.swimlane`).
        swimlane: bool,
        /// What the switches beside Rounded can change on them.
        effects: effects::Shown,
        /// The one shape selected: its place and size in its parent, as the file has them, and
        /// whether it is pinned where it is (locked, or on a locked layer).
        geometry: Option<(Rect, bool)>,
        /// A creation tool this cell can set the default for, and whether the cell's style is
        /// that default.
        default_tool: Option<(Tool, bool)>,
    },
    Page {
        name: String,
        size: (f64, f64),
        background: Option<Color>,
    },
}

/// One change the pane asks for.
pub enum Change {
    /// Style keys on every selected cell, as one step.
    Style(Vec<(&'static str, Option<String>)>),
    /// Style keys on the selected arrows alone, as draw.io's arrow rows set them (`ss.edges`).
    EdgeStyle(Vec<(&'static str, Option<String>)>),
    /// The whole style string of the one selected cell.
    Raw(String),
    /// The one selected cell's style as its tool's Default Style, or the default cleared where
    /// the style is it already.
    DefaultStyle,
    /// The one selected cell's style in an editor tab of its own.
    EditStyle,
    PageAttr(&'static str, Option<String>),
    PageName(String),
    /// The one selected shape's place and size in its parent.
    Geometry(Rect),
    /// The one selected line's free start (else end), at a point in its parent.
    End(bool, Point),
    /// draw.io's Flip of the selected shapes, across (else up and down).
    Flip(bool),
    /// Fixed Corner Size or Fixed Size on or off, each selected shape kept as it is drawn.
    Fixed(accent_drawio::Fixed, bool),
    /// A group's Copy button: what it shows, onto the clipboard.
    Copy(Clip),
    /// A group's Paste button: what the clipboard holds, onto the selection.
    Paste(Clip),
    /// A row of the Layers group.
    Layer(super::layers::LayerChange),
    /// What a geometry or Rotation row holds, for the canvas to show until it lands.
    Held(Placed),
}

/// The one shape's place and size in its parent or its turn in degrees, or the one line's free
/// ends (`None` one left as it is): what a drag under way gives it, or what a row holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placed {
    Frame(Rect),
    Turn(f64),
    Ends([Option<Point>; 2]),
}

/// What a Copy or Paste button carries: `x, y`, `w, h`, or the style string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clip {
    Position,
    Size,
    Style,
}

impl Clip {
    const ALL: [Clip; 3] = [Clip::Position, Clip::Size, Clip::Style];

    /// What it carries, as a toast says it.
    pub fn noun(self) -> &'static str {
        match self {
            Clip::Position => "position",
            Clip::Size => "size",
            Clip::Style => "style",
        }
    }

    /// The format of our own a copy puts beside its text, since a position and a size read
    /// alike.
    fn mime(self) -> &'static str {
        match self {
            Clip::Position => "application/x-accent-diagram-position",
            Clip::Size => "application/x-accent-diagram-size",
            Clip::Style => "application/x-accent-diagram-style",
        }
    }

    /// What a Copy button put on `clipboard`, if that is what it holds.
    pub fn held(clipboard: &gtk::gdk::Clipboard) -> Option<Clip> {
        let formats = clipboard.formats();
        Clip::ALL
            .into_iter()
            .find(|clip| formats.contain_mime_type(clip.mime()))
    }
}

/// draw.io's gradient directions (Format panel), south when a style names none.
const DIRECTIONS: [(&str, &str); 5] = [
    ("north", "North"),
    ("east", "East"),
    ("south", "South"),
    ("west", "West"),
    ("radial", "Radial"),
];

type OnChange = Rc<RefCell<Option<Box<dyn Fn(Change)>>>>;
/// Spin rows' changes waiting for their burst to settle, and entry rows' text waiting for Return
/// or the keyboard leaving them, one a slot (the four geometry rows share one), in the order
/// they were made.
type Held = RefCell<Vec<(&'static str, Box<dyn FnOnce()>)>>;

/// A colour a cell may also have none of: a switch for whether it has one, and the colour.
struct ColourRow {
    row: adw::ActionRow,
    switch: gtk::Switch,
    button: gtk::ColorDialogButton,
    /// What the switch turns on while there is no colour: draw.io's `default`, ink or paper.
    blank: Color,
}

impl ColourRow {
    fn new(title: &str, blank: Color) -> ColourRow {
        let button = gtk::ColorDialogButton::new(Some(
            gtk::ColorDialog::builder().with_alpha(false).build(),
        ));
        button.set_valign(gtk::Align::Center);
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        let row = adw::ActionRow::builder().title(title).build();
        row.add_suffix(&button);
        row.add_suffix(&switch);
        ColourRow {
            row,
            switch,
            button,
            blank,
        }
    }

    fn fill(&self, colour: Option<Color>) {
        self.switch.set_active(colour.is_some());
        self.button.set_visible(colour.is_some());
        self.button
            .set_rgba(&super::paint::rgba(colour.unwrap_or(self.blank)));
    }

    /// What the row says now, as a style value: the colour, or `none`.
    fn value(&self) -> String {
        match self.switch.is_active() {
            true => hex(self.button.rgba()),
            false => "none".to_string(),
        }
    }

    /// Call `f` with the new value whenever either half changes.
    fn connect(self: &Rc<Self>, filling: &Rc<Cell<bool>>, f: impl Fn(String) + 'static) {
        let f = Rc::new(f);
        let (me, guard, g) = (Rc::downgrade(self), filling.clone(), f.clone());
        self.switch.connect_active_notify(move |switch| {
            if let Some(me) = me.upgrade() {
                me.button.set_visible(switch.is_active());
                if !guard.get() {
                    g(me.value());
                }
            }
        });
        let (me, guard) = (Rc::downgrade(self), filling.clone());
        self.button.connect_rgba_notify(move |_| {
            if let Some(me) = me.upgrade()
                && !guard.get()
            {
                f(me.value());
            }
        });
    }
}

pub struct Props {
    root: gtk::ScrolledWindow,
    filling: Rc<Cell<bool>>,
    on_change: OnChange,
    position: adw::PreferencesGroup,
    size: adw::PreferencesGroup,
    x: adw::SpinRow,
    y: adw::SpinRow,
    width: adw::SpinRow,
    height: adw::SpinRow,
    ends: ends::Ends,
    /// Each group's Copy and Paste buttons, by what they carry.
    clips: Vec<(Clip, gtk::Button, gtk::Button)>,
    /// The page's layers, shown whatever is selected.
    layers: super::layers::Layers,
    shape: adw::PreferencesGroup,
    text: text::Text,
    line: line::Line,
    raw_group: adw::PreferencesGroup,
    page_group: adw::PreferencesGroup,
    fill: Rc<ColourRow>,
    gradient: Rc<ColourRow>,
    /// Whether the cells shown take a gradient: a fill that is not a picture's background.
    takes_gradient: Cell<bool>,
    direction: adw::ComboRow,
    /// The fill's own opacity (`fillOpacity`), which leaves the line and the label as they are.
    fill_opacity: adw::SpinRow,
    rounded: adw::SwitchRow,
    arc_size: adw::SpinRow,
    effects: effects::Effects,
    /// A swimlane's divider under its title (`swimlaneLine`).
    divider: adw::SwitchRow,
    shadow: adw::SwitchRow,
    rotation: adw::SpinRow,
    flip: [gtk::Button; 2],
    /// Constrain Proportions (`aspect=fixed`).
    aspect: adw::SwitchRow,
    /// The size the Width and Height rows had, which Constrain Proportions scales from.
    size_was: Cell<(f64, f64)>,
    /// Whether a drag is under way on the canvas, which the rows show rather than the model.
    dragging: Cell<bool>,
    raw: adw::EntryRow,
    default_style: gtk::Button,
    edit_style: gtk::Button,
    page_name: adw::EntryRow,
    background: Rc<ColourRow>,
    page_width: adw::SpinRow,
    page_height: adw::SpinRow,
    debounce: Debounce,
    held: Held,
}

/// An entry with no apply button, what is typed being applied on Return or as the keyboard leaves
/// it (`Props::typed`). Hide the idle pencil because clicking the row already starts editing, and
/// the apply button and the indicator it shares a place with, whose room an entry row keeps at
/// the text's end even while neither shows.
pub(super) fn entry_row(title: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).build();
    for part in ["edit-icon", "apply-button", "indicator"] {
        hide_part(&row, part);
    }
    row
}

/// Hide the part of `row` with CSS class `class` (the pencil, `edit-icon`) with `visible`;
/// libadwaita toggles child visibility, so this remains hidden through editing-state changes.
pub(super) fn hide_part(row: &adw::EntryRow, class: &str) {
    let mut todo = vec![row.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = todo.pop() {
        if widget.has_css_class(class) {
            widget.set_visible(false);
            return;
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
}

fn combo(title: &str, labels: &[&str]) -> adw::ComboRow {
    adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(labels))
        .build()
}

fn spin(title: &str, range: (f64, f64, f64)) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(range.0, range.1, range.2);
    row.set_title(title);
    row
}

/// A row of the geometry or a corner's size: whole units a step, fractions kept, shown as the
/// file writes them.
fn coordinate(title: &str, low: f64) -> adw::SpinRow {
    let row = spin(title, (low, 1e6, 1.0));
    row.set_digits(2);
    row.connect_output(|row| {
        row.set_text(&number((row.value() * 100.0).round() / 100.0));
        true
    });
    row
}

/// Copy and Paste buttons for a group's header, sliding out while the pointer or the keyboard
/// is on the group, `what` naming what they carry.
fn clip_buttons(group: &adw::PreferencesGroup, what: &str) -> (gtk::Button, gtk::Button) {
    let copy = icon_button("edit-copy-symbolic", &format!("Copy {what}"));
    let paste = icon_button("edit-paste-symbolic", &format!("Paste {what}"));
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    buttons.append(&copy);
    buttons.append(&paste);
    let revealer = hover_revealer();
    revealer.set_child(Some(&buttons));
    group.set_header_suffix(Some(&revealer));
    reveal_on_hover(group);
    (copy, paste)
}

/// The `rotation` a turn of `degrees` writes: none for no turn, as a shape draw.io never turned
/// has none.
pub(super) fn rotation(degrees: f64) -> Option<String> {
    (degrees != 0.0).then(|| number(degrees))
}

/// A number as a style value: `12`, not `12.0`, and to a millionth, so five steps of a
/// hundredth from 0.1 are written `0.15`, not `0.15000000000000002`.
pub(super) fn number(v: f64) -> String {
    let v = (v * 1e6).round() / 1e6;
    match v.fract() == 0.0 {
        true => format!("{}", v as i64),
        false => format!("{v}"),
    }
}

impl Props {
    /// What the Fill row says, for a drill.
    #[cfg(feature = "bench")]
    pub fn fill_value(&self) -> String {
        self.fill.value()
    }

    pub fn new() -> Rc<Props> {
        let page = adw::PreferencesPage::new();
        let group = |title: &str| {
            let g = adw::PreferencesGroup::builder().title(title).build();
            page.add(&g);
            g
        };
        let (position, size) = (group("Position"), group("Size"));
        let ends = ends::Ends::new([group("Line Start"), group("Line End")]);
        let (x, y) = (coordinate("X", -1e6), coordinate("Y", -1e6));
        let (width, height) = (coordinate("Width", 0.0), coordinate("Height", 0.0));
        position.add(&x);
        position.add(&y);
        size.add(&width);
        size.add(&height);
        let aspect = adw::SwitchRow::builder()
            .title("Constrain Proportions")
            .build();
        size.add(&aspect);
        let (shape, line, text, raw_group, page_group) = (
            group("Shape"),
            group("Line"),
            group("Text"),
            group("Style"),
            group("Page"),
        );

        let fill = Rc::new(ColourRow::new("Fill", Color::WHITE));
        let gradient = Rc::new(ColourRow::new("Gradient", Color::WHITE));
        let direction = combo("Gradient Direction", &DIRECTIONS.map(|(_, l)| l));
        let fill_opacity = spin("Fill Opacity", (0.0, 100.0, 5.0));
        let rounded = adw::SwitchRow::builder().title("Rounded").build();
        let arc_size = coordinate("Arc Size", 0.0);
        let effects = effects::Effects::new();
        let divider = adw::SwitchRow::builder().title("Divider").build();
        let shadow = adw::SwitchRow::builder().title("Shadow").build();
        let rotation = spin("Rotation", (-180.0, 180.0, 1.0));
        shape.add(&fill.row);
        shape.add(&gradient.row);
        shape.add(&direction);
        shape.add(&fill_opacity);
        shape.add(&rounded);
        shape.add(&arc_size);
        effects.add_fixed(&shape);
        shape.add(&divider);
        shape.add(&effects.sketch);
        shape.add(&shadow);
        shape.add(&rotation);
        let flip = [
            icon_button("object-flip-horizontal-symbolic", "Flip Horizontally"),
            icon_button("object-flip-vertical-symbolic", "Flip Vertically"),
        ];
        let flip_row = adw::ActionRow::builder().title("Flip").build();
        for button in &flip {
            flip_row.add_suffix(button);
        }
        shape.add(&flip_row);
        let line = line::Line::new(line);
        let text = text::Text::new(text);

        let raw = entry_row("Style");
        raw.add_css_class("monospace");
        let default_style = icon_button("non-starred-symbolic", "Set Default Style");
        default_style.set_valign(gtk::Align::Center);
        raw.add_suffix(&default_style);
        let edit_style = icon_button("document-edit-symbolic", "Edit Style");
        edit_style.set_valign(gtk::Align::Center);
        raw.add_suffix(&edit_style);
        raw_group.add(&raw);
        let clips = [
            (Clip::Position, &position, "Position"),
            (Clip::Size, &size, "Size"),
            (Clip::Style, &raw_group, "Style"),
        ]
        .map(|(clip, group, what)| {
            let (copy, paste) = clip_buttons(group, what);
            (clip, copy, paste)
        })
        .to_vec();

        let page_name = entry_row("Name");
        let background = Rc::new(ColourRow::new("Background", Color::WHITE));
        let page_width = spin("Width", (100.0, 10000.0, 10.0));
        let page_height = spin("Height", (100.0, 10000.0, 10.0));
        page_group.add(&page_name);
        page_group.add(&background.row);
        page_group.add(&page_width);
        page_group.add(&page_height);
        // The window's preference, not the page's: the switch shows and flips the action's state.
        let grid = gtk::Switch::builder()
            .action_name("win.diagram-grid")
            .valign(gtk::Align::Center)
            .build();
        let grid_row = adw::ActionRow::builder()
            .title(crate::actions::label_of("win.diagram-grid"))
            .activatable_widget(&grid)
            .build();
        grid_row.add_suffix(&grid);
        page_group.add(&grid_row);
        let on_change: OnChange = Rc::new(RefCell::new(None));
        let on = on_change.clone();
        let layers = super::layers::Layers::new(move |change| {
            if let Some(f) = on.borrow().as_ref() {
                f(Change::Layer(change));
            }
        });
        page.add(&layers.group);

        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();
        crate::widgets::smooth_wheel(&root);
        let props = Rc::new(Props {
            root,
            filling: Rc::new(Cell::new(false)),
            on_change,
            position,
            size,
            x,
            y,
            width,
            height,
            ends,
            clips,
            layers,
            shape,
            text,
            line,
            raw_group,
            page_group,
            fill,
            gradient,
            takes_gradient: Cell::new(false),
            direction,
            fill_opacity,
            rounded,
            arc_size,
            effects,
            divider,
            shadow,
            rotation,
            flip,
            aspect,
            size_was: Cell::new((0.0, 0.0)),
            dragging: Cell::new(false),
            raw,
            default_style,
            edit_style,
            page_name,
            background,
            page_width,
            page_height,
            debounce: Debounce::new(Duration::from_millis(300)),
            held: RefCell::new(Vec::new()),
        });
        props.wire();
        props
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn connect_change(&self, f: impl Fn(Change) + 'static) {
        *self.on_change.borrow_mut() = Some(Box::new(f));
    }

    fn wire(self: &Rc<Self>) {
        let send: Rc<dyn Fn(Change)> = {
            let (me, on, filling) = (
                Rc::downgrade(self),
                self.on_change.clone(),
                self.filling.clone(),
            );
            Rc::new(move |change: Change| {
                if filling.get() {
                    return;
                }
                if let Some(me) = me.upgrade() {
                    me.flush();
                }
                if let Some(f) = on.borrow().as_ref() {
                    f(change);
                }
            })
        };
        colour(&self.fill, &self.filling, &send, "fillColor");
        colour(&self.gradient, &self.filling, &send, "gradientColor");
        let toggles: [&glib::Object; 3] = [
            self.fill.switch.upcast_ref(),
            self.gradient.switch.upcast_ref(),
            self.rounded.upcast_ref(),
        ];
        for toggle in toggles {
            let me = Rc::downgrade(self);
            toggle.connect_notify_local(Some("active"), move |_, _| {
                if let Some(me) = me.upgrade() {
                    me.sync_rows();
                }
            });
        }
        self.effects.wire(send.clone());
        self.text.wire(self, send.clone());
        let page_send = send.clone();
        self.background.connect(&self.filling, move |value| {
            let value = (value != "none").then_some(value);
            page_send(Change::PageAttr("background", value));
        });

        let switch = |row: &adw::SwitchRow, key: &'static str, off: Option<&'static str>| {
            let set = style(&send, key);
            row.connect_active_notify(move |row| {
                set(match row.is_active() {
                    true => Some("1".to_string()),
                    false => off.map(str::to_string),
                })
            });
        };
        switch(&self.rounded, "rounded", Some("0"));
        switch(&self.divider, "swimlaneLine", Some("0"));
        switch(&self.shadow, "shadow", None);
        self.spins(
            &send,
            &[
                (&self.fill_opacity, "fillOpacity", Some(100.0), false),
                (&self.arc_size, "arcSize", None, false),
                (&self.rotation, "rotation", Some(0.0), false),
            ],
        );
        // Degrees typed into Rotation show on the canvas as they are typed, and land as the
        // geometry rows' digits do.
        let (me, rotation_send) = (Rc::downgrade(self), send.clone());
        self.rotation.connect_changed(move |row| {
            let Some(me) = me.upgrade().filter(|me| !me.filling.get()) else {
                return;
            };
            let Some(degrees) = typed(row) else { return };
            let send = rotation_send.clone();
            me.keep("rotation", move || {
                send(Change::Style(vec![("rotation", rotation(degrees))]))
            });
            me.held(Placed::Turn(degrees));
        });
        for (row, key) in [
            (&self.page_width, "pageWidth"),
            (&self.page_height, "pageHeight"),
        ] {
            let (me, send) = (Rc::downgrade(self), send.clone());
            row.connect_value_notify(move |row| {
                let Some(me) = me.upgrade() else { return };
                if me.filling.get() {
                    return;
                }
                let (value, send) = (row.value(), send.clone());
                me.hold(key, move || {
                    send(Change::PageAttr(key, Some(number(value))))
                });
            });
        }

        // The four read together when the burst settles, so a second row changed meanwhile
        // does not take back the first one's change; the canvas shows them at once.
        for row in [&self.x, &self.y, &self.width, &self.height] {
            let (me, send) = (Rc::downgrade(self), send.clone());
            row.connect_value_notify(move |_| {
                let Some(me) = me.upgrade() else { return };
                if me.filling.get() {
                    return;
                }
                if me.aspect.is_active() {
                    me.keep_proportions();
                }
                let (weak, send) = (Rc::downgrade(&me), send.clone());
                me.hold("geometry", move || {
                    if let Some(me) = weak.upgrade() {
                        send(Change::Geometry(me.frame()));
                    }
                });
                me.held(Placed::Frame(me.frame()));
            });
        }
        // Digits typed into one show on the canvas as they are typed, and land as Return or the
        // keyboard leaving the row has the spin button read them, one step.
        for (i, row) in [&self.x, &self.y, &self.width, &self.height]
            .into_iter()
            .enumerate()
        {
            let (me, send) = (Rc::downgrade(self), send.clone());
            row.connect_changed(move |row| {
                let Some(me) = me.upgrade().filter(|me| !me.filling.get()) else {
                    return;
                };
                let Some(v) = typed(row) else { return };
                let r = me.frame();
                let mut at = [r.x, r.y, r.w, r.h];
                at[i] = v;
                if i > 1 && me.aspect.is_active() {
                    (at[2], at[3]) = proportioned(me.size_was.get(), (at[2], at[3]));
                }
                let (r, send) = (Rect::new(at[0], at[1], at[2], at[3]), send.clone());
                me.keep("geometry", move || send(Change::Geometry(r)));
                me.held(Placed::Frame(r));
            });
        }
        self.ends.wire(self, send.clone());
        let set = style(&send, "aspect");
        self.aspect
            .connect_active_notify(move |row| set(row.is_active().then(|| "fixed".to_string())));
        for (button, across) in self.flip.iter().zip([true, false]) {
            let send = send.clone();
            button.connect_clicked(move |_| send(Change::Flip(across)));
        }
        for (clip, copy, paste) in &self.clips {
            let (clip, send_copy, send_paste) = (*clip, send.clone(), send.clone());
            copy.connect_clicked(move |_| send_copy(Change::Copy(clip)));
            paste.connect_clicked(move |_| send_paste(Change::Paste(clip)));
        }
        pick(
            &self.direction,
            &send,
            "gradientDirection",
            DIRECTIONS.map(|(v, _)| v).to_vec(),
        );
        self.line.wire(self, send.clone());

        let raw_send = send.clone();
        self.typed(&self.raw, "style", move |text| raw_send(Change::Raw(text)));
        let send_default = send.clone();
        self.default_style
            .connect_clicked(move |_| send_default(Change::DefaultStyle));
        let send_edit = send.clone();
        self.edit_style
            .connect_clicked(move |_| send_edit(Change::EditStyle));
        self.typed(&self.page_name, "page name", move |text| {
            send(Change::PageName(text))
        });
    }

    /// Hold what is typed into `row` as `slot`'s change, `apply` made of its text, until Return or
    /// the keyboard leaving the row lands it; anything else that flushes first lands it on the
    /// cells or the page it was typed for.
    fn typed(
        self: &Rc<Self>,
        row: &adw::EntryRow,
        slot: &'static str,
        apply: impl Fn(String) + 'static,
    ) {
        let (me, apply) = (Rc::downgrade(self), Rc::new(apply));
        row.connect_changed(move |row| {
            let Some(me) = me.upgrade().filter(|me| !me.filling.get()) else {
                return;
            };
            let (apply, text) = (apply.clone(), row.text().to_string());
            me.keep(slot, move || apply(text));
        });
        let me = Rc::downgrade(self);
        row.connect_entry_activated(move |_| {
            if let Some(me) = me.upgrade() {
                me.flush();
            }
        });
        let (me, focus) = (Rc::downgrade(self), gtk::EventControllerFocus::new());
        focus.connect_leave(move |_| {
            if let Some(me) = me.upgrade() {
                me.flush();
            }
        });
        row.add_controller(focus);
    }

    /// Send each spin row's value as its key's once its burst settles, onto the arrows alone
    /// where the row says so: spin rows step once per arrow press, and a burst of them is one
    /// change. A default is where the key is left out, as draw.io leaves out an opacity of 100;
    /// a key a named style can give (`spacingLeft`) or whose default depends on another
    /// (`arcSize`) is always written.
    fn spins(
        self: &Rc<Self>,
        send: &Rc<dyn Fn(Change)>,
        rows: &[(&adw::SpinRow, &'static str, Option<f64>, bool)],
    ) {
        for &(row, key, default, edges) in rows {
            let (me, send) = (Rc::downgrade(self), send.clone());
            row.connect_value_notify(move |row| {
                let Some(me) = me.upgrade() else { return };
                if me.filling.get() {
                    return;
                }
                let (value, send) = (row.value(), send.clone());
                me.hold(key, move || {
                    let pairs = vec![(key, (Some(value) != default).then(|| number(value)))];
                    send(match edges {
                        true => Change::EdgeStyle(pairs),
                        false => Change::Style(pairs),
                    })
                });
                if key == "rotation" {
                    me.held(Placed::Turn(value));
                }
            });
        }
    }

    /// Show `target`, writing nothing back.
    pub fn fill(&self, target: &Target) {
        self.filling.set(true);
        let cells = matches!(target, Target::Cells { .. });
        self.page_group.set_visible(!cells);
        for group in [
            &self.shape,
            &self.line.group,
            &self.text.group,
            &self.raw_group,
        ] {
            group.set_visible(cells);
        }
        match target {
            Target::Cells {
                style,
                raw,
                count,
                vertices,
                edges,
                fills,
                swimlane,
                effects,
                geometry,
                default_tool,
            } => {
                self.position.set_visible(geometry.is_some());
                self.size.set_visible(geometry.is_some());
                if let Some((r, pinned)) = geometry {
                    let rows = [&self.x, &self.y, &self.width, &self.height];
                    for (row, v) in rows.into_iter().zip([r.x, r.y, r.w, r.h]) {
                        row.set_value(v);
                        row.set_sensitive(!pinned);
                    }
                    self.size_was.set((r.w, r.h));
                    self.aspect.set_active(style.get("aspect") == Some("fixed"));
                    self.aspect.set_sensitive(!pinned);
                }
                for (clip, copy, paste) in &self.clips {
                    match clip {
                        Clip::Style => copy.set_visible(*count == 1),
                        _ => paste.set_visible(!geometry.is_some_and(|(_, pinned)| pinned)),
                    }
                }
                // A picture's fill and line are its background and border, as draw.io's panel
                // has them (`edit::set_styles` writes them so), with no gradient.
                let picture = style.shape() == "image";
                let (fill_key, stroke_key) = match picture {
                    true => ("imageBackground", "imageBorder"),
                    false => ("fillColor", "strokeColor"),
                };
                // An arrow's line is drawn from the same keys as a shape's outline, so every cell
                // shows the line's colour, width and dash; the fill only where a cell takes one,
                // the corners and the turn for shapes (an arrow's corners are its line style),
                // the route and the heads for arrows. draw.io offers a shape's perimeter only
                // with shapes alone selected, and a swimlane's divider with one among them.
                self.fill.row.set_visible(*fills);
                self.fill_opacity.set_visible(*fills);
                self.takes_gradient.set(*fills && !picture);
                self.rounded.set_visible(*vertices);
                self.divider.set_visible(*swimlane);
                self.rotation.set_visible(*vertices);
                self.line.fill(style, stroke_key, *vertices, *edges);
                self.text.fill(style, *vertices);
                self.fill.fill(style.color(fill_key));
                self.gradient.fill(style.color("gradientColor"));
                self.fill_opacity.set_value(style.num("fillOpacity", 100.0));
                self.rounded.set_active(style.flag("rounded", false));
                // The corner as the first cell's shape reads `arcSize`: a folder's a share of its
                // shorter side, 0.1 when the style gives none and stepped by hundredths, or with
                // `absoluteArcSize` its radius (`FolderShape`); any other's mxGraph's, 15 % of the
                // shorter side (`RECTANGLE_ROUNDING_FACTOR`) or 20 units (`LINE_ARCSIZE`).
                let fixed = style.flag("absoluteArcSize", false);
                let (arc, step) = match (style.shape() == "folder", fixed) {
                    (true, false) => (0.1, 0.01),
                    (true, true) => (0.1, 1.0),
                    (false, true) => (20.0, 1.0),
                    (false, false) => (15.0, 1.0),
                };
                let steps = self.arc_size.adjustment();
                steps.set_step_increment(step);
                steps.set_page_increment(10.0 * step);
                self.arc_size.set_value(style.num("arcSize", arc));
                self.effects.fill(style, effects);
                self.divider.set_active(style.flag("swimlaneLine", true));
                self.shadow.set_active(style.flag("shadow", false));
                // draw.io keeps any turn it was given; the row shows it within a half turn.
                let turn = (style.num("rotation", 0.0) + 180.0).rem_euclid(360.0) - 180.0;
                self.rotation.set_value(turn);
                let direction = style.get("gradientDirection").unwrap_or("south");
                self.direction
                    .set_selected(found(&DIRECTIONS.map(|(v, _)| v), Some(direction)));
                // A style string is one cell's: with several selected there is none to show.
                self.raw.set_visible(*count == 1);
                self.raw.set_text(if *count == 1 { raw } else { "" });
                self.default_style.set_visible(default_tool.is_some());
                if let Some((tool, is_default)) = default_tool {
                    let kind = match tool {
                        Tool::Rect => "rectangles",
                        Tool::Ellipse => "ellipses",
                        Tool::Text => "text boxes",
                        Tool::Connector => "connectors",
                        Tool::Select | Tool::Image => unreachable!(),
                    };
                    // Filled while the cell's style is the default, which a press then clears.
                    let (icon, action) = match is_default {
                        true => ("starred-symbolic", "Clear"),
                        false => ("non-starred-symbolic", "Set"),
                    };
                    self.default_style.set_icon_name(icon);
                    self.default_style
                        .set_tooltip_text(Some(&format!("{action} Default Style for {kind}")));
                }
            }
            Target::Page {
                name,
                size,
                background,
            } => {
                self.position.set_visible(false);
                self.size.set_visible(false);
                self.default_style.set_visible(false);
                self.page_name.set_text(name);
                self.background.fill(*background);
                self.page_width.set_value(size.0);
                self.page_height.set_value(size.1);
            }
        }
        self.sync_rows();
        self.filling.set(false);
    }

    /// Hold `f` as `slot`'s change until the rows have been still for a moment, in place of the
    /// one held for it: a burst of steps is one change.
    fn hold(self: &Rc<Self>, slot: &'static str, f: impl FnOnce() + 'static) {
        self.keep(slot, f);
        let me = Rc::downgrade(self);
        self.debounce.call(move || {
            if let Some(me) = me.upgrade() {
                me.flush();
            }
        });
    }

    /// Hold `f` as `slot`'s change, in place of the one held for it, until the next flush.
    fn keep(&self, slot: &'static str, f: impl FnOnce() + 'static) {
        let mut held = self.held.borrow_mut();
        held.retain(|(s, _)| *s != slot);
        held.push((slot, Box::new(f)));
    }

    /// Make the held changes now, each its own step: before any other change, a refill, or
    /// the selection or the page changing, so none is lost or lands on other cells.
    pub fn flush(&self) {
        self.debounce.cancel();
        let held = std::mem::take(&mut *self.held.borrow_mut());
        for (_, f) in held {
            f();
        }
    }

    /// Width or Height changed under Constrain Proportions: the other follows, scaled to a
    /// hundredth as draw.io's Arrange panel scales it (Format.js 3183-3230).
    fn keep_proportions(&self) {
        let size = (self.width.value(), self.height.value());
        let (w, h) = proportioned(self.size_was.get(), size);
        self.size_was.set((w, h));
        self.filling.set(true);
        self.width.set_value(w);
        self.height.set_value(h);
        self.filling.set(false);
    }

    /// The four geometry rows as they stand.
    fn frame(&self) -> Rect {
        let (x, y) = (self.x.value(), self.y.value());
        Rect::new(x, y, self.width.value(), self.height.value())
    }

    /// Have the canvas show what a row holds, nothing landing first ([`Change::Held`]).
    fn held(&self, placed: Placed) {
        if let Some(f) = self.on_change.borrow().as_ref() {
            f(Change::Held(placed));
        }
    }

    /// Show a drag under way in the Position and Size rows, or in Rotation, writing nothing back.
    fn show_drag(&self, placed: Placed) {
        self.filling.set(true);
        match placed {
            Placed::Frame(r) => {
                let rows = [&self.x, &self.y, &self.width, &self.height];
                for (row, v) in rows.into_iter().zip([r.x, r.y, r.w, r.h]) {
                    row.set_value(v);
                }
            }
            Placed::Turn(degrees) => self.rotation.set_value(degrees),
            Placed::Ends(ends) => self.ends.show(ends),
        }
        self.filling.set(false);
    }

    /// Show the page's layers, the topmost first, `current` marked.
    pub fn fill_layers(&self, layers: &[super::layers::Layer], current: Option<&str>) {
        self.layers.fill(layers, current);
    }

    /// The Layers group, for a drill.
    #[cfg(feature = "bench")]
    pub fn layers(&self) -> &super::layers::Layers {
        &self.layers
    }

    /// A gradient runs from the fill, so it shows only with one, and its direction only while
    /// there is a gradient; the corners' size and Fixed Corner Size show only while they are
    /// rounded.
    fn sync_rows(&self) {
        let gradient = self.takes_gradient.get() && self.fill.switch.is_active();
        self.gradient.row.set_visible(gradient);
        self.direction
            .set_visible(gradient && self.gradient.switch.is_active());
        let rounded = self.rounded.is_visible() && self.rounded.is_active();
        self.arc_size.set_visible(rounded);
        self.effects.sync(rounded);
    }
}

/// Width and height `size` under Constrain Proportions, from `was` before: the side left alone
/// follows the one changed, scaled to a hundredth as draw.io's Arrange panel scales it (Format.js
/// 3183-3230).
fn proportioned(was: (f64, f64), size: (f64, f64)) -> (f64, f64) {
    let ((w0, h0), (w, h)) = (was, size);
    let hundredth = |v: f64| (v * 100.0).round() / 100.0;
    match (w != w0, h != h0) {
        (true, false) if w0 > 0.0 => (w, hundredth(h0 * w / w0)),
        (false, true) if h0 > 0.0 => (hundredth(w0 * h / h0), h),
        _ => (w, h),
    }
}

/// The number being typed into `row`, within its range, while the keyboard is in it: its own
/// value where the text is that value as the row rounds it, `None` for text that is no number.
fn typed(row: &adw::SpinRow) -> Option<f64> {
    if !row.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN) {
        return None;
    }
    let typed = row.text().trim().parse::<f64>().ok()?;
    let value = row.value();
    if (typed - value).abs() <= 0.5 / 10f64.powi(row.digits() as i32) {
        return Some(value);
    }
    let range = row.adjustment();
    typed
        .is_finite()
        .then(|| typed.clamp(range.lower(), range.upper()))
}

/// Style pairs as a [`Change`] carries them.
fn owned(pairs: &[(&'static str, Option<&str>)]) -> Vec<(&'static str, Option<String>)> {
    pairs
        .iter()
        .map(|(k, v)| (*k, v.map(str::to_string)))
        .collect()
}

/// A combo row's position for entry `i`, none when there is no entry.
fn listed(i: Option<usize>) -> u32 {
    i.map_or(gtk::INVALID_LIST_POSITION, |i| i as u32)
}

/// A picked colour as a style value.
fn hex(c: gtk::gdk::RGBA) -> String {
    let [r, g, b] = crate::theme::rgb_of(c);
    Color::rgb(r, g, b).hex()
}

/// What sets `key` on every selected cell, as one step.
fn style(send: &Rc<dyn Fn(Change)>, key: &'static str) -> impl Fn(Option<String>) + 'static {
    let send = send.clone();
    move |value: Option<String>| send(Change::Style(vec![(key, value)]))
}

/// Set `key` to what colour row `row` says whenever either half changes.
fn colour(
    row: &Rc<ColourRow>,
    filling: &Rc<Cell<bool>>,
    send: &Rc<dyn Fn(Change)>,
    key: &'static str,
) {
    let set = style(send, key);
    row.connect(filling, move |value| set(Some(value)));
}

/// Set `key` to the value of the entry picked in `row`, from `values`.
fn pick(
    row: &adw::ComboRow,
    send: &Rc<dyn Fn(Change)>,
    key: &'static str,
    values: Vec<&'static str>,
) {
    let set = style(send, key);
    row.connect_selected_notify(move |row| {
        if let Some(v) = values.get(row.selected() as usize) {
            set(Some(v.to_string()));
        }
    });
}

/// A pick that writes several keys, as one step.
fn choose(
    row: &adw::ComboRow,
    send: &Rc<dyn Fn(Change)>,
    change: Box<dyn Fn(usize) -> Option<Change>>,
) {
    let send = send.clone();
    row.connect_selected_notify(move |row| {
        if let Some(change) = change(row.selected() as usize) {
            send(change);
        }
    });
}

/// A combo row's position for value `v` among `values`, none when it is not one of them.
fn found(values: &[&str], v: Option<&str>) -> u32 {
    listed(v.and_then(|v| values.iter().position(|x| *x == v)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers_are_written_as_draw_io_writes_them() {
        assert_eq!(super::number(12.0), "12");
        assert_eq!(super::number(1.5), "1.5");
        assert_eq!(super::number(0.1 + 0.01 * 5.0), "0.15");
        let c = gtk::gdk::RGBA::new(0.0, 150.0 / 255.0, 130.0 / 255.0, 1.0);
        assert_eq!(super::hex(c), "#009682");
    }

    #[test]
    fn constrained_proportions_follow_the_side_changed() {
        assert_eq!(
            super::proportioned((120.0, 60.0), (240.0, 60.0)),
            (240.0, 120.0)
        );
        assert_eq!(
            super::proportioned((120.0, 60.0), (120.0, 20.0)),
            (40.0, 20.0)
        );
        assert_eq!(super::proportioned((0.0, 60.0), (30.0, 60.0)), (30.0, 60.0));
    }
}
