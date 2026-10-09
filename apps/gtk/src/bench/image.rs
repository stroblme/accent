//! A drill over a zoomed image whose file changes size under it.

use super::*;
use crate::look::Look;
use accent_core::config::Theme;
use webkit6::prelude::*;

/// `ACCENT_BENCH_IMAGE=<rel_png>,<rel_other_png>` opens `<rel_png>`, zooms it one step off the fit
/// so it asks for a size of its own, then copies `<rel_other_png>` over it — an image of another
/// size changing under an open tab, which is what a re-exported figure is — and prints what the
/// picture asks for, what it is drawn at and what the readout says either side of the reload.
///
/// A fitted image was already covered by the reload drills of 8facc35; what a zoom adds is the
/// size request, which is worked out from the paintable and so is of the file that has gone.
///
/// `=zoom:<rel_svg>,<rel_svg>,…` instead steps each SVG in thirty times, then forty more, then
/// back to the fit, printing the picture just after each run and once the zoom has settled, with
/// how long the drawing at the new zoom took to land: it is drawn once per run, at the last step.
///
/// `=anchor:<rel>` instead zooms the image around a point two fifths into the view, as a
/// Ctrl+wheel there does, forty steps in from the fit and ten out, then once in by the chord,
/// printing after each where in the image the point is (`under`, in fractions of it), and the
/// middle of the view for the chord. Once the image is larger than the view, neither moves. Then
/// two pinches around the same point, to one and a half times the zoom and back to half of that,
/// handed the zoom the fingers reached as a pinch hands it over (`App::zoom_image_to`).
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
        let Ok(scroller) = image.page.child().downcast::<gtk::ScrolledWindow>() else {
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

/// What the picture in front holds: the file's own size, the size it asks the scroller for, the
/// size it is drawn at, the texture it is drawn from, the times it was sent to be drawn, and the
/// readout over it.
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

/// `ACCENT_BENCH_IMAGE_LOOK=<rel>,<rel>,…` opens each image under Dark, printing how long it took
/// to appear (decoded, classified and recoloured), then walks it through Light, Dark and
/// Solarized, printing what the classifier said of it, whether the tab shows it recoloured, its
/// size and its texture's, the pixel at (2,2) of that texture and the paintable's type — then the
/// same with Invert Image Colours on. `ms` is the worker's recolouring of the decoded texture, timed by calling it
/// here.
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
    let recoloured = texture.as_ref() != Some(&original);
    let pixels = texture.map_or((0, 0), |t| (t.width(), t.height()));
    let t = Instant::now();
    let zoom = crate::look::drawn_zoom(&path, image.zoom.get());
    let _ = crate::look::show(&path, Some(original), Look::now(), inverted, scale, zoom);
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

/// `ACCENT_BENCH_PREVIEW_LOOK=<rel_note>` shows a note in the split view and prints, for every
/// image on the page, the pixel WebKit painted two in from its corner and at its centre, with how
/// many requests the page has made so far: after the first render, after a render of the same
/// text, under Light, Dark and Solarized, and then with every image on it inverted. Each side of a
/// conflict block gets a line too: its class, caption, height and the tints the two are painted
/// in. With `ACCENT_BENCH_SHOTS=<dir>` each step's whole page is saved there as a PNG.
///
/// `=hold:<rel_note>` instead prints where each image is on screen and stays up for 40 s, printing
/// the inverted images and the page's requests every two seconds, for an XTEST right-click on an
/// image and a pick from its menu.
///
/// `=time:<rel_note>` instead renders the note five times over and prints, for each, how long the
/// render held the main thread (`render_ms`), until its page was up (`shown_ms`), and the
/// longest the main loop went without turning from the render until 200 ms past the load
/// (`stall_ms`), which is how long typing would wait on the preview; then whether, of the note
/// and a word rendered at once, the word asked for last is what the page shows (`latest`).
///
/// `=change:<rel_note>,<rel_img>,<rel_other>` instead copies `<rel_other>` over `<rel_img>` once
/// the note shows, a figure exported again under the preview, and prints the page either side.
/// `=gone:<rel_note>,<rel_img>[,<rel_to>]` removes `<rel_img>` instead, or renames it to
/// `<rel_to>`. On a remote vault the file is changed on the host, over ssh.
///
/// `=type:<rel_note>[,<word>]` types into the note instead, on a scratch vault, and prints what a
/// re-render costs and what it keeps (`preview::bench_preview_type`).
pub(super) fn bench_preview_look(app: &Rc<App>, rel: &str) {
    if let Some(arg) = rel.strip_prefix("type:") {
        return bench_preview_type(app, arg);
    }
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
        app.set_mode(Mode::Split);
        if hold {
            return hold_preview(&app).await;
        }
        if time {
            return time_preview(&app).await;
        }
        preview_look(&app, "first").await;
        if let Some(tab) = app.active() {
            app.render(&tab);
        }
        preview_look(&app, "rerender").await;
        for theme in [Theme::Light, Theme::Dark, Theme::Solarized] {
            crate::theme::apply(theme);
            app.restyle_all();
            preview_look(&app, &format!("{theme:?}")).await;
        }
        // The image menu's own route: the page's address, resolved to the vault key it names.
        let mut keys: Vec<String> = page_images(&app)
            .await
            .iter()
            .filter_map(|(src, ..)| {
                // A diagram's address names its page after a `?`.
                let rel = src.strip_prefix("accent://file/")?.split('?').next()?;
                app.vault()?
                    .asset(&accent_core::markdown::percent_decode(rel))
            })
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

/// Every image on the preview's page: its address, its box in the document and whether it has
/// finished loading.
pub(super) async fn page_images(app: &Rc<App>) -> Vec<(String, f64, f64, f64, f64, bool)> {
    let Some(view) = app.preview.borrow().as_ref().map(|p| p.view().clone()) else {
        return Vec::new();
    };
    let script = "JSON.stringify(Array.from(document.images).map(function (i) { \
        var r = i.getBoundingClientRect(); \
        return [i.src, r.left + scrollX, r.top + scrollY, r.width, r.height, \
                i.complete && i.naturalWidth > 0]; }))";
    let json = match view.evaluate_javascript_future(script, None, None).await {
        Ok(value) => value.to_str().to_string(),
        Err(e) => {
            println!("bench preview_look js_error {e}");
            return Vec::new();
        }
    };
    serde_json::from_str(&json).unwrap_or_default()
}

/// Wait for the page to settle, then print a `bench preview_look` line per image, with how long
/// the page and its images took to arrive (`ms`, from 300 ms after the render was asked for).
async fn preview_look(app: &Rc<App>, when: &str) {
    // The render is asked for on an idle or after the cache is cleared, so the old page is still
    // up for a moment.
    glib::timeout_future(Duration::from_millis(300)).await;
    let Some(view) = app.preview.borrow().as_ref().map(|p| p.view().clone()) else {
        return println!("bench preview_look {when} no_preview");
    };
    let started = Instant::now();
    let settled = || app.preview.borrow().as_ref().is_some_and(|p| p.settled());
    while started.elapsed() < Duration::from_secs(20)
        && (view.is_loading() || !settled() || page_images(app).await.iter().any(|i| !i.5))
    {
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    let ms = ms_since(started);
    // A frame for what arrived to be painted.
    glib::timeout_future(Duration::from_millis(200)).await;
    let requests = app.preview.borrow().as_ref().map_or(0, |p| p.requests());
    let shot = view
        .snapshot_future(
            webkit6::SnapshotRegion::FullDocument,
            webkit6::SnapshotOptions::NONE,
        )
        .await;
    let Ok(shot) = shot else {
        return println!("bench preview_look {when} no_snapshot");
    };
    let mut downloader = gdk::TextureDownloader::new(&shot);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let pixel = |x: f64, y: f64| {
        let (x, y) = (x as usize, y as usize);
        match (x < shot.width() as usize, y < shot.height() as usize) {
            (true, true) => {
                let at = y * stride + x * 4;
                format!("{},{},{}", bytes[at], bytes[at + 1], bytes[at + 2])
            }
            _ => "-".to_string(),
        }
    };
    for (src, x, y, w, h, loaded) in page_images(app).await {
        println!(
            "bench preview_look {when} dark={} {src} loaded={loaded} size={w}x{h} px(2,2)={} \
             px(centre)={} requests={requests} ms={ms:.0}",
            adw::StyleManager::default().is_dark(),
            pixel(x + 2.0, y + 2.0),
            pixel(x + w / 2.0, y + h / 2.0),
        );
    }
    let script = "JSON.stringify(Array.from(document.querySelectorAll('.conflict > div')) \
        .map(function (d) { var l = d.firstElementChild, s = getComputedStyle; \
        return [d.className, l.textContent, d.offsetHeight, s(d).backgroundColor, \
                s(l).backgroundColor]; }))";
    let sides: Vec<(String, String, f64, String, String)> = view
        .evaluate_javascript_future(script, None, None)
        .await
        .ok()
        .and_then(|v| serde_json::from_str(&v.to_str()).ok())
        .unwrap_or_default();
    for (class, label, h, body, caption) in sides {
        println!(
            "bench preview_look {when} dark={} {class} label={label:?} h={h} body={body} \
             caption={caption}",
            adw::StyleManager::default().is_dark(),
        );
    }
    if let Ok(dir) = std::env::var("ACCENT_BENCH_SHOTS")
        && let Err(e) = shot.save_to_png(Path::new(&dir).join(format!("preview-{when}.png")))
    {
        println!("bench preview_look {when} shot_error {e}");
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
        app.set_mode(Mode::Split);
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

/// See `=time:` on [`bench_preview_look`].
async fn time_preview(app: &Rc<App>) {
    let (Some(view), Some(tab)) = (
        app.preview.borrow().as_ref().map(|p| p.view().clone()),
        app.active(),
    ) else {
        println!("bench preview_look time no_preview");
        return bench_quit(app);
    };
    let settled = || app.preview.borrow().as_ref().is_some_and(|p| p.settled());
    let bytes = tab.text().len();
    for _ in 0..5 {
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
        app.render(&tab);
        let render = ms_since(t);
        while !settled() && t.elapsed() < Duration::from_secs(20) {
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        let shown = ms_since(t);
        glib::timeout_future(Duration::from_millis(200)).await;
        beat.remove();
        println!(
            "bench preview_look time bytes={bytes} render_ms={render:.1} shown_ms={shown:.0} \
             stall_ms={:.1}",
            worst.get()
        );
    }
    // The note and a word asked for at once: the word, asked for last, is what stays up.
    if let Some(preview) = app.preview.borrow().as_ref() {
        preview.render(&tab.rel(), &tab.text());
        preview.render(&tab.rel(), "latest");
    }
    glib::timeout_future(Duration::from_secs(2)).await;
    let shown = view
        .evaluate_javascript_future("document.body.textContent", None, None)
        .await
        .map_or_else(|e| e.to_string(), |v| v.to_str().to_string());
    println!(
        "bench preview_look time latest={}",
        shown.trim() == "latest"
    );
    bench_quit(app);
}

/// See `=hold:` on [`bench_preview_look`].
async fn hold_preview(app: &Rc<App>) {
    preview_look(app, "hold").await;
    let Some(view) = app.preview.borrow().as_ref().map(|p| p.view().clone()) else {
        return bench_quit(app);
    };
    let origin = view
        .compute_point(&app.window, &graphene::Point::new(0.0, 0.0))
        .unwrap_or_else(|| graphene::Point::new(0.0, 0.0));
    let scroll = view
        .evaluate_javascript_future("scrollY", None, None)
        .await
        .map_or(0.0, |v| v.to_double());
    for (src, x, y, w, h, _) in page_images(app).await {
        println!(
            "bench preview_look hold {src} at={:.0},{:.0}",
            f64::from(origin.x()) + x + w / 2.0,
            f64::from(origin.y()) + y - scroll + h / 2.0,
        );
    }
    for _ in 0..20 {
        glib::timeout_future(Duration::from_secs(2)).await;
        let mut inverted: Vec<String> = app.inverted_images.borrow().iter().cloned().collect();
        inverted.sort();
        println!(
            "bench preview_look hold inverted={inverted:?} requests={}",
            app.preview.borrow().as_ref().map_or(0, |p| p.requests())
        );
    }
    bench_quit(app);
}
