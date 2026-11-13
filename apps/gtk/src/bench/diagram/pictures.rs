//! A diagram's pictures: web images behind their banner, and SVG pictures drawn sharp at
//! every zoom.

use super::*;

/// Probe a web-image diagram before/after Load: banner, canvas pixel, download state,
/// note-embed images and saved permission. Run twice with the same scratch home,
/// server offline on the second run; permission and cached drawing must persist.
pub(super) fn web(app: &Rc<App>, rel: &str, url: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        let diagram = format!(
            r#"<mxfile><diagram name="Web" id="bench-web"><mxGraphModel page="1" pageWidth="400" pageHeight="300"><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="w" style="shape=image;image={url};" vertex="1" parent="1">
            <mxGeometry x="100" y="100" width="80" height="80" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#
        );
        std::fs::write(&path, diagram).expect("write the diagram");
    }
    let (app, rel, url) = (app.clone(), rel.to_string(), url.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let state = |when: &str| {
            let canvas = tab.key_target();
            let r = tab.frame_of("w").map(|r| tab.to_widget(&r));
            let pixel = r.zip(picture(&canvas)).map(|(r, shot)| {
                let mut downloader = gdk::TextureDownloader::new(&shot);
                downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
                let (bytes, stride) = downloader.download_bytes();
                let i = (r.y + r.h / 2.0) as usize * stride + (r.x + r.w / 2.0) as usize * 4;
                [bytes[i], bytes[i + 1], bytes[i + 2]]
            });
            println!(
                "bench diagram web {when} banner={} allowed={} pixel={pixel:?} downloaded={}",
                tab.web_banner_shown(),
                tab.web_allowed(),
                crate::diagram::web_downloaded(&url)
            );
        };
        let embed = |web: bool| {
            let path = tab.path();
            async move {
                let svg = crate::diagram::embed::svg(&path, None, web).await;
                svg.map_or(0, |s| s.matches("<image").count())
            }
        };
        pumped().await;
        state("opened");
        let allowed = app.web_images.borrow().contains(&rel);
        println!("bench diagram web embed pictures={}", embed(allowed).await);
        if tab.web_banner_shown() {
            tab.press_load();
            let asked = Instant::now();
            while !crate::diagram::web_downloaded(&url) && asked.elapsed() < Duration::from_secs(5)
            {
                glib::timeout_future(Duration::from_millis(50)).await;
            }
            glib::timeout_future(Duration::from_millis(300)).await;
            pumped().await;
            state("loaded");
            let allowed = app.web_images.borrow().contains(&rel);
            println!("bench diagram web embed pictures={}", embed(allowed).await);
            app.save_session();
            let stored = app.vault().map(|v| v.session().web_images);
            println!("bench diagram web session={stored:?}");
        }
        bench_quit(&app);
    });
}

/// A page holding one SVG picture of hairlines, rings and small text, 80 × 60 on its own and
/// drawn into a 160 × 120 cell.
fn svg_sample() -> String {
    let lines: String = (0..24).map(|i| format!("M{} 4V30", 4 + i)).collect();
    let rings: String = (1..13)
        .map(|r| format!(r#"<circle cx="56" cy="17" r="{r}"/>"#))
        .collect();
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="60" viewBox="0 0 80 60"><rect width="80" height="60" fill="#fff"/><path d="{lines}" stroke="#000" stroke-width="0.25"/><g fill="none" stroke="#a00" stroke-width="0.3">{rings}</g><text x="4" y="42" font-family="sans-serif" font-size="4">4 px text, sharp at any zoom</text></svg>"##
    );
    let uri = format!("data:image/svg+xml,{}", glib::base64_encode(svg.as_bytes()));
    format!(
        r#"<mxfile host="accent">
  <diagram name="Picture" id="sharp">
    <mxGraphModel grid="1" gridSize="10" page="1" pageWidth="400" pageHeight="300">
      <root>
        <mxCell id="0" />
        <mxCell id="1" parent="0" />
        <mxCell id="svg" value="" style="shape=image;imageAspect=0;image={uri};" parent="1" vertex="1">
          <mxGeometry x="120" y="90" width="160" height="120" as="geometry" />
        </mxCell>
      </root>
    </mxGraphModel>
  </diagram>
</mxfile>
"#
    )
}

/// `=sharp:<rel>`: the SVG pictures of `rel` (written from [`svg_sample`] if absent) at 100, 200
/// and 400 %, then at 400 % a view further right: where each is on the canvas, the copies librsvg
/// drew of them once the view settled (`box/part`, in device pixels), and the milliseconds a frame
/// of the canvas takes to build and to paint, at once and once the copies have landed.
/// `ACCENT_BENCH_DIAGRAM_SHOT=<png>` adds a picture of the canvas at each, named after it.
pub(super) fn sharp(app: &Rc<App>, rel: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, svg_sample()).expect("write the sample diagram");
    }
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let canvas = tab.key_target();
        let scene = accent_drawio::scene::scene(&tab.file().pages[0]);
        let pictures: Vec<accent_drawio::Rect> = (scene.prims.iter())
            .filter_map(|p| match p {
                accent_drawio::Prim::Image { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        let shot = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT").ok();
        for (zoom, step) in [(1.0, false), (2.0, false), (4.0, false), (4.0, true)] {
            match step {
                true => tab.scroll_by(f64::from(canvas.width()), 0.0),
                false => tab.zoom_to(zoom),
            }
            pumped().await;
            let (build, paint) = frame_cost(&tab, &canvas);
            glib::timeout_future(Duration::from_millis(1500)).await;
            pumped().await;
            let (settled_build, settled_paint) = frame_cost(&tab, &canvas);
            let copies: Vec<String> = (tab.svg_copies().iter())
                .map(|w| {
                    format!(
                        "{:.0}x{:.0}/{:.0}x{:.0}",
                        w.size.0, w.size.1, w.part.w, w.part.h
                    )
                })
                .collect();
            let at: Vec<String> = (pictures.iter().map(|r| tab.to_widget(r)))
                .map(|r| format!("{:.0},{:.0},{:.0}x{:.0}", r.x, r.y, r.w, r.h))
                .collect();
            let name = format!("{:.0}{}", zoom * 100.0, if step { "-right" } else { "" });
            println!(
                "bench diagram sharp zoom={name} at={at:?} copies={copies:?} \
                 build_ms={build:.2}/{settled_build:.2} paint_ms={paint:.1}/{settled_paint:.1}"
            );
            if let Some(png) = &shot {
                shoot(
                    &canvas,
                    Path::new(&png.replace(".png", &format!("-{name}.png"))),
                );
            }
        }
        bench_quit(&app);
    });
}

/// The median milliseconds of five frames of `tab`'s canvas `widget`: building its render
/// nodes, and painting them with its window's renderer.
fn frame_cost(tab: &crate::diagram::DiagramTab, widget: &gtk::Widget) -> (f64, f64) {
    let (w, h) = (f64::from(widget.width()), f64::from(widget.height()));
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let (mut build, mut paint) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        build.push(tab.bench_move(&[], None).1);
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, w, h);
        let (Some(node), Some(renderer)) = (
            snapshot.to_node(),
            widget.native().and_then(|n| n.renderer()),
        ) else {
            continue;
        };
        let started = Instant::now();
        let viewport = graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
        renderer.render_texture(&node, Some(&viewport));
        paint.push(ms_since(started));
    }
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
    };
    (median(build), median(paint))
}
