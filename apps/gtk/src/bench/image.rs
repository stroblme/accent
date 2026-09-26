//! A drill over a zoomed image whose file changes size under it.

use super::*;
use crate::look::Look;
use accent_core::config::Theme;

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
/// Solarized, printing what the classifier said of it, whether the tab shows it recoloured, the
/// pixel at (2,2) of what it shows and the paintable's type — then the same with Invert Image
/// Colours on. `ms` is the worker's recolouring of the decoded texture, timed by calling it here.
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
    let shown = picture_of(&image.page).and_then(|p| p.paintable());
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
    let px = shown
        .downcast_ref::<gdk::Texture>()
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
    let recoloured = shown.downcast_ref::<gdk::Texture>() != Some(&original);
    let t = Instant::now();
    let _ = crate::look::show(&path, Some(original), Look::now(), inverted);
    println!(
        "bench image_look {rel} theme={theme:?} dark={} inverted={inverted} {verdict} \
         recoloured={recoloured} px(2,2)={px} type={} ms={:.1}",
        adw::StyleManager::default().is_dark(),
        shown.type_().name(),
        ms_since(t),
    );
}
