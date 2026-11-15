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
/// selected group's empty middle, and first clicks under the three parents draw.io's
/// `isPropagateSelectionCell` treats apart.
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
        for id in ["p1", "t1", "xl"] {
            tab.select(Vec::new());
            click(on(id)).await;
            say(&format!("first click {id}"));
        }
        bench_quit(&app);
    });
}
