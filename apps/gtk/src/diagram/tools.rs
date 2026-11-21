//! Diagram tools, cell creation and tool ring. Use draw.io sidebar style defaults
//! so new cells look the same when opened in draw.io.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_core::config::{DiagramConfig, Route};
use accent_drawio::palette::{self, Entry};
use accent_drawio::presets::{self, EdgeKind};
use accent_drawio::{Color, Style};
use adw::prelude::*;

use super::colours::{SLOTS, hex};
use super::geometry::{DEFAULT_SIZE, Frame};
use super::paint::{self, Cache, Tint};
use crate::ring::Ring;

/// What a drag over the page does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    /// Select, move and resize what is there.
    #[default]
    Select,
    Rect,
    Ellipse,
    /// One of draw.io's palette shapes, the one picked on the outer orbit.
    Shape,
    Text,
    /// Draw an edge from one shape to another, or to where the drag ends.
    Connector,
    /// Pick a picture to embed; nothing to drag.
    Image,
}

impl Tool {
    /// The creation tool a selected cell's shape belongs to, if this editor can make it.
    pub fn of(cell: &accent_drawio::Cell) -> Option<Tool> {
        if cell.edge {
            return Some(Tool::Connector);
        }
        if !cell.vertex || cell.style.names("group") || cell.style.names("edgeLabel") {
            return None;
        }
        match cell.style.resolve(false).shape() {
            "ellipse" => Some(Tool::Ellipse),
            "label" if cell.style.names("text") => Some(Tool::Text),
            "label" => Some(Tool::Rect),
            _ => None,
        }
    }

    pub fn default_attr(self) -> Option<&'static str> {
        match self {
            Tool::Rect => Some("accentStyleRect"),
            Tool::Ellipse => Some("accentStyleEllipse"),
            Tool::Text => Some("accentStyleText"),
            Tool::Connector => Some("accentStyleConnector"),
            Tool::Select | Tool::Shape | Tool::Image => None,
        }
    }

    /// File-specific style, falling back to the ordinary ring preset. A Note is always draw.io's
    /// own, the text box's Default Style being no Note's.
    pub fn style_in(self, file: &accent_drawio::File, options: &DiagramConfig) -> String {
        let stored = self
            .default_attr()
            .filter(|_| !(self == Tool::Text && options.note))
            .and_then(|key| accent_drawio::model::attr(&file.attrs, key))
            .map(str::to_string);
        match stored {
            Some(style) if self == Tool::Connector => {
                // Attachment points belong to the old endpoints, not the new edge's look.
                let mut style = Style::parse(&style);
                for key in [
                    "exitX",
                    "exitY",
                    "exitDx",
                    "exitDy",
                    "exitPerimeter",
                    "entryX",
                    "entryY",
                    "entryDx",
                    "entryDy",
                    "entryPerimeter",
                    "sourcePort",
                    "targetPort",
                    "sourcePortConstraint",
                    "targetPortConstraint",
                ] {
                    style.set(key, None);
                }
                style.to_string()
            }
            Some(style) => style,
            None => self.style(options),
        }
    }

    /// Action id shared by ring buttons, status bar and palette to obtain labels from `ACTIONS`.
    pub fn action(self) -> &'static str {
        match self {
            Tool::Select => "win.diagram-select",
            Tool::Rect => "win.diagram-rect",
            Tool::Ellipse => "win.diagram-ellipse",
            Tool::Shape => "win.diagram-shape",
            Tool::Text => "win.diagram-text",
            Tool::Connector => "win.diagram-connector",
            Tool::Image => "win.diagram-image",
        }
    }

    /// Whether a drag with it draws a box: the shapes and text.
    pub fn draws_box(self) -> bool {
        matches!(self, Tool::Rect | Tool::Ellipse | Tool::Shape | Tool::Text)
    }

    /// The size a click draws its cell at: the palette entry's for a shape, as draw.io's sidebar
    /// inserts it, and draw.io's rectangle's for the rest.
    pub fn size(self, options: &DiagramConfig) -> (f64, f64) {
        match self {
            Tool::Shape => (options.shape.width.into(), options.shape.height.into()),
            _ => DEFAULT_SIZE,
        }
    }

    /// New-cell style from the ring's options, persisted in `[diagram]` so all tabs
    /// share drawing settings.
    pub fn style(self, options: &DiagramConfig) -> String {
        match self {
            Tool::Rect if options.rounded => presets::ROUNDED.to_string(),
            Tool::Rect => presets::RECT.to_string(),
            Tool::Ellipse => presets::ELLIPSE.to_string(),
            Tool::Shape => options.shape.style.to_string(),
            Tool::Text if options.note => presets::NOTE.to_string(),
            Tool::Text => presets::TEXT.to_string(),
            Tool::Connector => presets::edge(edge_kind(options.route), options.arrow),
            Tool::Select | Tool::Image => String::new(),
        }
    }
}

/// The text a new cell of `style` starts with: a text box is useless empty, a shape, a Note too,
/// is not.
pub fn label(style: &str) -> &'static str {
    match Style::parse(style).names("text") {
        true => "Text",
        false => "",
    }
}

impl crate::ring::Tool for Tool {
    const TOOLS: &'static [(Tool, &'static str)] = &[
        (Tool::Select, "tool-select-symbolic"),
        (Tool::Rect, "tool-rect-symbolic"),
        (Tool::Ellipse, "tool-circle-symbolic"),
        (Tool::Shape, "tool-shapes-symbolic"),
        (Tool::Text, "insert-text-symbolic"),
        (Tool::Connector, "tool-line-symbolic"),
        (Tool::Image, "insert-image-symbolic"),
    ];

    fn action(self) -> Option<&'static str> {
        Some(Tool::action(self))
    }

    fn none() -> Tool {
        Tool::Select
    }
}

fn edge_kind(route: Route) -> EdgeKind {
    match route {
        Route::Straight => EdgeKind::Straight,
        Route::Orthogonal => EdgeKind::Orthogonal,
        Route::Curved => EdgeKind::Curved,
    }
}

/// The keys a connector's route is written in: what [`presets::edge`] writes for one, and the
/// `noEdgeStyle` draw.io's Waypoints menu clears.
const ROUTE_KEYS: [&str; 5] = [
    "edgeStyle",
    "curved",
    "noEdgeStyle",
    "orthogonalLoop",
    "jettySize",
];

/// A click on the ring's outer orbit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Rounded(bool),
    /// The text tool's Note, or plain text.
    Note(bool),
    Route(Route),
    Arrow(bool),
    /// The Shape tool's palette entry.
    Shape(&'static Entry),
}

impl Pick {
    /// The Default Style it is written into where the diagram has one: its tool's, and none for
    /// Text or Note, which picks the shape the text tool draws, not a key of the text box's style,
    /// nor for a shape, whose tool has no Default Style.
    pub fn default_attr(self) -> Option<&'static str> {
        match self {
            Pick::Rounded(_) => Tool::Rect.default_attr(),
            Pick::Route(_) | Pick::Arrow(_) => Tool::Connector.default_attr(),
            Pick::Note(_) | Pick::Shape(_) => None,
        }
    }

    /// `options` with it picked.
    pub fn apply(self, options: DiagramConfig) -> DiagramConfig {
        match self {
            Pick::Rounded(rounded) => DiagramConfig { rounded, ..options },
            Pick::Note(note) => DiagramConfig { note, ..options },
            Pick::Route(route) => DiagramConfig { route, ..options },
            Pick::Arrow(arrow) => DiagramConfig { arrow, ..options },
            Pick::Shape(shape) => DiagramConfig { shape, ..options },
        }
    }

    /// A Default Style with it picked: only the keys it is about rewritten, as the ring's own
    /// preset writes them.
    pub fn restyle(self, style: &str) -> String {
        let mut style = Style::parse(style);
        match self {
            Pick::Rounded(on) => style.set("rounded", Some(if on { "1" } else { "0" })),
            // Never written into a Default Style (`Pick::default_attr`).
            Pick::Note(_) | Pick::Shape(_) => {}
            Pick::Arrow(on) => style.set("endArrow", (!on).then_some("none")),
            Pick::Route(route) => {
                let preset = Style::parse(&presets::edge(edge_kind(route), true));
                for key in ROUTE_KEYS {
                    style.set(key, preset.get(key));
                }
            }
        }
        style.to_string()
    }
}

/// What the ring's outer orbit shows picked: the config's picks, or where the diagram has a
/// Default Style for the tool, what that style draws. A route the ring does not offer, an elbow
/// above all, shows none of its three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picks {
    pub rounded: bool,
    pub note: bool,
    pub route: Option<Route>,
    pub arrow: bool,
    pub shape: &'static Entry,
}

impl Picks {
    pub fn of(file: &accent_drawio::File, options: &DiagramConfig) -> Picks {
        let default = |tool: Tool| {
            let key = tool.default_attr()?;
            accent_drawio::model::attr(&file.attrs, key).map(Style::parse)
        };
        let mut picks = Picks::from(*options);
        if let Some(style) = default(Tool::Rect) {
            picks.rounded = style.resolve(false).flag("rounded", false);
        }
        if let Some(style) = default(Tool::Connector) {
            let style = style.resolve(true);
            picks.route = match style.get("edgeStyle") {
                _ if style.flag("noEdgeStyle", false) => Some(Route::Straight),
                None => Some(Route::Straight),
                Some("orthogonalEdgeStyle") if style.flag("curved", false) => Some(Route::Curved),
                Some("orthogonalEdgeStyle") => Some(Route::Orthogonal),
                Some(_) => None,
            };
            picks.arrow = style.get("endArrow").is_some();
        }
        picks
    }
}

impl From<DiagramConfig> for Picks {
    fn from(options: DiagramConfig) -> Picks {
        Picks {
            rounded: options.rounded,
            note: options.note,
            route: Some(options.route),
            arrow: options.arrow,
            shape: options.shape,
        }
    }
}

/// The glyphs on the outer orbit: what a rectangle's corners, the text tool's cell and a
/// connector's line will be.
#[derive(Debug, Clone, Copy)]
enum Glyph {
    Square,
    Rounded,
    Text,
    Note,
    Line(Route),
    Arrow,
}

type OnPick = RefCell<Option<Box<dyn Fn(Pick)>>>;
type OnColour = RefCell<Option<Box<dyn Fn([u8; 3])>>>;
type OnWidth = RefCell<Option<Box<dyn Fn(f64)>>>;

/// The line widths on the outer orbit, as the PDF ring's three: the `strokeWidth`, the diameter of
/// the dot that shows it and its name.
const WIDTHS: [(f64, f64, &str); 3] = [
    (1.0, 6.0, "Narrow Width"),
    (2.0, 10.0, "Medium Width"),
    (4.0, 15.0, "Wide Width"),
];

/// The Select tool's commands, on the swatches' slots as the PDF ring has its own, while anything
/// is selected. Paste is left to `Ctrl+V` and the page's menu, so Select alone puts nothing out.
const COMMANDS: [(&str, &str); 4] = [
    ("win.diagram-cut", "edit-cut-symbolic"),
    ("win.diagram-copy", "edit-copy-symbolic"),
    ("win.diagram-duplicate", "tool-duplicate-symbolic"),
    ("win.diagram-delete", "user-trash-symbolic"),
];

/// The shape grid's columns, the size its glyphs are drawn at, and the height it scrolls past.
const COLUMNS: u32 = 6;
const CELL: i32 = 32;
const GRID_HEIGHT: i32 = 360;
/// How wide a shape's glyph draws its line on screen, as the other glyphs do.
const LINE: f64 = 1.5;

/// The generic ring with a diagram's options on its outer orbit: plain or rounded corners while
/// the rectangle is in hand, the shape picked while the Shape tool is, plain text or a Note while
/// the text tool is, and the line's route and its arrow while the connector is. Colour swatches
/// follow them for every drawing tool, and line widths for every tool that draws a line; Select
/// has its commands in the swatches' place.
pub struct DiagramRing {
    ring: Rc<Ring<Tool>>,
    corners: Vec<(bool, gtk::ToggleButton)>,
    /// The Shape tool's one option, its pick's glyph, which opens the grid of the others.
    shape: gtk::ToggleButton,
    grid: std::cell::OnceCell<Grid>,
    texts: Vec<(bool, gtk::ToggleButton)>,
    lines: Vec<(Route, gtk::ToggleButton)>,
    arrow: gtk::ToggleButton,
    picks: Rc<Cell<Picks>>,
    swatches: Vec<gtk::ToggleButton>,
    /// The swatches' colours as the file is given them, and the one picked.
    colours: Rc<RefCell<Vec<[u8; 3]>>>,
    picked: Cell<Option<[u8; 3]>>,
    /// The canvas's recolouring, which the swatches are painted through.
    tint: Rc<Cell<Tint>>,
    widths: Vec<gtk::ToggleButton>,
    width: Cell<Option<f64>>,
    /// Select's [`COMMANDS`], its Colour on the slot before them, which puts the swatches out in
    /// their place, whether they are out, and whether anything is selected.
    commands: Vec<gtk::Button>,
    colour: gtk::ToggleButton,
    choosing: Cell<bool>,
    held: Cell<bool>,
    on_pick: OnPick,
    on_colour: OnColour,
    on_width: OnWidth,
}

impl DiagramRing {
    pub fn new() -> Rc<DiagramRing> {
        let ring = Ring::<Tool>::new();
        // The tool's options from the top, at most the connector's four, then the swatches and
        // the widths, in the same slots for every tool.
        let slots = 4 + SLOTS + WIDTHS.len();
        let option = |i: usize, glyph: Glyph, tooltip: &str| {
            let button = ring.add_option(i, slots, glyph_area(glyph).upcast_ref());
            button.set_tooltip_text(Some(tooltip));
            button
        };
        let corners = vec![
            (false, option(0, Glyph::Square, "Square Corners")),
            (true, option(1, Glyph::Rounded, "Rounded Corners")),
        ];
        let picks = Rc::new(Cell::new(Picks::from(DiagramConfig::default())));
        let picked = picks.clone();
        let glyph = shape_glyph(move || picked.get().shape, crate::ring::GLYPH);
        let shape = ring.add_option(0, slots, glyph.upcast_ref());
        shape.set_tooltip_text(Some(palette::DEFAULT.title));
        let texts = vec![
            (false, option(0, Glyph::Text, "Text")),
            (true, option(1, Glyph::Note, "Note")),
        ];
        let lines = vec![
            (
                Route::Straight,
                option(0, Glyph::Line(Route::Straight), "Straight"),
            ),
            (
                Route::Orthogonal,
                option(1, Glyph::Line(Route::Orthogonal), "Orthogonal"),
            ),
            (
                Route::Curved,
                option(2, Glyph::Line(Route::Curved), "Curved"),
            ),
        ];
        let arrow = option(3, Glyph::Arrow, "Arrow at the End");
        let colours: Rc<RefCell<Vec<[u8; 3]>>> = Rc::default();
        let tint: Rc<Cell<Tint>> = Rc::default();
        let swatches = (0..SLOTS)
            .map(|i| {
                let (colours, tint) = (colours.clone(), tint.clone());
                let colour = move |_: &gtk::DrawingArea| {
                    let [r, g, b] = colours.borrow().get(i).copied().unwrap_or_default();
                    tint.get().rgba(Color::rgb(r, g, b))
                };
                ring.add_option(4 + i, slots, crate::ring::dot(18.0, colour).upcast_ref())
            })
            .collect();
        let widths = (WIDTHS.iter().enumerate())
            .map(|(i, (_, diameter, name))| {
                let dot = crate::ring::dot(*diameter, |area| area.color());
                let button = ring.add_option(4 + SLOTS + i, slots, dot.upcast_ref());
                button.set_tooltip_text(Some(name));
                button
            })
            .collect();
        let commands = (COMMANDS.iter().enumerate())
            .map(|(i, (action, icon))| ring.add_button(4 + i, slots, icon, action))
            .collect();
        let palette = crate::ring::glyph("preferences-color-symbolic");
        let colour = ring.add_option(3, slots, palette.upcast_ref());
        colour.set_tooltip_text(Some("Colour"));
        let diagram = Rc::new(DiagramRing {
            ring,
            corners,
            shape,
            grid: std::cell::OnceCell::new(),
            texts,
            lines,
            arrow,
            picks,
            swatches,
            colours,
            picked: Cell::new(None),
            tint,
            widths,
            width: Cell::new(None),
            commands,
            colour,
            choosing: Cell::new(false),
            held: Cell::new(false),
            on_pick: RefCell::new(None),
            on_colour: RefCell::new(None),
            on_width: RefCell::new(None),
        });
        for (rounded, button) in &diagram.corners {
            let (rounded, weak) = (*rounded, Rc::downgrade(&diagram));
            button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.choose(Pick::Rounded(rounded));
                }
            });
        }
        let weak = Rc::downgrade(&diagram);
        diagram.shape.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.open_grid();
            }
        });
        for (note, button) in &diagram.texts {
            let (note, weak) = (*note, Rc::downgrade(&diagram));
            button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.choose(Pick::Note(note));
                }
            });
        }
        for (route, button) in &diagram.lines {
            let (route, weak) = (*route, Rc::downgrade(&diagram));
            button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.choose(Pick::Route(route));
                }
            });
        }
        let weak = Rc::downgrade(&diagram);
        diagram.arrow.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.choose(Pick::Arrow(!d.picks.get().arrow));
            }
        });
        for (i, button) in diagram.swatches.iter().enumerate() {
            let weak = Rc::downgrade(&diagram);
            button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.choose_colour(i);
                }
            });
        }
        for (button, (width, ..)) in diagram.widths.iter().zip(WIDTHS) {
            let weak = Rc::downgrade(&diagram);
            button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    if let Some(f) = d.on_width.borrow().as_ref() {
                        f(width);
                    }
                    d.sync();
                }
            });
        }
        let weak = Rc::downgrade(&diagram);
        diagram.colour.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                d.choosing.set(!d.choosing.get());
                d.sync();
            }
        });
        diagram
    }

    #[cfg(feature = "bench")]
    pub fn widget(&self) -> &gtk::Widget {
        self.ring.widget()
    }

    pub fn attach(&self, overlay: &gtk::Overlay) {
        self.ring.attach(overlay);
    }

    pub fn set_visible(&self, visible: bool, at: Option<(f64, f64)>) {
        self.ring.set_visible(visible, at);
    }

    pub fn connect_moved(&self, f: impl Fn((f64, f64)) + 'static) {
        self.ring.connect_moved(f);
    }

    pub fn set_tool(&self, tool: Tool) {
        self.ring.set_tool(tool);
        self.sync();
    }

    /// Show the insert tools only while the page has a layer to put a new cell into.
    pub fn show_inserts(&self, shown: bool) {
        self.ring.show_tools(|tool| shown || tool == Tool::Select);
    }

    pub fn set_picks(&self, picks: Picks) {
        self.picks.set(picks);
        self.shape.set_tooltip_text(Some(picks.shape.title));
        if let Some(glyph) = self.shape.child() {
            glyph.queue_draw();
        }
        self.sync();
    }

    /// Call `f` with each click on the outer orbit; it answers with [`DiagramRing::set_picks`].
    pub fn connect_pick(&self, f: impl Fn(Pick) + 'static) {
        *self.on_pick.borrow_mut() = Some(Box::new(f));
    }

    /// Open the grid of shapes beside the shape slot, the one picked focused. It is built the
    /// first time, with the shapes accent then draws.
    fn open_grid(self: &Rc<Self>) {
        let grid = self.grid.get_or_init(|| self.build_grid());
        // The keyboard goes back where it was once the grid closes, to the page a click on the
        // ring leaves it on.
        grid.back
            .replace(self.shape.root().and_then(|root| root.focus()));
        grid.popover.set_parent(&self.shape);
        grid.popover.popup();
        let picked = self.picks.get().shape;
        for (flow, entries) in &grid.flows {
            let at = entries.iter().position(|entry| *entry == picked);
            match at.and_then(|i| flow.child_at_index(i as i32)) {
                // Focused as the keys focus a shape, which selects it and puts the grid's cursor
                // on it, where the arrow keys go on from.
                Some(child) => {
                    child.child_focus(gtk::DirectionType::TabForward);
                }
                None => flow.unselect_all(),
            }
        }
        // The click flipped the slot's look, which stays checked while the tool is in hand.
        self.sync();
    }

    /// The popover of draw.io's General and Basic shapes that accent draws, a grid under each
    /// palette's title, which a click or `Return` picks from and `Escape` closes.
    fn build_grid(self: &Rc<Self>) -> Grid {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let mut flows = Vec::new();
        for (title, entries) in palette::PALETTES {
            let entries: Vec<&'static Entry> = entries.iter().filter(|e| e.drawn()).collect();
            if entries.is_empty() {
                continue;
            }
            let heading = gtk::Label::new(Some(title));
            heading.set_xalign(0.0);
            heading.add_css_class("heading");
            let flow = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::Single)
                .homogeneous(true)
                .min_children_per_line(COLUMNS)
                .max_children_per_line(COLUMNS)
                .build();
            for &entry in &entries {
                let child = gtk::FlowBoxChild::builder()
                    .child(&shape_glyph(move || entry, CELL))
                    .tooltip_text(entry.title)
                    .build();
                child.update_property(&[gtk::accessible::Property::Label(entry.title)]);
                flow.insert(&child, -1);
            }
            content.append(&heading);
            content.append(&flow);
            flows.push((flow, entries));
        }
        for (flow, entries) in &flows {
            let (weak, entries) = (Rc::downgrade(self), entries.clone());
            flow.connect_child_activated(move |_, child| {
                let popover = child.ancestor(gtk::Popover::static_type());
                if let Some(popover) = popover.and_downcast::<gtk::Popover>() {
                    popover.popdown();
                }
                if let (Some(d), Some(entry)) =
                    (weak.upgrade(), entries.get(child.index() as usize).copied())
                {
                    d.choose(Pick::Shape(entry));
                }
            });
        }
        content.add_controller(across(&flows));
        let scroller = gtk::ScrolledWindow::builder()
            .child(&content)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .propagate_natural_height(true)
            .max_content_height(GRID_HEIGHT)
            .build();
        // Left of the ring, which starts in the page's top right corner, so the grid covers the
        // page rather than the ring; GTK turns it right where there is no room.
        let popover = gtk::Popover::builder()
            .child(&scroller)
            .position(gtk::PositionType::Left)
            .build();
        let back: Rc<RefCell<Option<gtk::Widget>>> = Rc::default();
        let to = back.clone();
        // A popover parented by hand stays parented until it is unparented by hand.
        popover.connect_closed(move |popover| {
            popover.unparent();
            if let Some(widget) = to.take() {
                widget.grab_focus();
            }
        });
        Grid {
            popover,
            flows,
            back,
        }
    }

    fn choose(&self, pick: Pick) {
        if let Some(f) = self.on_pick.borrow().as_ref() {
            f(pick);
        }
        // A click that changed nothing leaves its button as it was.
        self.sync();
    }

    /// Show `colours` on the swatches, `picked` checked where it is one of them, and `width`
    /// checked among the widths.
    pub fn set_swatches(&self, colours: Vec<[u8; 3]>, picked: Option<[u8; 3]>, width: Option<f64>) {
        for (i, (button, colour)) in self.swatches.iter().zip(&colours).enumerate() {
            let name = format!("{}, {}", swatch_name(i), hex(*colour));
            button.set_tooltip_text(Some(&name));
        }
        self.colours.replace(colours);
        self.picked.set(picked);
        self.width.set(width);
        self.redraw_swatches();
        self.sync();
    }

    /// Paint the swatches as the canvas paints their colours.
    pub fn set_tint(&self, tint: Tint) {
        self.tint.set(tint);
        self.redraw_swatches();
    }

    fn redraw_swatches(&self) {
        for dot in self.swatches.iter().filter_map(|b| b.child()) {
            dot.queue_draw();
        }
    }

    /// Call `f` with the colour of each swatch clicked; it answers with
    /// [`DiagramRing::set_swatches`].
    pub fn connect_colour(&self, f: impl Fn([u8; 3]) + 'static) {
        *self.on_colour.borrow_mut() = Some(Box::new(f));
    }

    /// Call `f` with the `strokeWidth` of each width clicked; it answers with
    /// [`DiagramRing::set_swatches`].
    pub fn connect_width(&self, f: impl Fn(f64) + 'static) {
        *self.on_width.borrow_mut() = Some(Box::new(f));
    }

    /// Whether anything is selected, which Select's commands, Colour and widths act on.
    pub fn set_held(&self, held: bool) {
        self.held.set(held);
        self.sync();
    }

    /// Put Select's swatches away and its commands back, changing nothing: `Escape`. Whether they
    /// were out.
    pub fn close_colours(&self) -> bool {
        let open = self.choosing.replace(false);
        if open {
            self.sync();
        }
        open
    }

    fn choose_colour(&self, i: usize) {
        // A swatch picked under Select brings its commands back.
        self.choosing.set(false);
        let colour = self.colours.borrow().get(i).copied();
        if let (Some(colour), Some(f)) = (colour, self.on_colour.borrow().as_ref()) {
            f(colour);
        }
        self.sync();
    }

    /// Show active-tool options, deriving checked state from the picks rather than clicks.
    fn sync(&self) {
        let (tool, picks) = (self.ring.tool(), self.picks.get());
        let check = |button: &gtk::ToggleButton, shown: bool, wanted: bool| {
            button.set_visible(shown);
            if button.is_active() != wanted {
                button.set_active(wanted);
            }
        };
        for (rounded, button) in &self.corners {
            check(button, tool == Tool::Rect, *rounded == picks.rounded);
        }
        check(&self.shape, tool == Tool::Shape, true);
        for (note, button) in &self.texts {
            check(button, tool == Tool::Text, *note == picks.note);
        }
        for (route, button) in &self.lines {
            check(button, tool == Tool::Connector, Some(*route) == picks.route);
        }
        check(&self.arrow, tool == Tool::Connector, picks.arrow);
        let (selecting, held) = (tool == Tool::Select, self.held.get());
        let choosing = selecting && held && self.choosing.get();
        self.choosing.set(choosing);
        for button in &self.commands {
            button.set_visible(selecting && held && !choosing);
        }
        check(&self.colour, selecting && held, choosing);
        let colours = self.colours.borrow();
        for (i, button) in self.swatches.iter().enumerate() {
            let colour = colours.get(i).copied();
            let shown = colour.is_some()
                && match tool {
                    Tool::Select => choosing,
                    Tool::Image => false,
                    _ => true,
                };
            check(button, shown, shown && colour == self.picked.get());
        }
        let lines = match tool {
            Tool::Text => picks.note,
            Tool::Select => held,
            Tool::Image => false,
            _ => true,
        };
        for (button, (width, ..)) in self.widths.iter().zip(WIDTHS) {
            check(button, lines, lines && self.width.get() == Some(width));
        }
    }
}

/// The arrow keys going on from a palette's grid into the one above or below it, to the shape in
/// the same column, as in one grid: `Up` from its first row and `Down` from where no shape is
/// below. GTK's `keynav-failed` says Down for either key (ISSUES.md), so the keys are read first.
fn across(flows: &[(gtk::FlowBox, Vec<&'static Entry>)]) -> gtk::EventControllerKey {
    let flows: Vec<(gtk::FlowBox, u32)> = (flows.iter())
        .map(|(flow, entries)| (flow.clone(), entries.len() as u32))
        .collect();
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, key, _, _| {
        let (down, direction) = match key {
            gtk::gdk::Key::Up => (false, gtk::DirectionType::Up),
            gtk::gdk::Key::Down => (true, gtk::DirectionType::Down),
            _ => return gtk::glib::Propagation::Proceed,
        };
        let focused = flows.iter().enumerate().find_map(|(i, (flow, n))| {
            let child = flow.focus_child().and_downcast::<gtk::FlowBoxChild>()?;
            Some((i, flow, *n, child.index() as u32))
        });
        let Some((i, flow, n, at)) = focused else {
            return gtk::glib::Propagation::Proceed;
        };
        let next = match down {
            true if at + COLUMNS >= n => flows.get(i + 1),
            false if at < COLUMNS => i.checked_sub(1).and_then(|i| flows.get(i)),
            _ => None,
        };
        let Some((to, m)) = next else {
            return gtk::glib::Propagation::Proceed;
        };
        let row = if down { 0 } else { (m - 1) / COLUMNS };
        let Some(child) = to.child_at_index((row * COLUMNS + at % COLUMNS).min(m - 1) as i32)
        else {
            return gtk::glib::Propagation::Proceed;
        };
        flow.unselect_all();
        // Focused as the keys focus a shape, which selects it and puts the grid's cursor on it.
        child.child_focus(direction);
        gtk::glib::Propagation::Stop
    });
    keys
}

/// The Shape tool's grid: its popover, each palette's grid with the entries it shows, and where the
/// keyboard goes back to once it closes.
struct Grid {
    popover: gtk::Popover,
    flows: Vec<(gtk::FlowBox, Vec<&'static Entry>)>,
    back: Rc<RefCell<Option<gtk::Widget>>>,
}

/// What swatch `i` is called, on the ring and in Recolour Selection's palette rows.
fn swatch_name(i: usize) -> String {
    format!("Swatch {}", i + 1)
}

/// A small drawing of what an option does, in the foreground colour, centred in its disc at the
/// size of the PDF ring's icons: filling the disc, a square's outline would lie on its edge.
fn glyph_area(glyph: Glyph) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_size_request(crate::ring::GLYPH, crate::ring::GLYPH);
    area.set_halign(gtk::Align::Center);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |area, cr, w, h| {
        let c = area.color();
        cr.set_source_rgba(
            f64::from(c.red()),
            f64::from(c.green()),
            f64::from(c.blue()),
            f64::from(c.alpha()),
        );
        cr.set_line_width(1.5);
        let (w, h) = (f64::from(w), f64::from(h));
        let (l, t, r, b) = (2.0, 3.0, w - 2.0, h - 3.0);
        match glyph {
            Glyph::Square => cr.rectangle(l, t, r - l, b - t),
            Glyph::Rounded => {
                let k = 4.0;
                cr.new_sub_path();
                cr.arc(r - k, t + k, k, -std::f64::consts::FRAC_PI_2, 0.0);
                cr.arc(r - k, b - k, k, 0.0, std::f64::consts::FRAC_PI_2);
                cr.arc(
                    l + k,
                    b - k,
                    k,
                    std::f64::consts::FRAC_PI_2,
                    std::f64::consts::PI,
                );
                cr.arc(
                    l + k,
                    t + k,
                    k,
                    std::f64::consts::PI,
                    1.5 * std::f64::consts::PI,
                );
                cr.close_path();
            }
            Glyph::Text => {
                cr.move_to(l + 2.0, t);
                cr.line_to(r - 2.0, t);
                cr.move_to(w / 2.0, t);
                cr.line_to(w / 2.0, b);
            }
            // A sheet standing up, as draw.io's is, its top right corner folded over.
            Glyph::Note => {
                let (l, r, t, b, k) = (4.0, w - 4.0, 2.0, h - 2.0, 5.0);
                cr.move_to(l, t);
                cr.line_to(r - k, t);
                cr.line_to(r, t + k);
                cr.line_to(r, b);
                cr.line_to(l, b);
                cr.close_path();
                cr.move_to(r - k, t);
                cr.line_to(r - k, t + k);
                cr.line_to(r, t + k);
            }
            Glyph::Line(Route::Straight) => {
                cr.move_to(l, b);
                cr.line_to(r, t);
            }
            Glyph::Line(Route::Orthogonal) => {
                cr.move_to(l, b);
                cr.line_to(w / 2.0, b);
                cr.line_to(w / 2.0, t);
                cr.line_to(r, t);
            }
            Glyph::Line(Route::Curved) => {
                cr.move_to(l, b);
                cr.curve_to(w / 2.0, b, w / 2.0, t, r, t);
            }
            Glyph::Arrow => {
                cr.move_to(l, h / 2.0);
                cr.line_to(r, h / 2.0);
                let _ = cr.stroke();
                cr.move_to(r, h / 2.0);
                cr.line_to(r - 5.0, h / 2.0 - 4.0);
                cr.line_to(r - 5.0, h / 2.0 + 4.0);
                cr.close_path();
                let _ = cr.fill();
                return;
            }
        }
        let _ = cr.stroke();
    });
    area
}

/// The cell of `entry()`'s style, drawn by the canvas's own renderer in a box `size` pixels square:
/// all of what it paints in the foreground colour, as the other glyphs are drawn, its fill left
/// out.
fn shape_glyph(entry: impl Fn() -> &'static Entry + 'static, size: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_size_request(size, size);
    area.set_halign(gtk::Align::Center);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |area, cr, w, h| {
        let entry = entry();
        let (w, h) = (f64::from(w), f64::from(h));
        let (ew, eh) = (f64::from(entry.width), f64::from(entry.height));
        // At its own size, scaled down whole as draw.io's sidebar draws it, so the parts of a
        // fixed size keep their place, and a pixel in, where a line on the edge is not cut.
        let scale = ((w - 2.0) / ew).min((h - 2.0) / eh);
        let frame = Frame {
            scale,
            x: (w - ew * scale) / 2.0,
            y: (h - eh * scale) / 2.0,
            pixels: scale,
        };
        let (snapshot, cache) = (gtk::Snapshot::new(), Cache::default());
        for (i, prim) in glyph_prims(entry, scale).iter().enumerate() {
            paint::prim(
                &snapshot,
                area.upcast_ref(),
                i,
                prim,
                &frame,
                &cache,
                Tint::FILE,
            );
        }
        let Some(node) = snapshot.to_node() else {
            return;
        };
        // What the renderer painted, as the mask the foreground colour goes through.
        cr.push_group();
        node.draw(cr);
        let Ok(mask) = cr.pop_group() else {
            return;
        };
        let c = area.color();
        cr.set_source_rgba(
            f64::from(c.red()),
            f64::from(c.green()),
            f64::from(c.blue()),
            f64::from(c.alpha()),
        );
        let _ = cr.mask(&mask);
    });
    area
}

/// The display list of one cell of `entry`'s style at its own size, unfilled, its line as wide as
/// [`LINE`] once scaled by `scale`.
fn glyph_prims(entry: &Entry, scale: f64) -> Vec<accent_drawio::Prim> {
    let mut style = Style::parse(entry.style);
    style.set("fillColor", Some("none"));
    style.set("strokeColor", Some("#000000"));
    style.set("strokeWidth", Some(&(LINE / scale).to_string()));
    let (w, h) = (f64::from(entry.width), f64::from(entry.height));
    let rect = accent_drawio::Rect::new(0.0, 0.0, w, h);
    let cell = accent_drawio::Cell::new_vertex("glyph", "1", rect, &style.to_string(), "");
    let mut page = accent_drawio::Page::blank("", "");
    page.cells.push(cell);
    accent_drawio::scene(&page).prims
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recolour Selection names each swatch in the palette as the ring's tooltip does.
    #[test]
    fn the_palette_names_the_swatches_as_the_ring_does() {
        for i in 0..SLOTS {
            let action = format!("win.diagram-recolour-{}", i + 1);
            let label = format!("Recolour Selection: {}", swatch_name(i));
            assert_eq!(crate::actions::label_of(&action), label);
        }
    }

    #[test]
    fn each_tool_makes_draw_io_s_own_cell() {
        let mut options = DiagramConfig::default();
        assert_eq!(Tool::Rect.style(&options), presets::RECT);
        options.rounded = true;
        assert!(Tool::Rect.style(&options).contains("rounded=1"));
        assert!(
            Tool::Connector
                .style(&options)
                .contains("orthogonalEdgeStyle")
        );
        options.arrow = false;
        options.route = Route::Curved;
        let edge = Tool::Connector.style(&options);
        assert!(edge.contains("curved=1") && edge.contains("endArrow=none"));
        assert!(Tool::Text.draws_box() && !Tool::Connector.draws_box());
    }

    /// Note picked, the text tool draws draw.io's Note, empty, whatever the text box's Default
    /// Style, and the pick goes to the config, never into that style.
    #[test]
    fn the_text_tool_draws_a_note_when_picked() {
        let mut file = accent_drawio::File::blank();
        accent_drawio::model::set_attr(&mut file.attrs, "accentStyleText", "text;fontSize=20;");
        let options = Pick::Note(true).apply(DiagramConfig::default());
        assert_eq!(Tool::Text.style_in(&file, &options), presets::NOTE);
        assert_eq!(label(presets::NOTE), "");
        assert_eq!(Pick::Note(true).default_attr(), None);
        let options = Pick::Note(false).apply(options);
        assert_eq!(Tool::Text.style_in(&file, &options), "text;fontSize=20;");
        assert_eq!(label(&Tool::Text.style_in(&file, &options)), "Text");
    }

    /// The Shape tool draws the entry picked, as draw.io's sidebar inserts it and at its size for a
    /// click, and the pick goes to the config, there being no Default Style for it.
    #[test]
    fn the_shape_tool_draws_the_entry_picked() {
        let file = accent_drawio::File::blank();
        let style =
            "shape=cylinder3;whiteSpace=wrap;html=1;boundedLbl=1;backgroundOutline=1;size=15;";
        let cylinder = palette::find(style).expect("the General palette's Cylinder");
        let options = Pick::Shape(cylinder).apply(DiagramConfig::default());
        assert_eq!(Tool::Shape.style_in(&file, &options), style);
        assert_eq!(label(style), "");
        assert_eq!(Tool::Shape.size(&options), (60.0, 80.0));
        assert_eq!(Tool::Rect.size(&options), DEFAULT_SIZE);
        assert_eq!(Pick::Shape(cylinder).default_attr(), None);
        assert_eq!(Picks::of(&file, &options).shape, cylinder);
        assert!(Tool::Shape.draws_box() && Tool::Shape.default_attr().is_none());
    }

    #[test]
    fn only_creation_shapes_can_supply_a_default_style() {
        use accent_drawio::{Cell, Point, Rect};

        let vertex = |style| Cell::new_vertex("v", "1", Rect::new(0.0, 0.0, 10.0, 10.0), style, "");
        assert_eq!(Tool::of(&vertex("rounded=1;")), Some(Tool::Rect));
        assert_eq!(
            Tool::of(&vertex("ellipse;fillColor=#fff;")),
            Some(Tool::Ellipse)
        );
        assert_eq!(Tool::of(&vertex("text;fillColor=none;")), Some(Tool::Text));
        assert_eq!(Tool::of(&vertex("group;")), None);
        // A palette shape's tool has no Default Style.
        assert_eq!(Tool::of(&vertex(palette::DEFAULT.style)), None);
        assert_eq!(Tool::of(&vertex("shape=image;")), None);
        let edge = Cell::new_edge(
            "e",
            "1",
            (None, Point::new(0.0, 0.0)),
            (None, Point::new(10.0, 10.0)),
            "strokeColor=#fff;",
        );
        assert_eq!(Tool::of(&edge), Some(Tool::Connector));
    }

    #[test]
    fn each_tool_uses_only_its_own_file_default() {
        let mut file = accent_drawio::File::blank();
        let options = DiagramConfig::default();
        assert_eq!(
            Tool::Rect.style_in(&file, &options),
            Tool::Rect.style(&options)
        );
        accent_drawio::model::set_attr(&mut file.attrs, "accentStyleRect", "rounded=1;");
        assert_eq!(Tool::Rect.style_in(&file, &options), "rounded=1;");
        assert_eq!(
            Tool::Ellipse.style_in(&file, &options),
            Tool::Ellipse.style(&options)
        );
        accent_drawio::model::set_attr(
            &mut file.attrs,
            "accentStyleConnector",
            "strokeColor=#fff;exitX=0.2;entryY=0.9;sourcePortConstraint=east;targetPortConstraint=west;",
        );
        assert_eq!(
            Tool::Connector.style_in(&file, &options),
            "strokeColor=#fff;"
        );
    }

    #[test]
    fn the_ring_shows_what_a_default_style_draws() {
        use accent_drawio::model::set_attr;
        let mut file = accent_drawio::File::blank();
        let options = DiagramConfig::default();
        assert_eq!(Picks::of(&file, &options), Picks::from(options));
        set_attr(
            &mut file.attrs,
            "accentStyleRect",
            "rounded=1;whiteSpace=wrap;",
        );
        let routes = [
            ("strokeColor=#f00;", Some(Route::Straight)),
            ("curved=1;", Some(Route::Straight)),
            (
                "edgeStyle=orthogonalEdgeStyle;noEdgeStyle=1;",
                Some(Route::Straight),
            ),
            ("edgeStyle=orthogonalEdgeStyle;", Some(Route::Orthogonal)),
            (
                "edgeStyle=orthogonalEdgeStyle;curved=1;",
                Some(Route::Curved),
            ),
            ("edgeStyle=elbowEdgeStyle;elbow=vertical;", None),
        ];
        for (style, route) in routes {
            set_attr(&mut file.attrs, "accentStyleConnector", style);
            let picks = Picks::of(&file, &options);
            assert_eq!(
                (picks.rounded, picks.route, picks.arrow),
                (true, route, true)
            );
        }
        set_attr(&mut file.attrs, "accentStyleConnector", "endArrow=none;");
        assert!(!Picks::of(&file, &options).arrow);
    }

    #[test]
    fn a_pick_rewrites_only_its_own_keys_of_a_default_style() {
        let elbow = "edgeStyle=elbowEdgeStyle;strokeColor=#f00;endArrow=block;";
        let curved = Pick::Route(Route::Curved).restyle(elbow);
        assert_eq!(
            curved,
            "edgeStyle=orthogonalEdgeStyle;strokeColor=#f00;endArrow=block;curved=1;orthogonalLoop=1;jettySize=auto;"
        );
        assert_eq!(
            Pick::Route(Route::Straight).restyle(&curved),
            "strokeColor=#f00;endArrow=block;"
        );
        assert_eq!(
            Pick::Arrow(false).restyle(elbow),
            "edgeStyle=elbowEdgeStyle;strokeColor=#f00;endArrow=none;"
        );
        assert_eq!(Pick::Arrow(true).restyle("endArrow=none;"), "");
        assert_eq!(
            Pick::Rounded(true).restyle("rounded=0;fillColor=#fff;"),
            "rounded=1;fillColor=#fff;"
        );
        // What a pick writes is what the ring then shows.
        let mut file = accent_drawio::File::blank();
        for route in [Route::Straight, Route::Orthogonal, Route::Curved] {
            let style = Pick::Route(route).restyle(elbow);
            accent_drawio::model::set_attr(&mut file.attrs, "accentStyleConnector", &style);
            let picks = Picks::of(&file, &DiagramConfig::default());
            assert_eq!(picks.route, Some(route));
        }
    }
}
