//! The Properties pane: how the selected cells look, or the page when nothing is selected.
//!
//! Every row writes one change through the tab, which is one undo step; spin rows wait for a
//! burst of steps to settle first. Filling the rows from the model sets `filling`, so the
//! notifications that causes are not taken for edits.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use accent_drawio::{Color, Resolved};
use adw::prelude::*;

use crate::widgets::Debounce;

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
    /// The whole style string of the one selected cell.
    Raw(String),
    PageAttr(&'static str, Option<String>),
    PageName(String),
}

/// Arrow heads a menu offers, by style value and label.
const HEADS: [(&str, &str); 6] = [
    ("none", "None"),
    ("classic", "Classic"),
    ("block", "Block"),
    ("open", "Open"),
    ("oval", "Oval"),
    ("diamond", "Diamond"),
];
const ALIGN: [(&str, &str); 3] = [("left", "Left"), ("center", "Centre"), ("right", "Right")];
const VALIGN: [(&str, &str); 3] = [("top", "Top"), ("middle", "Middle"), ("bottom", "Bottom")];
const ROUTES: [&str; 3] = ["Straight", "Orthogonal", "Curved"];
/// draw.io's gradient directions (Format panel), south when a style names none.
const DIRECTIONS: [(&str, &str); 5] = [
    ("north", "North"),
    ("east", "East"),
    ("south", "South"),
    ("west", "West"),
    ("radial", "Radial"),
];

type OnChange = Rc<RefCell<Option<Box<dyn Fn(Change)>>>>;

/// A colour a cell may also have none of: a switch for whether it has one, and the colour.
struct ColourRow {
    row: adw::ActionRow,
    switch: gtk::Switch,
    button: gtk::ColorDialogButton,
}

impl ColourRow {
    fn new(title: &str) -> ColourRow {
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
        }
    }

    fn fill(&self, colour: Option<Color>) {
        self.switch.set_active(colour.is_some());
        self.button.set_sensitive(colour.is_some());
        if let Some(c) = colour {
            self.button.set_rgba(&super::paint::rgba(c));
        }
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
                me.button.set_sensitive(switch.is_active());
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
    shape: adw::PreferencesGroup,
    text: adw::PreferencesGroup,
    line: adw::PreferencesGroup,
    raw_group: adw::PreferencesGroup,
    page_group: adw::PreferencesGroup,
    fill: Rc<ColourRow>,
    gradient: Rc<ColourRow>,
    direction: adw::ComboRow,
    stroke: Rc<ColourRow>,
    stroke_width: adw::SpinRow,
    dashed: adw::SwitchRow,
    rounded: adw::SwitchRow,
    shadow: adw::SwitchRow,
    opacity: adw::SpinRow,
    rotation: adw::SpinRow,
    font_size: adw::SpinRow,
    font_colour: Rc<ColourRow>,
    bold: gtk::ToggleButton,
    italic: gtk::ToggleButton,
    underline: gtk::ToggleButton,
    align: adw::ComboRow,
    valign: adw::ComboRow,
    route: adw::ComboRow,
    start: adw::ComboRow,
    end: adw::ComboRow,
    raw: adw::EntryRow,
    page_name: adw::EntryRow,
    background: Rc<ColourRow>,
    page_width: adw::SpinRow,
    page_height: adw::SpinRow,
    /// The font style bits the cell had when the pane was filled, which a toggle flips one of.
    font_bits: Cell<u32>,
    debounce: Debounce,
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

/// The `rotation` a turn of `degrees` writes: none for no turn, as a shape draw.io never turned
/// has none.
pub(super) fn rotation(degrees: f64) -> Option<String> {
    (degrees != 0.0).then(|| number(degrees))
}

/// A number as a style value: `12`, not `12.0`.
pub(super) fn number(v: f64) -> String {
    match v.fract() == 0.0 {
        true => format!("{}", v as i64),
        false => format!("{v}"),
    }
}

impl Props {
    pub fn new() -> Rc<Props> {
        let page = adw::PreferencesPage::new();
        let group = |title: &str| {
            let g = adw::PreferencesGroup::builder().title(title).build();
            page.add(&g);
            g
        };
        let (shape, text, line, raw_group, page_group) = (
            group("Shape"),
            group("Text"),
            group("Line"),
            group("Style"),
            group("Page"),
        );

        let fill = Rc::new(ColourRow::new("Fill"));
        let gradient = Rc::new(ColourRow::new("Gradient"));
        let direction = combo("Gradient Direction", &DIRECTIONS.map(|(_, l)| l));
        let stroke = Rc::new(ColourRow::new("Line Colour"));
        let stroke_width = spin("Line Width", (0.0, 20.0, 0.5));
        stroke_width.set_digits(1);
        let dashed = adw::SwitchRow::builder().title("Dashed").build();
        let rounded = adw::SwitchRow::builder().title("Rounded").build();
        let shadow = adw::SwitchRow::builder().title("Shadow").build();
        let opacity = spin("Opacity", (0.0, 100.0, 5.0));
        let rotation = spin("Rotation", (-180.0, 180.0, 1.0));
        shape.add(&fill.row);
        shape.add(&gradient.row);
        shape.add(&direction);
        shape.add(&stroke.row);
        shape.add(&stroke_width);
        shape.add(&dashed);
        shape.add(&rounded);
        shape.add(&shadow);
        shape.add(&opacity);
        shape.add(&rotation);

        let font_size = spin("Font Size", (6.0, 96.0, 1.0));
        let font_colour = Rc::new(ColourRow::new("Font Colour"));
        let toggle = |icon: &str, tooltip: &str| {
            gtk::ToggleButton::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .valign(gtk::Align::Center)
                .build()
        };
        let bold = toggle("format-text-bold-symbolic", "Bold");
        let italic = toggle("format-text-italic-symbolic", "Italic");
        let underline = toggle("format-text-underline-symbolic", "Underline");
        let styles = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        styles.add_css_class("linked");
        for b in [&bold, &italic, &underline] {
            styles.append(b);
        }
        let style_row = adw::ActionRow::builder().title("Style").build();
        style_row.add_suffix(&styles);
        let align = combo("Alignment", &ALIGN.map(|(_, l)| l));
        let valign = combo("Vertical Alignment", &VALIGN.map(|(_, l)| l));
        text.add(&font_size);
        text.add(&font_colour.row);
        text.add(&style_row);
        text.add(&align);
        text.add(&valign);

        let route = combo("Route", &ROUTES);
        let start = combo("Start", &HEADS.map(|(_, l)| l));
        let end = combo("End", &HEADS.map(|(_, l)| l));
        line.add(&route);
        line.add(&start);
        line.add(&end);

        let raw = adw::EntryRow::builder()
            .title("Style")
            .show_apply_button(true)
            .build();
        raw.add_css_class("monospace");
        raw_group.add(&raw);

        let page_name = adw::EntryRow::builder()
            .title("Name")
            .show_apply_button(true)
            .build();
        let background = Rc::new(ColourRow::new("Background"));
        let page_width = spin("Width", (100.0, 10000.0, 10.0));
        let page_height = spin("Height", (100.0, 10000.0, 10.0));
        page_group.add(&page_name);
        page_group.add(&background.row);
        page_group.add(&page_width);
        page_group.add(&page_height);

        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();
        let props = Rc::new(Props {
            root,
            filling: Rc::new(Cell::new(false)),
            on_change: Rc::new(RefCell::new(None)),
            shape,
            text,
            line,
            raw_group,
            page_group,
            fill,
            gradient,
            direction,
            stroke,
            stroke_width,
            dashed,
            rounded,
            shadow,
            opacity,
            rotation,
            font_size,
            font_colour,
            bold,
            italic,
            underline,
            align,
            valign,
            route,
            start,
            end,
            raw,
            page_name,
            background,
            page_width,
            page_height,
            font_bits: Cell::new(0),
            debounce: Debounce::new(Duration::from_millis(300)),
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
        let send = {
            let (on, filling) = (self.on_change.clone(), self.filling.clone());
            Rc::new(move |change: Change| {
                if filling.get() {
                    return;
                }
                if let Some(f) = on.borrow().as_ref() {
                    f(change);
                }
            })
        };
        let style = |key: &'static str| {
            let send = send.clone();
            move |value: Option<String>| send(Change::Style(vec![(key, value)]))
        };
        let colour = |row: &Rc<ColourRow>, key: &'static str| {
            let set = style(key);
            row.connect(&self.filling, move |value| set(Some(value)));
        };
        colour(&self.fill, "fillColor");
        colour(&self.gradient, "gradientColor");
        colour(&self.stroke, "strokeColor");
        for switch in [&self.fill.switch, &self.gradient.switch] {
            let me = Rc::downgrade(self);
            switch.connect_active_notify(move |_| {
                if let Some(me) = me.upgrade() {
                    me.sync_gradient();
                }
            });
        }
        colour(&self.font_colour, "fontColor");
        let page_send = send.clone();
        self.background.connect(&self.filling, move |value| {
            let value = (value != "none").then_some(value);
            page_send(Change::PageAttr("background", value));
        });

        let switch = |row: &adw::SwitchRow, key: &'static str, off: Option<&'static str>| {
            let set = style(key);
            row.connect_active_notify(move |row| {
                set(match row.is_active() {
                    true => Some("1".to_string()),
                    false => off.map(str::to_string),
                })
            });
        };
        switch(&self.dashed, "dashed", None);
        switch(&self.rounded, "rounded", Some("0"));
        switch(&self.shadow, "shadow", None);

        // Spin rows step once per arrow press; a burst of them is one change.
        let spins: [(&adw::SpinRow, &'static str, Option<f64>); 4] = [
            (&self.stroke_width, "strokeWidth", None),
            (&self.opacity, "opacity", Some(100.0)),
            (&self.rotation, "rotation", Some(0.0)),
            (&self.font_size, "fontSize", None),
        ];
        for (row, key, default) in spins {
            let (me, set) = (Rc::downgrade(self), style(key));
            let set = Rc::new(set);
            row.connect_value_notify(move |row| {
                let Some(me) = me.upgrade() else { return };
                if me.filling.get() {
                    return;
                }
                let (value, set) = (row.value(), set.clone());
                me.debounce.call(move || {
                    set((Some(value) != default).then(|| number(value)));
                });
            });
        }
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
                me.debounce
                    .call(move || send(Change::PageAttr(key, Some(number(value)))));
            });
        }

        for (button, bit) in [(&self.bold, 1), (&self.italic, 2), (&self.underline, 4)] {
            let (me, set) = (Rc::downgrade(self), style("fontStyle"));
            button.connect_toggled(move |button| {
                let Some(me) = me.upgrade() else { return };
                let bits = match button.is_active() {
                    true => me.font_bits.get() | bit,
                    false => me.font_bits.get() & !bit,
                };
                me.font_bits.set(bits);
                set(Some(bits.to_string()));
            });
        }

        let pick = |row: &adw::ComboRow, key: &'static str, values: Vec<&'static str>| {
            let set = style(key);
            row.connect_selected_notify(move |row| {
                if let Some(v) = values.get(row.selected() as usize) {
                    set(Some(v.to_string()));
                }
            });
        };
        pick(&self.align, "align", ALIGN.map(|(v, _)| v).to_vec());
        pick(
            &self.valign,
            "verticalAlign",
            VALIGN.map(|(v, _)| v).to_vec(),
        );
        pick(
            &self.direction,
            "gradientDirection",
            DIRECTIONS.map(|(v, _)| v).to_vec(),
        );
        pick(&self.start, "startArrow", HEADS.map(|(v, _)| v).to_vec());
        pick(&self.end, "endArrow", HEADS.map(|(v, _)| v).to_vec());
        let route_send = send.clone();
        self.route.connect_selected_notify(move |row| {
            let (edge_style, curved) = match row.selected() {
                1 => (Some("orthogonalEdgeStyle"), None),
                2 => (None, Some("1")),
                _ => (None, None),
            };
            route_send(Change::Style(vec![
                ("edgeStyle", edge_style.map(str::to_string)),
                ("curved", curved.map(str::to_string)),
            ]));
        });

        let raw_send = send.clone();
        self.raw
            .connect_apply(move |row| raw_send(Change::Raw(row.text().to_string())));
        self.page_name
            .connect_apply(move |row| send(Change::PageName(row.text().to_string())));
    }

    /// Show `target`, writing nothing back.
    pub fn fill(&self, target: &Target) {
        self.filling.set(true);
        self.debounce.cancel();
        let cells = matches!(target, Target::Cells { .. });
        self.page_group.set_visible(!cells);
        self.raw_group.set_visible(cells);
        self.text.set_visible(cells);
        match target {
            Target::Cells {
                style,
                raw,
                count,
                vertices,
                edges,
                fills,
            } => {
                // Every shape takes a fill; flex arrows alone show only the rows it takes.
                self.shape.set_visible(*fills);
                let outline: [&gtk::Widget; 7] = [
                    self.stroke.row.upcast_ref(),
                    self.stroke_width.upcast_ref(),
                    self.dashed.upcast_ref(),
                    self.rounded.upcast_ref(),
                    self.shadow.upcast_ref(),
                    self.opacity.upcast_ref(),
                    self.rotation.upcast_ref(),
                ];
                for row in outline {
                    row.set_visible(*vertices);
                }
                self.line.set_visible(*edges);
                self.fill.fill(style.color("fillColor"));
                self.gradient.fill(style.color("gradientColor"));
                self.stroke.fill(style.color("strokeColor"));
                self.stroke_width.set_value(style.num("strokeWidth", 1.0));
                self.dashed.set_active(style.flag("dashed", false));
                self.rounded.set_active(style.flag("rounded", false));
                self.shadow.set_active(style.flag("shadow", false));
                self.opacity.set_value(style.num("opacity", 100.0));
                // draw.io keeps any turn it was given; the row shows it within a half turn.
                let turn = (style.num("rotation", 0.0) + 180.0).rem_euclid(360.0) - 180.0;
                self.rotation.set_value(turn);
                self.font_size.set_value(style.num("fontSize", 12.0));
                self.font_colour.fill(style.color("fontColor"));
                let bits = style.num("fontStyle", 0.0) as u32;
                self.font_bits.set(bits);
                self.bold.set_active(bits & 1 != 0);
                self.italic.set_active(bits & 2 != 0);
                self.underline.set_active(bits & 4 != 0);
                let at = |values: &[&str], v: Option<&str>| {
                    v.and_then(|v| values.iter().position(|x| *x == v))
                        .map_or(gtk::INVALID_LIST_POSITION, |i| i as u32)
                };
                self.align
                    .set_selected(at(&ALIGN.map(|(v, _)| v), style.get("align")));
                self.valign
                    .set_selected(at(&VALIGN.map(|(v, _)| v), style.get("verticalAlign")));
                let direction = style.get("gradientDirection").unwrap_or("south");
                self.direction
                    .set_selected(at(&DIRECTIONS.map(|(v, _)| v), Some(direction)));
                let heads = HEADS.map(|(v, _)| v);
                self.start
                    .set_selected(at(&heads, Some(style.get("startArrow").unwrap_or("none"))));
                self.end
                    .set_selected(at(&heads, Some(style.get("endArrow").unwrap_or("none"))));
                self.route.set_selected(
                    match (style.get("edgeStyle"), style.flag("curved", false)) {
                        (Some("orthogonalEdgeStyle"), _) => 1,
                        (_, true) => 2,
                        _ => 0,
                    },
                );
                // A style string is one cell's: with several selected there is none to show.
                self.raw.set_sensitive(*count == 1);
                self.raw.set_text(if *count == 1 { raw } else { "" });
            }
            Target::Page {
                name,
                size,
                background,
            } => {
                self.shape.set_visible(false);
                self.line.set_visible(false);
                self.page_name.set_text(name);
                self.background.fill(*background);
                self.page_width.set_value(size.0);
                self.page_height.set_value(size.1);
            }
        }
        self.sync_gradient();
        self.filling.set(false);
    }

    /// A gradient runs from the fill, so it is greyed without one, and its direction shows only
    /// while there is a gradient.
    fn sync_gradient(&self) {
        let fill = self.fill.switch.is_active();
        self.gradient.row.set_sensitive(fill);
        self.direction
            .set_visible(fill && self.gradient.switch.is_active());
    }
}

/// A picked colour as a style value.
fn hex(c: gtk::gdk::RGBA) -> String {
    let [r, g, b] = crate::theme::rgb_of(c);
    Color::rgb(r, g, b).hex()
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers_are_written_as_draw_io_writes_them() {
        assert_eq!(super::number(12.0), "12");
        assert_eq!(super::number(1.5), "1.5");
        let c = gtk::gdk::RGBA::new(0.0, 150.0 / 255.0, 130.0 / 255.0, 1.0);
        assert_eq!(super::hex(c), "#009682");
    }
}
