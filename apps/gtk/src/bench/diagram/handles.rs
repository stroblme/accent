//! An arrow's waypoints added and taken away, a resize with Shift held and a move resting at the
//! canvas's edge, by XTEST (DESIGN.md, Diagram).

use super::*;

/// Two boxes joined by a straight arrow with no waypoints, and a box of its own to resize.
const HANDLES: &str = r#"<mxfile host="accent">
  <diagram name="Handles" id="bench-handles">
    <mxGraphModel grid="1" gridSize="10" page="1" pageWidth="800" pageHeight="500">
      <root>
        <mxCell id="0" />
        <mxCell id="1" parent="0" />
        <mxCell id="a" value="A" style="whiteSpace=wrap;html=1;" parent="1" vertex="1"><mxGeometry x="100" y="100" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="b" value="B" style="whiteSpace=wrap;html=1;" parent="1" vertex="1"><mxGeometry x="400" y="100" width="80" height="60" as="geometry" /></mxCell>
        <mxCell id="e" style="endArrow=classic;html=1;" parent="1" source="a" target="b" edge="1"><mxGeometry relative="1" as="geometry" /></mxCell>
        <mxCell id="s" value="S" style="whiteSpace=wrap;html=1;" parent="1" vertex="1"><mxGeometry x="100" y="300" width="80" height="60" as="geometry" /></mxCell>
      </root>
    </mxGraphModel>
  </diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_DIAGRAM=handles:<rel>` writes [`HANDLES`] to `<rel>` and prints the arrow's
/// waypoints as the canvas's menu adds one on its line, a double click on its handle and Remove
/// Waypoint take it away and Clear Waypoints from a shape it is on clears it, each with the menu
/// offered and Undo after; then the box a right handle dragged with Shift held leaves, and where
/// a box lands that was held at the canvas's bottom edge for a second.
pub(super) fn handles(app: &Rc<App>, rel: &str) {
    std::fs::write(app.root().join(rel), HANDLES).expect("write the diagram");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // A file written a moment ago is only in the vault once the watcher has seen it.
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        let laid_out = || app.active_diagram().is_some_and(|tab| tab.laid_out());
        until("handles", "laid out", files::WAIT, laid_out).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram handles no_tab");
            return bench_quit(&app);
        };
        let xtest = super::super::git::xtest;
        // Page point `(x, y)` on the screen.
        let at = |x: f64, y: f64| {
            let r = tab.to_widget(&accent_drawio::Rect::new(x, y, 0.0, 0.0));
            on_screen(&app, &tab.key_target(), r.x, r.y).unwrap_or_default()
        };
        xtest(&format!("move {}; focus", at(20.0, 480.0))).await;
        let page = || {
            let file = accent_drawio::File::from_bytes(tab.text().as_bytes()).ok();
            file.and_then(|f| f.pages.into_iter().next())
                .unwrap_or_default()
        };
        let say = |step: &str| {
            let points = page().cell("e").and_then(|c| c.geometry.clone()?.points);
            let points: Vec<_> = points.iter().flatten().map(|p| (p.x, p.y)).collect();
            println!(
                "bench diagram handles {step} points={points:?} history={:?}",
                tab.history()
            );
        };
        // The menu a right-click at page point `(x, y)` opens: its labels, and the one `label`
        // names picked, or the menu closed with none.
        let menu = async |x: f64, y: f64, label: &str| {
            xtest(&format!("move {}; down 3; up 3", at(x, y))).await;
            let menu = find_widget(app.window.upcast_ref(), &|w| {
                w.is::<gtk::PopoverMenu>() && w.is_visible()
            })
            .and_downcast::<gtk::PopoverMenu>();
            let labels = menu.as_ref().and_then(|m| m.menu_model());
            let labels = labels.map(|m| fileops::labels(&m)).unwrap_or_default();
            let picked = menu
                .as_ref()
                .is_some_and(|m| super::super::kernel::pick(m, label));
            if let Some(menu) = menu {
                menu.popdown();
            }
            glib::timeout_future(Duration::from_millis(300)).await;
            (labels, picked)
        };
        let waypoints = |labels: &[String]| -> Vec<String> {
            let named = |l: &&String| l.contains("Waypoint");
            labels.iter().filter(named).cloned().collect()
        };
        tab.select(vec!["e".into()]);
        let (labels, picked) = menu(250.0, 130.0, "Add Waypoint").await;
        println!(
            "bench diagram handles menu line {:?} picked={picked}",
            waypoints(&labels)
        );
        say("added");
        xtest(&format!("move {}; down; up; down; up", at(250.0, 130.0))).await;
        say("double clicked");
        tab.undo();
        say("undone");
        let (labels, picked) = menu(250.0, 130.0, "Remove Waypoint").await;
        println!(
            "bench diagram handles menu waypoint {:?} picked={picked}",
            waypoints(&labels)
        );
        say("removed");
        tab.undo();
        tab.select(vec!["a".into()]);
        let (labels, picked) = menu(140.0, 130.0, "Clear Waypoints").await;
        println!(
            "bench diagram handles menu shape {:?} picked={picked}",
            waypoints(&labels)
        );
        say("cleared");

        // Its right handle 40 units out, with Shift held: 120 wide, and as tall as that keeps it.
        let placed = |id: &str| {
            let g = page().cell(id)?.geometry.clone()?;
            Some(format!("{},{},{},{}", g.x, g.y, g.width, g.height))
        };
        tab.select(vec!["s".into()]);
        let r = tab.frame_of("s").unwrap_or_default();
        let (x, y) = (r.right() + 1.0, r.y + r.h / 2.0);
        let (from, to) = (at(x, y), at(x + 40.0, y));
        xtest(&format!("keydown shift; drag {from} {to}; keyup shift")).await;
        println!("bench diagram handles shift resize s={:?}", placed("s"));

        // Box b taken to the canvas's bottom edge and held there: the canvas scrolls on past where
        // it ended and b lands beyond that, under the pointer, the page not moving under the drop.
        tab.select(vec!["b".into()]);
        let canvas = tab.key_target();
        let origin = tab.to_widget(&accent_drawio::Rect::new(0.0, 0.0, 0.0, 0.0));
        let end = (f64::from(canvas.height()) - origin.y) / tab.scale();
        let r = tab.to_widget(&tab.frame_of("b").unwrap_or_default());
        let (bx, by) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        let edge = f64::from(canvas.height()) - 5.0;
        let screen = |x: f64, y: f64| on_screen(&app, &canvas, x, y).unwrap_or_default();
        let steps = format!(
            "move {}; down; move {}; move {}; sleep 1; up",
            screen(bx, by),
            screen(bx, by + 10.0),
            screen(bx, edge)
        );
        xtest(&steps).await;
        let b = page().cell("b").and_then(|c| c.geometry.clone());
        let beyond = b.as_ref().is_some_and(|g| g.y > end);
        let r = tab.to_widget(&tab.frame_of("b").unwrap_or_default());
        let under = (r.y + r.h / 2.0 - edge).abs() < 20.0;
        println!("bench diagram handles edge beyond_canvas={beyond} under_pointer={under}");
        bench_quit(&app);
    });
}
