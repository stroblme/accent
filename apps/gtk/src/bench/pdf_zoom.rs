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
