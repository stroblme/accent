//! The `scene` module's tests.

use super::*;
use crate::model::{Cell, File};

fn page(cells: Vec<Cell>) -> Page {
    let mut p = Page::blank("P", "p");
    p.cells.extend(cells);
    p
}

fn cells_in_order(s: &Scene) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for prim in &s.prims {
        if out.last() != Some(&prim.cell()) {
            out.push(prim.cell());
        }
    }
    out
}

fn text(s: &Scene, id: &str) -> (Rect, Point, bool) {
    s.prims
        .iter()
        .find_map(|p| match p {
            Prim::Text {
                cell,
                rect,
                anchor,
                wrap,
                ..
            } if cell == id => Some((*rect, *anchor, *wrap)),
            _ => None,
        })
        .expect("a label")
}

#[test]
fn paint_order_is_layers_then_documents_then_children() {
    let mut p = page(vec![
        Cell::new_vertex("a", "1", Rect::new(0.0, 0.0, 10.0, 10.0), "", ""),
        Cell::new_vertex("g", "1", Rect::new(100.0, 100.0, 50.0, 50.0), "", ""),
        Cell::new_vertex("child", "g", Rect::new(5.0, 5.0, 10.0, 10.0), "", ""),
        Cell::new_vertex("b", "2", Rect::new(0.0, 0.0, 10.0, 10.0), "", ""),
    ]);
    p.cells.insert(2, Cell::layer("2", "0"));
    let s = scene(&p);
    assert_eq!(cells_in_order(&s), ["a", "g", "child", "b"]);
}

#[test]
fn children_of_a_group_are_placed_from_it_and_a_group_takes_no_clicks() {
    let p = page(vec![
        Cell::new_vertex("g", "1", Rect::new(100.0, 50.0, 50.0, 50.0), "group", ""),
        Cell::new_vertex("c", "g", Rect::new(10.0, 20.0, 30.0, 10.0), "", ""),
    ]);
    let s = scene(&p);
    assert_eq!(cells_in_order(&s), ["c"], "an empty group paints nothing");
    let bounds = s.bounds_of("c").unwrap();
    assert!(
        (bounds.x - 109.5).abs() < 1e-9 && (bounds.y - 69.5).abs() < 1e-9,
        "{bounds:?}"
    );
}

#[test]
fn labels_sit_where_draw_io_puts_them() {
    let p = page(vec![
        Cell::new_vertex(
            "mid",
            "1",
            Rect::new(0.0, 0.0, 120.0, 60.0),
            "whiteSpace=wrap;",
            "A",
        ),
        Cell::new_vertex("top", "1", Rect::new(0.0, 100.0, 120.0, 60.0), "text;", "B"),
        Cell::new_vertex(
            "img",
            "1",
            Rect::new(0.0, 200.0, 40.0, 40.0),
            "image;image=x.png;",
            "C",
        ),
    ]);
    let s = scene(&p);
    // Centre/middle: the anchor is the centre and the box loses 2 of spacing on each side.
    let (rect, anchor, wrap) = text(&s, "mid");
    assert!(wrap);
    assert_eq!(anchor, Point::new(60.0, 30.0));
    assert_eq!(rect, Rect::new(2.0, 2.0, 116.0, 56.0));
    // A text cell is left/top: 2 of spacing in, and draw.io's 5 more above.
    let (_, anchor, wrap) = text(&s, "top");
    assert!(!wrap);
    assert_eq!(anchor, Point::new(2.0, 100.0 + 2.0 + 5.0));
    // A picture's label hangs under it.
    let (_, anchor, _) = text(&s, "img");
    assert_eq!(anchor, Point::new(20.0, 240.0 + 2.0 + 5.0));
}

#[test]
fn label_text_is_set_where_draw_ios_canvas_sets_it() {
    let cell = |id: &str, y, style: &str, label: &str| {
        Cell::new_vertex(id, "1", Rect::new(0.0, y, 120.0, 60.0), style, label)
    };
    let p = page(vec![
        cell("html", 0.0, "html=1;", "A"),
        cell("wrap", 100.0, "whiteSpace=wrap;", "B"),
        cell("svg", 200.0, "", "C\nD"),
    ]);
    let s = scene(&p);
    let origin = |id: &str, block, tops: &[f64]| {
        let mut label = s.prims.iter().filter(|p| p.cell() == id);
        label.find_map(|p| p.text_origin(block, 11.0, tops, (11.0, 3.0)))
    };
    // An HTML label's block is centred on a box a unit square at its anchor, (60, 30), its
    // lines drawn down it as the next test has them.
    let at = origin("html", (30.0, 15.0), &[0.0]).map(|(at, _)| at);
    assert_eq!(at, Some(Point::new(60.0 + 0.5 - 15.0, 30.0 + 0.5 - 7.5)));
    // Wrapped, its lines are aligned across its room and 2 more, from that box's left.
    let at = origin("wrap", (30.0, 15.0), &[0.0]).map(|(at, _)| at);
    assert_eq!(at, Some(Point::new(60.0 - 118.0 / 2.0, 130.0 + 0.5 - 7.5)));
    // SVG text's 12 px lines are 14 apart, the first baseline 11 below the anchor less half
    // of 12 + 14.
    let (x, y) = (60.0 - 15.0, 230.0 + 11.0 - 13.0 - 11.0);
    let at = origin("svg", (30.0, 28.0), &[0.0, 14.0]);
    assert_eq!(at, Some((Point::new(x, y), vec![0.0])));
}

#[test]
fn an_html_label_s_lines_are_set_on_whole_units_as_chromium_sets_them() {
    // Liberation Sans, and where draw.io desktop's exports put its baselines below the
    // anchor of a middle-aligned label: one line's at 12, 16 and 24 px, three at 12 px.
    let set = |size: f64, tops: &[f64]| {
        let style = format!("html=1;fontSize={size};");
        let cell = Cell::new_vertex("a", "1", Rect::new(0.0, 0.0, 160.0, 100.0), &style, "A");
        let s = scene(&page(vec![cell]));
        let metrics = (size * 1854.0 / 2048.0, size * 434.0 / 2048.0);
        let line = line_height(size, true);
        // Pango's first baseline, half the leading above the ascent.
        let baseline = (line - metrics.0 - metrics.1) / 2.0 + metrics.0;
        let block = (30.0, tops.len() as f64 * line);
        let origin = |p: &Prim| p.text_origin(block, baseline, tops, metrics);
        let (at, downs) = s.prims.iter().find_map(origin).expect("a label");
        let drawn = |(top, down): (&f64, f64)| at.y + down + top + baseline - 50.0;
        tops.iter().zip(downs).map(drawn).collect::<Vec<_>>()
    };
    for (size, below) in [(12.0, 4.0), (16.0, 6.0), (24.0, 8.0)] {
        assert!((set(size, &[0.0])[0] - below).abs() < 1e-9, "{size} px");
    }
    // Pango's lines are evenly spaced, and each is drawn down onto Chromium's.
    let line = line_height(12.0, true);
    let three = set(12.0, &[0.0, line, 2.0 * line]);
    for (at, chromium) in three.iter().zip([-10.0, 4.0, 19.0]) {
        assert!((at - chromium).abs() < 1e-9, "{three:?}");
    }
}

#[test]
fn an_odd_pixel_wide_line_is_drawn_half_a_pixel_over() {
    let offset = |width, pixels| {
        let stroked = Snap {
            width,
            stroked: true,
        };
        stroked.offset(pixels, false)
    };
    // A 1 unit line at 100 % and at 300 %, at 200 %, and a 2 unit one at 50 %.
    assert_eq!(offset(1.0, 1.0), 0.5);
    assert_eq!(offset(1.0, 3.0), 0.5);
    assert_eq!(offset(1.0, 2.0), 0.0);
    assert_eq!(offset(2.0, 0.5), 0.5);
    // A hairline is a pixel wide at the least, a line 0 wide too.
    assert_eq!(offset(0.2, 1.0), 0.5);
    assert_eq!(offset(0.0, 1.0), 0.5);
}

#[test]
fn an_svg_moves_only_a_shape_with_a_line() {
    let snap = |width, stroked| Snap { width, stroked };
    // On screen any shape moves by its width, in an SVG only one with a line.
    assert_eq!(snap(1.0, false).offset(1.0, false), 0.5);
    assert_eq!(snap(1.0, false).offset(1.0, true), 0.0);
    assert_eq!(snap(1.0, true).offset(1.0, true), 0.5);
    assert_eq!(snap(0.0, true).offset(1.0, true), 0.0);
    assert_eq!(snap(2.0, true).offset(1.0, true), 0.0);
}

#[test]
fn a_line_is_a_tenth_of_a_pixel_wide_at_the_least() {
    use super::line_width;
    assert_eq!(line_width(0.0, 1.0), 0.1);
    assert_eq!(line_width(0.0, 2.0), 0.05);
    assert_eq!(line_width(0.05, 1.0), 0.1);
    assert_eq!(line_width(1.0, 1.0), 1.0);
}

#[test]
fn a_label_whose_style_skips_the_stylesheet_takes_draw_ios_own_defaults() {
    let font = |style: &str| {
        let r = Rect::new(0.0, 0.0, 40.0, 20.0);
        let p = page(vec![Cell::new_vertex("a", "1", r, style, "A")]);
        scene(&p)
            .prims
            .iter()
            .find_map(|prim| match prim {
                Prim::Text { font, .. } => Some(font.clone()),
                _ => None,
            })
            .expect("a label")
    };
    let bare = font(";");
    assert_eq!(
        (bare.size, bare.family.as_str()),
        (DEFAULT_FONTSIZE, DEFAULT_FONTFAMILY)
    );
    let sheet = font("rounded=1;");
    assert_eq!((sheet.size, sheet.family.as_str()), (12.0, "Helvetica"));
}

#[test]
fn each_connector_preset_is_drawn_as_draw_io_s_waypoints_of_that_name() {
    use crate::style::presets::{EdgeKind, edge};
    let drawn = |kind| {
        let (a, b) = (Point::new(0.0, 0.0), Point::new(100.0, 60.0));
        let e = Cell::new_edge("e", "1", (None, a), (None, b), &edge(kind, true));
        let s = scene(&page(vec![e]));
        let curved = s.prims.iter().any(|p| match p {
            Prim::Path { path, .. } => path
                .iter()
                .any(|c| matches!(c, PathCmd::QuadTo(..) | PathCmd::CurveTo(..))),
            _ => false,
        });
        let bent = s.route("e").is_some_and(|r| r.len() > 2);
        (bent, curved)
    };
    assert_eq!(drawn(EdgeKind::Straight), (false, false));
    assert_eq!(drawn(EdgeKind::Orthogonal), (true, false));
    // draw.io's Curved is the orthogonal route smoothed (Menus.js `edgeStyle`).
    assert_eq!(drawn(EdgeKind::Curved), (true, true));
}

#[test]
fn hidden_cells_and_their_children_are_not_painted() {
    let mut hidden = Cell::new_vertex("h", "1", Rect::new(0.0, 0.0, 10.0, 10.0), "", "");
    hidden.attrs.push(("visible".into(), "0".into()));
    let p = page(vec![
        hidden,
        Cell::new_vertex("under", "h", Rect::new(0.0, 0.0, 5.0, 5.0), "", ""),
        Cell::new_vertex("shown", "1", Rect::new(0.0, 0.0, 10.0, 10.0), "", ""),
    ]);
    assert_eq!(cells_in_order(&scene(&p)), ["shown"]);
}

#[test]
fn rotation_is_applied_to_the_outline_and_the_label_turns_with_it() {
    let p = page(vec![Cell::new_vertex(
        "r",
        "1",
        Rect::new(0.0, 0.0, 100.0, 20.0),
        "rotation=90;",
        "R",
    )]);
    let s = scene(&p);
    let b = s.prims[0].bounds();
    assert!(
        (b.w - 21.0).abs() < 1e-6 && (b.h - 101.0).abs() < 1e-6,
        "{b:?}"
    );
    match &s.prims[1] {
        Prim::Text { rotation, .. } => assert_eq!(*rotation, 90.0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_locked_layer_locks_what_is_on_it() {
    let mut p = page(vec![Cell::new_vertex(
        "a",
        "1",
        Rect::new(0.0, 0.0, 10.0, 10.0),
        "",
        "",
    )]);
    p.cells[1].style = crate::style::Style::parse("locked=1;");
    assert!(scene(&p).prims.iter().all(Prim::locked));
}

#[test]
fn an_edge_runs_between_its_shapes_with_its_head_on_top_and_its_label_midway() {
    let mut edge = Cell::new_edge(
        "e",
        "1",
        (Some("a"), Point::default()),
        (Some("b"), Point::default()),
        "edgeStyle=none;endArrow=block;",
    );
    edge.set_label("go");
    let p = page(vec![
        Cell::new_vertex("a", "1", Rect::new(0.0, 0.0, 40.0, 40.0), "", ""),
        Cell::new_vertex("b", "1", Rect::new(200.0, 0.0, 40.0, 40.0), "", ""),
        edge,
    ]);
    let s = scene(&p);
    let route = s.route("e").expect("a route");
    assert!(
        route.len() == 2 && route[0].distance(Point::new(40.0, 20.0)) < 1e-9,
        "{route:?}"
    );
    assert!(
        route[1].distance(Point::new(200.0, 20.0)) < 1e-9,
        "the route runs to the outline, the head's room not taken off: {route:?}"
    );
    assert_eq!(s.route("a"), None);
    let edge: Vec<&Prim> = s.prims.iter().filter(|p| p.cell() == "e").collect();
    let (line, head) = match (edge[0], edge[1]) {
        (
            Prim::Path { path: line, .. },
            Prim::Path {
                path: head, fill, ..
            },
        ) => {
            assert!(fill.is_some(), "a block head is filled");
            (line, head)
        }
        other => panic!("{other:?}"),
    };
    let line = geom::path_bounds(line).unwrap();
    let head = geom::path_bounds(head).unwrap();
    assert_eq!(line.x, 40.0, "leaves the right side of a");
    assert!(
        line.right() < 200.0 && head.right() <= 200.0 + 1e-9,
        "stops short for its head"
    );
    match edge[2] {
        Prim::Text { anchor, .. } => assert!((anchor.x - (40.0 + 200.0) / 2.0).abs() < 1.0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_label_moves_along_its_edge_and_off_it() {
    let line = [Point::new(0.0, 0.0), Point::new(100.0, 0.0)];
    assert_eq!(
        point_along(&line, 0.0, 0.0, Point::default()),
        Point::new(50.0, 0.0)
    );
    assert_eq!(
        point_along(&line, -1.0, 0.0, Point::default()),
        Point::new(0.0, 0.0)
    );
    // Positive `y` is to the left of the way the edge runs: up, for one running right.
    assert_eq!(
        point_along(&line, 0.5, 10.0, Point::new(1.0, 2.0)),
        Point::new(76.0, -8.0)
    );
}

#[test]
fn a_dash_pattern_keeps_only_lengths_a_line_can_be_drawn_with() {
    let style = crate::style::Style::parse("dashed=1;dashPattern=1e999 -2 NaN 3;");
    let dash = stroke(&style.resolve(false)).unwrap().dash;
    assert_eq!(dash, Some(vec![3.0]));
}

#[test]
fn a_thin_line_has_its_dashes_in_its_own_width() {
    let dash = |s: &str| {
        stroke(&crate::style::Style::parse(s).resolve(false))
            .unwrap()
            .dash
    };
    assert_eq!(dash("dashed=1;strokeWidth=0.5;"), Some(vec![1.5, 1.5]));
    assert_eq!(
        dash("dashed=1;strokeWidth=0.5;fixDash=1;"),
        Some(vec![3.0, 3.0])
    );
}

#[test]
fn an_edge_takes_the_line_the_properties_pane_gives_it_its_head_undashed() {
    let mut p = page(vec![Cell::new_edge(
        "e",
        "1",
        (None, Point::default()),
        (None, Point::new(100.0, 0.0)),
        "edgeStyle=none;",
    )]);
    let line = [
        ("strokeColor", Some("#ff0000")),
        ("strokeWidth", Some("3")),
        ("dashed", Some("1")),
    ];
    crate::edit::set_styles(&mut p, &["e".to_string()], &line).unwrap();
    let s = scene(&p);
    let strokes: Vec<&Stroke> = s
        .prims
        .iter()
        .filter_map(|p| match p {
            Prim::Path {
                stroke: Some(s), ..
            } => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(strokes.len(), 2, "the line and its classic head");
    let red = Color::rgb(255, 0, 0);
    assert!(strokes.iter().all(|s| s.width == 3.0 && s.color == red));
    assert_eq!(strokes[0].dash, Some(vec![9.0, 9.0]), "in widths");
    assert_eq!(strokes[1].dash, None);
}

#[test]
fn a_comic_cell_wobbles_where_it_is_and_draws_its_lines_round() {
    let drawn = |dx: f64, style: &str| {
        let rect = Rect::new(dx, 0.0, 100.0, 40.0);
        let edge = Cell::new_edge(
            "e",
            "1",
            (None, Point::new(dx, 100.0)),
            (None, Point::new(dx + 100.0, 100.0)),
            &format!("edgeStyle=none;{style}"),
        );
        let s = scene(&page(vec![
            Cell::new_vertex("a", "1", rect, style, ""),
            edge,
        ]));
        let paths: Vec<(Vec<PathCmd>, Option<Stroke>)> = s
            .prims
            .into_iter()
            .filter_map(|p| match p {
                Prim::Path { path, stroke, .. } => Some((path, stroke)),
                _ => None,
            })
            .collect();
        paths
    };
    let comic = "sketch=1;sketchStyle=comic;";
    let (plain, drawn_by_hand) = (drawn(0.0, ""), drawn(0.0, comic));
    assert_eq!(drawn_by_hand.len(), 3, "the box, the line and its head");
    for ((plain, _), (by_hand, stroke)) in plain.iter().zip(&drawn_by_hand) {
        assert!(by_hand.len() > plain.len());
        let stroke = stroke.as_ref().unwrap();
        assert_eq!((stroke.cap, stroke.join), (Cap::Round, Join::Round));
    }
    assert_eq!(drawn_by_hand, drawn(0.0, comic), "the same every time");
    // Moved, it wobbles as it did where it was.
    for ((here, _), (there, _)) in drawn_by_hand.iter().zip(&drawn(50.0, comic)) {
        let (a, b) = (
            geom::path_bounds(here).unwrap(),
            geom::path_bounds(there).unwrap(),
        );
        assert_eq!(here.len(), there.len());
        assert!((a.x + 50.0 - b.x).abs() < 1e-9 && (a.w - b.w).abs() < 1e-9 && a.h == b.h);
    }
}

#[test]
fn a_cell_wobbles_by_hand_where_it_paints_a_straight_piece() {
    let shape = |id, x, style| Cell::new_vertex(id, "1", Rect::new(x, 0.0, 80.0, 40.0), style, "");
    let edge = |id, style| {
        let (from, to) = (Point::new(0.0, 200.0), Point::new(100.0, 250.0));
        Cell::new_edge(id, "1", (None, from), (None, to), style)
    };
    let s = scene(&page(vec![
        shape("box", 0.0, ""),
        shape("oval", 100.0, "ellipse;"),
        Cell::new_vertex(
            "text",
            "1",
            Rect::new(200.0, 0.0, 80.0, 40.0),
            "text;",
            "Hi",
        ),
        shape("group", 300.0, "group"),
        edge("curve", "curved=1;endArrow=none;"),
        edge("headed", "curved=1;"),
    ]));
    let ids = ["box", "oval", "text", "group", "curve", "headed"];
    let by_hand: Vec<&str> = ids.into_iter().filter(|id| s.wobbles(id)).collect();
    assert_eq!(by_hand, ["box", "headed"], "the curve's head is lines");
}

#[test]
fn a_gradient_runs_its_direction_across_the_outline_it_fills() {
    let fill = |cell: Cell| match &scene(&page(vec![cell])).prims[0] {
        Prim::Path { fill, .. } => fill.clone().expect("a fill"),
        other => panic!("{other:?}"),
    };
    let colours = "fillColor=#ffffff;gradientColor=#000000;";
    // A level arrow's route has no height; draw.io's SVG spans each outline's own box, so
    // the default south still runs across the band, top to bottom.
    let arrow = Cell::new_edge(
        "e",
        "1",
        (None, Point::new(0.0, 50.0)),
        (None, Point::new(200.0, 50.0)),
        &format!("shape=flexArrow;endArrow=none;{colours}"),
    );
    match fill(arrow) {
        Paint::Linear { start, end, .. } => {
            assert_eq!((start.x, start.y, end.y), (end.x, 45.0, 55.0));
        }
        other => panic!("{other:?}"),
    }
    let r = Rect::new(0.0, 0.0, 100.0, 40.0);
    let shape = |direction: &str| {
        let style = format!("{colours}gradientDirection={direction};");
        fill(Cell::new_vertex("v", "1", r, &style, ""))
    };
    match shape("west") {
        Paint::Linear { start, end, .. } => {
            assert_eq!(
                (start, end),
                (Point::new(100.0, 20.0), Point::new(0.0, 20.0))
            );
        }
        other => panic!("{other:?}"),
    }
    match shape("radial") {
        Paint::Radial { centre, radii, .. } => {
            assert_eq!((centre, radii), (Point::new(50.0, 20.0), (50.0, 20.0)));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_shape_faces_its_direction_and_mirrors_with_its_flips() {
    let tip_at = |style: &str, tip: Point| {
        let r = Rect::new(0.0, 0.0, 80.0, 40.0);
        let p = page(vec![Cell::new_vertex("t", "1", r, style, "")]);
        match &scene(&p).prims[0] {
            Prim::Path { path, .. } => path.iter().any(|c| match c {
                PathCmd::MoveTo(p) | PathCmd::LineTo(p) => p.distance(tip) < 1e-9,
                _ => false,
            }),
            other => panic!("{other:?}"),
        }
    };
    assert!(tip_at("triangle;", Point::new(80.0, 20.0)));
    assert!(tip_at("triangle;direction=south;", Point::new(40.0, 40.0)));
    assert!(tip_at("triangle;flipH=1;", Point::new(0.0, 20.0)));
    assert!(tip_at(
        "triangle;direction=north;flipV=1;",
        Point::new(40.0, 40.0)
    ));
}

#[test]
fn a_picture_is_placed_as_its_shape_is() {
    let placed = |style: &str| {
        let r = Rect::new(0.0, 0.0, 80.0, 40.0);
        let p = page(vec![Cell::new_vertex("i", "1", r, style, "")]);
        scene(&p).prims.into_iter().find_map(|prim| match prim {
            Prim::Image {
                rect,
                rotation,
                flip_h,
                flip_v,
                ..
            } => Some((rect, rotation, flip_h, flip_v)),
            _ => None,
        })
    };
    let r = Rect::new(0.0, 0.0, 80.0, 40.0);
    assert_eq!(
        placed("shape=image;image=x.png;flipH=1;"),
        Some((r, 0.0, true, false))
    );
    // Facing south it is drawn 40 by 80 and turned a quarter, its flips swapped.
    assert_eq!(
        placed("shape=image;image=x.png;flipH=1;direction=south;"),
        Some((Rect::new(20.0, -20.0, 40.0, 80.0), 90.0, false, true))
    );
}

#[test]
fn a_picture_takes_the_fill_opacity_its_border_the_line_s_and_its_label_the_text_s() {
    let style = "shape=image;image=x.png;imageBackground=#ff0000;imageBorder=#0000ff;\
                 fillOpacity=50;strokeOpacity=40;textOpacity=60;opacity=80;";
    let r = Rect::new(0.0, 0.0, 80.0, 40.0);
    let s = scene(&page(vec![Cell::new_vertex("i", "1", r, style, "A")]));
    let alphas: Vec<(u8, f64)> = s
        .prims
        .iter()
        .filter_map(|prim| match prim {
            Prim::Path {
                fill: Some(Paint::Solid(c)),
                opacity,
                ..
            } => Some((c.a, *opacity)),
            Prim::Path {
                stroke: Some(line),
                opacity,
                ..
            } => Some((line.color.a, *opacity)),
            Prim::Image { opacity, .. } | Prim::Text { opacity, .. } => Some((255, *opacity)),
            _ => None,
        })
        .collect();
    // The label is faded by `textOpacity` alone, not by the cell's `opacity`.
    assert_eq!(alphas, [(128, 0.8), (255, 0.4), (102, 0.8), (255, 0.6)]);
}

#[test]
fn a_label_keeps_clear_of_a_bounded_shape_and_lies_across_a_turned_one() {
    let r = Rect::new(0.0, 0.0, 100.0, 60.0);
    let doc = page(vec![Cell::new_vertex(
        "d",
        "1",
        r,
        "shape=document;boundedLbl=1;",
        "D",
    )]);
    assert_eq!(
        text(&scene(&doc), "d").1,
        Point::new(50.0, 21.0),
        "above the wave"
    );
    // `horizontal=0` wraps across the height and turns the text a quarter back.
    let lane = page(vec![Cell::new_vertex(
        "s",
        "1",
        Rect::new(0.0, 0.0, 200.0, 100.0),
        "swimlane;horizontal=0;startSize=30;whiteSpace=wrap;",
        "S",
    )]);
    let s = scene(&lane);
    let (rect, anchor, wrap) = text(&s, "s");
    assert!(wrap && (rect.w - 96.0).abs() < 1e-9, "{rect:?}");
    assert!(
        anchor.distance(Point::new(15.0, 50.0)) < 1e-9,
        "in the title: {anchor:?}"
    );
    match s.prims.iter().find(|p| matches!(p, Prim::Text { .. })) {
        Some(Prim::Text { rotation, .. }) => assert_eq!(*rotation, -90.0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_shape_not_ported_yet_is_a_dashed_box() {
    let p = page(vec![Cell::new_vertex(
        "s",
        "1",
        Rect::new(0.0, 0.0, 10.0, 10.0),
        "shape=mxgraph.aws4.lambda;",
        "",
    )]);
    match &scene(&p).prims[0] {
        Prim::Path { stroke, .. } => assert!(stroke.as_ref().unwrap().dash.is_some()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_stencil_is_drawn_as_its_program_says() {
    // A 10 by 10 stencil stroking a box over its top half, as `Graph.compress` writes it.
    let style = "shape=stencil(UzV2Ls5ILEhVNTIoVzV2UTUyMjQAEkBuBjLX2FXV2Dktvyg1vSi/NC8Fwi9KTS4BKqyAKIRqq0ThYTfTFMxzg5hSXFKUn50K5wIZ6PYAhSBuNHYFAA==);strokeColor=#ff0000;flipV=1;";
    let p = page(vec![Cell::new_vertex(
        "s",
        "1",
        Rect::new(0.0, 0.0, 20.0, 40.0),
        style,
        "",
    )]);
    let s = scene(&p);
    let [
        Prim::Path {
            path, fill, stroke, ..
        },
    ] = s.prims.as_slice()
    else {
        panic!("{:?}", s.prims)
    };
    assert_eq!(
        geom::path_bounds(path),
        Some(Rect::new(0.0, 20.0, 20.0, 20.0)),
        "stretched over the cell and mirrored into its lower half"
    );
    let stroke = stroke.as_ref().unwrap();
    assert_eq!(
        (fill, stroke.color, &stroke.dash),
        (&None, Color::rgb(255, 0, 0), &None)
    );
    // It takes a click on what it paints and none in the empty half of its box.
    assert_eq!(s.hit(Point::new(10.0, 30.0), 1.0), Some("s"));
    assert_eq!(s.hit(Point::new(10.0, 10.0), 1.0), None);
}

#[test]
fn a_shape_is_put_on_whole_pixels_by_one_line_width() {
    let snaps = |style: &str, w: f64, h: f64| {
        let p = page(vec![Cell::new_vertex(
            "s",
            "1",
            Rect::new(0.0, 0.0, w, h),
            style,
            "",
        )]);
        let s = scene(&p);
        let snap = |p: &Prim| match p {
            Prim::Path { stroke, snap, .. } => {
                Some((stroke.as_ref().map(|s| s.width), snap.map(|s| s.width)))
            }
            _ => None,
        };
        s.prims.iter().filter_map(snap).collect::<Vec<_>>()
    };
    // A 10 by 10 box at a stencil's own width, 1, drawn one and a half times its size.
    let own = "shape=stencil(UzV2Ls5ILEhVNTIoVzV2UTUyMjQAEkBuBjLX2FXV2Dktvyg1vSi/NC8Fwi9KTS4BKqyAKIRqq0Th4TPTyA1iTHFJUX52KpwLZKBbBBSCONLYFQA=);";
    assert_eq!(snaps(own, 15.0, 15.0), [(Some(1.5), Some(1.0))]);
    // Two boxes of a stencil that inherits the cell's width, the second set 1 wide at twice
    // its size.
    let inherits = "shape=stencil(rZBBDoQgDEVP0z3QI6hzj8lYhZjIpGLQ21tTEsW1G8Lrb/8vADaL//4JnMmALTjnjByCXtEWXBLHiXLoUxHC7IlDOlXsAJshMo0c17lXZvqJaDbtLi57RbmOqBM/aqO5D9Q1xOBax96H6mz7Vrhcnq+Ukv4fdgc=);strokeWidth=1.5;";
    let both = [(Some(1.5), Some(1.5)), (Some(2.0), Some(1.5))];
    assert_eq!(snaps(inherits, 40.0, 20.0), both);
    // An unstroked shape by the width it would be stroked with.
    let unstroked = "strokeColor=none;strokeWidth=2;";
    assert_eq!(snaps(unstroked, 20.0, 20.0), [(None, Some(2.0))]);
}

#[test]
fn a_stencil_s_text_and_pictures_move_with_its_lines() {
    // A box with a letter and a picture in it, at a line width of its own, deflated into the
    // style as `Graph.compress` writes it.
    let xml = r#"<shape w="10" h="10" strokewidth="3"><foreground><rect x="0" y="0" w="10" h="10"/><fillstroke/><text str="A" x="5" y="5"/><image src="data:image/png,AA" x="0" y="0" w="5" h="5"/></foreground></shape>"#;
    let packed = crate::base64::encode(&miniz_oxide::deflate::compress_to_vec(xml.as_bytes(), 6));
    let snaps = |style: &str| {
        let style = format!("shape=stencil({packed});{style}");
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let p = page(vec![Cell::new_vertex("s", "1", r, &style, "")]);
        let snap = |p: &Prim| match p {
            Prim::Path { snap, .. } | Prim::Text { snap, .. } | Prim::Image { snap, .. } => *snap,
        };
        scene(&p).prims.iter().map(snap).collect::<Vec<_>>()
    };
    let own = Some(Snap {
        width: 3.0,
        stroked: true,
    });
    assert_eq!(snaps(""), [own; 3]);
    // A cell with no line keeps its stencil's width but moves in no SVG.
    let unstroked = own.map(|s| Snap {
        stroked: false,
        ..s
    });
    assert_eq!(snaps("strokeColor=none;"), [unstroked; 3]);
    // A label stays where it is set.
    let r = Rect::new(0.0, 0.0, 10.0, 10.0);
    let p = page(vec![Cell::new_vertex("t", "1", r, "", "Label")]);
    let label = scene(&p).prims.into_iter().find_map(|p| match p {
        Prim::Text { snap, .. } => Some(snap),
        _ => None,
    });
    assert_eq!(label, Some(None));
}

/// `depth` boxes of `style` on one page, each inside the one before it, as a file holds them. The
/// cells are flat in the XML, so its nesting cap leaves their parents as deep as the file has cells.
fn nested(depth: usize, style: &str) -> Page {
    use std::fmt::Write;
    let mut xml =
        String::from(r#"<mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>"#);
    for i in 2..depth + 2 {
        let parent = i - 1;
        let geometry = r#"<mxGeometry x="1" y="1" width="10" height="10" as="geometry"/>"#;
        write!(
            xml,
            r#"<mxCell id="{i}" parent="{parent}" style="{style}" vertex="1">{geometry}</mxCell>"#
        )
        .unwrap();
    }
    let xml = xml + "</root></mxGraphModel>";
    File::from_bytes(xml.as_bytes()).unwrap().pages.remove(0)
}

/// Boxes nested deeper than a recursion over them has stack for are drawn, resized with what they
/// hold, and as groups with `transparentBounds=1` measured by what they hold. A recursion ran out of
/// a test thread's stack below 5,000 boxes drawn, 1,000 resized and 5,000 measured, and 20,000 take
/// a file of 2 MB. Running out of stack aborts the whole process, so the work runs in this test
/// binary started again as a child.
#[test]
fn boxes_nested_deeper_than_a_stack_holds_are_drawn_resized_and_measured() {
    const CHILD: &str = "ACCENT_TEST_DEEP_NESTING";
    if std::env::var_os(CHILD).is_some() {
        let drawn = scene(&nested(20_000, "")).prims.len();
        // A resize and a measure cost more a box in a debug build, so their chains are shorter.
        let mut page = nested(2_000, "");
        crate::edit::resize(&mut page, "2", Rect::new(1.0, 1.0, 20.0, 20.0), false).unwrap();
        let inner = page.cells.last().and_then(|c| c.geometry.as_ref()).unwrap();
        let mut groups = nested(5_000, "group;transparentBounds=1;");
        let leaf = Rect::new(0.0, 0.0, 10.0, 10.0);
        groups
            .cells
            .push(Cell::new_vertex("leaf", "5001", leaf, "", ""));
        let measured = crate::edit::bounds(&groups, groups.cell("2").unwrap()).unwrap();
        let said = format!(
            "drawn {drawn} innermost {} measured {}",
            inner.width, measured.x
        );
        return println!("\n{said}");
    }
    let this =
        "scene::tests::boxes_nested_deeper_than_a_stack_holds_are_drawn_resized_and_measured";
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", this, "--nocapture"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    // The leaf is a unit in from each group's corner, 5,000 of them.
    assert!(
        said.contains("drawn 20000 innermost 20 measured 5000"),
        "{said}"
    );
}
