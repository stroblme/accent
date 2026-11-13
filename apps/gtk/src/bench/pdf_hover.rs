//! The Ctrl-hover link preview: its band at the reading view's zoom, inside the PDF pane.

use super::pdf::opened;
use super::*;

/// Ctrl over every link on the first page of `rel`, at 100 % and at 200 %, through XTEST: each
/// `bench hover aim <x> <y>` line is a point of the display for the pointer to go to, Ctrl held.
/// Once the preview is up and its band drawn, a line gives the PDF pane's size and the reading
/// view's pixels per point, then the band's size, whether it is still being drawn, and where the
/// popover is in the pane and whether that is inside it; then the preview is held two seconds for
/// a screenshot.
pub(super) fn bench_pdf_hover(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench hover no_tab");
            return bench_quit(&app);
        };
        let Ok(view) = pdf.views()[0].clone().downcast::<pdfview::PdfView>() else {
            return bench_quit(&app);
        };
        let links = until("hover", "links", 10_000, || {
            pdf.page_links(0).filter(|links| !links.is_empty())
        });
        let links = links.await.unwrap_or_default();
        // A point of the reading view on the display, where the window is at the corner with no
        // window manager to place it.
        let on_display = |x: i32, y: i32| {
            let root = view.root()?;
            let at = view.compute_point(&root, &graphene::Point::new(x as f32, y as f32))?;
            let (dx, dy) = root.surface_transform();
            Some((
                (f64::from(at.x()) + dx) as i32,
                (f64::from(at.y()) + dy) as i32,
            ))
        };
        for percent in [100, 200] {
            pdf.set_zoom(pdfview::PdfZoom::Scale(f64::from(percent) / 100.0));
            glib::timeout_future(Duration::from_millis(300)).await;
            for (n, link) in links.iter().enumerate() {
                let r = view.widget_rect(0, &link.rect);
                let shown = |&(x, y): &(i32, i32)| {
                    (0..view.width()).contains(&x) && (0..view.height()).contains(&y)
                };
                let at = r.map(|r| (r.x() + r.width() / 2, r.y() + r.height() / 2));
                let at = at.filter(shown);
                let Some((x, y)) = at.and_then(|(x, y)| on_display(x, y)) else {
                    println!("bench hover {percent}% link={n} off_screen");
                    continue;
                };
                println!("bench hover aim {x} {y}");
                // The pointer's motion first, which takes the last link's preview away.
                glib::timeout_future(Duration::from_millis(500)).await;
                let up = until("hover", "preview", 5_000, || pdf.preview_report() != "none");
                if up.await.is_none() {
                    continue;
                }
                glib::timeout_future(Duration::from_millis(1000)).await;
                println!("bench hover {percent}% link={n} {}", pdf.preview_report());
                glib::timeout_future(Duration::from_millis(2000)).await;
            }
        }
        bench_quit(&app);
    });
}
