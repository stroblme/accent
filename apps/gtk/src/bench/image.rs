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
pub(super) fn bench_image(app: &Rc<App>, arg: &str) {
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

/// What the picture in front holds: the file's own size, the size it asks the scroller for, the
/// size it is drawn at, and the readout over it.
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
    let intrinsic = paintable.map_or((0, 0), |p| (p.intrinsic_width(), p.intrinsic_height()));
    println!(
        "bench image {when} file={}x{} request={}x{} drawn={}x{} label={:?}",
        intrinsic.0,
        intrinsic.1,
        picture.width_request(),
        picture.height_request(),
        picture.width(),
        picture.height(),
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
    let texture = shown.downcast_ref::<gdk::Texture>().cloned().or_else(|| {
        shown
            .downcast_ref::<crate::look::Scaled>()
            .map(|s| s.texture())
    });
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
    let _ = crate::look::show(&path, Some(original), Look::now(), inverted, scale);
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
/// `=change:<rel_note>,<rel_img>,<rel_other>` instead copies `<rel_other>` over `<rel_img>` once
/// the note shows, a figure exported again under the preview, and prints the page either side.
pub(super) fn bench_preview_look(app: &Rc<App>, rel: &str) {
    if let Some(arg) = rel.strip_prefix("change:") {
        return change_preview(app, arg);
    }
    let (hold, rel) = match rel.strip_prefix("hold:") {
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
        let keys: Vec<String> = page_images(&app)
            .await
            .iter()
            .filter_map(|(src, ..)| {
                let rel = src.strip_prefix("accent://file/")?;
                app.vault()?
                    .asset(&accent_core::markdown::percent_decode(rel))
            })
            .collect();
        for key in &keys {
            app.invert_image(key);
        }
        preview_look(&app, "inverted").await;
        bench_quit(&app);
    });
}

/// Every image on the preview's page: its address, its box in the document and whether it has
/// finished loading.
async fn page_images(app: &Rc<App>) -> Vec<(String, f64, f64, f64, f64, bool)> {
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
    while started.elapsed() < Duration::from_secs(20)
        && (view.is_loading() || page_images(app).await.iter().any(|i| !i.5))
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

/// See `=change:` on [`bench_preview_look`].
fn change_preview(app: &Rc<App>, arg: &str) {
    let [rel, image, other] = arg.split(',').collect::<Vec<_>>()[..] else {
        println!("bench preview_look change needs <rel_note>,<rel_img>,<rel_other>");
        return bench_quit(app);
    };
    app.open_path(rel);
    let (app, image, other) = (app.clone(), image.to_string(), other.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        app.set_mode(Mode::Split);
        preview_look(&app, "before").await;
        let root = app.root();
        if let Err(e) = std::fs::copy(root.join(&other), root.join(&image)) {
            println!("bench preview_look cannot_replace {e}");
            return bench_quit(&app);
        }
        // The watcher's event, and the render it asks for.
        glib::timeout_future(Duration::from_millis(1500)).await;
        preview_look(&app, "changed").await;
        bench_quit(&app);
    });
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
