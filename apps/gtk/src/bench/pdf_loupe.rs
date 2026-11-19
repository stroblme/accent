//! The loupe over a finger selecting text, `ACCENT_BENCH_PDF=loupe`.
//!
//! No finger reaches a headless window, so a finger's drag goes through the view's own selection
//! path from where the long press hands it over (`PdfView::bench_drag_select`). Each line prints
//! the tab's `loupe_report` and what is selected; a mouse drag through XTEST shows no loupe. The
//! report says how long each line's band took to render, and the loupe's longest paint.

use super::*;

/// A body line at 11 pt and a heading at 48 pt, in points down the page: baselines and sizes.
const BODY: (f64, f64) = (200.0, 11.0);
const HEADING: (f64, f64) = (60.0, 48.0);
/// The second page's three body lines, over a plot of [`PLOT`] points: the page whose band costs
/// the most to render.
const DENSE: [f64; 3] = [200.0, 230.0, 260.0];
const PLOT: u32 = 30_000;
const NAME: &str = "loupe-drill.pdf";

/// Write a page with a heading at its top and a line of body text into the vault, open it at
/// 100 %, and drag a finger along the body line, its height wobbling by a few pixels, printing the
/// loupe at each step; hold it two seconds for a screenshot (`bench loupe hold`), its picture
/// saved as `loupe.png` under `ACCENT_BENCH_SHOTS=<dir>`, and let go. Then a finger on the heading
/// at the top of the view and on the body line's start at the view's left edge at 300 %, a pen and
/// a mouse through the same path, and a real mouse drag through XTEST. Then two lines over a dense
/// plot on the second page, and a drag down its lines and back a step a frame, with the longest
/// gap between frames. Takes the file away again.
pub(super) fn run(app: &Rc<App>) {
    let path = app.root().join(NAME);
    if let Err(e) = write(&path) {
        println!("bench loupe wrote=false {e}");
        return bench_quit(app);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        let Some(pdf) = super::pdf::opened(&app, NAME).await else {
            println!("bench loupe opened=false");
            return bench_quit(&app);
        };
        let view = pdf.views()[0]
            .clone()
            .downcast::<pdfview::PdfView>()
            .expect("reading view");
        pdf.set_zoom(pdfview::PdfZoom::Scale(1.0));
        pdf.scroll_to(pdfview::Anchor::default());
        super::pdf::settled(&pdf, "loupe").await;
        let selected = || pdf.selection_in(view.upcast_ref()).unwrap_or_default();
        let say = |what: &str| {
            let (loupe, text) = (pdf.loupe_report(), selected());
            println!("bench loupe {what} {loupe} selected={text:?}");
        };
        // Until the report says `what` of a popup laid out: a popup's surface is 1 by 1 before.
        let shows = |what: &'static str| {
            let tab = &pdf;
            until("loupe", what, 3_000, move || {
                let report = tab.loupe_report();
                report.contains(what) && !report.contains(", 1, 1))")
            })
        };
        let line_on = |page: usize, baseline: f64, size: f64| {
            let r = accent_core::pdf::Rect::from_corners(
                (60.0, (baseline - size * 0.7) as f32),
                (300.0, baseline as f32),
            );
            let r = view.widget_rect(page, &r).expect("the line on screen");
            (f64::from(r.x()), f64::from(r.y() + r.height() / 2))
        };
        let line = |baseline: f64, size: f64| line_on(0, baseline, size);
        let finger = pdfview::tools::Hand::Finger;

        // Along the body line, the first step asking for the page's glyphs.
        let (x0, y0) = line(BODY.0, BODY.1);
        let from = (x0 + 2.0, y0);
        view.bench_drag_select(from, (x0 + 12.0, y0), finger);
        let up = until("loupe", "glyphs", 5_000, || {
            !pdf.loupe_report().contains("line=none")
        });
        if up.await.is_none() {
            return quit(&app, &path);
        }
        // Up once the glyphs are in, the finger not having moved since.
        shows("shown=true").await;
        say("read");
        for (step, wobble) in [0.0, 3.0, -3.0, 2.0, -2.0, 0.0].into_iter().enumerate() {
            let to = (x0 + 30.0 * (step + 1) as f64, y0 + wobble);
            view.bench_drag_select(from, to, finger);
            glib::timeout_future(Duration::from_millis(100)).await;
            say(&format!("touch step={step}"));
        }
        // Sharp once the line's band has landed, which the steps above asked for once.
        shows("source_scale=2").await;
        say("sharp");
        println!("bench loupe hold");
        let shots = std::env::var("ACCENT_BENCH_SHOTS");
        // Its picture, the popover around it being a surface of its own that paints nothing here.
        let picture = pdf.loupe_popover().and_then(|popover| popover.child());
        if let (Ok(dir), Some(picture)) = (shots, picture) {
            let shot = std::path::Path::new(&dir).join("loupe.png");
            let pictured = super::diagram::shoot(&picture, &shot);
            println!("bench loupe pictured={pictured}");
        }
        glib::timeout_future(Duration::from_millis(2500)).await;
        view.bench_lift();
        say("lifted");

        // The heading at the top of the view: no room above it on the screen.
        let (hx, hy) = line(HEADING.0, HEADING.1);
        view.bench_drag_select((hx + 2.0, hy), (hx + 60.0, hy), finger);
        shows("above=false").await;
        say("heading");
        view.bench_lift();

        // The body line's start at the view's left edge, which is the screen's with the sidebar
        // put away, zoomed in.
        app.sidebar_column.set_visible(false);
        pdf.set_zoom(pdfview::PdfZoom::Scale(3.0));
        pdf.scroll_to(pdfview::Anchor {
            page: 0,
            u: 59.0 / 595.0,
            v: 150.0 / 842.0,
        });
        super::pdf::settled(&pdf, "loupe_zoomed").await;
        let (_, y) = line(BODY.0, BODY.1);
        view.bench_drag_select((40.0, y), (4.0, y), finger);
        shows("on_screen=true").await;
        say("edge");
        view.bench_lift();
        app.sidebar_column.set_visible(true);
        pdf.set_zoom(pdfview::PdfZoom::Scale(1.0));
        pdf.scroll_to(pdfview::Anchor::default());
        super::pdf::settled(&pdf, "loupe_back").await;

        // A pen covers what it selects too; a mouse does not.
        let (x0, y0) = line(BODY.0, BODY.1);
        let pen = pdfview::tools::Hand::Pen { eraser: false };
        for (name, hand) in [("pen", pen), ("mouse", pdfview::tools::Hand::Mouse)] {
            view.bench_drag_select((x0 + 2.0, y0), (x0 + 90.0, y0), hand);
            if name == "pen" {
                shows("shown=true").await;
            }
            say(name);
            view.bench_lift();
        }

        // A real mouse's drag, held down while the loupe is asked about.
        let at = |x: f64| super::diagram::on_screen(&app, &view, x, y0).unwrap_or_default();
        let steps = format!(
            "move {}; down; move {}; move {}",
            at(x0 + 2.0),
            at(x0 + 40.0),
            at(x0 + 120.0)
        );
        let before = selected();
        super::git::xtest_now(&steps).await;
        // The press lets the last selection go before the drag makes its own.
        let changed = until("loupe", "xtest", 3_000, || {
            let now = selected();
            !now.is_empty() && now != before
        });
        let changed = changed.await;
        say("xtest_mouse");
        println!("bench loupe xtest_mouse changed={}", changed.is_some());
        super::git::xtest_now("up").await;

        // The plot's page: a band of its first line, then of its second.
        pdf.scroll_to(pdfview::Anchor {
            page: 1,
            u: 0.0,
            v: 0.0,
        });
        super::pdf::settled(&pdf, "loupe_dense").await;
        let (x0, y0) = line_on(1, DENSE[0], BODY.1);
        let from = (x0 + 2.0, y0);
        for (name, baseline) in [("dense", DENSE[0]), ("dense_next", DENSE[1])] {
            let (_, y) = line_on(1, baseline, BODY.1);
            view.bench_drag_select(from, (x0 + 60.0, y), finger);
            shows("source_scale=2").await;
            say(name);
        }
        // Down the three lines and back, a step a frame: the longest gap between frames meanwhile.
        let gaps = Rc::new(std::cell::Cell::new((0i64, 0i64, 0u32)));
        let clock = view.frame_clock().expect("a frame clock");
        let counted = gaps.clone();
        let handler = clock.connect_after_paint(move |clock| {
            let (last, gap, frames) = counted.get();
            let now = clock.frame_time();
            let gap = if last > 0 { gap.max(now - last) } else { gap };
            counted.set((now, gap, frames + 1));
        });
        for step in 0..60 {
            let (_, y) = line_on(1, DENSE[[0, 1, 2, 1, 0, 1][step / 10]], BODY.1);
            view.bench_drag_select(from, (x0 + 10.0 + 3.0 * step as f64, y), finger);
            glib::timeout_future(Duration::from_millis(16)).await;
        }
        clock.disconnect(handler);
        let (_, gap, frames) = gaps.get();
        println!(
            "bench loupe glide frames={frames} gap_ms={:.1}",
            gap as f64 / 1000.0
        );
        say("glide");
        view.bench_lift();
        quit(&app, &path);
    });
}

/// The drill's pages, in cairo's own sans: the heading and the body line under it, then three lines
/// over a plot.
fn write(path: &std::path::Path) -> Result<(), cairo::Error> {
    let surface = cairo::PdfSurface::new(595.0, 842.0, path)?;
    let cr = cairo::Context::new(&surface)?;
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    for ((baseline, size), text) in [
        (HEADING, "Loupe heading"),
        (BODY, "The loupe shows the line a finger is on."),
    ] {
        cr.set_font_size(size);
        cr.move_to(60.0, baseline);
        cr.show_text(text)?;
    }
    cr.show_page()?;
    // A random walk across the page, light enough for the lines over it to read.
    cr.set_source_rgb(0.7, 0.7, 0.7);
    cr.set_line_width(0.5);
    cr.move_to(40.0, 400.0);
    let mut seed: u32 = 1;
    for i in 0..PLOT {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let y = 100.0 + f64::from(seed >> 16) % 600.0;
        cr.line_to(40.0 + 515.0 * f64::from(i) / f64::from(PLOT), y);
    }
    cr.stroke()?;
    cr.set_source_rgb(0.0, 0.0, 0.0);
    cr.set_font_size(BODY.1);
    for (n, baseline) in DENSE.into_iter().enumerate() {
        cr.move_to(60.0, baseline);
        cr.show_text(&format!(
            "Line {} of the plot's page, over its points.",
            n + 1
        ))?;
    }
    cr.show_page()?;
    drop(cr);
    surface.finish();
    Ok(())
}

fn quit(app: &Rc<App>, path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    bench_quit(app);
}
