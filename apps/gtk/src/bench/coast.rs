//! `ACCENT_BENCH_COAST=1`: a zoom made while a scrolled window coasts, as it does after a
//! touchpad's two-finger scroll, in each view that zooms inside one and keeps a place as it does.
//! A diagram and an image are pinched, wheeled with Ctrl and zoomed by the chord, a notebook, a
//! note and its preview wheeled and zoomed by the chord. Each line says whether the scrolled
//! window still coasted when the zoom came, whether the view zoomed, whether it kept what it
//! keeps once the coast would have ended (the point under the pointer, or for a chord the middle
//! of a diagram's or an image's view, a notebook's line at its top and a note's), both readings
//! and the zoom before and after. Writes `coast.*` into the vault, a scratch one.

use super::*;

/// How fast the fling is, in pixels a second: GTK coasts it out over 2.5 s, 500 px all told.
const FLING: f64 = 2000.0;
/// How long into the coast the zoom comes, when it has a few pixels left to go and is still
/// writing its offset every frame.
const INTO_COAST_MS: u64 = 1200;

/// How a case zooms.
#[derive(Clone, Copy, PartialEq)]
enum How {
    Pinch,
    Wheel,
    Chord,
}

impl How {
    fn name(self) -> &'static str {
        match self {
            How::Pinch => "pinch",
            How::Wheel => "wheel",
            How::Chord => "chord",
        }
    }
}

pub(super) fn bench_coast(app: &Rc<App>) {
    scratch_only(app, "ACCENT_BENCH_COAST");
    let root = app.root();
    for (rel, text) in fixtures() {
        std::fs::write(root.join(rel), text).expect("write a coast fixture");
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        // A file written a moment ago is only in the vault once the watcher has seen it.
        glib::timeout_future(Duration::from_millis(1500)).await;
        diagram(&app).await;
        image(&app).await;
        notebook(&app).await;
        note(&app).await;
        bench_quit(&app);
    });
}

/// A page taller than the view, an image as tall, a notebook of eighty code cells of eight lines
/// and a note of four hundred paragraphs.
fn fixtures() -> [(&'static str, String); 4] {
    let diagram = r#"<mxfile host="accent"><diagram name="Tall" id="tall"><mxGraphModel page="0"><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="t" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="0" y="0" width="400" height="4000" as="geometry"/></mxCell></root></mxGraphModel></diagram></mxfile>
"#;
    let stripes: String = (0..60)
        .map(|n| {
            format!(
                r##"<rect y="{}" width="600" height="50" fill="#{}"/>"##,
                n * 100,
                ["c33", "36c"][n % 2]
            )
        })
        .collect();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="600" height="6000">{stripes}</svg>
"#
    );
    let cells: Vec<String> = (0..80)
        .map(|n| {
            let lines: Vec<String> = (0..8).map(|l| format!(r#""x{n}_{l} = {l}\n""#)).collect();
            format!(
                r#"{{"cell_type": "code", "execution_count": null, "metadata": {{}}, "outputs": [], "source": [{}]}}"#,
                lines.join(", ")
            )
        })
        .collect();
    let notebook = format!(
        r#"{{"cells": [{}], "metadata": {{}}, "nbformat": 4, "nbformat_minor": 5}}
"#,
        cells.join(",\n ")
    );
    let note: String = (1..=400)
        .map(|n| format!("Paragraph {n} of a note long enough to scroll through.\n\n"))
        .collect();
    [
        ("coast.drawio", diagram.to_string()),
        ("coast.svg", svg),
        ("coast.ipynb", notebook),
        ("coast.md", note),
    ]
}

async fn diagram(app: &Rc<App>) {
    app.open_path("coast.drawio");
    let found = until("coast", "diagram", 10_000, || match app.active_doc() {
        Some(Doc::Diagram(d)) => Some(d),
        _ => None,
    });
    let Some(d) = found.await else { return };
    let Some(scroller) = scroller_in(&d.page) else {
        return;
    };
    laid_out(&scroller).await;
    for how in [How::Pinch, How::Wheel, How::Chord] {
        d.zoom_to(1.0);
        let at = point(&scroller, how);
        // Where on the page `at` is, in the page's own units.
        let read = || {
            let page = d.to_widget(&accent_drawio::Rect::new(0.0, 0.0, 1000.0, 1000.0));
            Some((at.1 - page.y) * 1000.0 / page.h)
        };
        let probe = Probe {
            read: &read,
            slack: 4.0,
            zoom: &|| d.scale(),
            pinch: &|| d.pinched(crate::zoom::pinched_zoom(d.scale(), 1.1), at),
        };
        case(app, ("diagram", how), &scroller, at, &probe).await;
    }
    app.close_page(&d.page.clone());
}

async fn image(app: &Rc<App>) {
    app.open_path("coast.svg");
    let found = until("coast", "image", 10_000, || match app.active_doc() {
        Some(Doc::Image(image)) => picture_of(&image.page)
            .and_then(|p| p.paintable())
            .map(|_| image),
        _ => None,
    });
    let Some(image) = found.await else { return };
    let Ok(scroller) = crate::loading::content(&image.page).downcast::<gtk::ScrolledWindow>()
    else {
        return;
    };
    laid_out(&scroller).await;
    for how in [How::Pinch, How::Wheel, How::Chord] {
        app.zoom_image_to(&image, Some(1.0), None);
        // The SVG drawn again at its zoom.
        glib::timeout_future(crate::zoom::ZOOM_SETTLE * 2).await;
        let at = point(&scroller, how);
        // How far down the image `at` is, in thousandths of its height.
        let read = || {
            let v = scroller.vadjustment();
            Some((v.value() + at.1) * 1000.0 / v.upper())
        };
        let zoom = || crate::zoom::image_zoom(&image).unwrap_or(1.0);
        let pinch = || {
            let to = crate::zoom::pinched_zoom(zoom(), 1.1);
            app.zoom_image_to(&image, Some(to), Some(at));
        };
        let probe = Probe {
            read: &read,
            slack: 2.0,
            zoom: &zoom,
            pinch: &pinch,
        };
        case(app, ("image", how), &scroller, at, &probe).await;
    }
    app.close_page(&image.page.clone());
}

async fn notebook(app: &Rc<App>) {
    app.open_path("coast.ipynb");
    let found = until("coast", "notebook", 10_000, || {
        app.doc_for("coast.ipynb")
            .and_then(|d| d.notebook().cloned())
    });
    let Some(tab) = found.await else { return };
    let scroller = tab.bench_scroller();
    laid_out(&scroller).await;
    for how in [How::Wheel, How::Chord] {
        app.set_zoom(1.0);
        // A chord keeps the line at the top.
        let at = match how {
            How::Chord => (f64::from(scroller.width()) / 2.0, 1.0),
            _ => point(&scroller, how),
        };
        // The cell under `at` when first read and how far down it, then how far that point of it
        // has moved from `at` since.
        let spot = Cell::new(None);
        let read = || match spot.get() {
            Some((n, down)) => cell_span(&tab, n).map(|(top, height)| top + down * height - at.1),
            None => {
                let n = (0..80).find(|&n| {
                    cell_span(&tab, n)
                        .is_some_and(|(top, height)| (top..top + height).contains(&at.1))
                })?;
                let (top, height) = cell_span(&tab, n)?;
                spot.set(Some((n, (at.1 - top) / height)));
                Some(0.0)
            }
        };
        let probe = Probe {
            read: &read,
            slack: 12.0,
            zoom: &|| app.zoom.get(),
            pinch: &|| (),
        };
        case(app, ("notebook", how), &scroller, at, &probe).await;
    }
    app.set_zoom(1.0);
    app.close_page(&tab.page.clone());
}

/// The note in its editor, then in its preview.
async fn note(app: &Rc<App>) {
    app.open_path("coast.md");
    let Some(tab) = until("coast", "note", 10_000, || app.active()).await else {
        return;
    };
    glib::timeout_future(Duration::from_millis(500)).await;
    let Some(scroller) = tab
        .view
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
    else {
        return;
    };
    laid_out(&scroller).await;
    let editor: gtk::TextView = tab.view.clone().upcast();
    for how in [How::Wheel, How::Chord] {
        app.set_zoom(1.0);
        let at = point(&scroller, how);
        let probe = Probe {
            read: &|| top_line(&editor),
            slack: 1.0,
            zoom: &|| app.zoom.get(),
            pinch: &|| (),
        };
        case(app, ("note", how), &scroller, at, &probe).await;
    }
    app.set_zoom(1.0);
    let _ = WidgetExt::activate_action(&app.window, "win.view-mode", None);
    glib::timeout_future(Duration::from_secs(2)).await;
    let Some(scroller) = app.tabs().selected_page().and_then(|p| scroller_in(&p)) else {
        return;
    };
    laid_out(&scroller).await;
    let Some(preview) =
        find_widget(scroller.upcast_ref(), &|w| w.is::<gtk::TextView>()).and_downcast()
    else {
        return;
    };
    for how in [How::Wheel, How::Chord] {
        app.set_zoom(1.0);
        let at = point(&scroller, how);
        let probe = Probe {
            read: &|| top_line(&preview),
            slack: 1.0,
            zoom: &|| app.zoom.get(),
            pinch: &|| (),
        };
        case(app, ("preview", how), &scroller, at, &probe).await;
    }
    app.set_zoom(1.0);
}

/// Where a case's zoom is about, in `scroller`'s coordinates: two fifths down the middle for the
/// pointer, the middle of the view for a chord, which a diagram and an image keep.
fn point(scroller: &gtk::ScrolledWindow, how: How) -> (f64, f64) {
    let (w, h) = (f64::from(scroller.width()), f64::from(scroller.height()));
    match how {
        How::Chord => (w / 2.0, h / 2.0),
        _ => (w / 2.0, h * 0.4),
    }
}

/// What a case reads off a view, and how it pinches it.
struct Probe<'a> {
    /// What the zoom keeps, which may move by `slack` and still count as kept.
    read: &'a dyn Fn() -> Option<f64>,
    slack: f64,
    /// The zoom the view is at, which the case's zoom has to have moved.
    zoom: &'a dyn Fn() -> f64,
    /// A pinch a tenth in about the case's point.
    pinch: &'a dyn Fn(),
}

/// One case: `scroller` scrolled to its middle, the pointer put on `at` and the view flung down,
/// then zoomed a tenth in by `how` well into the coast.
async fn case(
    app: &Rc<App>,
    (view, how): (&str, How),
    scroller: &gtk::ScrolledWindow,
    at: (f64, f64),
    probe: &Probe<'_>,
) {
    // The layout a zoom set back asked for.
    glib::timeout_future(Duration::from_millis(400)).await;
    let v = scroller.vadjustment();
    v.set_value((v.upper() - v.page_size()) / 2.0);
    glib::timeout_future(Duration::from_millis(400)).await;
    if let Some(p) = super::diagram::on_screen(app, scroller, at.0, at.1) {
        super::git::xtest_now(&format!("move {p}")).await;
    }
    glib::timeout_future(Duration::from_millis(200)).await;
    let flung = fling(scroller, FLING);
    glib::timeout_future(Duration::from_millis(INTO_COAST_MS)).await;
    let moving = v.value();
    glib::timeout_future(Duration::from_millis(100)).await;
    let coasting = flung && v.value() != moving;
    let (before, from) = ((probe.read)(), (probe.zoom)());
    match how {
        How::Pinch => (probe.pinch)(),
        How::Wheel => super::git::xtest_now("keydown ctrl; scroll 1; keyup ctrl").await,
        How::Chord => {
            let _ = WidgetExt::activate_action(&app.window, "win.zoom-in", None);
        }
    }
    // Past the coast's end, and the layout the zoom asked for.
    glib::timeout_future(Duration::from_millis(2000)).await;
    let (after, to) = ((probe.read)(), (probe.zoom)());
    let kept = before
        .zip(after)
        .is_some_and(|(b, a)| (a - b).abs() <= probe.slack);
    let show = |r: Option<f64>| r.map_or("none".to_string(), |r| format!("{r:.1}"));
    println!(
        "bench coast {view} {} coasting={coasting} zoomed={} kept={kept} before={} after={} \
         zoom={from:.2}>{to:.2}",
        how.name(),
        to > from,
        show(before),
        show(after)
    );
}

/// Fling `scroller` down at `speed` pixels a second, as the end of a touchpad's two-finger scroll
/// does: its own two scroll controllers told what a touchpad's events tell them, a scroll begun,
/// a step of it and its speed at the end, which X11's wheel never sends. GTK scales the speed by
/// the step it scrolls a notch, `page^(2/3)`, or by 2.5 for a touchpad's pixels.
fn fling(scroller: &gtk::ScrolledWindow, speed: f64) -> bool {
    let theirs =
        gtk::EventControllerScrollFlags::BOTH_AXES | gtk::EventControllerScrollFlags::KINETIC;
    let (mut bubble, mut capture) = (None, None);
    let controllers = scroller.observe_controllers();
    for i in 0..controllers.n_items() {
        let Some(c) = controllers
            .item(i)
            .and_downcast::<gtk::EventControllerScroll>()
            .filter(|c| c.flags() == theirs)
        else {
            continue;
        };
        match c.propagation_phase() {
            gtk::PropagationPhase::Capture => capture = Some(c),
            _ => bubble = Some(c),
        }
    }
    let (Some(bubble), Some(capture)) = (bubble, capture) else {
        return false;
    };
    let step = match capture.unit() {
        gdk::ScrollUnit::Wheel => scroller.vadjustment().page_size().powf(2.0 / 3.0),
        _ => 2.5,
    };
    bubble.emit_by_name::<()>("scroll-begin", &[]);
    bubble.emit_by_name::<bool>("scroll", &[&0.0f64, &0.01f64]);
    capture.emit_by_name::<()>("decelerate", &[&0.0f64, &(speed / step)]);
    bubble.emit_by_name::<()>("scroll-end", &[]);
    true
}

/// Until `scroller` has a size, which the points a case zooms about are taken from.
async fn laid_out(scroller: &gtk::ScrolledWindow) {
    until("coast", "laid out", 5_000, || scroller.height() > 0).await;
    glib::timeout_future(Duration::from_millis(300)).await;
}

/// The outermost scroller on screen in `page` that scrolls up and down.
fn scroller_in(page: &adw::TabPage) -> Option<gtk::ScrolledWindow> {
    find_widget(&page.child(), &|w| {
        w.is_mapped()
            && w.downcast_ref::<gtk::ScrolledWindow>()
                .is_some_and(|s| s.vscrollbar_policy() != gtk::PolicyType::Never)
    })
    .and_downcast()
}

/// The line at the top of `view`.
fn top_line(view: &gtk::TextView) -> Option<f64> {
    let (_, y) = view.window_to_buffer_coords(gtk::TextWindowType::Widget, 0, 0);
    let (iter, _) = view.line_at_y(y);
    Some(f64::from(iter.line()))
}

/// Where cell `n` of the notebook starts in the view, and how far it is to the next one's top.
fn cell_span(tab: &crate::notebook::NotebookTab, n: usize) -> Option<(f64, f64)> {
    let top = tab.bench_cell_y(n)?;
    Some((top, tab.bench_cell_y(n + 1)? - top))
}
