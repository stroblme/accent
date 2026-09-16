//! A drill over a zoomed image whose file changes size under it.

use super::*;

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
