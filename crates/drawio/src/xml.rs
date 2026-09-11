//! Reading and writing the file format: `<mxfile>` → `<diagram>` → `<mxGraphModel>` → `<root>`
//! → `<mxCell>`s, compressed pages included on the way in.
//!
//! Derived from draw.io's `Graph.js` (`Graph.decompress`), `mxUtils.js` (`zapGremlins`) and
//! `mxCellCodec.js` (user objects), Apache-2.0; see `NOTICE`.

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use crate::Error;
use crate::geom::{Point, Rect};
use crate::model::{Cell, Element, File, Geometry, Page, Value, guid};
use crate::style::Style;

/// Parse a whole file.
pub fn parse(bytes: &[u8]) -> Result<File, Error> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let xml = std::str::from_utf8(bytes)
        .map_err(|e| Error::Format(format!("the file is not UTF-8 text: {e}")))?;
    let mut root = tree(xml)?;
    // A diagram that is not in an `<mxfile>` becomes one, as if accent had made it.
    let ours = || vec![("host".to_string(), "accent".to_string())];
    let (attrs, mut pages) = match root.name.as_str() {
        "mxfile" => {
            let diagrams = std::mem::take(&mut root.children);
            let pages = diagrams
                .into_iter()
                .filter(|c| c.name == "diagram")
                .map(page)
                .collect::<Result<Vec<_>, _>>()?;
            (root.attrs, pages)
        }
        "diagram" => (ours(), vec![page(root)?]),
        "mxGraphModel" => {
            let attrs = vec![("name".into(), "Page-1".into()), ("id".into(), guid())];
            (ours(), vec![model_page(attrs, root)])
        }
        other => return Err(Error::Format(format!("the document is <{other}>"))),
    };
    // A file always has a page; draw.io opens an `<mxfile>` without one as a blank diagram too.
    if pages.is_empty() {
        pages.push(Page::blank("Page-1", &guid()));
    }
    Ok(File { attrs, pages })
}

/// The document element as a tree, attribute values and text unescaped. Text beside child
/// elements is trimmed, which drops the indentation between them.
fn tree(xml: &str) -> Result<Element, Error> {
    let mut reader = Reader::from_str(xml);
    // The elements opened and not yet closed, innermost last.
    let mut open: Vec<Element> = Vec::new();
    let mut root = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| Error::Xml(format!("{e} (at byte {})", reader.error_position())))?;
        match event {
            Event::Start(start) => open.push(element(&start)?),
            Event::Empty(start) => close(element(&start)?, &mut open, &mut root),
            Event::End(_) => {
                if let Some(done) = open.pop() {
                    close(done, &mut open, &mut root);
                }
            }
            Event::Text(text) => {
                if let Some(top) = open.last_mut() {
                    top.text.push_str(&text.xml10_content().map_err(xml_error)?);
                }
            }
            Event::CData(data) => {
                if let Some(top) = open.last_mut() {
                    top.text.push_str(&data.decode().map_err(xml_error)?);
                }
            }
            // `&lt;`, `&#10;` and the like between runs of text.
            Event::GeneralRef(reference) => {
                let reference = format!("&{};", reference.decode().map_err(xml_error)?);
                let text = quick_xml::escape::unescape(&reference).map_err(xml_error)?;
                if let Some(top) = open.last_mut() {
                    top.text.push_str(&text);
                }
            }
            Event::Eof => break,
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
        }
    }
    if let Some(unclosed) = open.last() {
        return Err(Error::Xml(format!("<{}> is never closed", unclosed.name)));
    }
    root.ok_or_else(|| Error::Xml("there is no element".into()))
}

/// Hand a finished element to its parent, or keep it as the document element.
fn close(mut done: Element, open: &mut [Element], root: &mut Option<Element>) {
    if !done.children.is_empty() {
        done.text = done.text.trim().to_string();
    }
    match open.last_mut() {
        Some(parent) => parent.children.push(done),
        None => {
            root.get_or_insert(done);
        }
    }
}

/// An element's name and attributes. Values are normalised as an XML parser must: a literal line
/// break is a space, `&#10;` a line break.
fn element(start: &BytesStart) -> Result<Element, Error> {
    let mut attrs = Vec::new();
    for attr in start.attributes() {
        let attr = attr.map_err(xml_error)?;
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(xml_error)?;
        attrs.push((utf8(attr.key.as_ref()), value.into_owned()));
    }
    Ok(Element {
        name: utf8(start.name().as_ref()),
        attrs,
        ..Element::default()
    })
}

/// Names come out of a `&str`, so they are always UTF-8.
fn utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn xml_error(e: impl std::fmt::Display) -> Error {
    Error::Xml(e.to_string())
}

/// One `<diagram>`: its model inline, compressed into its text, or neither for an empty page.
fn page(diagram: Element) -> Result<Page, Error> {
    let Element {
        attrs,
        children,
        text,
        ..
    } = diagram;
    if let Some(model) = children.into_iter().find(|c| c.name == "mxGraphModel") {
        return Ok(model_page(attrs, model));
    }
    if text.trim().is_empty() {
        return Ok(Page {
            attrs,
            ..Page::blank("", "")
        });
    }
    let model = tree(&decompress(&text)?)?;
    if model.name != "mxGraphModel" {
        return Err(Error::Format(format!(
            "a compressed page holds <{}>",
            model.name
        )));
    }
    Ok(model_page(attrs, model))
}

/// A compressed page's model: base64 of raw deflate of `encodeURIComponent` of the XML
/// (`Graph.decompress`).
fn decompress(text: &str) -> Result<String, Error> {
    let broken = |what: &str| Error::Format(format!("a compressed page is not valid {what}"));
    let deflated = crate::base64::decode(text).ok_or_else(|| broken("base64"))?;
    let inflated =
        miniz_oxide::inflate::decompress_to_vec(&deflated).map_err(|_| broken("deflate data"))?;
    let escaped = std::str::from_utf8(&inflated).map_err(|_| broken("text"))?;
    let xml = percent_decode(escaped).ok_or_else(|| broken("URI encoding"))?;
    Ok(zap_gremlins(&xml))
}

/// JavaScript's `decodeURIComponent`: each `%XX` is a byte of UTF-8. `None` for a broken escape
/// or bytes that are not UTF-8, where JavaScript throws.
fn percent_decode(s: &str) -> Option<String> {
    let hex = |b: Option<u8>| char::from(b?).to_digit(16);
    let mut bytes = s.bytes();
    let mut out = Vec::with_capacity(s.len());
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let (high, low) = (hex(bytes.next())?, hex(bytes.next())?);
            out.push((high * 16 + low) as u8);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

/// The text without the characters XML cannot hold: controls other than tab, line feed and
/// carriage return, and U+FFFE/U+FFFF (`mxUtils.zapGremlins`; a Rust string has none of the
/// unpaired surrogates it also removes).
fn zap_gremlins(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            (c >= ' ' || matches!(c, '\t' | '\n' | '\r')) && !matches!(c, '\u{FFFE}' | '\u{FFFF}')
        })
        .collect()
}

/// A page from its `<diagram>` attributes and its `<mxGraphModel>`.
fn model_page(attrs: Vec<(String, String)>, model: Element) -> Page {
    let cells = model
        .children
        .into_iter()
        .filter(|c| c.name == "root")
        .flat_map(|root| root.children)
        .map(cell)
        .collect();
    Page {
        attrs,
        model_attrs: model.attrs,
        cells,
    }
}

/// A child of `<root>`: an `<mxCell>`, or a user object wrapping one, whose element is the cell's
/// value and whose `id` is the cell's (`mxCellCodec.beforeDecode`).
fn cell(e: Element) -> Cell {
    if e.name == "mxCell" {
        return mx_cell(e);
    }
    let Element {
        name: tag,
        mut attrs,
        children,
        ..
    } = e;
    // ponytail: the wrapper's children other than its mxCell are dropped, as `Value::Object` has
    // no place for them; draw.io writes none. Give it a `children` field if a file ever needs it.
    let inner = children.into_iter().find(|c| c.name == "mxCell");
    let mut cell = mx_cell(inner.unwrap_or_default());
    if let Some(i) = attrs.iter().position(|(k, _)| k == "id") {
        cell.id = attrs.remove(i).1;
    }
    cell.value = Value::Object { tag, attrs };
    cell
}

fn mx_cell(e: Element) -> Cell {
    let mut cell = Cell::default();
    for (key, value) in e.attrs {
        match key.as_str() {
            "id" => cell.id = value,
            "value" => cell.value = Value::Text(value),
            "style" => cell.style = Style::parse(&value),
            "parent" => cell.parent = Some(value),
            "source" => cell.source = Some(value),
            "target" => cell.target = Some(value),
            "vertex" => cell.vertex = value == "1",
            "edge" => cell.edge = value == "1",
            _ => cell.attrs.push((key, value)),
        }
    }
    if cell.id.is_empty() {
        cell.id = guid();
    }
    for child in e.children {
        if child.name == "mxGeometry" && child.attr("as") == Some("geometry") {
            cell.geometry = Some(geometry(child));
        } else {
            cell.extra.push(child);
        }
    }
    cell
}

fn geometry(e: Element) -> Geometry {
    const KNOWN: [&str; 6] = ["x", "y", "width", "height", "relative", "as"];
    let r = rect(&e);
    let mut g = Geometry {
        x: r.x,
        y: r.y,
        width: r.w,
        height: r.h,
        relative: e.attr("relative") == Some("1"),
        ..Geometry::default()
    };
    g.attrs = e
        .attrs
        .into_iter()
        .filter(|(k, _)| !KNOWN.contains(&k.as_str()))
        .collect();
    for child in e.children {
        let role = child.attr("as").unwrap_or("");
        match (child.name.as_str(), role) {
            ("mxPoint", "sourcePoint") => g.source_point = Some(point(&child)),
            ("mxPoint", "targetPoint") => g.target_point = Some(point(&child)),
            ("mxPoint", "offset") => g.offset = Some(point(&child)),
            ("Array", "points") => {
                let points = child.children.iter().filter(|p| p.name == "mxPoint");
                g.points = Some(points.map(point).collect());
            }
            ("mxRectangle", "alternateBounds") => g.alternate_bounds = Some(rect(&child)),
            _ => g.extra.push(child),
        }
    }
    g
}

fn point(e: &Element) -> Point {
    Point::new(num(e, "x"), num(e, "y"))
}

fn rect(e: &Element) -> Rect {
    Rect::new(num(e, "x"), num(e, "y"), num(e, "width"), num(e, "height"))
}

/// A numeric attribute; 0, draw.io's default, when it is missing or not a finite number.
fn num(e: &Element, name: &str) -> f64 {
    e.attr(name)
        .and_then(|v| v.trim().parse().ok())
        .filter(|n: &f64| n.is_finite())
        .unwrap_or(0.0)
}

/// The file as XML, uncompressed.
pub fn write(file: &File) -> String {
    let mut attrs = file.attrs.clone();
    for (key, value) in &mut attrs {
        match key.as_str() {
            "pages" => *value = file.pages.len().to_string(),
            "compressed" => *value = "false".into(),
            _ => {}
        }
    }
    let pages = file.pages.iter().map(diagram).collect();
    let mut out = String::new();
    write_element(&mut out, &node("mxfile", attrs, pages), 0);
    out
}

fn node(name: &str, attrs: Vec<(String, String)>, children: Vec<Element>) -> Element {
    Element {
        name: name.to_string(),
        attrs,
        children,
        text: String::new(),
    }
}

fn as_attr(role: &str) -> (String, String) {
    ("as".to_string(), role.to_string())
}

fn diagram(page: &Page) -> Element {
    let cells = page.cells.iter().map(cell_node).collect();
    let root = node("root", Vec::new(), cells);
    let model = node("mxGraphModel", page.model_attrs.clone(), vec![root]);
    node("diagram", page.attrs.clone(), vec![model])
}

/// A cell's `<mxCell>`, inside its user object's element when it has one.
fn cell_node(cell: &Cell) -> Element {
    let mut attrs = Vec::new();
    let mut put = |key: &str, value: &str| attrs.push((key.to_string(), value.to_string()));
    if let Value::Text(text) = &cell.value {
        put("id", &cell.id);
        // draw.io writes every vertex's and edge's value, a layer's only when it is named.
        if !text.is_empty() || cell.vertex || cell.edge {
            put("value", text);
        }
    }
    if !cell.style.is_empty() {
        put("style", &cell.style.to_string());
    }
    let ends = [
        ("parent", &cell.parent),
        ("source", &cell.source),
        ("target", &cell.target),
    ];
    for (key, id) in ends {
        if let Some(id) = id {
            put(key, id);
        }
    }
    if cell.vertex {
        put("vertex", "1");
    }
    if cell.edge {
        put("edge", "1");
    }
    attrs.extend(cell.attrs.iter().cloned());
    let mut children: Vec<Element> = cell.geometry.iter().map(geometry_node).collect();
    children.extend(cell.extra.iter().cloned());
    let mx_cell = node("mxCell", attrs, children);
    match &cell.value {
        Value::Text(_) => mx_cell,
        Value::Object { tag, attrs } => {
            let mut attrs = attrs.clone();
            attrs.push(("id".to_string(), cell.id.clone()));
            node(tag, attrs, vec![mx_cell])
        }
    }
}

fn geometry_node(g: &Geometry) -> Element {
    let mut attrs = rect_attrs(g.rect());
    if g.relative {
        attrs.push(("relative".to_string(), "1".to_string()));
    }
    attrs.extend(g.attrs.iter().cloned());
    attrs.push(as_attr("geometry"));
    let mut children = Vec::new();
    children.extend(g.source_point.map(|p| point_node(p, Some("sourcePoint"))));
    children.extend(g.target_point.map(|p| point_node(p, Some("targetPoint"))));
    if let Some(points) = &g.points {
        let points = points.iter().map(|&p| point_node(p, None)).collect();
        children.push(node("Array", vec![as_attr("points")], points));
    }
    children.extend(g.offset.map(|p| point_node(p, Some("offset"))));
    if let Some(r) = g.alternate_bounds {
        let mut attrs = rect_attrs(r);
        attrs.push(as_attr("alternateBounds"));
        children.push(node("mxRectangle", attrs, Vec::new()));
    }
    children.extend(g.extra.iter().cloned());
    node("mxGeometry", attrs, children)
}

fn point_node(p: Point, role: Option<&str>) -> Element {
    let mut attrs = numbers(&[("x", p.x), ("y", p.y)]);
    attrs.extend(role.map(as_attr));
    node("mxPoint", attrs, Vec::new())
}

fn rect_attrs(r: Rect) -> Vec<(String, String)> {
    numbers(&[("x", r.x), ("y", r.y), ("width", r.w), ("height", r.h)])
}

/// Numeric attributes as draw.io writes them: the zeros left out, the rest as `10` or `674.25`.
fn numbers(pairs: &[(&str, f64)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .filter(|(_, n)| *n != 0.0)
        .map(|(k, n)| (k.to_string(), n.to_string()))
        .collect()
}

/// `e` at `depth` levels of two-space indentation, one element to a line, as
/// `mxUtils.getPrettyXml` lays a file out.
fn write_element(out: &mut String, e: &Element, depth: usize) {
    let indent = "  ".repeat(depth);
    out.push_str(&indent);
    out.push('<');
    out.push_str(&e.name);
    for (key, value) in &e.attrs {
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        push_attr_value(out, value);
        out.push('"');
    }
    if e.children.is_empty() && e.text.is_empty() {
        out.push_str(" />\n");
        return;
    }
    out.push('>');
    out.push_str(&quick_xml::escape::partial_escape(e.text.as_str()));
    if !e.children.is_empty() {
        out.push('\n');
        for child in &e.children {
            write_element(out, child, depth + 1);
        }
        out.push_str(&indent);
    }
    out.push_str("</");
    out.push_str(&e.name);
    out.push_str(">\n");
}

/// An attribute value escaped as a browser's `XMLSerializer` does it. Line breaks and tabs become
/// character references, the only form of them an attribute keeps through a parser.
fn push_attr_value(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
}

/// The MIME type and bytes of a `data:` URI, with or without `;base64` (draw.io leaves it out
/// inside a style, where `;` is the separator). `None` for anything else.
pub fn decode_data_uri(uri: &str) -> Option<(String, Vec<u8>)> {
    let rest = uri.strip_prefix("data:")?;
    let (head, data) = rest.split_once(',')?;
    let mime = head.strip_suffix(";base64").unwrap_or(head);
    Some((mime.to_string(), crate::base64::decode(data)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;

    /// Two pages in draw.io's own layout, so that writing it back gives the same bytes.
    const FIXTURE: &str = r#"<mxfile host="Electron" agent="test" version="24.7.17" pages="2">
  <diagram name="First" id="p1">
    <mxGraphModel dx="1000" dy="600" grid="1" gridSize="10" pageWidth="827" pageHeight="1169" math="0">
      <root>
        <mxCell id="0" />
        <mxCell id="1" parent="0" />
        <mxCell id="2" value="Background" style="locked=1;" parent="0" />
        <mxCell id="a" value="Line one&#10;&lt;b&gt;two&lt;/b&gt; &amp; &quot;three&quot;" style="rounded=1;whiteSpace=wrap;html=1;" parent="1" vertex="1">
          <mxGeometry x="10" y="20" width="120" height="60" as="geometry" />
        </mxCell>
        <object label="%AUTHOR%" placeholders="1" AUTHOR="x" id="obj">
          <mxCell style="text;foo=bar;" parent="1" vertex="1">
            <mxGeometry x="200.5" y="20" width="80" height="30" as="geometry" />
          </mxCell>
        </object>
        <mxCell id="e" value="" style="edgeStyle=orthogonalEdgeStyle;html=1;" parent="1" source="a" target="obj" edge="1">
          <mxGeometry relative="1" as="geometry">
            <mxPoint x="130" y="50" as="sourcePoint" />
            <mxPoint x="200" y="35" as="targetPoint" />
            <Array as="points">
              <mxPoint x="160" y="50" />
              <mxPoint x="160" y="35" />
            </Array>
          </mxGeometry>
        </mxCell>
        <mxCell id="lbl" value="yes" style="edgeLabel;html=1;" parent="e" vertex="1" connectable="0">
          <mxGeometry x="-0.25" y="1" relative="1" as="geometry">
            <mxPoint x="4" y="-6" as="offset" />
          </mxGeometry>
        </mxCell>
        <mxCell id="u" value="" style="ellipse;" parent="2" vertex="1">
          <mxGeometry width="40" height="40" as="geometry" />
          <customData key="k">kept &amp; &lt;safe&gt;</customData>
        </mxCell>
      </root>
    </mxGraphModel>
  </diagram>
  <diagram name="Second" id="p2">
    <mxGraphModel grid="0">
      <root>
        <mxCell id="0" />
        <mxCell id="1" parent="0" />
        <mxCell id="s" value="" style="endArrow=none;" parent="1" edge="1">
          <mxGeometry relative="1" foo="1" as="geometry">
            <Array as="points" />
          </mxGeometry>
        </mxCell>
      </root>
    </mxGraphModel>
  </diagram>
</mxfile>
"#;

    fn fixture() -> File {
        parse(FIXTURE.as_bytes()).unwrap()
    }

    #[test]
    fn parses_every_cell_form() {
        let file = fixture();
        assert_eq!(file.pages.len(), 2);
        let page = &file.pages[0];
        assert_eq!((page.name(), file.pages[1].name()), ("First", "Second"));
        assert_eq!(page.model_attr("dx"), Some("1000"));
        assert_eq!(page.cells.len(), 8);
        assert_eq!(page.layers().len(), 2);
        assert_eq!(page.cell("2").unwrap().style.get("locked"), Some("1"));

        let a = page.cell("a").unwrap();
        assert_eq!(a.label(), "Line one\n<b>two</b> & \"three\"");
        assert!(a.vertex && a.is_html());
        assert_eq!(
            a.geometry.as_ref().unwrap().rect(),
            Rect::new(10.0, 20.0, 120.0, 60.0)
        );

        let obj = page.cell("obj").unwrap();
        let Value::Object { tag, attrs } = &obj.value else {
            panic!("{:?}", obj.value)
        };
        assert_eq!(tag, "object");
        let keys: Vec<&str> = attrs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["label", "placeholders", "AUTHOR"]);
        assert_eq!(obj.label(), "%AUTHOR%");
        assert_eq!(obj.style.get("foo"), Some("bar"));
        assert_eq!((obj.parent.as_deref(), obj.vertex), (Some("1"), true));
        assert_eq!(obj.geometry.as_ref().unwrap().x, 200.5);

        let e = page.cell("e").unwrap();
        assert!(e.edge && !e.vertex);
        assert_eq!(
            (e.source.as_deref(), e.target.as_deref()),
            (Some("a"), Some("obj"))
        );
        let g = e.geometry.as_ref().unwrap();
        assert!(g.relative);
        assert_eq!(g.source_point, Some(Point::new(130.0, 50.0)));
        assert_eq!(g.target_point, Some(Point::new(200.0, 35.0)));
        assert_eq!(
            g.points,
            Some(vec![Point::new(160.0, 50.0), Point::new(160.0, 35.0)])
        );

        let label = page.cell("lbl").unwrap().geometry.as_ref().unwrap();
        assert!(label.relative);
        assert_eq!((label.x, label.y), (-0.25, 1.0));
        assert_eq!(label.offset, Some(Point::new(4.0, -6.0)));

        let s = file.pages[1].cell("s").unwrap();
        assert_eq!(s.geometry.as_ref().unwrap().points, Some(vec![]));
    }

    #[test]
    fn round_trip_is_structurally_equal() {
        let file = fixture();
        let xml = write(&file);
        assert_eq!(parse(xml.as_bytes()).unwrap(), file);
        assert_eq!(
            xml, FIXTURE,
            "draw.io's own layout comes back byte for byte"
        );
    }

    #[test]
    fn unknown_attributes_and_elements_survive() {
        // Written back they are part of the byte-for-byte round trip above.
        let file = fixture();
        let page = &file.pages[0];
        assert_eq!(model::attr(&file.attrs, "agent"), Some("test"));
        assert_eq!(page.cell("lbl").unwrap().attr("connectable"), Some("0"));
        let extra = &page.cell("u").unwrap().extra[0];
        assert_eq!(
            (extra.name.as_str(), extra.text.as_str()),
            ("customData", "kept & <safe>")
        );
        assert_eq!(extra.attr("key"), Some("k"));
        let g = file.pages[1].cell("s").unwrap().geometry.clone().unwrap();
        assert_eq!(g.attrs, [("foo".into(), "1".into())]);
    }

    #[test]
    fn a_bare_model_is_one_page() {
        let bare = "\u{feff}<mxGraphModel dx=\"1\"><root><mxCell id=\"0\"/><mxCell id=\"1\" parent=\"0\"/></root></mxGraphModel>";
        let file = parse(bare.as_bytes()).unwrap();
        assert_eq!(file.attrs, [("host".into(), "accent".into())]);
        assert_eq!(file.pages.len(), 1);
        let page = &file.pages[0];
        assert_eq!(page.name(), "Page-1");
        assert_eq!(model::attr(&page.attrs, "id").map(str::len), Some(20));
        assert_eq!(page.model_attr("dx"), Some("1"));
        assert_eq!(page.layers().len(), 1);

        let lone = parse(br#"<diagram name="D" id="d"/>"#).unwrap();
        assert_eq!(lone.pages[0].name(), "D");
        assert_eq!(
            lone.pages[0].cells.len(),
            2,
            "an empty page has its root and layer"
        );
    }

    #[test]
    fn a_compressed_page_decodes() {
        // `Graph.compress` of a model whose label holds "Grüße", a control character and "%".
        let xml = r#"<mxfile host="x" compressed="true"><diagram name="C" id="c">
            jVFBDoMgEHzN3hHapr1K1VMfQcJGTEAMotXfFwWrbdKkB8nO7MwyrMC4mSonOvWwEjVQUrtGArsDpSR8wApg3FnrY2UmjnqRfYho+aObrV3SCYet/8cwRsMo9ICRqRxwBjlfzluJQMLMMOMiTAcsD3VGAjxHX+9nnXzODq3EZWy26r5zpGgjOo/TgUq5KrQGvZuDZOvG5GSOMD2EPBvpVaSuiVLY1Cpdc0qc6COu33P3LYQiLWKD+8LX3uEHseIF
        </diagram></mxfile>"#;
        let file = parse(xml.as_bytes()).unwrap();
        let page = &file.pages[0];
        assert_eq!((page.name(), page.model_attr("grid")), ("C", Some("0")));
        let v = page.cell("v").unwrap();
        assert_eq!(v.label(), "Grüße & 100%");
        assert_eq!(
            v.geometry.as_ref().unwrap().rect(),
            Rect::new(10.0, 20.0, 80.0, 40.0)
        );
        assert!(write(&file).contains(r#"<mxCell id="v" value="Grüße &amp; 100%""#));
    }

    #[test]
    fn other_xml_is_refused() {
        assert!(matches!(parse(b"<svg/>"), Err(Error::Format(_))));
        assert!(matches!(
            parse(b"<mxfile><diagram></mxfile>"),
            Err(Error::Xml(_))
        ));
        assert!(matches!(parse(b"\xff<mxfile/>"), Err(Error::Format(_))));
    }

    #[test]
    fn pages_and_compressed_are_rewritten() {
        let mut file = fixture();
        file.pages.push(Page::blank("Third", "p3"));
        model::set_attr(&mut file.attrs, "compressed", "true");
        let xml = write(&file);
        assert!(xml.starts_with(r#"<mxfile host="Electron" agent="test" version="24.7.17" pages="3" compressed="false">"#));
        let bare = write(&File::blank());
        assert!(!bare.contains("pages=") && !bare.contains("compressed="));
    }

    #[test]
    fn empty_input_is_a_blank_page() {
        let file = File::from_bytes(b"  ").unwrap();
        assert_eq!(file.pages.len(), 1);
        assert_eq!(file.pages[0].cells.len(), 2);
        assert_eq!(parse(write(&file).as_bytes()).unwrap(), file);
    }

    #[test]
    fn data_uris_decode_with_and_without_the_base64_marker() {
        let a = super::decode_data_uri("data:image/png,Zm9v").unwrap();
        let b = super::decode_data_uri("data:image/png;base64,Zm9v").unwrap();
        assert_eq!(a, ("image/png".to_string(), b"foo".to_vec()));
        assert_eq!(a, b);
        assert!(super::decode_data_uri("https://example.org/a.png").is_none());
    }
}
