//! What real clicks and bands select among groups, by XTEST (DESIGN.md, Diagram).

use super::*;

/// A group of two, a group of two groups of two, a swimlane that asks to be selected first, a
/// group whose bounds are its shapes' and a connector holding a label of its own.
const GROUPS: &str = r#"<mxfile host="accent">
  <diagram name="Groups" id="bench-select">
    <mxGraphModel grid="1" gridSize="10" page="1" pageWidth="800" pageHeight="600">
      <root>
        <mxCell id="0" />
        <mxCell id="1" parent="0" />
        <mxCell id="g" style="group" parent="1" vertex="1" connectable="0"><mxGeometry x="40" y="40" width="200" height="60" as="geometry" /></mxCell>
        <mxCell id="c" value="C" style="whiteSpace=wrap;html=1;" parent="g" vertex="1"><mxGeometry width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="d" value="D" style="whiteSpace=wrap;html=1;" parent="g" vertex="1"><mxGeometry x="120" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="G" style="group" parent="1" vertex="1" connectable="0"><mxGeometry x="40" y="160" width="440" height="60" as="geometry" /></mxCell>
        <mxCell id="g1" style="group" parent="G" vertex="1" connectable="0"><mxGeometry width="200" height="60" as="geometry" /></mxCell>
        <mxCell id="c1" value="C1" style="whiteSpace=wrap;html=1;" parent="g1" vertex="1"><mxGeometry width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="c2" value="C2" style="whiteSpace=wrap;html=1;" parent="g1" vertex="1"><mxGeometry x="120" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="g2" style="group" parent="G" vertex="1" connectable="0"><mxGeometry x="240" width="200" height="60" as="geometry" /></mxCell>
        <mxCell id="d1" value="D1" style="whiteSpace=wrap;html=1;" parent="g2" vertex="1"><mxGeometry width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="d2" value="D2" style="whiteSpace=wrap;html=1;" parent="g2" vertex="1"><mxGeometry x="120" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="p" value="Lane" style="swimlane;selectParentFirst=1;startSize=20;" parent="1" vertex="1"><mxGeometry x="40" y="280" width="200" height="100" as="geometry" /></mxCell>
        <mxCell id="p1" value="P1" style="whiteSpace=wrap;html=1;" parent="p" vertex="1"><mxGeometry x="20" y="40" width="60" height="40" as="geometry" /></mxCell>
        <mxCell id="tb" style="group;transparentBounds=1;" parent="1" vertex="1" connectable="0"><mxGeometry as="geometry" /></mxCell>
        <mxCell id="t1" value="T1" style="whiteSpace=wrap;html=1;" parent="tb" vertex="1"><mxGeometry x="300" y="280" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="t2" value="T2" style="whiteSpace=wrap;html=1;" parent="tb" vertex="1"><mxGeometry x="420" y="280" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="s1" value="S1" style="whiteSpace=wrap;html=1;" parent="1" vertex="1"><mxGeometry x="560" y="40" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="s2" value="S2" style="whiteSpace=wrap;html=1;" parent="1" vertex="1"><mxGeometry x="560" y="160" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="x" style="endArrow=classic;html=1;" parent="1" source="s1" target="s2" edge="1"><mxGeometry relative="1" as="geometry" /></mxCell>
        <mxCell id="xl" value="Label" style="edgeLabel;html=1;align=center;verticalAlign=middle;" parent="x" vertex="1" connectable="0"><mxGeometry relative="1" as="geometry"><mxPoint as="offset" /></mxGeometry></mxCell>
      </root>
    </mxGraphModel>
  </diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_DIAGRAM=select:<rel>` writes [`GROUPS`] to `<rel>` and with real clicks and
/// bands prints what each selects: a band over a group and over part of one, clicks into a group
/// and on to a sibling, into a nested group and on to a cousin, a click and a band from a
/// selected group's empty middle, first clicks under the three parents draw.io's
/// `isPropagateSelectionCell` treats apart, the clicks out of a group whose bounds are its
/// shapes' and back, drags from a cell not selected banding and from a selected one moving it,
/// Select All, and pastes inside a group and outside it. Then it prints how
/// the groups fit what they hold as the keys move a shape out of one and delete what it holds,
/// move the group whose bounds are its shapes', and a group's handle scales what it holds.
pub(super) fn select(app: &Rc<App>, rel: &str) {
    std::fs::write(app.root().join(rel), GROUPS).expect("write the diagram");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // A file written a moment ago is only in the vault once the watcher has seen it.
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        let laid_out = || app.active_diagram().is_some_and(|tab| tab.laid_out());
        until("select", "laid out", files::WAIT, laid_out).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram select no_tab");
            return bench_quit(&app);
        };
        // Page point `(x, y)` on the screen, and the middle of cell `id`.
        let at = |x: f64, y: f64| {
            let r = tab.to_widget(&accent_drawio::Rect::new(x, y, 0.0, 0.0));
            on_screen(&app, &tab.key_target(), r.x, r.y).unwrap_or_default()
        };
        let on = |id: &str| {
            let r = tab.frame_of(id).unwrap_or_default();
            at(r.x + r.w / 2.0, r.y + r.h / 2.0)
        };
        let say = |step: &str| println!("bench diagram select {step} {:?}", tab.selection());
        let click = |to: String| async move {
            super::super::git::xtest(&format!("move {to}; down; up")).await;
        };
        super::super::git::xtest(&format!("move {}; focus", at(20.0, 560.0))).await;
        let band = |from: (f64, f64), to: (f64, f64)| {
            let (a, b) = (at(from.0, from.1), at(to.0, to.1));
            format!("drag {a} {b}")
        };
        super::super::git::xtest(&band((20.0, 20.0), (260.0, 120.0))).await;
        say("band over g");
        tab.select(Vec::new());
        super::super::git::xtest(&band((20.0, 150.0), (140.0, 240.0))).await;
        say("band over c1");
        tab.select(Vec::new());
        let steps = [
            ("click c", "c"),
            ("click c again", "c"),
            ("click sibling d", "d"),
            ("clear", ""),
            ("click c1", "c1"),
            ("click c1 again", "c1"),
            ("click c1 a third time", "c1"),
            ("click cousin d1", "d1"),
        ];
        for (step, id) in steps {
            if id.is_empty() {
                tab.select(Vec::new());
                continue;
            }
            click(on(id)).await;
            say(step);
        }
        // A hand's click moves a pixel or two between press and release.
        tab.select(Vec::new());
        click(on("c")).await;
        let r = tab.to_widget(&tab.frame_of("c").unwrap_or_default());
        let to = on_screen(
            &app,
            &tab.key_target(),
            r.x + r.w / 2.0 + 2.0,
            r.y + r.h / 2.0,
        );
        let (from, to) = (on("c"), to.unwrap_or_default());
        super::super::git::xtest(&format!("move {from}; down; move {to}; up")).await;
        say("click c again moving 2 px");
        tab.select(Vec::new());
        click(on("c")).await;
        click(at(140.0, 70.0)).await;
        say("click inside g between its shapes");
        tab.select(Vec::new());
        click(on("c")).await;
        super::super::git::xtest(&band((140.0, 70.0), (380.0, 230.0))).await;
        say("band from inside g over c2 and d1");
        for id in ["p1", "xl", "t1"] {
            tab.select(Vec::new());
            click(on(id)).await;
            say(&format!("first click {id}"));
        }
        // The group whose bounds are its shapes' is framed round them and taken by a second
        // click on t1, as draw.io's is, and a third goes back in.
        click(on("t1")).await;
        say("click t1 again");
        let r = tab.frame_of("tb").unwrap_or_default();
        println!("bench diagram select tb frame {:?}", (r.x, r.y, r.w, r.h));
        click(on("t1")).await;
        say("click t1 a third time");
        // Only what is selected moves (user decision 2026-11-18): a drag from the lane's body,
        // which is not selected, bands over p1 and moves nothing, and with Ctrl adds p1 to s1; a
        // click on s2 that travels 2 px selects it, and a drag from it then moves it, as one from
        // c moves the group g it selected.
        let frames = || ["p", "p1", "s2", "g"].map(|id| tab.frame_of(id).map(|r| (r.x, r.y)));
        let still = frames();
        let lane = band((200.0, 300.0), (50.0, 370.0));
        tab.select(Vec::new());
        super::super::git::xtest(&lane).await;
        say("band from lane p over p1");
        let moved = frames() != still;
        println!(
            "bench diagram select band from p moved={moved} history={:?}",
            tab.history()
        );
        tab.select(vec!["s1".into()]);
        super::super::git::xtest(&format!("keydown ctrl; {lane}; keyup ctrl")).await;
        say("ctrl band from lane p over p1");
        // A mouse's click may travel under 3 px, a finger's or a pen's GTK's drag threshold
        // (`widgets::click_slop`, unit-tested, XTEST having no touch): 4 px is a band over
        // nothing, which lets go of s1.
        tab.select(vec!["s1".into()]);
        let r = tab.to_widget(&tab.frame_of("s2").unwrap_or_default());
        let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let to = on_screen(&app, &tab.key_target(), x + 4.0, y).unwrap_or_default();
        super::super::git::xtest(&format!("move {}; down; move {to}; up", on("s2"))).await;
        say("click s2 moving 4 px");
        tab.select(Vec::new());
        let to = on_screen(&app, &tab.key_target(), x + 2.0, y).unwrap_or_default();
        super::super::git::xtest(&format!("move {}; down; move {to}; up", on("s2"))).await;
        say("click s2 moving 2 px");
        // The pointer says so: the move cursor over s2, and none over p1, clear of the ring.
        let cursor = async |id: &str| {
            super::super::git::xtest(&format!("move {}; sleep 0.1", on(id))).await;
            let cursor = tab.key_target().cursor();
            cursor.and_then(|c| c.name()).map(|n| n.to_string())
        };
        let (over, beside) = (cursor("s2").await, cursor("p1").await);
        println!("bench diagram select cursor s2={over:?} p1={beside:?}");
        let unmoved = frames() == still;
        super::super::git::xtest(&band((600.0, 190.0), (640.0, 190.0))).await;
        println!(
            "bench diagram select drag selected s2 unmoved_before={unmoved} s2={:?} history={:?}",
            tab.frame_of("s2").map(|r| (r.x, r.y)),
            tab.history()
        );
        tab.undo();
        tab.select(Vec::new());
        click(on("c")).await;
        super::super::git::xtest(&band((80.0, 70.0), (80.0, 110.0))).await;
        say("drag c of selected g");
        println!(
            "bench diagram select drag c of selected g g={:?}",
            tab.frame_of("g").map(|r| (r.x, r.y))
        );
        tab.undo();
        println!("bench diagram select undone {}", frames() == still);
        super::super::git::xtest("key ctrl+a").await;
        say("select all");
        // A group is open while the selection is inside it: its outline shows and a paste goes
        // into it, and once the selection is let go of a paste lands on the layer.
        let parents = || {
            let file = accent_drawio::File::from_bytes(tab.text().as_bytes()).ok();
            let page = file.as_ref().and_then(|f| f.pages.first());
            let parent = |id: &String| page.and_then(|p| p.cell(id)?.parent.clone());
            tab.selection().iter().map(parent).collect::<Vec<_>>()
        };
        tab.select(Vec::new());
        click(on("c")).await;
        click(on("c")).await;
        println!("bench diagram select open inside g {:?}", tab.open_group());
        if let Ok(path) = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT") {
            let shot = shoot(app.window.upcast_ref(), Path::new(&path));
            println!("bench diagram select shot {shot}");
        }
        super::super::git::xtest("key ctrl+c; key ctrl+v").await;
        let (open, inside) = (tab.open_group(), parents());
        println!("bench diagram select paste inside g {inside:?} open={open:?}");
        click(at(20.0, 560.0)).await;
        super::super::git::xtest("key ctrl+v").await;
        let (open, outside) = (tab.open_group(), parents());
        println!("bench diagram select paste outside {outside:?} open={open:?}");
        // A group fits what it holds, c moved left out of g and down by the keys ten units a
        // press, then d deleted and the rest with it, g going with the last of them.
        let page = || {
            let file = accent_drawio::File::from_bytes(tab.text().as_bytes()).ok();
            file.and_then(|f| f.pages.into_iter().next())
                .unwrap_or_default()
        };
        let placed = |id: &str| {
            let g = page().cell(id)?.geometry.clone()?;
            Some(format!("{},{},{},{}", g.x, g.y, g.width, g.height))
        };
        let fits = |step: &str| {
            let [g, c, d] = ["g", "c", "d"].map(placed);
            println!("bench diagram select fit {step} g={g:?} c={c:?} d={d:?}");
        };
        tab.select(vec!["c".into()]);
        super::super::git::xtest(&"key shift+Left; key shift+Down; ".repeat(4)).await;
        fits("move c");
        tab.select(vec!["d".into()]);
        super::super::git::xtest("key Delete").await;
        fits("delete d");
        tab.select(page().children("g").map(|c| c.id.clone()).collect());
        super::super::git::xtest("key Delete").await;
        fits("delete the rest");
        // The group whose bounds are its shapes' moves by them and keeps its 0,0,0,0.
        tab.select(vec!["tb".into()]);
        super::super::git::xtest("key shift+Right; key shift+Right").await;
        let [tb, t1] = ["tb", "t1"].map(placed);
        println!("bench diagram select fit move tb tb={tb:?} t1={t1:?}");
        // G dragged by its bottom-right handle half as large again, what it holds with it.
        tab.select(vec!["G".into()]);
        let corner = tab.frame_of("G").unwrap_or_default();
        let (x, y) = (corner.right() + 1.0, corner.bottom() + 1.0);
        let grown = (x + corner.w / 2.0, y + corner.h / 2.0);
        super::super::git::xtest(&band((x, y), grown)).await;
        let [g, g2, d2] = ["G", "g2", "d2"].map(placed);
        println!("bench diagram select fit resize G G={g:?} g2={g2:?} d2={d2:?}");
        bench_quit(&app);
    });
}
