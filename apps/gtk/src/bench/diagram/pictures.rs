//! A diagram's pictures: web images behind their banner, and SVG pictures drawn sharp at
//! every zoom.

use super::*;

/// Probe a web-image diagram before/after Load: banner, canvas pixel, download state,
/// note-embed images and saved permission. Run twice with the same scratch home,
/// server offline on the second run; permission and cached drawing must persist. A `url` that
/// is a path is a picture beside the diagram, drawn with no banner and no Load, then drawn again
/// as its file turns blue, goes and comes back red. An absolute `rel` is a diagram outside the
/// vault.
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
        let pixel = || {
            let canvas = tab.key_target();
            let r = tab.frame_of("w").map(|r| tab.to_widget(&r));
            r.zip(picture(&canvas)).map(|(r, shot)| {
                let mut downloader = gdk::TextureDownloader::new(&shot);
                downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
                let (bytes, stride) = downloader.download_bytes();
                let i = (r.y + r.h / 2.0) as usize * stride + (r.x + r.w / 2.0) as usize * 4;
                [bytes[i], bytes[i + 1], bytes[i + 2]]
            })
        };
        let state = |when: &str| {
            println!(
                "bench diagram web {when} banner={} allowed={} pixel={:?} downloaded={}",
                tab.web_banner_shown(),
                tab.web_allowed(),
                pixel(),
                crate::diagram::web_downloaded(&url)
            );
        };
        let drawn = |web: bool| {
            let (path, files) = (tab.path(), tab.files());
            async move { crate::diagram::embed::svg(&path, None, web, files).await }
        };
        let embed = |web: bool| {
            let drawn = drawn(web);
            async move { drawn.await.map_or(0, |s| s.matches("<image").count()) }
        };
        pumped().await;
        state("opened");
        let allowed = app.web_images.borrow().contains(&rel);
        println!("bench diagram web embed pictures={}", embed(allowed).await);
        if crate::diagram::files::is_file(&url) {
            let file = app
                .root()
                .join(accent_core::path::parent_dir(&rel))
                .join(&url);
            let red = std::fs::read(&file).expect("the picture");
            let blue = solid_png([0, 0, 255]);
            let mut was = drawn(allowed).await;
            // Each with the pixel it is to show: blue, the paper inside its dashed box, red.
            for (when, bytes, shown) in [
                ("changed", Some(blue), [0, 0, 255]),
                ("removed", None, [255, 255, 255]),
                ("added", Some(red), [255, 0, 0]),
            ] {
                match bytes {
                    Some(bytes) => std::fs::write(&file, bytes).expect("write the picture"),
                    None => std::fs::remove_file(&file).expect("remove the picture"),
                }
                // Until the watcher has told the window and the picture has landed, or 5 s.
                let asked = Instant::now();
                while pixel() != Some(shown) && asked.elapsed() < Duration::from_secs(5) {
                    glib::timeout_future(Duration::from_millis(50)).await;
                }
                state(when);
                let now = drawn(allowed).await;
                let pictures = now.as_ref().map_or(0, |s| s.matches("<image").count());
                println!(
                    "bench diagram web {when} embed pictures={pictures} new={}",
                    now != was
                );
                was = now;
            }
        }
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

/// A 4 × 4 PNG of one colour.
fn solid_png(rgb: [u8; 3]) -> Vec<u8> {
    let bytes = glib::Bytes::from_owned(rgb.repeat(16));
    let format = gdk::MemoryFormat::R8g8b8;
    let texture = gdk::MemoryTexture::new(4, 4, format, &bytes, 12);
    texture.save_to_png_bytes().to_vec()
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

/// How many pictures [`pictures_sample`] puts on its second page.
const PICTURES: usize = 30;

/// An SVG picture of its own for picture `i`: 160 curves, 40 rings and a line of text in a
/// colour of its own, line art a few hundred elements big as a diagram's icons and charts are.
pub(in crate::bench) fn svg_picture(i: usize) -> String {
    let curves: String = (0..160)
        .map(|k| {
            let (x, y) = ((k * 7 + i) % 200, (k * 13 + i * 3) % 150);
            let (x1, y1, x2, y2) = (x + 30, y + 40, x + 25, y + 30);
            format!("M{x} {y}C{x1} {y} {x} {y1} {x2} {y2}")
        })
        .collect();
    let rings: String = (1..41)
        .map(|r| format!(r#"<circle cx="{}" cy="75" r="{r}"/>"#, 100 + i))
        .collect();
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="150" viewBox="0 0 200 150"><rect width="200" height="150" fill="#fff"/><path d="{curves}" fill="none" stroke="hsl({} 70% 40%)" stroke-width="0.5"/><g fill="none" stroke="#a00" stroke-width="0.3">{rings}</g><text x="6" y="140" font-family="sans-serif" font-size="9">Picture {i}</text></svg>"##,
        i * 37 % 360
    )
}

/// A PNG picture of its own for picture `i`: 320 × 240 pixels of a gradient under noise, which
/// compresses as little as a photo does.
pub(in crate::bench) fn png_picture(i: usize) -> Vec<u8> {
    let (w, h) = (320, 240);
    let mut seed = 0x9e37_79b9_u32.wrapping_mul(i as u32 + 1);
    let mut pixels = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 26) as usize;
            pixels.extend([(x + noise) as u8, (y + noise) as u8, (i * 8 + noise) as u8]);
        }
    }
    let bytes = glib::Bytes::from_owned(pixels);
    let format = gdk::MemoryFormat::R8g8b8;
    let texture = gdk::MemoryTexture::new(w as i32, h as i32, format, &bytes, w * 3);
    texture.save_to_png_bytes().to_vec()
}

/// A blank page, and a page of [`PICTURES`] pictures, SVGs and PNGs in turn, each its own.
fn pictures_sample() -> String {
    let cells: String = (0..PICTURES)
        .map(|i| {
            let uri = match i % 2 {
                0 => format!(
                    "data:image/svg+xml,{}",
                    glib::base64_encode(svg_picture(i).as_bytes())
                ),
                _ => format!("data:image/png,{}", glib::base64_encode(&png_picture(i))),
            };
            let (x, y) = (20 + i % 6 * 195, 20 + i / 6 * 175);
            format!(
                r#"<mxCell id="p{i}" value="" style="shape=image;imageAspect=0;image={uri};" parent="1" vertex="1"><mxGeometry x="{x}" y="{y}" width="180" height="150" as="geometry" /></mxCell>"#
            )
        })
        .collect();
    format!(
        r#"<mxfile host="accent">
  <diagram name="Blank" id="blank"><mxGraphModel page="1" pageWidth="1200" pageHeight="900"><root><mxCell id="0" /><mxCell id="1" parent="0" /></root></mxGraphModel></diagram>
  <diagram name="Pictures" id="pictures"><mxGraphModel page="1" pageWidth="1200" pageHeight="900"><root><mxCell id="0" /><mxCell id="1" parent="0" />{cells}</root></mxGraphModel></diagram>
</mxfile>
"#
    )
}

/// `=pictures:<rel>[:outline]`: turn from a blank page to one of [`PICTURES`] pictures (`rel`,
/// written from [`pictures_sample`] if absent) and time every frame of the canvas on the main
/// thread, from its frame clock's before-paint to its after-paint, until each picture is drawn:
/// how many frames, the worst and their sum, and when the last picture was drawn after the turn.
/// With `:outline` the Outline pane is shown as the page turns, and its thumbnails are waited for
/// too.
pub(super) fn pictures(app: &Rc<App>, rel: &str, outline: bool) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, pictures_sample()).expect("write the sample diagram");
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
        let Some(clock) = tab.key_target().frame_clock() else {
            println!("bench diagram pictures no_frame_clock");
            return bench_quit(&app);
        };
        let (started, frames) = (Rc::new(Cell::new(None)), Rc::new(RefCell::new(Vec::new())));
        let s = started.clone();
        let before = clock.connect_before_paint(move |_| s.set(Some(Instant::now())));
        let (s, f) = (started.clone(), frames.clone());
        let after = clock.connect_after_paint(move |_| {
            if let Some(t) = s.take() {
                f.borrow_mut().push(ms_since(t));
            }
        });
        let turned = Instant::now();
        if outline {
            app.show_pane("outline");
        }
        tab.show_page(1);
        let thumbs = || tab.thumbs_painted() == tab.page_count() || !outline;
        while (tab.pictures_drawn() < PICTURES || !thumbs())
            && turned.elapsed() < Duration::from_secs(10)
        {
            glib::timeout_future(Duration::from_millis(2)).await;
        }
        let drawn_ms = ms_since(turned);
        glib::timeout_future(Duration::from_millis(300)).await;
        clock.disconnect(before);
        clock.disconnect(after);
        let frames = frames.take();
        let worst = frames.iter().copied().fold(0.0, f64::max);
        let shown: Vec<String> = frames.iter().map(|ms| format!("{ms:.1}")).collect();
        println!(
            "bench diagram pictures drawn={}/{PICTURES} thumbs={}/{} frames={} worst_ms={worst:.1} \
             sum_ms={:.1} drawn_ms={drawn_ms:.0} each_ms=[{}]",
            tab.pictures_drawn(),
            tab.thumbs_painted(),
            tab.page_count(),
            frames.len(),
            frames.iter().sum::<f64>(),
            shown.join(",")
        );
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
