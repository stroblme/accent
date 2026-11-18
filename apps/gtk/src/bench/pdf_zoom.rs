//! Zoom frames captured before replacement tiles arrive, then time to a sharp viewport.

use super::*;

pub(super) fn run(app: &Rc<App>, arg: &str) {
    let (rel, shots) = arg
        .split_once(',')
        .map_or((arg, None), |(rel, dir)| (rel, Some(PathBuf::from(dir))));
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = pdf::opened(&app, &rel).await else {
            panic!("PDF did not open")
        };
        let reader = pdf.views()[0]
            .clone()
            .downcast::<pdfview::PdfView>()
            .expect("reading view");
        if let Some(dir) = &shots {
            std::fs::create_dir_all(dir).unwrap();
        }
        for theme in [
            accent_core::config::Theme::Light,
            accent_core::config::Theme::Dark,
        ] {
            crate::theme::apply(theme);
            pdf.restyle();
            pdf.set_zoom(pdfview::PdfZoom::Scale(1.0));
            assert!(!pdf::settled(&pdf, "zoom_start").await);
            for (step, scale) in [1.25, 0.9, 1.6, 1.0].into_iter().enumerate() {
                let start = Instant::now();
                let cpu = pdf::render_cpu_ms();
                pdf.set_zoom(pdfview::PdfZoom::Scale(scale));
                let paint = Instant::now();
                let (node, missing, reused) = reader.zoom_frame();
                let paint_ms = ms_since(paint);
                assert!(
                    missing == 0 || reused > 0,
                    "zoom discarded all existing detail"
                );
                if let (Some(dir), Some(node)) = (&shots, node) {
                    let viewport = graphene::Rect::new(
                        0.0,
                        0.0,
                        reader.width() as f32,
                        reader.height() as f32,
                    );
                    let texture = reader
                        .native()
                        .unwrap()
                        .renderer()
                        .unwrap()
                        .render_texture(&node, Some(&viewport));
                    texture
                        .save_to_png(dir.join(format!("{theme:?}-{step}-zoom.png")))
                        .unwrap();
                }
                while reader.unsharp() > 0 && start.elapsed() < Duration::from_secs(20) {
                    glib::timeout_future(Duration::from_millis(16)).await;
                }
                let sharp_ms = ms_since(start);
                assert_eq!(reader.unsharp(), 0, "visible tiles did not settle");
                assert!(!pdf::settled(&pdf, "zoom_ahead").await);
                if let Some(dir) = &shots {
                    super::diagram::shoot(
                        reader.upcast_ref(),
                        &dir.join(format!("{theme:?}-{step}-sharp.png")),
                    );
                }
                println!(
                    "bench pdf zoom theme={theme:?} scale={scale} missing={missing} reused={reused} paint_ms={paint_ms:.2} sharp_ms={sharp_ms:.1} cpu_ms={:.1}",
                    pdf::render_cpu_ms() - cpu
                );
            }
        }
        bench_quit(&app);
    });
}

/// A two-finger pinch on `rel` (generated `Attachments/pages.pdf`) from Fit Width, fed to the
/// reading view as `zoom::zoom_on_pinch` feeds it GTK's `GestureZoom`: on the third page reached
/// by the Outline pane's Details row, on the same place reached by a scroll, and at the top while
/// the scrolled window coasts. Each line says the page and the point of it under the fingers
/// before and after, which the pinch keeps where they are.
pub(super) fn pinch(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let pdf = pdf::opened(&app, &rel).await.expect("PDF opened");
        let reader = pdf.views()[0]
            .clone()
            .downcast::<pdfview::PdfView>()
            .expect("reading view");
        let scroller = reader
            .parent()
            .and_downcast::<gtk::ScrolledWindow>()
            .expect("the reader's scroller");
        app.show_pane("outline");
        let sidebar = app.sidebar.get().expect("sidebar");
        let listed = until("pdf pinch", "outline", 10_000, || {
            let lines = sidebar.outline_lines(None);
            lines.iter().position(|l| l.trim() == "Details")
        });
        let Some(details) = listed.await else {
            return bench_quit(&app);
        };
        let at = (
            f64::from(reader.width()) / 2.0,
            f64::from(reader.height()) / 3.0,
        );
        for case in ["outline", "scrolled", "coasting"] {
            pdf.set_zoom(pdfview::PdfZoom::FitWidth);
            pdf.scroll_to(pdfview::Anchor::default());
            glib::timeout_future(Duration::from_millis(300)).await;
            let overshot = Rc::new(Cell::new(false));
            match case {
                "outline" => drop(sidebar.outline_lines(Some(details))),
                "scrolled" => pdf.scroll_to(pdfview::Anchor {
                    page: 2,
                    u: 0.0,
                    v: 0.0,
                }),
                // A touchpad's pinch can begin as up to 300 ms of two-finger scroll, where
                // libinput first guesses a scroll, and the end of that scroll sets the scrolled
                // window coasting, as a fling does. Neither reaches a headless window: the wheel
                // turned up past the top sets the same coast going, back down to it. From the
                // very top, or the eased wheel takes the notches on its way there.
                _ => {
                    scroller.vadjustment().set_value(0.0);
                    let id = scroller.connect_edge_overshot({
                        let overshot = overshot.clone();
                        move |_, _| overshot.set(true)
                    });
                    let middle = graphene::Point::new(
                        reader.width() as f32 / 2.0,
                        reader.height() as f32 / 2.0,
                    );
                    let p = reader.compute_point(&app.window, &middle).unwrap();
                    let (sx, sy) = app.window.surface_transform();
                    let steps = format!(
                        "move {:.0} {:.0}; sleep 0.2; scroll 3",
                        f64::from(p.x()) + sx,
                        f64::from(p.y()) + sy
                    );
                    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../build-aux/xtest.py");
                    let display = std::env::var("DISPLAY").unwrap_or_default();
                    let argv = ["python3", script, &display, &steps].map(std::ffi::OsStr::new);
                    if let Ok(run) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
                        let _ = run.wait_future().await;
                    }
                    until("pdf pinch", "overshoot", 3_000, || overshot.get()).await;
                    scroller.disconnect(id);
                }
            }
            // The coast back starts 50 ms after the last notch, and lasts most of a second.
            glib::timeout_future(Duration::from_millis(80)).await;
            let before = under(&reader, at);
            // GTK's scale is the whole pinch so far, a little more each event, a frame apart.
            let start = reader.pinch_from();
            let mut last = crate::zoom::pinched_zoom(start, 1.0);
            for step in 1..=12 {
                let zoom = crate::zoom::pinched_zoom(start, 1.0 + 0.05 * f64::from(step));
                if std::mem::replace(&mut last, zoom) != zoom {
                    reader.pinched(zoom, at);
                }
                glib::timeout_future(Duration::from_millis(16)).await;
            }
            glib::timeout_future(Duration::from_millis(1000)).await;
            let coast = match case {
                "coasting" => format!(" overshot={}", overshot.get()),
                _ => String::new(),
            };
            println!(
                "bench pdf pinch {case}{coast} before {before} after {} zoom={:?}",
                under(&reader, at),
                pdf.zoom_label()
            );
        }
        bench_quit(&app);
    });
}

/// The page under `at`, a point of the view, and where on it in its own points.
fn under(reader: &pdfview::PdfView, (x, y): (f64, f64)) -> String {
    match reader.page_point(x, y) {
        Some((page, x, y)) => format!("page={} point=({x:.0},{y:.0})", page + 1),
        None => "page=none".to_string(),
    }
}
