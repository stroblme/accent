//! Shared zoom steps, limits, wheel handling and readouts for notes, images, PDFs and shells.

use super::*;

/// One press of Zoom In or Zoom Out, in percent: a tenth of the document font.
const ZOOM_STEP: i64 = 10;

/// Debounce SVG rerendering after zoom steps, scaling the previous image while waiting.
const ZOOM_SETTLE: Duration = Duration::from_millis(300);

impl App {
    /// Step image zoom, or reset to fit with `None`.
    ///
    /// Use stored zoom rather than rounded pixel sizes so each step advances. Keep the content
    /// under `at` fixed, or the viewport centre for keyboard zoom.
    pub fn zoom_image(
        self: &Rc<Self>,
        image: &Rc<doc::Viewer>,
        out: Option<bool>,
        at: Option<(f64, f64)>,
    ) {
        let zoom = out
            .zip(image_zoom(image))
            .map(|(out, from)| stepped_zoom(from, out));
        self.zoom_image_to(image, zoom, at);
    }

    /// Draw an image at `zoom`, or fitted to the window with `None`, keeping what is under `at`
    /// where it is: a step's, or a pinch's, which hands over the zoom its fingers reached.
    pub fn zoom_image_to(
        self: &Rc<Self>,
        image: &Rc<doc::Viewer>,
        zoom: Option<f64>,
        at: Option<(f64, f64)>,
    ) {
        let Some(picture) = picture_of(&image.page) else {
            return;
        };
        let zoom = zoom.map(clamp_scale);
        image.zoom.set(zoom);
        let size = set_image_zoom(&picture, zoom);
        if let (Some(size), Ok(scroller)) =
            (size, image.page.child().downcast::<gtk::ScrolledWindow>())
        {
            let middle = (
                f64::from(scroller.width()) / 2.0,
                f64::from(scroller.height()) / 2.0,
            );
            keep_under(&scroller, size, at.unwrap_or(middle));
        }
        self.refresh_zoom();
        // `show_image` draws an SVG at its zoom (`look::drawn_zoom`), and does nothing for an
        // image the zoom leaves as it is drawn.
        let image = Rc::downgrade(image);
        glib::timeout_add_local_once(
            ZOOM_SETTLE,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || {
                    if let Some(image) = image.upgrade().filter(|i| i.zoom.get() == zoom) {
                        app.show_image(&image, None);
                    }
                }
            ),
        );
    }

    /// Zoom document content and preview/presentation together; leave interface fonts to the system.
    pub fn set_zoom(self: &Rc<Self>, zoom: f64) {
        let zoom = clamp_zoom(zoom);
        self.zoom.set(zoom);
        let font = self.config.borrow().editor_font.clone();
        for tab in self.open_tabs() {
            tab.set_font(font.as_deref(), zoom);
        }
        for diff in self.diffs() {
            diff.set_font(font.as_deref(), zoom);
        }
        for notebook in self.notebooks() {
            notebook.set_font(font.as_deref(), zoom);
        }
        if let Some(preview) = self.preview.borrow().as_ref() {
            preview.set_zoom(zoom);
        }
        // The goals are set in the editor's font at this zoom when they land.
        self.refresh_goals();
        self.refresh_zoom();
        self.save_session_soon();
    }

    /// Update the zoom readout to match [`App::zoom_action`]'s supported document kinds.
    ///
    /// Text and shells hide the readout at their default size. PDFs and images always show it
    /// because reset means fit; status pages and diffs have no zoom readout.
    pub fn refresh_zoom(&self) {
        let label = match self.active_doc() {
            Some(Doc::Pdf(pdf)) => pdf.zoom_label(),
            Some(Doc::Diagram(d)) => Some(d.zoom_label()),
            Some(Doc::Terminal(term)) => term.zoom_label(),
            Some(Doc::Text(_)) | Some(Doc::Diff(_)) | Some(Doc::Notebook(_)) => {
                let zoom = self.zoom.get();
                (zoom != 1.0).then(|| format!("{} %", (zoom * 100.0).round() as i32))
            }
            Some(Doc::Image(image)) => Some(image_zoom_label(&image)),
            Some(Doc::Status(_)) | None => None,
        };
        self.statusbar.set_zoom(label.as_deref());
    }
}

/// What an image is drawn at: its own zoom, or the scale the window fitted it to.
pub fn image_zoom(image: &doc::Viewer) -> Option<f64> {
    if let Some(zoom) = image.zoom.get() {
        return Some(zoom);
    }
    let picture = picture_of(&image.page)?;
    let paintable = picture.paintable()?;
    let (w, h) = (paintable.intrinsic_width(), paintable.intrinsic_height());
    if w <= 0 || h <= 0 {
        return None;
    }
    // Fitted: `ScaleDown` takes whichever axis binds and never enlarges.
    let fitted = (f64::from(picture.width()) / f64::from(w))
        .min(f64::from(picture.height()) / f64::from(h))
        .min(1.0);
    Some(fitted)
}

/// The status bar's readout for an image, in the shape a PDF's is: what it is fitted to, or the
/// percentage it is at.
pub fn image_zoom_label(image: &doc::Viewer) -> String {
    match image.zoom.get() {
        Some(zoom) => format!("{} %", (zoom * 100.0).round() as i32),
        None => "Fit".to_string(),
    }
}

/// Draw an image at `zoom`, or fitted to the window when there is none, returning the size a
/// zoomed one asks for.
///
/// A zoomed picture is centred and asks for its exact size, so the scroller scrolls it once it
/// is larger than the viewport and does not stretch it while it is smaller.
pub fn set_image_zoom(picture: &gtk::Picture, zoom: Option<f64>) -> Option<(i32, i32)> {
    let size = zoom.and_then(|zoom| {
        let paintable = picture.paintable()?;
        let (w, h) = (paintable.intrinsic_width(), paintable.intrinsic_height());
        let side = |n: i32| (f64::from(n) * zoom).round() as i32;
        (w > 0 && h > 0).then(|| (side(w), side(h)))
    });
    match size {
        Some((w, h)) => {
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_halign(gtk::Align::Center);
            picture.set_valign(gtk::Align::Center);
            picture.set_size_request(w, h);
        }
        None => {
            picture.set_content_fit(gtk::ContentFit::ScaleDown);
            picture.set_halign(gtk::Align::Fill);
            picture.set_valign(gtk::Align::Fill);
            picture.set_size_request(-1, -1);
        }
    }
    size
}

/// Adjust image scrolling to keep content under `at` after resizing to `size`.
///
/// Set adjustment extents before values because allocation still has the old size and would
/// clamp too early. Bounds use at least the viewport size.
fn keep_under(scroller: &gtk::ScrolledWindow, size: (i32, i32), at: (f64, f64)) {
    for (adjustment, size, at) in [
        (scroller.hadjustment(), size.0, at.0),
        (scroller.vadjustment(), size.1, at.1),
    ] {
        let now = f64::from(size).max(adjustment.page_size());
        let value = zoomed_scroll(at, adjustment.value(), adjustment.upper(), now);
        adjustment.set_upper(now);
        adjustment.set_value(value);
    }
}

/// The `GtkPicture` inside a page built by [`App::open_image`].
///
/// Through the viewport: a picture is not a `GtkScrollable`, so the scroller puts one in between,
/// and the `child` property hands that back rather than what was put in it.
pub fn picture_of(page: &adw::TabPage) -> Option<gtk::Picture> {
    let child = page
        .child()
        .downcast::<gtk::ScrolledWindow>()
        .ok()?
        .child()?;
    match child.downcast::<gtk::Viewport>() {
        Ok(viewport) => viewport.child().and_downcast(),
        Err(child) => child.downcast().ok(),
    }
}

/// Clamp font zoom to 0.5–3.0 and round to tenths to prevent drift and invalid saved sizes.
pub fn clamp_zoom(zoom: f64) -> f64 {
    ((zoom * 10.0).round() / 10.0).clamp(0.5, 3.0)
}

/// Shared scale bounds: below 0.1× is unreadable; above 8× PDF tile memory grows excessively.
pub const MIN_SCALE: f64 = 0.1;
pub const MAX_SCALE: f64 = 8.0;

/// A zoom held to [`MIN_SCALE`]..=[`MAX_SCALE`]; one that is not finite (a NaN) is 1.0.
pub fn clamp_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(MIN_SCALE, MAX_SCALE)
    } else {
        1.0
    }
}

/// Step `zoom` to the next [`ZOOM_STEP`] multiple, shared by chords, wheels and pinches.
///
/// Round to whole percent before stepping: floating-point or pixel-size roundoff can
/// otherwise select the current step again. Each result moves by at least half a percent.
pub fn stepped_zoom(zoom: f64, out: bool) -> f64 {
    let percent = (zoom * 100.0).round() as i64;
    let next = match out {
        true => (percent - 1) / ZOOM_STEP * ZOOM_STEP,
        false => (percent / ZOOM_STEP + 1) * ZOOM_STEP,
    };
    next as f64 / 100.0
}

/// Keep viewport point `at` over the same content fraction as size changes from
/// `was` to `now`. Leave clamping to the adjustment when edges prevent anchoring.
pub fn zoomed_scroll(at: f64, offset: f64, was: f64, now: f64) -> f64 {
    (offset + at) * now / was.max(1.0) - at
}

/// Accumulate fractional smooth-scroll deltas so one wheel notch yields one zoom step.
pub fn wheel_steps(accum: &Cell<f64>, dy: f64) -> i32 {
    let total = accum.get() + dy;
    accum.set(total.fract());
    total.trunc() as i32
}

/// Step zoom on Ctrl+scroll: `step(true)` zooms out and `step(false)` zooms in.
///
/// Each controller accumulates smooth deltas independently so widgets cannot share notch
/// remainders. Non-Control events pass through. Scroll lacks coordinates; anchored zoom
/// uses [`track_pointer`] or the canvas's motion controller.
///
/// Callers choose phase: Bubble precedes text scrollers; Capture overrides WebKit and VTE's
/// private zoom handling so readouts and sessions track the result.
pub fn zoom_on_wheel(
    widget: &impl IsA<gtk::Widget>,
    phase: gtk::PropagationPhase,
    step: impl Fn(bool) + 'static,
) {
    let accum = Cell::new(0.0);
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.set_propagation_phase(phase);
    wheel.connect_scroll(move |controller, _, dy| {
        if !controller
            .current_event_state()
            .contains(crate::platform::PRIMARY_MODIFIER)
        {
            return glib::Propagation::Proceed;
        }
        let steps = wheel_steps(&accum, dy);
        for _ in 0..steps.abs() {
            step(steps > 0);
        }
        glib::Propagation::Stop
    });
    widget.add_controller(wheel);
}

/// Where the pointer is over `widget`, in its coordinates, until it leaves: what a Ctrl+scroll
/// zooms around ([`zoom_on_wheel`]).
pub fn track_pointer(widget: &impl IsA<gtk::Widget>) -> Rc<Cell<Option<(f64, f64)>>> {
    let at = Rc::new(Cell::new(None));
    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion(glib::clone!(
        #[strong]
        at,
        move |_, x, y| at.set(Some((x, y)))
    ));
    motion.connect_leave(glib::clone!(
        #[strong]
        at,
        move |_| at.set(None)
    ));
    widget.add_controller(motion);
    at
}

/// Round `initial_zoom × scale` to tenths; GTK pinch scale is cumulative, not an incremental step.
pub fn pinched_zoom(start: f64, scale: f64) -> f64 {
    (start * scale * 10.0).round() / 10.0
}

/// Apply two-finger zoom from `from()` through `to(zoom, at)`, anchored between the fingers.
///
/// [`pinched_zoom`] emits only new tenths, limiting relayout and leaving an untouched fitted
/// zoom such as 137 % unchanged.
pub fn zoom_on_pinch(
    widget: &impl IsA<gtk::Widget>,
    from: impl Fn() -> f64 + 'static,
    to: impl Fn(f64, (f64, f64)) + 'static,
) {
    // The zoom the pinch began at, and the last one it handed on.
    let (start, last) = (Rc::new(Cell::new(1.0)), Rc::new(Cell::new(1.0)));
    let pinch = gtk::GestureZoom::new();
    pinch.connect_begin(glib::clone!(
        #[strong]
        start,
        #[strong]
        last,
        move |gesture, _| {
            // Both fingers are the pinch's from the moment the second lands: the scrolled
            // window's touch drag would otherwise take the first once it moved past the drag
            // threshold, its pan cancelling the pinch.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            start.set(from());
            last.set(pinched_zoom(start.get(), 1.0));
        }
    ));
    pinch.connect_scale_changed(move |gesture, scale| {
        let zoom = pinched_zoom(start.get(), scale);
        if let Some(at) = gesture.bounding_box_center()
            && last.replace(zoom) != zoom
        {
            to(zoom, at);
        }
    });
    widget.add_controller(pinch);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps_in_tenths_and_stops_at_the_ends() {
        assert_eq!(clamp_zoom(stepped_zoom(1.0, false)), 1.1);
        assert_eq!(clamp_zoom(stepped_zoom(1.0, true)), 0.9);
        assert_eq!(clamp_zoom(0.1), 0.5, "no zooming down to nothing");
        assert_eq!(clamp_zoom(9.0), 3.0, "nor up past legibility");
        assert_eq!(clamp_zoom(1.24), 1.2, "a hand-edited state file is rounded");
    }

    #[test]
    fn a_scale_stays_in_range_and_nan_is_its_own_size() {
        assert_eq!(clamp_scale(1000.0), MAX_SCALE);
        assert_eq!(clamp_scale(0.0), MIN_SCALE);
        assert_eq!(clamp_scale(f64::NAN), 1.0);
    }

    #[test]
    fn stepped_zoom_moves_to_the_next_tenth() {
        assert_eq!(stepped_zoom(1.0, false), 1.1);
        assert_eq!(stepped_zoom(1.0, true), 0.9);
        // Off a tenth, which is where a PDF fitted to the window sits: the next tenth, not a
        // tenth further.
        assert_eq!(stepped_zoom(1.37, false), 1.4);
        assert_eq!(stepped_zoom(1.37, true), 1.3);
        assert_eq!(stepped_zoom(1.1, false), 1.2);
        // A zoom read back off a rendered size lands a hair either side of the tenth it was, and
        // a step from it still has to move.
        assert_eq!(stepped_zoom(2.2999998, false), 2.4);
        assert_eq!(stepped_zoom(2.3000002, true), 2.2);
    }

    #[test]
    fn a_zoom_keeps_what_is_under_the_pointer_under_it() {
        // 800 px into content 2000 wide (scrolled 500, pointer at 300): doubled, that point is
        // 1600 in, and still under the pointer.
        assert_eq!(zoomed_scroll(300.0, 500.0, 2000.0, 4000.0), 1300.0);
        assert_eq!(zoomed_scroll(300.0, 1300.0, 4000.0, 2000.0), 500.0);
    }

    #[test]
    fn a_pinch_follows_the_fingers_from_where_it_began() {
        // The scale is the whole pinch so far, so it multiplies the zoom the pinch began at: a
        // step taken per event instead ran off to the end of the range.
        assert_eq!(pinched_zoom(1.0, 1.5), 1.5);
        assert_eq!(pinched_zoom(2.0, 0.5), 1.0);
        // On the nearest tenth, as every other zoom lands.
        assert_eq!(pinched_zoom(1.37, 1.0), 1.4);
        assert_eq!(pinched_zoom(1.0, 1.04), 1.0);
        assert_eq!(pinched_zoom(1.0, 1.26), 1.3);
    }

    #[test]
    fn a_wheel_notch_is_one_step() {
        let accum = Cell::new(0.0);
        assert_eq!(
            wheel_steps(&accum, 0.5),
            0,
            "half a notch is not a step yet"
        );
        assert_eq!(wheel_steps(&accum, 0.5), 1, "the other half completes it");
        assert_eq!(wheel_steps(&accum, 1.0), 1);
        assert_eq!(wheel_steps(&accum, -2.0), -2);
    }
}
