//! The `edit` module's tests.

use super::*;
use crate::scene::scene;

/// An editor on one page holding `cells` after the root and the layer.
fn open(cells: impl IntoIterator<Item = Cell>) -> Editor {
    let mut page = Page::blank("P", "p");
    page.cells.extend(cells);
    Editor::new(File {
        attrs: Vec::new(),
        pages: vec![page],
    })
}

/// Boxes `a` and `b`, joined by `e` through one waypoint.
fn editor() -> Editor {
    let square = |id, x| Cell::new_vertex(id, "1", Rect::new(x, 0.0, 40.0, 40.0), "", id);
    let mut e = Cell::new_edge(
        "e",
        "1",
        (Some("a"), Point::new(40.0, 20.0)),
        (Some("b"), Point::new(100.0, 20.0)),
        "",
    );
    e.geometry.as_mut().unwrap().points = Some(vec![Point::new(70.0, 60.0)]);
    open([square("a", 0.0), square("b", 100.0), e])
}

fn list(ids: &[&str]) -> Vec<CellId> {
    ids.iter().map(|id| id.to_string()).collect()
}

fn order(e: &Editor) -> Vec<&str> {
    e.file.pages[0]
        .cells
        .iter()
        .map(|c| c.id.as_str())
        .collect()
}

fn geometry<'a>(e: &'a Editor, id: &str) -> &'a Geometry {
    let cell = e.file.pages[0].cell(id);
    cell.and_then(|c| c.geometry.as_ref()).unwrap()
}

#[test]
fn undo_redo_restore_cells_and_dirty() {
    let mut e = editor();
    let before = e.page(0).unwrap().clone();
    assert!(!e.dirty() && !e.can_undo() && !e.undo());
    let id = e
        .add_vertex(0, None, Rect::new(0.0, 100.0, 10.0, 10.0), "", "C")
        .unwrap();
    assert!(e.dirty() && e.can_undo() && !e.can_redo());
    e.mark_saved();
    assert!(!e.dirty());
    assert!(e.undo());
    assert_eq!(e.page(0).unwrap(), &before);
    assert!(e.dirty() && e.can_redo());
    assert!(e.redo());
    assert!(e.page(0).unwrap().cell(&id).is_some());
    assert!(!e.redo());
    e.undo();
    e.set_style(0, &list(&["a"]), "rounded", Some("1")).unwrap();
    assert!(!e.can_redo(), "a new change drops the steps to redo");
}

#[test]
fn new_ids_are_unique_and_draw_io_shaped() {
    let mut e = editor();
    let taken = format!("{}-1", e.ids.prefix);
    e.file.pages[0]
        .cells
        .push(Cell::new_vertex(&taken, "1", Rect::default(), "", ""));
    let made: Vec<CellId> = (0..3)
        .map(|_| e.add_vertex(0, None, Rect::default(), "", "").unwrap())
        .collect();
    assert!(!made.contains(&taken), "an id on the page is skipped");
    for id in &made {
        let (prefix, n) = id.rsplit_once('-').unwrap();
        let guid_char = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
        assert!(prefix.len() == 20 && prefix.bytes().all(guid_char), "{id}");
        assert!(
            !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()),
            "{id}"
        );
    }
    assert_eq!(made.iter().collect::<HashSet<_>>().len(), made.len());
}

#[test]
fn new_cells_go_into_the_current_layer_else_the_topmost_open_one() {
    let locked = |id| Cell {
        style: Style::parse("locked=1;"),
        ..Cell::layer(id, "0")
    };
    let hidden = Cell {
        attrs: vec![("visible".into(), "0".into())],
        ..Cell::layer("3", "0")
    };
    let mut e = open([Cell::layer("2", "0"), hidden]);
    e.file.pages[0].cells[1] = locked("1");
    // Picked, but locked: the topmost layer neither locked nor hidden takes them.
    e.set_current_layer(Some("1".into()));
    assert_eq!(e.current_layer(0), Some("1"));
    let v = e.add_vertex(0, None, Rect::default(), "", "").unwrap();
    let ends = (None, Point::default());
    let f = e.add_edge(0, ends, ends, "").unwrap();
    let parent = |e: &Editor, id: &str| e.page(0).unwrap().cell(id).unwrap().parent.clone();
    assert_eq!(parent(&e, &v).as_deref(), Some("2"));
    assert_eq!(parent(&e, &f).as_deref(), Some("2"));
    e.set_visible(0, "3", true).unwrap();
    e.set_current_layer(None);
    assert_eq!(e.current_layer(0), Some("3"));
    let v = e.add_vertex(0, None, Rect::default(), "", "").unwrap();
    assert_eq!(parent(&e, &v).as_deref(), Some("3"));

    let mut e = open(Vec::new());
    e.file.pages[0].cells[1] = locked("1");
    let r = e.add_vertex(0, None, Rect::default(), "", "");
    assert!(matches!(
        r,
        Err(Error::Refused(
            "every layer on this page is locked or hidden"
        ))
    ));
    assert!(!e.can_undo());
}

#[test]
fn a_pasted_style_gives_its_look_and_keeps_the_shape() {
    let cell = |id, style| Cell::new_vertex(id, "1", Rect::default(), style, "");
    let mut e = open([
        cell("r", "rounded=1;strokeColor=#0000ff;html=1;"),
        cell("t", "text;html=1;"),
        Cell::new_edge(
            "f",
            "1",
            (None, Point::default()),
            (None, Point::default()),
            "",
        ),
    ]);
    let from = Style::parse("ellipse;fillColor=#ff0000;dashed=1;fontSize=20;endArrow=block;");
    e.paste_style(0, &list(&["r", "t", "f"]), &from).unwrap();
    let style = |e: &Editor, id: &str| e.page(0).unwrap().cell(id).unwrap().style.to_string();
    // The default stroke drawn again, the arrow head an edge's alone.
    assert_eq!(
        style(&e, "r"),
        "strokeColor=default;html=1;dashed=1;fillColor=#ff0000;fontSize=20;"
    );
    assert_eq!(style(&e, "t"), "text;html=1;fontSize=20;");
    assert_eq!(
        style(&e, "f"),
        "dashed=1;fillColor=#ff0000;fontSize=20;endArrow=block;"
    );
    // A look drawn without a fill takes the fill off.
    e.paste_style(0, &list(&["r"]), &Style::parse("text;"))
        .unwrap();
    assert!(style(&e, "r").contains("fillColor=none;"));
    assert_eq!(e.undo.len(), 2, "one step each");
}

#[test]
fn layers_are_added_locked_hidden_reordered_and_deleted_a_step_each() {
    let mut e = editor();
    let layers = |e: &Editor| -> Vec<String> {
        let page = e.page(0).unwrap();
        page.layers().iter().map(|l| l.id.clone()).collect()
    };
    let top = e.add_layer(0, "Untitled Layer").unwrap();
    assert_eq!(layers(&e), ["1", top.as_str()]);
    assert_eq!(e.current_layer(0), Some(top.as_str()));
    let v = e.add_vertex(0, None, Rect::default(), "", "").unwrap();
    let ends = (Some("a"), Point::default());
    let f = e
        .add_edge(0, ends, (Some(&v), Point::default()), "")
        .unwrap();
    e.set_style(0, std::slice::from_ref(&top), "locked", Some("1"))
        .unwrap();
    e.set_visible(0, "1", false).unwrap();
    e.rename_layer(0, &top, "Top").unwrap();
    // Written as draw.io writes them, and read back.
    let read = File::from_bytes(e.file().to_xml().as_bytes()).unwrap();
    let page = &read.pages[0];
    assert!(!page.cell("1").unwrap().is_visible());
    let layer = page.cell(&top).unwrap();
    assert!(layer.is_locked() && layer.label() == "Top");
    assert_eq!(page.default_parent(), None);
    e.reorder(0, std::slice::from_ref(&top), ZOrder::Backward)
        .unwrap();
    assert_eq!(layers(&e), [top.as_str(), "1"]);
    // Its cells go with it, and the edge from a shape on another layer to one of them.
    e.delete_layer(0, &top).unwrap();
    assert_eq!(layers(&e), ["1"]);
    assert_eq!(order(&e), ["0", "1", "a", "b", "e"]);
    assert!(matches!(
        e.delete_layer(0, "1"),
        Err(Error::Refused("a page keeps at least one layer"))
    ));
    assert_eq!(e.undo.len(), 8, "one step each");
    e.undo();
    let page = e.page(0).unwrap();
    assert!(page.cell(&v).is_some() && page.cell(&f).is_some());
}

#[test]
fn a_vertex_in_a_group_is_stored_relative() {
    let mut e = editor();
    let group = Rect::new(100.0, 50.0, 200.0, 100.0);
    let g = e.add_vertex(0, None, group, "group", "").unwrap();
    let r = Rect::new(110.0, 70.0, 30.0, 40.0);
    let c = e.add_vertex(0, Some(&g), r, "", "").unwrap();
    assert_eq!(geometry(&e, &c).rect(), Rect::new(10.0, 20.0, 30.0, 40.0));
    assert_eq!(e.page(0).unwrap().absolute_rect(&c), Some(r));
    e.resize(0, &c, Rect::new(120.0, 60.0, 50.0, 50.0)).unwrap();
    assert_eq!(geometry(&e, &c).rect(), Rect::new(20.0, 10.0, 50.0, 50.0));
}

#[test]
fn an_edge_moved_on_its_own_lets_go_of_its_shapes_where_it_is_drawn() {
    let mut e = editor();
    let shown = scene(e.page(0).unwrap());
    let drawn = shown.route("e").unwrap().to_vec();
    e.move_cells(0, &list(&["e"]), 10.0, 5.0, &shown).unwrap();
    let cell = e.page(0).unwrap().cell("e").unwrap();
    assert_eq!(
        (cell.source.as_deref(), cell.target.as_deref()),
        (None, None)
    );
    let g = geometry(&e, "e");
    let shifted = |p: Point| Some(Point::new(p.x + 10.0, p.y + 5.0));
    assert_eq!(g.source_point, shifted(drawn[0]));
    assert_eq!(g.target_point, shifted(drawn[drawn.len() - 1]));
    assert_eq!(g.points, Some(vec![Point::new(80.0, 65.0)]));
    // Moved with one of its shapes, it keeps that end and lets go of the other.
    let mut e = editor();
    let drawn = scene(e.page(0).unwrap());
    e.move_cells(0, &list(&["a", "e"]), 10.0, 0.0, &drawn)
        .unwrap();
    let cell = e.page(0).unwrap().cell("e").unwrap();
    assert_eq!(
        (cell.source.as_deref(), cell.target.as_deref()),
        (Some("a"), None)
    );
}

#[test]
fn a_move_started_once_shifts_as_move_cells_does() {
    let page = editor().page(0).unwrap().clone();
    let ids = list(&["a", "e"]);
    let (mut started, mut whole) = (page.clone(), page.clone());
    let drawn = scene(&page);
    let moving = start_move(&mut started, &ids, &drawn).unwrap();
    assert_eq!(moving.count, 2);
    for (dx, dy) in [(10.0, 5.0), (-30.0, 20.0)] {
        let mut shifted = started.clone();
        moving.shift(&mut shifted, dx, dy);
        let mut moved = page.clone();
        move_cells(&mut moved, &ids, dx, dy, &drawn).unwrap();
        assert_eq!(shifted, moved);
    }
    assert!(matches!(
        start_move(&mut whole, &list(&["nope"]), &drawn),
        Err(Error::NoCell(_))
    ));
}

#[test]
fn an_end_pins_floats_or_dangles() {
    let mut e = editor();
    let pin = Constraint {
        point: Point::new(0.5, 0.0),
        dx: 0.0,
        dy: 0.0,
        perimeter: false,
    };
    e.set_end(0, "e", false, (Some("a"), Point::default()), Some(&pin))
        .unwrap();
    let cell = e.page(0).unwrap().cell("e").unwrap();
    assert_eq!(cell.target.as_deref(), Some("a"));
    let style = cell.style.to_string();
    assert!(
        style.contains("entryX=0.5") && style.contains("entryPerimeter=0"),
        "{style}"
    );
    // Floating on b: the constraint goes.
    e.set_end(0, "e", false, (Some("b"), Point::default()), None)
        .unwrap();
    let cell = e.page(0).unwrap().cell("e").unwrap();
    assert!(!cell.style.to_string().contains("entry"));
    // Dangling: the point is the end.
    e.set_end(0, "e", true, (None, Point::new(5.0, 6.0)), None)
        .unwrap();
    let cell = e.page(0).unwrap().cell("e").unwrap();
    assert_eq!(cell.source, None);
    assert_eq!(geometry(&e, "e").source_point, Some(Point::new(5.0, 6.0)));
    assert!(matches!(
        e.set_end(0, "a", true, (None, Point::default()), None),
        Err(Error::Refused(_))
    ));
    e.set_points(0, "e", &[Point::new(1.0, 2.0)]).unwrap();
    assert_eq!(geometry(&e, "e").points, Some(vec![Point::new(1.0, 2.0)]));
    e.set_points(0, "e", &[]).unwrap();
    assert_eq!(geometry(&e, "e").points, None);
}

#[test]
fn an_edge_label_moves_along_and_off_its_edge() {
    let mut e = editor();
    let route = [Point::new(0.0, 0.0), Point::new(100.0, 0.0)];
    // A quarter of the way along, 10 below the line.
    e.move_label(0, "e", &route, Point::new(25.0, 10.0))
        .unwrap();
    let g = geometry(&e, "e");
    assert_eq!((g.x, g.y, g.offset), (-0.5, -10.0, Some(Point::default())));
    assert_eq!(
        crate::scene::edge_label_at(&route, g),
        Point::new(25.0, 10.0)
    );
    // Beyond the end: at the end, as far across as it is away, the rest as offset.
    e.move_label(0, "e", &route, Point::new(130.0, 0.0))
        .unwrap();
    let g = geometry(&e, "e");
    assert_eq!((g.x, g.y), (1.0, 30.0));
    assert_eq!(g.offset, Some(Point::new(30.0, 30.0)));
    assert_eq!(
        crate::scene::edge_label_at(&route, g),
        Point::new(130.0, 0.0)
    );
}

#[test]
fn a_group_holds_its_cells_where_they_were_and_ungrouping_lets_them_go() {
    let mut e = editor();
    // One cell is not a group.
    assert_eq!(e.group(0, &list(&["a"])).unwrap(), None);
    let g = e.group(0, &list(&["b", "a", "e"])).unwrap().unwrap();
    let page = e.page(0).unwrap();
    assert_eq!(order(&e), ["0", "1", &g, "a", "b", "e"]);
    let group = page.cell(&g).unwrap();
    assert_eq!(group.style.to_string(), "group;");
    // Boxes a and b, and the edge's waypoint (70, 60): the group is sized to them.
    assert_eq!(geometry(&e, &g).rect(), Rect::new(0.0, 0.0, 140.0, 60.0));
    assert_eq!(
        e.page(0).unwrap().absolute_rect("b"),
        Some(Rect::new(100.0, 0.0, 40.0, 40.0))
    );
    // Moved as one, then ungrouped: the transparent group goes, the cells stay put.
    e.move_cells(0, &list(&[g.as_str()]), 10.0, 5.0, &Scene::default())
        .unwrap();
    let chosen = e.ungroup(0, std::slice::from_ref(&g)).unwrap();
    assert_eq!(chosen, list(&["a", "b", "e"]));
    assert!(e.page(0).unwrap().cell(&g).is_none());
    assert_eq!(geometry(&e, "b").rect(), Rect::new(110.0, 5.0, 40.0, 40.0));
    assert_eq!(geometry(&e, "e").points, Some(vec![Point::new(80.0, 65.0)]));
    // A group with a fill of its own stays, as a shape.
    let g = e.group(0, &list(&["a", "b"])).unwrap().unwrap();
    e.set_style(0, std::slice::from_ref(&g), "fillColor", Some("#ff0000"))
        .unwrap();
    let chosen = e.ungroup(0, std::slice::from_ref(&g)).unwrap();
    assert_eq!(chosen, list(&["a", "b", g.as_str()]));
    assert!(
        e.page(0)
            .unwrap()
            .cell(&g)
            .unwrap()
            .style
            .to_string()
            .contains("container=0")
    );
}

#[test]
fn a_paste_takes_fresh_ids_on_the_current_layer_and_a_cut_lets_edges_go() {
    let mut e = editor();
    let drawn = crate::scene::scene(e.page(0).unwrap());
    let xml = crate::clipboard::copy(e.page(0).unwrap(), &list(&["a", "e"]), &drawn);
    let from = crate::clipboard::pages_in(&xml);
    let pasted = e.paste(0, &from, 10.0, 10.0).unwrap();
    assert_eq!(pasted.len(), 2);
    let page = e.page(0).unwrap();
    let (a2, e2) = (
        page.cell(&pasted[0]).unwrap(),
        page.cell(&pasted[1]).unwrap(),
    );
    assert_eq!(a2.parent.as_deref(), Some("1"));
    assert_eq!(
        a2.geometry.as_ref().unwrap().rect(),
        Rect::new(10.0, 10.0, 40.0, 40.0)
    );
    // The pasted edge is on the pasted shape and let go of b, which was not copied.
    assert_eq!(
        (e2.source.as_ref(), e2.target.as_ref()),
        (Some(&pasted[0]), None)
    );
    assert_eq!(order(&e).len(), 7);
    // A file of two pages: the first onto the page, the second a page of its own.
    let file = format!(
        "<mxfile><diagram name=\"One\" id=\"x\">{xml}</diagram><diagram id=\"y\">{xml}</diagram></mxfile>"
    );
    let before = e.file().pages.len();
    let pasted = e
        .paste(0, &crate::clipboard::pages_in(&file), 0.0, 0.0)
        .unwrap();
    assert_eq!((pasted.len(), e.file().pages.len()), (2, before + 1));
    let added = &e.file().pages[before];
    assert_eq!(added.name(), "Page-2");
    assert_ne!(crate::model::attr(&added.attrs, "id"), Some("y"));
    assert!(e.undo());
    assert_eq!(e.file().pages.len(), before);
    // A cut keeps the edge, loose where it was drawn.
    let mut e = editor();
    let drawn = crate::scene::scene(e.page(0).unwrap());
    e.remove(0, &list(&["b"]), &drawn).unwrap();
    let cell = e.page(0).unwrap().cell("e").unwrap();
    assert_eq!(cell.target, None);
    let end = drawn.route("e").unwrap().last().copied();
    assert_eq!(geometry(&e, "e").target_point, end);
}

#[test]
fn move_takes_edges_between_moved_cells() {
    let mut e = editor();
    let c = e
        .add_vertex(0, None, Rect::new(0.0, 200.0, 40.0, 40.0), "", "")
        .unwrap();
    let f = e
        .add_edge(
            0,
            (Some("a"), Point::default()),
            (Some(&c), Point::default()),
            "",
        )
        .unwrap();
    let drawn = scene(e.page(0).unwrap());
    e.move_cells(0, &list(&["a", "b"]), 10.0, 5.0, &drawn)
        .unwrap();
    assert_eq!(geometry(&e, "a").rect(), Rect::new(10.0, 5.0, 40.0, 40.0));
    assert_eq!(geometry(&e, "b").x, 110.0);
    assert_eq!(geometry(&e, "e").points, Some(vec![Point::new(80.0, 65.0)]));
    assert_eq!(
        geometry(&e, &f).source_point,
        Some(Point::default()),
        "an edge with one end moved stays"
    );
    let group = Rect::new(300.0, 0.0, 100.0, 100.0);
    let g = e.add_vertex(0, None, group, "group", "").unwrap();
    let inner = Rect::new(310.0, 10.0, 10.0, 10.0);
    let inner = e.add_vertex(0, Some(&g), inner, "", "").unwrap();
    let drawn = scene(e.page(0).unwrap());
    e.move_cells(0, &[g.clone(), inner.clone()], 5.0, 0.0, &drawn)
        .unwrap();
    assert_eq!(geometry(&e, &g).x, 305.0);
    assert_eq!(
        geometry(&e, &inner).x,
        10.0,
        "a child moves with its group, not again"
    );
}

#[test]
fn delete_removes_connected_edges() {
    let mut e = editor();
    let mut label = Cell::new_vertex("l", "e", Rect::default(), "edgeLabel", "x");
    label.geometry.as_mut().unwrap().relative = true;
    e.file.pages[0].cells.push(label);
    e.delete(0, &list(&["0", "1"])).unwrap();
    assert!(!e.can_undo(), "the root and the layer stay");
    e.delete(0, &list(&["a"])).unwrap();
    assert_eq!(order(&e), ["0", "1", "b"]);
}

#[test]
fn duplicate_remaps_terminals_and_offsets() {
    let mut e = editor();
    let made = e.duplicate(0, &list(&["a", "b", "e"])).unwrap();
    let [a2, b2, e2] = &made[..] else {
        panic!("{made:?}")
    };
    assert_eq!(order(&e), ["0", "1", "a", a2, "b", b2, "e", e2]);
    let copy = e.page(0).unwrap().cell(e2).unwrap();
    assert_eq!(copy.source.as_ref(), Some(a2));
    assert_eq!(copy.target.as_ref(), Some(b2));
    assert_eq!(geometry(&e, e2).points, Some(vec![Point::new(80.0, 70.0)]));
    assert_eq!(geometry(&e, a2).rect(), Rect::new(10.0, 10.0, 40.0, 40.0));
    let alone = e.duplicate(0, &list(&["e"])).unwrap();
    let copy = e.page(0).unwrap().cell(&alone[0]).unwrap();
    assert_eq!(
        (copy.source.as_deref(), copy.target.as_deref()),
        (Some("a"), Some("b")),
        "an edge copied alone keeps its ends"
    );
}

#[test]
fn reorder_moves_among_siblings() {
    let tree = [("a", "1"), ("g", "1"), ("g1", "g"), ("g2", "g"), ("b", "1")];
    let mut e =
        open(tree.map(|(id, parent)| Cell::new_vertex(id, parent, Rect::default(), "", "")));
    e.reorder(0, &list(&["a"]), ZOrder::ToFront).unwrap();
    assert_eq!(order(&e), ["0", "1", "g", "g1", "g2", "b", "a"]);
    e.reorder(0, &list(&["a"]), ZOrder::Backward).unwrap();
    assert_eq!(order(&e), ["0", "1", "g", "g1", "g2", "a", "b"]);
    e.reorder(0, &list(&["g"]), ZOrder::Forward).unwrap();
    assert_eq!(order(&e), ["0", "1", "a", "g", "g1", "g2", "b"]);
    e.reorder(0, &list(&["b"]), ZOrder::ToBack).unwrap();
    assert_eq!(order(&e), ["0", "1", "b", "a", "g", "g1", "g2"]);
    e.reorder(0, &list(&["g2"]), ZOrder::ToBack).unwrap();
    assert_eq!(order(&e), ["0", "1", "b", "a", "g", "g2", "g1"]);
    e.reorder(0, &list(&["a", "b"]), ZOrder::ToFront).unwrap();
    assert_eq!(order(&e), ["0", "1", "g", "g2", "g1", "b", "a"]);
}

#[test]
fn the_last_page_cannot_be_deleted() {
    let mut e = editor();
    assert!(matches!(e.delete_page(0), Err(Error::Refused(_))));
    assert!(matches!(e.delete_page(3), Err(Error::NoPage(3))));
    assert_eq!(e.file().pages.len(), 1);
}

#[test]
fn page_ops_undo() {
    let mut e = editor();
    let before = e.file().pages.clone();
    assert_eq!(e.add_page("Two"), 1);
    e.rename_page(1, "Second").unwrap();
    e.set_page_attr(0, "background", Some("#ff0000")).unwrap();
    assert_eq!(e.page(1).unwrap().name(), "Second");
    assert_eq!(e.page(0).unwrap().model_attr("background"), Some("#ff0000"));
    e.delete_page(1).unwrap();
    assert_eq!(e.file().pages.len(), 1);
    while e.undo() {}
    assert_eq!(e.file().pages, before);
}

#[test]
fn failed_edits_leave_no_undo_step() {
    let mut e = editor();
    let r = e.move_cells(0, &list(&["a", "nope"]), 1.0, 1.0, &Scene::default());
    assert!(matches!(r, Err(Error::NoCell(_))));
    assert_eq!(geometry(&e, "a").x, 0.0);
    let r = e.set_style(2, &list(&["a"]), "rounded", None);
    assert!(matches!(r, Err(Error::NoPage(2))));
    let r = e.resize(0, "e", Rect::default());
    assert!(matches!(r, Err(Error::Refused(_))));
    let r = e.add_vertex(0, Some("nope"), Rect::default(), "", "");
    assert!(matches!(r, Err(Error::NoCell(_))));
    e.file.pages[0].cells.truncate(1);
    let r = e.add_vertex(0, None, Rect::default(), "", "");
    assert!(matches!(r, Err(Error::Refused(_))), "no layer to go in");
    assert!(!e.can_undo() && !e.dirty());
}
