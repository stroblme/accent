//! Drills over a draw.io diagram: an edit round trip through the model, the save and the disk,
//! and pictures of every page as the canvas paints them.

use super::*;

/// A two-page diagram with a shape, an ellipse, an edge between them, a formula, and a bracket
/// and a text turned a quarter each way.
pub(super) const SAMPLE: &str = r#"<mxfile host="accent">
  <diagram name="One" id="bench-one">
    <mxGraphModel grid="1" gridSize="10" page="1" pageWidth="800" pageHeight="500" math="1">
      <root>
        <mxCell id="0" />
        <mxCell id="1" parent="0" />
        <mxCell id="a" value="A" style="rounded=1;whiteSpace=wrap;html=1;fillColor=#dae8fc;strokeColor=#6c8ebf;" parent="1" vertex="1">
          <mxGeometry x="100" y="100" width="120" height="60" as="geometry" />
        </mxCell>
        <mxCell id="b" value="&lt;b&gt;B&lt;/b&gt;" style="ellipse;whiteSpace=wrap;html=1;" parent="1" vertex="1">
          <mxGeometry x="400" y="100" width="120" height="60" as="geometry" />
        </mxCell>
        <mxCell id="e" style="edgeStyle=orthogonalEdgeStyle;rounded=0;html=1;" parent="1" source="a" target="b" edge="1">
          <mxGeometry relative="1" as="geometry" />
        </mxCell>
        <mxCell id="m" value="\(x^2\)" style="text;html=1;" parent="1" vertex="1">
          <mxGeometry x="100" y="300" width="80" height="30" as="geometry" />
        </mxCell>
        <mxCell id="k" style="shape=curlyBracket;whiteSpace=wrap;html=1;rounded=1;rotation=90;" parent="1" vertex="1">
          <mxGeometry x="350" y="260" width="20" height="160" as="geometry" />
        </mxCell>
        <mxCell id="t" value="Turned" style="text;html=1;align=center;verticalAlign=middle;rotation=-90;" parent="1" vertex="1">
          <mxGeometry x="560" y="330" width="120" height="30" as="geometry" />
        </mxCell>
      </root>
    </mxGraphModel>
  </diagram>
  <diagram name="Two" id="bench-two">
    <mxGraphModel><root><mxCell id="0" /><mxCell id="1" parent="0" /></root></mxGraphModel>
  </diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_DIAGRAM=<rel>` edits the diagram at `rel` (written from a sample first when
/// there is none) and prints each step; `=shot:<rel>:<dir>` paints every page of it into
/// `<dir>/page-N.png` and prints how long each took; `=hold:<rel>[:<tool>]` prints where the
/// sample's shapes are on the screen and stays up for ten seconds, for an XTEST pointer to work
/// on (`build-aux/xtest.py`), then prints what the model holds.
pub(super) fn bench_diagram(app: &Rc<App>, arg: &str) {
    if let Some(rest) = arg.strip_prefix("hold:") {
        let (rel, tool) = rest.split_once(':').unwrap_or((rest, ""));
        return hold(app, rel, tool);
    }
    let (rel, shots) = match arg.strip_prefix("shot:") {
        Some(rest) => match rest.split_once(':') {
            Some((rel, dir)) => (rel.to_string(), Some(PathBuf::from(dir))),
            None => (rest.to_string(), None),
        },
        None => (arg.to_string(), None),
    };
    let path = app.root().join(&rel);
    let written = shots.is_none() && !path.exists();
    if written {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let app = app.clone();
    // A file written a moment ago is only in the vault once the watcher has seen it.
    let wait = if written { 1500 } else { 0 };
    glib::timeout_add_local_once(Duration::from_millis(wait), move || {
        app.open_path(&rel);
        glib::timeout_add_local_once(Duration::from_millis(800), move || drill(&app, shots));
    });
}

fn drill(app: &Rc<App>, shots: Option<PathBuf>) {
    let Some(tab) = app.active_diagram() else {
        println!("bench diagram no_tab");
        return bench_quit(app);
    };
    println!(
        "bench diagram pages={} names={:?} facts={:?}",
        tab.page_count(),
        tab.page_names(),
        tab.facts()
    );
    match shots {
        Some(dir) => {
            shoot_pages(&tab, &dir);
            bench_quit(app);
        }
        None => edit_round(app, &tab),
    }
}

fn shoot_pages(tab: &Rc<crate::diagram::DiagramTab>, dir: &Path) {
    std::fs::create_dir_all(dir).expect("a directory for the pictures");
    for i in 0..tab.page_count() {
        tab.show_page(i);
        bench_pump();
        // Formulas are typeset in WebKit's own time; the picture waits for them.
        let asked = Instant::now();
        while asked.elapsed() < Duration::from_secs(10) {
            glib::MainContext::default().iteration(false);
            if tab.typesetting() {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            // One more frame paints what arrived and asks for what the paint found missing.
            bench_pump();
            if !tab.typesetting() {
                break;
            }
        }
        let started = Instant::now();
        let shot = shoot(&tab.key_target(), &dir.join(format!("page-{}.png", i + 1)));
        println!(
            "bench diagram page={} paint_ms={:.1} {}",
            i + 1,
            ms_since(started),
            if shot { "written" } else { "NOT WRITTEN" }
        );
    }
}

/// Paint `widget` as it is on screen into a PNG at `path`.
fn shoot(widget: &gtk::Widget, path: &Path) -> bool {
    let (w, h) = (f64::from(widget.width()), f64::from(widget.height()));
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w, h);
    let (Some(node), Some(renderer)) = (
        snapshot.to_node(),
        widget.native().and_then(|n| n.renderer()),
    ) else {
        return false;
    };
    renderer
        .render_texture(&node, None)
        .save_to_png(path)
        .is_ok()
}

fn edit_round(app: &Rc<App>, tab: &Rc<crate::diagram::DiagramTab>) {
    let bounds = |tab: &crate::diagram::DiagramTab| tab.frame_of("a");
    tab.select(vec!["a".to_string()]);
    println!("bench diagram select a frame={:?}", bounds(tab));
    tab.nudge(30.0, 0.0);
    println!(
        "bench diagram nudge frame={:?} modified={}",
        bounds(tab),
        tab.save.modified.get()
    );
    tab.edit(|e, page| e.set_label_markdown(page, "a", "**bold** A"));
    println!("bench diagram label markdown={:?}", tab.label_markdown("a"));
    tab.undo();
    println!("bench diagram undo markdown={:?}", tab.label_markdown("a"));
    tab.undo();
    println!(
        "bench diagram undo frame={:?} history={:?}",
        bounds(tab),
        tab.history()
    );
    tab.redo();
    let etag = tab.save.etag.get();
    let flushed = app.flush_diagram(tab);
    println!(
        "bench diagram flush {:?} modified={} etag_moved={}",
        flushed.map_err(|e| e.to_string()),
        tab.save.modified.get(),
        tab.save.etag.get() != etag
    );
    let disk = std::fs::read(tab.path()).expect("the file");
    let back = accent_drawio::File::from_bytes(&disk).expect("it reads back");
    let moved = back.pages[0]
        .cell("a")
        .and_then(|c| c.geometry.as_ref())
        .map(|g| g.x);
    println!(
        "bench diagram reparse pages={} a.x={moved:?}",
        back.pages.len()
    );

    let floor = || {
        app.sidebar_column
            .measure(gtk::Orientation::Horizontal, -1)
            .0
    };
    let sidebar = app.sidebar.get().expect("a vault window has a sidebar");
    app.show_pane("properties");
    tab.select(vec!["a".to_string()]);
    tab.edit_label();
    // A picture of the window as it stands, for looking at rather than asserting on.
    if let Ok(path) = std::env::var("ACCENT_BENCH_SHOT") {
        bench_pump();
        println!(
            "bench diagram shot {}",
            shoot(app.window.upcast_ref(), Path::new(&path))
        );
    }
    tab.finish_label();
    println!(
        "bench diagram properties shown={} showing={} sidebar_floor={}",
        sidebar.has_pane("properties"),
        sidebar.is_showing("properties"),
        floor()
    );
    app.open_path("note.md");
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let sidebar = app.sidebar.get().expect("a sidebar");
        println!(
            "bench diagram over a note properties={} showing={:?} sidebar_floor={}",
            sidebar.has_pane("properties"),
            ["outline", "properties", "files"].map(|p| sidebar.is_showing(p)),
            app.sidebar_column
                .measure(gtk::Orientation::Horizontal, -1)
                .0
        );
        bench_quit(&app);
    });
}

fn hold(app: &Rc<App>, rel: &str, tool: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel, tool) = (app.clone(), rel.to_string(), tool.to_string());
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        app.open_path(&rel);
        glib::timeout_add_local_once(Duration::from_millis(800), move || {
            let Some(tab) = app.active_diagram() else {
                println!("bench diagram no_tab");
                return bench_quit(&app);
            };
            // The main loop's stalls while the pointer works: a tick every 20 ms that reports a
            // gap of more than 150, which is a freeze to the reader.
            let (start, last) = (Instant::now(), Rc::new(Cell::new(Instant::now())));
            glib::timeout_add_local(Duration::from_millis(20), move || {
                let now = Instant::now();
                let gap = now.duration_since(last.replace(now));
                if gap > Duration::from_millis(150) {
                    println!(
                        "bench diagram stall {}ms at {:.1}s",
                        gap.as_millis(),
                        (now - start).as_secs_f64()
                    );
                }
                glib::ControlFlow::Continue
            });
            let canvas = tab.key_target();
            // Screen coordinates, for XTEST: the window sits at 0,0 with no window manager, but
            // its client-side shadow puts the widgets a margin in from the surface's corner.
            let (sx, sy) = app.window.surface_transform();
            let centre = |id: &str| {
                let r = tab.frame_of(id).map(|r| tab.to_widget(&r))?;
                canvas
                    .compute_point(
                        &app.window,
                        &graphene::Point::new((r.x + r.w / 2.0) as f32, (r.y + r.h / 2.0) as f32),
                    )
                    .map(|p| graphene::Point::new(p.x() + sx as f32, p.y() + sy as f32))
            };
            let (a, b) = (centre("a"), centre("b"));
            let (k, t) = (centre("k"), centre("t"));
            println!(
                "bench diagram turned k={:.0},{:.0} t={:.0},{:.0}",
                k.map_or(0.0, |p| p.x()),
                k.map_or(0.0, |p| p.y()),
                t.map_or(0.0, |p| p.x()),
                t.map_or(0.0, |p| p.y())
            );
            if !tool.is_empty() {
                let _ =
                    WidgetExt::activate_action(&app.window, &format!("win.diagram-{tool}"), None);
            }
            // The page's origin on the screen, for aiming at any diagram's page points.
            let o = tab.to_widget(&accent_drawio::Rect::new(0.0, 0.0, 0.0, 0.0));
            let origin = canvas
                .compute_point(&app.window, &graphene::Point::new(o.x as f32, o.y as f32))
                .map(|p| (p.x() + sx as f32, p.y() + sy as f32))
                .unwrap_or_default();
            println!(
                "bench diagram at a={:.0},{:.0} b={:.0},{:.0} scale={:.3} origin={:.1},{:.1}",
                a.map_or(0.0, |p| p.x()),
                a.map_or(0.0, |p| p.y()),
                b.map_or(0.0, |p| p.x()),
                b.map_or(0.0, |p| p.y()),
                tab.scale(),
                origin.0,
                origin.1
            );
            glib::timeout_add_local_once(Duration::from_secs(10), move || {
                let canvas = tab.key_target();
                let r = tab
                    .frame_of("a")
                    .map(|r| tab.to_widget(&r))
                    .unwrap_or_default();
                let at = canvas.compute_point(
                    &app.window,
                    &graphene::Point::new((r.x + r.w / 2.0) as f32, (r.y + r.h / 2.0) as f32),
                );
                println!(
                    "bench diagram a_now={:?} editing={:?} labels={:?}",
                    at.map(|p| (p.x(), p.y())),
                    tab.editing_label(),
                    ["a", "b", "e"].map(|id| tab.label_markdown(id))
                );
                let label = tab.label_at();
                println!(
                    "bench diagram label at={:?} px={:?} focus={:?} scale={:.3}",
                    label.map(|(x, y, ..)| (x.round(), y.round())),
                    label.map(|(.., px, _)| (px * 10.0).round() / 10.0),
                    label.map(|(.., focus)| focus),
                    tab.scale()
                );
                println!(
                    "bench diagram after a={:?} b={:?} selection={:?} history={:?}",
                    tab.frame_of("a"),
                    tab.frame_of("b"),
                    tab.selection(),
                    tab.history()
                );
                if let Ok(path) = std::env::var("ACCENT_BENCH_SHOT") {
                    println!(
                        "bench diagram shot {}",
                        shoot(app.window.upcast_ref(), Path::new(&path))
                    );
                }
                // What the gesture made, as the file will say it.
                let xml = tab.text();
                let file = accent_drawio::File::from_bytes(xml.as_bytes()).expect("our own XML");
                for cell in file.pages[0]
                    .cells
                    .iter()
                    .filter(|c| !["0", "1", "a", "b", "e", "m", "k", "t"].contains(&c.id.as_str()))
                {
                    println!(
                        "bench diagram new {} label={:?} style={} source={:?} target={:?} geometry={:?} ends={:?}",
                        if cell.edge { "edge" } else { "vertex" },
                        cell.label(),
                        cell.style,
                        cell.source,
                        cell.target,
                        cell.geometry
                            .as_ref()
                            .map(|g| (g.x, g.y, g.width, g.height)),
                        cell.geometry
                            .as_ref()
                            .map(|g| (g.source_point, g.target_point))
                    );
                }
                bench_quit(&app);
            });
        });
    });
}
