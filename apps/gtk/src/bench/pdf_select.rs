//! The Select tool's drill, `ACCENT_BENCH_PDF=select:<rel>[,<n>]`: on the first page, two lines,
//! a rectangle and a text of ours written into the file first, then through XTEST a lasso, a
//! move and its Undo, a copy pasted a step off it, a pick and a delete, Ctrl+click, a cut pasted
//! in place, a click away and Escape, a group stretched by its corner, a duplicate and the arrows,
//! each against the file. Pictures of the lasso and the group, light and dark, go to `$TMPDIR`.
//! With `,<n>`, `n` strokes down the page's left edge first, and how long the render thread takes
//! to move and delete them all. Writes the file.

use super::pdf::{opened, written};
use super::*;
use accent_core::pdf::{self, Rect};

/// Two lines on the left, a rectangle on the right and a text under them, in page points.
const LEFT: [f32; 2] = [200.0, 260.0];
const RECT: (f32, f32, f32, f32) = (350.0, 200.0, 450.0, 280.0);
const TEXT: (f32, f32) = (100.0, 330.0);

/// The strokes and texts of `path`'s first page, each as where its box begins, sorted.
fn filed(path: &std::path::Path) -> Vec<(i32, i32)> {
    let Ok(doc) = pdf::PdfDoc::open(path) else {
        return Vec::new();
    };
    let inks = doc
        .inks(0)
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.bounds);
    let texts = doc
        .texts(0)
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.bounds);
    let mut out: Vec<(i32, i32)> = (inks.chain(texts))
        .map(|b| (b.left.round() as i32, b.top.round() as i32))
        .collect();
    out.sort();
    out
}

/// The two lines, the rectangle and the text, written into `path`'s first page.
fn draw(path: &std::path::Path) -> anyhow::Result<()> {
    let mut doc = pdf::PdfDoc::open(path)?;
    let style = pdf::InkStyle {
        width: 2.0,
        rgba: [30, 30, 160, 255],
        multiply: false,
    };
    for y in LEFT {
        let line = pdf::Shape::Line {
            a: (100.0, y),
            b: (200.0, y),
        };
        doc.add_shape(0, line, style)?;
    }
    let (l, t, r, b) = RECT;
    doc.add_shape(
        0,
        pdf::Shape::Rect(Rect::from_corners((l, t), (r, b))),
        style,
    )?;
    let red = pdf::TextStyle {
        size: 14.0,
        rgb: [200, 0, 0],
    };
    doc.add_text(0, TEXT, "Hello", red)?;
    doc.save_to(path, None)?;
    Ok(())
}

pub(super) fn bench_pdf_select(app: &Rc<App>, arg: &str) {
    let mut parts = arg.split(',');
    let rel = parts.next().unwrap_or_default().to_string();
    let fill: usize = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let app = app.clone();
    glib::spawn_future_local(async move {
        let path = app.root().join(&rel);
        if let Err(e) = draw(&path) {
            println!("bench select draw_failed {e:#}");
            return bench_quit(&app);
        }
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench select no_tab");
            return bench_quit(&app);
        };
        pdf.goto_page(0);
        let view = pdf.views()[0].clone();
        let Some(reader) = view.downcast_ref::<pdfview::PdfView>().cloned() else {
            return bench_quit(&app);
        };
        app.pdf_mode(pdfview::Mode::Select);
        glib::timeout_future(Duration::from_secs(2)).await;
        let (sx, sy) = app.window.surface_transform();
        // A point of the first page on the screen, which is where XTEST aims.
        let at = |(x, y): (f32, f32)| {
            let r = reader.widget_rect(0, &Rect::from_corners((x, y), (x, y)))?;
            let p = graphene::Point::new(r.x() as f32, r.y() as f32);
            let p = view.compute_point(&app.window, &p).unwrap_or(p);
            Some((f64::from(p.x()) + sx, f64::from(p.y()) + sy))
        };
        let to =
            |p: (f32, f32)| at(p).map_or(String::new(), |(x, y)| format!("move {x:.0} {y:.0}"));
        let click = |p: (f32, f32)| format!("{}; down; up; sleep 0.2", to(p));
        let lasso = |corners: [(f32, f32); 4], up: bool| {
            let mut steps = vec![to(corners[0]), "down".to_string()];
            for k in 0..40 {
                let (a, b) = (corners[k / 10], corners[(k / 10 + 1) % 4]);
                let t = (k % 10 + 1) as f32 / 10.0;
                steps.push(to((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)));
                steps.push("sleep 0.01".to_string());
            }
            if up {
                steps.push("up".to_string());
            }
            steps.join("; ")
        };
        let picked = || reader.picked_box().map_or(0, |(_, _, n)| n);
        let save = async || {
            pdf.flush();
            written(&app).await;
        };
        let undo = async |redo: bool| {
            let action = if redo { "win.pdf-redo" } else { "win.pdf-undo" };
            let _ = WidgetExt::activate_action(&app.window, action, None);
            save().await;
        };
        let drawn = filed(&path);
        println!("bench select drawn {drawn:?} {}", pdf.ring().state());

        // A lasso round the two lines, pictured before it is let go.
        let round_left = [(80.0, 180.0), (220.0, 180.0), (220.0, 280.0), (80.0, 280.0)];
        super::git::xtest(&lasso(round_left, false)).await;
        picture(&view, "select-lasso.png");
        super::git::xtest("up").await;
        println!(
            "bench select lassoed picked={} {}",
            picked(),
            pdf.ring().state()
        );
        picture(&view, "select-picked.png");

        // Dragged by one of them, both go down 40 px; one Undo puts both back.
        if let Some((x, y)) = at((150.0, LEFT[0])) {
            let steps = format!(
                "move {x:.0} {y:.0}; down; sleep 0.1; move {x:.0} {:.0}; sleep 0.1; \
                 move {x:.0} {:.0}; sleep 0.1; up",
                y + 20.0,
                y + 40.0
            );
            super::git::xtest(&steps).await;
        }
        save().await;
        let moved = filed(&path);
        let changed = moved.iter().filter(|p| !drawn.contains(p)).count();
        let others = [drawn[2], drawn[3]].iter().all(|p| moved.contains(p));
        println!("bench select moved={changed} others_same={others}");
        undo(false).await;
        println!("bench select undone one_step={}", filed(&path) == drawn);

        // Copied, and pasted a step down and right of it wherever the pointer is, which the
        // copies are then held as.
        let copied = reader.picked_box().map(|(_, b, _)| b);
        super::git::xtest(&format!("focus; {}; key ctrl+c", to((300.0, 500.0)))).await;
        let mime = reader
            .clipboard()
            .formats()
            .contain_mime_type("application/x-accent-pdf-ink");
        println!("bench select copied mime={mime}");
        super::git::xtest("key ctrl+v").await;
        save().await;
        let on_paper = reader.picked_box().is_some_and(|(page, b, _)| {
            let (w, h) = pdf.page_size(page).unwrap_or_default();
            b.left >= 0.0 && b.top >= 0.0 && b.right <= w && b.bottom <= h
        });
        let stepped = reader
            .picked_box()
            .zip(copied)
            .is_some_and(|((_, b, _), c)| {
                (b.left - c.left - 10.0).abs() < 0.5 && (b.top - c.top - 10.0).abs() < 0.5
            });
        println!(
            "bench select pasted +{} picked={} on_paper={on_paper} stepped={stepped}",
            filed(&path).len() - drawn.len(),
            picked()
        );
        undo(false).await;
        println!("bench select paste_undone {}", filed(&path) == drawn);

        // The rectangle picked by its edge, and deleted; its empty middle picks nothing.
        super::git::xtest(&click((RECT.0, 240.0))).await;
        println!("bench select edge picked={}", picked());
        super::git::xtest("key Delete").await;
        save().await;
        println!("bench select deleted -{}", drawn.len() - filed(&path).len());
        undo(false).await;
        super::git::xtest(&click((400.0, 240.0))).await;
        println!("bench select middle picked={}", picked());

        // A line, then the text added with Ctrl; cut, and pasted back in its place.
        let steps = format!(
            "{}; keydown ctrl; {}; keyup ctrl",
            click((150.0, LEFT[0])),
            click((TEXT.0 + 10.0, TEXT.1 + 8.0))
        );
        super::git::xtest(&steps).await;
        println!("bench select added picked={}", picked());
        super::git::xtest("key ctrl+x").await;
        save().await;
        println!("bench select cut -{}", drawn.len() - filed(&path).len());
        super::git::xtest(&format!("{}; key ctrl+v", to((300.0, 600.0)))).await;
        save().await;
        println!(
            "bench select cut_pasted {} picked={} in_place={}",
            filed(&path).len(),
            picked(),
            filed(&path) == drawn
        );
        undo(false).await;
        undo(false).await;
        println!("bench select cut_undone {}", filed(&path) == drawn);

        // A click on the paper lets go, and Escape then puts the tool down.
        super::git::xtest(&click((300.0, 700.0))).await;
        let let_go = picked();
        super::git::xtest("key Escape").await;
        println!(
            "bench select clicked_away picked={let_go} mode={:?}",
            pdf.mode()
        );
        app.pdf_mode(pdfview::Mode::Select);
        glib::timeout_future(Duration::from_secs(1)).await;

        // Both lines stretched by the group's bottom-right corner, 40 px each way, then back.
        super::git::xtest(&lasso(round_left, true)).await;
        if let Some((page, b, _)) = reader.picked_box()
            && let Some(r) = reader.widget_rect(page, &b)
        {
            let p = graphene::Point::new((r.x() + r.width()) as f32, (r.y() + r.height()) as f32);
            let p = view.compute_point(&app.window, &p).unwrap_or(p);
            let (x, y) = (f64::from(p.x()) + sx, f64::from(p.y()) + sy);
            let steps = format!(
                "move {x:.0} {y:.0}; down; sleep 0.1; move {:.0} {:.0}; sleep 0.1; \
                 move {:.0} {:.0}; sleep 0.1; up",
                x + 20.0,
                y + 20.0,
                x + 40.0,
                y + 40.0
            );
            super::git::xtest(&steps).await;
        }
        save().await;
        let widths = |path: &std::path::Path| -> Vec<i32> {
            let doc = pdf::PdfDoc::open(path).ok();
            let inks = doc.and_then(|d| d.inks(0).ok()).unwrap_or_default();
            let mut w: Vec<i32> = inks
                .iter()
                .map(|i| i.bounds.width().round() as i32)
                .collect();
            w.sort();
            w
        };
        println!("bench select scaled widths={:?}", widths(&path));
        undo(false).await;

        // Duplicated a step down and right, the copies held; then the arrows, a step each.
        super::git::xtest(&lasso(round_left, true)).await;
        super::git::xtest("key ctrl+d").await;
        save().await;
        let shifted = reader
            .picked_box()
            .map(|(_, b, _)| (b.left.round(), b.top.round()));
        println!(
            "bench select duplicated +{} picked={} box={shifted:?}",
            filed(&path).len() - drawn.len(),
            picked()
        );
        undo(false).await;
        super::git::xtest(&lasso(round_left, true)).await;
        super::git::xtest("key Right; sleep 0.2; key shift+Down").await;
        save().await;
        let nudged = filed(&path);
        let by: Vec<(i32, i32)> = (drawn.iter().zip(&nudged))
            .map(|(a, b)| (b.0 - a.0, b.1 - a.1))
            .filter(|d| *d != (0, 0))
            .collect();
        undo(false).await;
        undo(false).await;
        println!(
            "bench select nudged by={by:?} undone={}",
            filed(&path) == drawn
        );

        // The group with its handles, and a lasso being drawn, light and dark.
        super::git::xtest(&lasso(round_left, true)).await;
        use accent_core::config::Theme;
        for (name, theme) in [("light", Theme::Light), ("dark", Theme::Dark)] {
            crate::theme::apply(theme);
            glib::timeout_future(Duration::from_millis(300)).await;
            pdf.restyle();
            style::bench_painted(&app.window, String::new).await;
            glib::timeout_future(Duration::from_secs(1)).await;
            picture(&view, &format!("select-{name}.png"));
            let round_rect = [
                (330.0, 180.0),
                (470.0, 180.0),
                (470.0, 300.0),
                (330.0, 300.0),
            ];
            super::git::xtest(&lasso(round_rect, false)).await;
            picture(&view, &format!("select-lasso-{name}.png"));
            super::git::xtest("up").await;
        }
        crate::theme::apply(Theme::Light);

        if fill > 0 {
            timed(&app, &pdf, &reader, &path, fill).await;
        }
        bench_quit(&app);
    });
}

/// `n` strokes put down the first page's left edge, then all of them held and moved down a
/// point, and deleted, each as one step: the render thread's time for each.
async fn timed(
    app: &Rc<App>,
    pdf: &Rc<pdftab::PdfTab>,
    reader: &pdfview::PdfView,
    path: &std::path::Path,
    n: usize,
) {
    pdf.flush_blocking();
    if let Err(e) = super::pdf_draw::fill_first_page(path, n) {
        return println!("bench select fill_failed {e:#}");
    }
    // The tab reads the file again once its change has settled.
    glib::timeout_future(Duration::from_secs(3)).await;
    app.pdf_mode(pdfview::Mode::Read);
    app.pdf_mode(pdfview::Mode::Select);
    glib::timeout_future(Duration::from_secs(2)).await;
    let (_, held) = reader.inks_heard(0);
    let items: Vec<_> = (held.unwrap_or_default().iter())
        .filter(|(_, ink)| ink.bounds.left < 30.0)
        .map(crate::pdf::tools::Picked::stroke)
        .collect();
    let count = items.len();
    // Once the render thread's time has not moved for half a second: a reload reads every page's
    // comments, which on a long document is seconds of it.
    let quiet = async || {
        let mut last = super::pdf::render_cpu_ms();
        for _ in 0..120 {
            glib::timeout_future(Duration::from_millis(500)).await;
            let now = super::pdf::render_cpu_ms();
            if now - last < 2.0 {
                break;
            }
            last = now;
        }
    };
    let spent = async |what: &str, run: &dyn Fn()| {
        quiet().await;
        let cpu = super::pdf::render_cpu_ms();
        let t = Instant::now();
        run();
        quiet().await;
        println!(
            "bench select timed {what} strokes={count} render_cpu_ms={:.0} until_quiet_ms={}",
            super::pdf::render_cpu_ms() - cpu,
            t.elapsed().as_millis()
        );
    };
    reader.pick(0, items);
    spent("move", &|| {
        reader.nudge(0.0, 1.0);
    })
    .await;
    spent("delete", &|| pdf.delete_picked()).await;
}

/// What the page `view` is in shows, pictured into `$TMPDIR`.
fn picture(view: &gtk::Widget, name: &str) {
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    if let Some(overlay) = view.parent().and_then(|scroller| scroller.parent()) {
        super::diagram::shoot(&overlay, &std::path::Path::new(&dir).join(name));
    }
}
