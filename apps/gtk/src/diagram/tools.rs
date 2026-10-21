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

/// The glyphs on the outer orbit: what a rectangle's corners and a connector's line will be.
#[derive(Debug, Clone, Copy)]
enum Glyph {
    Square,
    Rounded,
    Line(Route),
    Arrow,
}

type OnOptions = RefCell<Option<Box<dyn Fn(DiagramConfig)>>>;

/// The generic ring with a diagram's options on its outer orbit: plain or rounded corners while
/// the rectangle is in hand, and the line's route and its arrow while the connector is.
pub struct DiagramRing {
    ring: Rc<Ring<Tool>>,
    corners: Vec<(bool, gtk::ToggleButton)>,
    lines: Vec<(Route, gtk::ToggleButton)>,
    arrow: gtk::ToggleButton,
    options: Cell<DiagramConfig>,
    on_options: OnOptions,
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
            options: Cell::new(DiagramConfig::default()),
            on_options: RefCell::new(None),
        });
        for (rounded, button) in &diagram.corners {
            let (rounded, weak) = (*rounded, Rc::downgrade(&diagram));
            button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.choose(DiagramConfig {
                        rounded,
                        ..d.options.get()
                    });
                }
            });
        }
        for (route, button) in &diagram.lines {
            let (route, weak) = (*route, Rc::downgrade(&diagram));
            button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    d.choose(DiagramConfig {
                        route,
                        ..d.options.get()
                    });
                }
            });
        }
        let weak = Rc::downgrade(&diagram);
        diagram.arrow.connect_clicked(move |_| {
            if let Some(d) = weak.upgrade() {
                let options = d.options.get();
                d.choose(DiagramConfig {
                    arrow: !options.arrow,
                    ..options
                });
            }
        });
        diagram
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.ring.widget()
    }

    pub fn set_visible(&self, visible: bool, at: Option<(f64, f64)>) {
        self.ring.set_visible(visible, at);
    }

    pub fn at(&self) -> Option<(f64, f64)> {
        self.ring.at()
    }

    pub fn set_tool(&self, tool: Tool) {
        self.ring.set_tool(tool);
        self.sync();
    }

    pub fn set_options(&self, options: DiagramConfig) {
        self.options.set(options);
        self.sync();
    }

    pub fn connect_options(&self, f: impl Fn(DiagramConfig) + 'static) {
        *self.on_options.borrow_mut() = Some(Box::new(f));
    }

    fn choose(&self, options: DiagramConfig) {
        self.set_options(options);
        if let Some(f) = self.on_options.borrow().as_ref() {
            f(options);
        }
    }

    /// Show active-tool options, deriving checked state from settings rather than clicks.
    fn sync(&self) {
        let (tool, options) = (self.ring.tool(), self.options.get());
        let check = |button: &gtk::ToggleButton, shown: bool, wanted: bool| {
            button.set_visible(shown);
            if button.is_active() != wanted {
                button.set_active(wanted);
            }
        };
        for (rounded, button) in &self.corners {
            check(button, tool == Tool::Rect, *rounded == options.rounded);
        }
        for (route, button) in &self.lines {
            check(button, tool == Tool::Connector, *route == options.route);
        }
        check(&self.arrow, tool == Tool::Connector, options.arrow);
    }
}

/// A small drawing of what an option does, in the foreground colour.
fn glyph_area(glyph: Glyph) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_size_request(16, 16);
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
}
