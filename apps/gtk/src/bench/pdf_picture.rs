//! The pictures drill, `ACCENT_BENCH_PDF=picture:<rel>`: on the first page, a picture pasted from
//! the clipboard through XTEST, pasted again and undone, stretched by a corner and by an edge,
//! turned by the ring with Shift, copied, cut and pasted again; a PNG and a JPEG, and an image
//! itself, dropped as another application drops them; an image row dragged from the Files tree;
//! an eraser pass over a picture; each against the file. A held picture is pictured light and
//! dark into `$TMPDIR`. Last, a 12-megapixel JPEG pasted as one: the render thread's time to draw
//! the screen again over it, and a tile's alone. Writes the file, and `picture-drill.png` into the
//! vault.

use std::time::Instant;

use super::pdf::{drag_steps, file_drop, opened, tree_row, value_drop, written};
use super::*;
use accent_core::pdf::{self, Rect};

/// The picture copied onto the clipboard first: 200 × 100 pixels, its left half red in noise a
/// deflate cannot squeeze, so a copy written twice would show in the file's size, and its right
/// half clear.
fn half_red() -> gdk::Texture {
    let mut seed = 1u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 24) as u8
    };
    let data: Vec<u8> = (0..200 * 100)
        .flat_map(|i| match i % 200 < 100 {
            true => [200 + noise() % 56, noise() % 40, noise() % 40, 255],
            false => [0; 4],
        })
        .collect();
    let bytes = glib::Bytes::from_owned(data);
    gdk::MemoryTexture::new(200, 100, gdk::MemoryFormat::R8g8b8a8, &bytes, 800).upcast()
}

/// A JPEG of `w` × `h` pixels, gradients with noise in them as a photo has, through gdk-pixbuf's
/// own encoder; `None` where it has none.
fn photo(w: i32, h: i32) -> Option<Vec<u8>> {
    let mut seed = 7u32;
    let rgb: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (seed >> 29) as u8;
            let (x, y) = (i % w, i / w);
            [
                (x * 255 / w) as u8 ^ n,
                (y * 255 / h) as u8,
                n.wrapping_mul(8),
            ]
        })
        .collect();
    let bytes = glib::Bytes::from_owned(rgb);
    let colourspace = gtk::gdk_pixbuf::Colorspace::Rgb;
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_bytes(&bytes, colourspace, false, 8, w, h, w * 3);
    (pixbuf.save_to_bufferv("jpeg", &[("quality", "90")]))
        .inspect_err(|e| println!("bench picture jpeg_unavailable {e}"))
        .ok()
}

/// The pictures of `path`'s first page, in `/Annots` order.
fn filed(path: &std::path::Path) -> Vec<pdf::PlacedPicture> {
    let doc = pdf::PdfDoc::open(path).ok();
    doc.and_then(|doc| doc.pictures(0).ok()).unwrap_or_default()
}

fn middle(b: Rect) -> (f32, f32) {
    ((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0)
}

fn near((x, y): (f32, f32), (u, v): (f32, f32)) -> bool {
    (x - u).abs() < 1.0 && (y - v).abs() < 1.0
}

/// Whether the right half of the picture in `b` on `path`'s first page shows the paper through,
/// as `half_red`'s clear half does: no pixel of a block in its middle dark, as one whose alpha was
/// lost draws it.
fn clear_half(path: &std::path::Path, b: Rect) -> bool {
    let page = pdf::PdfDoc::open(path).and_then(|doc| doc.render_page(0, 1.0, pdf::Theme::Plain));
    let Ok(page) = page else {
        return false;
    };
    let (x, y) = ((b.left + 0.75 * b.width()) as usize, middle(b).1 as usize);
    let w = page.width as usize;
    (y - 2..=y + 2)
        .flat_map(|y| (x - 2..=x + 2).map(move |x| (y * w + x) * 4))
        .all(|at| page.data[at..at + 3].iter().any(|&c| c > 60))
}

/// Whether the last picture filed is twice as wide as tall, as every picture here is.
fn proportioned(pictures: &[pdf::PlacedPicture]) -> bool {
    let corners = pictures.last().map(|p| pdf::picture_corners(p.place));
    corners.is_some_and(|[a, b, _, d]| {
        let side = |p: (f32, f32), q: (f32, f32)| (q.0 - p.0).hypot(q.1 - p.1);
        (side(a, b) / side(a, d) - 2.0).abs() < 0.02
    })
}

pub(super) fn bench_pdf_picture(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let path = app.root().join(&rel);
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench picture no_tab");
            return bench_quit(&app);
        };
        pdf.goto_page(0);
        let view = pdf.views()[0].clone();
        let Some(reader) = view.downcast_ref::<pdfview::PdfView>().cloned() else {
            return bench_quit(&app);
        };
        glib::timeout_future(Duration::from_secs(2)).await;
        let save = async || {
            pdf.flush();
            written(&app).await;
        };
        let run = async |action: &str| {
            let _ = WidgetExt::activate_action(&app.window, action, None);
            save().await;
        };
        let keys = async |keys: &str| {
            view.grab_focus();
            super::git::xtest(&format!("focus; {keys}")).await;
            save().await;
        };
        let held = || reader.picked_box().map(|(_, b, n)| (b, n));
        // A point of the view on the screen, which is where XTEST aims.
        let (sx, sy) = app.window.surface_transform();
        let screen = |x: f32, y: f32| {
            let p = graphene::Point::new(x, y);
            let p = view.compute_point(&app.window, &p).unwrap_or(p);
            (f64::from(p.x()) + sx, f64::from(p.y()) + sy)
        };
        // What the Select tool holds, on the screen: left, top, right and bottom.
        let frame = || {
            let (page, b, _) = reader.picked_box()?;
            let r = reader.widget_rect(page, &b)?;
            let (l, t) = screen(r.x() as f32, r.y() as f32);
            Some((l, t, l + f64::from(r.width()), t + f64::from(r.height())))
        };
        let drag = |from: (f64, f64), to: (f64, f64)| {
            let (x, y) = from;
            let step = |t: f64| format!("move {:.0} {:.0}", x + (to.0 - x) * t, y + (to.1 - y) * t);
            format!(
                "move {x:.0} {y:.0}; down; sleep 0.1; {}; sleep 0.1; {}; sleep 0.1; up",
                step(0.5),
                step(1.0)
            )
        };

        // A picture on the clipboard, which the ring's Paste offers.
        reader.clipboard().set_texture(&half_red());
        app.pdf_mode(pdfview::Mode::Select);
        glib::timeout_future(Duration::from_millis(500)).await;
        println!(
            "bench picture pasteable={} {}",
            pdf.pasteable(),
            pdf.ring().state()
        );

        // Pasted at its natural size in the middle of the page on screen, wherever the pointer
        // is, and held.
        let (x, y) = screen(20.0, 20.0);
        keys(&format!("move {x:.0} {y:.0}; key ctrl+v")).await;
        let first = held();
        let centred = first
            .zip(reader.visible_part(0))
            .is_some_and(|((b, _), shown)| near(middle(b), middle(shown)));
        let (b, n) = first.unwrap_or((Rect::ZERO, 0));
        println!(
            "bench picture pasted +{} picked={n} size={:.0}x{:.0} centred={centred} clear={}",
            filed(&path).len(),
            b.width(),
            b.height(),
            clear_half(&path, b)
        );

        // Again, a step down and right of it; two Undos take both off.
        keys("key ctrl+v").await;
        let stepped = (first.zip(held()))
            .is_some_and(|((a, _), (b, _))| near((b.left, b.top), (a.left + 10.0, a.top + 10.0)));
        println!(
            "bench picture again stepped={stepped} filed={}",
            filed(&path).len()
        );
        run("win.pdf-undo").await;
        run("win.pdf-undo").await;
        println!("bench picture undone filed={}", filed(&path).len());

        // Stretched by its bottom-right corner, then by its right edge, it keeps its proportions.
        keys("key ctrl+v").await;
        if let Some((_, _, r, b)) = frame() {
            super::git::xtest(&drag((r, b), (r + 40.0, b + 40.0))).await;
        }
        save().await;
        let corner = proportioned(&filed(&path));
        if let Some((_, t, r, b)) = frame() {
            super::git::xtest(&drag((r, (t + b) / 2.0), (r + 30.0, (t + b) / 2.0))).await;
        }
        save().await;
        let edge = proportioned(&filed(&path));
        let (b, _) = held().unwrap_or((Rect::ZERO, 0));
        println!(
            "bench picture scaled corner_kept={corner} edge_kept={edge} size={:.0}x{:.0}",
            b.width(),
            b.height()
        );

        // The ring beyond its top-right corner turned a quarter round its middle, Shift held:
        // ninety degrees exactly.
        if let Some((l, t, r, b)) = frame() {
            // The ring is 12 px out from the corner, as the diagram's is.
            let (hx, hy) = (r + 12.0, t - 12.0);
            let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
            let arc: Vec<String> = (1..=6)
                .map(|i| {
                    let (sin, cos) = (f64::from(i) * 15.0 + 2.0).to_radians().sin_cos();
                    let (vx, vy) = (hx - cx, hy - cy);
                    let (x, y) = (cx + vx * cos - vy * sin, cy + vx * sin + vy * cos);
                    format!("move {x:.0} {y:.0}; sleep 0.05")
                })
                .collect();
            let steps = format!(
                "move {hx:.0} {hy:.0}; down; sleep 0.1; keydown shift; {}; sleep 0.1; up; \
                 keyup shift",
                arc.join("; ")
            );
            super::git::xtest(&steps).await;
        }
        save().await;
        let turned = (filed(&path).last()).map_or(0.0, |p| p.place[1].atan2(p.place[0]));
        println!("bench picture turned={:.1}", turned.to_degrees());

        // Copied and pasted, the copy shares its image: the file grows by a stamp, not a picture.
        let size = || std::fs::metadata(&path).map_or(0, |m| m.len());
        let (count, before) = (filed(&path).len(), size());
        keys("key ctrl+c; sleep 0.3; key ctrl+v").await;
        let grew = size().saturating_sub(before);
        println!(
            "bench picture copied +{} shared={} grew={grew}",
            filed(&path).len() - count,
            grew < 4096
        );
        // Cut and pasted, back where it was.
        let (was, count) = (held(), filed(&path).len());
        keys("key ctrl+x").await;
        let cut = count - filed(&path).len();
        keys("key ctrl+v").await;
        let in_place = was
            .zip(held())
            .is_some_and(|((a, _), (b, _))| near(middle(a), middle(b)));
        println!(
            "bench picture cut -{cut} +{} in_place={in_place}",
            filed(&path).len() + cut - count
        );

        // A PNG and a JPEG dropped from another application, the first centred on the pointer,
        // the second a step on from it, the JPEG written as it is.
        let dir = std::path::PathBuf::from(std::env::var("TMPDIR").unwrap_or("/tmp".into()));
        let png = dir.join("picture-drill.png");
        let jpg = dir.join("picture-drill.jpg");
        let _ = half_red().save_to_png(&png);
        if let Some(jpeg) = photo(120, 60) {
            let _ = std::fs::write(&jpg, jpeg);
        }
        let count = filed(&path).len();
        let at = (view.width() as f32 / 2.0, view.height() as f32 / 3.0);
        let taken = file_drop(&view, &[&png, &jpg], at);
        save().await;
        let dropped = filed(&path);
        let pointer = reader.page_point(f64::from(at.0), f64::from(at.1));
        let at_pointer = (dropped.get(count))
            .zip(pointer)
            .is_some_and(|(p, (_, x, y))| near(middle(p.bounds), (x, y)));
        let bytes = std::fs::read(&path).unwrap_or_default();
        let dct = bytes.windows(10).any(|w| w == b"/DCTDecode");
        println!(
            "bench picture dropped taken={taken} +{} at_pointer={at_pointer} jpeg={}",
            dropped.len() - count,
            if dct { "DCTDecode" } else { "none" }
        );
        // An image itself, as a browser may drag one.
        let count = filed(&path).len();
        let taken = value_drop(&view, half_red().to_value(), (at.0, at.1 * 2.0));
        save().await;
        println!(
            "bench picture texture_dropped taken={taken} +{}",
            filed(&path).len() - count
        );

        // An image row of the Files tree dragged onto the page: in as a picture, opening no tab.
        let row = "picture-drill.png";
        let _ = std::fs::copy(&png, app.root().join(row));
        for _ in 0..50 {
            if tree_row(&app, row).is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let count = filed(&path).len();
        let from = tree_row(&app, row).map(|(x, y)| (f64::from(x) + sx, f64::from(y) + sy));
        if let Some((x, y)) = from {
            let (tx, ty) = screen(view.width() as f32 / 2.0, view.height() as f32 / 2.0);
            super::git::xtest(&drag_steps(&format!("{x} {y}"), &format!("{tx} {ty}"))).await;
        }
        save().await;
        println!(
            "bench picture tree found={} +{} opened_tab={}",
            from.is_some(),
            filed(&path).len() - count,
            app.doc_for(row).is_some()
        );

        // The eraser passes over a picture and leaves it.
        let count = filed(&path).len();
        app.pdf_mode(pdfview::Mode::Eraser);
        glib::timeout_future(Duration::from_millis(300)).await;
        if let Some(p) = filed(&path).last()
            && let Some(r) = reader.widget_rect(0, &p.bounds)
        {
            let y = r.y() as f32 + r.height() as f32 / 2.0;
            let from = screen(r.x() as f32 - 10.0, y);
            let to = screen((r.x() + r.width()) as f32 + 10.0, y);
            super::git::xtest(&drag(from, to)).await;
        }
        save().await;
        println!("bench picture erased kept={}", filed(&path).len() == count);

        // A picture held, light and dark.
        app.pdf_mode(pdfview::Mode::Select);
        if let Some(p) = filed(&path).first()
            && let Some(r) = reader.widget_rect(0, &p.bounds)
        {
            let (x, y) = screen(
                r.x() as f32 + r.width() as f32 / 2.0,
                r.y() as f32 + r.height() as f32 / 2.0,
            );
            super::git::xtest(&format!("move {x:.0} {y:.0}; down; up; sleep 0.3")).await;
        }
        use accent_core::config::Theme;
        for (name, theme) in [("light", Theme::Light), ("dark", Theme::Dark)] {
            crate::theme::apply(theme);
            glib::timeout_future(Duration::from_millis(300)).await;
            pdf.restyle();
            style::bench_painted(&app.window, String::new).await;
            glib::timeout_future(Duration::from_secs(1)).await;
            shoot(&view, &format!("picture-{name}.png"));
        }
        crate::theme::apply(Theme::Light);

        timed(&app, &pdf, &path).await;
        bench_quit(&app);
    });
}

/// A 12-megapixel JPEG pasted onto the first page as another application's copy, its bytes as
/// they are: the render thread's time to draw the screen again in another theme over it against
/// without it, and the time of one 512 px tile over it at 2 px a point on a copy of the file, on
/// a worker, against one of the same page clear of it.
async fn timed(app: &Rc<App>, pdf: &Rc<pdftab::PdfTab>, path: &std::path::Path) {
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
    // Every tile on screen drawn again, its colours being baked in, by a theme from Light to
    // Dark, and back after.
    let redrawn = async || {
        use accent_core::config::Theme;
        quiet().await;
        let cpu = super::pdf::render_cpu_ms();
        crate::theme::apply(Theme::Dark);
        pdf.restyle();
        quiet().await;
        let spent = super::pdf::render_cpu_ms() - cpu;
        crate::theme::apply(Theme::Light);
        pdf.restyle();
        spent
    };
    let Some(jpeg) = photo(4000, 3000) else {
        return;
    };
    let megabytes = jpeg.len() as f64 / 1e6;
    let without = redrawn().await;
    let provider = gdk::ContentProvider::for_bytes("image/jpeg", &glib::Bytes::from_owned(jpeg));
    let clipboard = pdf.views()[0].clipboard();
    let _ = clipboard.set_content(Some(&provider));
    let _ = WidgetExt::activate_action(&app.window, "win.pdf-paste", None);
    pdf.flush();
    written(app).await;
    let bytes = std::fs::read(path).unwrap_or_default();
    let dct = bytes.windows(10).filter(|w| *w == b"/DCTDecode").count();
    let with = redrawn().await;
    let copy = path.with_file_name("picture-render.pdf");
    let _ = std::fs::write(&copy, &bytes);
    let tile = crate::work::off_thread("time a tile", move || {
        let doc = pdf::PdfDoc::open(&copy).ok()?;
        let cpu = || {
            let stat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
            stat.split_whitespace().next()?.parse::<f64>().ok()
        };
        // The median of five, `y` px down the page.
        let timed = |y: i32| {
            let mut runs: Vec<(f64, f64)> = (0..5)
                .filter_map(|_| {
                    let (t, c) = (Instant::now(), cpu()?);
                    doc.render_tile(0, 2.0, 0, y, 512, 512, pdf::Theme::Plain)
                        .ok()?;
                    Some((t.elapsed().as_secs_f64() * 1e3, (cpu()? - c) / 1e6))
                })
                .collect();
            runs.sort_by(|a, b| a.0.total_cmp(&b.0));
            runs.get(runs.len() / 2).copied().unwrap_or_default()
        };
        // Over its top-left part, and down past it.
        let measured = (timed(512), timed(1536));
        let _ = std::fs::remove_file(&copy);
        Some(measured)
    })
    .await
    .flatten()
    .unwrap_or_default();
    let ((over, over_cpu), (clear, _)) = tile;
    println!(
        "bench picture render jpeg=4000x3000 jpeg_mb={megabytes:.1} dct_images={dct} \
         redraw_cpu_ms={with:.0} without_ms={without:.0} tile_ms={over:.0} \
         tile_cpu_ms={over_cpu:.0} clear_tile_ms={clear:.0}"
    );
}

/// What the page `view` is in shows, pictured into `$TMPDIR`.
fn shoot(view: &gtk::Widget, name: &str) {
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    if let Some(overlay) = view.parent().and_then(|scroller| scroller.parent()) {
        super::diagram::shoot(&overlay, &std::path::Path::new(&dir).join(name));
    }
}
