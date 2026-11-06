//! Diagram tools, cell creation and tool ring. Use draw.io sidebar style defaults
//! so new cells look the same when opened in draw.io.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use accent_core::config::{DiagramConfig, Route};
use accent_drawio::Style;
use accent_drawio::presets::{self, EdgeKind};
use adw::prelude::*;

use crate::ring::Ring;

/// What a drag over the page does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    /// Select, move and resize what is there.
    #[default]
    Select,
    Rect,
    Ellipse,
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
            Tool::Select | Tool::Image => None,
        }
    }

    /// File-specific style, falling back to the ordinary ring preset.
    pub fn style_in(self, file: &accent_drawio::File, options: &DiagramConfig) -> String {
        let stored = self
            .default_attr()
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
            Tool::Text => "win.diagram-text",
            Tool::Connector => "win.diagram-connector",
            Tool::Image => "win.diagram-image",
        }
    }

    /// Whether a drag with it draws a box: the shapes and text.
    pub fn draws_box(self) -> bool {
        matches!(self, Tool::Rect | Tool::Ellipse | Tool::Text)
    }

    /// New-cell style from the ring's options, persisted in `[diagram]` so all tabs
    /// share drawing settings.
    pub fn style(self, options: &DiagramConfig) -> String {
        match self {
            Tool::Rect if options.rounded => presets::ROUNDED.to_string(),
            Tool::Rect => presets::RECT.to_string(),
            Tool::Ellipse => presets::ELLIPSE.to_string(),
            Tool::Text => presets::TEXT.to_string(),
            Tool::Connector => presets::edge(edge_kind(options.route), options.arrow),
            Tool::Select | Tool::Image => String::new(),
        }
    }

    /// The text a new cell starts with: a text box is useless empty, a shape is not.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Text => "Text",
            _ => "",
        }
    }
}

impl crate::ring::Tool for Tool {
    const TOOLS: &'static [(Tool, &'static str)] = &[
        (Tool::Select, "tool-select-symbolic"),
        (Tool::Rect, "tool-rect-symbolic"),
        (Tool::Ellipse, "tool-circle-symbolic"),
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
    Route(Route),
    Arrow(bool),
}

impl Pick {
    /// The tool whose new cells it is for.
    pub fn tool(self) -> Tool {
        match self {
            Pick::Rounded(_) => Tool::Rect,
            Pick::Route(_) | Pick::Arrow(_) => Tool::Connector,
        }
    }

    /// `options` with it picked.
    pub fn apply(self, options: DiagramConfig) -> DiagramConfig {
        match self {
            Pick::Rounded(rounded) => DiagramConfig { rounded, ..options },
            Pick::Route(route) => DiagramConfig { route, ..options },
            Pick::Arrow(arrow) => DiagramConfig { arrow, ..options },
        }
    }

    /// A Default Style with it picked: only the keys it is about rewritten, as the ring's own
    /// preset writes them.
    pub fn restyle(self, style: &str) -> String {
        let mut style = Style::parse(style);
        match self {
            Pick::Rounded(on) => style.set("rounded", Some(if on { "1" } else { "0" })),
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
    pub route: Option<Route>,
    pub arrow: bool,
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
            route: Some(options.route),
            arrow: options.arrow,
        }
    }
}

/// The glyphs on the outer orbit: what a rectangle's corners and a connector's line will be.
#[derive(Debug, Clone, Copy)]
enum Glyph {
    Square,
    Rounded,
    Line(Route),
    Arrow,
}

type OnPick = RefCell<Option<Box<dyn Fn(Pick)>>>;

/// The generic ring with a diagram's options on its outer orbit: plain or rounded corners while
/// the rectangle is in hand, and the line's route and its arrow while the connector is.
pub struct DiagramRing {
    ring: Rc<Ring<Tool>>,
    corners: Vec<(bool, gtk::ToggleButton)>,
    lines: Vec<(Route, gtk::ToggleButton)>,
    arrow: gtk::ToggleButton,
    picks: Cell<Picks>,
    on_pick: OnPick,
}

impl DiagramRing {
    pub fn new() -> Rc<DiagramRing> {
        let ring = Ring::<Tool>::new();
        let slots = 6;
        let option = |i: usize, glyph: Glyph, tooltip: &str| {
            let button = ring.add_option(i, slots, glyph_area(glyph).upcast_ref());
            button.set_tooltip_text(Some(tooltip));
            button
        };
        let corners = vec![
            (false, option(0, Glyph::Square, "Square Corners")),
            (true, option(1, Glyph::Rounded, "Rounded Corners")),
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
        let diagram = Rc::new(DiagramRing {
            ring,
            corners,
            lines,
            arrow,
            picks: Cell::new(DiagramConfig::default().into()),
            on_pick: RefCell::new(None),
        });
        for (rounded, button) in &diagram.corners {
            let (rounded, weak) = (*rounded, Rc::downgrade(&diagram));
            button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.choose(Pick::Rounded(rounded));
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

    pub fn set_picks(&self, picks: Picks) {
        self.picks.set(picks);
        self.sync();
    }

    /// Call `f` with each click on the outer orbit; it answers with [`DiagramRing::set_picks`].
    pub fn connect_pick(&self, f: impl Fn(Pick) + 'static) {
        *self.on_pick.borrow_mut() = Some(Box::new(f));
    }

    fn choose(&self, pick: Pick) {
        if let Some(f) = self.on_pick.borrow().as_ref() {
            f(pick);
        }
        // A click that changed nothing leaves its button as it was.
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
        for (route, button) in &self.lines {
            check(button, tool == Tool::Connector, Some(*route) == picks.route);
        }
        check(&self.arrow, tool == Tool::Connector, picks.arrow);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
