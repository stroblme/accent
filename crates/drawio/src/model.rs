//! The diagram as the file holds it. Everything accent does not interpret is kept verbatim —
//! attributes, style keys, unknown child elements — so a file opened and saved here loses
//! nothing draw.io put in it.

use crate::Error;
use crate::geom::{Point, Rect};
use crate::style::{Color, Style, parse_num};

pub type CellId = String;

/// An XML element accent has no model for, kept whole so it writes back unchanged. The parser
/// builds its tree out of these too.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// Character data directly inside the element, entities decoded.
    pub text: String,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        attr(&self.attrs, name)
    }
}

/// The value of `name` in an attribute list.
pub fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Set `name` in place, or append it.
pub fn set_attr(attrs: &mut Vec<(String, String)>, name: &str, value: &str) {
    match attrs.iter_mut().find(|(k, _)| k == name) {
        Some(item) => item.1 = value.to_string(),
        None => attrs.push((name.to_string(), value.to_string())),
    }
}

/// A `.drawio` file: an `<mxfile>` of pages.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct File {
    /// `<mxfile>`'s attributes (`host`, `agent`, `version`, `pages`, …) in order.
    pub attrs: Vec<(String, String)>,
    pub pages: Vec<Page>,
}

impl File {
    /// An `<mxfile>`, or a bare `<mxGraphModel>` as one page. Empty input is a new diagram of one
    /// blank page, so an empty `.drawio` made by New File opens ready to draw on.
    pub fn from_bytes(bytes: &[u8]) -> Result<File, Error> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(File::blank());
        }
        crate::xml::parse(bytes)
    }

    /// The file as draw.io writes it, uncompressed.
    pub fn to_xml(&self) -> String {
        crate::xml::write(self)
    }

    /// One blank page, as draw.io starts a new diagram.
    pub fn blank() -> File {
        File {
            attrs: vec![("host".into(), "accent".into())],
            pages: vec![Page::blank("Page-1", &guid())],
        }
    }

    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    pub fn page(&self, i: usize) -> Result<&Page, Error> {
        self.pages.get(i).ok_or(Error::NoPage(i))
    }
}

/// One `<diagram>`: a page of the file, holding one `<mxGraphModel>`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    /// `<diagram>`'s attributes, `name` and `id` among them.
    pub attrs: Vec<(String, String)>,
    /// `<mxGraphModel>`'s attributes: grid, page size, background and the rest.
    pub model_attrs: Vec<(String, String)>,
    /// Every cell in document order, the root (`0`) and the layers included. Order is paint
    /// order among siblings.
    pub cells: Vec<Cell>,
}

/// draw.io's page when a model names none: A4 portrait at 96 dpi (`mxConstants.PAGE_FORMAT_A4_PORTRAIT`).
const DEFAULT_PAGE: (f64, f64) = (827.0, 1169.0);

impl Page {
    /// A page with the root and one layer, carrying the model attributes draw.io gives a new one.
    pub fn blank(name: &str, id: &str) -> Page {
        let pairs = [
            ("grid", "1"),
            ("gridSize", "10"),
            ("guides", "1"),
            ("tooltips", "1"),
            ("connect", "1"),
            ("arrows", "1"),
            ("fold", "1"),
            ("page", "1"),
            ("pageScale", "1"),
            ("pageWidth", "827"),
            ("pageHeight", "1169"),
            ("math", "0"),
            ("shadow", "0"),
        ];
        Page {
            attrs: vec![("name".into(), name.into()), ("id".into(), id.into())],
            model_attrs: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            cells: vec![Cell::root("0"), Cell::layer("1", "0")],
        }
    }

    pub fn name(&self) -> &str {
        attr(&self.attrs, "name").unwrap_or("")
    }

    pub fn set_name(&mut self, name: &str) {
        set_attr(&mut self.attrs, "name", name);
    }

    pub fn model_attr(&self, name: &str) -> Option<&str> {
        attr(&self.model_attrs, name)
    }

    pub fn set_model_attr(&mut self, name: &str, value: Option<&str>) {
        match value {
            Some(v) => set_attr(&mut self.model_attrs, name, v),
            None => self.model_attrs.retain(|(k, _)| k != name),
        }
    }

    fn model_num(&self, name: &str) -> Option<f64> {
        self.model_attr(name).and_then(parse_num)
    }

    /// The page's size in page units.
    pub fn size(&self) -> (f64, f64) {
        (
            self.model_num("pageWidth").unwrap_or(DEFAULT_PAGE.0),
            self.model_num("pageHeight").unwrap_or(DEFAULT_PAGE.1),
        )
    }

    /// The page colour, `None` for the default (white) or `none`.
    pub fn background(&self) -> Option<Color> {
        self.model_attr("background").and_then(Color::parse)
    }

    pub fn grid_size(&self) -> f64 {
        self.model_num("gridSize")
            .filter(|g| *g > 0.0)
            .unwrap_or(10.0)
    }

    /// The grid a drag snaps to: `None` where the page has it off (`grid="0"`, draw.io's
    /// `gridEnabled`, Editor.js 1379-1393).
    pub fn grid(&self) -> Option<f64> {
        (self.model_attr("grid") != Some("0")).then(|| self.grid_size())
    }

    pub fn cell(&self, id: &str) -> Option<&Cell> {
        self.cells.iter().find(|c| c.id == id)
    }

    pub fn cell_mut(&mut self, id: &str) -> Option<&mut Cell> {
        self.cells.iter_mut().find(|c| c.id == id)
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.cells.iter().position(|c| c.id == id)
    }

    /// The cell with no parent, which holds the layers.
    pub fn root(&self) -> Option<&Cell> {
        self.cells.iter().find(|c| c.parent.is_none())
    }

    /// The children of `id` in paint order.
    pub fn children<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Cell> + 'a {
        self.cells
            .iter()
            .filter(move |c| c.parent.as_deref() == Some(id))
    }

    /// The layers, bottom first.
    pub fn layers(&self) -> Vec<&Cell> {
        match self.root() {
            Some(root) => self.children(&root.id).collect(),
            None => Vec::new(),
        }
    }

    /// The layer new cells go into: the first one not locked (`locked=1`), so that what is drawn
    /// can be picked again. draw.io puts them in its default parent, the first layer until the
    /// reader picks another in the Layers dialog, and disables inserting while that layer is
    /// locked (EditorUi.js `updateActionStates` 6004-6096, Graph.js `isCellLocked` 1431-1444).
    /// `None` when every layer is locked or there is none.
    // ponytail: a layer the reader picks, as draw.io's Layers dialog does, is the upgrade.
    pub fn default_parent(&self) -> Option<&str> {
        self.layers()
            .into_iter()
            .find(|c| c.style.get("locked") != Some("1"))
            .map(|c| c.id.as_str())
    }

    /// Where a cell's geometry is measured from: the absolute top-left of its parent vertex, or
    /// the page origin under a layer. Edges do not offset their children this way.
    pub fn origin_of(&self, id: &str) -> Point {
        let mut at = Point::default();
        let mut cell = self.cell(id);
        // A chain longer than the page has cells has walked a `parent` loop — which a hand-edited
        // file can hold, draw.io writing none — and a cell in one is measured from the page, as a
        // cell under a layer is. Counted rather than remembered: this runs per vertex per scene.
        let mut left = self.cells.len();
        while let Some(parent) = cell
            .and_then(|c| c.parent.as_deref())
            .and_then(|p| self.cell(p))
        {
            match left.checked_sub(1) {
                Some(rest) => left = rest,
                None => return Point::default(),
            }
            if parent.vertex
                && let Some(g) = &parent.geometry
                && !g.relative
            {
                at.x += g.x;
                at.y += g.y;
            }
            cell = Some(parent);
        }
        at
    }

    /// A vertex's rectangle in page coordinates, unrotated; `None` for edges, layers and cells
    /// with relative geometry.
    pub fn absolute_rect(&self, id: &str) -> Option<Rect> {
        let cell = self.cell(id)?;
        let g = cell
            .geometry
            .as_ref()
            .filter(|g| cell.vertex && !g.relative)?;
        let o = self.origin_of(id);
        Some(Rect::new(o.x + g.x, o.y + g.y, g.width, g.height))
    }
}

/// What a cell shows: plain `value` text, or a user object (`<object>`, `<UserObject>`) whose
/// `label` attribute is the text and whose other attributes are the user's own data.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Object {
        tag: String,
        /// Everything but `id`, which is the cell's, in the order the file had.
        attrs: Vec<(String, String)>,
    },
}

impl Default for Value {
    fn default() -> Value {
        Value::Text(String::new())
    }
}

/// One `<mxCell>`: the root, a layer, a vertex or an edge.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cell {
    pub id: CellId,
    pub value: Value,
    pub style: Style,
    pub parent: Option<CellId>,
    pub source: Option<CellId>,
    pub target: Option<CellId>,
    pub vertex: bool,
    pub edge: bool,
    /// Every other `<mxCell>` attribute (`connectable`, `visible`, `collapsed`, …) verbatim.
    pub attrs: Vec<(String, String)>,
    pub geometry: Option<Geometry>,
    /// Child elements other than the geometry, verbatim.
    pub extra: Vec<Element>,
}

impl Cell {
    pub fn root(id: &str) -> Cell {
        Cell {
            id: id.into(),
            ..Cell::default()
        }
    }

    pub fn layer(id: &str, root: &str) -> Cell {
        Cell {
            id: id.into(),
            parent: Some(root.into()),
            ..Cell::default()
        }
    }

    pub fn new_vertex(id: &str, parent: &str, rect: Rect, style: &str, label: &str) -> Cell {
        Cell {
            id: id.into(),
            value: Value::Text(label.into()),
            style: Style::parse(style),
            parent: Some(parent.into()),
            vertex: true,
            geometry: Some(Geometry {
                x: rect.x,
                y: rect.y,
                width: rect.w,
                height: rect.h,
                ..Geometry::default()
            }),
            ..Cell::default()
        }
    }

    /// An edge between two cells; a missing end is left dangling at the point given for it.
    pub fn new_edge(
        id: &str,
        parent: &str,
        source: (Option<&str>, Point),
        target: (Option<&str>, Point),
        style: &str,
    ) -> Cell {
        Cell {
            id: id.into(),
            style: Style::parse(style),
            parent: Some(parent.into()),
            source: source.0.map(Into::into),
            target: target.0.map(Into::into),
            edge: true,
            // draw.io writes both terminal points on every new edge, attached or not.
            geometry: Some(Geometry {
                relative: true,
                source_point: Some(source.1),
                target_point: Some(target.1),
                ..Geometry::default()
            }),
            ..Cell::default()
        }
    }

    /// The text shown: `value`, or a user object's `label`.
    pub fn label(&self) -> &str {
        match &self.value {
            Value::Text(t) => t,
            Value::Object { attrs, .. } => attr(attrs, "label").unwrap_or(""),
        }
    }

    pub fn set_label(&mut self, text: &str) {
        match &mut self.value {
            Value::Text(t) => *t = text.to_string(),
            Value::Object { attrs, .. } => set_attr(attrs, "label", text),
        }
    }

    /// Whether the cell is drawn with a fill it can be given (`Graph.isFillState`): every vertex,
    /// and of the edges the flex arrow, the one edge shape ported with an inside.
    pub fn takes_fill(&self) -> bool {
        self.vertex || (self.edge && self.style.resolve(true).shape() == "flexArrow")
    }

    /// Whether the label is an HTML fragment (`html=1`) rather than plain text.
    pub fn is_html(&self) -> bool {
        self.style.get("html") == Some("1")
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        attr(&self.attrs, name)
    }

    /// `visible="0"` hides a cell and everything under it.
    pub fn is_visible(&self) -> bool {
        self.attr("visible") != Some("0")
    }
}

/// `<mxGeometry>`. For a vertex, a rectangle relative to its parent; for an edge, its waypoints
/// and dangling ends; for a label on an edge (`relative`), a position along it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Geometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub relative: bool,
    pub offset: Option<Point>,
    pub source_point: Option<Point>,
    pub target_point: Option<Point>,
    /// The waypoints. `Some(vec![])` is an empty `<Array as="points"/>`, kept as such.
    pub points: Option<Vec<Point>>,
    pub alternate_bounds: Option<Rect>,
    /// Other attributes, verbatim.
    pub attrs: Vec<(String, String)>,
    /// Other child elements, verbatim.
    pub extra: Vec<Element>,
}

impl Geometry {
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }
}

/// A new id in draw.io's own form: 20 characters of `[0-9a-zA-Z-_]` (`Editor.guid`). Random per
/// call, from the standard library's per-process random hasher keys.
pub fn guid() -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8; 64] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ-_";
    let state = std::collections::hash_map::RandomState::new();
    let mut out = String::with_capacity(20);
    let mut round = 0u64;
    while out.len() < 20 {
        let mut h = state.build_hasher();
        h.write_u64(round);
        let mut bits = h.finish();
        round += 1;
        for _ in 0..10 {
            if out.len() == 20 {
                break;
            }
            out.push(ALPHABET[(bits & 63) as usize] as char);
            bits >>= 6;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Page {
        let mut p = Page::blank("P", "p");
        p.cells.push(Cell::new_vertex(
            "g",
            "1",
            Rect::new(100.0, 50.0, 200.0, 100.0),
            "group",
            "",
        ));
        p.cells.push(Cell::new_vertex(
            "a",
            "g",
            Rect::new(10.0, 20.0, 30.0, 40.0),
            "",
            "A",
        ));
        p
    }

    #[test]
    fn children_are_placed_from_their_parent() {
        let p = page();
        assert_eq!(
            p.absolute_rect("a"),
            Some(Rect::new(110.0, 70.0, 30.0, 40.0))
        );
        assert_eq!(p.default_parent(), Some("1"));
    }

    #[test]
    fn a_blank_page_has_the_default_size_and_grid() {
        let p = Page::blank("Page-1", "x");
        assert_eq!(p.size(), (827.0, 1169.0));
        assert_eq!(p.grid_size(), 10.0);
        assert_eq!(p.background(), None);
        assert_eq!(p.name(), "Page-1");
    }

    #[test]
    fn a_page_with_the_grid_off_snaps_to_nothing() {
        let mut p = Page::blank("P", "x");
        p.set_model_attr("gridSize", Some("20"));
        assert_eq!(p.grid(), Some(20.0));
        p.set_model_attr("grid", None);
        assert_eq!(p.grid(), Some(20.0), "draw.io's default is on");
        p.set_model_attr("grid", Some("0"));
        assert_eq!(p.grid(), None);
    }

    #[test]
    fn guids_look_like_draw_io_ids_and_differ() {
        let (a, b) = (guid(), guid());
        assert_eq!(a.len(), 20);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert_ne!(a, b);
    }

    #[test]
    fn a_user_object_labels_through_its_attribute() {
        let mut c = Cell {
            value: Value::Object {
                tag: "object".into(),
                attrs: vec![
                    ("label".into(), "%AUTHOR%".into()),
                    ("AUTHOR".into(), "M".into()),
                ],
            },
            ..Cell::default()
        };
        assert_eq!(c.label(), "%AUTHOR%");
        c.set_label("x");
        assert_eq!(c.label(), "x");
    }

    #[test]
    fn shapes_and_flex_arrows_take_a_fill_and_a_line_does_not() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        let edge = |style| {
            Cell::new_edge(
                "e",
                "1",
                (None, Point::default()),
                (None, rect.centre()),
                style,
            )
        };
        assert!(Cell::new_vertex("v", "1", rect, "text;", "").takes_fill());
        assert!(edge("shape=flexArrow;").takes_fill());
        assert!(!edge("endArrow=block;").takes_fill());
    }

    /// A cell that is its own parent: draw.io writes none, a hand-edited file can hold one, and
    /// the walk up the parents is reached from opening the file.
    #[test]
    fn a_cell_parented_to_itself_does_not_hang() {
        let mut p = Page::blank("P", "p");
        p.cells.push(Cell::new_vertex(
            "A",
            "A",
            Rect::new(10.0, 20.0, 30.0, 40.0),
            "",
            "",
        ));
        // Measured from the page: a cell in a parent loop has no origin to be offset from.
        assert_eq!(p.origin_of("A"), Point::default());
        assert_eq!(
            p.absolute_rect("A"),
            Some(Rect::new(10.0, 20.0, 30.0, 40.0))
        );
    }
}
