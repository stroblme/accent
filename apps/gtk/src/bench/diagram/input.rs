//! The drills that hold a diagram on screen for real input by XTEST: `hold:`, `strip:` and
//! `present:`.

use super::*;

/// The thumbnail strip held on screen for 40 s: the Outline pane up, and the diagram in front,
/// which Go to File may change, its pages, the one shown, its strip's buttons and the window's
/// offset on the screen printed every two seconds; then the pages `rel` on disk holds.
pub(super) fn strip(app: &Rc<App>, rel: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        app.open_path(&rel);
        glib::spawn_future_local(async move {
            until("strip", "laid out", files::WAIT, || {
                app.active_diagram().is_some_and(|tab| tab.laid_out())
            })
            .await;
            let Some(tab) = app.active_diagram() else {
                println!("bench strip no_tab");
                return bench_quit(&app);
            };
            app.show_pane("outline");
            for _ in 0..20 {
                let front = app.active_diagram().unwrap_or_else(|| tab.clone());
                let (sx, sy) = app.window.surface_transform();
                println!(
                    "bench strip file={} pages={:?} page={} {} surface={sx},{sy}",
                    front.key(),
                    front.page_names(),
                    front.page_index(),
                    front.strip_buttons(app.window.upcast_ref())
                );
                glib::timeout_future(Duration::from_secs(2)).await;
            }
            let _ = app.flush_model(&tab);
            let file = std::fs::read(app.root().join(&rel)).unwrap_or_default();
            let names: Vec<String> = accent_drawio::File::from_bytes(&file)
                .map(|f| f.pages.iter().map(|p| p.name().to_string()).collect())
                .unwrap_or_default();
            println!("bench strip saved={names:?}");
            bench_quit(&app);
        });
    });
}

pub(super) fn hold(app: &Rc<App>, rel: &str, tool: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel, tool) = (app.clone(), rel.to_string(), tool.to_string());
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        app.open_path(&rel);
        glib::spawn_future_local(async move {
            // Once the page is laid out at the canvas's size: before, the shapes are not where
            // the pointer will find them (`scale=1.000`).
            until("diagram", "laid out", files::WAIT, || {
                app.active_diagram().is_some_and(|tab| tab.laid_out())
            })
            .await;
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
                    label.map(|(at, ..)| (at.x.round(), at.y.round())),
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
                if let Ok(path) = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT") {
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

/// Probe PDF and diagram presentation entry/exit with active tools, ring state and
/// zoom. Real keys, drags and double-clicks must not edit presented diagrams; test
/// page keys/wheel inside and outside presentation. `bench diagram xtest <steps>`
/// requests input and `bench diagram present <state>` reports results.
pub(super) fn present(app: &Rc<App>, rel: &str, pdf: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel, pdf) = (app.clone(), rel.to_string(), pdf.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&pdf);
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.pdf_mode(pdfview::Mode::Pen);
        let pdf_state = |app: &Rc<App>, when: &str| {
            if let Some(pdf) = app.active_pdf() {
                println!(
                    "bench diagram present pdf_{when} presenting={} ring={} tool={:?}",
                    app.presenting.get().is_some(),
                    pdf.ring_visible(),
                    pdf.mode_label()
                );
            }
        };
        pdf_state(&app, "before");
        app.set_presenting(true);
        glib::timeout_future(Duration::from_millis(500)).await;
        pdf_state(&app, "presented");
        app.set_presenting(false);
        glib::timeout_future(Duration::from_millis(500)).await;
        pdf_state(&app, "after");

        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        for _ in 0..10 {
            tab.zoom_step(false);
        }
        let _ = WidgetExt::activate_action(&app.window, "win.diagram-rect", None);
        glib::timeout_future(Duration::from_millis(300)).await;
        // A widget point on the screen, for XTEST: the window sits at 0,0 under Xvfb, its
        // client-side shadow putting the widgets a margin in.
        let canvas = tab.key_target();
        let (sx, sy) = app.window.surface_transform();
        let on_screen = |x: f64, y: f64| {
            let p = graphene::Point::new(x as f32, y as f32);
            let p = canvas.compute_point(&app.window, &p).unwrap_or(p);
            (p.x() + sx as f32, p.y() + sy as f32)
        };
        let (mx, my) = on_screen(
            f64::from(canvas.width()) / 2.0,
            f64::from(canvas.height()) / 2.0,
        );
        println!("bench diagram xtest move {mx:.0} {my:.0}; focus; sleep 0.5; key F5");
        let (mut last, mut asked) = (String::new(), false);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(14) {
            // Presented, the page is fitted to a larger canvas: `a` is aimed at from there.
            if app.presenting.get().is_some() && !asked {
                asked = true;
                glib::timeout_future(Duration::from_millis(300)).await;
                let a = tab
                    .frame_of("a")
                    .map(|r| tab.to_widget(&r))
                    .unwrap_or_default();
                let (ax, ay) = on_screen(a.x + a.w / 2.0, a.y + a.h / 2.0);
                println!(
                    "bench diagram xtest drag {ax:.0} {ay:.0} {:.0} {:.0}; move {ax:.0} {ay:.0}; \
                     down; up; down; up; sleep 0.5; key Delete; key ctrl+z; key Down; sleep 0.3; \
                     key space; sleep 0.3; key Left; sleep 0.3; key Right; sleep 0.3; \
                     key shift+space; sleep 0.3; key Page_Down; sleep 0.3; key Page_Up; \
                     sleep 0.3; scroll -1; sleep 0.3; scroll 1; sleep 0.3; key F5; sleep 0.5; \
                     move {mx:.0} {my:.0}; scroll -12; sleep 0.5; scroll 24",
                    ax + 150.0,
                    ay + 80.0
                );
            }
            let place = tab.place();
            let state = format!(
                "presenting={} page={} zoom={} y={:.0} ring={} tool={:?} selected={} \
                 history={:?} editing={:?} a={:?}",
                app.presenting.get().is_some(),
                place.page + 1,
                tab.zoom_label(),
                place.y,
                tab.ring_visible(),
                tab.tool(),
                tab.selection().len(),
                tab.history(),
                tab.editing_label(),
                tab.frame_of("a").map(|r| (r.x, r.y))
            );
            if state != last {
                println!("bench diagram present {state}");
                last = state;
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        bench_quit(&app);
    });
}
