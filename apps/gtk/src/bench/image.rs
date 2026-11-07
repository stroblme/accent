//! A drill over a zoomed image whose file changes size under it.

use super::*;
use crate::look::Look;
use accent_core::config::Theme;

/// `ACCENT_BENCH_IMAGE=<rel_png>,<rel_other_png>` zooms off fit, replaces the file with
/// a differently sized image and prints requested/drawn sizes and zoom readout. This
/// checks that reload updates size requests derived from the old paintable.
///
/// `=zoom:<rel_svg>,…` steps each SVG in 30 times, then 40, then fits; print immediate
/// and settled state and render latency. Each run should render only its final zoom.
/// `=anchor:<rel>` zooms 40 steps in and 10 out around a point two fifths into the view,
/// then by chord about the centre. Print `under` fractions to check anchor stability.
/// Repeat pinches to 1.5× zoom and half that value (`App::zoom_image_to`).
pub(super) fn bench_image(app: &Rc<App>, arg: &str) {
    if let Some(rels) = arg.strip_prefix("zoom:") {
        return zoom_svgs(app, rels);
    }
    if let Some(rel) = arg.strip_prefix("anchor:") {
        return zoom_around(app, rel);
    }
    let Some((rel, other)) = arg.split_once(',') else {
        println!("bench image needs <rel_png>,<rel_other_png>");
        return bench_quit(app);
    };
    app.open_path(rel);
    let (app, rel, other) = (app.clone(), rel.to_string(), other.to_string());
    // The picture is loaded from a file, so nothing about it is known until the frame after.
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        image_state(&app, "fitted");
        let _ = WidgetExt::activate_action(&app.window, "win.zoom-in", None);
        // A frame later: what a widget is drawn at is last frame's allocation until the layout
        // the zoom queued has run, and the whole question here is the size it ends up at.
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            image_state(&app, "zoomed");
            let root = app.root();
            if let Err(e) = std::fs::copy(root.join(&other), root.join(&rel)) {
                println!("bench image cannot_replace {e}");
                return bench_quit(&app);
            }
            // The watcher's event and the fetch behind it, then the frame that lays it out.
            glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                image_state(&app, "reloaded");
                bench_quit(&app);
            });
        });
    });
}

/// See `=zoom:` on [`bench_image`].
fn zoom_svgs(app: &Rc<App>, rels: &str) {
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    let app = app.clone();
    glib::spawn_future_local(async move {
        for rel in rels {
            app.open_path(&rel);
            glib::timeout_future(Duration::from_millis(600)).await;
            image_state(&app, "fitted");
            for (action, steps) in [
                ("win.zoom-in", 30),
                ("win.zoom-in", 40),
                ("win.zoom-reset", 1),
            ] {
                let before = drawn_pixels(&app);
                for _ in 0..steps {
                    let _ = WidgetExt::activate_action(&app.window, action, None);
                }
                let t = Instant::now();
                image_state(&app, "stepped");
                while drawn_pixels(&app) == before && t.elapsed() < Duration::from_secs(5) {
                    glib::timeout_future(Duration::from_millis(5)).await;
                }
                let landed = ms_since(t);
                glib::timeout_future(Duration::from_millis(600)).await;
                image_state(&app, &format!("settled landed_ms={landed:.0}"));
            }
        }
        bench_quit(&app);
    });
}

/// See `=anchor:` on [`bench_image`].
fn zoom_around(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(600)).await;
        let Some(Doc::Image(image)) = app.active_doc() else {
            println!("bench image anchor no_tab");
            return bench_quit(&app);
        };
        let Ok(scroller) = crate::loading::content(&image.page).downcast::<gtk::ScrolledWindow>()
        else {
            return bench_quit(&app);
        };
        let at = (
            f64::from(scroller.width()) * 0.4,
            f64::from(scroller.height()) * 0.4,
        );
        anchor_state(&image, &scroller, at, "fitted");
        for (out, steps) in [(false, 40), (true, 10)] {
            for _ in 0..steps {
                app.zoom_image(&image, Some(out), Some(at));
                // A frame, for the viewport to take the picture's new size.
                glib::timeout_future(Duration::from_millis(100)).await;
                anchor_state(&image, &scroller, at, &format!("out={out}"));
            }
        }
        let middle = (
            f64::from(scroller.width()) / 2.0,
            f64::from(scroller.height()) / 2.0,
        );
        anchor_state(&image, &scroller, middle, "before_chord");
        let _ = WidgetExt::activate_action(&app.window, "win.zoom-in", None);
        glib::timeout_future(Duration::from_millis(100)).await;
        anchor_state(&image, &scroller, middle, "chord");
        for scale in [1.5, 0.5] {
            let from = crate::zoom::image_zoom(&image).unwrap_or(1.0);
            let zoom = crate::zoom::pinched_zoom(from, scale);
            app.zoom_image_to(&image, Some(zoom), Some(at));
            glib::timeout_future(Duration::from_millis(100)).await;
            anchor_state(&image, &scroller, at, &format!("pinch={scale}"));
        }
        bench_quit(&app);
    });
}

/// One `bench image anchor` line: the image's drawn size, the scroll offsets and where `at`, a
/// point in `scroller`, falls in the image.
fn anchor_state(image: &doc::Viewer, scroller: &gtk::ScrolledWindow, at: (f64, f64), when: &str) {
    let picture = picture_of(&image.page);
    let paintable = picture.as_ref().and_then(|p| p.paintable());
    let origin = picture
        .as_ref()
        .and_then(|p| p.compute_point(scroller, &graphene::Point::zero()));
    let (Some(picture), Some(paintable), Some(origin)) = (picture, paintable, origin) else {
        return println!("bench image anchor {when} no_picture");
    };
    let (pw, ph) = (f64::from(picture.width()), f64::from(picture.height()));
    let (iw, ih) = (
        f64::from(paintable.intrinsic_width()),
        f64::from(paintable.intrinsic_height()),
    );
    // Fitted, the picture fills the view and draws the image scaled down in its middle.
    let scale = (pw / iw).min(ph / ih);
    let scale = match image.zoom.get() {
        Some(_) => scale,
        None => scale.min(1.0),
    };
    let (dw, dh) = (iw * scale, ih * scale);
    let x = f64::from(origin.x()) + (pw - dw) / 2.0;
    let y = f64::from(origin.y()) + (ph - dh) / 2.0;
    println!(
        "bench image anchor {when} label={:?} drawn={dw:.0}x{dh:.0} view={}x{} scroll={:.0},{:.0} \
         under={:.4},{:.4}",
        crate::zoom::image_zoom_label(image),
        scroller.width(),
        scroller.height(),
        scroller.hadjustment().value(),
        scroller.vadjustment().value(),
        (at.0 - x) / dw,
        (at.1 - y) / dh,
    );
}

/// The pixels of the texture the picture in front draws, an SVG's inside its paintable.
fn drawn_pixels(app: &Rc<App>) -> (i32, i32) {
    let Some(Doc::Image(image)) = app.active_doc() else {
        return (0, 0);
    };
    picture_of(&image.page)
        .and_then(|p| p.paintable())
        .as_ref()
        .and_then(texture_of)
        .map_or((0, 0), |t| (t.width(), t.height()))
}

/// The texture `paintable` draws: itself, or an SVG's inside [`crate::look::Scaled`].
fn texture_of(paintable: &gdk::Paintable) -> Option<gdk::Texture> {
    paintable
        .downcast_ref::<gdk::Texture>()
        .cloned()
        .or_else(|| {
            paintable
                .downcast_ref::<crate::look::Scaled>()
                .map(|s| s.texture())
        })
}

/// Print original/requested/drawn/texture sizes, render count and zoom readout.
fn image_state(app: &Rc<App>, when: &str) {
    let Some(Doc::Image(image)) = app.active_doc() else {
        println!("bench image no_tab {when}");
        return;
    };
    let Some(picture) = picture_of(&image.page) else {
        println!("bench image no_picture {when}");
        return;
    };
    let paintable = picture.paintable();
    let intrinsic = paintable
        .as_ref()
        .map_or((0, 0), |p| (p.intrinsic_width(), p.intrinsic_height()));
    let pixels = drawn_pixels(app);
    println!(
        "bench image {when} file={}x{} request={}x{} drawn={}x{} pixels={}x{} shows={} label={:?}",
        intrinsic.0,
        intrinsic.1,
        picture.width_request(),
        picture.height_request(),
        picture.width(),
        picture.height(),
        pixels.0,
        pixels.1,
        image.shows.get(),
        crate::zoom::image_zoom_label(&image),
    );
}

/// `ACCENT_BENCH_IMAGE_LOOK=<rel>,…` measures initial decode/classify/recolour time
/// under Dark, then prints classification, recolouring, image/texture sizes, pixel
/// (2,2) and paintable type under Light, Dark and Solarized, with inversion too.
/// `ms` times worker recolouring of the decoded texture.
pub(super) fn bench_image_look(app: &Rc<App>, rels: &str) {
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    let app = app.clone();
    glib::spawn_future_local(async move {
        for rel in rels {
            crate::theme::apply(Theme::Dark);
            let t = Instant::now();
            app.open_path(&rel);
            let Some(Doc::Image(image)) = app.active_doc() else {
                println!("bench image_look {rel} no_tab");
                continue;
            };
            while image.image.borrow().is_none() && t.elapsed() < Duration::from_secs(20) {
                glib::timeout_future(Duration::from_millis(2)).await;
            }
            println!("bench image_look {rel} appeared_ms={:.1}", ms_since(t));
            for theme in [Theme::Light, Theme::Dark, Theme::Solarized] {
                crate::theme::apply(theme);
                app.restyle_all();
                glib::timeout_future(Duration::from_millis(800)).await;
                look_state(&image, &rel, theme, false);
                let _ = WidgetExt::activate_action(&app.window, "win.image-invert", None);
                glib::timeout_future(Duration::from_millis(800)).await;
                look_state(&image, &rel, theme, true);
                let _ = WidgetExt::activate_action(&app.window, "win.image-invert", None);
            }
        }
        bench_quit(&app);
    });
}

/// One `bench image_look` line for the image in `image`'s tab.
fn look_state(image: &doc::Viewer, rel: &str, theme: Theme, inverted: bool) {
    let picture = picture_of(&image.page);
    let scale = picture.as_ref().map_or(1, |p| p.scale_factor());
    let shown = picture.and_then(|p| p.paintable());
    let read = image.image.borrow().clone();
    let (Some(shown), Some((path, original))) = (shown, read) else {
        return println!("bench image_look {rel} theme={theme:?} not_shown");
    };
    let verdict = match crate::look::verdict(&path) {
        Some(v) => format!(
            "document={} paper={:.2} colours={}",
            v.document, v.paper, v.colours
        ),
        None => "document=- paper=- colours=-".to_string(),
    };
    // An SVG's texture is drawn at the display's scale, inside a paintable of its logical size.
    let texture = texture_of(&shown);
    let px = texture
        .as_ref()
        .map(|t| {
            let mut downloader = gdk::TextureDownloader::new(t);
            downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
            let (bytes, stride) = downloader.download_bytes();
            let at = 2 * stride + 2 * 4;
            format!(
                "{},{},{},{}",
                bytes[at],
                bytes[at + 1],
                bytes[at + 2],
                bytes[at + 3]
            )
        })
        .unwrap_or_else(|| "-".to_string());
    // An SVG keeps no plain drawing to compare with: recoloured where the look asks for it.
    let recoloured = match &original {
        Some(original) => texture.as_ref() != Some(&original.texture),
        None => Look::now().palette(true, inverted).is_some(),
    };
    let pixels = texture.map_or((0, 0), |t| (t.width(), t.height()));
    let t = Instant::now();
    let zoom = crate::look::drawn_zoom(&path, image.zoom.get(), image.size.get(), scale);
    let _ = crate::look::show(&path, original, Look::now(), inverted, scale, zoom, None);
    println!(
        "bench image_look {rel} theme={theme:?} dark={} inverted={inverted} {verdict} \
         recoloured={recoloured} size={}x{} pixels={}x{} px(2,2)={px} type={} ms={:.1}",
        adw::StyleManager::default().is_dark(),
        shown.intrinsic_width(),
        shown.intrinsic_height(),
        pixels.0,
        pixels.1,
        shown.type_().name(),
        ms_since(t),
    );
}

/// `ACCENT_BENCH_PREVIEW_LOOK=<rel_note>` prints each picture's pixels as drawn and the reads
/// the pictures took, across repeated renders, themes and inversion.
///
/// - `=hold:<rel_note>` reports image positions and waits 40 s for XTEST menu input,
///   printing inversion and requests every 2 s.
/// - `=zoom:<rel_note>` shows the preview alone in its pane at zoom 1, then 2, each held 10 s
///   once its pictures are read, printing the pixels each holds.
/// - `=time:<rel_note>` renders five times, reporting `render_ms`, `shown_ms`, `stall_ms`
///   through 200 ms after load, and whether the latest simultaneous render wins.
/// - `=change:<note>,<img>,<other>` replaces an image; `=gone:<note>,<img>[,<to>]` deletes
///   or renames it. Remote changes run on the host to exercise its watcher.
pub(super) fn bench_preview_look(app: &Rc<App>, rel: &str) {
    if let Some(arg) = rel.strip_prefix("change:") {
        return change_preview(app, "cp", arg);
    }
    if let Some(arg) = rel.strip_prefix("gone:") {
        let op = match arg.split(',').count() {
            2 => "rm",
            _ => "mv",
        };
        return change_preview(app, op, arg);
    }
    if let Some(rel) = rel.strip_prefix("zoom:") {
        return zoom_preview(app, rel);
    }
    let (hold, rel) = match rel.strip_prefix("hold:") {
        Some(rel) => (true, rel),
        None => (false, rel),
    };
    let (time, rel) = match rel.strip_prefix("time:") {
        Some(rel) => (true, rel),
        None => (false, rel),
    };
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        super::preview::beside(&app).await;
        if hold {
            return hold_preview(&app).await;
        }
        if time {
            return time_preview(&app).await;
        }
        preview_look(&app, "first").await;
        if let Some(rendered) = super::preview::shown(&app) {
            app.render_rendered(&rendered, true);
        }
        preview_look(&app, "rerender").await;
        for theme in [Theme::Light, Theme::Dark, Theme::Solarized] {
            crate::theme::apply(theme);
            app.restyle_all();
            preview_look(&app, &format!("{theme:?}")).await;
        }
        // The image menu's own route: the vault key each picture names.
        let mut keys: Vec<String> = (figures(&app).into_iter())
            .flat_map(|(figures, _)| figures.into_iter().filter_map(|f| f.key))
            .collect();
        // Once each: a diagram embedded twice, at two pages, is one file.
        keys.sort();
        keys.dedup();
        for key in &keys {
            app.invert_image(key);
        }
        preview_look(&app, "inverted").await;
        bench_quit(&app);
    });
}

/// See `=zoom:` on [`bench_preview_look`].
fn zoom_preview(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        let t = Instant::now();
        while app.active().is_none() && t.elapsed() < Duration::from_secs(10) {
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        let Some(tab) = app.active() else {
            println!("bench preview_look zoom no_tab");
            return bench_quit(&app);
        };
        let rendered = (app.rendered_of(&tab.page)).unwrap_or_else(|| {
            app.open_rendered(&crate::rendered::Note::Tab(tab.clone()), Opened::Kept)
        });
        app.reveal_page(&rendered.page);
        for zoom in [1.0, 2.0] {
            app.set_zoom(zoom);
            preview_look(&app, &format!("zoom={zoom}")).await;
            println!("bench preview_look zoom={zoom} held");
            glib::timeout_future(Duration::from_secs(10)).await;
        }
        bench_quit(&app);
    });
}

/// How many reads the preview tab's pictures took.
fn requests(app: &App) -> u32 {
    figures(app).map_or(0, |(_, reads)| reads)
}

/// The preview tab's pictures and the reads they took.
fn figures(app: &App) -> Option<(Vec<crate::preview::Probe>, u32)> {
    let rendered = super::preview::shown(app)?;
    let preview = rendered.preview();
    Some(preview.as_ref()?.figures())
}

/// Whether the preview tab's render is in and none of its pictures is being read.
fn settled(app: &App) -> bool {
    let rendered = super::preview::shown(app);
    let preview = rendered.as_ref().map(|r| r.preview());
    let settled = preview.is_some_and(|p| p.as_ref().is_some_and(|p| p.settled()));
    settled && figures(app).is_some_and(|(f, _)| f.iter().all(|f| !f.pending))
}

/// The preview tab's pictures as its page names them, and whether each shows its file.
pub(super) async fn preview_pictures(app: &Rc<App>) -> Vec<(String, bool)> {
    (figures(app).into_iter())
        .flat_map(|(figures, _)| figures.into_iter().map(|f| (f.uri, f.loaded)))
        .collect()
}

/// Wait until no picture is being read, then print a `bench preview_look` line per picture as
/// drawn, with how long that took (`ms`, from 300 ms after the render was asked for).
async fn preview_look(app: &Rc<App>, when: &str) {
    // The render, or a recolouring, is asked for on an idle, so the old page is still up for a
    // moment.
    glib::timeout_future(Duration::from_millis(300)).await;
    let started = Instant::now();
    while !settled(app) && started.elapsed() < Duration::from_secs(20) {
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    let ms = ms_since(started);
    let Some((figures, reads)) = figures(app) else {
        return println!("bench preview_look {when} no_preview");
    };
    for f in figures {
        println!(
            "bench preview_look {when} dark={} {} loaded={} animated={} size={}x{} \
             pixels={}x{} px(2,2)={} px(centre)={} reads={reads} ms={ms:.0}",
            adw::StyleManager::default().is_dark(),
            f.uri,
            f.loaded,
            f.animated,
            f.size.0,
            f.size.1,
            f.pixels.0,
            f.pixels.1,
            f.corner,
            f.centre,
        );
    }
}

/// See `=change:` and `=gone:` on [`bench_preview_look`]: `arg` is the note, then the files
/// `op` (`cp`, `rm` or `mv`) is run on, the copy's source last.
fn change_preview(app: &Rc<App>, op: &'static str, arg: &str) {
    let mut args = arg.split(',').map(str::to_string);
    let rel = args.next().unwrap_or_default();
    let mut files: Vec<String> = args.collect();
    if op == "cp" {
        files.reverse();
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        // A remote vault opens nothing while it is still connecting.
        let t = Instant::now();
        while !app.reconciled.get() && t.elapsed() < Duration::from_secs(60) {
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(400)).await;
        super::preview::beside(&app).await;
        preview_look(&app, "before").await;
        if let Err(e) = change_files(&app, op, &files) {
            println!("bench preview_look cannot_change {e}");
            return bench_quit(&app);
        }
        // The watcher's event, and the render it asks for.
        glib::timeout_future(Duration::from_millis(1500)).await;
        preview_look(&app, "changed").await;
        bench_quit(&app);
    });
}

/// Run `op` on the vault files `rels` where they are: on a remote vault's host over ssh, as a
/// program there would, so the news comes from the host's watcher.
fn change_files(app: &Rc<App>, op: &str, rels: &[String]) -> Result<(), String> {
    let root = app.root();
    let paths = rels.iter().map(|rel| root.join(rel));
    let status = match app.vault().and_then(|v| v.remote()) {
        Some(remote) => std::process::Command::new("ssh")
            .arg(remote.url().destination())
            .arg(op)
            .args(paths)
            .status(),
        None => std::process::Command::new(op).args(paths).status(),
    };
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(status.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// See `=time:` on [`bench_preview_look`]: each render the note's page put up whole after
/// another's, until its pictures are read.
async fn time_preview(app: &Rc<App>) {
    let (Some(tab), Some(rendered)) = (app.active(), super::preview::shown(app)) else {
        println!("bench preview_look time no_preview");
        return bench_quit(app);
    };
    let render = |rel: &str, text: &str| {
        if let Some(preview) = rendered.preview().as_ref() {
            preview.render(rel, text);
        }
    };
    let bytes = tab.text().len();
    for _ in 0..5 {
        render("other.md", "");
        glib::timeout_future(Duration::from_millis(500)).await;
        let (worst, last) = (Rc::new(Cell::new(0.0)), Rc::new(Cell::new(Instant::now())));
        let beat = glib::timeout_add_local(Duration::from_millis(1), {
            let (worst, last) = (worst.clone(), last.clone());
            move || {
                let now = Instant::now();
                worst.set(f64::max(worst.get(), ms_since(last.replace(now))));
                glib::ControlFlow::Continue
            }
        });
        let t = Instant::now();
        app.render_rendered(&rendered, true);
        let asked = ms_since(t);
        while !settled(app) && t.elapsed() < Duration::from_secs(20) {
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        let shown = ms_since(t);
        glib::timeout_future(Duration::from_millis(200)).await;
        beat.remove();
        println!(
            "bench preview_look time bytes={bytes} render_ms={asked:.1} shown_ms={shown:.0} \
             stall_ms={:.1}",
            worst.get()
        );
    }
    // The note and a word asked for at once: the word, asked for last, is what stays up.
    render(&tab.rel(), &tab.text());
    render(&tab.rel(), "latest");
    glib::timeout_future(Duration::from_secs(2)).await;
    let shown = (rendered.preview().as_ref())
        .map(|n| {
            let buffer = n.buffer();
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), false)
        })
        .unwrap_or_default();
    println!(
        "bench preview_look time latest={}",
        shown.trim() == "latest"
    );
    bench_quit(app);
}

/// See `=hold:` on [`bench_preview_look`]: each picture's middle in the window.
async fn hold_preview(app: &Rc<App>) {
    preview_look(app, "hold").await;
    let view =
        super::preview::shown(app).and_then(|r| Some(r.preview().as_ref()?.focus_target().clone()));
    if let (Some(view), Some((figures, _))) = (view, figures(app)) {
        for f in figures {
            let at = graphene::Point::new(f.at.0 as f32, f.at.1 as f32);
            let at = view.compute_point(&app.window, &at).unwrap_or(at);
            println!(
                "bench preview_look hold {} at={:.0},{:.0}",
                f.uri,
                at.x(),
                at.y()
            );
        }
    }
    for _ in 0..20 {
        glib::timeout_future(Duration::from_secs(2)).await;
        let mut inverted: Vec<String> = app.inverted_images.borrow().iter().cloned().collect();
        inverted.sort();
        println!(
            "bench preview_look hold inverted={inverted:?} requests={}",
            requests(app)
        );
    }
    bench_quit(app);
}
