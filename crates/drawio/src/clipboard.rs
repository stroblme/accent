// The copying here is derived from draw.io mxgraph/src/view/mxGraph.js (cloneCells) and
// js/grapheditor/EditorUi.js, js/grapheditor/Graph.js and js/diagramly/EditorUi.js (Apache-2.0,
// Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for
// accent; see crates/drawio/NOTICE.
//! draw.io's clipboard: cells written as the `<mxGraphModel>` its Copy puts there, and the
//! forms its Paste reads a diagram back out of. Pasting itself is [`crate::Editor::paste`].

use std::collections::HashMap;

use crate::geom::Point;
use crate::model::{Cell, CellId, Page};
use crate::scene::Scene;
use crate::xml;

/// The XML draw.io's Copy writes for cells `ids` of `page`, drawn as `drawn` (`mxClipboard.copy`,
/// `Graph.encodeCells`, `mxGraph.cloneCells`): the topmost of them in the page's order, all
/// under each going along, on a layer of their own and numbered afresh from 2, placed from the
/// page's origin. An edge whose shape was not copied lets go of it where its end is drawn, and
/// a shape placed on a parent that was not copied by fractions of it is placed where it is.
// mxGraph.js 4523-4660; grapheditor/EditorUi.js 3562-3615; Graph.js 17817-17857
pub fn copy(page: &Page, ids: &[CellId], drawn: &Scene) -> String {
    let top = crate::edit::topmost(page, ids);
    let under = crate::edit::with_subtrees(page, top.iter().cloned());
    let cells: Vec<&Cell> = page
        .cells
        .iter()
        .filter(|c| under.contains(&c.id))
        .collect();
    // As a new model numbers what is added to it, after its root (0) and its layer (1).
    let fresh: HashMap<&str, CellId> = cells
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), (i + 2).to_string()))
        .collect();
    let mut out = vec![Cell::root("0"), Cell::layer("1", "0")];
    for cell in cells {
        let mut copy = cell.clone();
        copy.id = fresh[cell.id.as_str()].clone();
        let mapped = |id: &Option<CellId>| id.as_deref().and_then(|id| fresh.get(id).cloned());
        copy.source = mapped(&cell.source);
        copy.target = mapped(&cell.target);
        let is_top = top.contains(&cell.id);
        copy.parent = match is_top {
            true => Some("1".to_string()),
            false => mapped(&cell.parent),
        };
        if is_top && let Some(g) = &mut copy.geometry {
            let origin = page.origin_of(&cell.id);
            let route = drawn.route(&cell.id).unwrap_or_default();
            let ends = [
                (
                    cell.source.is_some() && copy.source.is_none(),
                    route.first(),
                ),
                (cell.target.is_some() && copy.target.is_none(), route.last()),
            ];
            let at = |p: &Point| Some(Point::new(p.x.round(), p.y.round()));
            if cell.edge {
                // Where the end is drawn, and anything placed from the parent from the page.
                if let (true, Some(p)) = ends[0] {
                    g.source_point = at(p).map(|p| Point::new(p.x - origin.x, p.y - origin.y));
                }
                if let (true, Some(p)) = ends[1] {
                    g.target_point = at(p).map(|p| Point::new(p.x - origin.x, p.y - origin.y));
                }
                let shift = |p: &mut Point| (p.x, p.y) = (p.x + origin.x, p.y + origin.y);
                g.source_point.iter_mut().for_each(shift);
                g.target_point.iter_mut().for_each(shift);
                g.points.iter_mut().flatten().for_each(shift);
            } else if g.relative {
                if let Some(r) = page.absolute_rect(&cell.id) {
                    (g.x, g.y, g.relative, g.offset) = (r.x, r.y, false, None);
                }
            } else {
                g.x += origin.x;
                g.y += origin.y;
            }
        }
        out.push(copy);
    }
    xml::write_model(&out)
}

/// The diagram in clipboard text, as draw.io's Paste finds one (`EditorUi.pasteCells`,
/// `extractGraphModelFromHtml`, `isCompatibleString`): the XML of a model, a page or a file —
/// as written, URI-encoded as draw.io's own Copy leaves it, or escaped inside HTML. A file's
/// first page. `None` for anything else, which pastes as text.
// diagramly/EditorUi.js 20600-20760, 1535-1575; grapheditor/EditorUi.js 6949-6975
pub fn diagram_in(text: &str) -> Option<Page> {
    let text = xml::zap_gremlins(text.trim());
    let unescape = |t: &str| {
        t.replace("&gt;", ">")
            .replace("&lt;", "<")
            .replace("\\&quot;", "\"")
            .replace('\n', "")
    };
    let within = |open: &str, close: &str| {
        let i = text.find(open)?;
        let j = text.rfind(close).filter(|j| *j > i)?;
        Some(unescape(&text[i..j + close.len()]))
    };
    let candidates = [
        xml::percent_decode(&text),
        within("&lt;mxGraphModel ", "&lt;/mxGraphModel&gt;"),
        within("&lt;mxfile ", "&lt;/mxfile&gt;"),
        Some(text.clone()),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter(|t| t.starts_with('<'))
        .find_map(|t| xml::parse(t.as_bytes()).ok())
        .and_then(|file| file.pages.into_iter().next())
}

/// Plain text as the HTML label of the text cell it pastes as: what it says, each line its own.
pub fn text_label(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.trim_end().chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::scene::scene;

    #[test]
    fn a_copy_is_a_model_of_its_own_that_reads_back_in_every_form() {
        let mut page = Page::blank("P", "p");
        let square = |id: &str, x| Cell::new_vertex(id, "1", Rect::new(x, 0.0, 40.0, 40.0), "", id);
        page.cells.push(square("a", 0.0));
        page.cells.push(square("b", 100.0));
        page.cells.push(Cell::new_edge(
            "e",
            "1",
            (Some("a"), Point::new(40.0, 20.0)),
            (Some("b"), Point::new(100.0, 20.0)),
            "",
        ));
        let drawn = scene(&page);
        // a and the edge, not b: the edge lets go of b at its end.
        let xml = copy(&page, &["e".into(), "a".into()], &drawn);
        assert!(xml.starts_with("<mxGraphModel>"), "{xml}");
        let back = diagram_in(&xml).expect("our own copy reads back");
        let ids: Vec<&str> = back.cells.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["0", "1", "2", "3"]);
        let edge = back.cell("3").unwrap();
        assert_eq!(
            (edge.source.as_deref(), edge.target.as_deref()),
            (Some("2"), None)
        );
        let end = edge.geometry.as_ref().unwrap().target_point;
        assert_eq!(end, Some(Point::new(100.0, 20.0)));
        // URI-encoded as draw.io writes it, and escaped in HTML.
        let encoded: String = xml
            .bytes()
            .map(|b| match b {
                b'<' => "%3C".to_string(),
                b'>' => "%3E".to_string(),
                b' ' => "%20".to_string(),
                b'"' => "%22".to_string(),
                b'\n' => "%0A".to_string(),
                b => (b as char).to_string(),
            })
            .collect();
        assert_eq!(diagram_in(&encoded).map(|p| p.cells.len()), Some(4));
        let html = format!(
            "<div>{}</div>",
            xml.replace("<mxGraphModel>", "<mxGraphModel dx=\"0\">")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        );
        assert_eq!(diagram_in(&html).map(|p| p.cells.len()), Some(4));
        assert!(diagram_in("just words").is_none());
        assert_eq!(text_label("a < b\r\nc\n"), "a &lt; b<br>c");
        assert!(diagram_in("<p>html</p>").is_none());
    }
}
