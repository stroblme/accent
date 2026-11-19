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
        /// The selected cells, which each row reads over those it applies to.
        styles: Styles,
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

/// One selected cell as the rows read it.
pub struct Seen {
    pub style: Resolved,
    pub vertex: bool,
    pub edge: bool,
}

impl Seen {
    pub fn of(cell: &accent_drawio::Cell) -> Seen {
        Seen {
            style: cell.style.resolve(cell.edge),
            vertex: cell.vertex,
            edge: cell.edge,
        }
    }

    /// Whether it is one of `cells`.
    pub fn is(&self, cells: Cells) -> bool {
        match cells {
            Cells::All => true,
            Cells::Shapes => self.vertex,
            Cells::Arrows => self.edge,
            Cells::Fills => self.vertex || (self.edge && self.style.shape() == "flexArrow"),
        }
    }
}

/// Which of the selected cells a row reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cells {
    All,
    /// Shapes, whose corners, turn, perimeter and label placement an arrow reads otherwise or
    /// not at all.
    Shapes,
    /// Arrows, as draw.io's arrow rows set them (`ss.edges`).
    Arrows,
    /// What takes a fill: a shape, or a flex arrow (`Graph.isFillState`).
    Fills,
}

/// What a row shows over the cells it reads: the first one's value, and whether another one's
/// differs from it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Read<T> {
    pub value: T,
    pub mixed: bool,
}

impl<T: PartialEq> Read<T> {
    /// The first of `values`, mixed where another differs, or `None` for none.
    pub fn of(mut values: impl Iterator<Item = T>) -> Option<Read<T>> {
        let value = values.next()?;
        let mixed = values.any(|v| v != value);
        Some(Read { value, mixed })
    }
}

/// The selected cells as the rows read them, in the order selected.
#[derive(Default)]
pub struct Styles(pub Vec<Seen>);

impl Styles {
    pub fn of(cells: &[&accent_drawio::Cell]) -> Styles {
        Styles(cells.iter().map(|c| Seen::of(c)).collect())
    }

    /// What `value` makes of each of `cells`.
    pub fn read<T: PartialEq + Default>(
        &self,
        cells: Cells,
        value: impl Fn(&Resolved) -> T,
    ) -> Read<T> {
        self.read_where(|c| c.is(cells).then(|| value(&c.style)))
    }

    /// What `value` makes of each cell it reads, a row's own few: a swimlane's divider, a flex
    /// arrow's widths.
    pub fn read_where<T: PartialEq + Default>(
        &self,
        value: impl Fn(&Seen) -> Option<T>,
    ) -> Read<T> {
        Read::of(self.0.iter().filter_map(value)).unwrap_or_default()
    }
}

/// The key a picture's fill or line is written under, its background and its border, as draw.io's
/// panel has them (`edit::set_styles` writes them so).
fn picture_key(style: &Resolved, key: &'static str) -> &'static str {
    match (style.shape() == "image", key) {
        (true, "fillColor") => "imageBackground",
        (true, "strokeColor") => "imageBorder",
        _ => key,
    }
}

/// One change the pane asks for.
pub enum Change {
    /// Style keys on the selected cells of a kind, as one step.
    Style(Cells, Vec<(&'static str, Option<String>)>),
    /// draw.io's Bold, Italic or Underline, a bit of `fontStyle`, on or off on every selected
    /// cell, its other bits as each cell has them.
    FontStyle(u32, bool),
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
    /// The button in `button`'s place while the cells' colours differ: a hatched swatch whose
    /// dialog opens at no colour, since GTK's chooser keeps any colour it opens at that its
    /// palette lacks among the custom colours every GTK app outside a sandbox shares
    /// (`gtkcolorchooserwidget.c`), and whatever is picked there reaches every cell.
    mixed: gtk::Button,
    /// What the switch turns on while there is no colour: draw.io's `default`, ink or paper.
    blank: Color,
}

impl ColourRow {
    fn new(title: &str, blank: Color) -> ColourRow {
        let button = gtk::ColorDialogButton::new(Some(
            gtk::ColorDialog::builder().with_alpha(false).build(),
        ));
        button.set_valign(gtk::Align::Center);
        // As large as the colour button's swatch, which GTK sizes to the word Black in the
        // button's font (`gtkcolordialogbutton.c`).
        let (_, black) = button.create_pango_layout(Some("Black")).pixel_extents();
        let swatch = gtk::Box::builder()
            .css_classes(["accent-swatch"])
            .width_request(black.width())
            .height_request(black.height())
            .build();
        let mixed = gtk::Button::builder()
            .child(&swatch)
            .css_classes(["color"])
            .tooltip_text("Mixed")
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        mixed.update_property(&[
            gtk::accessible::Property::Label(title),
            gtk::accessible::Property::Description("Mixed"),
            gtk::accessible::Property::HasPopup(true),
        ]);
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        let row = adw::ActionRow::builder().title(title).build();
        row.add_suffix(&button);
        row.add_suffix(&mixed);
        row.add_suffix(&switch);
        ColourRow {
            row,
            switch,
            button,
            mixed,
            blank,
        }
    }

    /// Show `shade`: its colour, or where the cells differ a hatched swatch with no colour and
    /// the tooltip Mixed, as GTK's check button shows an inconsistent state, the switch on since
    /// some cell has a colour (user decision 2026-11-18).
    fn fill(&self, shade: Read<Option<Color>>) {
        let on = shade.value.is_some() || shade.mixed;
        self.switch.set_active(on);
        self.button
            .set_rgba(&super::paint::rgba(shade.value.unwrap_or(self.blank)));
        self.button.set_visible(on && !shade.mixed);
        // The keyboard stays on the row as a colour picked from Mixed takes the swatch's place.
        if self.mixed.is_focus() && !shade.mixed {
            self.button.grab_focus();
        }
        self.mixed.set_visible(on && shade.mixed);
    }

    /// What the row says now, as a style value: the colour, or `none`.
    fn value(&self) -> String {
        match self.switch.is_active() {
            true => hex(self.button.rgba()),
            false => "none".to_string(),
        }
    }

    /// Call `f` with the new value whenever either half changes, or a colour is picked from
    /// Mixed.
    fn connect(self: &Rc<Self>, filling: &Rc<Cell<bool>>, f: impl Fn(String) + 'static) {
        let f = Rc::new(f);
        let (me, guard, g) = (Rc::downgrade(self), filling.clone(), f.clone());
        self.switch.connect_active_notify(move |switch| {
            if let Some(me) = me.upgrade() {
                me.button.set_visible(switch.is_active());
                me.mixed.set_visible(false);
                if !guard.get() {
                    g(me.value());
                }
            }
        });
        let (dialog, g) = (self.button.dialog(), f.clone());
        self.mixed.connect_clicked(move |button| {
            let (Some(dialog), g) = (dialog.clone(), g.clone()) else {
                return;
            };
            let window = button.root().and_downcast::<gtk::Window>();
            glib::spawn_future_local(async move {
                // A dialog closed without a colour leaves the cells as they are.
                if let Ok(rgba) = dialog.choose_rgba_future(window.as_ref(), None).await {
                    g(hex(rgba));
                }
            });
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

/// A row with a check button where a switch would do, since only a check button shows GTK's
/// mixed state, which the HIG gives a setting of several items (Checkboxes; user decision
/// 2026-11-19).
pub(super) struct Check {
    pub(super) row: adw::ActionRow,
    check: gtk::CheckButton,
}

impl Check {
    pub(super) fn new(title: &str) -> Check {
        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .build();
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable_widget(&check)
            .build();
        row.add_suffix(&check);
        Check { row, check }
    }

    /// Show `read`, where the cells differ GTK's mixed state, off underneath it so that a click
    /// turns it on for them all.
    pub(super) fn fill(&self, read: Read<bool>) {
        self.check.set_active(read.value && !read.mixed);
        self.check.set_inconsistent(read.mixed);
    }

    /// Whether it is on, or mixed and so on for some cell.
    pub(super) fn is_on(&self) -> bool {
        self.check.is_active() || self.check.is_inconsistent()
    }

    /// Call `f` with whether it is on now each time it is flipped, from mixed too.
    pub(super) fn connect(&self, f: impl Fn(bool) + 'static) {
        self.check.connect_active_notify(move |check| {
            check.set_inconsistent(false);
            f(check.is_active())
        });
    }

    /// What says it flipped, for the rows that show with it.
    pub(super) fn notifier(&self) -> &glib::Object {
        self.check.upcast_ref()
    }
}

/// Tell a screen reader and the pointer that `widget` stands for cells that differ, or no
/// longer.
fn say_mixed(widget: &impl IsA<gtk::Widget>, mixed: bool) {
    let widget = widget.upcast_ref::<gtk::Widget>();
    widget.set_tooltip_text(mixed.then_some("Mixed"));
    match mixed {
        true => widget.update_property(&[gtk::accessible::Property::Description("Mixed")]),
        false => widget.reset_property(gtk::AccessibleProperty::Description),
    }
}

/// The first part of `root` that is a `T`.
fn part<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>) -> Option<T> {
    let mut todo = vec![root.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = todo.pop() {
        if let Ok(found) = widget.clone().downcast::<T>() {
            return Some(found);
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
    None
}

/// Show `read` in a spin row: its value, or where the cells differ an empty field with a dash
/// in it, which [`spin`] keeps empty until a number is stepped to or typed.
fn show(row: &adw::SpinRow, read: Read<f64>) {
    set_mixed(row, false);
    put(row, read.value);
    if read.mixed {
        set_mixed(row, true);
        row.set_text("");
    }
}

/// Give a spin row `value` through its spin button, which writes the number into the field even
/// where it holds that value already, where `AdwSpinRow::set_value` would leave the field empty.
fn put(row: &adw::SpinRow, value: f64) {
    match part::<gtk::SpinButton>(row) {
        Some(spin) => spin.set_value(value),
        None => row.set_value(value),
    }
}

/// Mark a spin row as standing for cells that differ, or no longer: GTK's inconsistent state on
/// the row, a dash where its empty field would show nothing, and Mixed for a screen reader.
fn set_mixed(row: &adw::SpinRow, mixed: bool) {
    match mixed {
        true => row.set_state_flags(gtk::StateFlags::INCONSISTENT, false),
        false => row.unset_state_flags(gtk::StateFlags::INCONSISTENT),
    }
    if let Some(text) = part::<gtk::Text>(row) {
        text.set_placeholder_text(mixed.then_some("—"));
    }
    if let Some(spin) = part::<gtk::SpinButton>(row) {
        say_mixed(&spin, mixed);
    }
}

/// Whether a spin row stands for cells that differ ([`show`]).
fn mixed(row: &adw::SpinRow) -> bool {
    row.state_flags().contains(gtk::StateFlags::INCONSISTENT)
}

/// The entry a combo row ends with while the cells differ ([`pick_shown`]).
const MIXED: &str = "Mixed";

/// Show `read` in a combo row: its entry, or where the cells differ an entry Mixed at the end of
/// its list, as Nautilus's menus show `---`, since a combo row always has an entry picked. Mixed
/// picked writes nothing, being none of the values the row's entries stand for.
fn pick_shown(row: &adw::ComboRow, read: Read<Option<usize>>) {
    let Some(list) = row.model().and_downcast::<gtk::StringList>() else {
        return;
    };
    let last = list.n_items().saturating_sub(1);
    let marked = list.string(last).as_deref() == Some(MIXED);
    match (read.mixed, marked) {
        (true, false) => list.append(MIXED),
        (false, true) => list.remove(last),
        _ => {}
    }
    row.set_selected(match read.mixed {
        true => list.n_items() - 1,
        false => listed(read.value),
    });
    say_mixed(row, read.mixed);
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
    rounded: Check,
    arc_size: adw::SpinRow,
    effects: effects::Effects,
    /// A swimlane's divider under its title (`swimlaneLine`).
    divider: Check,
    shadow: Check,
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

/// A spin row, which while the cells differ ([`show`]) stays empty: the keyboard leaving it or a
/// step would otherwise read the emptiness as 0 and write that to them all. A step or a number
/// typed ends it, the number landing on every cell, even where it is the first one's own value,
/// which the spin button takes for no change.
fn spin(title: &str, range: (f64, f64, f64)) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(range.0, range.1, range.2);
    row.set_title(title);
    row.connect_output(|row| {
        let empty = mixed(row);
        if empty {
            row.set_text("");
        }
        empty
    });
    row.connect_input(|row| {
        if !mixed(row) {
            return None;
        }
        let Ok(typed) = row.text().trim().parse::<f64>() else {
            return Some(Ok(row.value()));
        };
        let range = row.adjustment();
        let typed = typed.clamp(range.lower(), range.upper());
        set_mixed(row, false);
        if typed == row.value() {
            row.notify("value");
        }
        Some(Ok(typed))
    });
    row.connect_value_notify(|row| {
        if mixed(row) {
            set_mixed(row, false);
            put(row, row.value());
        }
    });
    row
}

/// A row of the geometry or a corner's size: whole units a step, fractions kept, shown as the
/// file writes them to a hundredth of a step, so a folder's share stepped by hundredths shows
/// four places and the text the row reads back as the keyboard leaves it is its value.
fn coordinate(title: &str, low: f64) -> adw::SpinRow {
    let row = spin(title, (low, 1e6, 1.0));
    row.set_digits(2);
    row.connect_output(|row| {
        let places = 100.0 / row.adjustment().step_increment();
        row.set_text(&number((row.value() * places).round() / places));
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
        let rounded = Check::new("Rounded");
        let arc_size = coordinate("Arc Size", 0.0);
        let effects = effects::Effects::new();
        let divider = Check::new("Divider");
        let shadow = Check::new("Shadow");
        let rotation = spin("Rotation", (-180.0, 180.0, 1.0));
        shape.add(&fill.row);
        shape.add(&gradient.row);
        shape.add(&direction);
        shape.add(&fill_opacity);
        shape.add(&rounded.row);
        shape.add(&arc_size);
        effects.add_fixed(&shape);
        shape.add(&divider.row);
        shape.add(&effects.sketch.row);
        shape.add(&shadow.row);
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
        colour(&self.fill, &self.filling, &send, Cells::Fills, "fillColor");
        colour(
            &self.gradient,
            &self.filling,
            &send,
            Cells::Fills,
            "gradientColor",
        );
        let toggles: [&glib::Object; 3] = [
            self.fill.switch.upcast_ref(),
            self.gradient.switch.upcast_ref(),
            self.rounded.notifier(),
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

        let check = |row: &Check, cells: Cells, key: &'static str, off: Option<&'static str>| {
            let set = style(&send, cells, key);
            row.connect(move |on| {
                set(match on {
                    true => Some("1".to_string()),
                    false => off.map(str::to_string),
                })
            });
        };
        check(&self.rounded, Cells::Shapes, "rounded", Some("0"));
        check(&self.divider, Cells::Shapes, "swimlaneLine", Some("0"));
        check(&self.shadow, Cells::All, "shadow", None);
        self.spins(
            &send,
            &[
                (&self.fill_opacity, "fillOpacity", Some(100.0), Cells::Fills),
                (&self.arc_size, "arcSize", None, Cells::Shapes),
                (&self.rotation, "rotation", Some(0.0), Cells::Shapes),
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
                send(Change::Style(
                    Cells::Shapes,
                    vec![("rotation", rotation(degrees))],
                ))
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
        let set = style(&send, Cells::Shapes, "aspect");
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
            Cells::Fills,
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

    /// Send each spin row's value as its key's once its burst settles, onto the cells the row
    /// says: spin rows step once per arrow press, and a burst of them is one change. A default is
    /// where the key is left out, as draw.io leaves out an opacity of 100; a key a named style can
    /// give (`spacingLeft`) or whose default depends on another (`arcSize`) is always written.
    fn spins(
        self: &Rc<Self>,
        send: &Rc<dyn Fn(Change)>,
        rows: &[(&adw::SpinRow, &'static str, Option<f64>, Cells)],
    ) {
        for &(row, key, default, cells) in rows {
            let (me, send) = (Rc::downgrade(self), send.clone());
            row.connect_value_notify(move |row| {
                let Some(me) = me.upgrade() else { return };
                if me.filling.get() {
                    return;
                }
                let (value, send) = (row.value(), send.clone());
                me.hold(key, move || {
                    let pairs = vec![(key, (Some(value) != default).then(|| number(value)))];
                    send(Change::Style(cells, pairs))
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
                styles,
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
                // An arrow's line is drawn from the same keys as a shape's outline, so every cell
                // shows the line's colour, width and dash; the fill only where a cell takes one,
                // the corners and the turn for shapes (an arrow's corners are its line style),
                // the route and the heads for arrows. draw.io offers a shape's perimeter only
                // with shapes alone selected, and a swimlane's divider with one among them. Each
                // row reads the cells it writes, and shows them mixed where they differ (user
                // decision 2026-11-19).
                self.fill.row.set_visible(*fills);
                self.fill_opacity.set_visible(*fills);
                // A picture's fill and line are its background and border, with no gradient.
                let gradients = |c: &Seen| c.is(Cells::Fills) && c.style.shape() != "image";
                self.takes_gradient.set(styles.0.iter().any(gradients));
                self.rounded.row.set_visible(*vertices);
                self.divider.row.set_visible(*swimlane);
                self.rotation.set_visible(*vertices);
                self.line.fill(style, styles, *vertices, *edges);
                self.text.fill(styles, *vertices);
                self.fill
                    .fill(styles.read(Cells::Fills, |s| s.color(picture_key(s, "fillColor"))));
                self.gradient.fill(
                    styles.read_where(|c| gradients(c).then(|| c.style.color("gradientColor"))),
                );
                show(
                    &self.fill_opacity,
                    styles.read(Cells::Fills, |s| s.num("fillOpacity", 100.0)),
                );
                self.rounded
                    .fill(styles.read(Cells::Shapes, |s| s.flag("rounded", false)));
                // The digits take the hundredth of a step the row shows (`coordinate`), which the
                // spin button would otherwise refuse as text; the first cell's shape decides.
                let (_, step, digits) = arc_of(style);
                let steps = self.arc_size.adjustment();
                steps.set_step_increment(step);
                steps.set_page_increment(10.0 * step);
                self.arc_size.set_digits(digits);
                show(
                    &self.arc_size,
                    styles.read(Cells::Shapes, |s| s.num("arcSize", arc_of(s).0)),
                );
                self.effects.fill(effects);
                self.divider.fill(styles.read_where(|c| {
                    let lane = c.vertex && c.style.shape() == "swimlane";
                    lane.then(|| c.style.flag("swimlaneLine", true))
                }));
                self.shadow
                    .fill(styles.read(Cells::All, |s| s.flag("shadow", false)));
                // draw.io keeps any turn it was given; the row shows it within a half turn.
                show(
                    &self.rotation,
                    styles.read(Cells::Shapes, |s| {
                        (s.num("rotation", 0.0) + 180.0).rem_euclid(360.0) - 180.0
                    }),
                );
                let directions = DIRECTIONS.map(|(v, _)| v);
                pick_shown(
                    &self.direction,
                    styles.read_where(|c| {
                        let direction = c.style.get("gradientDirection").unwrap_or("south");
                        let at = directions.iter().position(|d| *d == direction);
                        gradients(c).then_some(at)
                    }),
                );
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
                self.background.fill(Read {
                    value: *background,
                    mixed: false,
                });
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
        let rounded = self.rounded.row.is_visible() && self.rounded.is_on();
        self.arc_size.set_visible(rounded);
        self.effects.sync(rounded);
    }
}

/// The `arcSize` a shape is drawn with where its style gives none, and the step and the digits
/// its row takes: a folder's is a share of its shorter side, 0.1, stepped by hundredths, or with
/// `absoluteArcSize` its radius (`FolderShape`); any other's mxGraph's, 15 % of the shorter side
/// (`RECTANGLE_ROUNDING_FACTOR`) or 20 units (`LINE_ARCSIZE`).
fn arc_of(style: &Resolved) -> (f64, f64, u32) {
    match (
        style.shape() == "folder",
        style.flag("absoluteArcSize", false),
    ) {
        (true, false) => (0.1, 0.01, 4),
        (true, true) => (0.1, 1.0, 2),
        (false, true) => (20.0, 1.0, 2),
        (false, false) => (15.0, 1.0, 2),
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

/// What sets `key` on the selected `cells`, as one step.
fn style(
    send: &Rc<dyn Fn(Change)>,
    cells: Cells,
    key: &'static str,
) -> impl Fn(Option<String>) + 'static {
    let send = send.clone();
    move |value: Option<String>| send(Change::Style(cells, vec![(key, value)]))
}

/// Set `key` on `cells` to what colour row `row` says whenever either half changes.
fn colour(
    row: &Rc<ColourRow>,
    filling: &Rc<Cell<bool>>,
    send: &Rc<dyn Fn(Change)>,
    cells: Cells,
    key: &'static str,
) {
    let set = style(send, cells, key);
    row.connect(filling, move |value| set(Some(value)));
}

/// Set `key` on `cells` to the value of the entry picked in `row`, from `values`.
fn pick(
    row: &adw::ComboRow,
    send: &Rc<dyn Fn(Change)>,
    cells: Cells,
    key: &'static str,
    values: Vec<&'static str>,
) {
    let set = style(send, cells, key);
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
